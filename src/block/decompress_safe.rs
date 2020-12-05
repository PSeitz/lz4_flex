//! The decompression algorithm.
// use crate::block::wild_copy_from_src_8;

use crate::block::DecompressError;

/// Read an integer LSIC (linear small integer code) encoded.
///
/// In LZ4, we encode small integers in a way that we can have an arbitrary number of bytes. In
/// particular, we add the bytes repeatedly until we hit a non-0xFF byte. When we do, we add
/// this byte to our sum and terminate the loop.
///
/// # Example
///
/// ```notest
///     255, 255, 255, 4, 2, 3, 4, 6, 7
/// ```
///
/// is encoded to _255 + 255 + 255 + 4 = 769_. The bytes after the first 4 is ignored, because
/// 4 is the first non-0xFF byte.
// #[inline(never)]
#[inline]
fn read_integer(input: &[u8], input_pos: &mut usize) -> Result<u32, DecompressError> {
    // We start at zero and count upwards.
    let mut n: u32 = 0;
    // If this byte takes value 255 (the maximum value it can take), another byte is read
    // and added to the sum. This repeats until a byte lower than 255 is read.
    while {
        // We add the next byte until we get a byte which we add to the counting variable.

        // #[cfg(feature = "safe-decode")]
        // {
        //     if input.len() < *input_pos + 1 {
        //         return Err(Error::ExpectedAnotherByte);
        //     };
        // }
        let extra: u8 = input[*input_pos];
        // check alread done in move_cursor
        *input_pos += 1;
        n += extra as u32;

        // We continue if we got 255.
        extra == 0xFF
    } {}

    // 255, 255, 255, 8
    // 111, 111, 111, 101

    Ok(n)
}

/// Read a little-endian 16-bit integer from the input stream.
#[inline]
fn read_u16(input: &[u8], input_pos: &mut usize) -> u16 {
    let dst = [input[*input_pos], input[*input_pos + 1]];
    *input_pos += 2;
    u16::from_le_bytes(dst)
}

const FIT_TOKEN_MASK_LITERAL: u8 = 0b00001111;
const FIT_TOKEN_MASK_MATCH: u8 = 0b11110000;

#[test]
fn check_token() {
    assert_eq!(does_token_fit(15), false);
    assert_eq!(does_token_fit(14), true);
    assert_eq!(does_token_fit(114), true);
    assert_eq!(does_token_fit(0b11110000), false);
    assert_eq!(does_token_fit(0b10110000), true);
}

/// The token consists of two parts, the literal length (upper 4 bits) and match_length (lower 4 bits)
/// if the literal length and match_length are both below 15, we don't need to read additional data, so the token does fit the metadata.
#[inline]
fn does_token_fit(token: u8) -> bool {
    !((token & FIT_TOKEN_MASK_LITERAL) == FIT_TOKEN_MASK_LITERAL
        || (token & FIT_TOKEN_MASK_MATCH) == FIT_TOKEN_MASK_MATCH)
}

#[inline]
fn is_safe_distance(input_pos: usize, in_len: usize) -> bool {
    input_pos < in_len
}

/// We copy 24 byte blocks, because aligned copies are faster
const BLOCK_COPY_SIZE: usize = 24;

