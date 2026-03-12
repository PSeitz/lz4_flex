//! The compression algorithm.
//!
//! We make use of hash tables to find duplicates. This gives a reasonable compression ratio with a
//! high performance. It has fixed memory usage, which contrary to other approaches, makes it less
//! memory hungry.

use crate::block::hashtable::HashTable;
use crate::block::END_OFFSET;
use crate::block::LZ4_MIN_LENGTH;
use crate::block::MAX_DISTANCE;
use crate::block::MFLIMIT;
use crate::block::MINMATCH;
#[cfg(not(feature = "safe-encode"))]
use crate::sink::PtrSink;
use crate::sink::Sink;
use crate::sink::SliceSink;
#[allow(unused_imports)]
use alloc::vec;

#[allow(unused_imports)]
use alloc::vec::Vec;

pub(crate) use super::hashtable::HashTable4K;
pub(crate) use super::hashtable::HashTable4KU16;
use super::{CompressError, WINDOW_SIZE};

/// Increase step size after 1<<INCREASE_STEPSIZE_BITSHIFT non matches
const INCREASE_STEPSIZE_BITSHIFT: usize = 5;

/// Read a 4-byte "batch" from some position.
///
/// This will read a native-endian 4-byte integer from some position.
#[inline]
#[cfg(not(feature = "safe-encode"))]
pub(super) fn get_batch(input: &[u8], n: usize) -> u32 {
    unsafe { read_u32_ptr(input.as_ptr().add(n)) }
}

#[inline]
#[cfg(feature = "safe-encode")]
pub(super) fn get_batch(input: &[u8], n: usize) -> u32 {
    u32::from_ne_bytes(input[n..n + 4].try_into().unwrap())
}

/// Read an usize sized "batch" from some position.
///
/// This will read a native-endian usize from some position.
#[inline]
#[allow(dead_code)]
#[cfg(not(feature = "safe-encode"))]
pub(super) fn get_batch_arch(input: &[u8], n: usize) -> usize {
    unsafe { read_usize_ptr(input.as_ptr().add(n)) }
}

#[inline]
#[allow(dead_code)]
#[cfg(feature = "safe-encode")]
pub(super) fn get_batch_arch(input: &[u8], n: usize) -> usize {
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    let arr: &[u8; USIZE_SIZE] = input[n..n + USIZE_SIZE].try_into().unwrap();
    usize::from_ne_bytes(*arr)
}

#[inline]
fn token_from_literal(lit_len: usize) -> u8 {
    if lit_len < 0xF {
        // Since we can fit the literals length into it, there is no need for saturation.
        (lit_len as u8) << 4
    } else {
        // We were unable to fit the literals into it, so we saturate to 0xF. We will later
        // write the extensional value.
        0xF0
    }
}

#[inline]
fn token_from_literal_and_match_length(lit_len: usize, duplicate_length: usize) -> u8 {
    let mut token = if lit_len < 0xF {
        // Since we can fit the literals length into it, there is no need for saturation.
        (lit_len as u8) << 4
    } else {
        // We were unable to fit the literals into it, so we saturate to 0xF. We will later
        // write the extensional value.
        0xF0
    };

    token |= if duplicate_length < 0xF {
        // We could fit it in.
        duplicate_length as u8
    } else {
        // We were unable to fit it in, so we default to 0xF, which will later be extended.
        0xF
    };

    token
}

