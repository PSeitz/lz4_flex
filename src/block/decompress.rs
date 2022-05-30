//! The decompression algorithm.
use crate::block::{DecompressError, MINMATCH};
use crate::sink::{Sink, SliceSink, VecSink};
use alloc::vec::Vec;

/// Copies data to output_ptr by self-referential copy from start and match_length
#[inline]
unsafe fn duplicate(
    output_ptr: &mut *mut u8,
    output_end: *mut u8,
    start: *const u8,
    match_length: usize,
) {
    // We cannot simply use memcpy or `extend_from_slice`, because these do not allow
    // self-referential copies: http://ticki.github.io/img/lz4_runs_encoding_diagram.svg

    // Considering that `wild_copy_match_16` can copy up to `16 - 1` extra bytes.
    // Defer to `duplicate_overlapping` in case of an overlapping match
    // OR the if the wild copy would copy beyond the end of the output.
    if (output_ptr.offset_from(start) as usize) < match_length + 16 - 1
        || (output_end.offset_from(*output_ptr) as usize) < match_length + 16 - 1
    {
        duplicate_overlapping(output_ptr, start, match_length);
    } else {
        debug_assert!(
            output_ptr.add(match_length / 16 * 16 + ((match_length % 16) != 0) as usize * 16)
                <= output_end
        );
        wild_copy_from_src_16(start, *output_ptr, match_length);
        *output_ptr = output_ptr.add(match_length);
    }
}

#[inline]
fn wild_copy_from_src_16(mut source: *const u8, mut dst_ptr: *mut u8, num_items: usize) {
    // Note: if the compiler transforms this into a call to memcpy it'll hurt performance!
    // It doesn't seem to be the case though, probably due to the 16 byte stride length.
    unsafe {
        let dst_ptr_end = dst_ptr.add(num_items);
        while (dst_ptr as usize) < dst_ptr_end as usize {
            core::ptr::copy_nonoverlapping(source, dst_ptr, 16);
            source = source.add(16);
            dst_ptr = dst_ptr.add(16);
        }
    }
}

/// Copy function, if the data start + match_length overlaps into output_ptr
#[inline]
unsafe fn duplicate_overlapping(
    output_ptr: &mut *mut u8,
    mut start: *const u8,
    match_length: usize,
) {
    // There is an edge case when output_ptr == start, which causes the decoder to potentially
    // expose up to match_length bytes of uninitialized data in the decompression buffer.
    // To prevent that we write a dummy zero to output, which will zero out output in such cases.
    // This is the same strategy used by the reference C implementation https://github.com/lz4/lz4/pull/772
    output_ptr.write(0u8);
    // Note: this looks like a harmless loop but is unrolled/auto-vectorized by the compiler
    for _ in 0..match_length {
        let curr = start.read();
        output_ptr.write(curr);
        *output_ptr = output_ptr.add(1);
        start = start.add(1);
    }
}

