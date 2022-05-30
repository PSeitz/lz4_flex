//! The compression algorithm.
//!
//! We make use of hash tables to find duplicates. This gives a reasonable compression ratio with a
//! high performance. It has fixed memory usage, which contrary to other approachs, makes it less
//! memory hungry.

use crate::block::hashtable::get_table_size;
use crate::block::hashtable::HashTable;
use crate::block::hashtable::{HashTableU16, HashTableU32, HashTableUsize};
use crate::block::END_OFFSET;
use crate::block::LZ4_MIN_LENGTH;
use crate::block::MAX_DISTANCE;
use crate::block::MFLIMIT;
use crate::block::MINMATCH;
use crate::sink::{vec_sink_for_compression, Sink, SliceSink};
use alloc::vec::Vec;

#[cfg(feature = "safe-encode")]
use core::convert::TryInto;

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
    let arr: &[u8; 4] = input[n..n + 4].try_into().unwrap();
    u32::from_ne_bytes(*arr)
}

/// Read an usize sized "batch" from some position.
///
/// This will read a native-endian usize from some position.
#[inline]
#[cfg(not(feature = "safe-encode"))]
pub(super) fn get_batch_arch(input: &[u8], n: usize) -> usize {
    unsafe { read_usize_ptr(input.as_ptr().add(n)) }
}

#[inline]
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
///
/// The function ignores the last END_OFFSET bytes in input as those should be literals.
#[inline]
#[cfg(feature = "safe-encode")]
fn count_same_bytes(input: &[u8], cur: &mut usize, source: &[u8], candidate: usize) -> usize {
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    let cur_slice = &input[*cur..input.len() - END_OFFSET];
    let cand_slice = &source[candidate..];

    let mut num = 0;
    for (block1, block2) in cur_slice
        .chunks_exact(USIZE_SIZE)
        .zip(cand_slice.chunks_exact(USIZE_SIZE))
    {
        let input_block = usize::from_ne_bytes(block1.try_into().unwrap());
        let match_block = usize::from_ne_bytes(block2.try_into().unwrap());

        if input_block == match_block {
            num += USIZE_SIZE;
        } else {
            let diff = input_block ^ match_block;
            num += (diff.to_le().trailing_zeros() / 8) as usize;
            *cur += num;
            return num;
        }
    }

    // If we're here we may have 1 to 7 bytes left to check close to the end of input
    // or source slices. Since this is rare occurrence we mark it cold to get better
    // ~5% better performance.
    #[cold]
    fn count_same_bytes_tail(a: &[u8], b: &[u8], offset: usize) -> usize {
        a.iter()
            .zip(b)
            .skip(offset)
            .take_while(|(a, b)| a == b)
            .count()
    }
    num += count_same_bytes_tail(cur_slice, cand_slice, num);

    *cur += num;
    num
}

/// Counts the number of same bytes in two byte streams.
/// `input` is the complete input
/// `cur` is the current position in the input. it will be incremented by the number of matched
/// bytes `source` either the same as input OR an external slice
/// `candidate` is the candidate position in `source`
///
/// The function ignores the last END_OFFSET bytes in input as those should be literals.
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn count_same_bytes(input: &[u8], cur: &mut usize, source: &[u8], candidate: usize) -> usize {
    let max_input_match = input.len().saturating_sub(*cur + END_OFFSET);
    let max_candidate_match = source.len() - candidate;
    // Considering both limits calc how far we may match in input.
    let input_end = *cur + max_input_match.min(max_candidate_match);

    let start = *cur;
    let mut source_ptr = unsafe { source.as_ptr().add(candidate) };

    // compare 4/8 bytes blocks depending on the arch
    const STEP_SIZE: usize = core::mem::size_of::<usize>();
    while *cur + STEP_SIZE <= input_end {
        let diff = read_usize_ptr(unsafe { input.as_ptr().add(*cur) }) ^ read_usize_ptr(source_ptr);

        if diff == 0 {
            *cur += STEP_SIZE;
            unsafe {
                source_ptr = source_ptr.add(STEP_SIZE);
            }
        } else {
            *cur += (diff.to_le().trailing_zeros() / 8) as usize;
            return *cur - start;
        }
    }

    // compare 4 bytes block
    #[cfg(target_pointer_width = "64")]
    {
        if input_end - *cur >= 4 {
            let diff = read_u32_ptr(unsafe { input.as_ptr().add(*cur) }) ^ read_u32_ptr(source_ptr);

            if diff == 0 {
                *cur += 4;
                unsafe {
                    source_ptr = source_ptr.add(4);
                }
            } else {
                *cur += (diff.to_le().trailing_zeros() / 8) as usize;
                return *cur - start;
            }
        }
    }

    // compare 2 bytes block
    if input_end - *cur >= 2
        && unsafe { read_u16_ptr(input.as_ptr().add(*cur)) == read_u16_ptr(source_ptr) }
    {
        *cur += 2;
        unsafe {
            source_ptr = source_ptr.add(2);
        }
    }

    if *cur < input_end
        && unsafe { input.as_ptr().add(*cur).read() } == unsafe { source_ptr.read() }
    {
        *cur += 1;
    }

    *cur - start
}

