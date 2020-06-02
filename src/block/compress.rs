//! The compression algorithm.
//!
//! We make use of hash tables to find duplicates. This gives a reasonable compression ratio with a
//! high performance. It has fixed memory usage, which contrary to other approachs, makes it less
//! memory hungry.
use crate::block::hash;
use crate::block::END_OFFSET;
use crate::block::LZ4_MIN_LENGTH;
use crate::block::MAX_DISTANCE;
use crate::block::MFLIMIT;
use crate::block::MINMATCH;

/// Increase step size after 1<<4 non matches
const INCREASE_STEPSIZE_BITSHIFT: usize = 4;

/// Duplication dictionary size.
///
/// Every four bytes is assigned an entry. When this number is lower, fewer entries exists, and
/// thus collisions are more likely, hurting the compression ratio.

/// Read a 4-byte "batch" from some position.
///
/// This will read a native-endian 4-byte integer from some position.
#[inline]
fn get_batch(input: *const u8, n: usize) -> u32 {
    let mut batch: u32 = 0;
    unsafe {
        std::ptr::copy_nonoverlapping(input.add(n), &mut batch as *mut u32 as *mut u8, 4);
    }
    batch
}

#[inline]
fn get_hash_at(input: *const u8, pos: usize, dict_bitshift: u8) -> usize {
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

/// Read the batch at the cursor.
#[inline]
unsafe fn count_same_bytes(
    first: *const u8,
    mut second: *const u8,
    cur: &mut usize,
    input_size: usize,
) -> usize {
    let start = *cur;

    // compare 4/8 bytes blocks depending on the arch
    const STEP_SIZE: usize = std::mem::size_of::<usize>();
    while *cur + STEP_SIZE + END_OFFSET < input_size {
        let diff = read_usize_ptr(first.add(*cur)) ^ read_usize_ptr(second);

        if diff == 0 {
            *cur += STEP_SIZE;
            second = second.add(STEP_SIZE);
            continue;
        } else {
            *cur += get_common_bytes(diff) as usize;
            return *cur - start;
        }
    }

    // compare 4 bytes block
    #[cfg(target_pointer_width = "64")]
    {
        if *cur + 4 + END_OFFSET < input_size {
            let diff = read_u32_ptr(first.add(*cur)) ^ read_u32_ptr(second);

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
    if *cur + 2 + END_OFFSET < input_size {
        let diff = read_u16_ptr(first.add(*cur)) ^ read_u16_ptr(second);

        if diff == 0 {
            *cur += 2;
            return *cur - start;
        } else {
            *cur += (diff.trailing_zeros() >> 3) as usize;
            return *cur - start;
        }
    }

    // TODO add end_pos_check, last 5 bytes should be literals
    if *cur + 1 + END_OFFSET < input_size && first.add(*cur).read() == second.read() {
        *cur += 1;
    }

    *cur - start
}

/// Write an integer to the output in LSIC format.
#[inline]
fn write_integer(output_ptr: &mut *mut u8, mut n: usize) -> std::io::Result<()> {
    // Write the 0xFF bytes as long as the integer is higher than said value.
    while n >= 0xFF {
        n -= 0xFF;
        unsafe {
            output_ptr.write(0xFF);
            *output_ptr = output_ptr.add(1);
        }
    }

    // Write the remaining byte.
    unsafe {
        output_ptr.write(n as u8);
        *output_ptr = output_ptr.add(1);
    }
    Ok(())
}

/// Handle the last bytes from the input as literals
#[inline]
fn handle_last_literals(
    output_ptr: &mut *mut u8,
    input: *const u8,
    input_size: usize,
    start: usize,
    out_ptr_start: *mut u8,
) -> std::io::Result<usize> {
    let lit_len = input_size - start;
    
    let token = token_from_literal(lit_len);
    push_unsafe(output_ptr, token);
    if lit_len >= 0xF {
        write_integer(output_ptr, lit_len - 0xF)?;
    }
    // Now, write the actual literals.
    unsafe {
        std::ptr::copy_nonoverlapping(input.add(start), *output_ptr, lit_len);
        *output_ptr = output_ptr.add(lit_len);
    }
    Ok(*output_ptr as usize - out_ptr_start as usize)
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
    // Shift the hash value for the dictionary to the right, so match the dictionary size.
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
    let input = input.as_ptr();
    let mut output_ptr = output.as_mut_ptr();
    let dict_bitshift = dict_bitshift;

    let out_ptr_start = output_ptr;
    // Input too small, no compression (all literals)
    if input_size < LZ4_MIN_LENGTH as usize {
        // The length (in bytes) of the literals section.
        let lit_len = input_size;
        let token = token_from_literal(lit_len);
        push_unsafe(&mut output_ptr, token);
        // output.push(token);
        if lit_len >= 0xF {
            write_integer(&mut output_ptr, lit_len - 0xF)?;
        }

        // Now, write the actual literals.
        // output.extend_from_slice(&input);
        copy_into_vec(&mut output_ptr, input, input_size);
        return Ok(output_ptr as usize - out_ptr_start as usize);
    }

    let hash = get_hash_at(input, 0, dict_bitshift);
    unsafe { *dict.get_unchecked_mut(hash) = 0 };

    let end_pos_check = input_size - MFLIMIT as usize;
    let mut candidate;
    let mut cur = 0;
    let mut start = cur;

    cur += 1;
    // let mut forward_hash = get_hash_at(input, cur, dict_bitshift);

    loop {
        // Read the next block into two sections, the literals and the duplicates.
        let mut step_size;
        let mut non_match_count = 1 << INCREASE_STEPSIZE_BITSHIFT;
        // The number of bytes before our cursor, where the duplicate starts.
        let mut next_cur = cur;

        while {
            non_match_count += 1;
            step_size = non_match_count >> INCREASE_STEPSIZE_BITSHIFT;

            cur = next_cur;
            next_cur += step_size;

            if cur > end_pos_check {
                return handle_last_literals(
                    &mut output_ptr,
                    input,
                    input_size,
                    start,
                    out_ptr_start,
                );
            }
            // Find a candidate in the dictionary with the hash of the current four bytes.
            // Unchecked is safe as long as the values from the hash function don't exceed the size of the table.
            // This is ensured by right shifting the hash values (`dict_bitshift`) to fit them in the table
            let hash = get_hash_at(input, cur, dict_bitshift);
            candidate = unsafe { *dict.get_unchecked(hash) };
            unsafe { *dict.get_unchecked_mut(hash) = cur };
            

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
        let duplicate_length = unsafe {
            count_same_bytes(input, input.add(candidate + MINMATCH), &mut cur, input_size)
        };

        let hash = get_hash_at(input, cur - 2, dict_bitshift);
        unsafe { *dict.get_unchecked_mut(hash) = cur - 2 };

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
        push_unsafe(&mut output_ptr, token);
        // If we were unable to fit the literals length into the token, write the extensional
        // part through LSIC.
        if lit_len >= 0xF {
            write_integer(&mut output_ptr, lit_len - 0xF)?;
        }

        // Now, write the actual literals.
        unsafe {
            // TODO check wildcopy 8byte
            std::ptr::copy_nonoverlapping(input.add(start), output_ptr, lit_len);
            output_ptr = output_ptr.add(lit_len);
        }

        // write the offset in little endian.
        unsafe {
            std::ptr::copy_nonoverlapping(
                &offset.to_le() as *const u16 as *const u8,
                output_ptr,
                2,
            );
            output_ptr = output_ptr.add(2);
        }
        // If we were unable to fit the duplicates length into the token, write the
        // extensional part through LSIC.
        if duplicate_length >= 0xF {
            write_integer(&mut output_ptr, duplicate_length - 0xF)?;
        }
        start = cur;
        // forward_hash = get_hash_at(input, cur, dict_bitshift);
    }
}

#[inline]
fn push_unsafe(output: &mut *mut u8, el: u8) {
    unsafe {
        std::ptr::write(*output, el);
        *output = output.add(1);
    }
}

/// Compress all bytes of `input`.
#[inline]
pub fn compress(input: &[u8]) -> Vec<u8> {
    // In most cases, the compression won't expand the size, so we set the input size as capacity.
    let mut vec = Vec::with_capacity(16 + (input.len() as f64 * 1.1) as usize);

    let bytes_written = compress_into(input, &mut vec).unwrap();
    unsafe {
        vec.set_len(bytes_written);
    }
    vec
}

#[inline]
fn copy_into_vec(out_ptr: &mut *mut u8, start: *const u8, num_items: usize) {
    // vec.reserve(num_items);
    unsafe {
        std::ptr::copy_nonoverlapping(start, *out_ptr, num_items);
        *out_ptr = out_ptr.add(num_items);
        // vec.set_len(vec.len() + num_items);
    }
}
// fn copy_into_vec(vec: &mut Vec<u8>, start: *const u8, num_items: usize) {
//     // vec.reserve(num_items);
//     unsafe {
//         std::ptr::copy_nonoverlapping(start, vec.as_mut_ptr().add(vec.len()), num_items);
//         vec.set_len(vec.len() + num_items);
//     }
// }

// fn read_u64_ptr(input: *const u8) -> usize {
//     let mut num:usize = 0;
//     unsafe{std::ptr::copy_nonoverlapping(input, &mut num as *mut usize as *mut u8, 8);}
//     num
// }
#[inline]
fn read_u32_ptr(input: *const u8) -> u32 {
    let mut num: u32 = 0;
    unsafe {
        std::ptr::copy_nonoverlapping(input, &mut num as *mut u32 as *mut u8, 4);
    }
    num
}
#[inline]
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
//     // 8byte aligned block
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4];
//     assert_eq!(count_same_bytes(&first, &second), 16);

//     // 4byte aligned block
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4];
//     assert_eq!(count_same_bytes(&first, &second), 20);

//     // 2byte aligned block
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4];
//     assert_eq!(count_same_bytes(&first, &second), 22);

//     // 1byte aligned block
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5];
//     assert_eq!(count_same_bytes(&first, &second), 23);

//     // 1byte aligned block - last byte different
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6];
//     assert_eq!(count_same_bytes(&first, &second), 22);

//     // 1byte aligned block
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 9, 5] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6];
//     assert_eq!(count_same_bytes(&first, &second), 21);
// }

// #[test]
// fn yoops() {
//     const COMPRESSION66K: &'static [u8] = include_bytes!("../../benches/compression_66k_JSON.txt");
// }

#[test]
fn test_bug() {
    let input: &[u8] = &[
        10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
    ];
    let out = compress(&input);
    dbg!(&out);
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
