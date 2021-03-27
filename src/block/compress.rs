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
use alloc::vec::Vec;

#[cfg(feature = "safe-encode")]
use core::convert::TryInto;

/// Increase step size after 1<<INCREASE_STEPSIZE_BITSHIFT non matches
const INCREASE_STEPSIZE_BITSHIFT: usize = 5;

/// hashes and right shifts to a maximum value of 16bit, 65535
/// The right shift is done in order to not exceed, the hashtables capacity
fn hash(sequence: u32) -> u32 {
    (sequence.wrapping_mul(2654435761_u32)) >> 16
}

/// hashes and right shifts to a maximum value of 16bit, 65535
/// The right shift is done in order to not exceed, the hashtables capacity
#[cfg(target_pointer_width = "64")]
fn hash5(sequence: usize) -> u32 {
    let primebytes = if cfg!(target_endian = "little") {
        889523592379_usize
    } else {
        11400714785074694791_usize
    };
    (((sequence << 24).wrapping_mul(primebytes)) >> 48) as u32
}

/// Read a 4-byte "batch" from some position.
///
/// This will read a native-endian 4-byte integer from some position.
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn get_batch(input: &[u8], n: usize) -> u32 {
    unsafe { read_u32_ptr(input.as_ptr().add(n)) }
}

#[inline]
#[cfg(feature = "safe-encode")]
fn get_batch(input: &[u8], n: usize) -> u32 {
    let arr: &[u8; 4] = input[n..n + 4].try_into().unwrap();
    u32::from_le_bytes(*arr)
}

/// Read a 4-byte "batch" from some position.
///
/// This will read a native-endian 4-byte integer from some position.
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn get_batch_arch(input: &[u8], n: usize) -> usize {
    unsafe { read_usize_ptr(input.as_ptr().add(n)) }
}

#[inline]
#[cfg(feature = "safe-encode")]
fn get_batch_arch(input: &[u8], n: usize) -> usize {
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    let arr: &[u8; USIZE_SIZE] = input[n..n + USIZE_SIZE].try_into().unwrap();
    usize::from_le_bytes(*arr)
}

#[inline]
#[cfg(target_pointer_width = "64")]
fn get_hash_at(input: &[u8], pos: usize) -> usize {
    if input.len() < u16::MAX as usize {
        hash(get_batch(input, pos)) as usize
    } else {
        hash5(get_batch_arch(input, pos)) as usize
    }
}

#[inline]
#[cfg(target_pointer_width = "32")]
fn get_hash_at(input: &[u8], pos: usize) -> usize {
    hash(get_batch(input, pos)) as usize
}

#[inline]
fn token_from_literal(lit_len: usize) -> u8 {
    if lit_len < 0xF {
        // Since we can fit the literals length into it, there is no need for saturation.
        (lit_len as u8) << 4
    } else {
        // We were unable to fit the literals into it, so we saturate to 0xF. We will later
        // write the extensional value through LSIC encoding.
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
        // write the extensional value through LSIC encoding.
        0xF0
    };

    token |= if duplicate_length < 0xF {
        // We could fit it in.
        duplicate_length as u8
    } else {
        // We were unable to fit it in, so we default to 0xF, which will later be extended
        // by LSIC encoding.
        0xF
    };

    token
}

/// Counts the number of same bytes in two byte streams.
/// `input` is the complete input
/// `cur` is the current position in the input. it will be incremented by the number of matched bytes
/// `input_dupl` is a pointer back in the input
///
/// The function ignores the last 7bytes (END_OFFSET) in input as this should be literals.
#[inline]
#[cfg(feature = "safe-encode")]
fn count_same_bytes(input: &[u8], cur: &mut usize, source: &[u8], candidate: usize) -> usize {
    let cand_slice = &source[candidate as usize..];
    let cur_slice = &input[*cur..input.len() - END_OFFSET];

    let mut num = 0;
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    for (block1, block2) in cur_slice
        .chunks_exact(USIZE_SIZE)
        .zip(cand_slice.chunks_exact(USIZE_SIZE))
    {
        let input_block = as_usize_le(block1);
        let match_block = as_usize_le(block2);

        if input_block == match_block {
            num += USIZE_SIZE;
        } else {
            let diff = input_block ^ match_block;
            num += get_common_bytes(diff) as usize;
            break;
        }
    }

    *cur += num;
    num
}