/// Counts the number of same bytes in two byte streams.
/// `input` is the complete input
/// `cur` is the current position in the input. it will be incremented by the number of matched
/// bytes `source` either the same as input or an external slice
/// `candidate` is the candidate position in `source`
/// `match_limit` is the absolute position in `input` beyond which we must not read
#[inline]
#[cfg(feature = "safe-encode")]
pub(crate) fn count_same_bytes(input: &[u8], cur: &mut usize, source: &[u8], candidate: usize, match_limit: usize) -> usize {
    const STEP: usize = core::mem::size_of::<usize>();
    let max_input = match_limit.saturating_sub(*cur);
    let max_source = source.len().saturating_sub(candidate);
    let max_len = max_input.min(max_source);

    let cur_slice = &input[*cur..*cur + max_len];
    let cand_slice = &source[candidate..candidate + max_len];

    let mut num = 0;
    for (block1, block2) in cur_slice.chunks_exact(STEP).zip(cand_slice.chunks_exact(STEP)) {
        let v1 = usize::from_ne_bytes(block1.try_into().unwrap());
        let v2 = usize::from_ne_bytes(block2.try_into().unwrap());
        if v1 == v2 {
            num += STEP;
        } else {
            num += ((v1 ^ v2).to_le().trailing_zeros() / 8) as usize;
            *cur += num;
            return num;
        }
    }

    // Cold tail: byte-by-byte for the remaining 0..7 bytes
    #[cold]
    fn count_tail(a: &[u8], b: &[u8], offset: usize) -> usize {
        a.iter().zip(b).skip(offset).take_while(|(a, b)| a == b).count()
    }
    num += count_tail(cur_slice, cand_slice, num);

    *cur += num;
    num
}

/// Counts the number of same bytes in two byte streams, using pointer-based comparison.
/// `input` is the complete input
/// `cur` is the current position in the input. it will be incremented by the number of matched
/// bytes `source` either the same as input OR an external slice
/// `candidate` is the candidate position in `source`
/// `match_limit` is the absolute position in `input` beyond which we must not read
#[inline]
#[cfg(not(feature = "safe-encode"))]
pub(crate) fn count_same_bytes(input: &[u8], cur: &mut usize, source: &[u8], candidate: usize, match_limit: usize) -> usize {
    let max_input = match_limit.saturating_sub(*cur);
    let max_source = source.len() - candidate;
    let max_len = max_input.min(max_source);

    // SAFETY: *cur + max_len <= match_limit <= input.len(), candidate + max_len <= source.len()
    unsafe {
        let mut p_in = input.as_ptr().add(*cur);
        let mut p_match = source.as_ptr().add(candidate);
        let p_in_limit = p_in.add(max_len);
        let p_start = p_in;

        const STEP: usize = core::mem::size_of::<usize>();

        while p_in < p_in_limit.sub(STEP - 1) {
            let diff = read_usize_ptr(p_in) ^ read_usize_ptr(p_match);
            if diff == 0 {
                p_in = p_in.add(STEP);
                p_match = p_match.add(STEP);
            } else {
                p_in = p_in.add((diff.to_le().trailing_zeros() / 8) as usize);
                let num = p_in.offset_from(p_start) as usize;
                *cur += num;
                return num;
            }
        }

        #[cfg(target_pointer_width = "64")]
        if p_in < p_in_limit.sub(3) {
            let diff = read_u32_ptr(p_in) ^ read_u32_ptr(p_match);
            if diff == 0 {
                p_in = p_in.add(4);
                p_match = p_match.add(4);
            } else {
                p_in = p_in.add((diff.to_le().trailing_zeros() / 8) as usize);
                let num = p_in.offset_from(p_start) as usize;
                *cur += num;
                return num;
            }
        }

        if p_in < p_in_limit.sub(1)
            && read_u16_ptr(p_in) == read_u16_ptr(p_match)
        {
            p_in = p_in.add(2);
            p_match = p_match.add(2);
        }

        if p_in < p_in_limit && *p_in == *p_match {
            p_in = p_in.add(1);
        }

        let num = p_in.offset_from(p_start) as usize;
        *cur += num;
        num
    }
}

/// Write an integer to the output.
///
/// Each additional byte then represent a value from 0 to 255, which is added to the previous value
/// to produce a total length. When the byte value is 255, another byte must read and added, and so
/// on. There can be any number of bytes of value "255" following token
#[inline]
pub(super) fn write_integer(output: &mut impl Sink, mut n: usize) {
    // Note: Since `n` is usually < 0xFF and writing multiple bytes to the output
    // requires 2 branches of bound check (due to the possibility of add overflows)
    // the simple byte at a time implementation below is faster in most cases.
    while n >= 0xFF {
        n -= 0xFF;
        push_byte(output, 0xFF);
    }
    push_byte(output, n as u8);
}

