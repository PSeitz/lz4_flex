extern crate lz4_flex;

#[macro_use]
extern crate quick_error;

const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/dickens.txt");
// const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/compression_66k_JSON.txt");

fn main() {

    let compressed = lz4_flex::compress(COMPRESSION10MB as &[u8]);
    for _ in 0..30 {
        decompress(&compressed, COMPRESSION10MB.len()).unwrap();
    }
    
}


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


/// A LZ4 decoder.
///
/// This will decode in accordance to the LZ4 format. It represents a particular state of the
/// decompressor.
struct Decoder<'a> {
    /// The compressed input.
    input: &'a [u8],
    /// The current read position in the input.
    input_pos: usize,
    /// The decompressed output.
    output_ptr: *mut u8,

    #[cfg(feature = "safe-decode")]
    output_start: *mut u8,

}

impl<'a> Decoder<'a> {

    /// Write an already decompressed match to the output stream.
    ///
    /// This is used for the essential part of the algorithm: deduplication. We start at some
    /// position `start` and then keep pushing the following element until we've added
    /// `match_length` elements.
    #[inline]
    fn duplicate(&mut self, mut start: *const u8, match_length: usize) {
        // We cannot simply use memcpy or `extend_from_slice`, because these do not allow
        // self-referential copies: http://ticki.github.io/img/lz4_runs_encoding_diagram.svg
        // `reserve` enough space on the vector to safely copy self referential data.
        // Check overlap copy
        if (self.output_ptr as usize) < unsafe{start.add(match_length)} as usize {
            for _ in 0..match_length {
                unsafe {
                    let curr = start.read();
                    self.output_ptr.write(curr);
                    self.output_ptr = self.output_ptr.add(1);
                    start = start.add(1);
                }
            }
        }else{
            copy_on_self(&mut self.output_ptr, start, match_length);
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
    fn read_integer(&mut self) -> Result<u32, Error>  {
        // We start at zero and count upwards.
        let mut n:u32 = 0;
        // If this byte takes value 255 (the maximum value it can take), another byte is read
        // and added to the sum. This repeats until a byte lower than 255 is read.
        while {
            // We add the next byte until we get a byte which we add to the counting variable.
            
            #[cfg(feature = "safe-decode")]
            {
                if self.input.len() < self.input_pos + 1 {
                    return Err(Error::ExpectedAnotherByte);
                };
            }
            // check alread done in move_cursor
            let extra = *unsafe{self.input.get_unchecked(self.input_pos)};
            self.input_pos+=1;
            n += extra as u32;

            // We continue if we got 255.
            extra == 0xFF
        } {}

        // 255, 255, 255, 8
        // 111, 111, 111, 101

        Ok(n)
    }

    /// Read a little-endian 16-bit integer from the input stream.
    // #[inline(never)]
    #[inline]
    fn read_u16(&mut self) -> Result<u16, Error> {
        // We use byteorder to read an u16 in little endian.

        let mut num: u16 = 0;
        unsafe{
            std::ptr::copy_nonoverlapping(self.input.as_ptr().add(self.input_pos), &mut num as *mut u16 as *mut u8, 2);
        }
        self.input_pos+=2;
        Ok(num)

        // self.input_pos+=2;
        // Ok(num)
    }

    /// Complete the decompression by reading all the blocks.
    ///
    /// # Decompressing a block
    ///
    /// Blocks consists of:
    ///  - A 1 byte token
    ///      * A 4 bit integer $t_1$.
    ///      * A 4 bit integer $t_2$.
    ///  - A $n$ byte sequence of 0xFF bytes (if $t_1 \neq 15$, then $n = 0$).
    ///  - $x$ non-0xFF 8-bit integers, L (if $t_1 = 15$, $x = 1$, else $x = 0$).
    ///  - $t_1 + 15n + L$ bytes of uncompressed data (literals).
    ///  - 16-bits offset (little endian), $a$.
    ///  - A $m$ byte sequence of 0xFF bytes (if $t_2 \neq 15$, then $m = 0$).
    ///  - $y$ non-0xFF 8-bit integers, $c$ (if $t_2 = 15$, $y = 1$, else $y = 0$).
    ///
    /// First, the literals are copied directly and unprocessed to the output buffer, then (after
    /// the involved parameters are read) $t_2 + 15m + c$ bytes are copied from the output buffer
    /// at position $a + 4$ and appended to the output buffer. Note that this copy can be
    /// overlapping.
    #[inline(never)]
    fn complete(&mut self) -> Result<(), Error> {
        if self.input.is_empty() {
            return Err(Error::ExpectedAnotherByte);
        }
        // Exhaust the decoder by reading and decompressing all blocks until the remaining buffer
        // is empty.
        let in_len = self.input.len() - 1;
        while in_len > self.input_pos {
            // Read the token. The token is the first byte in a block. It is divided into two 4-bit
            // subtokens, the higher and the lower.

            // check alread done in move_cursor
            // This token contains to 4-bit "fields", a higher and a lower, representing the literals'
            // length and the back reference's length, respectively. LSIC is used if either are their
            // maximal values.
            let token = unsafe{*self.input.get_unchecked(self.input_pos)};
            self.input_pos+=1;

            // Now, we read the literals section.
            // Literal Section
            // self.read_literal_section();
            let mut literal_length = (token >> 4) as usize;
            // If the initial value is 15, it is indicated that another byte will be read and added to
            // it.
            if literal_length != 0 {
            
                if literal_length == 15 {
                    // The literal_length length took the maximal value, indicating that there is more than 15
                    // literal_length bytes. We read the extra integer.
                    literal_length += self.read_integer()? as usize;
                }

                if cfg!(feature = "safe-decode"){
                    if self.input.len() < self.input_pos + literal_length {
                        return Err(Error::ExpectedAnotherByte);
                    };
                }
                unsafe{
                    std::ptr::copy_nonoverlapping(self.input.as_ptr().add(self.input_pos), self.output_ptr, literal_length);
                    self.output_ptr = self.output_ptr.add(literal_length);
                }

                self.input_pos+=literal_length;
            }

            // If the input stream is emptied, we break out of the loop. This is only the case
            // in the end of the stream, since the block is intact otherwise.
            if in_len <= self.input_pos { break; }

            // Now, we read the duplicates section.
            // self.read_duplicate_section()?;

            // Read duplicate section

            let offset = self.read_u16()?;
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
                match_length += self.read_integer()? as usize;
            }

            // We now copy from the already decompressed buffer. This allows us for storing duplicates
            // by simply referencing the other location.

            // Calculate the start of this duplicate segment. We use wrapping subtraction to avoid
            // overflow checks, which we will catch later.
            let start_ptr = unsafe{self.output_ptr.sub(offset as usize)};

            // We'll do a bound check to avoid panicking.

            #[cfg(feature = "safe-decode")]{
                if start_ptr as usize >= self.output_start as usize {
                    // Write the duplicate segment to the output buffer.
                    self.duplicate(start_ptr, match_length);
                } else {
                    return Err(Error::OffsetOutOfBounds)
                }
            }

            #[cfg(not(feature = "safe-decode"))]{
                self.duplicate(start_ptr, match_length);
            }

            // if !self.safe_decode || start_ptr as usize >= self.output_start as usize {
            //     // Write the duplicate segment to the output buffer.
            //     self.duplicate(start_ptr, match_length);
            // } else {
            //     return Err(Error::OffsetOutOfBounds)
            // }
        }

        Ok(())
    }
}

/// Decompress all bytes of `input` into `output`.
#[inline(never)]
pub fn decompress_into(input: &[u8], output: &mut Vec<u8>) -> Result<(), Error> {
    // Decode into our vector.
    Decoder {
        input: input,
        input_pos: 0,
        output_ptr: output.as_mut_ptr(),
        #[cfg(feature = "safe-decode")]
        output_start: output.as_mut_ptr(),
    }.complete()?;

    Ok(())
}


/// Decompress all bytes of `input`.
#[inline(never)]
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
        std::ptr::copy_nonoverlapping(start, *out_ptr, num_items);
        *out_ptr = out_ptr.add(num_items);
    }
}
