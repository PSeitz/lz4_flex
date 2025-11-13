//! High compression algorithm implementation.
//!
//! This module implements the LZ4 high compression algorithm using the HashTableHCU32
//! for better compression ratios at the cost of some performance.

use crate::block::{encode_sequence, handle_last_literals, CompressError, LAST_LITERALS, MFLIMIT, MINMATCH};
use crate::sink::Sink;
#[cfg(test)]
use crate::block::decompress;

const HASHTABLE_SIZE_HC: usize = 1 << 15;
const MAX_DISTANCE_HC: usize = 1 << 16;

const MIN_MATCH: usize = 4;
const OPTIMAL_ML: usize = 32;
const ML_MASK: usize = 31;

#[derive(Debug)]
pub struct HashTableHCU32 {
    dict: Box<[u32; HASHTABLE_SIZE_HC]>,
    chain_table: Box<[u16; MAX_DISTANCE_HC]>,
    next_to_update: usize,
    max_attempts: usize,
}

/// Match structure for storing match information
#[derive(Debug, Clone, Copy)]
pub struct Match {
    pub start: usize,
    pub len: usize,
    pub ref_pos: usize,
}

impl Match {
    pub fn new() -> Self {
        Self {
            start: 0,
            len: 0,
            ref_pos: 0,
        }
    }

    pub fn end(&self) -> usize {
        self.start + self.len
    }

    pub fn fix(&mut self, correction: usize) {
        self.start += correction;
        self.len = self.len.saturating_sub(correction);
    }

    pub fn offset(&self) -> u16 {
        (self.start - self.ref_pos) as u16
    }

    pub fn encode_to<S: Sink>(&self, input: &[u8], anchor: usize, output: &mut S) {
        encode_sequence(
            &input[anchor..self.start],
            output,
            self.offset(),
            self.len - MIN_MATCH
        )
    }
}

impl HashTableHCU32 {
    #[inline]
    pub fn new(max_attempts: usize) -> Self {
        let dict = alloc::vec![0u32; HASHTABLE_SIZE_HC]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        let chain_table = alloc::vec![0u16; MAX_DISTANCE_HC]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        Self {
            dict,
            chain_table,
            next_to_update: 0,
            max_attempts,
        }
    }


    /// Get the next position in the chain for a given offset
    #[inline]
    fn next(&self, pos: usize) -> usize {
        const MASK: usize = MAX_DISTANCE_HC - 1;
        pos - (self.chain_table[pos & MASK] as usize)
    }

    #[inline]
    fn add_hash(&mut self, hash: usize, pos: usize) {
        let delta = pos - self.dict[hash] as usize;
        const MASK : usize = MAX_DISTANCE_HC - 1;
        let delta = if delta >= MAX_DISTANCE_HC {
            MASK
        } else {
            delta
        };
        self.chain_table[pos & MASK] = delta as u16;
        self.dict[hash] = pos as u32;
    }

