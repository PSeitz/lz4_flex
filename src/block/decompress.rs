//! The decompression algorithm.
use crate::block::wild_copy_from_src_8;
use crate::block::wild_copy_from_src;


quick_error! {
    /// An error representing invalid compressed data.
    #[derive(Debug)]
    pub enum Error {
        /// Expected another byte, but none found.
        ExpectedAnotherByte {
            description("Expected another byte, found none.")
        }
        /// Deduplication offset out of bounds (not in buffer).
        OffsetOutOfBounds {
            description("The offset to copy is not contained in the decompressed buffer.")
        }
    }
}

#[inline]
fn duplicate(output_ptr: &mut *mut u8, start: *const u8, match_length: usize) {
    // We cannot simply use memcpy or `extend_from_slice`, because these do not allow
    // self-referential copies: http://ticki.github.io/img/lz4_runs_encoding_diagram.svg
    // `reserve` enough space on the vector to safely copy self referential data.
    // Check overlap copy
    if (*output_ptr as usize) < unsafe{start.add(match_length)} as usize {
        duplicate_overlapping(output_ptr, start, match_length);
    }else{
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
fn read_integer(input: &[u8], input_pos: &mut usize) -> Result<u32, Error>  {
    // We start at zero and count upwards.
    let mut n:u32 = 0;
    // If this byte takes value 255 (the maximum value it can take), another byte is read
    // and added to the sum. This repeats until a byte lower than 255 is read.
    while {
        // We add the next byte until we get a byte which we add to the counting variable.
        
        #[cfg(feature = "safe-decode")]
        {
            if input.len() < input_pos + 1 {
                return Err(Error::ExpectedAnotherByte);
            };
        }
        // check alread done in move_cursor
        let extra = *unsafe{input.get_unchecked(*input_pos)};
        *input_pos+=1;
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
    unsafe{
        std::ptr::copy_nonoverlapping(input.as_ptr().add(*input_pos), &mut num as *mut u16 as *mut u8, 2);
    }

    *input_pos+=2;
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

#[inline]
/// The token consists of two parts, the literal length (upper 4 bits) and match_length (lower 4 bits)
/// if the literal length and match_length are both below 15, we don't need to read additional data
fn does_token_fit(token: u8) -> bool {
    !(
        (token & FIT_TOKEN_MASK_LITERAL) == FIT_TOKEN_MASK_LITERAL
        ||
        (token & FIT_TOKEN_MASK_MATCH) == FIT_TOKEN_MASK_MATCH
    )
}

#[inline]
fn is_safe_distance(input_pos: usize, in_len: usize) -> bool {
    input_pos < in_len
}

fn block_copy_from_src(source: *const u8, dst_ptr: *mut u8, num_items: usize) {
    debug_assert!(num_items <= 24);
    unsafe{
        let dst_ptr_end = dst_ptr.add(num_items);
        if (dst_ptr as usize) < dst_ptr_end as usize {
            std::ptr::copy_nonoverlapping(source, dst_ptr, 24);
        }
    }
}


/// Decompress all bytes of `input` into `output`.
#[inline]
pub fn decompress_into(input: &[u8], output: &mut Vec<u8>) -> Result<(), Error> {
    // Decode into our vector.
    let mut input_pos = 0;
    let mut output_ptr = output.as_mut_ptr();
    
    #[cfg(feature = "safe-decode")]
    let output_start = output_ptr;

    if input.is_empty() {
        return Err(Error::ExpectedAnotherByte);
    }
    // Exhaust the decoder by reading and decompressing all blocks until the remaining buffer
    // is empty.
    let in_len = input.len() - 1;
    let end_check = input.len().wrapping_sub(18);
    while in_len > input_pos {
        // Read the token. The token is the first byte in a block. It is divided into two 4-bit
        // subtokens, the higher and the lower.

        // check alread done in move_cursor
        // This token contains to 4-bit "fields", a higher and a lower, representing the literals'
        // length and the back reference's length, respectively. LSIC is used if either are their
        // maximal values.
        let token = unsafe{*input.get_unchecked(input_pos)};
        input_pos+=1;

        // TODO maybe handle small inputs seperately
        if in_len > 50 && does_token_fit(token) && is_safe_distance(input_pos, end_check) {
            let literal_length = (token >> 4) as usize;
            let match_length = (4 + (token & 0xF)) as usize;

            unsafe{block_copy_from_src(input.as_ptr().add(input_pos), output_ptr, literal_length)};
            input_pos+=literal_length;
            unsafe{output_ptr = output_ptr.add(literal_length);}

            let offset = read_u16(input, &mut input_pos);
            let start_ptr = unsafe{output_ptr.sub(offset as usize)};

            // Write the duplicate segment to the output buffer.
            if (output_ptr as usize) < unsafe{start_ptr.add(match_length)} as usize {
                duplicate_overlapping(&mut output_ptr, start_ptr, match_length);
            }else{
                unsafe {
                    block_copy_from_src(start_ptr, output_ptr, match_length);
                    output_ptr = output_ptr.add(match_length);
                }
            }

            continue;
        }

        // Now, we read the literals section.
        // Literal Section
        // read_literal_section();
        let mut literal_length = (token >> 4) as usize;
        // If the initial value is 15, it is indicated that another byte will be read and added to
        // it.
        
        if literal_length == 15 {
            // The literal_length length took the maximal value, indicating that there is more than 15
            // literal_length bytes. We read the extra integer.
            literal_length += read_integer(input, &mut input_pos)? as usize;
        }

        if cfg!(feature = "safe-decode"){
            if input.len() < input_pos + literal_length {
                return Err(Error::ExpectedAnotherByte);
            };
        }
        unsafe{
            std::ptr::copy_nonoverlapping(input.as_ptr().add(input_pos), output_ptr, literal_length);
            output_ptr = output_ptr.add(literal_length);
        }

        input_pos+=literal_length;

        // If the input stream is emptied, we break out of the loop. This is only the case
        // in the end of the stream, since the block is intact otherwise.
        if in_len <= input_pos { break; }

        // Read duplicate section
        if cfg!(feature = "safe-decode"){
            if input_pos + 2 >= input.len() {
                return Err(Error::OffsetOutOfBounds);
            }
        };
        let offset = read_u16(input, &mut input_pos);
        // Obtain the initial match length. The match length is the length of the duplicate segment
        // which will later be copied from data previously decompressed into the output buffer. The
        // initial length is derived from the second part of the token (the lower nibble), we read
        // earlier. Since having a match length of less than 4 would mean negative compression
        // ratio, we start at 4.
        let mut match_length = (4 + (token & 0xF)) as usize;

        // The intial match length can maximally be 19. As with the literal length, this indicates
        // that there are more bytes to read.
        if match_length == 4 + 15 {
            // The match length took the maximal value, indicating that there is more bytes. We
            // read the extra integer.
            match_length += read_integer(input, &mut input_pos)? as usize;
        }

        // We now copy from the already decompressed buffer. This allows us for storing duplicates
        // by simply referencing the other location.

        // Calculate the start of this duplicate segment. We use wrapping subtraction to avoid
        // overflow checks, which we will catch later.
        let start_ptr = unsafe{output_ptr.sub(offset as usize)};

        // We'll do a bound check to avoid panicking.
        #[cfg(feature = "safe-decode")]
        {
            if (start_ptr as usize) < (output_ptr as usize) {
                return Err(Error::OffsetOutOfBounds)
            };
        }
        duplicate(&mut output_ptr, start_ptr, match_length);

    }
    Ok(())

}

/// Decompress all bytes of `input`.
#[inline]
pub fn decompress(input: &[u8], uncompressed_size: usize) -> Result<Vec<u8>, Error> {
    // Allocate a vector to contain the decompressed stream.
    let mut vec = Vec::with_capacity(uncompressed_size + 8);
    unsafe{vec.set_len(uncompressed_size);}
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

// #[test]
// // #[inline(never)]
// fn test_copy_on_self() {
//     let mut data: Vec<u8>= vec![10];
//     copy_on_self(&mut data.as_mut_ptr(), 0, 1);
//     assert_eq!(data, [10, 10]);
// }


#[cfg(test)]
mod test {
    use super::*;

    #[test]
    // #[inline(never)]
    fn aaaaaaaaaaa_lots_of_aaaaaaaaa() {
        assert_eq!(decompress(&[0x11, b'a', 1, 0], "aaaaaa".len()).unwrap(), b"aaaaaa");
    }

    #[test]
    // #[inline(never)]
    fn multiple_repeated_blocks() {
        assert_eq!(decompress(&[0x11, b'a', 1, 0, 0x22, b'b', b'c', 2, 0], "aaaaaabcbcbcbc".len()).unwrap(), b"aaaaaabcbcbcbc");
    }

    #[test]
    // #[inline(never)]
    fn all_literal() {
        assert_eq!(decompress(&[0x30, b'a', b'4', b'9'], 3).unwrap(), b"a49");
    }

    // #[test]
    // // #[inline(never)]
    // fn offset_oob() {
    //     decompress(&[0x10, b'a', 2, 0]).unwrap_err();
    //     decompress(&[0x40, b'a', 1, 0]).unwrap_err();
    // }
}
