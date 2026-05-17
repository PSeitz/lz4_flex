use crate::block::compress::count_same_bytes;
use crate::block::{
    encode_sequence, handle_last_literals, CompressError, LAST_LITERALS, LZ4_MIN_LENGTH, MFLIMIT,
    MINMATCH,
};
use crate::sink::Sink;
use alloc::boxed::Box;
use alloc::vec;

use super::{HASHTABLE_SIZE_HC, MATCH_LENGTH_MASK, MAX_DISTANCE_HC, OPTIMAL_MATCH_LENGTH};

/// Hash table with chain for LZ4 high compression.
///
/// Uses a two-level structure: `dictionary` maps each hash to the most recent input
/// position, and `chain_table` links older positions with the same hash via
/// stored deltas, forming an implicit linked list per hash bucket.
#[derive(Debug)]
pub(super) struct HashTableHCU32 {
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
    /// Byte position in the input where this match starts.
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
        let batch = crate::block::compress::get_batch_arch(input, cur);
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
        if crate::block::compress::get_batch(input, cur - 4) != pattern {
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
        crate::block::compress::get_batch(primary, pos)
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
    crate::block::compress::get_batch(input, pos1) == crate::block::compress::get_batch(input, pos2)
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
        crate::block::compress::get_batch(ext_dict, candidate)
            == crate::block::compress::get_batch(input, cur)
    } else if candidate < ext_dict.len() {
        read_u32_from_two_slices(ext_dict, candidate, input)
            == crate::block::compress::get_batch(input, cur)
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
    hash_hc(crate::block::compress::get_batch(input, pos)) as usize
}

#[inline]
fn chain_table_size(input_len: usize) -> usize {
    input_len.clamp(256, MAX_DISTANCE_HC).next_power_of_two()
}

#[inline(always)]
fn candidate_position_for_distance(start_position: usize, distance: usize) -> u32 {
    (start_position as u32).wrapping_sub(distance as u32)
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
        let v1 = crate::block::compress::get_batch_arch(input, pos1 - len - STEP_SIZE);
        let v2 = crate::block::compress::get_batch_arch(input, pos2 - len - STEP_SIZE);
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
        let v1 = crate::block::compress::get_batch(input, pos1 - 4);
        let v2 = crate::block::compress::get_batch(input, pos2 - 4);
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

/// Count the match between `candidate` and `cur` in `input`.
/// Returns 0 if the candidate does not match or cannot beat `best_match_length`.
#[inline(always)]
fn count_buffer_match(
    input: &[u8],
    candidate: usize,
    cur: usize,
    match_limit: usize,
    best_match_length: usize,
) -> usize {
    let cant_beat_best =
        best_match_length >= MINMATCH && !end_bytes_match(input, candidate, cur, best_match_length);
    if cant_beat_best || !read_min_match_equals(input, candidate, cur) {
        return 0;
    }

    MINMATCH + count_common_bytes(input, candidate + MINMATCH, cur + MINMATCH, match_limit)
}

/// Result of pattern/repeat chain optimization inside [`find_longer_hash_chain_match`]
/// (mirrors LZ4HC repeat detection).
enum PatternChainAction {
    /// Continue with normal chain-step logic at the end of the loop body.
    NoAction,
    /// Set `candidate` and restart the search loop iteration (`continue`).
    RetryCandidate(usize),
    /// Exit the search loop (`break`).
    StopSearch,
}

/// Which byte slice contains a hash-chain candidate position. Stored value is slice-relative.
enum CandidateSource {
    Input(usize),
    ExternalDictionary(usize),
    Unavailable,
}

#[inline(always)]
fn candidate_source(
    candidate: usize,
    stream_offset: usize,
    ext_dict_len: usize,
) -> CandidateSource {
    let ext_dict_stream_offset = stream_offset - ext_dict_len;
    if candidate >= stream_offset {
        return CandidateSource::Input(candidate - stream_offset);
    }

    if ext_dict_len != 0 && candidate >= ext_dict_stream_offset {
        return CandidateSource::ExternalDictionary(candidate - ext_dict_stream_offset);
    }

    CandidateSource::Unavailable
}

impl HashTableHCU32 {
    #[inline]
    pub(super) fn new(max_attempts: usize, input_len: usize) -> Self {
        // Dict table: fixed size, hash function already bounds to this range
        let dictionary = vec![0u32; HASHTABLE_SIZE_HC]
            .into_boxed_slice()
            .try_into()
            .unwrap();

        // Chain table: dynamically sized based on input length
        // min(input_len, MAX_DISTANCE_HC), at least 256, must be power of 2
        let chain_size = chain_table_size(input_len);

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
    pub(super) fn reset(&mut self, max_attempts: usize, input_len: usize) {
        let needed_chain_size = chain_table_size(input_len);

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
    pub(super) fn prepare_linked_block(&mut self, max_attempts: usize, block_start: usize) {
        if self.chain_table.len() < MAX_DISTANCE_HC {
            let mut new_chain = vec![0u16; MAX_DISTANCE_HC].into_boxed_slice();
            let old_len = self.chain_table.len();
            new_chain[..old_len].copy_from_slice(&self.chain_table);
            self.chain_table = new_chain;
        }
        self.next_to_update = block_start;
        self.max_attempts = max_attempts;
    }

    /// Subtract `delta` from every absolute position stored in the hash table.
    /// Used when `stream_offset` approaches `u32::MAX / 2` to prevent overflow.
    #[cfg(feature = "frame")]
    pub(super) fn reposition(&mut self, delta: usize) {
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
            let pattern = crate::block::compress::get_batch(input, cur);
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
        let pattern = crate::block::compress::get_batch(input, cur);
        if match_candidate_relative + 4 > input.len()
            || crate::block::compress::get_batch(input, match_candidate_relative) != pattern
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
///
/// `input` is the full input buffer (prefix + block).
/// `cur` is the position in `input` to search at.
/// `match_limit` is the exclusive end position — matches must not extend past this.
/// `min_match_length` is the minimum match length to beat (current best).
/// `ext_dict` is the external dictionary for linked-block mode (empty if unused).
/// `stream_offset` is the logical position of `input[0]` in the stream.
///
/// Returns `(match_length, offset)`. Length is 0 if no match beats `min_match_length`.
/// Offset is `u16` since the LZ4 format limits back-reference distance to 16 bits.
#[inline]
pub(super) fn find_longer_hash_chain_match(
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

        match candidate_source(candidate, stream_offset, ext_dict.len()) {
            CandidateSource::Input(candidate_relative) => {
                let match_length = count_buffer_match(
                    input,
                    candidate_relative,
                    cur,
                    match_limit,
                    best_match_length,
                );
                if match_length > best_match_length {
                    best_match_length = match_length;
                    best_offset = (cur_absolute - candidate) as u16;
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
            }
            CandidateSource::ExternalDictionary(candidate_relative) => {
                let match_length =
                    try_ext_dict_match(input, cur, match_limit, ext_dict, candidate_relative);
                if match_length > best_match_length {
                    best_match_length = match_length;
                    best_offset = (cur_absolute - candidate) as u16;
                }
            }
            CandidateSource::Unavailable => {}
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

/// Insert `cur` into the hash/chain tables, then search the chain for the
/// longest match starting at `cur`.
///
/// `input` is the full input buffer (prefix + block).
/// `cur` is the position in `input` to search at.
/// `match_limit` is the exclusive end position — matches must not extend past this.
/// `ext_dict` is the external dictionary for linked-block mode (empty if unused).
/// `stream_offset` is the logical position of `input[0]` in the stream.
///
/// Returns the best match found, or `None` if no match of at least `MINMATCH` bytes exists.
fn find_best_hash_chain_match(
    hash_table: &mut HashTableHCU32,
    input: &[u8],
    cur: usize,
    match_limit: usize,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Option<Match> {
    find_wider_hash_chain_match(
        hash_table,
        input,
        cur,
        cur,
        match_limit,
        0,
        ext_dict,
        stream_offset,
    )
}

/// Insert `cur` into the hash/chain tables, then search the chain for a match
/// longer than `min_match_length`, extending both forward and backward.
///
/// `input` is the full input buffer (prefix + block).
/// `cur` is the position in `input` to search at.
/// `start_limit` is the earliest position the match may extend backward to.
/// `match_limit` is the exclusive end position — matches must not extend past this.
/// `min_match_length` is the minimum match length to beat (current best).
/// `ext_dict` is the external dictionary for linked-block mode (empty if unused).
/// `stream_offset` is the logical position of `input[0]` in the stream.
///
/// Returns the best wider match found, or `None` if no match beats `min_match_length`.
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
    let look_back_length = cur - start_limit;

    hash_table.insert(cur as u32, input, stream_offset);

    let mut candidate = hash_table.get_dictionary_at(get_hash_at(input, cur));

    for _ in 0..hash_table.max_attempts {
        if !hash_table.in_range(candidate, cur_absolute) {
            break;
        }

        match candidate_source(candidate, stream_offset, ext_dict.len()) {
            CandidateSource::Input(candidate_relative) => {
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
                            candidate_position_for_distance(cur - backward_length, distance);
                        best_match.start_position = (cur - backward_length) as u32;
                    }
                }
            }
            CandidateSource::ExternalDictionary(candidate_relative) => {
                let match_length =
                    try_ext_dict_match(input, cur, match_limit, ext_dict, candidate_relative);
                if match_length as u32 > best_match.match_length {
                    best_match.match_length = match_length as u32;
                    let distance = cur_absolute - candidate;
                    best_match.candidate = candidate_position_for_distance(cur, distance);
                    best_match.start_position = cur as u32;
                }
            }
            CandidateSource::Unavailable => {}
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

#[inline]
fn encode_match_and_advance(
    input: &[u8],
    output: &mut impl Sink,
    found_match: &Match,
    literal_start: &mut usize,
    cur: &mut usize,
) {
    found_match.encode_to(input, *literal_start, output);
    *cur = found_match.end();
    *literal_start = *cur;
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn find_wider_match_before_end(
    hash_table: &mut HashTableHCU32,
    input: &[u8],
    base_match: &Match,
    bytes_before_end: usize,
    end_pos_check: usize,
    match_limit: usize,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Option<Match> {
    if base_match.end() > end_pos_check {
        return None;
    }

    find_wider_hash_chain_match(
        hash_table,
        input,
        base_match.end() - bytes_before_end,
        base_match.start_position as usize,
        match_limit,
        base_match.match_length as usize,
        ext_dict,
        stream_offset,
    )
}

#[inline(always)]
fn find_following_wider_match(
    hash_table: &mut HashTableHCU32,
    input: &[u8],
    current_match: &Match,
    end_pos_check: usize,
    match_limit: usize,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Option<Match> {
    find_wider_match_before_end(
        hash_table,
        input,
        current_match,
        2,
        end_pos_check,
        match_limit,
        ext_dict,
        stream_offset,
    )
}

#[inline]
fn followup_match_overlaps_previous_match(
    previous_match: &Match,
    current_match: &Match,
    next_match: &Match,
) -> bool {
    previous_match.start_position < current_match.start_position
        && (next_match.start_position as usize)
            < current_match.start_position as usize + previous_match.match_length as usize
}

#[inline]
fn next_match_starts_too_close(current_match: &Match, next_match: &Match) -> bool {
    (next_match.start_position - current_match.start_position) < 3
}

#[inline(always)]
fn trim_next_match_for_current_overlap(current_match: &Match, next_match: &mut Match) {
    let start_delta = next_match.start_position - current_match.start_position;
    if start_delta >= OPTIMAL_MATCH_LENGTH as u32 {
        return;
    }

    let mut capped_length = (current_match.match_length as usize).min(OPTIMAL_MATCH_LENGTH);
    if current_match.start_position as usize + capped_length
        > next_match.end().saturating_sub(MINMATCH)
    {
        capped_length =
            start_delta as usize + (next_match.match_length as usize).saturating_sub(MINMATCH);
    }

    let overlap = capped_length.saturating_sub(start_delta as usize);
    if overlap > 0 {
        next_match.trim_front(overlap);
    }
}

/// Result of the three-match resolution loop.
enum ResolveAction {
    /// All matches encoded. `cur` and `literal_start` already advanced.
    Done,
    /// `current_match` was encoded. Restart lazy evaluation with the returned matches.
    Restart {
        current_match: Match,
        previous_match: Match,
    },
}

/// Resolve overlapping matches (`current_match`, `next_match`, and potentially `third_match`).
/// Encodes sequences to `output` and advances `cur`/`literal_start`.
fn resolve_overlapping_matches(
    input: &[u8],
    output: &mut impl Sink,
    hash_table: &mut HashTableHCU32,
    ext_dict: &[u8],
    stream_offset: usize,
    end_pos_check: usize,
    match_limit: usize,
    current_match: &mut Match,
    next_match: &mut Match,
    cur: &mut usize,
    literal_start: &mut usize,
) -> ResolveAction {
    loop {
        trim_next_match_for_current_overlap(current_match, next_match);

        let Some(third_match) = find_wider_match_before_end(
            hash_table,
            input,
            next_match,
            3,
            end_pos_check,
            match_limit,
            ext_dict,
            stream_offset,
        ) else {
            if (next_match.start_position as usize) < current_match.end() {
                current_match.match_length =
                    next_match.start_position - current_match.start_position;
            }
            encode_match_and_advance(input, output, current_match, literal_start, cur);
            encode_match_and_advance(input, output, next_match, literal_start, cur);
            return ResolveAction::Done;
        };

        let third_match_starts_near_current_end =
            (third_match.start_position as usize) < current_match.end() + 3;

        // third_match starts right after current_match — encode current_match, then continue.
        if third_match_starts_near_current_end
            && third_match.start_position as usize >= current_match.end()
        {
            if (next_match.start_position as usize) < current_match.end() {
                let overlap = current_match.end() - next_match.start_position as usize;
                next_match.trim_front(overlap);
                if (next_match.match_length as usize) < MINMATCH {
                    *next_match = third_match;
                }
            }
            encode_match_and_advance(input, output, current_match, literal_start, cur);
            return ResolveAction::Restart {
                current_match: third_match,
                previous_match: *next_match,
            };
        }

        // third_match starts too close to current_match — treat it as the new next_match.
        if third_match_starts_near_current_end {
            *next_match = third_match;
            continue;
        }

        // Resolve overlap between current_match and next_match.
        if (next_match.start_position as usize) < current_match.end() {
            if (next_match.start_position - current_match.start_position) < MATCH_LENGTH_MASK as u32
            {
                if current_match.match_length as usize > OPTIMAL_MATCH_LENGTH {
                    current_match.match_length = OPTIMAL_MATCH_LENGTH as u32;
                }
                if current_match.end() > next_match.end() - MINMATCH {
                    current_match.match_length = (next_match.end()
                        - current_match.start_position as usize
                        - MINMATCH) as u32;
                }
                let overlap = current_match.end() - next_match.start_position as usize;
                next_match.trim_front(overlap);
            } else {
                current_match.match_length =
                    next_match.start_position - current_match.start_position;
            }
        }

        // Encode current_match, then continue resolving with next_match and third_match.
        encode_match_and_advance(input, output, current_match, literal_start, cur);
        *current_match = *next_match;
        *next_match = third_match;
    }
}

/// Internal high-compression implementation for the hash-chain strategy.
/// `input_pos` is where the current block starts (positions before it are prefix).
/// `ext_dict` and `stream_offset` support linked block mode.
pub(super) fn compress_hash_chain_internal(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    hash_table: &mut HashTableHCU32,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    let output_start_pos = output.pos();
    if input.len() - input_pos < LZ4_MIN_LENGTH {
        handle_last_literals(output, &input[input_pos..]);
        return Ok(output.pos() - output_start_pos);
    }

    let input_end = input.len();
    // Inclusive max main-loop cursor: at least `MFLIMIT` bytes from cursor to `input_end`.
    let end_pos_check = input_end - MFLIMIT;
    // Do not extend matches into the last `LAST_LITERALS` bytes (they are literals).
    let match_limit = input_end - LAST_LITERALS;

    // Match C's LZ4HC main loop: start at block start and scan through `mflimit` inclusive.
    let mut cur = input_pos;
    let mut literal_start = input_pos;
    let mut previous_match;
    let mut current_match;
    let mut next_match;

    while cur <= end_pos_check {
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

        current_match = found_match;
        previous_match = current_match;

        // Lazy match evaluation: start from current_match and keep looking slightly ahead.
        loop {
            debug_assert!(current_match.start_position as usize >= literal_start);

            let Some(found_match) = find_following_wider_match(
                hash_table,
                input,
                &current_match,
                end_pos_check,
                match_limit,
                ext_dict,
                stream_offset,
            ) else {
                encode_match_and_advance(
                    input,
                    output,
                    &current_match,
                    &mut literal_start,
                    &mut cur,
                );
                break;
            };

            next_match = found_match;

            if followup_match_overlaps_previous_match(&previous_match, &current_match, &next_match)
            {
                current_match = previous_match;
            }
            debug_assert!(next_match.start_position >= current_match.start_position);

            if next_match_starts_too_close(&current_match, &next_match) {
                current_match = next_match;
                continue;
            }

            match resolve_overlapping_matches(
                input,
                output,
                hash_table,
                ext_dict,
                stream_offset,
                end_pos_check,
                match_limit,
                &mut current_match,
                &mut next_match,
                &mut cur,
                &mut literal_start,
            ) {
                ResolveAction::Done => break,
                ResolveAction::Restart {
                    current_match: restart_match,
                    previous_match: restart_previous_match,
                } => {
                    current_match = restart_match;
                    previous_match = restart_previous_match;
                }
            }
        }
    }

    handle_last_literals(output, &input[literal_start..input_end]);
    Ok(output.pos() - output_start_pos)
}
