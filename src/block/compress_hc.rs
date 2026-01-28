//! High compression algorithm implementation.
//!
//! This module implements the LZ4 high compression algorithm using the HashTableHCU32
//! for better compression ratios at the cost of some performance.
//!
//! It includes two compression strategies:
//! - `compress_hc`: The standard high compression algorithm (levels 3-9)
//! - `compress_opt`: The optimal parsing algorithm for maximum compression (levels 10-12)

use crate::block::{encode_sequence, handle_last_literals, CompressError, LAST_LITERALS, MFLIMIT, MINMATCH, MAX_DISTANCE};
use crate::block::compress::{backtrack_match, count_same_bytes};
use crate::sink::Sink;
use crate::sink::SliceSink;
#[cfg(test)]
use crate::block::decompress;
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

#[derive(Debug)]
pub struct HashTableHCU32 {
    dict: Box<[u32; HASHTABLE_SIZE_HC]>,  // Fixed size, hash function bounds to this range
    chain_table: Box<[u16]>,              // Dynamic size, power of 2 for masking
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
        self.ref_pos += correction;
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
    pub fn new(max_attempts: usize, input_len: usize) -> Self {
        // Dict table: fixed size, hash function already bounds to this range
        let dict = vec![0u32; HASHTABLE_SIZE_HC]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        
        // Chain table: dynamically sized based on input length
        // min(input_len, MAX_DISTANCE_HC), at least 256, must be power of 2
        let chain_size = input_len
            .min(MAX_DISTANCE_HC)
            .max(256)
            .next_power_of_two();
        
        Self {
            dict,
            chain_table: vec![0u16; chain_size].into_boxed_slice(),
            next_to_update: 0,
            max_attempts,
        }
    }
    
    /// Mask for chain table indexing (table size is always power of 2)
    #[inline]
    fn chain_mask(&self) -> usize {
        self.chain_table.len() - 1
    }


    /// Get the next position in the chain for a given offset
    #[inline]
    fn next(&self, pos: usize) -> usize {
        pos - (self.chain_table[pos & self.chain_mask()] as usize)
    }

