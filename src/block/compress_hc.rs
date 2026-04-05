//! High compression algorithm implementation.
//!
//! This module implements the LZ4 high compression algorithm using the HashTableHCU32
//! for better compression ratios at the cost of some performance.
//!
//! It includes two compression strategies:
//! - `compress_hc`: The standard high compression algorithm (levels 3-9)
//! - `compress_opt`: The optimal parsing algorithm for maximum compression (levels 10-12)

use crate::block::compress::{backtrack_match, count_same_bytes};
#[cfg(test)]
use crate::block::decompress;
use crate::block::{
    encode_sequence, handle_last_literals, CompressError, END_OFFSET, LAST_LITERALS, MAX_DISTANCE,
    MFLIMIT, MINMATCH,
};

#[cfg(not(feature = "safe-encode"))]
use crate::sink::PtrSink;
use crate::sink::Sink;
#[cfg(feature = "safe-encode")]
use crate::sink::SliceSink;
#[allow(unused_imports)]
use alloc::boxed::Box;
#[allow(unused_imports)]
use alloc::vec;
#[allow(unused_imports)]
use alloc::vec::Vec;

const HASHTABLE_SIZE_HC: usize = 1 << 15;
const MAX_DISTANCE_HC: usize = 1 << 16;

// LZ4MID constants (for levels 1-2)
const LZ4MID_HASH_LOG: usize = 15;
const LZ4MID_HASHTABLE_SIZE: usize = 1 << LZ4MID_HASH_LOG;

const OPTIMAL_MATCH_LENGTH: usize = 32;
const MATCH_LENGTH_MASK: usize = 31;

/// Size of the optimal parsing buffer
const LZ4_OPT_NUM: usize = 1 << 12; // 4096

/// Number of trailing literals to consider after last match
const TRAILING_LITERALS: usize = 3;

/// Run mask for literal/match length encoding
const RUN_MASK: usize = 15;

/// Which high-compression strategy applies for a given level (after clamping to 12).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HcCompressionStrategy {
    /// Levels 0–2: intermediate (lz4mid-style) compressor.
    Mid,
    /// Levels 3–9: hash-chain HC.
    HashChain,
    /// Levels 10–12: optimal parsing.
    Optimal,
}

/// Resolved parameters for HC compression (mid, hash-chain, or optimal).
///
/// Call [`hc_level_params`] once per block / public API entry, then pass this through internal
/// helpers so level is not re-mapped repeatedly.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HcLevelParams {
    pub(crate) strategy: HcCompressionStrategy,
    /// Hash-chain search budget for [`HcCompressionStrategy::HashChain`] and [`HcCompressionStrategy::Optimal`]
    /// (same meaning as [`HashTableHCU32::max_attempts`]). Zero in [`HcCompressionStrategy::Mid`].
    pub(crate) max_attempts: usize,
    /// Only for [`HcCompressionStrategy::Optimal`]: encode immediately when the first match is at least this long.
    pub(crate) sufficient_match_len: usize,
    /// Only for [`HcCompressionStrategy::Optimal`]: exhaustive refinement (level 12).
    pub(crate) full_optimal_update: bool,
}

/// Map a compression level to strategy and parameters. `level` should be clamped with `min(12)` first.
#[inline]
pub(crate) const fn hc_level_params(level: u8) -> HcLevelParams {
    match level {
        // The C reference (v1.10.0+) remaps level 0 to 9 (default) and only
        // exposes level 2 as the lz4mid entry point (LZ4HC_CLEVEL_MIN = 2).
        // We treat 0–2 uniformly as Mid for a simpler "0 = fastest" mapping.
        0..=2 => HcLevelParams {
            strategy: HcCompressionStrategy::Mid,
            max_attempts: 0,
            sufficient_match_len: 0,
            full_optimal_update: false,
        },
        3..=9 => HcLevelParams {
            strategy: HcCompressionStrategy::HashChain,
            max_attempts: 1usize << (level - 1),
            sufficient_match_len: 0,
            full_optimal_update: false,
        },
        10 => HcLevelParams {
            strategy: HcCompressionStrategy::Optimal,
            max_attempts: 96,
            sufficient_match_len: 64,
            full_optimal_update: false,
        },
        11 => HcLevelParams {
            strategy: HcCompressionStrategy::Optimal,
            max_attempts: 512,
            sufficient_match_len: 128,
            full_optimal_update: false,
        },
        // 12, or defensive fallback if `level` was not clamped
        _ => HcLevelParams {
            strategy: HcCompressionStrategy::Optimal,
            max_attempts: 16384,
            sufficient_match_len: LZ4_OPT_NUM,
            full_optimal_update: true,
        },
    }
}

/// Hash table with chain for LZ4 high compression.
///
/// Uses a two-level structure: `dictionary` maps each hash to the most recent input
/// position, and `chain_table` links older positions with the same hash via
/// stored deltas, forming an implicit linked list per hash bucket.
#[derive(Debug)]
struct HashTableHCU32 {
    /// Primary hash table: maps a 15-bit hash of each 4-byte sequence to the
    /// most recent input position (as `u32`) where that hash was seen.
    /// Fixed size of 2^15 entries, matching the output range of `hash_hc`.
    dictionary: Box<[u32; HASHTABLE_SIZE_HC]>,
    /// Chain table: stores a backward delta (as `u16`) at each position,
    /// pointing to the previous position with the same hash. Indexed by
    /// `pos & chain_mask()`. Dynamically sized (power of 2, up to
    /// `MAX_DISTANCE_HC`) based on input length, enabling efficient masking.
    chain_table: Box<[u16]>,
    /// Next input position to be inserted into the hash/chain tables.
    /// Positions below this have already been indexed; the compressor
    /// lazily inserts up to the current search offset on demand.
    next_to_update: usize,
    /// Maximum number of chain links to follow per search. Higher values
    /// yield better compression at the cost of more CPU time. Determined
    /// by the chosen compression level.
    max_attempts: usize,
}

/// A single LZ4 back-reference match.
///
/// Uses `u32` fields (12 bytes total vs 24 with `usize` on 64-bit) to reduce
/// stack pressure in the HC inner loop, which juggles up to 4 `Match` structs.
#[derive(Debug, Clone, Copy)]
struct Match {
    /// Byte position in the input where this match starts (the "current" cursor).
    start_position: u32,
    /// Length of the match in bytes (including the mandatory 4-byte minimum).
    match_length: u32,
    /// Byte position of the earlier occurrence being matched against.
    /// The encoded offset/distance is `start_position - candidate`.
    candidate: u32,
}

impl Match {
    #[inline]
    fn end(&self) -> usize {
        self.start_position as usize + self.match_length as usize
    }

    /// Remove `bytes_to_skip` bytes from the beginning of the match, advancing both
    /// start and reference positions forward while shrinking the match length.
    /// Used to resolve overlaps between consecutive matches.
    fn trim_front(&mut self, bytes_to_skip: usize) {
        self.start_position += bytes_to_skip as u32;
        self.candidate += bytes_to_skip as u32;
        self.match_length = self.match_length.saturating_sub(bytes_to_skip as u32);
    }

    #[inline]
    fn offset(&self) -> u16 {
        self.start_position.wrapping_sub(self.candidate) as u16
    }

    fn encode_to<S: Sink>(&self, input: &[u8], literal_start: usize, output: &mut S) {
        encode_sequence(
            &input[literal_start..self.start_position as usize],
            output,
            self.offset(),
            self.match_length as usize - MINMATCH,
        )
    }
}

/// Count how many consecutive bytes starting at `pos` are all the same value
/// (e.g. every byte is `0xAB`). Callers pass that byte **replicated four times**
/// in the low 32 bits: `let b: u8 = ...; let pattern = u32::from_ne_bytes([b, b, b, b])`
/// so `0xAB` becomes `0xABABABAB`. That word is XOR’d against loaded chunks to find
/// where the run ends; it is widened to `usize` for batch comparison on 32/64-bit.
/// Equivalent to C's LZ4HC_countPattern.
#[inline]
fn count_pattern(input: &[u8], pos: usize, limit: usize, pattern: u32) -> usize {
    let limit = limit.min(input.len());
    let mut cur = pos;

    // Extend 32-bit pattern to usize for batch comparison
    let pattern: usize = if core::mem::size_of::<usize>() == 8 {
        (pattern as usize) | ((pattern as usize) << 32)
    } else {
        pattern as usize
    };

    const STEP: usize = core::mem::size_of::<usize>();
    while cur + STEP <= limit {
        let batch = super::compress::get_batch_arch(input, cur);
        let diff = batch ^ pattern;
        if diff != 0 {
            cur += (diff.trailing_zeros() / 8) as usize;
            return cur - pos;
        }
        cur += STEP;
    }

    // Byte-by-byte tail
    let byte_val = (pattern & 0xFF) as u8; // single repeated byte
    while cur < limit && input[cur] == byte_val {
        cur += 1;
    }

    cur - pos
}

/// Like [`count_pattern`], but walks backward. `pattern` is one byte repeated
/// four times in a `u32` (same encoding as [`count_pattern`]).
/// Equivalent to C's LZ4HC_reverseCountPattern.
#[inline]
fn reverse_count_pattern(input: &[u8], pos: usize, low_limit: usize, pattern: u32) -> usize {
    let mut cur = pos;

    while cur >= low_limit + 4 {
        if super::compress::get_batch(input, cur - 4) != pattern {
            break;
        }
        cur -= 4;
    }

    // Byte-by-byte tail using native endian byte order (matches get_batch)
    let pattern_bytes = pattern.to_ne_bytes();
    let mut byte_idx: usize = 3;
    while cur > low_limit {
        if input[cur - 1] != pattern_bytes[byte_idx] {
            break;
        }
        cur -= 1;
        byte_idx = if byte_idx == 0 { 3 } else { byte_idx - 1 };
    }

    pos - cur
}

/// Read a u32 from a position that may span the boundary between `primary` and `secondary`.
/// When `pos + 4 > primary.len()`, the remaining bytes are read from `secondary[0..]`.
#[inline]
fn read_u32_from_two_slices(primary: &[u8], pos: usize, secondary: &[u8]) -> u32 {
    let remaining = primary.len() - pos;
    if remaining >= 4 {
        super::compress::get_batch(primary, pos)
    } else {
        let mut buf = [0u8; 4];
        buf[..remaining].copy_from_slice(&primary[pos..]);
        buf[remaining..].copy_from_slice(&secondary[..4 - remaining]);
        u32::from_le_bytes(buf)
    }
}

/// Check if two 4-byte sequences starting at the given positions are equal.
#[inline]
fn read_min_match_equals(input: &[u8], pos1: usize, pos2: usize) -> bool {
    super::compress::get_batch(input, pos1) == super::compress::get_batch(input, pos2)
}

