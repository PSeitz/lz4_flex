//! The compression algorithm.
//!
//! We make use of hash tables to find duplicates. This gives a reasonable compression ratio with a
//! high performance. It has fixed memory usage, which contrary to other approachs, makes it less
//! memory hungry.

use crate::block::END_OFFSET;
use crate::block::LZ4_MIN_LENGTH;
use crate::block::MAX_DISTANCE;
use crate::block::MFLIMIT;
use crate::block::MINMATCH;

#[cfg(feature = "safe-encode")]
use std::convert::TryInto;

/// Increase step size after 1<<4 non matches
const INCREASE_STEPSIZE_BITSHIFT: usize = 4;

/// hashes and right shifts to a maximum value of 16bit, 65535
pub fn hash(sequence: u32) -> u32 {
    (sequence.wrapping_mul(2654435761_u32)) >> 16
}

/// Duplication dictionary size.
///
/// Every four bytes is assigned an entry. When this number is lower, fewer entries exists, and
/// thus collisions are more likely, hurting the compression ratio.

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
    as_u32_le(arr)
}

#[inline]
fn get_hash_at(input: &[u8], pos: usize, dict_bitshift: u8) -> usize {
    hash(get_batch(input, pos)) as usize >> dict_bitshift
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

/// Counts the number of same bytes in two byte streams.
#[inline]
#[cfg(feature = "safe-encode")]
fn count_same_bytes(first: &[u8], second: &[u8], cur: &mut usize) -> usize {
    let cur_slice = &first[*cur..first.len() - END_OFFSET];

    let mut num = 0;

    for (block1, block2) in cur_slice.chunks_exact(8).zip(second.chunks_exact(8)) {
        let input_block = usize::from_le(as_usize_le(block1));
        let match_block = usize::from_le(as_usize_le(block2));

        if input_block == match_block {
            num += 8;
        } else {
            let diff = input_block ^ match_block;
            num += get_common_bytes(diff) as usize;
            break;
        }
    }

    *cur += num;
    return num;
}

#[inline]
#[cfg(feature = "safe-encode")]
fn as_usize_le(array: &[u8]) -> usize {
    ((array[0] as usize) << 0)
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
fn as_u32_le(array: &[u8; 4]) -> u32 {
    ((array[0] as u32) << 0)
        | ((array[1] as u32) << 8)
        | ((array[2] as u32) << 16)
        | ((array[3] as u32) << 24)
}

/// Counts the number of same bytes in two byte streams.
/// Counts the number of same bytes in two byte streams.
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn count_same_bytes(first: &[u8], mut second: &[u8], cur: &mut usize) -> usize {
    let start = *cur;

    // compare 4/8 bytes blocks depending on the arch
    const STEP_SIZE: usize = std::mem::size_of::<usize>();
    while *cur + STEP_SIZE + END_OFFSET < first.len() {
        let diff =
            read_usize_ptr(unsafe { first.as_ptr().add(*cur) }) ^ read_usize_ptr(second.as_ptr());

        if diff == 0 {
            *cur += STEP_SIZE;
            second = &second[STEP_SIZE..];
            continue;
        } else {
            *cur += get_common_bytes(diff) as usize;
            return *cur - start;
        }
    }

    // compare 4 bytes block
    #[cfg(target_pointer_width = "64")]
    {
        if *cur + 4 + END_OFFSET < first.len() {
            let diff =
                read_u32_ptr(unsafe { first.as_ptr().add(*cur) }) ^ read_u32_ptr(second.as_ptr());

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
    if *cur + 2 + END_OFFSET < first.len() {
        let diff =
            read_u16_ptr(unsafe { first.as_ptr().add(*cur) }) ^ read_u16_ptr(second.as_ptr());

        if diff == 0 {
            *cur += 2;
            return *cur - start;
        } else {
            *cur += (diff.trailing_zeros() >> 3) as usize;
            return *cur - start;
        }
    }

    // TODO add end_pos_check, last 5 bytes should be literals
    if *cur + 1 + END_OFFSET < first.len()
        && unsafe { first.as_ptr().add(*cur).read() } == unsafe { second.as_ptr().read() }
    {
        *cur += 1;
    }

    *cur - start
}

/// Write an integer to the output in LSIC format.
#[inline]
fn write_integer(output: &mut Vec<u8>, mut n: usize) -> std::io::Result<()> {
    // Write the 0xFF bytes as long as the integer is higher than said value.
    while n >= 0xFF {
        n -= 0xFF;
        push_byte(output, 0xFF);
    }

    // Write the remaining byte.
    push_byte(output, n as u8);
    Ok(())
}

/// Handle the last bytes from the input as literals
#[inline]
fn handle_last_literals(
    output: &mut Vec<u8>,
    input: &[u8],
    input_size: usize,
    start: usize,
) -> std::io::Result<usize> {
    let lit_len = input_size - start;

    let token = token_from_literal(lit_len);
    push_byte(output, token);
    // output.push(token);
    if lit_len >= 0xF {
        write_integer(output, lit_len - 0xF)?;
    }
    // Now, write the actual literals.
    copy_literals(output, &input[start..]);
    Ok(output.len())
}

/// Compress all bytes of `input` into `output`.
#[inline]
pub fn compress_into(input: &[u8], output: &mut Vec<u8>) -> std::io::Result<usize> {
    // TODO check dictionary sizes for input input_sizes
    // The dictionary of previously encoded sequences.
    //
    // This is used to find duplicates in the stream so they are not written multiple times.
    //
    // Every four bytes are hashed, and in the resulting slot their position in the input buffer
    // is placed. This way we can easily look up a candidate to back references.
    // dict_bitshift
    // Shift the hash value for the dictionary to the right, to match the dictionary size.
    let (dict_size, dict_bitshift) = match input.len() {
        0..=500 => (128, 9),
        501..=1_000 => (256, 8),
        1_001..=4_000 => (512, 7),
        4_001..=8_000 => (1024, 6),
        8_001..=16_000 => (2048, 5),
        16_001..=30_000 => (8192, 3),
        // 100_000..=400_000 => (8192, 3),
        _ => (16384, 2),
    };
    let mut dict = vec![0; dict_size];

    let input_size = input.len();
    // let input = input.as_ptr();
    // let mut output_ptr = output.as_mut_ptr();
    let dict_bitshift = dict_bitshift;

    // let out_ptr_start = output_ptr;
    // Input too small, no compression (all literals)
    if input_size < LZ4_MIN_LENGTH as usize {
        // The length (in bytes) of the literals section.
        let lit_len = input_size;
        let token = token_from_literal(lit_len);
        push_byte(output, token);
        // output.push(token);
        if lit_len >= 0xF {
            write_integer(output, lit_len - 0xF)?;
        }

        // Now, write the actual literals.
        copy_literals(output, &input);
        return Ok(output.len());
    }

    let hash = get_hash_at(input, 0, dict_bitshift);
    dict[hash] = 0;

    assert!(LZ4_MIN_LENGTH as usize > END_OFFSET);
    // let input = &input[..input.len() - END_OFFSET];

    let end_pos_check = input_size - MFLIMIT as usize;
    
    let mut cur = 0;
    let mut start = cur;
    cur += 1;
    // let mut forward_hash = get_hash_at(input, cur, dict_bitshift);

    loop {
        // Read the next block into two sections, the literals and the duplicates.
        let mut step_size;
        let mut candidate;
        let mut non_match_count = 1 << INCREASE_STEPSIZE_BITSHIFT;
        // The number of bytes before our cursor, where the duplicate starts.
        let mut next_cur = cur;

        while {
            non_match_count += 1;
            step_size = non_match_count >> INCREASE_STEPSIZE_BITSHIFT;

            cur = next_cur;
            next_cur += step_size;

            if cur > end_pos_check {
                return handle_last_literals(output, input, input_size, start);
            }
            // Find a candidate in the dictionary with the hash of the current four bytes.
            // Unchecked is safe as long as the values from the hash function don't exceed the size of the table.
            // This is ensured by right shifting the hash values (`dict_bitshift`) to fit them in the table
            let hash = get_hash_at(input, cur, dict_bitshift);
            candidate = dict[hash];
            dict[hash] = cur;

            // Two requirements to the candidate exists:
            // - We should not return a position which is merely a hash collision, so w that the
            //   candidate actually matches what we search for.
            // - We can address up to 16-bit offset, hence we are only able to address the candidate if
            //   its offset is less than or equals to 0xFFFF.
            (candidate + MAX_DISTANCE) < cur || get_batch(input, candidate) != get_batch(input, cur)
        } {}

        // The length (in bytes) of the literals section.
        let lit_len = cur - start;

        // Generate the higher half of the token.
        let mut token = token_from_literal(lit_len);

        let offset = (cur - candidate) as u16;
        cur += MINMATCH;
        let duplicate_length = count_same_bytes(input, &input[candidate + MINMATCH..], &mut cur);
        let hash = get_hash_at(input, cur - 2, dict_bitshift);
        dict[hash] = cur - 2;

        // Generate the lower half of the token, the duplicates length.
        // cur += duplicate_length + MINMATCH;
        token |= if duplicate_length < 0xF {
            // We could fit it in.
            duplicate_length as u8
        } else {
            // We were unable to fit it in, so we default to 0xF, which will later be extended
            // by LSIC encoding.
            0xF
        };

        // Push the token to the output stream.
        push_byte(output, token);
        // output.push(token);
        // If we were unable to fit the literals length into the token, write the extensional
        // part through LSIC.
        if lit_len >= 0xF {
            write_integer(output, lit_len - 0xF)?;
        }

        // Now, write the actual literals.
        // TODO check wildcopy 8byte
        copy_literals(output, &input[start..start + lit_len]);
        // write the offset in little endian.
        push_byte(output, offset as u8);
        push_byte(output, (offset >> 8) as u8);

        // If we were unable to fit the duplicates length into the token, write the
        // extensional part through LSIC.
        if duplicate_length >= 0xF {
            write_integer(output, duplicate_length - 0xF)?;
        }
        start = cur;
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
        std::ptr::write(output.as_mut_ptr().add(output.len()), el);
        output.set_len(output.len() + 1);
    }
}

#[inline]
fn copy_literals(output: &mut Vec<u8>, input: &[u8]) {
    output.extend_from_slice(input);
}

/// Compress all bytes of `input` into `output`. The uncompressed size will be prepended as litte endian.
/// Can be used in conjuction with `decompress_size_prepended`
#[inline]
pub fn compress_prepend_size(input: &[u8]) -> Vec<u8> {
    // In most cases, the compression won't expand the size, so we set the input size as capacity.
    let mut compressed = Vec::with_capacity(16 + 4 + (input.len() as f64 * 1.1) as usize);
    compressed.extend_from_slice(&[0, 0, 0, 0]);
    compress_into(input, &mut compressed).unwrap();
    let size = input.len() as u32;
    compressed[0] = size as u8;
    compressed[1] = (size >> 8) as u8;
    compressed[2] = (size >> 16) as u8;
    compressed[3] = (size >> 24) as u8;
    compressed
}

/// Compress all bytes of `input`.
#[inline]
pub fn compress(input: &[u8]) -> Vec<u8> {
    // In most cases, the compression won't expand the size, so we set the input size as capacity.
    let mut compressed = Vec::with_capacity(16 + (input.len() as f64 * 1.1) as usize);

    compress_into(input, &mut compressed).unwrap();
    compressed
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn read_u32_ptr(input: *const u8) -> u32 {
    let mut num: u32 = 0;
    unsafe {
        std::ptr::copy_nonoverlapping(input, &mut num as *mut u32 as *mut u8, 4);
    }
    num
}

#[inline]
#[cfg(not(feature = "safe-encode"))]
fn read_usize_ptr(input: *const u8) -> usize {
    let mut num: usize = 0;
    unsafe {
        std::ptr::copy_nonoverlapping(
            input,
            &mut num as *mut usize as *mut u8,
            std::mem::size_of::<usize>(),
        );
    }
    num
}
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn read_u16_ptr(input: *const u8) -> u16 {
    let mut num: u16 = 0;
    unsafe {
        std::ptr::copy_nonoverlapping(input, &mut num as *mut u16 as *mut u8, 2);
    }
    num
}

#[inline]
fn get_common_bytes(diff: usize) -> u32 {
    let tr_zeroes = diff.trailing_zeros();
    // right shift by 3, because we are only interested in 8 bit blocks (1 byte)
    tr_zeroes >> 3
}

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
fn test_get_common_bytes() {
    let num1 = read_u32(&[0, 0, 0, 1]);
    let num2 = read_u32(&[0, 0, 0, 2]);
    let diff = num1 ^ num2;

    assert_eq!(get_common_bytes(diff as usize), 3);

    let num1 = read_u32(&[0, 0, 1, 1]);
    let num2 = read_u32(&[0, 0, 0, 2]);
    let diff = num1 ^ num2;
    assert_eq!(get_common_bytes(diff as usize), 2);
    let num1 = read_u32(&[1, 0, 1, 1]);
    let num2 = read_u32(&[0, 0, 0, 2]);
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

#[test]
fn test_compare() {
    let mut input: &[u8] = &[10, 12, 14, 16];

    let mut cache = vec![];
    let mut encoder = lz4::EncoderBuilder::new()
        .level(2)
        .build(&mut cache)
        .unwrap();
    // let mut read = *input;
    std::io::copy(&mut input, &mut encoder).unwrap();
    let (comp_lz4, _result) = encoder.finish();

    println!("{:?}", comp_lz4);

    let input: &[u8] = &[10, 12, 14, 16];
    let out = compress(&input);
    dbg!(&out);
}