#[inline]
unsafe fn copy_from_dict(
    output_base: *mut u8,
    output_ptr: &mut *mut u8,
    ext_dict: &[u8],
    offset: usize,
    match_length: usize,
) -> usize {
    // If we're here we know offset > output pos, so we have at least 1 byte to copy from dict
    debug_assert!(output_ptr.offset_from(output_base) >= 0);
    debug_assert!(offset > output_ptr.offset_from(output_base) as usize);
    // If checked-decode is enabled we also know that the offset falls within ext_dict
    debug_assert!(ext_dict.len() + output_ptr.offset_from(output_base) as usize >= offset);

    let dict_offset = ext_dict.len() + output_ptr.offset_from(output_base) as usize - offset;
    // Can't copy past ext_dict len, the match may cross dict and output
    let dict_match_length = match_length.min(ext_dict.len() - dict_offset);
    core::ptr::copy_nonoverlapping(
        ext_dict.as_ptr().add(dict_offset),
        *output_ptr,
        dict_match_length,
    );
    *output_ptr = output_ptr.add(dict_match_length);
    dict_match_length
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
    loop {
        // We add the next byte until we get a byte which we add to the counting variable.

        #[cfg(feature = "checked-decode")]
        {
            if *input_pos >= input.len() {
                return Err(DecompressError::ExpectedAnotherByte);
            }
        }
        let extra = *unsafe { input.get_unchecked(*input_pos) };
        *input_pos += 1;
        n += extra as u32;

        // We continue if we got 255, break otherwise.
        if extra != 0xFF {
            break;
        }
    }

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

/// The token consists of two parts, the literal length (upper 4 bits) and match_length (lower 4
/// bits) if the literal length and match_length are both below 15, we don't need to read additional
/// data, so the token does fit the metadata in a single u8.
#[inline]
fn does_token_fit(token: u8) -> bool {
    !((token & FIT_TOKEN_MASK_LITERAL) == FIT_TOKEN_MASK_LITERAL
        || (token & FIT_TOKEN_MASK_MATCH) == FIT_TOKEN_MASK_MATCH)
}

/// Decompress all bytes of `input` into `output`.
///
/// Returns the number of bytes written (decompressed) into `output`.
#[inline]
pub(crate) fn decompress_internal<SINK: Sink, const USE_DICT: bool>(
    input: &[u8],
    output: &mut SINK,
    ext_dict: &[u8],
) -> Result<usize, DecompressError> {
    #[cfg(not(feature = "checked-decode"))]
    {
        // Prevent segfault for empty input even if checked-decode isn't enabled
        if input.is_empty() {
            return Err(DecompressError::ExpectedAnotherByte);
        }
    }

    let ext_dict = if USE_DICT {
        ext_dict
    } else {
        // ensure optimizer knows ext_dict length is 0 if !USE_DICT
        debug_assert!(ext_dict.is_empty());
        &[]
    };
    let output_base = unsafe { output.base_mut_ptr() };
    let output_end = unsafe { output_base.add(output.capacity()) };
    let output_start_pos_ptr = unsafe { output.pos_mut_ptr() };
    let mut output_ptr = output_start_pos_ptr;
    let mut input_pos = 0;

    let safe_input_pos = input
        .len()
        .saturating_sub(16 /* literal copy */ +  2 /* u16 match offset */);
    let safe_output_ptr = unsafe {
        output_base.add(
            output
                .capacity()
                .saturating_sub(16 /* literal copy */ + 18 /* match copy */),
        )
    };

    // Exhaust the decoder by reading and decompressing all blocks until the remaining buffer is
    // empty.
    loop {
        #[cfg(feature = "checked-decode")]
        {
            if input_pos >= input.len() {
                return Err(DecompressError::ExpectedAnotherByte);
            }
        }

        // Read the token. The token is the first byte in a block. It is divided into two 4-bit
        // subtokens, the higher and the lower.
        // This token contains to 4-bit "fields", a higher and a lower, representing the literals'
        // length and the back reference's length, respectively.
        let token = unsafe { *input.get_unchecked(input_pos) };
        input_pos += 1;

        // Checking for hot-loop.
        // In most cases the metadata does fit in a single 1byte token (statistically) and we are in
        // a safe-distance to the end. This enables some optimized handling.
        //
        // Ideally we want to check for safe output pos like: output.pos() <= safe_output_pos; But
        // that doesn't work when the safe_output_ptr is == output_ptr due to insufficient
        // capacity. So we use `<` instead of `<=`, which covers that case.
        if does_token_fit(token) && input_pos <= safe_input_pos && output_ptr < safe_output_ptr {
            let literal_length = (token >> 4) as usize;
            let mut match_length = MINMATCH + (token & 0xF) as usize;

            // output_ptr <= safe_output_ptr should guarantee we have enough space in output
            debug_assert!(
                unsafe { output_ptr.add(literal_length + match_length) } <= output_end,
                "{} wont fit ",
                literal_length + match_length
            );
            #[cfg(feature = "checked-decode")]
            {
                // Check if literal is out of bounds for the input
                if literal_length > input.len() - input_pos {
                    return Err(DecompressError::OffsetOutOfBounds);
                }
            }

            // Copy the literal
            // The literal is at max 14 bytes, and the is_safe_distance check assures
            // that we are far away enough from the end so we can safely copy 16 bytes
            unsafe {
                core::ptr::copy_nonoverlapping(input.as_ptr().add(input_pos), output_ptr, 16);
            }
            input_pos += literal_length;
            unsafe {
                output_ptr = output_ptr.add(literal_length);
            }

            // input_pos <= safe_input_pos should guarantee we have enough space in input
            debug_assert!(input.len() - input_pos >= 2);
            let offset = read_u16(input, &mut input_pos) as usize;

            let output_len = unsafe { output_ptr.offset_from(output_base) as usize };
            #[cfg(feature = "checked-decode")]
            {
                if offset > output_len + ext_dict.len() {
                    return Err(DecompressError::OffsetOutOfBounds);
                }
            }

            // Check if part of the match is in the external dict
            if USE_DICT && offset > output_len {
                let copied = unsafe {
                    copy_from_dict(output_base, &mut output_ptr, ext_dict, offset, match_length)
                };
                if copied == match_length {
                    continue;
                }
                // match crosses ext_dict and output
                match_length -= copied;
            }

            // Calculate the start of this duplicate segment. At this point offset was already
            // checked to be in bounds and the external dictionary copy, if any, was
            // already copied and subtracted from match_length.
            let start_ptr = unsafe { output_ptr.sub(offset) };
            debug_assert!(start_ptr >= output_base);
            debug_assert!(start_ptr < output_end);
            debug_assert!(unsafe { output_end.offset_from(start_ptr) as usize } >= match_length);

            // In this branch we know that match_length is at most 18 (14 + MINMATCH).
            // But the blocks can overlap, so make sure they are at least 18 bytes apart
            // to enable an optimized copy of 18 bytes.
            if offset >= match_length {
                unsafe {
                    // _copy_, not copy_non_overlaping, as it may overlap. That's ok.
                    core::ptr::copy(start_ptr, output_ptr, 18);
                    output_ptr = output_ptr.add(match_length);
                }
            } else {
                unsafe {
                    duplicate_overlapping(&mut output_ptr, start_ptr, match_length);
                }
            }

            continue;
        }

        // Now, we read the literals section.
        // Literal Section
        // If the initial value is 15, it is indicated that another byte will be read and added to
        // it
        let mut literal_length = (token >> 4) as usize;
        if literal_length != 0 {
            if literal_length == 15 {
                // The literal_length length took the maximal value, indicating that there is more
                // than 15 literal_length bytes. We read the extra integer.
                literal_length += read_integer(input, &mut input_pos)? as usize;
            }

            #[cfg(feature = "checked-decode")]
            {
                // Check if literal is out of bounds for the input, and if there is enough space on
                // the output
                if literal_length > input.len() - input_pos {
                    return Err(DecompressError::LiteralOutOfBounds);
                }
                if literal_length > unsafe { output_end.offset_from(output_ptr) as usize } {
                    return Err(DecompressError::OutputTooSmall {
                        expected: unsafe { output_ptr.offset_from(output_base) as usize }
                            + literal_length,
                        actual: output.capacity(),
                    });
                }
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
        if input_pos >= input.len() {
            break;
        }

        // Read duplicate section
        #[cfg(feature = "checked-decode")]
        {
            if input.len() - input_pos < 2 {
                return Err(DecompressError::ExpectedAnotherByte);
            }
        }
        let offset = read_u16(input, &mut input_pos) as usize;
        // Obtain the initial match length. The match length is the length of the duplicate segment
        // which will later be copied from data previously decompressed into the output buffer. The
        // initial length is derived from the second part of the token (the lower nibble), we read
        // earlier. Since having a match length of less than 4 would mean negative compression
        // ratio, we start at 4 (MINMATCH).

        // The initial match length can maximally be 19 (MINMATCH + 15). As with the literal length,
        // this indicates that there are more bytes to read.
        let mut match_length = MINMATCH + (token & 0xF) as usize;
        if match_length == MINMATCH + 15 {
            // The match length took the maximal value, indicating that there is more bytes. We
            // read the extra integer.
            match_length += read_integer(input, &mut input_pos)? as usize;
        }

        // We now copy from the already decompressed buffer. This allows us for storing duplicates
        // by simply referencing the other location.
        let output_len = unsafe { output_ptr.offset_from(output_base) as usize };

        // We'll do a bounds check in checked-decode.
        #[cfg(feature = "checked-decode")]
        {
            if offset > output_len + ext_dict.len() {
                return Err(DecompressError::OffsetOutOfBounds);
            }
            if match_length > unsafe { output_end.offset_from(output_ptr) as usize } {
                return Err(DecompressError::OutputTooSmall {
                    expected: output_len + match_length,
                    actual: output.capacity(),
                });
            }
        }

        if USE_DICT && offset > output_len {
            let copied = unsafe {
                copy_from_dict(output_base, &mut output_ptr, ext_dict, offset, match_length)
            };
            if copied == match_length {
                continue;
            }
            // match crosses ext_dict and output
            match_length -= copied;
        }

        // Calculate the start of this duplicate segment. At this point offset was already checked
        // to be in bounds and the external dictionary copy, if any, was already copied and
        // subtracted from match_length.
        let start_ptr = unsafe { output_ptr.sub(offset) };
        debug_assert!(start_ptr >= output_base);
        debug_assert!(start_ptr < output_end);
        debug_assert!(unsafe { output_end.offset_from(start_ptr) as usize } >= match_length);
        unsafe {
            duplicate(&mut output_ptr, output_end, start_ptr, match_length);
        }
    }
    unsafe {
        output.set_pos(output_ptr.offset_from(output_base) as usize);
        Ok(output_ptr.offset_from(output_start_pos_ptr) as usize)
    }
}

/// Decompress all bytes of `input` into `output`.
/// `output` should be preallocated with a size of of the uncompressed data.
#[inline]
pub fn decompress_into(input: &[u8], output: &mut [u8]) -> Result<usize, DecompressError> {
    decompress_internal::<_, false>(input, &mut SliceSink::new(output, 0), b"")
}

/// Decompress all bytes of `input` into `output`.
///
/// Returns the number of bytes written (decompressed) into `output`.
#[inline]
pub fn decompress_into_with_dict(
    input: &[u8],
    output: &mut [u8],
    ext_dict: &[u8],
) -> Result<usize, DecompressError> {
    decompress_internal::<_, true>(input, &mut SliceSink::new(output, 0), ext_dict)
}

/// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in
/// little endian. Can be used in conjunction with `compress_prepend_size`
#[inline]
pub fn decompress_size_prepended(input: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let (uncompressed_size, input) = super::uncompressed_size(input)?;
    decompress(input, uncompressed_size)
}

/// Decompress all bytes of `input` into a new vec.
#[inline]
pub fn decompress(input: &[u8], uncompressed_size: usize) -> Result<Vec<u8>, DecompressError> {
    // Allocate a vector to contain the decompressed stream.
    let mut vec: Vec<u8> = Vec::with_capacity(uncompressed_size);
    let decomp_len =
        decompress_internal::<_, false>(input, &mut VecSink::new(&mut vec, 0, 0), b"")?;
    if decomp_len != uncompressed_size {
        return Err(DecompressError::UncompressedSizeDiffers {
            expected: uncompressed_size,
            actual: decomp_len,
        });
    }
    Ok(vec)
}

/// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in
/// little endian. Can be used in conjunction with `compress_prepend_size_with_dict`
#[inline]
pub fn decompress_size_prepended_with_dict(
    input: &[u8],
    ext_dict: &[u8],
) -> Result<Vec<u8>, DecompressError> {
    let (uncompressed_size, input) = super::uncompressed_size(input)?;
    decompress_with_dict(input, uncompressed_size, ext_dict)
}

/// Decompress all bytes of `input` into a new vec.
#[inline]
pub fn decompress_with_dict(
    input: &[u8],
    uncompressed_size: usize,
    ext_dict: &[u8],
) -> Result<Vec<u8>, DecompressError> {
    // Allocate a vector to contain the decompressed stream.
    let mut vec: Vec<u8> = Vec::with_capacity(uncompressed_size);
    let decomp_len =
        decompress_internal::<_, true>(input, &mut VecSink::new(&mut vec, 0, 0), ext_dict)?;
    if decomp_len != uncompressed_size {
        return Err(DecompressError::UncompressedSizeDiffers {
            expected: uncompressed_size,
            actual: decomp_len,
        });
    }
    Ok(vec)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn all_literal() {
        assert_eq!(decompress(&[0x30, b'a', b'4', b'9'], 3).unwrap(), b"a49");
    }

    // this error test is only valid in checked-decode.
    #[cfg(feature = "checked-decode")]
    #[test]
    fn offset_oob() {
        decompress(&[0x10, b'a', 2, 0], 4).unwrap_err();
        decompress(&[0x40, b'a', 1, 0], 4).unwrap_err();
    }
}