/// Handle the last bytes from the input as literals
#[cold]
pub(crate) fn handle_last_literals(output: &mut impl Sink, input: &[u8]) {
    let lit_len = input.len();

    let token = token_from_literal(lit_len);
    push_byte(output, token);
    if lit_len >= 0xF {
        write_integer(output, lit_len - 0xF);
    }
    // Now, write the actual literals.
    output.extend_from_slice(input);
}

/// Moves the cursors back as long as the bytes match, to find additional bytes in a duplicate
#[inline]
#[cfg(feature = "safe-encode")]
pub(crate) fn backtrack_match(
    input: &[u8],
    cur: &mut usize,
    literal_start: usize,
    source: &[u8],
    candidate: &mut usize,
) {
    // Note: Even if iterator version of this loop has less branches inside the loop it has more
    // branches before the loop. That in practice seems to make it slower than the while version
    // bellow. TODO: It should be possible remove all bounds checks, since we are walking
    // backwards
    while *candidate > 0 && *cur > literal_start && input[*cur - 1] == source[*candidate - 1] {
        *cur -= 1;
        *candidate -= 1;
    }
}

/// Moves the cursors back as long as the bytes match, to find additional bytes in a duplicate
#[inline]
#[cfg(not(feature = "safe-encode"))]
pub(crate) fn backtrack_match(
    input: &[u8],
    cur: &mut usize,
    literal_start: usize,
    source: &[u8],
    candidate: &mut usize,
) {
    while unsafe {
        *candidate > 0
            && *cur > literal_start
            && input.get_unchecked(*cur - 1) == source.get_unchecked(*candidate - 1)
    } {
        *cur -= 1;
        *candidate -= 1;
    }
}