/// Check whether the last two bytes of two spans of `length` starting at `pos1` and `pos2` match.
#[inline]
fn end_bytes_match(input: &[u8], pos1: usize, pos2: usize, length: usize) -> bool {
    let tail = length - 1;
    #[cfg(not(feature = "safe-encode"))]
    unsafe {
        (input.as_ptr().add(pos1 + tail) as *const u16).read_unaligned()
            == (input.as_ptr().add(pos2 + tail) as *const u16).read_unaligned()
    }
    #[cfg(feature = "safe-encode")]
    {
        input[pos1 + tail] == input[pos2 + tail] && input[pos1 + tail + 1] == input[pos2 + tail + 1]
    }
}

/// Try to match at a candidate position in `ext_dict`.
/// Returns the total match length (>= MINMATCH) if the first 4 bytes match, or 0.
/// Handles boundary-crossing reads when the candidate is near the end of `ext_dict`.
#[inline]
fn try_ext_dict_match(
    input: &[u8],
    cur: usize,
    match_limit: usize,
    ext_dict: &[u8],
    candidate: usize,
) -> usize {
    let min_match_ok = if candidate + 4 <= ext_dict.len() {
        super::compress::get_batch(ext_dict, candidate) == super::compress::get_batch(input, cur)
    } else if candidate < ext_dict.len() {
        read_u32_from_two_slices(ext_dict, candidate, input)
            == super::compress::get_batch(input, cur)
    } else {
        false
    };
    if !min_match_ok {
        return 0;
    }
    MINMATCH
        + count_forward_ext_dict(
            input,
            cur + MINMATCH,
            ext_dict,
            candidate + MINMATCH,
            match_limit,
        )
}

/// Count matching bytes forward with the reference starting in `ext_dict` and
/// potentially continuing into `input[0..]` (the prefix) when ext_dict is exhausted.
/// `candidate` may already be past `ext_dict` (when the min-match check crossed the boundary).
#[inline]
fn count_forward_ext_dict(
    input: &[u8],
    cur: usize,
    ext_dict: &[u8],
    candidate: usize,
    match_limit: usize,
) -> usize {
    let mut cur = cur;

    if candidate >= ext_dict.len() {
        let prefix_pos = candidate - ext_dict.len();
        return count_same_bytes(input, &mut cur, input, prefix_pos, match_limit);
    }

    let ext_dict_match_len = count_same_bytes(input, &mut cur, ext_dict, candidate, match_limit);

    if candidate + ext_dict_match_len >= ext_dict.len() && cur < match_limit {
        ext_dict_match_len + count_same_bytes(input, &mut cur, input, 0, match_limit)
    } else {
        ext_dict_match_len
    }
}

/// Hash function for high compression
#[inline]
fn hash_hc(batch: u32) -> u32 {
    batch.wrapping_mul(2654435761u32) >> 17
}

#[inline]
fn get_hash_at(input: &[u8], pos: usize) -> usize {
    hash_hc(super::compress::get_batch(input, pos)) as usize
}

/// Count matching bytes forward. Delegates to the shared `count_same_bytes`
/// with `input` as both slices (HC always matches within the same buffer).
#[inline]
fn count_common_bytes(input: &[u8], pos1: usize, pos2: usize, limit: usize) -> usize {
    let mut cur = pos2;
    count_same_bytes(input, &mut cur, input, pos1, input.len().min(limit))
}

/// Find the number of common bytes backward from two positions
#[inline]
fn count_common_bytes_backward(
    input: &[u8],
    mut pos1: usize,
    mut pos2: usize,
    limit1: usize,
    limit2: usize,
) -> usize {
    let mut len = 0;
    let max_back = (pos1 - limit1).min(pos2 - limit2);

    if max_back == 0 {
        return 0;
    }

    // Process usize (8 bytes on 64-bit) at a time, backwards
    const STEP_SIZE: usize = core::mem::size_of::<usize>();
    while len + STEP_SIZE <= max_back {
        let v1 = super::compress::get_batch_arch(input, pos1 - len - STEP_SIZE);
        let v2 = super::compress::get_batch_arch(input, pos2 - len - STEP_SIZE);
        let diff = v1 ^ v2;

        if diff == 0 {
            len += STEP_SIZE;
        } else {
            // Find first differing byte from the end (using leading zeros for backward)
            return len + (diff.to_be().trailing_zeros() / 8) as usize;
        }
    }

    // Update positions to account for bytes already compared in batch loop
    pos1 -= len;
    pos2 -= len;

    // Process remaining 4 bytes if on 64-bit
    #[cfg(target_pointer_width = "64")]
    if len + 4 <= max_back {
        let v1 = super::compress::get_batch(input, pos1 - 4);
        let v2 = super::compress::get_batch(input, pos2 - 4);
        let diff = v1 ^ v2;
        if diff == 0 {
            len += 4;
            pos1 -= 4;
            pos2 -= 4;
        } else {
            return len + (diff.to_be().trailing_zeros() / 8) as usize;
        }
    }

    // Process remaining 2 bytes
    if len + 2 <= max_back {
        if input[pos1 - 2] == input[pos2 - 2] && input[pos1 - 1] == input[pos2 - 1] {
            len += 2;
            pos1 -= 2;
            pos2 -= 2;
        } else if input[pos1 - 1] == input[pos2 - 1] {
            return len + 1;
        } else {
            return len;
        }
    }

    // Process last byte
    if len < max_back && input[pos1 - 1] == input[pos2 - 1] {
        len += 1;
    }

    len
}

/// Compute match length between `candidate` and `cur` in `input`.
/// Uses `end_bytes_match` as a fast rejection filter when a prior match
/// already exists, then counts common bytes forward.
/// Returns 0 if the candidate doesn't match.
#[inline]
fn compute_match_length(
    input: &[u8],
    candidate: usize,
    cur: usize,
    match_limit: usize,
    match_info: &Match,
) -> usize {
    let cant_beat_best = match_info.match_length >= MINMATCH as u32
        && !end_bytes_match(input, candidate, cur, match_info.match_length as usize);
    if cant_beat_best || !read_min_match_equals(input, candidate, cur) {
        return 0;
    }
    MINMATCH + count_common_bytes(input, candidate + MINMATCH, cur + MINMATCH, match_limit)
}

/// Result of pattern/repeat chain optimization inside [`HashTableHCU32::find_longer_match`]
/// (mirrors LZ4HC repeat detection).
enum PatternChainAction {
    /// Continue with normal chain-step logic at the end of the loop body.
    NoAction,
    /// Set `candidate` and restart the search loop iteration (`continue`).
    RetryCandidate(usize),
    /// Exit the search loop (`break`).
    StopSearch,
}

impl HashTableHCU32 {
    #[inline]
    fn new(max_attempts: usize, input_len: usize) -> Self {
        // Dict table: fixed size, hash function already bounds to this range
        let dictionary = vec![0u32; HASHTABLE_SIZE_HC]
            .into_boxed_slice()
            .try_into()
            .unwrap();

        // Chain table: dynamically sized based on input length
        // min(input_len, MAX_DISTANCE_HC), at least 256, must be power of 2
        let chain_size = input_len.clamp(256, MAX_DISTANCE_HC).next_power_of_two();

        Self {
            dictionary,
            chain_table: vec![0u16; chain_size].into_boxed_slice(),
            next_to_update: 0,
            max_attempts,
        }
    }

    /// Reset the table for reuse, re-zeroing both tables.
    /// Avoids reallocation if the existing chain table is large enough.
    #[inline]
    fn reset(&mut self, max_attempts: usize, input_len: usize) {
        let needed_chain_size = input_len.clamp(256, MAX_DISTANCE_HC).next_power_of_two();

        self.dictionary.fill(0);

        // Reuse chain table if big enough, otherwise reallocate
        if self.chain_table.len() >= needed_chain_size {
            self.chain_table[..needed_chain_size].fill(0);
        } else {
            self.chain_table = vec![0u16; needed_chain_size].into_boxed_slice();
        }

        self.next_to_update = 0;
        self.max_attempts = max_attempts;
    }

    /// Prepare the table for a new linked block without clearing existing entries.
    /// Ensures the chain table is `MAX_DISTANCE_HC` so cross-block chain links work,
    /// and advances `next_to_update` past the positions now in `ext_dict`.
    #[cfg(feature = "frame")]
    fn prepare_linked_block(&mut self, max_attempts: usize, block_start: usize) {
        if self.chain_table.len() < MAX_DISTANCE_HC {
            let mut new_chain = vec![0u16; MAX_DISTANCE_HC].into_boxed_slice();
            let old_len = self.chain_table.len();
            for i in 0..old_len {
                new_chain[i] = self.chain_table[i];
            }
            self.chain_table = new_chain;
        }
        self.next_to_update = block_start;
        self.max_attempts = max_attempts;
    }

    /// Subtract `delta` from every absolute position stored in the hash table.
    /// Used when `stream_offset` approaches `u32::MAX / 2` to prevent overflow.
    #[cfg(feature = "frame")]
    fn reposition(&mut self, delta: usize) {
        let delta32 = delta as u32;
        for entry in self.dictionary.iter_mut() {
            *entry = entry.saturating_sub(delta32);
        }
        self.next_to_update = self.next_to_update.saturating_sub(delta);
    }

    /// Mask for chain table indexing (table size is always power of 2)
    #[inline]
    fn chain_mask(&self) -> usize {
        self.chain_table.len() - 1
    }

    /// Check if a candidate is within reachable range
    #[inline]
    fn in_range(&self, candidate: usize, cur_absolute: usize) -> bool {
        candidate < cur_absolute && cur_absolute - candidate <= self.chain_mask()
    }

    /// Advance to next candidate in chain, returning None if exhausted
    #[inline]
    fn advance(&self, candidate: usize, cur_absolute: usize) -> Option<usize> {
        let next = self.next(candidate);
        if next == candidate || !self.in_range(next, cur_absolute) {
            None
        } else {
            Some(next)
        }
    }

    /// Get the next position in the chain for a given offset
    #[inline]
    fn next(&self, pos: usize) -> usize {
        let chain_index = pos & self.chain_mask();
        pos - (self.chain_table[chain_index] as usize)
    }

    /// Get the raw chain delta at a position (equivalent to C's DELTANEXTU16)
    #[inline]
    fn chain_delta(&self, pos: usize) -> u16 {
        let chain_index = pos & self.chain_mask();
        self.chain_table[chain_index]
    }

    #[inline]
    fn add_hash(&mut self, hash: usize, pos: usize) {
        let chain_index = pos & self.chain_mask();
        let delta = pos - self.dictionary[hash] as usize;
        let delta = if delta > self.chain_mask() {
            self.chain_mask()
        } else {
            delta
        };
        self.chain_table[chain_index] = delta as u16;
        self.dictionary[hash] = pos as u32;
    }

    /// Get dictionary slot at hash index (most recent absolute position).
    #[inline]
    fn get_dictionary_at(&self, hash: usize) -> usize {
        self.dictionary[hash] as usize
    }

    /// Set dictionary slot at hash index.
    #[inline]
    fn set_dictionary_at(&mut self, hash: usize, pos: usize) {
        self.dictionary[hash] = pos as u32;
    }