    /// Hash function for high compression
    #[inline]
    fn hash_hc(v: u32) -> u32 {
        v.wrapping_mul(2654435761u32) >> 17
    }

    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        Self::hash_hc(super::compress::get_batch(input, pos)) as usize
    }

    /// Insert hashes for all positions up to the given offset
    pub fn insert(&mut self, off: usize, input: &[u8]) {
        for pos in self.next_to_update..off {
            self.add_hash(Self::get_hash_at(input, pos), pos);
        }
        self.next_to_update = off;
    }

    fn insert_and_find_best_match(&mut self, input: &[u8], off: usize, match_limit: usize, match_info: &mut Match) -> bool {
        match_info.start = off;
        match_info.len = 0;
        let mut delta = 0;
        let mut repl = 0;

        self.insert(off, input);

        let mut ref_pos = self.dict[Self::get_hash_at(input, off)] as usize;

        // Search for better matches
        for i in 0..=self.max_attempts {
            if self.read_min_match_equals(input, ref_pos, off) {
                let match_len = MIN_MATCH + self.common_bytes(input, ref_pos + MIN_MATCH, off + MIN_MATCH, match_limit);
                if match_len > match_info.len {
                    match_info.ref_pos = ref_pos;
                    match_info.len = match_len;
                }
                // record to deal with possible overlap
                if i == 0 {
                    repl = match_len;
                    delta = off - ref_pos;
                }
            }
            let next = self.next(ref_pos);
            if next >= off + MAX_DISTANCE_HC || next == ref_pos {
                break;
            }
            ref_pos = next;
        }

        // Handle pre hash
        if repl != 0 {
            let mut ptr = off;
            let end = off + repl - 3; // MIN_MATCH - 1 = 3
            const MASK: usize = MAX_DISTANCE_HC - 1;

            // possible overlap from off -> ref
            while ptr < end - delta {
                self.chain_table[ptr & MASK] = delta as u16; // pre load
                ptr += 1;
            }

            loop {
                self.chain_table[ptr & MASK] = delta as u16;
                self.dict[Self::get_hash_at(input, ptr)] = ptr as u32;
                ptr += 1;
                if ptr >= end {
                    break;
                }
            }
            self.next_to_update = end;
        }

        match_info.len != 0
    }

    /// Insert hashes and find a wider match, similar to Java insertAndFindWiderMatch
    pub fn insert_and_find_wider_match(&mut self, input: &[u8], off: usize, start_limit: usize, match_limit: usize, min_len: usize, match_info: &mut Match) -> bool {
        match_info.len = min_len;

        self.insert(off, input);

        let mut ref_pos = self.dict[Self::get_hash_at(input, off)] as usize;

        for _ in 0..self.max_attempts { // max_attempts equivalent
            if self.read_min_match_equals(input, ref_pos, off) {
                let match_len_forward = MIN_MATCH + self.common_bytes(input, ref_pos + MIN_MATCH, off + MIN_MATCH, match_limit);
                let match_len_backward = Self::common_bytes_backward(input, ref_pos, off, 0, start_limit);
                let match_len = match_len_backward + match_len_forward;

                if match_len > match_info.len {
                    match_info.len = match_len;
                    match_info.ref_pos = ref_pos - match_len_backward;
                    match_info.start = off - match_len_backward;
                }
            }
            let next = self.next(ref_pos);
            if next >= off + MAX_DISTANCE_HC || next == ref_pos {
                break;
            }
            ref_pos = next;
        }

        match_info.len > min_len
    }

    /// Check if two 4-byte sequences starting at the given positions are equal
    #[inline]
    fn read_min_match_equals(&self, input: &[u8], pos1: usize, pos2: usize) -> bool {
        input[pos1..pos1 + MIN_MATCH] == input[pos2..pos2 + MIN_MATCH]
    }

    /// Find the number of common bytes between two positions
    #[inline]
    fn common_bytes(&self, input: &[u8], mut pos1: usize, mut pos2: usize, limit: usize) -> usize {
        let mut len = 0;
        let limit = input.len().min(limit);
        loop {
            if pos2 >= limit || input[pos1] != input[pos2] {
                break len;
            }
            pos1 += 1;
            pos2 += 1;
            len += 1;
        }
    }

    /// Find the number of common bytes backward from two positions
    #[inline]
    fn common_bytes_backward(input: &[u8], mut pos1: usize, mut pos2: usize, limit1: usize, limit2: usize) -> usize {
        let mut len = 0;

        while pos1 > limit1 && pos2 > limit2 {
            pos1 -= 1;
            pos2 -= 1;
            if input[pos1] == input[pos2] {
                len += 1;
            } else {
                break;
            }
        }

        len
    }
}