#[inline]
#[cfg(feature = "safe-encode")]
#[cfg(target_pointer_width = "64")]
fn as_usize_le(array: &[u8]) -> usize {
    (array[0] as usize)
        | ((array[1] as usize) << 8)
        | ((array[2] as usize) << 16)
        | ((array[3] as usize) << 24)
        | ((array[4] as usize) << 32)
        | ((array[5] as usize) << 40)
        | ((array[6] as usize) << 48)
        | ((array[7] as usize) << 56)
}

#[inline]
#[cfg(feature = "safe-encode")]
#[cfg(target_pointer_width = "32")]
fn as_usize_le(array: &[u8]) -> usize {
    (array[0] as usize)
        | ((array[1] as usize) << 8)
        | ((array[2] as usize) << 16)
        | ((array[3] as usize) << 24)
}

/// Counts the number of same bytes in two byte streams.
/// `input` is the complete input
/// `cur` is the current position in the input. it will be incremented by the number of matched bytes
/// `input_dupl` is a pointer back in the input
///
/// The function ignores the last 7bytes (END_OFFSET) in input as this should be literals.
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn count_same_bytes(input: &[u8], cur: &mut usize, source: &[u8], candidate: usize) -> usize {
    let start = *cur;

    let mut source_ptr = unsafe { source.as_ptr().add(candidate) };

    // compare 4/8 bytes blocks depending on the arch
    const STEP_SIZE: usize = core::mem::size_of::<usize>();
    while *cur + STEP_SIZE + END_OFFSET < input.len() {
        let diff = read_usize_ptr(unsafe { input.as_ptr().add(*cur) }) ^ read_usize_ptr(source_ptr);

        if diff == 0 {
            *cur += STEP_SIZE;
            unsafe {
                source_ptr = source_ptr.add(STEP_SIZE);
            }
            continue;
        } else {
            *cur += get_common_bytes(diff) as usize;
            return *cur - start;
        }
    }

    // compare 4 bytes block
    #[cfg(target_pointer_width = "64")]
    {
        if *cur + 4 + END_OFFSET < input.len() {
            let diff = read_u32_ptr(unsafe { input.as_ptr().add(*cur) }) ^ read_u32_ptr(source_ptr);

            if diff == 0 {
                *cur += 4;
                return *cur - start;
            } else {
                *cur += (diff.trailing_zeros() >> 3) as usize;
                return *cur - start;
            }
        }
    }

    // compare 2 bytes block
    if *cur + 2 + END_OFFSET < input.len() {
        let diff = read_u16_ptr(unsafe { input.as_ptr().add(*cur) }) ^ read_u16_ptr(source_ptr);

        if diff == 0 {
            *cur += 2;
            return *cur - start;
        } else {
            *cur += (diff.trailing_zeros() >> 3) as usize;
            return *cur - start;
        }
    }

    if *cur + 1 + END_OFFSET < input.len()
        && unsafe { input.as_ptr().add(*cur).read() } == unsafe { source_ptr.read() }
    {
        *cur += 1;
    }

    *cur - start
}

/// Write an integer to the output in LSIC format.
#[inline]
#[cfg(feature = "safe-encode")]
fn write_integer(output: &mut Vec<u8>, mut n: usize) {
    while n >= 0xFF {
        n -= 0xFF;
        push_byte(output, 0xFF);
    }

    // Write the remaining byte.
    push_byte(output, n as u8);
}