/// Write an integer to the output.
///
/// Each additional byte then represent a value from 0 to 255, which is added to the previous value
/// to produce a total length. When the byte value is 255, another byte must read and added, and so
/// on. There can be any number of bytes of value "255" following token
#[inline]
#[cfg(feature = "safe-encode")]
fn write_integer(output: &mut impl Sink, mut n: usize) {
    // Note: Since `n` is usually < 0xFF and writing multiple bytes to the output
    // requires 2 branches of bound check (due to the possibility of add overflows)
    // the simple byte at a time implementation bellow is faster in most cases.
    while n >= 0xFF {
        n -= 0xFF;
        push_byte(output, 0xFF);
    }
    push_byte(output, n as u8);
}

/// Write an integer to the output.
///
/// Each additional byte then represent a value from 0 to 255, which is added to the previous value
/// to produce a total length. When the byte value is 255, another byte must read and added, and so
/// on. There can be any number of bytes of value "255" following token
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn write_integer(output: &mut impl Sink, mut n: usize) {
    // Write the 0xFF bytes as long as the integer is higher than said value.
    if n >= 4 * 0xFF {
        // In this unlikelly branch we use a fill instead of a loop,
        // otherwise rustc may output a large unrolled/vectorized loop.
        let bulk = n / (4 * 0xFF);
        n %= 4 * 0xFF;
        unsafe {
            core::ptr::write_bytes(output.pos_mut_ptr(), 0xFF, 4 * bulk);
            output.set_pos(output.pos() + 4 * bulk);
        }
    }

    // Handle last 1 to 4 bytes
    push_u32(output, 0xFFFFFFFF);
    // Updating output len for the remainder
    unsafe {
        output.set_pos(output.pos() - 4 + 1 + n / 255);
        // Write the remaining byte.
        *output.pos_mut_ptr().sub(1) = (n % 255) as u8;
    }
}

/// Handle the last bytes from the input as literals
#[cold]
fn handle_last_literals(output: &mut impl Sink, input: &[u8], start: usize) {
    let lit_len = input.len() - start;

    let token = token_from_literal(lit_len);
    push_byte(output, token);
    if lit_len >= 0xF {
        write_integer(output, lit_len - 0xF);
    }
    // Now, write the actual literals.
    output.extend_from_slice(&input[start..]);
}