    #[inline]
    fn add_hash(&mut self, hash: usize, pos: usize) {
        let delta = pos - self.dict[hash] as usize;
        let delta = if delta > self.chain_mask() {
            self.chain_mask()
        } else {
            delta
        };
        self.chain_table[pos & self.chain_mask()] = delta as u16;
        self.dict[hash] = pos as u32;
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

        let mut ref_pos = self.dict[Self::get_hash_at(input, off) ] as usize;

        // Search for better matches
        for i in 0..=self.max_attempts {
            // Validate ref_pos is within valid range and LZ4 format limit
            if ref_pos >= off || off - ref_pos > self.chain_mask() {
                break;
            }
            
            // Early termination: if we already have a match, check if the last 2 bytes match first
            // This avoids expensive full comparisons for candidates that can't be longer (like C's LZ4_read16 check)
            if match_info.len >= MIN_MATCH {
                let check_pos = match_info.len - 1;
                // Check 2 bytes at position (len - 1), like C does with LZ4_read16
                if ref_pos + check_pos + 1 < input.len() && off + check_pos + 1 < input.len() {
                    if input[ref_pos + check_pos] != input[off + check_pos]
                        || input[ref_pos + check_pos + 1] != input[off + check_pos + 1]
                    {
                        let next = self.next(ref_pos);
                        if next > off || off - next > self.chain_mask() || next == ref_pos {
                            break;
                        }
                        ref_pos = next;
                        continue;
                    }
                }
            }
            
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
            if next > off || off - next > self.chain_mask() || next == ref_pos {
                break;
            }
            ref_pos = next;
        }

        // Handle pre hash
        if repl != 0 {
            let mut ptr = off;
            let end = off + repl - 3; // MIN_MATCH - 1 = 3
            let mask = self.chain_mask();

            // possible overlap from off -> ref
            while ptr < end - delta {
                self.chain_table[ptr & mask] = delta as u16; // pre load
                ptr += 1;
            }

            loop {
                self.chain_table[ptr & mask] = delta as u16;
                self.dict[Self::get_hash_at(input, ptr) ] = ptr as u32;
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
        
        // lookBackLength = how much we can extend backward from search position
        let look_back_length = off - start_limit;

        self.insert(off, input);

        let mut ref_pos = self.dict[Self::get_hash_at(input, off) ] as usize;

        for _ in 0..self.max_attempts {
            // Validate ref_pos is within valid range and LZ4 format limit
            if ref_pos >= off || off - ref_pos > self.chain_mask() {
                break;
            }
            
            // Early termination: check if last 2 bytes of current best match also match
            // C uses: LZ4_read16(iLowLimit + longest - 1) == LZ4_read16(matchPtr - lookBackLength + longest - 1)
            // iLowLimit = start_limit, matchPtr = ref_pos, so we check:
            // - source: start_limit + longest - 1
            // - match: ref_pos - look_back_length + longest - 1
            if match_info.len >= MIN_MATCH && ref_pos >= look_back_length {
                let src_check = start_limit + match_info.len - 1;
                let match_check = ref_pos - look_back_length + match_info.len - 1;
                if src_check + 1 < input.len() && match_check + 1 < input.len() {
                    if input[src_check] != input[match_check]
                        || input[src_check + 1] != input[match_check + 1]
                    {
                        let next = self.next(ref_pos);
                        if next > off || off - next > self.chain_mask() || next == ref_pos {
                            break;
                        }
                        ref_pos = next;
                        continue;
                    }
                }
            }
            
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
            if next > off || off - next > self.chain_mask() || next == ref_pos {
                break;
            }
            ref_pos = next;
        }

        match_info.len > min_len
    }

    /// Check if two 4-byte sequences starting at the given positions are equal
    #[inline]
    fn read_min_match_equals(&self, input: &[u8], pos1: usize, pos2: usize) -> bool {
        // Fast u32 comparison instead of slice comparison
        super::compress::get_batch(input, pos1) == super::compress::get_batch(input, pos2)
    }

    /// Find the number of common bytes between two positions (optimized version)
    /// Uses usize-sized (8 bytes on 64-bit) batch comparison with XOR for speed
    #[inline]
    fn common_bytes(&self, input: &[u8], pos1: usize, pos2: usize, limit: usize) -> usize {
        let limit = input.len().min(limit);
        let max_len = limit.saturating_sub(pos2);
        
        if max_len == 0 {
            return 0;
        }
        
        let mut len = 0;
        
        // Process usize (8 bytes on 64-bit) at a time
        const STEP_SIZE: usize = core::mem::size_of::<usize>();
        while len + STEP_SIZE <= max_len {
            let v1 = super::compress::get_batch_arch(input, pos1 + len);
            let v2 = super::compress::get_batch_arch(input, pos2 + len);
            let diff = v1 ^ v2;
            
            if diff == 0 {
                len += STEP_SIZE;
            } else {
                // Find first differing byte
                return len + (diff.to_le().trailing_zeros() / 8) as usize;
            }
        }
        
        // Process remaining 4 bytes if on 64-bit
        #[cfg(target_pointer_width = "64")]
        if len + 4 <= max_len {
            let v1 = super::compress::get_batch(input, pos1 + len);
            let v2 = super::compress::get_batch(input, pos2 + len);
            let diff = v1 ^ v2;
            
            if diff == 0 {
                len += 4;
            } else {
                return len + (diff.to_le().trailing_zeros() / 8) as usize;
            }
        }
        
        // Process remaining bytes one at a time
        while len < max_len {
            if input[pos1 + len] != input[pos2 + len] {
                break;
            }
            len += 1;
        }
        
        len
    }

    /// Find the number of common bytes backward from two positions (optimized)
    #[inline]
    fn common_bytes_backward(input: &[u8], mut pos1: usize, mut pos2: usize, limit1: usize, limit2: usize) -> usize {
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
        
        // Process remaining bytes one at a time
        while len < max_back {
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

    /// Find a longer match at the given position for optimal parsing
    /// Returns (match_length, offset) where match_length is 0 if no match found
    pub fn find_longer_match(&mut self, input: &[u8], off: usize, match_limit: usize, min_len: usize) -> (usize, usize) {
        self.insert(off, input);

        let mut best_len = min_len;
        let mut best_offset = 0;

        let mut ref_pos = self.dict[Self::get_hash_at(input, off) ] as usize;

        for _ in 0..self.max_attempts {
            if ref_pos >= off || off - ref_pos > self.chain_mask() {
                break;
            }

            if self.read_min_match_equals(input, ref_pos, off) {
                let match_len = MIN_MATCH + self.common_bytes(input, ref_pos + MIN_MATCH, off + MIN_MATCH, match_limit);
                if match_len > best_len {
                    best_len = match_len;
                    best_offset = off - ref_pos;
                }
            }

            let next = self.next(ref_pos);
            if next > off || off - next > self.chain_mask() || next == ref_pos {
                break;
            }
            ref_pos = next;
        }

        if best_len > min_len {
            (best_len, best_offset)
        } else {
            (0, 0)
        }
    }
}

/// Optimal parsing state for a single position
#[derive(Debug, Clone, Copy)]
struct OptimalState {
    /// Cost in bytes to reach this position
    price: i32,
    /// Match offset (0 for literal)
    off: usize,
    /// Match length (1 for literal)
    mlen: usize,
    /// Literal length before this position
    litlen: usize,
}

impl OptimalState {
    fn new() -> Self {
        Self {
            price: i32::MAX,
            off: 0,
            mlen: 1,
            litlen: 0,
        }
    }
}

/// Calculate the cost in bytes of encoding literals
#[inline]
fn literals_price(litlen: usize) -> i32 {
    let mut price = litlen as i32;
    if litlen >= RUN_MASK {
        price += 1 + ((litlen - RUN_MASK) / 255) as i32;
    }
    price
}

/// Calculate the cost in bytes of encoding a sequence (literals + match)
#[inline]
fn sequence_price(litlen: usize, mlen: usize) -> i32 {
    // token + 16-bit offset
    let mut price: i32 = 1 + 2;

    // literal length encoding
    price += literals_price(litlen);

    // match length encoding (mlen >= MINMATCH)
    let ml_code = mlen - MIN_MATCH;
    if ml_code >= 15 {
        price += 1 + ((ml_code - 15) / 255) as i32;
    }

    price
}

/// Compress input data using the LZ4 high compression algorithm.
///
/// This function provides better compression ratios than the standard LZ4 algorithm
/// at the cost of compression speed. It automatically selects the best compression
/// strategy based on the compression level:
///
/// - **Levels 1-9**: Uses the standard HC (Hash Chain) algorithm with increasing
///   search depth for better compression.
/// - **Levels 10-12**: Uses the optimal parsing algorithm which employs dynamic
///   programming to find the best sequence of matches and literals for maximum
///   compression. This is significantly slower but produces smaller output.
///
/// # Arguments
/// * `input` - The input data to compress
/// * `output` - The output buffer to write compressed data to
/// * `level` - Compression level (1-12), higher means better compression but slower
///
/// # Returns
/// * `Ok(usize)` - The number of bytes written to output
/// * `Err(CompressError)` - If the output buffer is too small
///
/// # Example
/// ```ignore
/// use lz4_flex::block::compress_hc;
/// let input = b"Hello, this is some data to compress!";
/// let mut output = vec![0u8; input.len() * 2];
/// let size = compress_hc(input, &mut output, 9).unwrap(); // HC algorithm
/// let size = compress_hc(input, &mut output, 12).unwrap(); // Optimal algorithm
/// ```
pub fn compress_hc(input: &[u8], output: &mut impl Sink, level: u8) -> Result<usize, CompressError> {
    // Clamp level to valid range (0-12, matching C LZ4HC)
    let level = level.min(12);

    // Route to appropriate algorithm based on level (matching C LZ4HC k_clTable)
    // Levels 0-2: lz4mid (intermediate - two hash tables, better than fast)
    // Levels 3-9: HC (hash chain algorithm)
    // Levels 10-12: optimal parsing (best compression)
    if level >= 10 {
        compress_opt_internal(input, output, level)
    } else if level >= 3 {
        compress_hc_internal(input, output, level)
    } else {
        compress_mid_internal(input, output)
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
    let mut output = vec![0u8; max_size];
    let mut sink = SliceSink::new(&mut output, 0);
    let compressed_size = compress_hc(input, &mut sink, level).unwrap();
    output.truncate(compressed_size);
    output
}

// ============================================================================
// LZ4MID - Intermediate compression (levels 1-2)
// Uses two hash tables (4-byte and 8-byte) for better compression than fast
// algorithm while being faster than HC.
// ============================================================================

/// Hash table for lz4mid algorithm - contains two tables (4-byte and 8-byte)
struct HashTableMid {
    hash4: Box<[u32; LZ4MID_HASHTABLE_SIZE]>,
    hash8: Box<[u32; LZ4MID_HASHTABLE_SIZE]>,
}

impl HashTableMid {
    fn new() -> Self {
        HashTableMid {
            hash4: vec![0u32; LZ4MID_HASHTABLE_SIZE].into_boxed_slice().try_into().unwrap(),
            hash8: vec![0u32; LZ4MID_HASHTABLE_SIZE].into_boxed_slice().try_into().unwrap(),
        }
    }
}

/// 4-byte hash for lz4mid (same multiplier as fast algorithm)
#[inline]
fn get_hash4_mid(input: &[u8], pos: usize) -> usize {
    let v = u32::from_ne_bytes(input[pos..pos + 4].try_into().unwrap());
    (v.wrapping_mul(2654435761) >> (32 - LZ4MID_HASH_LOG)) as usize
}

/// 8-byte hash for lz4mid (hashes lower 56 bits for longer match detection)
#[inline]
fn get_hash8_mid(input: &[u8], pos: usize) -> usize {
    let v = u64::from_le_bytes(input[pos..pos + 8].try_into().unwrap());
    let v56 = v << 8;
    ((v56.wrapping_mul(58295818150454627)) >> (64 - LZ4MID_HASH_LOG)) as usize
}

/// Internal lz4mid compression
fn compress_mid_internal(input: &[u8], output: &mut impl Sink) -> Result<usize, CompressError> {
    let output_start = output.pos();
    
    if input.len() < MFLIMIT + 1 {
        handle_last_literals(output, input);
        return Ok(output.pos() - output_start);
    }

    let mut table = HashTableMid::new();
    let hash4 = &mut *table.hash4;
    let hash8 = &mut *table.hash8;

    let mut ip = 0usize;
    let mut anchor = 0usize;
    let input_end = input.len();
    let mflimit = input_end.saturating_sub(MFLIMIT);
    let ilimit = input_end.saturating_sub(8); // Need 8 bytes to read for hash8

    // Helper to add position to hash8 table
    #[inline(always)]
    fn add_hash8(hash8: &mut [u32; LZ4MID_HASHTABLE_SIZE], input: &[u8], pos: usize, input_end: usize) {
        if pos + 8 <= input_end {
            hash8[get_hash8_mid(input, pos)] = pos as u32;
        }
    }

    // Helper to add position to hash4 table
    #[inline(always)]
    fn add_hash4(hash4: &mut [u32; LZ4MID_HASHTABLE_SIZE], input: &[u8], pos: usize, input_end: usize) {
        if pos + 4 <= input_end {
            hash4[get_hash4_mid(input, pos)] = pos as u32;
        }
    }

    while ip <= mflimit {
        // Try 8-byte hash first (longer matches)
        let h8 = get_hash8_mid(input, ip);
        let pos8 = hash8[h8] as usize;
        hash8[h8] = ip as u32;

        if ip > pos8 && ip - pos8 <= MAX_DISTANCE {
            let mut probe = ip;
            let match_len = count_same_bytes(input, &mut probe, input, pos8);
            if match_len >= MIN_MATCH {
                let mut cur = ip;
                let mut candidate = pos8;
                backtrack_match(input, &mut cur, anchor, input, &mut candidate);
                let match_len = count_same_bytes(input, &mut cur, input, candidate);
                let match_start = cur - match_len;
                let offset = (match_start - candidate) as u16;

                // Fill hash tables at beginning of match (like C's lz4mid)
                add_hash8(hash8, input, match_start + 1, input_end);
                add_hash8(hash8, input, match_start + 2, input_end);
                add_hash4(hash4, input, match_start + 1, input_end);

                encode_sequence(&input[anchor..match_start], output, offset, match_len - MIN_MATCH);

                ip = cur;
                anchor = ip;

                // Fill hash tables at end of match (like C's lz4mid)
                if ip >= 5 && ip <= ilimit {
                    add_hash8(hash8, input, ip - 5, input_end);
                }
                if ip >= 3 && ip <= ilimit {
                    add_hash8(hash8, input, ip - 3, input_end);
                    add_hash8(hash8, input, ip - 2, input_end);
                }
                if ip >= 2 {
                    add_hash4(hash4, input, ip - 2, input_end);
                }
                if ip >= 1 {
                    add_hash4(hash4, input, ip - 1, input_end);
                }
                continue;
            }
        }

        // Try 4-byte hash (shorter matches)
        let h4 = get_hash4_mid(input, ip);
        let pos4 = hash4[h4] as usize;
        hash4[h4] = ip as u32;

        if ip > pos4 && ip - pos4 <= MAX_DISTANCE {
            let mut probe = ip;
            let match_len = count_same_bytes(input, &mut probe, input, pos4);
            if match_len >= MIN_MATCH {
                // Check ip+1 for potentially longer match
                let mut best_ip = ip;
                let mut best_pos = pos4;
                let mut best_len = match_len;

                if ip + 1 <= mflimit {
                    let h8_next = get_hash8_mid(input, ip + 1);
                    let pos8_next = hash8[h8_next] as usize;
                    if ip + 1 > pos8_next && ip + 1 - pos8_next <= MAX_DISTANCE {
                        let mut probe_next = ip + 1;
                        let len_next = count_same_bytes(input, &mut probe_next, input, pos8_next);
                        if len_next > best_len {
                            hash8[h8_next] = (ip + 1) as u32;
                            best_ip = ip + 1;
                            best_pos = pos8_next;
                            best_len = len_next;
                        }
                    }
                }
                let _ = best_len;

                let mut cur = best_ip;
                let mut candidate = best_pos;
                backtrack_match(input, &mut cur, anchor, input, &mut candidate);
                let match_len = count_same_bytes(input, &mut cur, input, candidate);
                let match_start = cur - match_len;
                let offset = (match_start - candidate) as u16;

                // Fill hash tables at beginning of match (like C's lz4mid)
                add_hash8(hash8, input, match_start + 1, input_end);
                add_hash8(hash8, input, match_start + 2, input_end);
                add_hash4(hash4, input, match_start + 1, input_end);

                encode_sequence(&input[anchor..match_start], output, offset, match_len - MIN_MATCH);

                ip = cur;
                anchor = ip;

                // Fill hash tables at end of match (like C's lz4mid)
                if ip >= 5 && ip <= ilimit {
                    add_hash8(hash8, input, ip - 5, input_end);
                }
                if ip >= 3 && ip <= ilimit {
                    add_hash8(hash8, input, ip - 3, input_end);
                    add_hash8(hash8, input, ip - 2, input_end);
                }
                if ip >= 2 {
                    add_hash4(hash4, input, ip - 2, input_end);
                }
                if ip >= 1 {
                    add_hash4(hash4, input, ip - 1, input_end);
                }
                continue;
            }
        }

        // No match - skip with acceleration
        ip += 1 + ((ip - anchor) >> 9);
    }

    // Handle remaining literals
    if anchor < input_end {
        handle_last_literals(output, &input[anchor..]);
    }

    Ok(output.pos() - output_start)
}

/// Internal HC compression implementation using hash chain algorithm
fn compress_hc_internal(input: &[u8], output: &mut impl Sink, level: u8) -> Result<usize, CompressError> {
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

    let mut ht = HashTableHCU32::new(1 << (level - 1), input.len());
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
            if match1.end() > mf_limit
                || !ht.insert_and_find_wider_match(
                    input,
                    match1.end() - 2,
                    match1.start,
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
            debug_assert!(match2.start >= match1.start);

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
                    if match1.start + new_match_len > match2.end().saturating_sub(MINMATCH) {
                        new_match_len = (match2.start - match1.start) + match2.len.saturating_sub(MINMATCH);
                    }
                    let correction = new_match_len.saturating_sub(match2.start - match1.start);
                    if correction > 0 {
                        match2.fix(correction);
                    }
                }

                if match2.start + match2.len > mf_limit  // C uses <=, so we use >
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
                    s_off = match1.end();
                    anchor = s_off;
                    // Encode seq 2
                    match2.encode_to(input, anchor, output);
                    s_off = match2.end();
                    anchor = s_off;
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
                        s_off = match1.end();
                        anchor = s_off;

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
                s_off = match1.end();
                anchor = s_off;

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

/// Internal optimal parsing compression implementation
fn compress_opt_internal(input: &[u8], output: &mut impl Sink, level: u8) -> Result<usize, CompressError> {
    let output_start_pos = output.pos();

    if input.len() < MFLIMIT + 1 {
        // Input too small to compress
        handle_last_literals(output, input);
        return Ok(output.pos() - output_start_pos);
    }

    let src_end = input.len();
    let mf_limit = src_end - MFLIMIT;
    let match_limit = src_end - LAST_LITERALS;

    // Determine search parameters based on level
    let (nb_searches, sufficient_len) = match level {
        10 => (96, 64),
        11 => (512, 128),
        _ => (16384, LZ4_OPT_NUM), // level 12+
    };

    let full_update = level >= 12;

    let mut anchor = 0;
    let mut ip = 0;

    let mut ht = HashTableHCU32::new(nb_searches, input.len());

    // Allocate optimal parsing buffer
    let mut opt = vec![OptimalState::new(); LZ4_OPT_NUM + TRAILING_LITERALS];

    // Main loop
    'main_loop: while ip <= mf_limit {
        let llen = ip - anchor;

        // Find first match
        let (first_len, first_off) = ht.find_longer_match(input, ip, match_limit, MIN_MATCH - 1);
        if first_len == 0 {
            ip += 1;
            continue;
        }

        // If match is good enough, encode immediately
        if first_len >= sufficient_len {
            encode_sequence(
                &input[anchor..ip],
                output,
                first_off as u16,
                first_len - MIN_MATCH,
            );
            ip += first_len;
            anchor = ip;
            continue;
        }

        // Initialize optimal parsing state for literals
        for rpos in 0..MIN_MATCH {
            let cost = literals_price(llen + rpos);
            opt[rpos].mlen = 1;
            opt[rpos].off = 0;
            opt[rpos].litlen = llen + rpos;
            opt[rpos].price = cost;
        }

        // Set prices using initial match
        let match_ml = first_len.min(LZ4_OPT_NUM - 1);
        for mlen in MIN_MATCH..=match_ml {
            let cost = sequence_price(llen, mlen);
            opt[mlen].mlen = mlen;
            opt[mlen].off = first_off;
            opt[mlen].litlen = llen;
            opt[mlen].price = cost;
        }

        let mut last_match_pos = first_len;

        // Add trailing literals after the match
        for add_lit in 1..=TRAILING_LITERALS {
            let pos = last_match_pos + add_lit;
            if pos < opt.len() {
                opt[pos].mlen = 1; // literal
                opt[pos].off = 0;
                opt[pos].litlen = add_lit;
                opt[pos].price = opt[last_match_pos].price + literals_price(add_lit);
            }
        }

        // Check further positions
        let mut cur = 1;
        while cur < last_match_pos {
            let cur_ptr = ip + cur;

            if cur_ptr > mf_limit {
                break;
            }

            if full_update {
                // Not useful to search here if next position has same (or lower) cost
                if opt[cur + 1].price <= opt[cur].price
                    && cur + MIN_MATCH < opt.len()
                    && opt[cur + MIN_MATCH].price < opt[cur].price.saturating_add(3)
                {
                    cur += 1;
                    continue;
                }
            } else {
                // Not useful to search here if next position has same (or lower) cost
                if opt[cur + 1].price <= opt[cur].price {
                    cur += 1;
                    continue;
                }
            }

            // Find longer match at current position
            let min_len_search = if full_update {
                MIN_MATCH - 1
            } else {
                last_match_pos.saturating_sub(cur)
            };

            let (new_len, new_off) = ht.find_longer_match(input, cur_ptr, match_limit, min_len_search);
            if new_len == 0 {
                cur += 1;
                continue;
            }

            // If match is good enough or extends beyond buffer, encode immediately
            if new_len >= sufficient_len || new_len + cur >= LZ4_OPT_NUM {
                // In C code: best_mlen = newMatch.len; best_off = newMatch.off; 
                // last_match_pos = cur + 1; goto encode;
                // At encode: candidate_pos = cur; selected = best;
                
                // DON'T store at opt[cur] before traversal - C code doesn't do this!
                // The reverse traversal will write the immediate match values to opt[cur]
                // while reading the OLD forward-pass values.
                
                // Set last_match_pos = cur + 1 as in C code
                last_match_pos = cur + 1;
                
                // Reverse traversal starting from cur
                // selected values start with the immediate match (new_len, new_off)
                let mut selected_mlen = new_len;
                let mut selected_off = new_off;
                let mut candidate_pos = cur;
                loop {
                    // Read OLD values from forward pass
                    let next_mlen = opt[candidate_pos].mlen;
                    let next_off = opt[candidate_pos].off;
                    // Write the selected values (from immediate match or previous iteration)
                    opt[candidate_pos].mlen = selected_mlen;
                    opt[candidate_pos].off = selected_off;
                    // Move selected to the old values for next iteration
                    selected_mlen = next_mlen;
                    selected_off = next_off;
                    if next_mlen > candidate_pos {
                        break;
                    }
                    candidate_pos -= next_mlen;
                }
                
                // Skip the normal reverse traversal section below
                // by jumping directly to encoding
                // (We've already done reverse traversal inline above)
                
                // Encode all recorded sequences in order
                let mut rpos = 0;
                while rpos < last_match_pos {
                    let ml = opt[rpos].mlen;
                    let offset = opt[rpos].off;

                    if ml == 1 {
                        ip += 1;
                        rpos += 1;
                        continue;
                    }

                    encode_sequence(
                        &input[anchor..ip],
                        output,
                        offset as u16,
                        ml - MIN_MATCH,
                    );

                    ip += ml;
                    anchor = ip;
                    rpos += ml;
                }

                // Reset opt array
                for state in opt.iter_mut().take(last_match_pos + TRAILING_LITERALS + 1) {
                    *state = OptimalState::new();
                }
                
                continue 'main_loop; // Continue OUTER main loop, skip the normal encoding path
            }

            // Update prices for literals before the match
            let base_litlen = opt[cur].litlen;
            if opt[cur].price != i32::MAX {
                for litlen in 1..MIN_MATCH {
                    let pos = cur + litlen;
                    if pos < opt.len() {
                        let price = opt[cur].price - literals_price(base_litlen) + literals_price(base_litlen + litlen);
                        if price < opt[pos].price {
                            opt[pos].mlen = 1; // literal
                            opt[pos].off = 0;
                            opt[pos].litlen = base_litlen + litlen;
                            opt[pos].price = price;
                        }
                    }
                }
            }

            // Set prices using match at current position
            let match_ml = new_len.min(LZ4_OPT_NUM - cur - 1);
            for ml in MIN_MATCH..=match_ml {
                let pos = cur + ml;
                if pos >= opt.len() {
                    break;
                }

                let (ll, price) = if opt[cur].mlen == 1 {
                    let ll = opt[cur].litlen;
                    let base_price = if cur > ll { opt[cur - ll].price } else { 0 };
                    if base_price == i32::MAX {
                        continue;
                    }
                    (ll, base_price.saturating_add(sequence_price(ll, ml)))
                } else {
                    if opt[cur].price == i32::MAX {
                        continue;
                    }
                    (0, opt[cur].price.saturating_add(sequence_price(0, ml)))
                };

                if pos > last_match_pos + TRAILING_LITERALS || price <= opt[pos].price {
                    if ml == match_ml && last_match_pos < pos {
                        last_match_pos = pos;
                    }
                    opt[pos].mlen = ml;
                    opt[pos].off = new_off;
                    opt[pos].litlen = ll;
                    opt[pos].price = price;
                }
            }

            // Complete following positions with literals
            for add_lit in 1..=TRAILING_LITERALS {
                let pos = last_match_pos + add_lit;
                if pos < opt.len() && opt[last_match_pos].price != i32::MAX {
                    opt[pos].mlen = 1; // literal
                    opt[pos].off = 0;
                    opt[pos].litlen = add_lit;
                    opt[pos].price = opt[last_match_pos].price.saturating_add(literals_price(add_lit));
                }
            }

            cur += 1;
        }

        // Reverse traversal to find the optimal path
        let mut best_mlen = opt[last_match_pos].mlen;
        let mut best_off = opt[last_match_pos].off;
        let mut candidate_pos = last_match_pos.saturating_sub(best_mlen);

        // Trace back through the optimal path and reverse the links
        loop {
            let next_mlen = opt[candidate_pos].mlen;
            let next_off = opt[candidate_pos].off;
            opt[candidate_pos].mlen = best_mlen;
            opt[candidate_pos].off = best_off;
            best_mlen = next_mlen;
            best_off = next_off;
            if next_mlen > candidate_pos {
                break; // First match
            }
            candidate_pos -= next_mlen;
        }

        // Encode all recorded sequences in order
        let mut rpos = 0;
        while rpos < last_match_pos {
            let ml = opt[rpos].mlen;
            let offset = opt[rpos].off;

            if ml == 1 {
                // Literal
                ip += 1;
                rpos += 1;
                continue;
            }

            // Encode the sequence
            encode_sequence(
                &input[anchor..ip],
                output,
                offset as u16,
                ml - MIN_MATCH,
            );

            ip += ml;
            anchor = ip;
            rpos += ml;
        }

        // Reset opt array for next iteration
        for state in opt.iter_mut().take(last_match_pos + TRAILING_LITERALS + 1) {
            *state = OptimalState::new();
        }
    }

    // Handle remaining literals
    handle_last_literals(output, &input[anchor..src_end]);
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
        assert!(opt_size <= hc_size, "Level 12 ({}) should be <= Level 9 ({})", opt_size, hc_size);

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
        let input: Vec<u8> = (0..10000)
            .map(|i| ((i * 7 + 13) % 256) as u8)
            .collect();

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
        assert!(compressed_size < input.len() / 10, "Should compress very well");

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
    let input = b"The quick brown fox jumps over the lazy dog. The quick brown fox.";
    println!("Input len: {}", input.len());
    println!("Input: {:?}", String::from_utf8_lossy(input));
    
    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);
    let size = compress_mid_internal(input, &mut sink).unwrap();
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