/// Write an integer to the output in LSIC format.
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn write_integer(output: &mut Vec<u8>, mut n: usize) {
    // Write the 0xFF bytes as long as the integer is higher than said value.
    push_u32(output, 0xFFFFFFFF);
    while n >= 4 * 0xFF {
        n -= 4 * 0xFF;
        unsafe {
            output.set_len(output.len() + 4);
        }
        push_u32(output, 0xFFFFFFFF);
    }

    // Updating output len for the remainder
    unsafe {
        output.set_len(output.len() + 1 + n / 255);
    }
    let last_index = output.len() - 1;
    unsafe {
        // Write the remaining byte.
        *output.get_unchecked_mut(last_index) = (n % 255) as u8;
    };
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn push_u32(output: &mut Vec<u8>, el: u32) {
    unsafe {
        let out_ptr = output.as_mut_ptr().add(output.len());
        core::ptr::copy_nonoverlapping(el.to_le_bytes().as_ptr(), out_ptr, 4);
    }
}

/// Handle the last bytes from the input as literals
#[cold]
fn handle_last_literals(
    output: &mut Vec<u8>,
    input: &[u8],
    input_size: usize,
    start: usize,
) -> usize {
    let lit_len = input_size - start;

    let token = token_from_literal(lit_len);
    push_byte(output, token);
    // output.push(token);
    if lit_len >= 0xF {
        write_integer(output, lit_len - 0xF);
    }
    // Now, write the actual literals.
    copy_literals(output, &input[start..]);
    output.len()
}

/// Moves the cursors back as long as the bytes match, to find additional bytes in a duplicate
///
#[inline]
#[cfg(feature = "safe-encode")]
pub fn backtrack_match(
    input: &[u8],
    cur: &mut usize,
    literal_start: usize,
    source: &[u8],
    candidate: &mut usize,
) {
    // TODO: It should be possible remove all bounds checks, since we are walking backwards
    while *candidate > 0 && *cur > literal_start && input[*cur - 1] == source[*candidate - 1] {
        *cur -= 1;
        *candidate -= 1;
    }
}

/// Moves the cursors back as long as the bytes match, to find additional bytes in a duplicate
///
#[inline]
#[cfg(not(feature = "safe-encode"))]
pub fn backtrack_match(
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

/// Compress all bytes of `input` into `output`.
///
/// T:HashTable is the dictionary of previously encoded sequences.
///
/// This is used to find duplicates in the stream so they are not written multiple times.
///
/// Every four bytes are hashed, and in the resulting slot their position in the input buffer
/// is placed in the dict. This way we can easily look up a candidate to back references.
#[inline]
fn compress_internal<T: HashTable>(
    input: &[u8],
    output: &mut Vec<u8>,
    dict: &mut T,
    ext_dict: &[u8],
) -> usize {
    assert!(ext_dict.len() < u16::MAX as usize);
    assert!(LZ4_MIN_LENGTH as usize > END_OFFSET);
    let end_pos_check = input.len() - MFLIMIT;
    let mut cur;
    let mut literal_start = 0;

    // According to the spec we can't start with a match,
    // except when referencing another block.
    if ext_dict.is_empty() {
        // Input too small, no compression (all literals)
        if input.len() < LZ4_MIN_LENGTH {
            return handle_last_literals(output, input, input.len(), 0);
        }

        let hash = get_hash_at(input, 0);
        dict.put_at(hash, 0);
        cur = 1;
    } else {
        cur = 0;
    }
    // let mut forward_hash = get_hash_at(input, cur, dict_bitshift);

    loop {
        // Read the next block into two sections, the literals and the duplicates.
        let mut step_size;
        let mut candidate;
        let mut candidate_source;
        let mut offset;
        let mut non_match_count = 1 << INCREASE_STEPSIZE_BITSHIFT;
        // The number of bytes before our cursor, where the duplicate starts.
        let mut next_cur = cur;

        // In this loop we search for duplicates via the hashtable. 4bytes or 8bytes are hashed and compared.
        loop {
            non_match_count += 1;
            step_size = non_match_count >> INCREASE_STEPSIZE_BITSHIFT;

            cur = next_cur;
            next_cur += step_size;

            if cur > end_pos_check {
                return handle_last_literals(output, input, input.len(), literal_start);
            }
            // Find a candidate in the dictionary with the hash of the current four bytes.
            // Unchecked is safe as long as the values from the hash function don't exceed the size of the table.
            // This is ensured by right shifting the hash values (`dict_bitshift`) to fit them in the table
            let hash = get_hash_at(input, cur);
            candidate = dict.get_at(hash);
            dict.put_at(hash, cur + ext_dict.len());

            // Two requirements to the candidate exists:
            // - We should not return a position which is merely a hash collision, so w that the
            //   candidate actually matches what we search for.
            // - We can address up to 16-bit offset, hence we are only able to address the candidate if
            //   its offset is less than or equals to 0xFFFF.
            if candidate + MAX_DISTANCE < cur {
                continue;
            }

            if candidate >= ext_dict.len() {
                candidate -= ext_dict.len();
                candidate_source = input;
                offset = (cur - candidate) as u16;
            } else {
                candidate_source = ext_dict;
                offset = (ext_dict.len() + cur - candidate) as u16;
            }

            if get_batch(candidate_source, candidate) == get_batch(input, cur) {
                break;
            }
        }

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

        let duplicate_length =
            count_same_bytes(input, &mut cur, candidate_source, candidate + MINMATCH);

        let hash = get_hash_at(input, cur - 2);
        dict.put_at(hash, cur - 2 + ext_dict.len());

        let token = token_from_literal_and_match_length(lit_len, duplicate_length);
        // Push the token to the output stream.
        push_byte(output, token);
        // If we were unable to fit the literals length into the token, write the extensional
        // part through LSIC.
        if lit_len >= 0xF {
            write_integer(output, lit_len - 0xF);
        }

        // Now, write the actual literals.
        //
        // The unsafe version copies blocks of 8bytes, and therefore may copy up to 7bytes more than needed.
        // This is safe, because the last 16 bytes (MF_LIMIT) are handled in handle_last_literals.
        copy_literals_wild(output, &input, literal_start, lit_len);
        // write the offset in little endian.
        push_u16(output, offset);

        // If we were unable to fit the duplicates length into the token, write the
        // extensional part through LSIC.
        if duplicate_length >= 0xF {
            write_integer(output, duplicate_length - 0xF);
        }
        literal_start = cur;
        // forward_hash = get_hash_at(input, cur, dict_bitshift);
    }
}

#[inline]
#[cfg(feature = "safe-encode")]
fn push_byte(output: &mut Vec<u8>, el: u8) {
    output.push(el);
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn push_byte(output: &mut Vec<u8>, el: u8) {
    unsafe {
        core::ptr::write(output.as_mut_ptr().add(output.len()), el);
        output.set_len(output.len() + 1);
    }
}

#[inline]
#[cfg(feature = "safe-encode")]
fn push_u16(output: &mut Vec<u8>, el: u16) {
    output.extend_from_slice(&el.to_le_bytes());
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn push_u16(output: &mut Vec<u8>, el: u16) {
    unsafe {
        let out_ptr = output.as_mut_ptr().add(output.len());
        core::ptr::write(out_ptr, el as u8);
        core::ptr::write(out_ptr.add(1), (el >> 8) as u8);
        output.set_len(output.len() + 2);
    }
}

#[inline]
fn copy_literals(output: &mut Vec<u8>, input: &[u8]) {
    output.extend_from_slice(input);
}

#[inline]
#[cfg(feature = "safe-encode")]
fn copy_literals_wild(output: &mut Vec<u8>, input: &[u8], input_start: usize, len: usize) {
    output.extend_from_slice(&input[input_start..input_start + len]);
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn copy_literals_wild(output: &mut Vec<u8>, input: &[u8], input_start: usize, len: usize) {
    use crate::block::wild_copy_from_src_8;
    unsafe {
        wild_copy_from_src_8(
            input.as_ptr().add(input_start),
            output.as_mut_ptr().add(output.len()),
            len,
        );
        output.set_len(output.len() + len);
    }
}

/// Returns the maximum output size of the compressed data.
/// Can be used to preallocate capacity on the output vector
pub fn get_maximum_output_size(input_len: usize) -> usize {
    16 + 4 + (input_len as f64 * 1.1) as usize
}

/// Compress all bytes of `input` into `output`.
/// The method chooses an appropriate hashtable to lookup duplicates and calls `compress_into_with_table`
///
/// The method will reserve the required space on the output vec.
#[inline]
pub fn compress_into(input: &[u8], compressed: &mut Vec<u8>) {
    compress_into_with_dict(input, compressed, b"")
}

#[inline]
pub fn compress_into_with_dict(input: &[u8], compressed: &mut Vec<u8>, dict_data: &[u8]) {
    compressed.reserve(get_maximum_output_size(input.len()));
    let (dict_size, dict_bitshift) = get_table_size(input.len());
    if dict_data.len() + input.len() < u16::MAX as usize {
        let mut dict = HashTableU16::new(dict_size, dict_bitshift);
        init_dict(&mut dict, dict_data);
        compress_internal(input, compressed, &mut dict, dict_data);
    } else if dict_data.len() + input.len() < u32::MAX as usize {
        let mut dict = HashTableU32::new(dict_size, dict_bitshift);
        init_dict(&mut dict, dict_data);
        compress_internal(input, compressed, &mut dict, dict_data);
    } else {
        let mut dict = HashTableUsize::new(dict_size, dict_bitshift);
        init_dict(&mut dict, dict_data);
        compress_internal(input, compressed, &mut dict, dict_data);
    }
}

#[inline]
fn init_dict<T: HashTable>(dict: &mut T, dict_data: &[u8]) {
    let mut i = 0usize;
    while i + core::mem::size_of::<usize>() <= dict_data.len() {
        let hash = get_hash_at(dict_data, i);
        dict.put_at(hash, i);
        i += 3;
    }
}

/// Compress all bytes of `input` into `output`. The uncompressed size will be prepended as litte endian.
/// Can be used in conjuction with `decompress_size_prepended`
#[inline]
pub fn compress_prepend_size(input: &[u8]) -> Vec<u8> {
    // In most cases, the compression won't expand the size, so we set the input size as capacity.
    let mut compressed = Vec::new();
    compressed.extend_from_slice(&[0, 0, 0, 0]);
    compress_into(input, &mut compressed);
    let size = input.len() as u32;
    compressed[0] = size as u8;
    compressed[1] = (size >> 8) as u8;
    compressed[2] = (size >> 16) as u8;
    compressed[3] = (size >> 24) as u8;
    compressed
}

/// Compress all bytes of `input`.
///
#[inline]
pub fn compress(input: &[u8]) -> Vec<u8> {
    // In most cases, the compression won't expand the size, so we set the input size as capacity.
    let mut compressed = Vec::new();
    compress_into(input, &mut compressed);
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

#[inline]
fn get_common_bytes(diff: usize) -> u32 {
    let tr_zeroes = diff.trailing_zeros();
    // right shift by 3, because we are only interested in 8 bit blocks (1 byte)
    tr_zeroes >> 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_pointer_width = "64")]
    #[cfg(not(feature = "safe-encode"))]
    fn test_get_common_bytes() {
        let num1 = read_usize_ptr([0, 0, 0, 0, 0, 0, 0, 1].as_ptr());
        let num2 = read_usize_ptr([0, 0, 0, 0, 0, 0, 0, 2].as_ptr());
        let diff = num1 ^ num2;

        assert_eq!(get_common_bytes(diff), 7);

        let num1 = read_usize_ptr([0, 0, 0, 0, 0, 0, 1, 1].as_ptr());
        let num2 = read_usize_ptr([0, 0, 0, 0, 0, 0, 0, 2].as_ptr());
        let diff = num1 ^ num2;
        assert_eq!(get_common_bytes(diff), 6);
        let num1 = read_usize_ptr([1, 0, 0, 0, 0, 0, 1, 1].as_ptr());
        let num2 = read_usize_ptr([0, 0, 0, 0, 0, 0, 0, 2].as_ptr());
        let diff = num1 ^ num2;
        assert_eq!(get_common_bytes(diff), 0);
    }

    #[test]
    #[cfg(target_pointer_width = "32")]
    #[cfg(not(feature = "safe-encode"))]
    fn test_get_common_bytes() {
        let num1 = read_usize_ptr([0, 0, 0, 1].as_ptr());
        let num2 = read_usize_ptr([0, 0, 0, 2].as_ptr());
        let diff = num1 ^ num2;

        assert_eq!(get_common_bytes(diff as usize), 3);

        let num1 = read_usize_ptr([0, 0, 1, 1].as_ptr());
        let num2 = read_usize_ptr([0, 0, 0, 2].as_ptr());
        let diff = num1 ^ num2;
        assert_eq!(get_common_bytes(diff as usize), 2);
        let num1 = read_usize_ptr([1, 0, 1, 1].as_ptr());
        let num2 = read_usize_ptr([0, 0, 0, 2].as_ptr());
        let diff = num1 ^ num2;
        assert_eq!(get_common_bytes(diff as usize), 0);
    }

    // #[test]
    // fn test_count_same_bytes() {
    //     // 8byte aligned block, zeros and ones are added because the end/offset
    //     let first:&[u8]  = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    //     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
    //     assert_eq!(count_same_bytes(first, second, &mut 0, first.len()), 16);

    //     // 4byte aligned block
    //     let first:&[u8]  = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] ;
    //     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
    //     assert_eq!(count_same_bytes(first, second, &mut 0, first.len()), 20);

    //     // 2byte aligned block
    //     let first:&[u8]  = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] ;
    //     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
    //     assert_eq!(count_same_bytes(first, second, &mut 0, first.len()), 22);

    //     // 1byte aligned block
    //     let first:&[u8]  = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] ;
    //     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
    //     assert_eq!(count_same_bytes(first, second, &mut 0, first.len()), 23);

    //     // 1byte aligned block - last byte different
    //     let first:&[u8]  = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] ;
    //     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
    //     assert_eq!(count_same_bytes(first, second, &mut 0, first.len()), 22);

    //     // 1byte aligned block
    //     let first:&[u8]  = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 9, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] ;
    //     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
    //     assert_eq!(count_same_bytes(first, second, &mut 0, first.len()), 21);
    // }

    #[test]
    fn test_bug() {
        let input: &[u8] = &[
            10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
        ];
        let _out = compress(&input);
    }

    #[cfg(feature = "safe-decode")]
    #[test]
    fn test_dict() {
        let input: &[u8] = &[
            10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
        ];
        let mut out = Vec::new();
        compress_into_with_dict(&input, &mut out, &input);
        let mut trip = Vec::new();
        crate::block::decompress::decompress_into_with_dict(&out, &mut trip, &input).unwrap();
        assert_eq!(input, trip);
        assert!(out.len() < compress(input).len());
    }

    // From the spec:
    // The last match must start at least 12 bytes before the end of block.
    // The last match is part of the penultimate sequence. It is followed by the last sequence, which contains only literals.
    // Note that, as a consequence, an independent block < 13 bytes cannot be compressed, because the match must copy "something",
    // so it needs at least one prior byte.
    // When a block can reference data from another block, it can start immediately with a match and no literal,
    // so a block of 12 bytes can be compressed.
    #[test]
    fn test_conformant_last_block() {
        let _12a: &[u8] = b"aaaaaaaaaaaa";
        let _13a: &[u8] = b"aaaaaaaaaaaaa";
        let _13b: &[u8] = b"bbbbbbbbbbbbb";

        let out = compress(&_12a);
        assert!(out.len() > 12);
        let out = compress(&_13b);
        assert!(out.len() < 13);

        let mut out = Vec::new();
        compress_into_with_dict(&_12a, &mut out, &_13b);
        assert!(out.len() > 12);

        let mut out = Vec::new();
        compress_into_with_dict(&_13a, &mut out, &_13b);
        assert!(out.len() < 13);

        let mut out = Vec::new();
        compress_into_with_dict(&_13a, &mut out, &_12a);
        assert!(out.len() < 13);

        // According to the spec this _could_ compres, but it doesn't in this lib
        // as it aborts compress for any input len < LZ4_MIN_LENGTH
        // let mut out = Vec::new();
        // compress_into_with_dict(&_12a, &mut out, &_12a);
        // assert!(out.len() < 12);
    }
}
