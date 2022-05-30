extern crate lz4_flex;

use lz4_flex::block::DecompressError;

const COMPRESSION10MB: &[u8] = include_bytes!("../../benches/dickens.txt");
// const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/compression_66k_JSON.txt");

fn main() {
    let compressed = lz4_flex::compress(COMPRESSION10MB as &[u8]);
    for _ in 0..30 {
        decompress(&compressed, COMPRESSION10MB.len()).unwrap();
    }
}

#[inline]
fn duplicate(output_ptr: &mut *mut u8, start: *const u8, match_length: usize) {
    // We cannot simply use memcpy or `extend_from_slice`, because these do not allow
    // self-referential copies: http://ticki.github.io/img/lz4_runs_encoding_diagram.svg
    // `reserve` enough space on the vector to safely copy self referential data.
    // Check overlap copy
    if (*output_ptr as usize) <= unsafe { start.add(match_length) } as usize {
        duplicate_overlapping(output_ptr, start, match_length);
    } else {
        copy_on_self(output_ptr, start, match_length);
    }
}

/// Copy function, if the data start + match_length overlaps into output_ptr
#[inline]
fn duplicate_overlapping(output_ptr: &mut *mut u8, mut start: *const u8, match_length: usize) {
    for _ in 0..match_length {
        unsafe {
            let curr = start.read();
            output_ptr.write(curr);
            *output_ptr = output_ptr.add(1);
            start = start.add(1);
        }
    }
}

/// Read an integer.
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

        #[cfg(feature = "checked-decode")]
        {
            if input.len() < *input_pos + 1 {
                return Err(DecompressError::ExpectedAnotherByte);
            };
        }
        // check alread done in move_cursor
        let extra = *unsafe { input.get_unchecked(*input_pos) };
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
    let mut num: u16 = 0;
    unsafe {
        core::ptr::copy_nonoverlapping(
            input.as_ptr().add(*input_pos),
            &mut num as *mut u16 as *mut u8,
            2,
        );
    }

    *input_pos += 2;
    u16::from_le(num)
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
/// if the literal length and match_length are both below 15, we don't need to read additional data, so the token does fit the metadata in a single u8.
#[inline]
fn does_token_fit(token: u8) -> bool {
    !((token & FIT_TOKEN_MASK_LITERAL) == FIT_TOKEN_MASK_LITERAL
        || (token & FIT_TOKEN_MASK_MATCH) == FIT_TOKEN_MASK_MATCH)
}

#[inline]
fn is_safe_distance(input_pos: usize, in_len: usize) -> bool {
    input_pos < in_len
}

#[cold]
unsafe fn copy_24(start_ptr: *const u8, output_ptr: *mut u8) {
    core::ptr::copy_nonoverlapping(start_ptr, output_ptr, 24);
}

/// We copy 24 byte blocks, because aligned copies are faster
const BLOCK_COPY_SIZE: usize = 24;