/// Moves the cursors back as long as the bytes match, to find additional bytes in a duplicate
#[inline]
#[cfg(feature = "safe-encode")]
fn backtrack_match(
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
fn backtrack_match(
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
pub(crate) fn compress_internal<T: HashTable, SINK: Sink, const USE_DICT: bool>(
    input: &[u8],
    input_pos: usize,
    output: &mut SINK,
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
            .map_or(false, |i| i <= isize::MAX as usize));
    } else {
        assert!(ext_dict.is_empty());
    }
    if output.capacity() - output.pos() < get_maximum_output_size(input.len() - input_pos) {
        return Err(CompressError::OutputTooSmall);
    }

    let output_start_pos = output.pos();
    if input.len() - input_pos < LZ4_MIN_LENGTH {
        handle_last_literals(output, input, input_pos);
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
                handle_last_literals(output, input, literal_start);
                return Ok(output.pos() - output_start_pos);
            }
            // Find a candidate in the dictionary with the hash of the current four bytes.
            // Unchecked is safe as long as the values from the hash function don't exceed the size
            // of the table. This is ensured by right shifting the hash values
            // (`dict_bitshift`) to fit them in the table
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

            if get_batch(candidate_source, candidate) == get_batch(input, cur) {
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
        let duplicate_length = count_same_bytes(input, &mut cur, candidate_source, candidate);

        // Note: The `- 2` offset was copied from the reference implementation, it could be
        // arbitrary.
        let hash = T::get_hash_at(input, cur - 2);
        dict.put_at(hash, cur - 2 + input_stream_offset);

        let token = token_from_literal_and_match_length(lit_len, duplicate_length);

        // Push the token to the output stream.
        push_byte(output, token);
        // If we were unable to fit the literals length into the token, write the extensional
        // part.
        if lit_len >= 0xF {
            write_integer(output, lit_len - 0xF);
        }

        // Now, write the actual literals.
        //
        // The unsafe version copies blocks of 8bytes, and therefore may copy up to 7bytes more than
        // needed. This is safe, because the last 12 bytes (MF_LIMIT) are handled in
        // handle_last_literals.
        copy_literals_wild(output, input, literal_start, lit_len);
        // write the offset in little endian.
        push_u16(output, offset);

        // If we were unable to fit the duplicates length into the token, write the
        // extensional part.
        if duplicate_length >= 0xF {
            write_integer(output, duplicate_length - 0xF);
        }
        literal_start = cur;
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

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn push_u32(output: &mut impl Sink, el: u32) {
    unsafe {
        core::ptr::copy_nonoverlapping(el.to_le_bytes().as_ptr(), output.pos_mut_ptr(), 4);
        output.set_pos(output.pos() + 4);
    }
}

#[inline(always)] // (always) necessary otherwise compiler fails to inline it
#[cfg(feature = "safe-encode")]
fn copy_literals_wild(output: &mut impl Sink, input: &[u8], input_start: usize, len: usize) {
    match len {
        0..=8 => output.extend_from_slice_wild(&input[input_start..input_start + 8], len),
        9..=16 => output.extend_from_slice_wild(&input[input_start..input_start + 16], len),
        17..=24 => output.extend_from_slice_wild(&input[input_start..input_start + 24], len),
        _ => output.extend_from_slice_wild(&input[input_start..input_start + len], len),
    }
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn copy_literals_wild(output: &mut impl Sink, input: &[u8], input_start: usize, len: usize) {
    debug_assert!(input_start + len / 8 * 8 + ((len % 8) != 0) as usize * 8 <= input.len());
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

#[inline]
pub(crate) fn compress_into_sink(
    input: &[u8],
    output: &mut impl Sink,
) -> Result<usize, CompressError> {
    let (dict_size, dict_bitshift) = get_table_size(input.len());
    if input.len() < u16::MAX as usize {
        let mut dict = HashTableU16::new(dict_size, dict_bitshift);
        compress_internal::<_, _, false>(input, 0, output, &mut dict, b"", 0)
    } else if input.len() < u32::MAX as usize {
        let mut dict = HashTableU32::new(dict_size, dict_bitshift);
        compress_internal::<_, _, false>(input, 0, output, &mut dict, b"", 0)
    } else {
        let mut dict = HashTableUsize::new(dict_size, dict_bitshift);
        compress_internal::<_, _, false>(input, 0, output, &mut dict, b"", 0)
    }
}

/// Same as compress_into_sink but with supports external dictionary
#[inline]
pub(crate) fn compress_into_sink_with_dict(
    input: &[u8],
    output: &mut impl Sink,
    mut dict_data: &[u8],
) -> Result<usize, CompressError> {
    let (dict_size, dict_bitshift) = get_table_size(input.len());
    if dict_data.len() + input.len() < u16::MAX as usize {
        let mut dict = HashTableU16::new(dict_size, dict_bitshift);
        init_dict(&mut dict, &mut dict_data);
        compress_internal::<_, _, true>(input, 0, output, &mut dict, dict_data, dict_data.len())
    } else if dict_data.len() + input.len() < u32::MAX as usize {
        let mut dict = HashTableU32::new(dict_size, dict_bitshift);
        init_dict(&mut dict, &mut dict_data);
        compress_internal::<_, _, true>(input, 0, output, &mut dict, dict_data, dict_data.len())
    } else {
        let mut dict = HashTableUsize::new(dict_size, dict_bitshift);
        init_dict(&mut dict, &mut dict_data);
        compress_internal::<_, _, true>(input, 0, output, &mut dict, dict_data, dict_data.len())
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
pub fn get_maximum_output_size(input_len: usize) -> usize {
    16 + 4 + (input_len as f64 * 1.1) as usize
}

/// Compress all bytes of `input` into `output`.
/// The method chooses an appropriate hashtable to lookup duplicates and calls
/// `compress_into_with_table`. output should be preallocated with a size of
/// `get_maximum_output_size`.
///
/// Returns the number of bytes written (compressed) into `output`.
#[inline]
pub fn compress_into(input: &[u8], output: &mut [u8]) -> Result<usize, CompressError> {
    compress_into_sink(input, &mut SliceSink::new(output, 0))
}

/// Compress all bytes of `input` into `output`.
/// The method chooses an appropriate hashtable to lookup duplicates and calls
/// `compress_into_with_table`. output should be preallocated with a size of
/// `get_maximum_output_size`.
///
/// Returns the number of bytes written (compressed) into `output`.
#[inline]
pub fn compress_into_with_dict(
    input: &[u8],
    output: &mut [u8],
    dict_data: &[u8],
) -> Result<usize, CompressError> {
    compress_into_sink_with_dict(input, &mut SliceSink::new(output, 0), dict_data)
}

/// Compress all bytes of `input` into `output`. The uncompressed size will be prepended as a little
/// endian u32. Can be used in conjunction with `decompress_size_prepended`
#[inline]
pub fn compress_prepend_size(input: &[u8]) -> Vec<u8> {
    let max_compressed_size = get_maximum_output_size(input.len());
    let mut compressed = Vec::with_capacity(4 + max_compressed_size);
    compressed.extend_from_slice(&(input.len() as u32).to_le_bytes());
    let compressed_len = compress_into_sink(
        input,
        &mut vec_sink_for_compression(&mut compressed, 4, 0, max_compressed_size),
    )
    .unwrap();
    compressed.truncate(4 + compressed_len);
    compressed
}

/// Compress all bytes of `input`.
#[inline]
pub fn compress(input: &[u8]) -> Vec<u8> {
    let max_compressed_size = get_maximum_output_size(input.len());
    let mut compressed = Vec::with_capacity(max_compressed_size);
    let compressed_len = compress_into_sink(
        input,
        &mut vec_sink_for_compression(&mut compressed, 0, 0, max_compressed_size),
    )
    .unwrap();
    compressed.truncate(compressed_len);
    compressed
}

/// Compress all bytes of `input` with an external dictionary.
#[inline]
pub fn compress_with_dict(input: &[u8], ext_dict: &[u8]) -> Vec<u8> {
    let max_compressed_size = get_maximum_output_size(input.len());
    let mut compressed = Vec::with_capacity(max_compressed_size);
    let compressed_len = compress_into_sink_with_dict(
        input,
        &mut vec_sink_for_compression(&mut compressed, 0, 0, max_compressed_size),
        ext_dict,
    )
    .unwrap();
    compressed.truncate(compressed_len);
    compressed
}

/// Compress all bytes of `input` into `output`. The uncompressed size will be prepended as a little
/// endian u32. Can be used in conjunction with `decompress_size_prepended_with_dict`
#[inline]
pub fn compress_prepend_size_with_dict(input: &[u8], ext_dict: &[u8]) -> Vec<u8> {
    let max_compressed_size = get_maximum_output_size(input.len());
    let mut compressed = Vec::with_capacity(4 + max_compressed_size);
    compressed.extend_from_slice(&(input.len() as u32).to_le_bytes());
    let compressed_len = compress_into_sink_with_dict(
        input,
        &mut vec_sink_for_compression(&mut compressed, 4, 0, max_compressed_size),
        ext_dict,
    )
    .unwrap();
    compressed.truncate(4 + compressed_len);
    compressed
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

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn read_u16_ptr(input: *const u8) -> u16 {
    let mut num: u16 = 0;
    unsafe {
        core::ptr::copy_nonoverlapping(input, &mut num as *mut u16 as *mut u8, 2);
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
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 16);

        // 4byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 20);

        // 2byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 22);

        // 1byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 23);

        // 1byte aligned block - last byte different
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 22);

        // 1byte aligned block
        let first: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 9, 5, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0,
        ];
        let second: &[u8] = &[
            1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1,
        ];
        assert_eq!(count_same_bytes(first, &mut 0, second, 0), 21);

        for diff_idx in 0..100 {
            let first: Vec<u8> = (0u8..255).cycle().take(100 + END_OFFSET).collect();
            let mut second = first.clone();
            second[diff_idx] = 255;
            for start in 0..=diff_idx {
                let same_bytes = count_same_bytes(&first, &mut start.clone(), &second, start);
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
            crate::block::decompress::decompress_internal::<_, true>(
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

        // uncompressible
        let out = compress(&aaas[..12]);
        assert_gt!(out.len(), 12);
        // compressible
        let out = compress(&aaas[..13]);
        assert_le!(out.len(), 13);
        let out = compress(&aaas[..14]);
        assert_le!(out.len(), 14);
        let out = compress(&aaas[..15]);
        assert_le!(out.len(), 15);

        // dict uncompressible
        let out = compress_with_dict(&aaas[..11], aaas);
        assert_gt!(out.len(), 11);
        // compressible
        let out = compress_with_dict(&aaas[..12], aaas);
        // According to the spec this _could_ compres, but it doesn't in this lib
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