/// Compress all bytes of `input[input_pos..]` into `output`.
///
/// Bytes in `input[..input_pos]` are treated as a preamble and can be used for lookback.
/// This part is known as the compressor "prefix".
/// Bytes in `ext_dict` logically precede the bytes in `input` and can also be used for lookback.
///
/// `input_stream_offset` is the logical position of the first byte of `input`. This allows same
/// `dict` to be used for many calls to `compress_internal` as we can "readdress" the first byte of
/// `input` to be something other than 0.
///
/// `dict` is the dictionary of previously encoded sequences.
///
/// This is used to find duplicates in the stream so they are not written multiple times.
///
/// Every four bytes are hashed, and in the resulting slot their position in the input buffer
/// is placed in the dict. This way we can easily look up a candidate to back references.
///
/// Returns the number of bytes written (compressed) into `output`.
///
/// # Const parameters
/// `USE_DICT`: Disables usage of ext_dict (it'll panic if a non-empty slice is used).
/// In other words, this generates more optimized code when an external dictionary isn't used.
///
/// A similar const argument could be used to disable the Prefix mode (eg. USE_PREFIX),
/// which would impose `input_pos == 0 && input_stream_offset == 0`. Experiments didn't
/// show significant improvement though.
// Intentionally avoid inlining.
// Empirical tests revealed it to be rarely better but often significantly detrimental.
#[inline(never)]
pub(crate) fn compress_internal<T: HashTable, const USE_DICT: bool, S: Sink>(
    input: &[u8],
    input_pos: usize,
    output: &mut S,
    dict: &mut T,
    ext_dict: &[u8],
    input_stream_offset: usize,
) -> Result<usize, CompressError> {
    assert!(input_pos <= input.len());
    if USE_DICT {
        assert!(ext_dict.len() <= super::WINDOW_SIZE);
        assert!(ext_dict.len() <= input_stream_offset);
        // Check for overflow hazard when using ext_dict
        assert!(input_stream_offset
            .checked_add(input.len())
            .and_then(|i| i.checked_add(ext_dict.len()))
            .is_some_and(|i| i <= isize::MAX as usize));
    } else {
        assert!(ext_dict.is_empty());
    }
    if output.capacity() - output.pos() < get_maximum_output_size(input.len() - input_pos) {
        return Err(CompressError::OutputTooSmall);
    }

    let output_start_pos = output.pos();
    if input.len() - input_pos < LZ4_MIN_LENGTH {
        handle_last_literals(output, &input[input_pos..]);
        return Ok(output.pos() - output_start_pos);
    }

    let ext_dict_stream_offset = input_stream_offset - ext_dict.len();
    let end_pos_check = input.len() - MFLIMIT;
    let mut literal_start = input_pos;
    let mut cur = input_pos;

    if cur == 0 && input_stream_offset == 0 {
        // According to the spec we can't start with a match,
        // except when referencing another block.
        let hash = T::get_hash_at(input, 0);
        dict.put_at(hash, 0);
        cur = 1;
    }

    loop {
        // Read the next block into two sections, the literals and the duplicates.
        let mut step_size;
        let mut candidate;
        let mut candidate_source;
        let mut offset;
        let mut non_match_count = 1 << INCREASE_STEPSIZE_BITSHIFT;
        // The number of bytes before our cursor, where the duplicate starts.
        let mut next_cur = cur;

        // In this loop we search for duplicates via the hashtable. 4bytes or 8bytes are hashed and
        // compared.
        loop {
            step_size = non_match_count >> INCREASE_STEPSIZE_BITSHIFT;
            non_match_count += 1;

            cur = next_cur;
            next_cur += step_size;

            // Same as cur + MFLIMIT > input.len()
            if cur > end_pos_check {
                handle_last_literals(output, &input[literal_start..]);
                return Ok(output.pos() - output_start_pos);
            }
            // Find a candidate in the dictionary with the hash of the current four bytes.
            // Unchecked is safe as long as the values from the hash function don't exceed the size
            // of the table. This is ensured by right shifting the hash values
            // (`dict_bitshift`) to fit them in the table

            // [Bounds Check]: Can be elided due to `end_pos_check` above
            let hash = T::get_hash_at(input, cur);
            candidate = dict.get_at(hash);
            dict.put_at(hash, cur + input_stream_offset);

            // Sanity check: Matches can't be ahead of `cur`.
            debug_assert!(candidate <= input_stream_offset + cur);

            // Two requirements to the candidate exists:
            // - We should not return a position which is merely a hash collision, so that the
            //   candidate actually matches what we search for.
            // - We can address up to 16-bit offset, hence we are only able to address the candidate
            //   if its offset is less than or equals to 0xFFFF.
            if input_stream_offset + cur - candidate > MAX_DISTANCE {
                continue;
            }

            if candidate >= input_stream_offset {
                // match within input
                offset = (input_stream_offset + cur - candidate) as u16;
                candidate -= input_stream_offset;
                candidate_source = input;
            } else if USE_DICT {
                // Sanity check, which may fail if we lost history beyond MAX_DISTANCE
                debug_assert!(
                    candidate >= ext_dict_stream_offset,
                    "Lost history in ext dict mode"
                );
                // match within ext dict
                offset = (input_stream_offset + cur - candidate) as u16;
                candidate -= ext_dict_stream_offset;
                candidate_source = ext_dict;
            } else {
                // Match is not reachable anymore
                // eg. compressing an independent block frame w/o clearing
                // the matches tables, only increasing input_stream_offset.
                // Sanity check
                debug_assert!(input_pos == 0, "Lost history in prefix mode");
                continue;
            }
            // [Bounds Check]: Candidate is coming from the Hashmap. It can't be out of bounds, but
            // impossible to prove for the compiler and remove the bounds checks.
            let cand_bytes: u32 = get_batch(candidate_source, candidate);
            // [Bounds Check]: Should be able to be elided due to `end_pos_check`.
            let curr_bytes: u32 = get_batch(input, cur);

            if cand_bytes == curr_bytes {
                break;
            }
        }

        // Extend the match backwards if we can
        backtrack_match(
            input,
            &mut cur,
            literal_start,
            candidate_source,
            &mut candidate,
        );

        // The length (in bytes) of the literals section.
        let lit_len = cur - literal_start;

        // Generate the higher half of the token.
        cur += MINMATCH;
        candidate += MINMATCH;
        let duplicate_length = count_same_bytes(input, &mut cur, candidate_source, candidate, input.len() - END_OFFSET);

        // Note: The `- 2` offset was copied from the reference implementation, it could be
        // arbitrary.
        let hash = T::get_hash_at(input, cur - 2);
        dict.put_at(hash, cur - 2 + input_stream_offset);

        encode_sequence(&input[literal_start..literal_start + lit_len], output, offset, duplicate_length);

        literal_start = cur;
    }
}