/// Compress input data using the high compression algorithm.
///
/// This function implements the same algorithm as the Java LZ4HC implementation,
/// providing better compression ratios than the standard LZ4 algorithm.
///
/// # Arguments
/// * `input` - The input data to compress
/// * `output` - The output buffer to write compressed data to
///
/// # Returns
/// * `Ok(usize)` - The number of bytes written to output
/// * `Err(CompressError)` - If the output buffer is too small
pub fn compress_hc(input: &[u8], output: &mut impl Sink, level: u8) -> Result<usize, CompressError> {
    let output_start_pos = output.pos();
    if input.len() < MFLIMIT + 1 {
        // Input too small to compress
        handle_last_literals(output, input);
        return Ok(output.pos() - output_start_pos);
    }

    let src_end = input.len();
    let mf_limit = src_end - MFLIMIT;
    let match_limit = src_end - LAST_LITERALS;

    let mut s_off = 1;
    // let mut d_off = output.pos();
    let mut anchor = 0;

    let mut ht = HashTableHCU32::new(1 << (level - 1));
    let mut match0;
    let mut match1 = Match::new();
    let mut match2 = Match::new();
    let mut match3 = Match::new();

    'main: while s_off < mf_limit {
        if !ht.insert_and_find_best_match(input, s_off, match_limit, &mut match1) {
            s_off += 1;
            continue;
        }

        // Saved, in case we would skip too much
        match0 = match1;

        'search2: loop {
            debug_assert!(match1.start >= anchor);
            if match1.end() >= mf_limit
                || !ht.insert_and_find_wider_match(
                    input,
                    match1.end() - 2,
                    match1.start + 1,
                    match_limit,
                    match1.len,
                    &mut match2,
                )
            {
                // No better match
                match1.encode_to(&input, anchor, output);
                s_off = match1.end();
                anchor = s_off;
                continue 'main;
            }

            if match0.start < match1.start {
                if match2.start < match1.start + match0.len {
                    // Empirical optimization
                    match1 = match0;
                }
            }
            debug_assert!(match2.start > match1.start);

            if match2.start - match1.start < 3 {
                // First match too small: removed
                match1 = match2;
                continue 'search2;
            }

            'search3: loop {
                if match2.start - match1.start < OPTIMAL_ML {
                    let mut new_match_len = match1.len;
                    if new_match_len > OPTIMAL_ML {
                        new_match_len = OPTIMAL_ML;
                    }
                    if match1.start + new_match_len > match2.end() - MINMATCH {
                        new_match_len = match2.start - match1.start + match2.len - MINMATCH;
                    }
                    let correction = new_match_len - (match2.start - match1.start);
                    if correction > 0 {
                        match2.fix(correction);
                    }
                }

                if match2.start + match2.len >= mf_limit
                    || !ht.insert_and_find_wider_match(
                        input,
                        match2.end() - 3,
                        match2.start,
                        match_limit,
                        match2.len,
                        &mut match3,
                    )
                {
                    // No better match -> 2 sequences to encode
                    if match2.start < match1.end() {
                        match1.len = match2.start - match1.start;
                    }
                    // Encode seq 1
                    match1.encode_to(input, anchor, output);
                    anchor = s_off;
                    s_off = match1.end();
                    // Encode seq 2
                    match2.encode_to(input, anchor, output);
                    anchor = s_off;
                    s_off = match2.end();
                    continue 'main;
                }

                if match3.start < match1.end() + 3 {
                    // Not enough space for match 2: remove it
                    if match3.start >= match1.end() {
                        // Can write Seq1 immediately ==> Seq2 is removed, so Seq3 becomes Seq1
                        if match2.start < match1.end() {
                            let correction = match1.end() - match2.start;
                            match2.fix(correction);
                            if match2.len < MINMATCH {
                                match2 = match3;
                            }
                        }

                        match1.encode_to(
                            input,
                            anchor,
                            output,
                        );
                        anchor = s_off;
                        s_off = match1.end();

                        match1 = match3;
                        match0 = match2;

                        continue 'search2;
                    }

                    match2 = match3;
                    continue 'search3;
                }

                // OK, now we have 3 ascending matches; let's write at least the first one
                if match2.start < match1.end() {
                    if match2.start - match1.start < ML_MASK {
                        if match1.len > OPTIMAL_ML {
                            match1.len = OPTIMAL_ML;
                        }
                        if match1.end() > match2.end() - MINMATCH {
                            match1.len = match2.end() - match1.start - MINMATCH;
                        }
                        let correction = match1.end() - match2.start;
                        match2.fix(correction);
                    } else {
                        match1.len = match2.start - match1.start;
                    }
                }

                match1.encode_to(
                    input, anchor,
                    output,
                );
                anchor = s_off;
                s_off = match1.end();

                match1 = match2;
                match2 = match3;

                continue 'search3;
            }
        }
    }

    // Handle remaining literals
    handle_last_literals(output, &input[anchor.. src_end]);
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
}
