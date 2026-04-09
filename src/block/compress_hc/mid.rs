//! Intermediate compression (levels 1-2)
//! Uses two hash tables (4-byte and 8-byte) for better compression than fast algorithm while being faster than HC.
//!
use crate::block::compress::{backtrack_match, count_same_bytes};
use crate::block::{
    encode_sequence, handle_last_literals, CompressError, END_OFFSET, LZ4_MIN_LENGTH, MAX_DISTANCE,
    MFLIMIT, MINMATCH,
};
use crate::sink::Sink;
use alloc::boxed::Box;
use alloc::vec;

use super::{LZ4MID_HASHTABLE_SIZE, LZ4MID_HASH_LOG};

/// Hash table for lz4mid algorithm — two tables keyed by 4-byte and 8-byte input sequences.
pub(super) struct HashTableMid {
    table_4byte: Box<[u32; LZ4MID_HASHTABLE_SIZE]>,
    table_8byte: Box<[u32; LZ4MID_HASHTABLE_SIZE]>,
}

impl HashTableMid {
    pub(super) fn new() -> Self {
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
    /// Reset the table for reuse.
    ///
    /// Stale entries are harmless: `resolve_candidate` bounds-checks every
    /// position before use, so old entries just fail the match attempt.
    /// Skipping the 128 KB memset is a large win for small inputs.
    pub(super) fn reset(&mut self) {
        // Intentionally not zeroed — see resolve_candidate.
    }

    /// Prepare the table for a new linked block without clearing entries.
    #[cfg(feature = "frame")]
    pub(super) fn prepare_linked_block(&mut self) {
        // Hash tables persist; no action needed since entries are absolute positions.
    }

    /// Subtract `delta` from every absolute position stored in both hash tables.
    #[cfg(feature = "frame")]
    pub(super) fn reposition(&mut self, delta: usize) {
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
    #[inline(always)]
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
    let sequence = crate::block::compress::get_batch(input, pos);
    (sequence.wrapping_mul(2654435761) >> (32 - LZ4MID_HASH_LOG)) as usize
}

/// Read 8 bytes as a little-endian `u64`.
#[inline]
fn read_u64_little_endian(input: &[u8], pos: usize) -> u64 {
    #[cfg(target_pointer_width = "64")]
    {
        (crate::block::compress::get_batch_arch(input, pos) as u64).to_le()
    }
    #[cfg(not(target_pointer_width = "64"))]
    {
        u64::from_le_bytes(input[pos..pos + 8].try_into().unwrap())
    }
}

/// 8-byte hash for lz4mid (hashes the lower 56 bits of a little-endian 8-byte read)
#[inline]
fn get_hash8_mid(input: &[u8], pos: usize) -> usize {
    let sequence = read_u64_little_endian(input, pos);
    let lower_56_bits = sequence << 8;
    ((lower_56_bits.wrapping_mul(58295818150454627)) >> (64 - LZ4MID_HASH_LOG)) as usize
}

struct MatchCandidate<'a> {
    cur: usize,
    source: &'a [u8],
    candidate: usize,
    match_length: usize,
    offset: u16,
}

struct MidMatchFinder<'a> {
    input: &'a [u8],
    ext_dict: &'a [u8],
    table: &'a mut HashTableMid,
    stream_offset: usize,
    ext_dict_stream_offset: usize,
    end_pos_check: usize,
    match_limit: usize,
    input_end: usize,
}

impl<'a> MidMatchFinder<'a> {
    #[inline]
    fn new(
        input: &'a [u8],
        ext_dict: &'a [u8],
        table: &'a mut HashTableMid,
        stream_offset: usize,
        end_pos_check: usize,
        match_limit: usize,
        input_end: usize,
    ) -> Self {
        MidMatchFinder {
            input,
            ext_dict,
            table,
            stream_offset,
            ext_dict_stream_offset: stream_offset - ext_dict.len(),
            end_pos_check,
            match_limit,
            input_end,
        }
    }

    #[inline]
    fn cur_absolute(&self, cur: usize) -> usize {
        cur + self.stream_offset
    }

    /// Resolve an absolute hash table position to a source slice and local index.
    /// Returns `None` if the candidate is out of range or unreachable.
    #[inline]
    fn resolve_candidate(&self, candidate: usize, cur: usize) -> Option<(&'a [u8], usize, usize)> {
        let distance = self.cur_absolute(cur).wrapping_sub(candidate);
        if distance == 0 || distance > MAX_DISTANCE {
            return None;
        }
        if candidate >= self.stream_offset {
            let local_pos = candidate - self.stream_offset;
            if local_pos >= self.input.len() {
                return None;
            }
            return Some((self.input, local_pos, distance));
        }
        if !self.ext_dict.is_empty() && candidate >= self.ext_dict_stream_offset {
            let local_pos = candidate - self.ext_dict_stream_offset;
            if local_pos >= self.ext_dict.len() {
                return None;
            }
            return Some((self.ext_dict, local_pos, distance));
        }
        None
    }

    #[inline(always)]
    fn probe_candidate(
        &self,
        cur: usize,
        source: &'a [u8],
        candidate: usize,
        distance: usize,
    ) -> Option<MatchCandidate<'a>> {
        let mut match_end = cur;
        let match_length = count_same_bytes(
            self.input,
            &mut match_end,
            source,
            candidate,
            self.match_limit,
        );
        if match_length < MINMATCH {
            return None;
        }