pub(crate) fn encode_sequence<S: Sink>(literal: &[u8], output: &mut S, offset: u16, match_len: usize) {
    let token = token_from_literal_and_match_length(literal.len(), match_len);
    // Push the token to the output stream.
    push_byte(output, token);
    // If we were unable to fit the literals length into the token, write the extensional
    // part.
    if literal.len() >= 0xF {
        write_integer(output, literal.len() - 0xF);
    }

    // Now, write the actual literals.
    //
    // The unsafe version copies blocks of 8bytes, and therefore may copy up to 7bytes more than
    // needed. This is safe, because the last 12 bytes (MF_LIMIT) are handled in
    // handle_last_literals.
    copy_literals_wild(output, literal, 0, literal.len());
    // write the offset in little endian.
    push_u16(output, offset);

    // If we were unable to fit the duplicates length into the token, write the
    // extensional part.
    if match_len >= 0xF {
        write_integer(output, match_len - 0xF);
    }

}

#[inline]
#[cfg(feature = "safe-encode")]
fn push_byte(output: &mut impl Sink, el: u8) {
    output.push(el);
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn push_byte(output: &mut impl Sink, el: u8) {
    unsafe {
        core::ptr::write(output.pos_mut_ptr(), el);
        output.set_pos(output.pos() + 1);
    }
}

#[inline]
#[cfg(feature = "safe-encode")]
fn push_u16(output: &mut impl Sink, el: u16) {
    output.extend_from_slice(&el.to_le_bytes());
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn push_u16(output: &mut impl Sink, el: u16) {
    unsafe {
        core::ptr::copy_nonoverlapping(el.to_le_bytes().as_ptr(), output.pos_mut_ptr(), 2);
        output.set_pos(output.pos() + 2);
    }
}

#[inline(always)] // (always) necessary otherwise compiler fails to inline it
#[cfg(feature = "safe-encode")]
fn copy_literals_wild(output: &mut impl Sink, input: &[u8], input_start: usize, len: usize) {
    output.extend_from_slice_wild(&input[input_start..input_start + len], len)
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn copy_literals_wild(output: &mut impl Sink, input: &[u8], input_start: usize, len: usize) {
    debug_assert!(output.pos() + len / 8 * 8 + ((len % 8) != 0) as usize * 8 <= output.capacity());
    unsafe {
        // Note: This used to be a wild copy loop of 8 bytes, but the compiler consistently
        // transformed it into a call to memcopy, which hurts performance significantly for
        // small copies, which are common.
        let start_ptr = input.as_ptr().add(input_start);
        match len {
            0..=8 => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), 8),
            9..=16 => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), 16),
            17..=24 => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), 24),
            _ => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), len),
        }
        output.set_pos(output.pos() + len);
    }
}

/// Compress all bytes of `input` into `output`.
/// The method chooses an appropriate hashtable to lookup duplicates.
/// output should be preallocated with a size of
/// `get_maximum_output_size`.
///
/// Returns the number of bytes written (compressed) into `output`.
#[inline]
pub(crate) fn compress_into_sink_with_dict<const USE_DICT: bool>(
    input: &[u8],
    output: &mut impl Sink,
    mut dict_data: &[u8],
) -> Result<usize, CompressError> {
    if dict_data.len() + input.len() < u16::MAX as usize {
        let mut dict = HashTable4KU16::new();
        init_dict(&mut dict, &mut dict_data);
        compress_internal::<_, USE_DICT, _>(input, 0, output, &mut dict, dict_data, dict_data.len())
    } else {
        let mut dict = HashTable4K::new();
        init_dict(&mut dict, &mut dict_data);
        compress_internal::<_, USE_DICT, _>(input, 0, output, &mut dict, dict_data, dict_data.len())
    }
}

#[inline]
fn init_dict<T: HashTable>(dict: &mut T, dict_data: &mut &[u8]) {
    if dict_data.len() > WINDOW_SIZE {
        *dict_data = &dict_data[dict_data.len() - WINDOW_SIZE..];
    }
    let mut i = 0usize;
    while i + core::mem::size_of::<usize>() <= dict_data.len() {
        let hash = T::get_hash_at(dict_data, i);
        dict.put_at(hash, i);
        // Note: The 3 byte step was copied from the reference implementation, it could be
        // arbitrary.
        i += 3;
    }
}

