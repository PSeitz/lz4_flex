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
    pub(super) fn reset(&mut self) {
        self.table_4byte.fill(0);
        self.table_8byte.fill(0);
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
    let batch = crate::block::compress::get_batch(input, pos);
    (batch.wrapping_mul(2654435761) >> (32 - LZ4MID_HASH_LOG)) as usize
}

/// 8-byte hash for lz4mid (hashes lower 56 bits for longer match detection)
#[inline]
fn get_hash8_mid(input: &[u8], pos: usize) -> usize {
    // Use get_batch_arch for the raw read (eliminates bounds check in unsafe mode),
    // then convert to u64 for the 56-bit hash computation.
    #[cfg(target_pointer_width = "64")]
    {
        let batch = crate::block::compress::get_batch_arch(input, pos) as u64;
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
pub(super) fn compress_mid_internal(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    table: &mut HashTableMid,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    let output_start = output.pos();

    if input.len() - input_pos < LZ4_MIN_LENGTH {
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