/// Decompress all bytes of `input` into `output`.
#[inline]
pub fn decompress_into(input: &[u8], output: &mut Vec<u8>) -> Result<(), DecompressError> {
    // Decode into our vector.
    let mut input_pos = 0;
    // let mut output_ptr = output.as_mut_ptr();

    // #[cfg(feature = "safe-decode")]
    // let output_start = output_ptr as usize;

    if input.is_empty() {
        return Err(DecompressError::ExpectedAnotherByte);
    }
    // Exhaust the decoder by reading and decompressing all blocks until the remaining buffer
    // is empty.
    let in_len = input.len() - 1;
    let end_pos_check = input.len().saturating_sub(18);

    loop {
        #[cfg(feature = "safe-decode")]
        {
            if input.len() < input_pos + 1 {
                return Err(DecompressError::LiteralOutOfBounds);
            };
        }

        // Read the token. The token is the first byte in a block. It is divided into two 4-bit
        // subtokens, the higher and the lower.
        // This token contains to 4-bit "fields", a higher and a lower, representing the literals'
        // length and the back reference's length, respectively. LSIC is used if either are their
        // maximal values.
        let token = input[input_pos];
        input_pos += 1;

        // Checking for hot-loop.
        // In most cases the metadata does fit in a single 1byte token (statistically) and we are in a safe-distance to the end.
        // This enables some optmized handling.
        if does_token_fit(token) && is_safe_distance(input_pos, end_pos_check) {
            let literal_length = (token >> 4) as usize;

            if input.len() < input_pos + literal_length {
                return Err(DecompressError::LiteralOutOfBounds);
            };

            // copy literal
            output.extend_from_slice(&input[input_pos..input_pos + literal_length]);
            input_pos += literal_length;

            let offset = read_u16(input, &mut input_pos) as usize;

            let match_length = (4 + (token & 0xF)) as usize;

            // Write the duplicate segment to the output buffer from the output buffer
            // The blocks can overlap, make sure they are at least BLOCK_COPY_SIZE apart
            duplicate_slice(output, offset, match_length)?;

            // unsafe{
            //     duplicate(&mut output.as_mut_ptr().add(output.len()), output.as_mut_ptr().add(output.len()).sub(offset), match_length);
            //     output.set_len(output.len() + match_length);
            // }

            continue;
        }

        // Now, we read the literals section.
        // Literal Section
        // If the initial value is 15, it is indicated that another byte will be read and added to it
        let mut literal_length = (token >> 4) as usize;
        if literal_length != 0 {
            if literal_length == 15 {
                // The literal_length length took the maximal value, indicating that there is more than 15
                // literal_length bytes. We read the extra integer.
                literal_length += read_integer(input, &mut input_pos)? as usize;
            }

            if input.len() < input_pos + literal_length {
                return Err(DecompressError::LiteralOutOfBounds);
            };
            output.extend_from_slice(&input[input_pos..input_pos + literal_length]);
            input_pos += literal_length;
        }

        // If the input stream is emptied, we break out of the loop. This is only the case
        // in the end of the stream, since the block is intact otherwise.
        if in_len <= input_pos {
            break;
        }

        // Read duplicate section
        // #[cfg(feature = "safe-decode")]
        // {
        //     if input_pos + 2 >= input.len() {
        //         return Err(DecompressError::OffsetOutOfBounds);
        //     }
        //     if input_pos + 2 >= output.len() {
        //         return Err(DecompressError::OffsetOutOfBounds);
        //     }
        // }
        let offset = read_u16(input, &mut input_pos) as usize;
        // Obtain the initial match length. The match length is the length of the duplicate segment
        // which will later be copied from data previously decompressed into the output buffer. The
        // initial length is derived from the second part of the token (the lower nibble), we read
        // earlier. Since having a match length of less than 4 would mean negative compression
        // ratio, we start at 4.
        // let mut match_length = (4 + (token & 0xF)) as usize;

        // The intial match length can maximally be 19. As with the literal length, this indicates
        // that there are more bytes to read.
        let mut match_length = (4 + (token & 0xF)) as usize;
        if match_length == 4 + 15 {
            // The match length took the maximal value, indicating that there is more bytes. We
            // read the extra integer.
            match_length += read_integer(input, &mut input_pos)? as usize;
        }

        // We now copy from the already decompressed buffer. This allows us for storing duplicates
        // by simply referencing the other location.
        duplicate_slice(output, offset, match_length)?;
    }
    Ok(())
}

/// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in litte endian.
/// Can be used in conjuction with `compress_prepend_size`
#[inline]
pub fn duplicate_slice(
    output: &mut Vec<u8>,
    offset: usize,
    match_length: usize,
) -> Result<(), DecompressError> {
    if match_length + 16 >= offset {
        // unsafe{
        //     duplicate_overlapping(&mut output.as_mut_ptr().add(output.len()), output.as_mut_ptr().add(output.len()).sub(offset), match_length);
        //     output.set_len(output.len() + match_length);
        // }
        duplicate_overlapping_slice(output, offset, match_length)?;

    // unsafe{
    //     let old_len = output.len();
    //     let mut output_ptr = output.as_mut_ptr().add(output.len());
    //     let start_ptr = output_ptr.sub(offset as usize);
    //     duplicate_overlapping(&mut output_ptr, start_ptr, match_length);
    //     output.set_len(old_len + match_length);
    // }
    } else {
        let old_len = output.len();
        let mut dst = [0u8; 16];
        for i in (output.len() - offset..output.len() - offset + match_length).step_by(16) {
            dst.clone_from_slice(&output[i..i + 16]);
            output.extend_from_slice(&dst);
        }
        // for i in (output.len() - offset..output.len() - offset + match_length).step_by(16) {
        //     let old_len = output.len();
        //     output.resize(old_len + 16, 0);
        //     let (left, right) = output.split_at_mut(old_len);
        //     right.copy_from_slice(&left[i..i+16]);
        // }

        // duplicate_slice
        // [x0, x1, x2, x3, x4, x5, x6, x7, x8, x9, x10, x11, x12, x13, x14, x15, x16, x17, x18, x19, x20]
        //     ^
        //     offset = 20
        //     match_length = 20
        //
        // step1 resize
        // [x0, x1, x2, x3, x4, x5, x6, x7, x8, x9, x10, x11, x12, x13, x14, x15, x16, x17, x18, x19, x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        //
        // step2 let (left, right) = split_at_mut(old_len)                                               ^
        // step3 copy min(16, right.len()) from left.end - offset, to right
        //
        //
        // [x0, x1, x2, x3, x4, x5, x6, x7, x8, x9, x10, x11, x12, x13, x14, x15, x16, x17, x18, x19, x20, x1, x2, x3, x4, x5, x6, x7, x8, x9, x10, x11, x12, x13, x14, x15, x16, 0, 0, 0, 0]
        //
        // move old_len by 16 and repeat                                                                                                                                        ^

        // output.resize(old_len + match_length, 0);
        // loop {
        //     let (left, right) = output.split_at_mut(old_len);
        //     let length = std::cmp::min(left.len(), std::cmp::min(right.len(), 16));
        //     right[..length].copy_from_slice(&left[left.len() - offset..left.len() - offset + length]);
        //     old_len +=16;
        //     if old_len >= output.len(){
        //         break;
        //     }
        // }

        // for i in (output.len() - offset..output.len() - offset + match_length).step_by(16) {
        //     let old_len = output.len();
        //     output.resize(old_len + 16, 0);
        //     let (left, right) = output.split_at_mut(old_len);
        //     right.copy_from_slice(&left[i..i+16]);
        // }
        output.truncate(old_len + match_length);

        // unsafe{
        //     copy_on_self(&mut output.as_mut_ptr().add(output.len()), output.as_mut_ptr().add(output.len()).sub(offset), match_length);
        //     output.set_len(output.len() + match_length);
        // }
    }
    // duplicate_overlapping_slice(output, offset, match_length)?;
    Ok(())
}

// /// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in litte endian.
// /// Can be used in conjuction with `compress_prepend_size`
// #[inline]
// pub fn duplicate_slice(output: &mut Vec<u8>, offset: usize, match_length: usize) -> Result<(), DecompressError> {
//     unsafe{
//         let mut output_ptr = output.as_mut_ptr().add(output.len());
//         let start_ptr = output_ptr.sub(offset as usize);
//         duplicate(&mut output_ptr, start_ptr, match_length);
//         output.set_len(output.len() + match_length );
//     }
//     // duplicate_overlapping_slice(output, offset, match_length)?;
//     Ok(())
// }