/// Returns the maximum output size of the compressed data.
/// Can be used to preallocate capacity on the output vector
#[inline]
pub const fn get_maximum_output_size(input_len: usize) -> usize {
    16 + 4 + (input_len as u64 * 110 / 100) as usize
}

/// Compress all bytes of `input` into `output`.
/// The method chooses an appropriate hashtable to lookup duplicates.
/// output should be preallocated with a size of
/// `get_maximum_output_size`.
///
/// Returns the number of bytes written (compressed) into `output`.
#[inline]
pub fn compress_into(input: &[u8], output: &mut [u8]) -> Result<usize, CompressError> {
    compress_into_sink_with_dict::<false>(input, &mut SliceSink::new(output, 0), b"")
}

/// Compress all bytes of `input` into `output`.
/// The method chooses an appropriate hashtable to lookup duplicates.
/// output should be preallocated with a size of
/// `get_maximum_output_size`.
///
/// Returns the number of bytes written (compressed) into `output`.
#[inline]
pub fn compress_into_with_dict(
    input: &[u8],
    output: &mut [u8],
    dict_data: &[u8],
) -> Result<usize, CompressError> {
    compress_into_sink_with_dict::<true>(input, &mut SliceSink::new(output, 0), dict_data)
}

#[inline]
fn compress_into_vec_with_dict<const USE_DICT: bool>(
    input: &[u8],
    prepend_size: bool,
    mut dict_data: &[u8],
) -> Vec<u8> {
    let prepend_size_num_bytes = if prepend_size { 4 } else { 0 };
    let max_compressed_size = get_maximum_output_size(input.len()) + prepend_size_num_bytes;
    if dict_data.len() <= 3 {
        dict_data = b"";
    }
    #[cfg(feature = "safe-encode")]
    let mut compressed = {
        let mut compressed: Vec<u8> = vec![0u8; max_compressed_size];
        let out = if prepend_size {
            compressed[..4].copy_from_slice(&(input.len() as u32).to_le_bytes());
            &mut compressed[4..]
        } else {
            &mut compressed
        };
        let compressed_len =
            compress_into_sink_with_dict::<USE_DICT>(input, &mut SliceSink::new(out, 0), dict_data)
                .unwrap();

        compressed.truncate(prepend_size_num_bytes + compressed_len);
        compressed
    };
    #[cfg(not(feature = "safe-encode"))]
    let mut compressed = {
        let mut vec = Vec::with_capacity(max_compressed_size);
        let start_pos = if prepend_size {
            vec.extend_from_slice(&(input.len() as u32).to_le_bytes());
            4
        } else {
            0
        };
        let compressed_len = compress_into_sink_with_dict::<USE_DICT>(
            input,
            &mut PtrSink::from_vec(&mut vec, start_pos),
            dict_data,
        )
        .unwrap();
        unsafe {
            vec.set_len(prepend_size_num_bytes + compressed_len);
        }
        vec
    };

    compressed.shrink_to_fit();
    compressed
}

/// Compress all bytes of `input` into `output`. The uncompressed size will be prepended as a little
/// endian u32. Can be used in conjunction with `decompress_size_prepended`
#[inline]
pub fn compress_prepend_size(input: &[u8]) -> Vec<u8> {
    compress_into_vec_with_dict::<false>(input, true, b"")
}

/// Compress all bytes of `input`.
#[inline]
pub fn compress(input: &[u8]) -> Vec<u8> {
    compress_into_vec_with_dict::<false>(input, false, b"")
}

/// Compress all bytes of `input` with an external dictionary.
#[inline]
pub fn compress_with_dict(input: &[u8], ext_dict: &[u8]) -> Vec<u8> {
    compress_into_vec_with_dict::<true>(input, false, ext_dict)
}

