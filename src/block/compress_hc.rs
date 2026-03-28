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

/// Minimum bytes that must remain from the main scan cursor to the block end (LZ4 block rule; same as [`MFLIMIT`]).
const MIN_BYTES_FROM_CURSOR_TO_BLOCK_END: usize = MFLIMIT;
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

const MIN_MATCH: usize = 4;
const OPTIMAL_ML: usize = 32;
const ML_MASK: usize = 31;

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
    pub strategy: HcCompressionStrategy,
    /// Hash-chain search budget for [`HcCompressionStrategy::HashChain`] and [`HcCompressionStrategy::Optimal`]
    /// (same meaning as [`HashTableHCU32::max_attempts`]). Zero in [`HcCompressionStrategy::Mid`].
    pub max_attempts: usize,
    /// Only for [`HcCompressionStrategy::Optimal`]: encode immediately when the first match is at least this long.
    pub sufficient_match_len: usize,
    /// Only for [`HcCompressionStrategy::Optimal`]: exhaustive refinement (level 12).
    pub full_optimal_update: bool,
}

/// Map a compression level to strategy and parameters. `level` should be clamped with `min(12)` first.
#[inline]
pub(crate) const fn hc_level_params(level: u8) -> HcLevelParams {
    match level {
        0 | 1 | 2 => HcLevelParams {
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
pub struct HashTableHCU32 {
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
pub struct Match {
    /// Byte position in the input where this match starts (the "current" cursor).
    pub start_position: u32,
    /// Length of the match in bytes (including the mandatory 4-byte minimum).
    pub match_length: u32,
    /// Byte position of the earlier occurrence being referenced.
    /// The encoded offset/distance is `start_position - reference_position`.
    pub reference_position: u32,
}

impl Match {
    pub fn new() -> Self {
        Self {
            start_position: 0,
            match_length: 0,
            reference_position: 0,
        }
    }

    #[inline]
    pub fn end(&self) -> usize {
        self.start_position as usize + self.match_length as usize
    }

    pub fn fix(&mut self, correction: usize) {
        self.start_position += correction as u32;
        self.reference_position += correction as u32;
        self.match_length = self.match_length.saturating_sub(correction as u32);
    }

    #[inline]
    pub fn offset(&self) -> u16 {
        self.start_position.wrapping_sub(self.reference_position) as u16
    }

    pub fn encode_to<S: Sink>(&self, input: &[u8], literal_anchor_pos: usize, output: &mut S) {
        encode_sequence(
            &input[literal_anchor_pos..self.start_position as usize],
            output,
            self.offset(),
            self.match_length as usize - MIN_MATCH,
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
    let mut p = pos;

    // Extend 32-bit pattern to usize for batch comparison
    let pattern: usize = if core::mem::size_of::<usize>() == 8 {
        (pattern as usize) | ((pattern as usize) << 32)
    } else {
        pattern as usize
    };

    const STEP: usize = core::mem::size_of::<usize>();
    while p + STEP <= limit {
        let v = super::compress::get_batch_arch(input, p);
        let diff = v ^ pattern;
        if diff != 0 {
            p += (diff.trailing_zeros() / 8) as usize;
            return p - pos;
        }
        p += STEP;
    }

    // Byte-by-byte tail
    let byte_val = (pattern & 0xFF) as u8; // single repeated byte
    while p < limit && input[p] == byte_val {
        p += 1;
    }

    p - pos
}

/// Like [`count_pattern`], but walks backward. `pattern` is one byte repeated
/// four times in a `u32` (same encoding as [`count_pattern`]).
/// Equivalent to C's LZ4HC_reverseCountPattern.
#[inline]
fn reverse_count_pattern(input: &[u8], pos: usize, low_limit: usize, pattern: u32) -> usize {
    let mut p = pos;

    while p >= low_limit + 4 {
        if super::compress::get_batch(input, p - 4) != pattern {
            break;
        }
        p -= 4;
    }

    // Byte-by-byte tail using native endian byte order (matches get_batch)
    let pattern_bytes = pattern.to_ne_bytes();
    let mut byte_idx: usize = 3;
    while p > low_limit {
        if input[p - 1] != pattern_bytes[byte_idx] {
            break;
        }
        p -= 1;
        byte_idx = if byte_idx == 0 { 3 } else { byte_idx - 1 };
    }

    pos - p
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

/// Count matching bytes forward with the reference starting in `external_dictionary` and
/// potentially continuing into `input[0..]` (the prefix) when external_dictionary is exhausted.
/// `reference_position` may already be past `external_dictionary` (when the min-match check crossed the boundary).
#[inline]
fn count_forward_external_dictionary(
    input: &[u8],
    cur: usize,
    external_dictionary: &[u8],
    reference_position: usize,
    match_limit: usize,
) -> usize {
    let mut cur = cur;

    if reference_position >= external_dictionary.len() {
        let prefix_pos = reference_position - external_dictionary.len();
        return count_same_bytes(input, &mut cur, input, prefix_pos, match_limit);
    }

    let matched1 = count_same_bytes(
        input,
        &mut cur,
        external_dictionary,
        reference_position,
        match_limit,
    );

    if reference_position + matched1 >= external_dictionary.len() && cur < match_limit {
        matched1 + count_same_bytes(input, &mut cur, input, 0, match_limit)
    } else {
        matched1
    }
}

/// Result of pattern/repeat chain optimization inside [`HashTableHCU32::find_longer_match`]
/// (mirrors LZ4HC repeat detection).
enum PatternChainAction {
    /// Continue with normal chain-step logic at the end of the loop body.
    Noop,
    /// Set `candidate_absolute_position` and restart the search loop iteration (`continue`).
    RetryCandidate(usize),
    /// Exit the search loop (`break`).
    StopSearch,
}

impl HashTableHCU32 {
    #[inline]
    pub fn new(max_attempts: usize, input_len: usize) -> Self {
        // Dict table: fixed size, hash function already bounds to this range
        let dictionary = vec![0u32; HASHTABLE_SIZE_HC]
            .into_boxed_slice()
            .try_into()
            .unwrap();

        // Chain table: dynamically sized based on input length
        // min(input_len, MAX_DISTANCE_HC), at least 256, must be power of 2
        let chain_size = input_len.min(MAX_DISTANCE_HC).max(256).next_power_of_two();

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
        let needed_chain_size = input_len.min(MAX_DISTANCE_HC).max(256).next_power_of_two();

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
    /// and advances `next_to_update` past the positions now in `external_dictionary`.
    #[cfg(feature = "frame")]
    fn prepare_linked_block(&mut self, max_attempts: usize, absolute_block_start_position: usize) {
        if self.chain_table.len() < MAX_DISTANCE_HC {
            let mut new_chain = vec![0u16; MAX_DISTANCE_HC].into_boxed_slice();
            let old_len = self.chain_table.len();
            for i in 0..old_len {
                new_chain[i] = self.chain_table[i];
            }
            self.chain_table = new_chain;
        }
        self.next_to_update = absolute_block_start_position;
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

    /// Hash function for high compression
    #[inline]
    fn hash_hc(v: u32) -> u32 {
        v.wrapping_mul(2654435761u32) >> 17
    }

    #[inline]
    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        Self::hash_hc(super::compress::get_batch(input, pos)) as usize
    }

    /// Insert hashes for all positions up to the given local offset.
    /// Positions stored in the hash table are absolute (`local_pos + stream_offset`).
    #[inline]
    pub fn insert(&mut self, off: u32, input: &[u8], stream_offset: usize) {
        let absolute_byte_offset = off as usize + stream_offset;
        for absolute_position in self.next_to_update..absolute_byte_offset {
            let local_pos = absolute_position - stream_offset;
            self.add_hash(Self::get_hash_at(input, local_pos), absolute_position);
        }
        self.next_to_update = absolute_byte_offset;
    }

    fn insert_and_find_best_match(
        &mut self,
        input: &[u8],
        off: u32,
        match_limit: u32,
        match_info: &mut Match,
        external_dictionary: &[u8],
        stream_offset: usize,
    ) -> bool {
        match_info.start_position = off;
        match_info.match_length = 0;
        let mut delta: usize = 0;
        let mut replacement_length: usize = 0;

        let off = off as usize;
        let match_limit = match_limit as usize;
        let absolute_byte_offset = off + stream_offset;
        let external_dictionary_stream_offset = stream_offset - external_dictionary.len();

        self.insert(off as u32, input, stream_offset);

        let mut candidate_absolute_position = self.get_dictionary_at(Self::get_hash_at(input, off));

        for i in 0..self.max_attempts {
            if candidate_absolute_position >= absolute_byte_offset
                || absolute_byte_offset - candidate_absolute_position > self.chain_mask()
            {
                break;
            }

            if candidate_absolute_position >= stream_offset {
                let reference_local_position = candidate_absolute_position - stream_offset;

                if match_info.match_length >= MIN_MATCH as u32 {
                    let check_pos = match_info.match_length as usize - 1;
                    if input[reference_local_position + check_pos] != input[off + check_pos]
                        || input[reference_local_position + check_pos + 1]
                            != input[off + check_pos + 1]
                    {
                        let next = self.next(candidate_absolute_position);
                        if next >= absolute_byte_offset
                            || absolute_byte_offset - next > self.chain_mask()
                            || next == candidate_absolute_position
                        {
                            break;
                        }
                        candidate_absolute_position = next;
                        continue;
                    }
                }

                if self.read_min_match_equals(input, reference_local_position, off) {
                    let match_len = MIN_MATCH
                        + self.common_bytes(
                            input,
                            reference_local_position + MIN_MATCH,
                            off + MIN_MATCH,
                            match_limit,
                        );
                    if match_len as u32 > match_info.match_length {
                        let distance = absolute_byte_offset - candidate_absolute_position;
                        match_info.reference_position = (off as u32).wrapping_sub(distance as u32);
                        match_info.match_length = match_len as u32;
                    }
                    if i == 0 {
                        replacement_length = match_len;
                        delta = absolute_byte_offset - candidate_absolute_position;
                    }
                }
            } else if !external_dictionary.is_empty()
                && candidate_absolute_position >= external_dictionary_stream_offset
            {
                let reference_local_position =
                    candidate_absolute_position - external_dictionary_stream_offset;

                if reference_local_position + 4 <= external_dictionary.len() {
                    if super::compress::get_batch(external_dictionary, reference_local_position)
                        == super::compress::get_batch(input, off)
                    {
                        let match_len = MIN_MATCH
                            + count_forward_external_dictionary(
                                input,
                                off + MIN_MATCH,
                                external_dictionary,
                                reference_local_position + MIN_MATCH,
                                match_limit,
                            );
                        if match_len as u32 > match_info.match_length {
                            let distance = absolute_byte_offset - candidate_absolute_position;
                            match_info.reference_position =
                                (off as u32).wrapping_sub(distance as u32);
                            match_info.match_length = match_len as u32;
                        }
                        if i == 0 {
                            replacement_length = match_len;
                            delta = absolute_byte_offset - candidate_absolute_position;
                        }
                    }
                } else if reference_local_position < external_dictionary.len() {
                    if read_u32_from_two_slices(
                        external_dictionary,
                        reference_local_position,
                        input,
                    ) == super::compress::get_batch(input, off)
                    {
                        let match_len = MIN_MATCH
                            + count_forward_external_dictionary(
                                input,
                                off + MIN_MATCH,
                                external_dictionary,
                                reference_local_position + MIN_MATCH,
                                match_limit,
                            );
                        if match_len as u32 > match_info.match_length {
                            let distance = absolute_byte_offset - candidate_absolute_position;
                            match_info.reference_position =
                                (off as u32).wrapping_sub(distance as u32);
                            match_info.match_length = match_len as u32;
                        }
                        if i == 0 {
                            replacement_length = match_len;
                            delta = absolute_byte_offset - candidate_absolute_position;
                        }
                    }
                }
            }

            let next = self.next(candidate_absolute_position);
            if next >= absolute_byte_offset
                || absolute_byte_offset - next > self.chain_mask()
                || next == candidate_absolute_position
            {
                break;
            }
            candidate_absolute_position = next;
        }

        // Handle pre hash (positions are absolute for hash table, local for input reads)
        if replacement_length != 0 {
            let mut absolute_pointer_position = absolute_byte_offset;
            let absolute_end_position = absolute_byte_offset + replacement_length - 3;
            while absolute_pointer_position < absolute_end_position - delta {
                self.set_chain(absolute_pointer_position, delta as u16);
                absolute_pointer_position += 1;
            }
            loop {
                self.set_chain(absolute_pointer_position, delta as u16);
                let local_ptr = absolute_pointer_position - stream_offset;
                self.set_dictionary_at(
                    Self::get_hash_at(input, local_ptr),
                    absolute_pointer_position,
                );
                absolute_pointer_position += 1;
                if absolute_pointer_position >= absolute_end_position {
                    break;
                }
            }
            self.next_to_update = absolute_end_position;
        }

        match_info.match_length != 0
    }

    /// Insert hashes and find a wider match, similar to Java insertAndFindWiderMatch
    pub fn insert_and_find_wider_match(
        &mut self,
        input: &[u8],
        off: u32,
        start_limit: u32,
        match_limit: u32,
        min_len: u32,
        match_info: &mut Match,
        external_dictionary: &[u8],
        stream_offset: usize,
    ) -> bool {
        match_info.match_length = min_len;

        let off = off as usize;
        let start_limit = start_limit as usize;
        let match_limit = match_limit as usize;
        let absolute_byte_offset = off + stream_offset;
        let external_dictionary_stream_offset = stream_offset - external_dictionary.len();

        let look_back_length = off - start_limit;

        self.insert(off as u32, input, stream_offset);

        let mut candidate_absolute_position = self.get_dictionary_at(Self::get_hash_at(input, off));

        for _ in 0..self.max_attempts {
            if candidate_absolute_position >= absolute_byte_offset
                || absolute_byte_offset - candidate_absolute_position > self.chain_mask()
            {
                break;
            }

            if candidate_absolute_position >= stream_offset {
                let reference_local_position = candidate_absolute_position - stream_offset;

                if match_info.match_length >= MIN_MATCH as u32
                    && reference_local_position >= look_back_length
                {
                    let source_check_position = start_limit + match_info.match_length as usize - 1;
                    let match_check_position = reference_local_position - look_back_length
                        + match_info.match_length as usize
                        - 1;
                    if input[source_check_position] != input[match_check_position]
                        || input[source_check_position + 1] != input[match_check_position + 1]
                    {
                        let next = self.next(candidate_absolute_position);
                        if next >= absolute_byte_offset
                            || absolute_byte_offset - next > self.chain_mask()
                            || next == candidate_absolute_position
                        {
                            break;
                        }
                        candidate_absolute_position = next;
                        continue;
                    }
                }

                if self.read_min_match_equals(input, reference_local_position, off) {
                    let match_len_forward = MIN_MATCH
                        + self.common_bytes(
                            input,
                            reference_local_position + MIN_MATCH,
                            off + MIN_MATCH,
                            match_limit,
                        );
                    let match_len_backward = Self::common_bytes_backward(
                        input,
                        reference_local_position,
                        off,
                        0,
                        start_limit,
                    );
                    let match_len = (match_len_backward + match_len_forward) as u32;

                    if match_len > match_info.match_length {
                        match_info.match_length = match_len;
                        let distance = absolute_byte_offset - candidate_absolute_position;
                        match_info.reference_position =
                            ((off - match_len_backward) as u32).wrapping_sub(distance as u32);
                        match_info.start_position = (off - match_len_backward) as u32;
                    }
                }
            } else if !external_dictionary.is_empty()
                && candidate_absolute_position >= external_dictionary_stream_offset
            {
                let reference_local_position =
                    candidate_absolute_position - external_dictionary_stream_offset;

                let min_match_ok = if reference_local_position + 4 <= external_dictionary.len() {
                    super::compress::get_batch(external_dictionary, reference_local_position)
                        == super::compress::get_batch(input, off)
                } else if reference_local_position < external_dictionary.len() {
                    read_u32_from_two_slices(external_dictionary, reference_local_position, input)
                        == super::compress::get_batch(input, off)
                } else {
                    false
                };

                if min_match_ok {
                    let match_len_forward = MIN_MATCH
                        + count_forward_external_dictionary(
                            input,
                            off + MIN_MATCH,
                            external_dictionary,
                            reference_local_position + MIN_MATCH,
                            match_limit,
                        );
                    // No backward extension for external_dictionary matches
                    let match_len = match_len_forward as u32;

                    if match_len > match_info.match_length {
                        match_info.match_length = match_len;
                        let distance = absolute_byte_offset - candidate_absolute_position;
                        match_info.reference_position = (off as u32).wrapping_sub(distance as u32);
                        match_info.start_position = off as u32;
                    }
                }
            }

            let next = self.next(candidate_absolute_position);
            if next >= absolute_byte_offset
                || absolute_byte_offset - next > self.chain_mask()
                || next == candidate_absolute_position
            {
                break;
            }
            candidate_absolute_position = next;
        }

        match_info.match_length > min_len
    }

    /// Check if two 4-byte sequences starting at the given positions are equal
    #[inline]
    fn read_min_match_equals(&self, input: &[u8], pos1: usize, pos2: usize) -> bool {
        // Fast u32 comparison instead of slice comparison
        super::compress::get_batch(input, pos1) == super::compress::get_batch(input, pos2)
    }

    /// Find the number of common bytes between two positions (optimized version)
    /// Count matching bytes forward. Delegates to the shared `count_same_bytes`
    /// with `input` as both slices (HC always matches within the same buffer).
    #[inline]
    fn common_bytes(&self, input: &[u8], pos1: usize, pos2: usize, limit: usize) -> usize {
        let mut cur = pos2;
        count_same_bytes(input, &mut cur, input, pos1, input.len().min(limit))
    }

    /// Find the number of common bytes backward from two positions (optimized)
    #[inline]
    fn common_bytes_backward(
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

    /// Pattern / repeat chain optimization when `chain_delta(candidate_absolute_position) == 1` and
    /// `match_chain_pos == 0`. Returns an action for the outer search loop.
    fn pattern_chain_action(
        &self,
        input: &[u8],
        off: usize,
        match_limit: usize,
        absolute_byte_offset: usize,
        stream_offset: usize,
        candidate_absolute_position: usize,
        match_chain_pos: usize,
        repeat: &mut u8,
        src_pattern_length: &mut usize,
        best_len: &mut usize,
        best_offset: &mut u16,
    ) -> PatternChainAction {
        if self.chain_delta(candidate_absolute_position) != 1 || match_chain_pos != 0 {
            return PatternChainAction::Noop;
        }

        let match_candidate = candidate_absolute_position.wrapping_sub(1);

        if *repeat == 0 {
            let pattern = super::compress::get_batch(input, off);
            if (pattern & 0xFFFF) == (pattern >> 16) && (pattern & 0xFF) == (pattern >> 24) {
                *repeat = 1;
                *src_pattern_length = count_pattern(input, off + 4, match_limit, pattern) + 4;
            } else {
                *repeat = 2;
            }
        }

        if *repeat != 1 {
            return PatternChainAction::Noop;
        }

        if match_candidate >= absolute_byte_offset
            || absolute_byte_offset - match_candidate > self.chain_mask()
            || match_candidate < stream_offset
        {
            return PatternChainAction::Noop;
        }

        let mc_local = match_candidate - stream_offset;
        let pattern = super::compress::get_batch(input, off);
        if mc_local + 4 > input.len() || super::compress::get_batch(input, mc_local) != pattern {
            return PatternChainAction::Noop;
        }

        let forward_pattern_len = count_pattern(input, mc_local + 4, match_limit, pattern) + 4;
        let back_length = reverse_count_pattern(input, mc_local, 0, pattern);
        let current_segment_len = back_length + forward_pattern_len;

        if current_segment_len >= *src_pattern_length && forward_pattern_len <= *src_pattern_length
        {
            let new_reference_local_position = mc_local + forward_pattern_len - *src_pattern_length;
            let new_ref_abs = new_reference_local_position + stream_offset;
            if absolute_byte_offset > new_ref_abs
                && absolute_byte_offset - new_ref_abs <= self.chain_mask()
            {
                return PatternChainAction::RetryCandidate(new_ref_abs);
            }
        } else {
            let new_reference_local_position = mc_local - back_length;
            let new_ref_abs = new_reference_local_position + stream_offset;
            if absolute_byte_offset > new_ref_abs
                && absolute_byte_offset - new_ref_abs <= self.chain_mask()
            {
                let max_ml = current_segment_len.min(*src_pattern_length);
                if max_ml > *best_len {
                    *best_len = max_ml;
                    *best_offset = (absolute_byte_offset - new_ref_abs) as u16;
                }
                let dist = self.chain_delta(new_ref_abs) as usize;
                if dist == 0 || dist > new_ref_abs {
                    return PatternChainAction::StopSearch;
                }
                return PatternChainAction::RetryCandidate(new_ref_abs - dist);
            }
        }

        PatternChainAction::Noop
    }

    /// Find the longest match at `off`, returning `(match_len_u32, offset_u16)`.
    /// Uses u32 params to reduce call overhead (LZ4 block max is ~2GB).
    /// Offset is u16 since LZ4 format limits distance to 16 bits.
    #[inline]
    pub fn find_longer_match(
        &mut self,
        input: &[u8],
        off: u32,
        match_limit: u32,
        min_len: u32,
        external_dictionary: &[u8],
        stream_offset: usize,
    ) -> (u32, u16) {
        self.insert(off, input, stream_offset);

        let off = off as usize;
        let match_limit = match_limit as usize;
        let absolute_byte_offset = off + stream_offset;
        let external_dictionary_stream_offset = stream_offset - external_dictionary.len();

        let mut best_len: usize = min_len as usize;
        let mut best_offset: u16 = 0;
        let mut match_chain_pos: usize = 0;

        let mut repeat: u8 = 0;
        let mut src_pattern_length: usize = 0;

        let mut candidate_absolute_position = self.get_dictionary_at(Self::get_hash_at(input, off));

        for _ in 0..self.max_attempts {
            if candidate_absolute_position >= absolute_byte_offset
                || absolute_byte_offset - candidate_absolute_position > self.chain_mask()
            {
                break;
            }

            let mut match_len: usize = 0;
            let ref_in_input = candidate_absolute_position >= stream_offset;

            if ref_in_input {
                let reference_local_position = candidate_absolute_position - stream_offset;

                let tail_matches_past_best = if best_len >= MIN_MATCH {
                    let check_pos = best_len - 1;
                    #[cfg(not(feature = "safe-encode"))]
                    unsafe {
                        (input.as_ptr().add(reference_local_position + check_pos) as *const u16)
                            .read_unaligned()
                            == (input.as_ptr().add(off + check_pos) as *const u16).read_unaligned()
                    }
                    #[cfg(feature = "safe-encode")]
                    {
                        input[reference_local_position + check_pos] == input[off + check_pos]
                            && input[reference_local_position + check_pos + 1]
                                == input[off + check_pos + 1]
                    }
                } else {
                    true
                };

                if tail_matches_past_best
                    && self.read_min_match_equals(input, reference_local_position, off)
                {
                    match_len = MIN_MATCH
                        + self.common_bytes(
                            input,
                            reference_local_position + MIN_MATCH,
                            off + MIN_MATCH,
                            match_limit,
                        );
                    if match_len > best_len {
                        best_len = match_len;
                        best_offset = (absolute_byte_offset - candidate_absolute_position) as u16;
                    }
                }

                // Chain swap: only for input matches
                if match_len == best_len
                    && match_len >= MIN_MATCH
                    && candidate_absolute_position + best_len <= absolute_byte_offset
                {
                    const K_TRIGGER: i32 = 4;
                    let mut dist_to_next: u16 = 1;
                    let end = (best_len - MIN_MATCH + 1) as i32;
                    let mut accel: i32 = 1 << K_TRIGGER;
                    let mut pos: i32 = 0;
                    while pos < end {
                        let candidate_dist = self
                            .chain_delta(candidate_absolute_position.wrapping_add(pos as usize));
                        let step = accel >> K_TRIGGER;
                        accel += 1;
                        if candidate_dist > dist_to_next {
                            dist_to_next = candidate_dist;
                            match_chain_pos = pos as usize;
                            accel = 1 << K_TRIGGER;
                        }
                        pos += step;
                    }
                    if dist_to_next > 1 {
                        if (dist_to_next as usize) > candidate_absolute_position {
                            break;
                        }
                        candidate_absolute_position -= dist_to_next as usize;
                        continue;
                    }
                }

                match self.pattern_chain_action(
                    input,
                    off,
                    match_limit,
                    absolute_byte_offset,
                    stream_offset,
                    candidate_absolute_position,
                    match_chain_pos,
                    &mut repeat,
                    &mut src_pattern_length,
                    &mut best_len,
                    &mut best_offset,
                ) {
                    PatternChainAction::RetryCandidate(new_abs) => {
                        candidate_absolute_position = new_abs;
                        continue;
                    }
                    PatternChainAction::StopSearch => break,
                    PatternChainAction::Noop => {}
                }
            } else if !external_dictionary.is_empty()
                && candidate_absolute_position >= external_dictionary_stream_offset
            {
                let reference_local_position =
                    candidate_absolute_position - external_dictionary_stream_offset;

                let min_match_ok = if reference_local_position + 4 <= external_dictionary.len() {
                    super::compress::get_batch(external_dictionary, reference_local_position)
                        == super::compress::get_batch(input, off)
                } else if reference_local_position < external_dictionary.len() {
                    read_u32_from_two_slices(external_dictionary, reference_local_position, input)
                        == super::compress::get_batch(input, off)
                } else {
                    false
                };

                if min_match_ok {
                    match_len = MIN_MATCH
                        + count_forward_external_dictionary(
                            input,
                            off + MIN_MATCH,
                            external_dictionary,
                            reference_local_position + MIN_MATCH,
                            match_limit,
                        );
                    if match_len > best_len {
                        best_len = match_len;
                        best_offset = (absolute_byte_offset - candidate_absolute_position) as u16;
                    }
                }
                // Skip chain swap and pattern analysis for external_dictionary matches
            }

            let delta = self.chain_delta(candidate_absolute_position + match_chain_pos) as usize;
            if delta == 0 || delta > candidate_absolute_position {
                break;
            }
            candidate_absolute_position -= delta;
        }

        if best_len > min_len as usize {
            (best_len as u32, best_offset)
        } else {
            (0, 0)
        }
    }
}

/// Optimal parsing state for a single position.
/// Matches C's LZ4HC_optimal_t layout (4x i32 = 16 bytes).
/// Using i32 for match offset/length instead of u16 avoids costly widening conversions
/// on every access in the hot DP loop (15-20% regression with u16).
/// The 4099-entry optimal-parsing DP buffer is ~64KB.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct OptimalState {
    /// Best known encoded size (byte-cost model) to reach this position.
    path_cost: i32,
    /// Length of the literal run immediately before this state (DP bookkeeping).
    literal_prefix_len: i32,
    /// Copy offset for the sequence ending here; `0` means literal step.
    match_offset: i32,
    /// Match length for this step; `1` means a single literal byte.
    match_len: i32,
}

impl OptimalState {
    const SENTINEL: Self = Self {
        path_cost: i32::MAX,
        literal_prefix_len: 0,
        match_offset: 0,
        match_len: 0,
    };
}

/// Calculate the cost in bytes of encoding literals
#[inline]
fn literals_price(litlen: i32) -> i32 {
    let mut price = litlen;
    if litlen >= RUN_MASK as i32 {
        price += 1 + (litlen - RUN_MASK as i32) / 255;
    }
    price
}

/// Calculate the cost in bytes of encoding a sequence (literals + match)
#[inline]
fn sequence_price(litlen: i32, mlen: i32) -> i32 {
    // token + 16-bit offset
    let mut price: i32 = 1 + 2;

    // literal length encoding
    price += literals_price(litlen);

    // match length encoding (mlen >= MINMATCH)
    let ml_code = mlen - MIN_MATCH as i32;
    if ml_code >= 15 {
        price += 1 + (ml_code - 15) / 255;
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

impl CompressTableHC {
    /// Create a new table. The internal variant is lazily chosen on first use.
    pub fn new() -> Self {
        CompressTableHC {
            inner: CompressTableHCInner::Mid(HashTableMid::new()),
        }
    }

    /// Prepare the table for a new linked block without clearing existing entries.
    /// Called by `FrameEncoder` between blocks in linked mode.
    ///
    /// `params` must be [`hc_level_params`] with the same clamped level used for the following
    /// [`compress_hc_linked`] call (typically `hc_level_params(level.min(12))`).
    #[cfg(feature = "frame")]
    pub(crate) fn prepare_linked_block(
        &mut self,
        params: HcLevelParams,
        absolute_block_start_position: usize,
    ) {
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
                        ht.prepare_linked_block(max_attempts, absolute_block_start_position);
                    }
                    _ => {
                        let mut ht = HashTableHCU32::new(max_attempts, MAX_DISTANCE_HC);
                        ht.prepare_linked_block(max_attempts, absolute_block_start_position);
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
            compress_hc_internal(input, 0, output, &mut ht, &[], 0)
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
            let mid = match &mut table.inner {
                CompressTableHCInner::Mid(mid) => {
                    mid.reset();
                    mid
                }
                _ => {
                    table.inner = CompressTableHCInner::Mid(HashTableMid::new());
                    match &mut table.inner {
                        CompressTableHCInner::Mid(mid) => mid,
                        _ => unreachable!(),
                    }
                }
            };
            compress_mid_internal(input, 0, output, mid, &[], 0)
        }
        HcCompressionStrategy::HashChain | HcCompressionStrategy::Optimal => {
            let ht = match &mut table.inner {
                CompressTableHCInner::HC(ht) => {
                    ht.reset(params.max_attempts, input.len());
                    ht
                }
                _ => {
                    table.inner = CompressTableHCInner::HC(HashTableHCU32::new(
                        params.max_attempts,
                        input.len(),
                    ));
                    match &mut table.inner {
                        CompressTableHCInner::HC(ht) => ht,
                        _ => unreachable!(),
                    }
                }
            };

            if matches!(params.strategy, HcCompressionStrategy::Optimal) {
                compress_opt_internal(input, 0, output, params, ht, &[], 0)
            } else {
                compress_hc_internal(input, 0, output, ht, &[], 0)
            }
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
    external_dictionary: &[u8],
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
            compress_mid_internal(
                input,
                input_pos,
                output,
                mid,
                external_dictionary,
                stream_offset,
            )
        }
        HcCompressionStrategy::HashChain => {
            let ht = match &mut table.inner {
                CompressTableHCInner::HC(ht) => ht,
                _ => unreachable!(
                    "prepare_linked_block should have ensured HC variant for HC levels"
                ),
            };
            compress_hc_internal(
                input,
                input_pos,
                output,
                ht,
                external_dictionary,
                stream_offset,
            )
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
                external_dictionary,
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

/// Hash table for lz4mid algorithm - contains two tables (4-byte and 8-byte)
pub(crate) struct HashTableMid {
    hash4: Box<[u32; LZ4MID_HASHTABLE_SIZE]>,
    hash8: Box<[u32; LZ4MID_HASHTABLE_SIZE]>,
}

impl HashTableMid {
    fn new() -> Self {
        HashTableMid {
            hash4: vec![0u32; LZ4MID_HASHTABLE_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
            hash8: vec![0u32; LZ4MID_HASHTABLE_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
        }
    }

    /// Reset the table for reuse by zeroing both hash tables.
    fn reset(&mut self) {
        self.hash4.fill(0);
        self.hash8.fill(0);
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
        for entry in self.hash4.iter_mut() {
            *entry = entry.saturating_sub(delta32);
        }
        for entry in self.hash8.iter_mut() {
            *entry = entry.saturating_sub(delta32);
        }
    }
}

/// 4-byte hash for lz4mid (same multiplier as fast algorithm)
#[inline]
fn get_hash4_mid(input: &[u8], pos: usize) -> usize {
    let v = super::compress::get_batch(input, pos);
    (v.wrapping_mul(2654435761) >> (32 - LZ4MID_HASH_LOG)) as usize
}

/// 8-byte hash for lz4mid (hashes lower 56 bits for longer match detection)
#[inline]
fn get_hash8_mid(input: &[u8], pos: usize) -> usize {
    // Use get_batch_arch for the raw read (eliminates bounds check in unsafe mode),
    // then convert to u64 for the 56-bit hash computation.
    #[cfg(target_pointer_width = "64")]
    {
        let v = super::compress::get_batch_arch(input, pos) as u64;
        let v56 = v.to_le() << 8;
        ((v56.wrapping_mul(58295818150454627)) >> (64 - LZ4MID_HASH_LOG)) as usize
    }
    #[cfg(not(target_pointer_width = "64"))]
    {
        let v = u64::from_le_bytes(input[pos..pos + 8].try_into().unwrap());
        let v56 = v << 8;
        ((v56.wrapping_mul(58295818150454627)) >> (64 - LZ4MID_HASH_LOG)) as usize
    }
}

/// Internal lz4mid compression.
/// `input_pos` is where the current block starts (positions before it are prefix).
/// `external_dictionary` and `stream_offset` support linked block mode.
fn compress_mid_internal(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    table: &mut HashTableMid,
    external_dictionary: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    let output_start = output.pos();

    if input.len() - input_pos < MIN_BYTES_FROM_CURSOR_TO_BLOCK_END + 1 {
        handle_last_literals(output, &input[input_pos..]);
        return Ok(output.pos() - output_start);
    }

    let hash4 = &mut *table.hash4;
    let hash8 = &mut *table.hash8;

    let external_dictionary_stream_offset = stream_offset - external_dictionary.len();

    let mut cursor_pos = input_pos;
    let mut literal_anchor_pos = input_pos;
    let input_end = input.len();
    // Inclusive max main-loop `cursor_pos`: at least `MIN_BYTES_FROM_CURSOR_TO_BLOCK_END` bytes remain from `cursor_pos` to `input_end`.
    let max_main_cursor_pos = input_end.saturating_sub(MIN_BYTES_FROM_CURSOR_TO_BLOCK_END);
    // Inclusive max `cursor_pos` for inserting 8-byte hashes (`cursor_pos + 8 <= input_end`).
    let max_hash8_probe_pos = input_end.saturating_sub(8);
    // Exclusive end for extending matches: last `END_OFFSET` bytes are handled as literals/trailer.
    let match_extension_end_pos = input_end - END_OFFSET;

    #[inline]
    fn add_hash8(
        hash8: &mut [u32; LZ4MID_HASHTABLE_SIZE],
        input: &[u8],
        pos: usize,
        input_end: usize,
        stream_offset: usize,
    ) {
        if pos + 8 <= input_end {
            let h = get_hash8_mid(input, pos);
            hash8[h] = (pos + stream_offset) as u32;
        }
    }

    #[inline]
    fn add_hash4(
        hash4: &mut [u32; LZ4MID_HASHTABLE_SIZE],
        input: &[u8],
        pos: usize,
        input_end: usize,
        stream_offset: usize,
    ) {
        if pos + 4 <= input_end {
            let h = get_hash4_mid(input, pos);
            hash4[h] = (pos + stream_offset) as u32;
        }
    }

    /// Resolve an absolute hash table position to a source slice and local index.
    /// Returns `(source, local_index, distance_from_cursor)`.
    #[inline]
    fn resolve_candidate<'a>(
        candidate_absolute_position: usize,
        cursor_absolute_position: usize,
        input: &'a [u8],
        stream_offset: usize,
        external_dictionary: &'a [u8],
        external_dictionary_stream_offset: usize,
    ) -> Option<(&'a [u8], usize, usize)> {
        let distance = cursor_absolute_position.wrapping_sub(candidate_absolute_position);
        if distance == 0 || distance > MAX_DISTANCE {
            return None;
        }
        if candidate_absolute_position >= stream_offset {
            let local = candidate_absolute_position - stream_offset;
            Some((input, local, distance))
        } else if !external_dictionary.is_empty()
            && candidate_absolute_position >= external_dictionary_stream_offset
        {
            let local = candidate_absolute_position - external_dictionary_stream_offset;
            Some((external_dictionary, local, distance))
        } else {
            None
        }
    }

    while cursor_pos <= max_main_cursor_pos {
        let cursor_absolute_position = cursor_pos + stream_offset;

        // Try 8-byte hash first
        let hash_8_index = get_hash8_mid(input, cursor_pos);
        let absolute_position_hash8 = hash8[hash_8_index] as usize;
        hash8[hash_8_index] = cursor_absolute_position as u32;

        if let Some((src8, cand8, dist8)) = resolve_candidate(
            absolute_position_hash8,
            cursor_absolute_position,
            input,
            stream_offset,
            external_dictionary,
            external_dictionary_stream_offset,
        ) {
            let mut probe = cursor_pos;
            let match_len =
                count_same_bytes(input, &mut probe, src8, cand8, match_extension_end_pos);
            if match_len >= MIN_MATCH {
                let mut cur = cursor_pos;
                let mut candidate = cand8;
                let cand_src = src8;
                backtrack_match(
                    input,
                    &mut cur,
                    literal_anchor_pos,
                    cand_src,
                    &mut candidate,
                );
                let match_len = count_same_bytes(
                    input,
                    &mut cur,
                    cand_src,
                    candidate,
                    match_extension_end_pos,
                );
                let match_start = cur - match_len;
                let offset = dist8 as u16;

                add_hash8(hash8, input, match_start + 1, input_end, stream_offset);
                add_hash8(hash8, input, match_start + 2, input_end, stream_offset);
                add_hash4(hash4, input, match_start + 1, input_end, stream_offset);

                encode_sequence(
                    &input[literal_anchor_pos..match_start],
                    output,
                    offset,
                    match_len - MIN_MATCH,
                );

                cursor_pos = cur;
                literal_anchor_pos = cursor_pos;

                if cursor_pos >= 5 && cursor_pos <= max_hash8_probe_pos {
                    add_hash8(hash8, input, cursor_pos - 5, input_end, stream_offset);
                }
                if cursor_pos >= 3 && cursor_pos <= max_hash8_probe_pos {
                    add_hash8(hash8, input, cursor_pos - 3, input_end, stream_offset);
                    add_hash8(hash8, input, cursor_pos - 2, input_end, stream_offset);
                }
                if cursor_pos >= 2 {
                    add_hash4(hash4, input, cursor_pos - 2, input_end, stream_offset);
                }
                if cursor_pos >= 1 {
                    add_hash4(hash4, input, cursor_pos - 1, input_end, stream_offset);
                }
                continue;
            }
        }

        // Try 4-byte hash
        let hash_4_index = get_hash4_mid(input, cursor_pos);
        let absolute_position_hash4 = hash4[hash_4_index] as usize;
        hash4[hash_4_index] = cursor_absolute_position as u32;

        if let Some((src4, cand4, dist4)) = resolve_candidate(
            absolute_position_hash4,
            cursor_absolute_position,
            input,
            stream_offset,
            external_dictionary,
            external_dictionary_stream_offset,
        ) {
            let mut probe = cursor_pos;
            let match_len =
                count_same_bytes(input, &mut probe, src4, cand4, match_extension_end_pos);
            if match_len >= MIN_MATCH {
                let mut best_cursor_pos = cursor_pos;
                let mut best_src: &[u8] = src4;
                let mut best_cand = cand4;
                let mut best_len = match_len;
                let mut best_dist = dist4;

                if cursor_pos + 1 <= max_main_cursor_pos {
                    let hash_8_next_index = get_hash8_mid(input, cursor_pos + 1);
                    let absolute_position_hash8_next = hash8[hash_8_next_index] as usize;
                    if let Some((src8n, cand8n, dist8n)) = resolve_candidate(
                        absolute_position_hash8_next,
                        cursor_absolute_position + 1,
                        input,
                        stream_offset,
                        external_dictionary,
                        external_dictionary_stream_offset,
                    ) {
                        let mut probe_next = cursor_pos + 1;
                        let len_next = count_same_bytes(
                            input,
                            &mut probe_next,
                            src8n,
                            cand8n,
                            match_extension_end_pos,
                        );
                        if len_next > best_len {
                            hash8[hash_8_next_index] = (cursor_pos + 1 + stream_offset) as u32;
                            best_cursor_pos = cursor_pos + 1;
                            best_src = src8n;
                            best_cand = cand8n;
                            best_len = len_next;
                            best_dist = dist8n;
                        }
                    }
                }
                let _ = best_len;

                let mut cur = best_cursor_pos;
                let mut candidate = best_cand;
                backtrack_match(
                    input,
                    &mut cur,
                    literal_anchor_pos,
                    best_src,
                    &mut candidate,
                );
                let match_len = count_same_bytes(
                    input,
                    &mut cur,
                    best_src,
                    candidate,
                    match_extension_end_pos,
                );
                let match_start = cur - match_len;
                let offset = best_dist as u16;

                add_hash8(hash8, input, match_start + 1, input_end, stream_offset);
                add_hash8(hash8, input, match_start + 2, input_end, stream_offset);
                add_hash4(hash4, input, match_start + 1, input_end, stream_offset);

                encode_sequence(
                    &input[literal_anchor_pos..match_start],
                    output,
                    offset,
                    match_len - MIN_MATCH,
                );

                cursor_pos = cur;
                literal_anchor_pos = cursor_pos;

                if cursor_pos >= 5 && cursor_pos <= max_hash8_probe_pos {
                    add_hash8(hash8, input, cursor_pos - 5, input_end, stream_offset);
                }
                if cursor_pos >= 3 && cursor_pos <= max_hash8_probe_pos {
                    add_hash8(hash8, input, cursor_pos - 3, input_end, stream_offset);
                    add_hash8(hash8, input, cursor_pos - 2, input_end, stream_offset);
                }
                if cursor_pos >= 2 {
                    add_hash4(hash4, input, cursor_pos - 2, input_end, stream_offset);
                }
                if cursor_pos >= 1 {
                    add_hash4(hash4, input, cursor_pos - 1, input_end, stream_offset);
                }
                continue;
            }
        }

        // No match - skip with acceleration
        cursor_pos += 1 + ((cursor_pos - literal_anchor_pos) >> 9);
    }

    if literal_anchor_pos < input_end {
        handle_last_literals(output, &input[literal_anchor_pos..]);
    }

    Ok(output.pos() - output_start)
}

/// Internal HC compression implementation using hash chain algorithm.
/// `input_pos` is where the current block starts (positions before it are prefix).
/// `external_dictionary` and `stream_offset` support linked block mode.
fn compress_hc_internal(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    ht: &mut HashTableHCU32,
    external_dictionary: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    let output_start_pos = output.pos();
    if input.len() - input_pos < MIN_BYTES_FROM_CURSOR_TO_BLOCK_END + 1 {
        handle_last_literals(output, &input[input_pos..]);
        return Ok(output.pos() - output_start_pos);
    }

    let input_end = input.len();
    // Inclusive max main-loop cursor: at least `MIN_BYTES_FROM_CURSOR_TO_BLOCK_END` bytes from cursor to `input_end`.
    let max_main_cursor_pos = input_end - MIN_BYTES_FROM_CURSOR_TO_BLOCK_END;
    // Do not extend matches into the last `LAST_LITERALS` bytes (they are literals).
    let match_extension_end_pos = input_end - LAST_LITERALS;

    let mut scan_pos = input_pos + 1;
    let mut literal_anchor_pos = input_pos;
    let mut match0;
    let mut match1 = Match::new();
    let mut match2 = Match::new();
    let mut match3 = Match::new();

    while scan_pos < max_main_cursor_pos {
        if !ht.insert_and_find_best_match(
            input,
            scan_pos as u32,
            match_extension_end_pos as u32,
            &mut match1,
            external_dictionary,
            stream_offset,
        ) {
            scan_pos += 1;
            continue;
        }

        match0 = match1;

        loop {
            debug_assert!(match1.start_position as usize >= literal_anchor_pos);
            if match1.end() > max_main_cursor_pos
                || !ht.insert_and_find_wider_match(
                    input,
                    (match1.end() - 2) as u32,
                    match1.start_position,
                    match_extension_end_pos as u32,
                    match1.match_length,
                    &mut match2,
                    external_dictionary,
                    stream_offset,
                )
            {
                match1.encode_to(&input, literal_anchor_pos, output);
                scan_pos = match1.end();
                literal_anchor_pos = scan_pos;
                break;
            }

            if match0.start_position < match1.start_position
                && (match2.start_position as usize)
                    < match1.start_position as usize + match0.match_length as usize
            {
                match1 = match0;
            }
            debug_assert!(match2.start_position >= match1.start_position);

            if (match2.start_position - match1.start_position) < 3 {
                match1 = match2;
                continue;
            }

            let restart = loop {
                if (match2.start_position - match1.start_position) < OPTIMAL_ML as u32 {
                    let mut new_match_len = match1.match_length as usize;
                    if new_match_len > OPTIMAL_ML {
                        new_match_len = OPTIMAL_ML;
                    }
                    if match1.start_position as usize + new_match_len
                        > match2.end().saturating_sub(MINMATCH)
                    {
                        new_match_len = (match2.start_position - match1.start_position) as usize
                            + (match2.match_length as usize).saturating_sub(MINMATCH);
                    }
                    let correction = new_match_len
                        .saturating_sub((match2.start_position - match1.start_position) as usize);
                    if correction > 0 {
                        match2.fix(correction);
                    }
                }

                if match2.end() > max_main_cursor_pos
                    || !ht.insert_and_find_wider_match(
                        input,
                        (match2.end() - 3) as u32,
                        match2.start_position,
                        match_extension_end_pos as u32,
                        match2.match_length,
                        &mut match3,
                        external_dictionary,
                        stream_offset,
                    )
                {
                    if (match2.start_position as usize) < match1.end() {
                        match1.match_length =
                            (match2.start_position - match1.start_position) as u32;
                    }
                    match1.encode_to(input, literal_anchor_pos, output);
                    scan_pos = match1.end();
                    literal_anchor_pos = scan_pos;
                    match2.encode_to(input, literal_anchor_pos, output);
                    scan_pos = match2.end();
                    literal_anchor_pos = scan_pos;
                    break false;
                }

                if (match3.start_position as usize) < match1.end() + 3 {
                    if match3.start_position as usize >= match1.end() {
                        if (match2.start_position as usize) < match1.end() {
                            let correction = match1.end() - match2.start_position as usize;
                            match2.fix(correction);
                            if (match2.match_length as usize) < MINMATCH {
                                match2 = match3;
                            }
                        }

                        match1.encode_to(input, literal_anchor_pos, output);
                        scan_pos = match1.end();
                        literal_anchor_pos = scan_pos;

                        match1 = match3;
                        match0 = match2;

                        break true;
                    }

                    match2 = match3;
                    continue;
                }

                if (match2.start_position as usize) < match1.end() {
                    if (match2.start_position - match1.start_position) < ML_MASK as u32 {
                        if match1.match_length as usize > OPTIMAL_ML {
                            match1.match_length = OPTIMAL_ML as u32;
                        }
                        if match1.end() > match2.end() - MINMATCH {
                            match1.match_length =
                                (match2.end() - match1.start_position as usize - MINMATCH) as u32;
                        }
                        let correction = match1.end() - match2.start_position as usize;
                        match2.fix(correction);
                    } else {
                        match1.match_length =
                            (match2.start_position - match1.start_position) as u32;
                    }
                }

                match1.encode_to(input, literal_anchor_pos, output);
                scan_pos = match1.end();
                literal_anchor_pos = scan_pos;

                match1 = match2;
                match2 = match3;

                continue;
            };

            if restart {
                continue;
            }
            break;
        }
    }

    handle_last_literals(output, &input[literal_anchor_pos..input_end]);
    Ok(output.pos() - output_start_pos)
}

/// Emit LZ4 sequences from DP states `optimal_states[0..last_match_pos)` (`match_len == 1` is one literal step).
#[inline]
fn encode_optimal_path_from_dp(
    optimal_states: &[OptimalState],
    last_match_pos: usize,
    input: &[u8],
    literal_anchor_pos: &mut usize,
    cursor_pos: &mut usize,
    output: &mut impl Sink,
) {
    let mut encode_step_index: usize = 0;
    while encode_step_index < last_match_pos {
        let step_match_length = optimal_states[encode_step_index].match_len as usize;
        let match_offset = optimal_states[encode_step_index].match_offset as u16;

        if step_match_length == 1 {
            *cursor_pos += 1;
            encode_step_index += 1;
            continue;
        }

        encode_sequence(
            &input[*literal_anchor_pos..*cursor_pos],
            output,
            match_offset,
            step_match_length - MIN_MATCH,
        );

        *cursor_pos += step_match_length;
        *literal_anchor_pos = *cursor_pos;
        encode_step_index += step_match_length;
    }
}

/// Internal optimal parsing compression implementation
fn compress_opt_internal(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    level_params: HcLevelParams,
    ht: &mut HashTableHCU32,
    external_dictionary: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    let output_start_pos = output.pos();

    if input.len() - input_pos < MIN_BYTES_FROM_CURSOR_TO_BLOCK_END + 1 {
        handle_last_literals(output, &input[input_pos..]);
        return Ok(output.pos() - output_start_pos);
    }

    let input_end = input.len();
    // Inclusive max main-loop `cursor_pos`: at least `MIN_BYTES_FROM_CURSOR_TO_BLOCK_END` bytes remain from `cursor_pos` to `input_end`.
    let max_main_cursor_pos = input_end - MIN_BYTES_FROM_CURSOR_TO_BLOCK_END;
    // Do not extend matches into the last `LAST_LITERALS` bytes (they are literals).
    let match_extension_end_pos = input_end - LAST_LITERALS;

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

    let mut literal_anchor_pos = input_pos;
    let mut cursor_pos = input_pos;

    let mut optimal_states = vec![OptimalState::SENTINEL; LZ4_OPT_NUM + TRAILING_LITERALS];

    let sufficient_match_len = sufficient_match_len.min(LZ4_OPT_NUM - 1);

    while cursor_pos <= max_main_cursor_pos {
        let literal_run_length = (cursor_pos - literal_anchor_pos) as i32;

        let (first_match_length, first_match_offset) = ht.find_longer_match(
            input,
            cursor_pos as u32,
            match_extension_end_pos as u32,
            (MIN_MATCH - 1) as u32,
            external_dictionary,
            stream_offset,
        );
        if first_match_length == 0 {
            cursor_pos += 1;
            continue;
        }
        let first_match_length = first_match_length as usize;

        // If match is good enough, encode immediately
        if first_match_length >= sufficient_match_len {
            encode_sequence(
                &input[literal_anchor_pos..cursor_pos],
                output,
                first_match_offset,
                first_match_length - MIN_MATCH,
            );
            cursor_pos += first_match_length;
            literal_anchor_pos = cursor_pos;
            continue;
        }

        // Initialize optimal parsing state for literals
        for literal_slot_index in 0..MIN_MATCH as i32 {
            let cost = literals_price(literal_run_length + literal_slot_index);
            optimal_states[literal_slot_index as usize].match_len = 1;
            optimal_states[literal_slot_index as usize].match_offset = 0;
            optimal_states[literal_slot_index as usize].literal_prefix_len =
                literal_run_length + literal_slot_index;
            optimal_states[literal_slot_index as usize].path_cost = cost;
        }

        // Set prices using initial match
        let initial_match_length_cap = first_match_length.min(LZ4_OPT_NUM - 1);
        for match_length in MIN_MATCH..=initial_match_length_cap {
            let cost = sequence_price(literal_run_length, match_length as i32);
            optimal_states[match_length].match_len = match_length as i32;
            optimal_states[match_length].match_offset = first_match_offset as i32;
            optimal_states[match_length].literal_prefix_len = literal_run_length;
            optimal_states[match_length].path_cost = cost;
        }

        let mut last_match_pos = first_match_length;

        // Add trailing literals after the match
        for trailing_literal_count in 1..=TRAILING_LITERALS {
            let state_index = last_match_pos + trailing_literal_count;
            if state_index < optimal_states.len() {
                optimal_states[state_index].match_len = 1; // literal
                optimal_states[state_index].match_offset = 0;
                optimal_states[state_index].literal_prefix_len = trailing_literal_count as i32;
                optimal_states[state_index].path_cost = optimal_states[last_match_pos].path_cost
                    + literals_price(trailing_literal_count as i32);
            }
        }

        // Refine costs along the optimal window; may encode a prefix and restart the main step.
        let mut skip_reverse_traversal_and_final_encode = false;
        let mut opt_window_index: usize = 1;
        while opt_window_index < last_match_pos {
            let inner_scan_pos = cursor_pos + opt_window_index;

            if inner_scan_pos > max_main_cursor_pos {
                break;
            }

            if full_optimal_update {
                // Not useful to search here if next position has same (or lower) cost
                if optimal_states[opt_window_index + 1].path_cost
                    <= optimal_states[opt_window_index].path_cost
                    && optimal_states[opt_window_index + MIN_MATCH].path_cost
                        < optimal_states[opt_window_index].path_cost + 3
                {
                    opt_window_index += 1;
                    continue;
                }
            } else {
                // Not useful to search here if next position has same (or lower) cost
                if optimal_states[opt_window_index + 1].path_cost
                    <= optimal_states[opt_window_index].path_cost
                {
                    opt_window_index += 1;
                    continue;
                }
            }

            // Find longer match at current position
            let min_match_len_for_search: u32 = if full_optimal_update {
                (MIN_MATCH - 1) as u32
            } else {
                (last_match_pos - opt_window_index) as u32
            };

            let (new_match_length, new_match_offset) = ht.find_longer_match(
                input,
                inner_scan_pos as u32,
                match_extension_end_pos as u32,
                min_match_len_for_search,
                external_dictionary,
                stream_offset,
            );
            if new_match_length == 0 {
                opt_window_index += 1;
                continue;
            }
            let new_match_length = new_match_length as usize;

            // If match is good enough or extends beyond buffer, encode immediately
            if new_match_length >= sufficient_match_len
                || new_match_length + opt_window_index >= LZ4_OPT_NUM
            {
                let capped_match_length = new_match_length;

                // Set last_match_pos = opt_window_index + 1 as in C code
                last_match_pos = opt_window_index + 1;

                // Reverse traversal starting from opt_window_index
                let mut selected_match_length = capped_match_length as i32;
                let mut selected_match_offset = new_match_offset as i32;
                let mut candidate_pos = opt_window_index;
                loop {
                    let next_match_length = optimal_states[candidate_pos].match_len;
                    let next_match_offset = optimal_states[candidate_pos].match_offset;
                    optimal_states[candidate_pos].match_len = selected_match_length;
                    optimal_states[candidate_pos].match_offset = selected_match_offset;
                    selected_match_length = next_match_length;
                    selected_match_offset = next_match_offset;
                    if (next_match_length as usize) > candidate_pos {
                        break;
                    }
                    candidate_pos -= next_match_length as usize;
                }

                encode_optimal_path_from_dp(
                    &optimal_states,
                    last_match_pos,
                    input,
                    &mut literal_anchor_pos,
                    &mut cursor_pos,
                    output,
                );

                skip_reverse_traversal_and_final_encode = true;
                break;
            }

            // Update prices for literals before the match
            {
                let base_literal_prefix_length =
                    optimal_states[opt_window_index].literal_prefix_len;
                for literal_step_length in 1..MIN_MATCH as i32 {
                    let state_index = opt_window_index + literal_step_length as usize;
                    let price = optimal_states[opt_window_index].path_cost
                        - literals_price(base_literal_prefix_length)
                        + literals_price(base_literal_prefix_length + literal_step_length);
                    if price < optimal_states[state_index].path_cost {
                        optimal_states[state_index].match_len = 1; // literal
                        optimal_states[state_index].match_offset = 0;
                        optimal_states[state_index].literal_prefix_len =
                            base_literal_prefix_length + literal_step_length;
                        optimal_states[state_index].path_cost = price;
                    }
                }
            }

            // Set prices using match at current position
            {
                let refinement_match_length_cap =
                    new_match_length.min(LZ4_OPT_NUM - opt_window_index - 1);
                for match_length in MIN_MATCH..=refinement_match_length_cap {
                    let state_index = opt_window_index + match_length;
                    let (literal_prefix_for_sequence, price) = if optimal_states[opt_window_index]
                        .match_len
                        == 1
                    {
                        let literal_prefix_len =
                            optimal_states[opt_window_index].literal_prefix_len;
                        let base_price = if opt_window_index as i32 > literal_prefix_len {
                            optimal_states[opt_window_index - literal_prefix_len as usize].path_cost
                        } else {
                            0
                        };
                        (
                            literal_prefix_len,
                            base_price + sequence_price(literal_prefix_len, match_length as i32),
                        )
                    } else {
                        (
                            0,
                            optimal_states[opt_window_index].path_cost
                                + sequence_price(0, match_length as i32),
                        )
                    };

                    if state_index > last_match_pos + TRAILING_LITERALS
                        || price <= optimal_states[state_index].path_cost
                    {
                        if match_length == refinement_match_length_cap
                            && last_match_pos < state_index
                        {
                            last_match_pos = state_index;
                        }
                        optimal_states[state_index].match_len = match_length as i32;
                        optimal_states[state_index].match_offset = new_match_offset as i32;
                        optimal_states[state_index].literal_prefix_len =
                            literal_prefix_for_sequence;
                        optimal_states[state_index].path_cost = price;
                    }
                }
            }

            // Complete following positions with literals
            for trailing_literal_count in 1..=TRAILING_LITERALS as i32 {
                let state_index = last_match_pos + trailing_literal_count as usize;
                optimal_states[state_index].match_len = 1; // literal
                optimal_states[state_index].match_offset = 0;
                optimal_states[state_index].literal_prefix_len = trailing_literal_count;
                optimal_states[state_index].path_cost = optimal_states[last_match_pos].path_cost
                    + literals_price(trailing_literal_count);
            }

            opt_window_index += 1;
        }

        if skip_reverse_traversal_and_final_encode {
            continue;
        }

        // Reverse traversal to find the optimal path
        {
            let mut best_match_length = optimal_states[last_match_pos].match_len;
            let mut best_match_offset = optimal_states[last_match_pos].match_offset;
            let mut candidate_pos = last_match_pos - best_match_length as usize;

            loop {
                let next_match_length = optimal_states[candidate_pos].match_len;
                let next_match_offset = optimal_states[candidate_pos].match_offset;
                optimal_states[candidate_pos].match_len = best_match_length;
                optimal_states[candidate_pos].match_offset = best_match_offset;
                best_match_length = next_match_length;
                best_match_offset = next_match_offset;
                if (next_match_length as usize) > candidate_pos {
                    break;
                }
                candidate_pos -= next_match_length as usize;
            }
        }

        encode_optimal_path_from_dp(
            &optimal_states,
            last_match_pos,
            input,
            &mut literal_anchor_pos,
            &mut cursor_pos,
            output,
        );

        // No optimal_states buffer reset needed (matches C behavior)
    }

    // Handle remaining literals
    handle_last_literals(output, &input[literal_anchor_pos..input_end]);
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

#[cfg(test)]
#[test]
fn test_lz4mid_debug() {
    use crate::sink::SliceSink;
    let input = b"The quick brown fox jumps over the lazy dog. The quick brown fox.";
    println!("Input len: {}", input.len());
    println!("Input: {:?}", String::from_utf8_lossy(input));

    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);
    let mut table = HashTableMid::new();
    let size = compress_mid_internal(input, 0, &mut sink, &mut table, &[], 0).unwrap();
    println!("Compressed size: {}", size);
    println!("Compressed: {:02x?}", &output[..size]);

    // Try to decompress
    match decompress(&output[..size], input.len()) {
        Ok(d) => {
            println!("Decompressed: {} bytes", d.len());
            println!("Match: {}", d == input);
        }
        Err(e) => println!("Decompress error: {:?}", e),
    }
}