        Some(MatchCandidate {
            cur,
            source,
            candidate,
            match_length,
            offset: distance as u16,
        })
    }

    #[inline]
    fn probe_8byte(&mut self, cur: usize) -> Option<MatchCandidate<'a>> {
        let hash = get_hash8_mid(self.input, cur);
        let candidate = self.table.table_8byte[hash] as usize;
        let cur_absolute = self.cur_absolute(cur);
        self.table.table_8byte[hash] = cur_absolute as u32;
        let (source, candidate, distance) = self.resolve_candidate(candidate, cur)?;
        self.probe_candidate(cur, source, candidate, distance)
    }

    #[inline]
    fn probe_4byte(&mut self, cur: usize) -> Option<MatchCandidate<'a>> {
        let hash = get_hash4_mid(self.input, cur);
        let candidate = self.table.table_4byte[hash] as usize;
        let cur_absolute = self.cur_absolute(cur);
        self.table.table_4byte[hash] = cur_absolute as u32;
        let (source, candidate, distance) = self.resolve_candidate(candidate, cur)?;

        // We have this additional check for 4-byte matches to avoid calling `probe_candidate` and
        // counting match length for candidates that don't even match the first 4 bytes. This is a
        // common case for the 4-byte table since it has more collisions, so it's worth doing a
        // quick check here before the more expensive `probe_candidate` logic.
        //
        // The common_bytes logic is not that expensive for the unsafe version.
        #[cfg(feature = "safe-encode")]
        {
            if source.len().saturating_sub(candidate) < MINMATCH {
                return None;
            }

            // [Bounds Check]: Candidate is coming from the hash table. It can't be out of bounds,
            // but impossible to prove for the compiler and remove the bounds checks.
            let candidate_bytes: u32 = crate::block::compress::get_batch(source, candidate);
            // [Bounds Check]: Should be able to be elided due to `end_pos_check`.
            let current_bytes: u32 = crate::block::compress::get_batch(self.input, cur);
            if candidate_bytes != current_bytes {
                return None;
            }
        }

        self.probe_candidate(cur, source, candidate, distance)
    }

    #[inline]
    fn upgrade_with_8byte_lookahead(
        &mut self,
        cur: usize,
        match_candidate: &mut MatchCandidate<'a>,
    ) {
        if cur >= self.end_pos_check {
            return;
        }

        let lookahead_cur = cur + 1;
        let lookahead_hash = get_hash8_mid(self.input, lookahead_cur);
        let lookahead_candidate = self.table.table_8byte[lookahead_hash] as usize;
        let lookahead_match = self
            .resolve_candidate(lookahead_candidate, lookahead_cur)
            .and_then(|(source, candidate, distance)| {
                self.probe_candidate(lookahead_cur, source, candidate, distance)
            });
        let Some(lookahead_match) = lookahead_match else {
            return;
        };

        if lookahead_match.match_length <= match_candidate.match_length {
            return;
        }

        let lookahead_absolute = self.cur_absolute(lookahead_cur);
        self.table.table_8byte[lookahead_hash] = lookahead_absolute as u32;
        *match_candidate = lookahead_match;
    }

    #[inline(always)]
    fn encode_match(
        &mut self,
        output: &mut impl Sink,
        literal_start: usize,
        match_candidate: MatchCandidate<'a>,
    ) -> usize {
        let mut match_start = match_candidate.cur;
        let mut candidate = match_candidate.candidate;
        backtrack_match(
            self.input,
            &mut match_start,
            literal_start,
            match_candidate.source,
            &mut candidate,
        );

        // probe_candidate already counted forward; backtracking only extends backwards.
        let match_end = match_candidate.cur + match_candidate.match_length;

        self.table.insert_match_hashes(
            self.input,
            match_start,
            match_end,
            self.input_end,
            self.stream_offset,
        );
        encode_sequence(
            &self.input[literal_start..match_start],
            output,
            match_candidate.offset,
            match_end - match_start - MINMATCH,
        );
        match_end
    }
}

/// Internal lz4mid compression.
/// `input_pos` is where the current block starts (positions before it are prefix).
/// `ext_dict` and `stream_offset` support linked block mode.
pub(super) fn compress_mid_internal(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    dict: &mut HashTableMid,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    let output_start = output.pos();

    if input.len() - input_pos < LZ4_MIN_LENGTH {
        handle_last_literals(output, &input[input_pos..]);
        return Ok(output.pos() - output_start);
    }

    let mut cur = input_pos;
    let mut literal_start = input_pos;
    let input_end = input.len();
    // Inclusive max main-loop `cur`: at least `MFLIMIT` bytes remain from `cur` to `input_end`.
    let end_pos_check = input_end.saturating_sub(MFLIMIT);
    // Exclusive end for extending matches: last `END_OFFSET` bytes are handled as literals/trailer.
    let match_limit = input_end - END_OFFSET;
    let mut match_finder = MidMatchFinder::new(
        input,
        ext_dict,
        dict,
        stream_offset,
        end_pos_check,
        match_limit,
        input_end,
    );

    while cur <= end_pos_check {
        if let Some(match_candidate) = match_finder.probe_8byte(cur) {
            cur = match_finder.encode_match(output, literal_start, match_candidate);
            literal_start = cur;
            continue;
        }

        // Try 4-byte hash, then look one byte ahead in the 8-byte table for a better match.
        if let Some(mut match_candidate) = match_finder.probe_4byte(cur) {
            match_finder.upgrade_with_8byte_lookahead(cur, &mut match_candidate);
            cur = match_finder.encode_match(output, literal_start, match_candidate);
            literal_start = cur;
            continue;
        }

        // No match - skip with acceleration
        cur += 1 + ((cur - literal_start) >> 9);
    }

    if literal_start < input_end {
        handle_last_literals(output, &input[literal_start..]);
    }

    Ok(output.pos() - output_start)
}