/// Compress all bytes of `input` into `output`. The uncompressed size will be prepended as a little
/// endian u32. Can be used in conjunction with `decompress_size_prepended_with_dict`
#[inline]
pub fn compress_prepend_size_with_dict(input: &[u8], ext_dict: &[u8]) -> Vec<u8> {
    compress_into_vec_with_dict::<true>(input, true, ext_dict)
}

/// A reusable compression table that avoids re-allocating the internal hash table on every call.
///
/// This is useful when compressing many small inputs in a loop. Create one table and pass it
/// to [`compress_into_with_table`] repeatedly.
///
/// # Example
/// ```
/// use lz4_flex::block::{compress_into_with_table, get_maximum_output_size, CompressTable};
///
/// let mut table = CompressTable::default();
/// let input = b"hello world, hello world, hello!";
/// let mut output = vec![0u8; get_maximum_output_size(input.len())];
/// let compressed_len = compress_into_with_table(input, &mut output, &mut table).unwrap();
/// ```
pub enum CompressTable {
    /// Table using 16-bit entries, suitable for inputs where `input.len() < u16::MAX`.
    Small(HashTable4KU16),
    /// Table using 32-bit entries, suitable for any input size.
    Large(HashTable4K),
}

impl Default for CompressTable {
    fn default() -> Self {
        CompressTable::Small(HashTable4KU16::new())
    }
}

impl CompressTable {
    /// Create a small table (16-bit entries). More memory efficient, but only usable when the
    /// total input size is less than 65535 bytes.
    pub fn small() -> Self {
        CompressTable::Small(HashTable4KU16::new())
    }

    /// Create a large table (32-bit entries). Works for any input size.
    pub fn large() -> Self {
        CompressTable::Large(HashTable4K::new())
    }
}