    /// Set chain value at position
    #[inline]
    fn set_chain(&mut self, pos: usize, delta: u16) {
        let chain_index = pos & self.chain_mask();
        self.chain_table[chain_index] = delta;
    }

    /// Insert hashes for all positions up to the given local offset.
    /// Positions stored in the hash table are absolute (`local_pos + stream_offset`).
    #[inline]
    fn insert(&mut self, cur: u32, input: &[u8], stream_offset: usize) {
        let cur_absolute = cur as usize + stream_offset;
        for absolute_position in self.next_to_update..cur_absolute {
            let local_pos = absolute_position - stream_offset;
            self.add_hash(get_hash_at(input, local_pos), absolute_position);
        }
        self.next_to_update = cur_absolute;
    }

    /// Pattern / repeat chain optimization when `chain_delta(candidate) == 1` and
    /// `chain_pos == 0`. Returns an action for the outer search loop.
    #[allow(clippy::too_many_arguments)]
    fn pattern_chain_action(
        &self,
        input: &[u8],
        cur: usize,
        match_limit: usize,
        cur_absolute: usize,
        stream_offset: usize,
        candidate: usize,
        chain_pos: usize,
        repeat: &mut u8,
        source_pattern_length: &mut usize,
        best_len: &mut usize,
        best_offset: &mut u16,
    ) -> PatternChainAction {
        if self.chain_delta(candidate) != 1 || chain_pos != 0 {
            return PatternChainAction::NoAction;
        }

        let match_candidate = candidate.wrapping_sub(1);

        if *repeat == 0 {
            let pattern = super::compress::get_batch(input, cur);
            if (pattern & 0xFFFF) == (pattern >> 16) && (pattern & 0xFF) == (pattern >> 24) {
                *repeat = 1;
                *source_pattern_length = count_pattern(input, cur + 4, match_limit, pattern) + 4;
            } else {
                *repeat = 2;
            }
        }

        if *repeat != 1 {
            return PatternChainAction::NoAction;
        }

        if match_candidate >= cur_absolute
            || cur_absolute - match_candidate > self.chain_mask()
            || match_candidate < stream_offset
        {
            return PatternChainAction::NoAction;
        }

        let match_candidate_relative = match_candidate - stream_offset;
        let pattern = super::compress::get_batch(input, cur);
        if match_candidate_relative + 4 > input.len()
            || super::compress::get_batch(input, match_candidate_relative) != pattern
        {
            return PatternChainAction::NoAction;
        }

        let forward_pattern_len =
            count_pattern(input, match_candidate_relative + 4, match_limit, pattern) + 4;
        let back_length = reverse_count_pattern(input, match_candidate_relative, 0, pattern);
        let segment_length = back_length + forward_pattern_len;

        if segment_length >= *source_pattern_length && forward_pattern_len <= *source_pattern_length
        {
            let new_candidate_relative =
                match_candidate_relative + forward_pattern_len - *source_pattern_length;
            let new_ref_abs = new_candidate_relative + stream_offset;
            if cur_absolute > new_ref_abs && cur_absolute - new_ref_abs <= self.chain_mask() {
                return PatternChainAction::RetryCandidate(new_ref_abs);
            }
        } else {
            let new_candidate_relative = match_candidate_relative - back_length;
            let new_ref_abs = new_candidate_relative + stream_offset;
            if cur_absolute > new_ref_abs && cur_absolute - new_ref_abs <= self.chain_mask() {
                let max_match_length = segment_length.min(*source_pattern_length);
                if max_match_length > *best_len {
                    *best_len = max_match_length;
                    *best_offset = (cur_absolute - new_ref_abs) as u16;
                }
                let dist = self.chain_delta(new_ref_abs) as usize;
                if dist == 0 || dist > new_ref_abs {
                    return PatternChainAction::StopSearch;
                }
                return PatternChainAction::RetryCandidate(new_ref_abs - dist);
            }
        }

        PatternChainAction::NoAction
    }
}

/// Insert `cur` into the hash/chain tables, then search the chain for a match
/// longer than `min_match_length`. Used by the optimal parser.
#[inline]
fn find_longer_hash_chain_match(
    hash_table: &mut HashTableHCU32,
    input: &[u8],
    cur: usize,
    match_limit: usize,
    min_match_length: usize,
    ext_dict: &[u8],
    stream_offset: usize,
) -> (u32, u16) {
    hash_table.insert(cur as u32, input, stream_offset);

    let cur_absolute = cur + stream_offset;
    let ext_dict_stream_offset = stream_offset - ext_dict.len();

    let mut best_match_length = min_match_length;
    let mut best_offset: u16 = 0;
    let mut chain_pos = 0usize;

    let mut repeat = 0u8;
    let mut source_pattern_length = 0usize;

    let mut candidate = hash_table.get_dictionary_at(get_hash_at(input, cur));

    for _ in 0..hash_table.max_attempts {
        if !hash_table.in_range(candidate, cur_absolute) {
            break;
        }

        let mut match_length = 0usize;

        if candidate >= stream_offset {
            let candidate_relative = candidate - stream_offset;

            if (best_match_length < MINMATCH
                || end_bytes_match(input, candidate_relative, cur, best_match_length))
                && read_min_match_equals(input, candidate_relative, cur)
            {
                match_length = MINMATCH
                    + count_common_bytes(
                        input,
                        candidate_relative + MINMATCH,
                        cur + MINMATCH,
                        match_limit,
                    );
                if match_length > best_match_length {
                    best_match_length = match_length;
                    best_offset = (cur_absolute - candidate) as u16;
                }
            }

            if match_length == best_match_length
                && match_length >= MINMATCH
                && candidate + best_match_length <= cur_absolute
            {
                const K_TRIGGER: i32 = 4;
                let mut distance_to_next = 1u16;
                let end = (best_match_length - MINMATCH + 1) as i32;
                let mut acceleration = 1 << K_TRIGGER;
                let mut pos = 0i32;
                while pos < end {
                    let candidate_dist =
                        hash_table.chain_delta(candidate.wrapping_add(pos as usize));
                    let step = acceleration >> K_TRIGGER;
                    acceleration += 1;
                    if candidate_dist > distance_to_next {
                        distance_to_next = candidate_dist;
                        chain_pos = pos as usize;
                        acceleration = 1 << K_TRIGGER;
                    }
                    pos += step;
                }
                if distance_to_next > 1 {
                    if (distance_to_next as usize) > candidate {
                        break;
                    }
                    candidate -= distance_to_next as usize;
                    continue;
                }
            }

            match hash_table.pattern_chain_action(
                input,
                cur,
                match_limit,
                cur_absolute,
                stream_offset,
                candidate,
                chain_pos,
                &mut repeat,
                &mut source_pattern_length,
                &mut best_match_length,
                &mut best_offset,
            ) {
                PatternChainAction::RetryCandidate(new_abs) => {
                    candidate = new_abs;
                    continue;
                }
                PatternChainAction::StopSearch => break,
                PatternChainAction::NoAction => {}
            }
        } else if !ext_dict.is_empty() && candidate >= ext_dict_stream_offset {
            match_length = try_ext_dict_match(
                input,
                cur,
                match_limit,
                ext_dict,
                candidate - ext_dict_stream_offset,
            );
            if match_length > best_match_length {
                best_match_length = match_length;
                best_offset = (cur_absolute - candidate) as u16;
            }
        }

        let delta = hash_table.chain_delta(candidate + chain_pos) as usize;
        if delta == 0 || delta > candidate {
            break;
        }
        candidate -= delta;
    }

    if best_match_length <= min_match_length {
        (0, 0)
    } else {
        (best_match_length as u32, best_offset)
    }
}

/// Update the hash chain for the first match found at `cur`.
///
/// This mirrors the pre-hash step from the LZ4 HC reference: positions inside the
/// first accepted match are inserted eagerly so later searches can skip ahead.
fn prehash_first_match(
    hash_table: &mut HashTableHCU32,
    input: &[u8],
    cur_absolute: usize,
    stream_offset: usize,
    first_match_length: usize,
    delta: usize,
) {
    let mut hash_pos = cur_absolute;
    let end_pos = cur_absolute + first_match_length - 3;

    while hash_pos < end_pos - delta {
        hash_table.set_chain(hash_pos, delta as u16);
        hash_pos += 1;
    }

    loop {
        hash_table.set_chain(hash_pos, delta as u16);
        let local_hash_pos = hash_pos - stream_offset;
        hash_table.set_dictionary_at(get_hash_at(input, local_hash_pos), hash_pos);
        hash_pos += 1;
        if hash_pos >= end_pos {
            break;
        }
    }

    hash_table.next_to_update = end_pos;
}