/// Decompress all bytes of `input` into `output`.
#[inline(never)]
pub fn decompress_into(input: &[u8], output: &mut Vec<u8>) -> Result<(), DecompressError> {
    // Decode into our vector.
    let mut input_pos = 0;
    let mut output_ptr = output.as_mut_ptr();

    #[cfg(feature = "checked-decode")]
    let output_start = output_ptr as usize;

    if input.is_empty() {
        return Err(DecompressError::ExpectedAnotherByte);
    }
    // Exhaust the decoder by reading and decompressing all blocks until the remaining buffer
    // is empty.
    let in_len = input.len() - 1;
    let end_pos_check = input.len().saturating_sub(18);
    loop {
        #[cfg(feature = "checked-decode")]
        {
            if input.len() < input_pos + 1 {
                return Err(DecompressError::LiteralOutOfBounds);
            };
        }

        // Read the token. The token is the first byte in a block. It is divided into two 4-bit
        // subtokens, the higher and the lower.
        // This token contains to 4-bit "fields", a higher and a lower, representing the literals'
        // length and the back reference's length, respectively.
        let token = unsafe { *input.get_unchecked(input_pos) };
        input_pos += 1;

        // Checking for hot-loop.
        // In most cases the metadata does fit in a single 1byte token (statistically) and we are in a safe-distance to the end.
        // This enables some optmized handling.
        if does_token_fit(token) && is_safe_distance(input_pos, end_pos_check) {
            let literal_length = (token >> 4) as usize;

            #[cfg(feature = "checked-decode")]
            {
                // Check if literal is out of bounds for the input, and if there is enough space on the output
                if input.len() < input_pos + literal_length {
                    return Err(DecompressError::LiteralOutOfBounds);
                };
                if output.len() < (output_ptr as usize - output_start + literal_length) {
                    return Err(DecompressError::OutputTooSmall {
                        expected_size: (output_ptr as usize - output_start + literal_length),
                        actual_size: output.len(),
                    });
                };
            }

            // Copy the literal
            // The literal is at max 14 bytes, and the is_safe_distance check assures
            // that we are far away enough from the end so we can safely copy 16 bytes
            unsafe {
                core::ptr::copy_nonoverlapping(input.as_ptr().add(input_pos), output_ptr, 16);
            };
            input_pos += literal_length;
            unsafe {
                output_ptr = output_ptr.add(literal_length);
            }

            let offset = read_u16(input, &mut input_pos);
            let start_ptr = unsafe { output_ptr.sub(offset as usize) };
            // unsafe{
            //     core::arch::x86_64::_mm_prefetch(start_ptr as *const i8, core::arch::x86_64::_MM_HINT_T0);
            // }

            let match_length = (4 + (token & 0xF)) as usize;
            // Write the duplicate segment to the output buffer from the output buffer
            // The blocks can overlap, make sure they are at least BLOCK_COPY_SIZE apart
            if (output_ptr as usize)
                < unsafe { start_ptr.add(match_length).add(BLOCK_COPY_SIZE) } as usize
            {
                duplicate_overlapping(&mut output_ptr, start_ptr, match_length);
            } else {
                unsafe {
                    // match_length is at max 14+4 = 18, so copy_24 covers only the values 17, 18 and is therefore marked as cold
                    if match_length <= 16 {
                        core::ptr::copy_nonoverlapping(start_ptr, output_ptr, 16);
                    } else {
                        copy_24(start_ptr, output_ptr)
                    }
                    output_ptr = output_ptr.add(match_length);
                };
            }

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

            #[cfg(feature = "checked-decode")]
            {
                // Check if literal is out of bounds for the input, and if there is enough space on the output
                if input.len() < input_pos + literal_length {
                    return Err(DecompressError::LiteralOutOfBounds);
                };
                if output.len() < (output_ptr as usize - output_start + literal_length) {
                    return Err(DecompressError::OutputTooSmall {
                        expected_size: (output_ptr as usize - output_start + literal_length),
                        actual_size: output.len(),
                    });
                };
            }
            unsafe {
                core::ptr::copy_nonoverlapping(
                    input.as_ptr().add(input_pos),
                    output_ptr,
                    literal_length,
                );
                output_ptr = output_ptr.add(literal_length);
            }
            input_pos += literal_length;
        }

        // If the input stream is emptied, we break out of the loop. This is only the case
        // in the end of the stream, since the block is intact otherwise.
        if in_len <= input_pos {
            break;
        }

        // Read duplicate section
        #[cfg(feature = "checked-decode")]
        {
            if input_pos + 2 >= input.len() {
                return Err(DecompressError::OffsetOutOfBounds);
            }
            if input_pos + 2 >= output.len() {
                return Err(DecompressError::OffsetOutOfBounds);
            }
        }
        let offset = read_u16(input, &mut input_pos);
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

        // Calculate the start of this duplicate segment.
        let start_ptr = unsafe { output_ptr.sub(offset as usize) };

        // We'll do a bound check to in checked-decode.
        #[cfg(feature = "checked-decode")]
        {
            if (start_ptr as usize) >= (output_ptr as usize) {
                return Err(DecompressError::OffsetOutOfBounds);
            };
        }
        duplicate(&mut output_ptr, start_ptr, match_length);
    }
    Ok(())
}

use core::convert::TryInto;
use core::ptr;

/// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in litte endian.
/// Can be used in conjuction with `compress_prepend_size`
#[inline]
pub fn decompress_size_prepended(input: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let (uncompressed_size, input) = uncompressed_size(input)?;
    // Allocate a vector to contain the decompressed stream. we may wildcopy out of bounds, so the vector needs to have ad additional BLOCK_COPY_SIZE capacity
    let mut vec = Vec::with_capacity(uncompressed_size + BLOCK_COPY_SIZE);
    unsafe {
        vec.set_len(uncompressed_size);
    }
    decompress_into(input, &mut vec)?;

    Ok(vec)
}

/// Decompress all bytes of `input` into a new vec.
#[inline]
pub fn decompress(input: &[u8], uncompressed_size: usize) -> Result<Vec<u8>, DecompressError> {
    // Allocate a vector to contain the decompressed stream. we may wildcopy out of bounds, so the vector needs to have ad additional BLOCK_COPY_SIZE capacity
    let mut vec = Vec::with_capacity(uncompressed_size + BLOCK_COPY_SIZE);
    unsafe {
        vec.set_len(uncompressed_size);
    }
    decompress_into(input, &mut vec)?;

    Ok(vec)
}

#[inline]
fn copy_on_self(out_ptr: &mut *mut u8, start: *const u8, num_items: usize) {
    unsafe {
        wild_copy_from_src_8(start, *out_ptr, num_items);
        *out_ptr = out_ptr.add(num_items);
    }
}
#[inline]
fn uncompressed_size(input: &[u8]) -> Result<(usize, &[u8]), DecompressError> {
    let size = input.get(..4).ok_or(DecompressError::ExpectedAnotherByte)?;
    let size: &[u8; 4] = size.try_into().unwrap();
    let uncompressed_size = u32::from_le_bytes(*size) as usize;
    let rest = &input[4..];
    Ok((uncompressed_size, rest))
}

#[allow(dead_code)]
fn wild_copy_from_src_8(mut source: *const u8, mut dst_ptr: *mut u8, num_items: usize) {
    unsafe {
        let dst_ptr_end = dst_ptr.add(num_items);
        while (dst_ptr as usize) < dst_ptr_end as usize {
            ptr::copy_nonoverlapping(source, dst_ptr, 8);
            source = source.add(8);
            dst_ptr = dst_ptr.add(8);
        }
    }
}