/// Compress all bytes of `input` into `output`, reusing a [`CompressTable`] to avoid
/// re-allocating the internal hash table.
///
/// `output` should be preallocated with a size of [`get_maximum_output_size`].
///
/// Returns the number of bytes written (compressed) into `output`.
///
/// **Note:** If the table variant doesn't match the input size (e.g. a `Small` table is used
/// with input >= 64KB), the table will be transparently upgraded. However, it won't be
/// downgraded automatically.
#[inline]
pub fn compress_into_with_table(
    input: &[u8],
    output: &mut [u8],
    table: &mut CompressTable,
) -> Result<usize, CompressError> {
    if input.len() >= u16::MAX as usize && matches!(table, CompressTable::Small(_)) {
        *table = CompressTable::Large(HashTable4K::new());
    }

    match table {
        CompressTable::Small(dict) => {
            dict.clear();
            compress_internal::<_, false, _>(input, 0, &mut SliceSink::new(output, 0), dict, b"", 0)
        }
        CompressTable::Large(dict) => {
            dict.clear();
            compress_internal::<_, false, _>(input, 0, &mut SliceSink::new(output, 0), dict, b"", 0)
        }
    }
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn read_u16_ptr(input: *const u8) -> u16 {
    let mut num: u16 = 0;
    unsafe {
        core::ptr::copy_nonoverlapping(input, &mut num as *mut u16 as *mut u8, 2);
    }
    num
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn read_u32_ptr(input: *const u8) -> u32 {
    let mut num: u32 = 0;
    unsafe {
        core::ptr::copy_nonoverlapping(input, &mut num as *mut u32 as *mut u8, 4);
    }
    num
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn read_usize_ptr(input: *const u8) -> usize {
    let mut num: usize = 0;
    unsafe {
        core::ptr::copy_nonoverlapping(
            input,
            &mut num as *mut usize as *mut u8,
            core::mem::size_of::<usize>(),
        );
    }
    num
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_same_bytes() {
        // 8byte aligned block, zeros and ones are added because the end/offset
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0, first.len() - END_OFFSET), 16);

        // 4byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0, first.len() - END_OFFSET), 20);

        // 2byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0, first.len() - END_OFFSET), 22);

        // 1byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0, first.len() - END_OFFSET), 23);

        // 1byte aligned block - last byte different
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0, first.len() - END_OFFSET), 22);

        // 1byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 9, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0, first.len() - END_OFFSET), 21);

        for diff_idx in 8..100 {
            let first: Vec<u8> = (0u8..255).cycle().take(100 + 12).collect();
            let mut second = first.clone();
            second[diff_idx] = 255;
            for start in 0..=diff_idx {
                let same_bytes = count_same_bytes(&first, &mut start.clone(), &second, start, first.len() - END_OFFSET);
                assert_eq!(same_bytes, diff_idx - start);
            }
        }
    }

    #[test]
    fn test_bug() {
        let input: &[u8] = &[
            10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
        ];
        let _out = compress(input);
    }

    #[test]
    fn test_dict() {
        let input: &[u8] = &[
            10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
        ];
        let dict = input;
        let compressed = compress_with_dict(input, dict);
        assert_lt!(compressed.len(), compress(input).len());

        assert!(compressed.len() < compress(input).len());
        let mut uncompressed = vec![0u8; input.len()];
        let uncomp_size = crate::block::decompress::decompress_into_with_dict(
            &compressed,
            &mut uncompressed,
            dict,
        )
        .unwrap();
        uncompressed.truncate(uncomp_size);
        assert_eq!(input, uncompressed);
    }

    #[test]
    fn test_dict_no_panic() {
        let input: &[u8] = &[
            10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
        ];
        let dict = &[10, 12, 14];
        let _compressed = compress_with_dict(input, dict);
    }

    #[test]
    fn test_dict_match_crossing() {
        let input: &[u8] = &[
            10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
        ];
        let dict = input;
        let compressed = compress_with_dict(input, dict);
        assert_lt!(compressed.len(), compress(input).len());

        let mut uncompressed = vec![0u8; input.len() * 2];
        // copy first half of the input into output
        let dict_cutoff = dict.len() / 2;
        let output_start = dict.len() - dict_cutoff;
        uncompressed[..output_start].copy_from_slice(&dict[dict_cutoff..]);
        let uncomp_len = {
            let mut sink = SliceSink::new(&mut uncompressed[..], output_start);
            crate::block::decompress::decompress_internal::<true, _>(
                &compressed,
                &mut sink,
                &dict[..dict_cutoff],
            )
            .unwrap()
        };
        assert_eq!(input.len(), uncomp_len);
        assert_eq!(
            input,
            &uncompressed[output_start..output_start + uncomp_len]
        );
    }

    #[test]
    fn test_conformant_last_block() {
        // From the spec:
        // The last match must start at least 12 bytes before the end of block.
        // The last match is part of the penultimate sequence. It is followed by the last sequence,
        // which contains only literals. Note that, as a consequence, an independent block <
        // 13 bytes cannot be compressed, because the match must copy "something",
        // so it needs at least one prior byte.
        // When a block can reference data from another block, it can start immediately with a match
        // and no literal, so a block of 12 bytes can be compressed.
        let aaas: &[u8] = b"aaaaaaaaaaaaaaa";

        // incompressible
        let out = compress(&aaas[..12]);
        assert_gt!(out.len(), 12);
        // compressible
        let out = compress(&aaas[..13]);
        assert_le!(out.len(), 13);
        let out = compress(&aaas[..14]);
        assert_le!(out.len(), 14);
        let out = compress(&aaas[..15]);
        assert_le!(out.len(), 15);

        // dict incompressible
        let out = compress_with_dict(&aaas[..11], aaas);
        assert_gt!(out.len(), 11);
        // compressible
        let out = compress_with_dict(&aaas[..12], aaas);
        // According to the spec this _could_ compress, but it doesn't in this lib
        // as it aborts compression for any input len < LZ4_MIN_LENGTH
        assert_gt!(out.len(), 12);
        let out = compress_with_dict(&aaas[..13], aaas);
        assert_le!(out.len(), 13);
        let out = compress_with_dict(&aaas[..14], aaas);
        assert_le!(out.len(), 14);
        let out = compress_with_dict(&aaas[..15], aaas);
        assert_le!(out.len(), 15);
    }

    #[test]
    fn test_dict_size() {
        let dict = vec![b'a'; 1024 * 1024];
        let input = &b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaa"[..];
        let compressed = compress_prepend_size_with_dict(input, &dict);
        let decompressed =
            crate::block::decompress_size_prepended_with_dict(&compressed, &dict).unwrap();
        assert_eq!(decompressed, input);
    }
}