/// Insert `cur` into the hash/chain tables, then search the chain for the
/// longest match starting at `cur`.
fn find_best_hash_chain_match(
    hash_table: &mut HashTableHCU32,
    input: &[u8],
    cur: usize,
    match_limit: usize,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Option<Match> {
    let mut best_match = Match {
        start_position: cur as u32,
        match_length: 0,
        candidate: 0,
    };
    let mut first_match_delta = 0usize;
    let mut first_match_length = 0usize;

    let cur_absolute = cur + stream_offset;
    let ext_dict_stream_offset = stream_offset - ext_dict.len();

    hash_table.insert(cur as u32, input, stream_offset);

    let mut candidate = hash_table.get_dictionary_at(get_hash_at(input, cur));

    for attempt in 0..hash_table.max_attempts {
        if !hash_table.in_range(candidate, cur_absolute) {
            break;
        }

        let match_length = if candidate >= stream_offset {
            compute_match_length(
                input,
                candidate - stream_offset,
                cur,
                match_limit,
                &best_match,
            )
        } else if !ext_dict.is_empty() && candidate >= ext_dict_stream_offset {
            try_ext_dict_match(
                input,
                cur,
                match_limit,
                ext_dict,
                candidate - ext_dict_stream_offset,
            )
        } else {
            0
        };

        if match_length as u32 > best_match.match_length {
            let distance = cur_absolute - candidate;
            best_match.candidate = (cur as u32).wrapping_sub(distance as u32);
            best_match.match_length = match_length as u32;
        }

        if attempt == 0 && match_length > 0 {
            first_match_length = match_length;
            first_match_delta = cur_absolute - candidate;
        }

        let Some(next_candidate) = hash_table.advance(candidate, cur_absolute) else {
            break;
        };
        candidate = next_candidate;
    }

    if first_match_length != 0 {
        prehash_first_match(
            hash_table,
            input,
            cur_absolute,
            stream_offset,
            first_match_length,
            first_match_delta,
        );
    }

    if best_match.match_length == 0 {
        None
    } else {
        Some(best_match)
    }
}

/// Insert `cur` into the hash/chain tables, then search the chain for a match
/// longer than `min_match_length`, extending both forward and backward.
fn find_wider_hash_chain_match(
    hash_table: &mut HashTableHCU32,
    input: &[u8],
    cur: usize,
    start_limit: usize,
    match_limit: usize,
    min_match_length: usize,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Option<Match> {
    let mut best_match = Match {
        start_position: cur as u32,
        match_length: min_match_length as u32,
        candidate: 0,
    };

    let cur_absolute = cur + stream_offset;
    let ext_dict_stream_offset = stream_offset - ext_dict.len();
    let look_back_length = cur - start_limit;

    hash_table.insert(cur as u32, input, stream_offset);

    let mut candidate = hash_table.get_dictionary_at(get_hash_at(input, cur));

    for _ in 0..hash_table.max_attempts {
        if !hash_table.in_range(candidate, cur_absolute) {
            break;
        }

        if candidate >= stream_offset {
            let candidate_relative = candidate - stream_offset;
            let can_check_tail = best_match.match_length >= MINMATCH as u32
                && candidate_relative >= look_back_length;
            if (!can_check_tail
                || end_bytes_match(
                    input,
                    candidate_relative - look_back_length,
                    start_limit,
                    best_match.match_length as usize,
                ))
                && read_min_match_equals(input, candidate_relative, cur)
            {
                let forward_length = MINMATCH
                    + count_common_bytes(
                        input,
                        candidate_relative + MINMATCH,
                        cur + MINMATCH,
                        match_limit,
                    );
                let backward_length =
                    count_common_bytes_backward(input, candidate_relative, cur, 0, start_limit);
                let match_length = backward_length + forward_length;

                if match_length as u32 > best_match.match_length {
                    best_match.match_length = match_length as u32;
                    let distance = cur_absolute - candidate;
                    best_match.candidate =
                        ((cur - backward_length) as u32).wrapping_sub(distance as u32);
                    best_match.start_position = (cur - backward_length) as u32;
                }
            }
        } else if !ext_dict.is_empty() && candidate >= ext_dict_stream_offset {
            let candidate_relative = candidate - ext_dict_stream_offset;
            let match_length =
                try_ext_dict_match(input, cur, match_limit, ext_dict, candidate_relative);
            if match_length as u32 > best_match.match_length {
                best_match.match_length = match_length as u32;
                let distance = cur_absolute - candidate;
                best_match.candidate = (cur as u32).wrapping_sub(distance as u32);
                best_match.start_position = cur as u32;
            }
        }

        let Some(next_candidate) = hash_table.advance(candidate, cur_absolute) else {
            break;
        };
        candidate = next_candidate;
    }

    if best_match.match_length as usize == min_match_length {
        None
    } else {
        Some(best_match)
    }
}

/// Optimal parsing state for a single position.
/// Matches C's LZ4HC_optimal_t layout (4x i32 = 16 bytes).
/// Using i32 for match offset/length instead of u16 avoids costly widening conversions
/// on every access in the hot optimal-parsing loop (15-20% regression with u16).
/// The 4099-entry optimal-parsing buffer is ~64KB.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct OptimalState {
    /// Best known encoded size (byte-cost model) to reach this position.
    path_cost: i32,
    /// Length of the literal run immediately before this state (optimal-parse bookkeeping).
    lit_len: i32,
    /// Copy offset for the sequence ending here; `0` means literal step.
    match_offset: i32,
    /// Match length for this step; `1` means a single literal byte.
    match_len: i32,
}

impl OptimalState {
    const SENTINEL: Self = Self {
        path_cost: i32::MAX,
        lit_len: 0,
        match_offset: 0,
        match_len: 0,
    };
}

/// Calculate the cost in bytes of encoding literals
#[inline]
fn literals_price(lit_len: i32) -> i32 {
    let mut price = lit_len;
    if lit_len >= RUN_MASK as i32 {
        price += 1 + (lit_len - RUN_MASK as i32) / 255;
    }
    price
}

/// Calculate the cost in bytes of encoding a sequence (literals + match)
#[inline]
fn sequence_price(lit_len: i32, match_len: i32) -> i32 {
    // token + 16-bit offset
    let mut price: i32 = 1 + 2;

    // literal length encoding
    price += literals_price(lit_len);

    // match length encoding (match_len >= MINMATCH)
    let match_length_code = match_len - MINMATCH as i32;
    if match_length_code >= 15 {
        price += 1 + (match_length_code - 15) / 255;
    }

    price
}

/// A reusable compression table for the HC algorithm that avoids re-allocating
/// internal hash tables on every call.
///
/// This is useful when compressing many inputs in a loop (e.g. frame blocks).
/// Create one table and pass it to [`compress_hc_with_table`] repeatedly.
///
/// The table automatically selects the right internal variant (mid vs HC)
/// based on the compression level, upgrading transparently when needed.
///
/// # Example
/// ```
/// use lz4_flex::block::{compress_hc_to_vec_with_table, CompressTableHC};
///
/// let mut table = CompressTableHC::new();
/// for input in [b"block one".as_slice(), b"block two".as_slice()] {
///     let compressed = compress_hc_to_vec_with_table(input, 9, &mut table);
/// }
/// ```
pub struct CompressTableHC {
    inner: CompressTableHCInner,
}

enum CompressTableHCInner {
    Mid(HashTableMid),
    HC(HashTableHCU32),
}

impl Default for CompressTableHC {
    fn default() -> Self {
        Self::new()
    }
}

impl CompressTableHC {
    /// Create a new table. The internal variant is lazily chosen on first use.
    pub fn new() -> Self {
        CompressTableHC {
            inner: CompressTableHCInner::Mid(HashTableMid::new()),
        }
    }

    /// Get (or create) the Mid table, resetting it for a fresh block.
    fn reset_mid(&mut self) -> &mut HashTableMid {
        if !matches!(self.inner, CompressTableHCInner::Mid(_)) {
            self.inner = CompressTableHCInner::Mid(HashTableMid::new());
        }
        match &mut self.inner {
            CompressTableHCInner::Mid(mid) => {
                mid.reset();
                mid
            }
            _ => unreachable!(),
        }
    }

    /// Get (or create) the HC table, resetting it for a fresh block.
    fn reset_hc(&mut self, max_attempts: usize, input_len: usize) -> &mut HashTableHCU32 {
        if let CompressTableHCInner::HC(ht) = &mut self.inner {
            ht.reset(max_attempts, input_len);
        } else {
            self.inner = CompressTableHCInner::HC(HashTableHCU32::new(max_attempts, input_len));
        }
        match &mut self.inner {
            CompressTableHCInner::HC(ht) => ht,
            _ => unreachable!(),
        }
    }

    /// Prepare the table for a new linked block without clearing existing entries.
    /// Called by `FrameEncoder` between blocks in linked mode.
    ///
    /// `params` must be [`hc_level_params`] with the same clamped level used for the following
    /// [`compress_hc_linked`] call (typically `hc_level_params(level.min(12))`).
    #[cfg(feature = "frame")]
    pub(crate) fn prepare_linked_block(&mut self, params: HcLevelParams, block_start: usize) {
        match params.strategy {
            HcCompressionStrategy::Mid => match &mut self.inner {
                CompressTableHCInner::Mid(mid) => {
                    mid.prepare_linked_block();
                }
                _ => {
                    self.inner = CompressTableHCInner::Mid(HashTableMid::new());
                }
            },
            HcCompressionStrategy::HashChain | HcCompressionStrategy::Optimal => {
                let max_attempts = params.max_attempts;
                match &mut self.inner {
                    CompressTableHCInner::HC(ht) => {
                        ht.prepare_linked_block(max_attempts, block_start);
                    }
                    _ => {
                        let mut ht = HashTableHCU32::new(max_attempts, MAX_DISTANCE_HC);
                        ht.prepare_linked_block(max_attempts, block_start);
                        self.inner = CompressTableHCInner::HC(ht);
                    }
                }
            }
        }
    }

    /// Subtract `delta` from all stored positions to prevent overflow.
    #[cfg(feature = "frame")]
    pub(crate) fn reposition(&mut self, delta: usize) {
        match &mut self.inner {
            CompressTableHCInner::HC(ht) => ht.reposition(delta),
            CompressTableHCInner::Mid(mid) => mid.reposition(delta),
        }
    }
}

/// Compress input data using the LZ4 high compression algorithm.
///
/// Allocates a fresh internal table on every call. For repeated compression
/// (e.g. compressing many blocks in a loop), prefer [`compress_hc_with_table`]
/// with a reusable [`CompressTableHC`] to avoid repeated allocation.
///
/// # Compression levels
/// - **Levels 1-2**: lz4mid intermediate algorithm
/// - **Levels 3-9**: HC hash chain algorithm with increasing search depth
/// - **Levels 10-12**: Optimal parsing (dynamic programming) for maximum compression
///
/// # Example
/// ```ignore
/// use lz4_flex::block::compress_hc;
/// let input = b"Hello, this is some data to compress!";
/// let mut output = vec![0u8; input.len() * 2];
/// let size = compress_hc(input, &mut output, 9).unwrap(); // HC algorithm
/// let size = compress_hc(input, &mut output, 12).unwrap(); // Optimal algorithm
/// ```
pub fn compress_hc(
    input: &[u8],
    output: &mut impl Sink,
    level: u8,
) -> Result<usize, CompressError> {
    let level = level.min(12);
    let params = hc_level_params(level);

    match params.strategy {
        HcCompressionStrategy::Optimal => {
            let mut ht = HashTableHCU32::new(params.max_attempts, input.len());
            compress_opt_internal(input, 0, output, params, &mut ht, &[], 0)
        }
        HcCompressionStrategy::HashChain => {
            let mut ht = HashTableHCU32::new(params.max_attempts, input.len());
            compress_hash_chain_internal(input, 0, output, &mut ht, &[], 0)
        }
        HcCompressionStrategy::Mid => {
            let mut table = HashTableMid::new();
            compress_mid_internal(input, 0, output, &mut table, &[], 0)
        }
    }
}

/// Compress input data using the LZ4 high compression algorithm, reusing a
/// [`CompressTableHC`] to avoid re-allocating internal hash tables.
///
/// The table is automatically reset before each call. If the level changes
/// between calls (e.g. mid vs HC), the table is transparently upgraded.
///
/// See [`compress_hc`] for compression level details.
///
/// # Example
/// ```ignore
/// use lz4_flex::block::{compress_hc_with_table, get_maximum_output_size, CompressTableHC};
///
/// let mut table = CompressTableHC::new();
/// let input = b"Hello, this is some data to compress with HC!";
/// let mut output = vec![0u8; get_maximum_output_size(input.len())];
/// let n = compress_hc_with_table(input, &mut output, 9, &mut table).unwrap();
/// ```
pub fn compress_hc_with_table(
    input: &[u8],
    output: &mut impl Sink,
    level: u8,
    table: &mut CompressTableHC,
) -> Result<usize, CompressError> {
    let level = level.min(12);
    let params = hc_level_params(level);

    match params.strategy {
        HcCompressionStrategy::Mid => {
            let mid = table.reset_mid();
            compress_mid_internal(input, 0, output, mid, &[], 0)
        }
        HcCompressionStrategy::HashChain => {
            let ht = table.reset_hc(params.max_attempts, input.len());
            compress_hash_chain_internal(input, 0, output, ht, &[], 0)
        }
        HcCompressionStrategy::Optimal => {
            let ht = table.reset_hc(params.max_attempts, input.len());
            compress_opt_internal(input, 0, output, params, ht, &[], 0)
        }
    }
}

/// Compress with HC using linked-block mode. Called by the frame encoder.
/// `input` includes the prefix at `[0..input_pos]`, current block at `[input_pos..]`.
/// `table` must have been prepared via [`CompressTableHC::prepare_linked_block`] with the same
/// `params` (typically one [`hc_level_params`] call per block, shared with prepare).
#[cfg(feature = "frame")]
pub(crate) fn compress_hc_linked(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    params: HcLevelParams,
    table: &mut CompressTableHC,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    match params.strategy {
        HcCompressionStrategy::Mid => {
            let mid = match &mut table.inner {
                CompressTableHCInner::Mid(mid) => mid,
                _ => unreachable!(
                    "prepare_linked_block should have ensured Mid variant for mid levels"
                ),
            };
            compress_mid_internal(input, input_pos, output, mid, ext_dict, stream_offset)
        }
        HcCompressionStrategy::HashChain => {
            let ht = match &mut table.inner {
                CompressTableHCInner::HC(ht) => ht,
                _ => unreachable!(
                    "prepare_linked_block should have ensured HC variant for HC levels"
                ),
            };
            compress_hash_chain_internal(input, input_pos, output, ht, ext_dict, stream_offset)
        }
        HcCompressionStrategy::Optimal => {
            let ht = match &mut table.inner {
                CompressTableHCInner::HC(ht) => ht,
                _ => unreachable!(
                    "prepare_linked_block should have ensured HC variant for optimal levels"
                ),
            };
            compress_opt_internal(
                input,
                input_pos,
                output,
                params,
                ht,
                ext_dict,
                stream_offset,
            )
        }
    }
}

/// Compress input data using the LZ4 high compression algorithm, returning a Vec.
///
/// This is a convenience function that allocates the output buffer internally.
/// See [`compress_hc`] for details on the compression algorithm and levels.
///
/// # Arguments
/// * `input` - The input data to compress
/// * `level` - Compression level (1-12), higher means better compression but slower
///
/// # Returns
/// A Vec containing the compressed data
///
/// # Example
/// ```
/// use lz4_flex::block::compress_hc_to_vec;
/// let input = b"Hello, this is some data to compress!";
/// let compressed = compress_hc_to_vec(input, 9); // HC algorithm
/// let compressed = compress_hc_to_vec(input, 12); // Optimal algorithm
/// ```
pub fn compress_hc_to_vec(input: &[u8], level: u8) -> Vec<u8> {
    let max_size = crate::block::compress::get_maximum_output_size(input.len());
    #[cfg(feature = "safe-encode")]
    {
        let mut output = vec![0u8; max_size];
        let mut sink = SliceSink::new(&mut output, 0);
        let compressed_size = compress_hc(input, &mut sink, level).unwrap();
        output.truncate(compressed_size);
        output
    }
    #[cfg(not(feature = "safe-encode"))]
    {
        let mut output = Vec::with_capacity(max_size);
        let compressed_size =
            compress_hc(input, &mut PtrSink::from_vec(&mut output, 0), level).unwrap();
        unsafe {
            output.set_len(compressed_size);
        }
        output.shrink_to_fit();
        output
    }
}

/// Compress input data using the LZ4 high compression algorithm, returning a Vec
/// and reusing a [`CompressTableHC`].
///
/// This is the reusable-table variant of [`compress_hc_to_vec`]. See
/// [`compress_hc_with_table`] for details on table reuse.
///
/// # Example
/// ```
/// use lz4_flex::block::{compress_hc_to_vec_with_table, CompressTableHC};
///
/// let mut table = CompressTableHC::new();
/// let compressed = compress_hc_to_vec_with_table(b"data to compress", 9, &mut table);
/// ```
pub fn compress_hc_to_vec_with_table(
    input: &[u8],
    level: u8,
    table: &mut CompressTableHC,
) -> Vec<u8> {
    let max_size = crate::block::compress::get_maximum_output_size(input.len());
    #[cfg(feature = "safe-encode")]
    {
        let mut output = vec![0u8; max_size];
        let mut sink = SliceSink::new(&mut output, 0);
        let compressed_size = compress_hc_with_table(input, &mut sink, level, table).unwrap();
        output.truncate(compressed_size);
        output
    }
    #[cfg(not(feature = "safe-encode"))]
    {
        let mut output = Vec::with_capacity(max_size);
        let compressed_size =
            compress_hc_with_table(input, &mut PtrSink::from_vec(&mut output, 0), level, table)
                .unwrap();
        unsafe {
            output.set_len(compressed_size);
        }
        output.shrink_to_fit();
        output
    }
}

// ============================================================================
// LZ4MID - Intermediate compression (levels 1-2)
// Uses two hash tables (4-byte and 8-byte) for better compression than fast
// algorithm while being faster than HC.
// ============================================================================

/// Hash table for lz4mid algorithm — two tables keyed by 4-byte and 8-byte input sequences.
struct HashTableMid {
    table_4byte: Box<[u32; LZ4MID_HASHTABLE_SIZE]>,
    table_8byte: Box<[u32; LZ4MID_HASHTABLE_SIZE]>,
}

impl HashTableMid {
    fn new() -> Self {
        HashTableMid {
            table_4byte: vec![0u32; LZ4MID_HASHTABLE_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            table_8byte: vec![0u32; LZ4MID_HASHTABLE_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
        }
    }

    /// Reset the table for reuse by zeroing both hash tables.
    fn reset(&mut self) {
        self.table_4byte.fill(0);
        self.table_8byte.fill(0);
    }

    /// Prepare the table for a new linked block without clearing entries.
    #[cfg(feature = "frame")]
    fn prepare_linked_block(&mut self) {
        // Hash tables persist; no action needed since entries are absolute positions.
    }

    /// Subtract `delta` from every absolute position stored in both hash tables.
    #[cfg(feature = "frame")]
    fn reposition(&mut self, delta: usize) {
        let delta32 = delta as u32;
        for entry in self.table_4byte.iter_mut() {
            *entry = entry.saturating_sub(delta32);
        }
        for entry in self.table_8byte.iter_mut() {
            *entry = entry.saturating_sub(delta32);
        }
    }

    #[inline]
    fn insert_4byte_hash(&mut self, input: &[u8], pos: usize, stream_offset: usize) {
        let hash = get_hash4_mid(input, pos);
        self.table_4byte[hash] = (pos + stream_offset) as u32;
    }

    #[inline]
    fn insert_8byte_hash(&mut self, input: &[u8], pos: usize, stream_offset: usize) {
        let hash = get_hash8_mid(input, pos);
        self.table_8byte[hash] = (pos + stream_offset) as u32;
    }

    /// Insert hashes near both ends of a just-encoded match so future searches can
    /// find overlapping sequences.
    fn insert_match_hashes(
        &mut self,
        input: &[u8],
        match_start: usize,
        match_end: usize,
        input_end: usize,
        stream_offset: usize,
    ) {
        // The furthest read is an 8-byte hash at match_end - 2, needing match_end + 6 <= input_end.
        // Since match_start <= match_end - MINMATCH, all match_start positions are also safe.
        if match_end + 6 > input_end {
            return;
        }

        // Near match start
        self.insert_8byte_hash(input, match_start + 1, stream_offset);
        self.insert_8byte_hash(input, match_start + 2, stream_offset);
        self.insert_4byte_hash(input, match_start + 1, stream_offset);

        // Near match end
        if match_end >= 5 {
            self.insert_8byte_hash(input, match_end - 5, stream_offset);
        }
        self.insert_8byte_hash(input, match_end - 3, stream_offset);
        self.insert_8byte_hash(input, match_end - 2, stream_offset);
        self.insert_4byte_hash(input, match_end - 2, stream_offset);
        self.insert_4byte_hash(input, match_end - 1, stream_offset);
    }
}

/// 4-byte hash for lz4mid (same multiplier as fast algorithm)
#[inline]
fn get_hash4_mid(input: &[u8], pos: usize) -> usize {
    let batch = super::compress::get_batch(input, pos);
    (batch.wrapping_mul(2654435761) >> (32 - LZ4MID_HASH_LOG)) as usize
}

/// 8-byte hash for lz4mid (hashes lower 56 bits for longer match detection)
#[inline]
fn get_hash8_mid(input: &[u8], pos: usize) -> usize {
    // Use get_batch_arch for the raw read (eliminates bounds check in unsafe mode),
    // then convert to u64 for the 56-bit hash computation.
    #[cfg(target_pointer_width = "64")]
    {
        let batch = super::compress::get_batch_arch(input, pos) as u64;
        let batch_56bit = batch.to_le() << 8;
        ((batch_56bit.wrapping_mul(58295818150454627)) >> (64 - LZ4MID_HASH_LOG)) as usize
    }
    #[cfg(not(target_pointer_width = "64"))]
    {
        let batch = u64::from_le_bytes(input[pos..pos + 8].try_into().unwrap());
        let batch_56bit = batch << 8;
        ((batch_56bit.wrapping_mul(58295818150454627)) >> (64 - LZ4MID_HASH_LOG)) as usize
    }
}

/// Resolve an absolute hash table position to a source slice and local index.
/// Returns `None` if the candidate is out of range or unreachable.
/// Returns `(source, local_index, distance)` on success.
#[inline]
fn resolve_candidate<'a>(
    candidate: usize,
    cur_absolute: usize,
    input: &'a [u8],
    stream_offset: usize,
    ext_dict: &'a [u8],
    ext_dict_stream_offset: usize,
) -> Option<(&'a [u8], usize, usize)> {
    let distance = cur_absolute.wrapping_sub(candidate);
    if distance == 0 || distance > MAX_DISTANCE {
        return None;
    }
    if candidate >= stream_offset {
        Some((input, candidate - stream_offset, distance))
    } else if !ext_dict.is_empty() && candidate >= ext_dict_stream_offset {
        Some((ext_dict, candidate - ext_dict_stream_offset, distance))
    } else {
        None
    }
}

/// Internal lz4mid compression.
/// `input_pos` is where the current block starts (positions before it are prefix).
/// `ext_dict` and `stream_offset` support linked block mode.
fn compress_mid_internal(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    table: &mut HashTableMid,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    let output_start = output.pos();

    if input.len() - input_pos < MFLIMIT + 1 {
        handle_last_literals(output, &input[input_pos..]);
        return Ok(output.pos() - output_start);
    }

    let ext_dict_stream_offset = stream_offset - ext_dict.len();

    let mut cur = input_pos;
    let mut literal_start = input_pos;
    let input_end = input.len();
    // Inclusive max main-loop `cur`: at least `MFLIMIT` bytes remain from `cur` to `input_end`.
    let end_pos_check = input_end.saturating_sub(MFLIMIT);
    // Exclusive end for extending matches: last `END_OFFSET` bytes are handled as literals/trailer.
    let match_limit = input_end - END_OFFSET;

    while cur <= end_pos_check {
        let cur_absolute = cur + stream_offset;

        // Try 8-byte hash first
        let hash_8byte = get_hash8_mid(input, cur);
        let candidate_8byte = table.table_8byte[hash_8byte] as usize;
        table.table_8byte[hash_8byte] = cur_absolute as u32;

        if let Some((source_8byte, candidate_8byte_pos, distance_8byte)) = resolve_candidate(
            candidate_8byte,
            cur_absolute,
            input,
            stream_offset,
            ext_dict,
            ext_dict_stream_offset,
        ) {
            let mut probe = cur;
            let match_len = count_same_bytes(
                input,
                &mut probe,
                source_8byte,
                candidate_8byte_pos,
                match_limit,
            );
            if match_len >= MINMATCH {
                let mut match_cur = cur;
                let mut candidate = candidate_8byte_pos;
                backtrack_match(
                    input,
                    &mut match_cur,
                    literal_start,
                    source_8byte,
                    &mut candidate,
                );
                let match_len =
                    count_same_bytes(input, &mut match_cur, source_8byte, candidate, match_limit);
                let match_start = match_cur - match_len;
                let offset = distance_8byte as u16;

                table.insert_match_hashes(input, match_start, match_cur, input_end, stream_offset);
                encode_sequence(
                    &input[literal_start..match_start],
                    output,
                    offset,
                    match_len - MINMATCH,
                );

                cur = match_cur;
                literal_start = cur;
                continue;
            }
        }

        // Try 4-byte hash
        let hash_4byte = get_hash4_mid(input, cur);
        let candidate_4byte = table.table_4byte[hash_4byte] as usize;
        table.table_4byte[hash_4byte] = cur_absolute as u32;

        if let Some((source_4byte, candidate_4byte_pos, distance_4byte)) = resolve_candidate(
            candidate_4byte,
            cur_absolute,
            input,
            stream_offset,
            ext_dict,
            ext_dict_stream_offset,
        ) {
            let mut probe = cur;
            let match_len = count_same_bytes(
                input,
                &mut probe,
                source_4byte,
                candidate_4byte_pos,
                match_limit,
            );
            if match_len >= MINMATCH {
                let mut best_cur = cur;
                let mut best_source: &[u8] = source_4byte;
                let mut best_candidate = candidate_4byte_pos;
                let mut best_len = match_len;
                let mut best_distance = distance_4byte;

                if cur < end_pos_check {
                    let hash_8byte_next = get_hash8_mid(input, cur + 1);
                    let candidate_8byte_next = table.table_8byte[hash_8byte_next] as usize;
                    if let Some((
                        source_8byte_next,
                        candidate_8byte_next_pos,
                        distance_8byte_next,
                    )) = resolve_candidate(
                        candidate_8byte_next,
                        cur_absolute + 1,
                        input,
                        stream_offset,
                        ext_dict,
                        ext_dict_stream_offset,
                    ) {
                        let mut probe_next = cur + 1;
                        let len_next = count_same_bytes(
                            input,
                            &mut probe_next,
                            source_8byte_next,
                            candidate_8byte_next_pos,
                            match_limit,
                        );
                        if len_next > best_len {
                            table.table_8byte[hash_8byte_next] = (cur + 1 + stream_offset) as u32;
                            best_cur = cur + 1;
                            best_source = source_8byte_next;
                            best_candidate = candidate_8byte_next_pos;
                            best_len = len_next;
                            best_distance = distance_8byte_next;
                        }
                    }
                }
                let _ = best_len;

                let mut match_cur = best_cur;
                let mut candidate = best_candidate;
                backtrack_match(
                    input,
                    &mut match_cur,
                    literal_start,
                    best_source,
                    &mut candidate,
                );
                let match_len =
                    count_same_bytes(input, &mut match_cur, best_source, candidate, match_limit);
                let match_start = match_cur - match_len;
                let offset = best_distance as u16;

                table.insert_match_hashes(input, match_start, match_cur, input_end, stream_offset);
                encode_sequence(
                    &input[literal_start..match_start],
                    output,
                    offset,
                    match_len - MINMATCH,
                );

                cur = match_cur;
                literal_start = cur;
                continue;
            }
        }

        // No match - skip with acceleration
        cur += 1 + ((cur - literal_start) >> 9);
    }

    if literal_start < input_end {
        handle_last_literals(output, &input[literal_start..]);
    }

    Ok(output.pos() - output_start)
}

/// Result of the three-match resolution loop.
enum ResolveAction {
    /// All matches encoded. `cur` and `literal_start` already advanced.
    Done,
    /// match1 was encoded. Restart lazy evaluation with the returned `(match1, match0)`.
    Restart {
        next_match1: Match,
        next_match0: Match,
    },
}

/// Resolve overlapping matches (match1, match2, and potentially match3).
/// Encodes sequences to `output` and advances `cur`/`literal_start`.
fn resolve_overlapping_matches(
    input: &[u8],
    output: &mut impl Sink,
    hash_table: &mut HashTableHCU32,
    ext_dict: &[u8],
    stream_offset: usize,
    end_pos_check: usize,
    match_limit: usize,
    match1: &mut Match,
    match2: &mut Match,
    cur: &mut usize,
    literal_start: &mut usize,
) -> ResolveAction {
    loop {
        // Adjust match2 if it overlaps with match1
        if (match2.start_position - match1.start_position) < OPTIMAL_MATCH_LENGTH as u32 {
            let mut capped_length = (match1.match_length as usize).min(OPTIMAL_MATCH_LENGTH);
            if match1.start_position as usize + capped_length
                > match2.end().saturating_sub(MINMATCH)
            {
                capped_length = (match2.start_position - match1.start_position) as usize
                    + (match2.match_length as usize).saturating_sub(MINMATCH);
            }
            let overlap = capped_length
                .saturating_sub((match2.start_position - match1.start_position) as usize);
            if overlap > 0 {
                match2.trim_front(overlap);
            }
        }

        // Try to find match3 near the end of match2
        let Some(match3) = (match2.end() <= end_pos_check)
            .then(|| {
                find_wider_hash_chain_match(
                    hash_table,
                    input,
                    match2.end() - 3,
                    match2.start_position as usize,
                    match_limit,
                    match2.match_length as usize,
                    ext_dict,
                    stream_offset,
                )
            })
            .flatten()
        else {
            // No match3 — encode match1 + match2
            if (match2.start_position as usize) < match1.end() {
                match1.match_length = match2.start_position - match1.start_position;
            }
            match1.encode_to(input, *literal_start, output);
            *cur = match1.end();
            *literal_start = *cur;
            match2.encode_to(input, *literal_start, output);
            *cur = match2.end();
            *literal_start = *cur;
            return ResolveAction::Done;
        };

        let match3_near_match1_end = (match3.start_position as usize) < match1.end() + 3;

        // match3 starts right after match1 — encode match1, restart lazy with match3
        if match3_near_match1_end && match3.start_position as usize >= match1.end() {
            if (match2.start_position as usize) < match1.end() {
                let overlap = match1.end() - match2.start_position as usize;
                match2.trim_front(overlap);
                if (match2.match_length as usize) < MINMATCH {
                    *match2 = match3;
                }
            }
            match1.encode_to(input, *literal_start, output);
            *cur = match1.end();
            *literal_start = *cur;
            return ResolveAction::Restart {
                next_match1: match3,
                next_match0: *match2,
            };
        }

        // match3 overlaps with match1 — demote to match2 and retry
        if match3_near_match1_end {
            *match2 = match3;
            continue;
        }

        // Resolve overlap between match1 and match2
        if (match2.start_position as usize) < match1.end() {
            if (match2.start_position - match1.start_position) < MATCH_LENGTH_MASK as u32 {
                if match1.match_length as usize > OPTIMAL_MATCH_LENGTH {
                    match1.match_length = OPTIMAL_MATCH_LENGTH as u32;
                }
                if match1.end() > match2.end() - MINMATCH {
                    match1.match_length =
                        (match2.end() - match1.start_position as usize - MINMATCH) as u32;
                }
                let overlap = match1.end() - match2.start_position as usize;
                match2.trim_front(overlap);
            } else {
                match1.match_length = match2.start_position - match1.start_position;
            }
        }

        // Encode match1, shift match2→match1, match3→match2, continue resolving
        match1.encode_to(input, *literal_start, output);
        *cur = match1.end();
        *literal_start = *cur;
        *match1 = *match2;
        *match2 = match3;
    }
}

/// Internal high-compression implementation for the hash-chain strategy.
/// `input_pos` is where the current block starts (positions before it are prefix).
/// `ext_dict` and `stream_offset` support linked block mode.
fn compress_hash_chain_internal(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    hash_table: &mut HashTableHCU32,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    let output_start_pos = output.pos();
    if input.len() - input_pos < MFLIMIT + 1 {
        handle_last_literals(output, &input[input_pos..]);
        return Ok(output.pos() - output_start_pos);
    }

    let input_end = input.len();
    // Inclusive max main-loop cursor: at least `MFLIMIT` bytes from cursor to `input_end`.
    let end_pos_check = input_end - MFLIMIT;
    // Do not extend matches into the last `LAST_LITERALS` bytes (they are literals).
    let match_limit = input_end - LAST_LITERALS;

    let mut cur = input_pos + 1;
    let mut literal_start = input_pos;
    let mut match0;
    let mut match1;
    let mut match2;

    while cur < end_pos_check {
        let Some(found_match) = find_best_hash_chain_match(
            hash_table,
            input,
            cur,
            match_limit,
            ext_dict,
            stream_offset,
        ) else {
            cur += 1;
            continue;
        };

        match1 = found_match;
        match0 = match1;

        // Lazy match evaluation: try to find better matches ahead.
        // Each iteration holds match1 (current best). We search for match2 near
        // match1's end. If match2 is better, we resolve overlaps (possibly finding
        // match3) and either emit sequences or loop back with an updated match1.
        loop {
            debug_assert!(match1.start_position as usize >= literal_start);

            // Try to find a wider match starting near the end of match1
            let next_match = if match1.end() > end_pos_check {
                None
            } else {
                find_wider_hash_chain_match(
                    hash_table,
                    input,
                    match1.end() - 2,
                    match1.start_position as usize,
                    match_limit,
                    match1.match_length as usize,
                    ext_dict,
                    stream_offset,
                )
            };

            let Some(found_match) = next_match else {
                // No better match found — encode match1
                match1.encode_to(input, literal_start, output);
                cur = match1.end();
                literal_start = cur;
                break;
            };

            match2 = found_match;

            // Prefer match0 over match1 if match2 overlaps with match0's range
            if match0.start_position < match1.start_position
                && (match2.start_position as usize)
                    < match1.start_position as usize + match0.match_length as usize
            {
                match1 = match0;
            }
            debug_assert!(match2.start_position >= match1.start_position);

            // If match2 is very close to match1, just use match2 and retry
            if (match2.start_position - match1.start_position) < 3 {
                match1 = match2;
                continue;
            }

            // Resolve overlaps between match1, match2, and potentially match3
            match resolve_overlapping_matches(
                input,
                output,
                hash_table,
                ext_dict,
                stream_offset,
                end_pos_check,
                match_limit,
                &mut match1,
                &mut match2,
                &mut cur,
                &mut literal_start,
            ) {
                ResolveAction::Done => break,
                ResolveAction::Restart {
                    next_match1,
                    next_match0,
                } => {
                    match1 = next_match1;
                    match0 = next_match0;
                    continue;
                }
            }
        }
    }

    handle_last_literals(output, &input[literal_start..input_end]);
    Ok(output.pos() - output_start_pos)
}

/// Reverse the optimal parse path: walk backward from `start`, swapping each state's
/// `(match_len, match_offset)` with the values from the next step forward.
/// After this, `opt[0..last_match_pos)` can be read forward to emit sequences.
#[inline]
fn reverse_optimal_parse_path(
    opt: &mut [OptimalState],
    start: usize,
    mut match_length: i32,
    mut match_offset: i32,
) {
    let mut pos = start;
    loop {
        let next_match_length = opt[pos].match_len;
        let next_match_offset = opt[pos].match_offset;
        opt[pos].match_len = match_length;
        opt[pos].match_offset = match_offset;
        match_length = next_match_length;
        match_offset = next_match_offset;
        if (next_match_length as usize) > pos {
            break;
        }
        pos -= next_match_length as usize;
    }
}

/// Emit LZ4 sequences from optimal states `opt[0..last_match_pos)` (`match_len == 1` is one literal step).
#[inline]
fn encode_optimal_parse_path(
    opt: &[OptimalState],
    last_match_pos: usize,
    input: &[u8],
    literal_start: &mut usize,
    cur: &mut usize,
    output: &mut impl Sink,
) {
    let mut pos: usize = 0;
    while pos < last_match_pos {
        let match_length = opt[pos].match_len as usize;
        let match_offset = opt[pos].match_offset as u16;

        if match_length == 1 {
            *cur += 1;
            pos += 1;
            continue;
        }

        encode_sequence(
            &input[*literal_start..*cur],
            output,
            match_offset,
            match_length - MINMATCH,
        );

        *cur += match_length;
        *literal_start = *cur;
        pos += match_length;
    }
}

/// Internal optimal parsing compression implementation
fn compress_opt_internal(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    level_params: HcLevelParams,
    hash_table: &mut HashTableHCU32,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    let output_start_pos = output.pos();

    if input.len() - input_pos < MFLIMIT + 1 {
        handle_last_literals(output, &input[input_pos..]);
        return Ok(output.pos() - output_start_pos);
    }

    let input_end = input.len();
    // Inclusive max main-loop `cur`: at least `MFLIMIT` bytes remain from `cur` to `input_end`.
    let end_pos_check = input_end - MFLIMIT;
    // Do not extend matches into the last `LAST_LITERALS` bytes (they are literals).
    let match_limit = input_end - LAST_LITERALS;

    debug_assert_eq!(
        level_params.strategy,
        HcCompressionStrategy::Optimal,
        "compress_opt_internal is only for optimal (levels 10–12)"
    );
    let HcLevelParams {
        sufficient_match_len,
        full_optimal_update,
        ..
    } = level_params;

    let mut literal_start = input_pos;
    let mut cur = input_pos;

    let mut opt = vec![OptimalState::SENTINEL; LZ4_OPT_NUM + TRAILING_LITERALS];

    let sufficient_match_len = sufficient_match_len.min(LZ4_OPT_NUM - 1);

    while cur <= end_pos_check {
        let lit_len = (cur - literal_start) as i32;

        let (first_match_length, first_match_offset) = find_longer_hash_chain_match(
            hash_table,
            input,
            cur,
            match_limit,
            MINMATCH - 1,
            ext_dict,
            stream_offset,
        );
        if first_match_length == 0 {
            cur += 1;
            continue;
        }
        let first_match_length = first_match_length as usize;

        // If match is good enough, encode immediately
        if first_match_length >= sufficient_match_len {
            encode_sequence(
                &input[literal_start..cur],
                output,
                first_match_offset,
                first_match_length - MINMATCH,
            );
            cur += first_match_length;
            literal_start = cur;
            continue;
        }

        // Initialize optimal parsing state for literals
        for opt_offset in 0..MINMATCH as i32 {
            let cost = literals_price(lit_len + opt_offset);
            opt[opt_offset as usize].match_len = 1;
            opt[opt_offset as usize].match_offset = 0;
            opt[opt_offset as usize].lit_len = lit_len + opt_offset;
            opt[opt_offset as usize].path_cost = cost;
        }

        // Set prices using initial match
        let first_match_length_capped = first_match_length.min(LZ4_OPT_NUM - 1);
        #[allow(clippy::needless_range_loop)]
        for match_length in MINMATCH..=first_match_length_capped {
            let cost = sequence_price(lit_len, match_length as i32);
            opt[match_length].match_len = match_length as i32;
            opt[match_length].match_offset = first_match_offset as i32;
            opt[match_length].lit_len = lit_len;
            opt[match_length].path_cost = cost;
        }

        let mut last_match_pos = first_match_length;

        // Add trailing literals after the match
        for trail in 1..=TRAILING_LITERALS {
            let opt_index = last_match_pos + trail;
            if opt_index < opt.len() {
                opt[opt_index].match_len = 1; // literal
                opt[opt_index].match_offset = 0;
                opt[opt_index].lit_len = trail as i32;
                opt[opt_index].path_cost =
                    opt[last_match_pos].path_cost + literals_price(trail as i32);
            }
        }

        // Refine costs along the optimal window; may encode a prefix and restart the main step.
        let mut early_encode = false;
        let mut scan_offset: usize = 1;
        while scan_offset < last_match_pos {
            let scan_pos = cur + scan_offset;

            if scan_pos > end_pos_check {
                break;
            }

            if full_optimal_update {
                // Not useful to search here if next position has same (or lower) cost
                if opt[scan_offset + 1].path_cost <= opt[scan_offset].path_cost
                    && opt[scan_offset + MINMATCH].path_cost < opt[scan_offset].path_cost + 3
                {
                    scan_offset += 1;
                    continue;
                }
            } else {
                // Not useful to search here if next position has same (or lower) cost
                if opt[scan_offset + 1].path_cost <= opt[scan_offset].path_cost {
                    scan_offset += 1;
                    continue;
                }
            }

            // Find longer match at current position
            let min_match_length = if full_optimal_update {
                MINMATCH - 1
            } else {
                last_match_pos - scan_offset
            };

            let (new_match_length, new_match_offset) = find_longer_hash_chain_match(
                hash_table,
                input,
                scan_pos,
                match_limit,
                min_match_length,
                ext_dict,
                stream_offset,
            );
            if new_match_length == 0 {
                scan_offset += 1;
                continue;
            }
            let new_match_length = new_match_length as usize;

            // If match is good enough or extends beyond buffer, encode immediately
            if new_match_length >= sufficient_match_len
                || new_match_length + scan_offset >= LZ4_OPT_NUM
            {
                // Set last_match_pos = scan_offset + 1 as in C code
                last_match_pos = scan_offset + 1;

                reverse_optimal_parse_path(
                    &mut opt,
                    scan_offset,
                    new_match_length as i32,
                    new_match_offset as i32,
                );

                encode_optimal_parse_path(
                    &opt,
                    last_match_pos,
                    input,
                    &mut literal_start,
                    &mut cur,
                    output,
                );

                early_encode = true;
                break;
            }

            // Update prices for literals before the match
            {
                let base_lit_len = opt[scan_offset].lit_len;
                for lit_step in 1..MINMATCH as i32 {
                    let opt_index = scan_offset + lit_step as usize;
                    let price = opt[scan_offset].path_cost - literals_price(base_lit_len)
                        + literals_price(base_lit_len + lit_step);
                    if price < opt[opt_index].path_cost {
                        opt[opt_index].match_len = 1; // literal
                        opt[opt_index].match_offset = 0;
                        opt[opt_index].lit_len = base_lit_len + lit_step;
                        opt[opt_index].path_cost = price;
                    }
                }
            }

            // Set prices using match at current position
            {
                let new_match_length_capped = new_match_length.min(LZ4_OPT_NUM - scan_offset - 1);
                for match_length in MINMATCH..=new_match_length_capped {
                    let opt_index = scan_offset + match_length;
                    let (literal_prefix, price) = if opt[scan_offset].match_len == 1 {
                        let lit_len = opt[scan_offset].lit_len;
                        let base_price = if scan_offset as i32 > lit_len {
                            opt[scan_offset - lit_len as usize].path_cost
                        } else {
                            0
                        };
                        (
                            lit_len,
                            base_price + sequence_price(lit_len, match_length as i32),
                        )
                    } else {
                        (
                            0,
                            opt[scan_offset].path_cost + sequence_price(0, match_length as i32),
                        )
                    };

                    if opt_index > last_match_pos + TRAILING_LITERALS
                        || price <= opt[opt_index].path_cost
                    {
                        if match_length == new_match_length_capped && last_match_pos < opt_index {
                            last_match_pos = opt_index;
                        }
                        opt[opt_index].match_len = match_length as i32;
                        opt[opt_index].match_offset = new_match_offset as i32;
                        opt[opt_index].lit_len = literal_prefix;
                        opt[opt_index].path_cost = price;
                    }
                }
            }

            // Complete following positions with literals
            for trail in 1..=TRAILING_LITERALS as i32 {
                let opt_index = last_match_pos + trail as usize;
                opt[opt_index].match_len = 1; // literal
                opt[opt_index].match_offset = 0;
                opt[opt_index].lit_len = trail;
                opt[opt_index].path_cost = opt[last_match_pos].path_cost + literals_price(trail);
            }

            scan_offset += 1;
        }

        if early_encode {
            continue;
        }

        // Reverse traversal to find the optimal path
        {
            let match_length = opt[last_match_pos].match_len;
            let match_offset = opt[last_match_pos].match_offset;
            reverse_optimal_parse_path(
                &mut opt,
                last_match_pos - match_length as usize,
                match_length,
                match_offset,
            );
        }

        encode_optimal_parse_path(
            &opt,
            last_match_pos,
            input,
            &mut literal_start,
            &mut cur,
            output,
        );

        // No opt buffer reset needed (matches C behavior)
    }

    // Handle remaining literals
    handle_last_literals(output, &input[literal_start..input_end]);
    Ok(output.pos() - output_start_pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::SliceSink;

    #[test]
    fn test_compress_hc_basic() {
        let input = b"Hello, this is a test string that should be compressed!";
        let mut output = vec![0u8; input.len() * 2]; // Ensure enough space
        let mut sink = SliceSink::new(&mut output, 0);

        let result = compress_hc(input, &mut sink, 17);
        assert!(result.is_ok());

        let compressed_size = result.unwrap();
        assert!(compressed_size > 0);
        let result = decompress(&output[..compressed_size], input.len());
        assert!(result.is_ok());
        assert_eq!(&input[..], &result.unwrap()[..])
    }

    #[test]
    fn test_compress_hc_small_input() {
        let input = b"Hi"; // Too small to compress
        let mut output = vec![0u8; 100];
        let mut sink = SliceSink::new(&mut output, 0);

        let result = compress_hc(input, &mut sink, 17);
        assert!(result.is_ok());

        let compressed_size = result.unwrap();
        assert!(compressed_size > 0);
        let result = decompress(&output[..compressed_size], input.len());
        assert!(result.is_ok());
        assert_eq!(&input[..], &result.unwrap()[..])
    }

    #[test]
    fn test_compress_hc_repeated_pattern() {
        let input = b"AAAAAAAAAAABBBBBAAABBBBBBBAAAAAAA"; // Highly compressible
        let mut output = vec![0u8; input.len() * 2];
        let mut sink = SliceSink::new(&mut output, 0);

        let result = compress_hc(input, &mut sink, 17);
        assert!(result.is_ok());

        let compressed_size = result.unwrap();
        assert!(compressed_size > 0);
        let result = decompress(&output[..compressed_size], input.len() * 8);
        assert!(result.is_ok());
        assert_eq!(&input[..], &result.unwrap()[..])
    }

    #[test]
    fn test_compress_hc_level_10() {
        // Level 10 uses optimal parsing
        let input = b"Hello, this is a test string that should be compressed!";
        let mut output = vec![0u8; input.len() * 2];
        let mut sink = SliceSink::new(&mut output, 0);

        let result = compress_hc(input, &mut sink, 10);
        assert!(result.is_ok());

        let compressed_size = result.unwrap();
        assert!(compressed_size > 0);
        let result = decompress(&output[..compressed_size], input.len());
        assert!(result.is_ok());
        assert_eq!(&input[..], &result.unwrap()[..])
    }

    #[test]
    fn test_compress_hc_level_10_small_input() {
        let input = b"Hi"; // Too small to compress
        let mut output = vec![0u8; 100];
        let mut sink = SliceSink::new(&mut output, 0);

        let result = compress_hc(input, &mut sink, 10);
        assert!(result.is_ok());

        let compressed_size = result.unwrap();
        assert!(compressed_size > 0);
        let result = decompress(&output[..compressed_size], input.len());
        assert!(result.is_ok());
        assert_eq!(&input[..], &result.unwrap()[..])
    }

    #[test]
    fn test_compress_hc_level_10_repeated_pattern() {
        let input = b"AAAAAAAAAAABBBBBAAABBBBBBBAAAAAAA"; // Highly compressible
        let mut output = vec![0u8; input.len() * 2];
        let mut sink = SliceSink::new(&mut output, 0);

        let result = compress_hc(input, &mut sink, 10);
        assert!(result.is_ok());

        let compressed_size = result.unwrap();
        assert!(compressed_size > 0);
        let result = decompress(&output[..compressed_size], input.len() * 8);
        assert!(result.is_ok());
        assert_eq!(&input[..], &result.unwrap()[..])
    }

    #[test]
    fn test_compress_hc_level_11() {
        let input = b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.";
        let mut output = vec![0u8; input.len() * 2];
        let mut sink = SliceSink::new(&mut output, 0);

        let result = compress_hc(input, &mut sink, 11);
        assert!(result.is_ok());

        let compressed_size = result.unwrap();
        assert!(compressed_size > 0);
        let result = decompress(&output[..compressed_size], input.len());
        assert!(result.is_ok());
        assert_eq!(&input[..], &result.unwrap()[..])
    }

    #[test]
    fn test_compress_hc_level_12() {
        let input = b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.";
        let mut output = vec![0u8; input.len() * 2];
        let mut sink = SliceSink::new(&mut output, 0);

        let result = compress_hc(input, &mut sink, 12);
        assert!(result.is_ok());

        let compressed_size = result.unwrap();
        assert!(compressed_size > 0);
        let result = decompress(&output[..compressed_size], input.len());
        assert!(result.is_ok());
        assert_eq!(&input[..], &result.unwrap()[..])
    }

    #[test]
    fn test_compress_hc_level_12_better_than_level_9() {
        // Level 12 (optimal) should produce same or smaller output than level 9 (HC)
        let input: Vec<u8> = (0..1000)
            .map(|i| {
                let patterns = [b"ABCD", b"EFGH", b"IJKL", b"MNOP"];
                patterns[(i / 50) % 4][i % 4]
            })
            .collect();

        let mut output_hc = vec![0u8; input.len() * 2];
        let mut sink_hc = SliceSink::new(&mut output_hc, 0);
        let hc_size = compress_hc(&input, &mut sink_hc, 9).unwrap();

        let mut output_opt = vec![0u8; input.len() * 2];
        let mut sink_opt = SliceSink::new(&mut output_opt, 0);
        let opt_size = compress_hc(&input, &mut sink_opt, 12).unwrap();

        // Optimal should produce same or smaller output
        assert!(
            opt_size <= hc_size,
            "Level 12 ({}) should be <= Level 9 ({})",
            opt_size,
            hc_size
        );

        // Both should decompress correctly
        let result_hc = decompress(&output_hc[..hc_size], input.len());
        assert!(result_hc.is_ok());
        assert_eq!(&input[..], &result_hc.unwrap()[..]);

        let result_opt = decompress(&output_opt[..opt_size], input.len());
        assert!(result_opt.is_ok());
        assert_eq!(&input[..], &result_opt.unwrap()[..]);
    }

    #[test]
    fn test_compress_hc_level_10_large_input() {
        // Test with a larger input to exercise the optimal algorithm
        let input: Vec<u8> = (0..10000).map(|i| ((i * 7 + 13) % 256) as u8).collect();

        let mut output = vec![0u8; input.len() * 2];
        let mut sink = SliceSink::new(&mut output, 0);

        let result = compress_hc(&input, &mut sink, 10);
        assert!(result.is_ok());

        let compressed_size = result.unwrap();
        assert!(compressed_size > 0);
        let result = decompress(&output[..compressed_size], input.len());
        assert!(result.is_ok());
        assert_eq!(&input[..], &result.unwrap()[..])
    }

    #[test]
    fn test_compress_hc_level_10_all_same() {
        // Test with all same bytes - highly compressible
        let input = vec![0x42u8; 5000];
        let mut output = vec![0u8; input.len() * 2];
        let mut sink = SliceSink::new(&mut output, 0);

        let result = compress_hc(&input, &mut sink, 10);
        assert!(result.is_ok());

        let compressed_size = result.unwrap();
        assert!(compressed_size > 0);
        // Should compress very well
        assert!(
            compressed_size < input.len() / 10,
            "Should compress very well"
        );

        let result = decompress(&output[..compressed_size], input.len());
        assert!(result.is_ok());
        assert_eq!(&input[..], &result.unwrap()[..])
    }

    /// Exact compressed sizes for larger structured inputs. These inputs separate
    /// the different HC strategies much better than tiny toy strings, so they are
    /// useful as a regression net for compression ratio changes.
    #[test]
    fn test_compressed_sizes_exact() {
        fn get_size(input: &[u8], level: u8) -> usize {
            let mut output = vec![0u8; input.len() * 2 + 100];
            let mut sink = SliceSink::new(&mut output, 0);
            compress_hc(input, &mut sink, level).unwrap()
        }

        let html_like: Vec<u8> = (0..10_000)
            .flat_map(|i| match i % 7 {
                0 => b"<div class=\"item\">".to_vec(),
                1 => format!("content {i} here ").into_bytes(),
                2 => b"</div>\n".to_vec(),
                3 => b"<span style=\"color:red\">".to_vec(),
                4 => format!("value={i} ").into_bytes(),
                5 => b"</span>".to_vec(),
                _ => b"<br/>\n".to_vec(),
            })
            .collect();

        let json_like: Vec<u8> = (0..2_000)
            .flat_map(|i| {
                format!(
                    "{{\"id\":{i},\"name\":\"user_{}\",\"score\":{},\"active\":true}},\n",
                    i % 100,
                    i * 7 % 1000,
                )
                .into_bytes()
            })
            .collect();

        let code_like: Vec<u8> = (0..3_000)
            .flat_map(|i| match i % 5 {
                0 => format!("    let value_{} = compute(input[{}]);\n", i % 50, i).into_bytes(),
                1 => b"    if value > threshold {\n".to_vec(),
                2 => format!("        result += value_{} * weight;\n", i % 50).into_bytes(),
                3 => b"    }\n".to_vec(),
                _ => format!("    // step {i}\n").into_bytes(),
            })
            .collect();

        // (level, html_like, json_like, code_like)
        let expected: &[(u8, usize, usize, usize)] = &[
            (1, 16_350, 16_183, 6_246),
            (4, 15_620, 16_198, 6_071),
            (9, 15_509, 15_698, 5_985),
            (10, 15_153, 15_102, 5_990),
            (12, 15_100, 15_083, 5_979),
        ];

        for &(level, expected_html_like, expected_json_like, expected_code_like) in expected {
            assert_eq!(
                get_size(&html_like, level),
                expected_html_like,
                "html_like @ level {level}"
            );
            assert_eq!(
                get_size(&json_like, level),
                expected_json_like,
                "json_like @ level {level}"
            );
            assert_eq!(
                get_size(&code_like, level),
                expected_code_like,
                "code_like @ level {level}"
            );
        }
    }

    #[test]
    fn test_compress_hc_level_clamping() {
        // Test that levels are clamped correctly
        let input = b"The quick brown fox jumps over the lazy dog. The quick brown fox.";

        // Level 0 should be clamped to 1
        let mut output = vec![0u8; input.len() * 2];
        let mut sink = SliceSink::new(&mut output, 0);
        let result = compress_hc(input, &mut sink, 0);
        assert!(result.is_ok());
        let size_level_0 = result.unwrap();
        let decompressed = decompress(&output[..size_level_0], input.len()).unwrap();
        assert_eq!(&input[..], &decompressed[..]);

        // Level 20 should be clamped to 12
        let mut output = vec![0u8; input.len() * 2];
        let mut sink = SliceSink::new(&mut output, 0);
        let result = compress_hc(input, &mut sink, 20);
        assert!(result.is_ok());
        let size_level_20 = result.unwrap();
        let decompressed = decompress(&output[..size_level_20], input.len()).unwrap();
        assert_eq!(&input[..], &decompressed[..]);
    }
}