// #[inline]
// fn duplicate(output_ptr: &mut *mut u8, start: *const u8, match_length: usize) {
//     // We cannot simply use memcpy or `extend_from_slice`, because these do not allow
//     // self-referential copies: http://ticki.github.io/img/lz4_runs_encoding_diagram.svg
//     // `reserve` enough space on the vector to safely copy self referential data.
//     // Check overlap copy
//     if (*output_ptr as usize) < unsafe { start.add(match_length) } as usize {
//         duplicate_overlapping(output_ptr, start, match_length);
//     } else {
//         copy_on_self(output_ptr, start, match_length);
//     }
// }

// #[inline]
// fn copy_on_self(out_ptr: &mut *mut u8, start: *const u8, num_items: usize) {
//     unsafe {
//         wild_copy_from_src_8(start, *out_ptr, num_items);
//         *out_ptr = out_ptr.add(num_items);
//     }
// }

// /// Copy function, if the data start + match_length overlaps into output_ptr
// #[inline]
// fn duplicate_overlapping(output_ptr: &mut *mut u8, mut start: *const u8, match_length: usize) {
//     for _ in 0..match_length {
//         unsafe {
//             let curr = start.read();
//             output_ptr.write(curr);
//             *output_ptr = output_ptr.add(1);
//             start = start.add(1);
//         }
//     }
// }

/// Copy function, if the data start + match_length overlaps into output_ptr
#[inline]
fn duplicate_overlapping_slice(
    output: &mut Vec<u8>,
    offset: usize,
    match_length: usize,
) -> Result<(), DecompressError> {
    // let old_length = output.len();
    // for i in 0..match_length {
    //     let b = output[old_length - offset + i];
    //     output.push(b);
    // }
    if offset == 1 {
        output.resize(output.len() + match_length, output[output.len() - 1]);
        Ok(())
    } else {
        let start = output.len().wrapping_sub(offset);
        if start < output.len() {
            for i in start..start + match_length {
                let b = output[i];
                output.push(b);
            }
            Ok(())
        } else {
            Err(DecompressError::OffsetOutOfBounds)
        }
    }
}

/// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in litte endian.
/// Can be used in conjuction with `compress_prepend_size`
#[inline]
pub fn decompress_size_prepended(input: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let uncompressed_size = (input[0] as usize)
        | (input[1] as usize) << 8
        | (input[2] as usize) << 16
        | (input[3] as usize) << 24;
    // Allocate a vector to contain the decompressed stream. we may wildcopy out of bounds, so the vector needs to have ad additional BLOCK_COPY_SIZE capacity
    let mut vec = Vec::with_capacity(uncompressed_size + BLOCK_COPY_SIZE);
    // unsafe {
    //     vec.set_len(uncompressed_size);
    // }
    decompress_into(&input[4..], &mut vec)?;

    Ok(vec)
}

/// Decompress all bytes of `input` into a new vec.
#[inline]
pub fn decompress(input: &[u8], uncompressed_size: usize) -> Result<Vec<u8>, DecompressError> {
    // Allocate a vector to contain the decompressed stream. we may wildcopy out of bounds, so the vector needs to have ad additional BLOCK_COPY_SIZE capacity
    let mut vec = Vec::with_capacity(uncompressed_size + BLOCK_COPY_SIZE);
    // unsafe {
    //     vec.set_len(uncompressed_size);
    // }
    decompress_into(input, &mut vec)?;

    Ok(vec)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn all_literal() {
        assert_eq!(decompress(&[0x30, b'a', b'4', b'9'], 3).unwrap(), b"a49");
    }

    // this error test is only valid in safe-decode.
    #[cfg(feature = "safe-decode")]
    #[test]
    fn offset_oob() {
        decompress(&[0x10, b'a', 2, 0], 4).unwrap_err();
        decompress(&[0x40, b'a', 1, 0], 4).unwrap_err();
    }
}
