//! The decompression algorithm.

use crate::block::DecompressError;
use alloc::vec::Vec;

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
#[inline]
fn read_integer(input: &[u8], input_pos: &mut usize) -> u32 {
    // We start at zero and count upwards.
    let mut n: u32 = 0;
    // If this byte takes value 255 (the maximum value it can take), another byte is read
    // and added to the sum. This repeats until a byte lower than 255 is read.
    loop {
        // We add the next byte until we get a byte which we add to the counting variable.

        let extra: u8 = input[*input_pos];
        // check alread done in move_cursor
        *input_pos += 1;
        n += extra as u32;

        // We continue if we got 255, break otherwise.
        if extra != 0xFF {
            break;
        }
    }

    // 255, 255, 255, 8
    // 111, 111, 111, 101

    n
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

#[cold]
fn copy_24(output: &mut Vec<u8>, offset: usize) {
    let mut dst = [0u8; BLOCK_COPY_SIZE];
    let i = output.len() - offset;
    dst.copy_from_slice(&output[i..i + BLOCK_COPY_SIZE]);
    output.extend_from_slice(&dst);
}

/// Decompress all bytes of `input` into `output`.
#[inline]
pub fn decompress_into(input: &[u8], output: &mut Vec<u8>) -> Result<(), DecompressError> {
    decompress_into_with_dict(input, output, b"")
}

#[inline]
pub fn decompress_into_with_dict(
    input: &[u8],
    output: &mut Vec<u8>,
    ext_dict: &[u8],
) -> Result<(), DecompressError> {
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
        if input.len() < input_pos + 1 {
            return Err(DecompressError::LiteralOutOfBounds);
        };

        // Read the token. The token is the first byte in a block. It is divided into two 4-bit
        // subtokens, the higher and the lower.
        // This token contains to 4-bit "fields", a higher and a lower, representing the literals'
        // length and the back reference's length, respectively. LSIC is used if either are their
        // maximal values.
        let token = input[input_pos];
        input_pos += 1;

        // Checking for hot-loop.
        // In most cases the metadata does fit in a single 1byte token (statistically) and we are in a safe-distance to the end.
        // This enables some optimized handling.
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
            if ext_dict.is_empty() && offset > output.len() {
                copy_from_dict(output, ext_dict, offset, match_length)?;
            } else if match_length + BLOCK_COPY_SIZE >= offset {
                duplicate_overlapping_slice(output, offset, match_length)?;
            } else {
                let old_len = output.len();
                if match_length <= 16 {
                    let mut dst = [0u8; 16];
                    let (start, did_overflow) = output.len().overflowing_sub(offset);
                    if did_overflow {
                        return Err(DecompressError::OffsetOutOfBounds);
                    }
                    dst.copy_from_slice(&output[start..start + 16]);
                    output.extend_from_slice(&dst);
                } else {
                    copy_24(output, offset)
                }
                output.truncate(old_len + match_length);
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
                literal_length += read_integer(input, &mut input_pos) as usize;
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

        let offset = read_u16(input, &mut input_pos) as usize;
        // Obtain the initial match length. The match length is the length of the duplicate segment
        // which will later be copied from data previously decompressed into the output buffer. The
        // initial length is derived from the second part of the token (the lower nibble), we read
        // earlier. Since having a match length of less than 4 would mean negative compression
        // ratio, we start at 4.

        // The initial match length can maximally be 19. As with the literal length, this indicates
        // that there are more bytes to read.
        let mut match_length = (4 + (token & 0xF)) as usize;
        if match_length == 4 + 15 {
            // The match length took the maximal value, indicating that there is more bytes. We
            // read the extra integer.
            match_length += read_integer(input, &mut input_pos) as usize;
        }

        if !ext_dict.is_empty() && offset > output.len() {
            copy_from_dict(output, ext_dict, offset, match_length)?;
        } else {
            // We now copy from the already decompressed buffer. This allows us for storing duplicates
            // by simply referencing the other location.
            duplicate_slice(output, offset, match_length)?;
        }
    }
    Ok(())
}

#[inline]
fn copy_from_dict(
    output: &mut Vec<u8>,
    ext_dict: &[u8],
    offset: usize,
    match_length: usize,
) -> Result<(), DecompressError> {
    let (start, did_overflow_1) = ext_dict.len().overflowing_sub(offset - output.len());
    let (end, did_overflow_2) = start.overflowing_add(match_length);
    if did_overflow_1 || did_overflow_2 || end > ext_dict.len() {
        return Err(DecompressError::OffsetOutOfBounds);
    }
    output.extend_from_slice(&ext_dict[start..end]);
    Ok(())
}

/// extends output by self-referential copies
#[inline]
fn duplicate_slice(
    output: &mut Vec<u8>,
    offset: usize,
    match_length: usize,
) -> Result<(), DecompressError> {
    if match_length + 16 >= offset {
        duplicate_overlapping_slice(output, offset, match_length)?;
    } else {
        let old_len = output.len();
        let mut dst = [0u8; 16];
        let (val, did_overflow) = output.len().overflowing_sub(offset);
        if did_overflow {
            return Err(DecompressError::OffsetOutOfBounds);
        }
        for i in (val..val + match_length).step_by(16) {
            dst.copy_from_slice(&output[i..i + 16]);
            output.extend_from_slice(&dst);
        }
        output.truncate(old_len + match_length);
    }
    Ok(())
}

/// self-referential copy for the case data start (end of output - offset) + match_length overlaps into output
#[inline]
fn duplicate_overlapping_slice(
    output: &mut Vec<u8>,
    offset: usize,
    match_length: usize,
) -> Result<(), DecompressError> {
    if offset == 1 {
        output.resize(output.len() + match_length, output[output.len() - 1]);
        Ok(())
    } else {
        let (start, did_overflow) = output.len().overflowing_sub(offset);
        if did_overflow {
            return Err(DecompressError::OffsetOutOfBounds);
        }
        #[cfg(feature = "checked-decode")]
        {
            if output.is_empty() {
                return Err(DecompressError::UnexpectedOutputEmpty);
            }
        }
        for i in start..start + match_length {
            let b = output[i];
            output.push(b);
        }
        Ok(())
    }
}

/// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in litte endian.
/// Can be used in conjuction with `compress_prepend_size`
#[inline]
pub fn decompress_size_prepended(input: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let (uncompressed_size, input) = super::uncompressed_size(input)?;
    // Allocate a vector to contain the decompressed stream.
    let mut vec = Vec::with_capacity(uncompressed_size);
    decompress_into(input, &mut vec)?;

    Ok(vec)
}

/// Decompress all bytes of `input` into a new vec.
#[inline]
pub fn decompress(input: &[u8], uncompressed_size: usize) -> Result<Vec<u8>, DecompressError> {
    // Allocate a vector to contain the decompressed stream.
    let mut vec = Vec::with_capacity(uncompressed_size);
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
