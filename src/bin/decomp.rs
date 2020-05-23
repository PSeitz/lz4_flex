extern crate lz4_flex;

use std::ptr;
#[macro_use]
extern crate quick_error;


// ML_BITS  4
// #define ML_MASK  ((1U<<ML_BITS)-1)
// #define RUN_BITS (8-ML_BITS)
// #define RUN_MASK ((1U<<RUN_BITS)-1)

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



// const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/dickens.txt");
const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/compression_66k_JSON.txt");

fn main() {

    let compressed = lz4_flex::compress(COMPRESSION10MB as &[u8]);
    for _ in 0..100000 {
        decompress(&compressed).unwrap();
    }
    
}






/// A LZ4 decoder.
///
/// This will decode in accordance to the LZ4 format. It represents a particular state of the
/// decompressor.
struct Decoder<'a> {
    curr: *const u8,
    end_pos: *const u8,
    /// The compressed input.
    // input: &'a [u8],

    // input_pos: usize,
    /// The decompressed output.
    output: &'a mut Vec<u8>,
    /// The current block's "token".
    ///
    /// This token contains to 4-bit "fields", a higher and a lower, representing the literals'
    /// length and the back reference's length, respectively. LSIC is used if either are their
    /// maximal values.
    token: u8,
}

impl<'a> Decoder<'a> {

    /// Write an already decompressed match to the output stream.
    ///
    /// This is used for the essential part of the algorithm: deduplication. We start at some
    /// position `start` and then keep pushing the following element until we've added
    /// `match_length` elements.
    // #[inline(never)]
    // fn duplicate(&mut self, start: usize, match_length: usize) {
    //     // We cannot simply use memcpy or `extend_from_slice`, because these do not allow
    //     // self-referential copies: http://ticki.github.io/img/lz4_runs_encoding_diagram.svg
    //     // `reserve` enough space on the vector to safely copy self referential data.
    //     if self.output.len() < start+match_length { // TODO handle special case
    //         for i in start..start + match_length {
    //             let b = self.output[i];
    //             self.output.push(b);
    //         }
    //     }else{
    //         copy_on_self(&mut self.output, start, match_length);
    //     }
    // }

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
    fn read_integer(&mut self) -> usize  {
        // We start at zero and count upwards.
        let mut n = 0;
        // If this byte takes value 255 (the maximum value it can take), another byte is read
        // and added to the sum. This repeats until a byte lower than 255 is read.
        loop {
            // We add the next byte until we get a byte which we add to the counting variable.
            let extra = unsafe{self.curr.read()};
            unsafe{self.curr = self.curr.add(1)};
            n += extra as usize;
            // We continue if we got 255.
            if extra != 0xFF {
                break;
            }
        }

        n
    }


    /// Read the literals section of a block.
    ///
    /// The literals section encodes some bytes which are to be copied to the output without any
    /// modification.
    ///
    /// It consists of two parts:
    ///
    /// 1. An LSIC integer extension to the literals length as defined by the first part of the
    ///    token, if it takes the highest value (15).
    /// 2. The literals themself.
    // #[inline(never)]
    // fn read_literal_section(&mut self) -> Result<(), Error>  {
    //     // The higher token is the literals part of the token. It takes a value from 0 to 15.
    //     let mut literal = (self.token >> 4) as usize;
    //     // If the initial value is 15, it is indicated that another byte will be read and added to
    //     // it.
    //     if literal == 15 {
    //         // The literal length took the maximal value, indicating that there is more than 15
    //         // literal bytes. We read the extra integer.
    //         literal += self.read_integer();
    //     }
    //     // Now we know the literal length. The number will be used to indicate how long the
    //     // following literal copied to the output buffer is.

    //     self.output.reserve(literal);
    //     unsafe{
    //         let dst_ptr = self.output.as_mut_ptr().offset(self.output.len() as isize);

    //         ptr::copy_nonoverlapping(self.curr, dst_ptr, literal);

    //         self.output.set_len(self.output.len() + literal);
    //         self.curr = self.curr.add(literal);
    //     }
    //     Ok(())

    // }

    /// Read the duplicates section of the block.
    ///
    /// The duplicates section serves to reference an already decoded segment. This consists of two
    /// parts:
    ///
    /// 1. A 16-bit little-endian integer defining the "offset", i.e. how long back we need to go
    ///    in the decoded buffer and copy.
    /// 2. An LSIC integer extension to the duplicate length as defined by the first part of the
    ///    token, if it takes the highest value (15).
    // #[inline(never)]
    // fn read_duplicate_section(&mut self) {
    //     // Now, we will obtain the offset which we will use to copy from the output. It is an
    //     // 16-bit integer.
    //     // let offset = self.read_u16()?;

    //     let mut offset:u16 = 0;
    //     unsafe{ptr::copy_nonoverlapping(self.curr, &mut offset as *mut u16 as *mut u8, 2);} // TODO check isLittleEndian
    //     // unsafe{
    //     //     self.curr = self.curr.add(2);
    //     // }

    //     let mut inc = 2;
    //     // let offset = LittleEndian::read_u16(&self.input[self.input_pos ..]);
    //     // self.input_pos+=2;

    //     // Obtain the initial match length. The match length is the length of the duplicate segment
    //     // which will later be copied from data previously decompressed into the output buffer. The
    //     // initial length is derived from the second part of the token (the lower nibble), we read
    //     // earlier. Since having a match length of less than 4 would mean negative compression
    //     // ratio, we start at 4.
    //     let mut match_length = (4 + (self.token & 0xF)) as usize;

    //     // The intial match length can maximally be 19. As with the literal length, this indicates
    //     // that there are more bytes to read.
    //     if match_length == 4 + 15 {
    //         // The match length took the maximal value, indicating that there is more bytes. We
    //         // read the extra integer.
    //         // match_length += self.read_integer()?;

    //         // let mut n = 0;
    //         // If this byte takes value 255 (the maximum value it can take), another byte is read
    //         // and added to the sum. This repeats until a byte lower than 255 is read.
    //         loop {
    //             // We add the next byte until we get a byte which we add to the counting variable.
    //             let extra = unsafe{self.curr.add(inc).read()};
    //             inc +=1;
    //             // unsafe{self.curr = self.curr.add(1)};
    //             match_length += extra as usize;
    //             // We continue if we got 255.
    //             if extra != 0xFF {
    //                 break;
    //             }
    //         }
    //     }

    //     self.curr = unsafe{ self.curr.add(inc)};

    //     // We now copy from the already decompressed buffer. This allows us for storing duplicates
    //     // by simply referencing the other location.

    //     // Calculate the start of this duplicate segment. We use wrapping subtraction to avoid
    //     // overflow checks, which we will catch later.
    //     // let start = self.output.len().wrapping_sub(offset as usize);
    //     let start = self.output.len() - offset as usize;

    //     // We'll do a bound check to avoid panicking.
    //     self.duplicate(start, match_length);
    // }

    /// Complete the decompression by reading all the blocks.
    ///
    /// # Decompressing a block
    ///
    /// Blocks consists of:
    ///  - A 1 byte token
    ///      * A 4 bit integer t_1.
    ///      * A 4 bit integer t_2.
    ///  - A n byte sequence of 0xFF bytes (if t_1 \neq 15, then n = 0).
    ///  - x non-0xFF 8-bit integers, L (if t_1 = 15, x = 1, else x = 0).
    ///  - t_1 + 15n + L bytes of uncompressed data (literals).
    ///  - 16-bits offset (little endian), a.
    ///  - A m byte sequence of 0xFF bytes (if t_2 \neq 15, then m = 0).
    ///  - y non-0xFF 8-bit integers, c (if t_2 = 15, y = 1, else y = 0).
    ///
    /// First, the literals are copied directly and unprocessed to the output buffer, then (after
    /// the involved parameters are read) t_2 + 15m + c bytes are copied from the output buffer
    /// at position a + 4 and appended to the output buffer. Note that this copy can be
    /// overlapping.
    #[inline(never)]
    fn complete(&mut self) -> Result<(), Error> {
        // Exhaust the decoder by reading and decompressing all blocks until the remaining buffer
        // is empty.
        // let in_len = self.input.len();
        loop {
            // Read the token. The token is the first byte in a block. It is divided into two 4-bit
            // subtokens, the higher and the lower.
            // self.token = self.take(1)[0];


            // self.move_cursor(&self.input, 1)?;

            // TODO CHECK
            // check alread done in move_cursor
            self.token = unsafe{self.curr.read()};
            unsafe{self.curr = self.curr.add(1);}

            // Now, we read the literals section.
            // self.read_literal_section()?;

            let mut literal = (self.token >> 4) as usize;
            if literal !=0 {
                // If the initial value is 15, it is indicated that another byte will be read and added to
                // it.
                if literal == 15 {
                    // The literal length took the maximal value, indicating that there is more than 15
                    // literal bytes. We read the extra integer.
                    literal += self.read_integer();
                }
                // Now we know the literal length. The number will be used to indicate how long the
                // following literal copied to the output buffer is.

                // self.output.reserve(literal);
                // unsafe{
                //     let dst_ptr = self.output.as_mut_ptr().offset(self.output.len() as isize);

                //     ptr::copy_nonoverlapping(self.curr, dst_ptr, literal);

                //     self.output.set_len(self.output.len() + literal);
                //     self.curr = self.curr.add(literal);
                // }

                copy_from_src(&mut self.output, self.curr, literal);
                unsafe{
                    self.curr = self.curr.add(literal);
                }
            }
            


            // If the input stream is emptied, we break out of the loop. This is only the case
            // in the end of the stream, since the block is intact otherwise.
            if self.curr == self.end_pos { break; }

            // Now, we read the duplicates section.
            let mut offset:u16 = 0;
            unsafe{ptr::copy_nonoverlapping(self.curr, &mut offset as *mut u16 as *mut u8, 2);} // TODO check isLittleEndian
            // unsafe{
            //     self.curr = self.curr.add(2);
            // }

            let mut inc = 2;
            // let offset = LittleEndian::read_u16(&self.input[self.input_pos ..]);
            // self.input_pos+=2;

            // Obtain the initial match length. The match length is the length of the duplicate segment
            // which will later be copied from data previously decompressed into the output buffer. The
            // initial length is derived from the second part of the token (the lower nibble), we read
            // earlier. Since having a match length of less than 4 would mean negative compression
            // ratio, we start at 4.
            let mut match_length = (4 + (self.token & 0xF)) as usize;

            // The intial match length can maximally be 19. As with the literal length, this indicates
            // that there are more bytes to read.
            if match_length == 4 + 15 {
                // The match length took the maximal value, indicating that there is more bytes. We
                // read the extra integer.
                // match_length += self.read_integer()?;

                // let mut n = 0;
                // If this byte takes value 255 (the maximum value it can take), another byte is read
                // and added to the sum. This repeats until a byte lower than 255 is read.
                loop {
                    // We add the next byte until we get a byte which we add to the counting variable.
                    let extra = unsafe{self.curr.add(inc).read()};
                    inc +=1;
                    // unsafe{self.curr = self.curr.add(1)};
                    match_length += extra as usize;
                    // We continue if we got 255.
                    if extra != 0xFF {
                        break;
                    }
                }
            }

            self.curr = unsafe{ self.curr.add(inc)};

            // We now copy from the already decompressed buffer. This allows us for storing duplicates
            // by simply referencing the other location.

            // Calculate the start of this duplicate segment. We use wrapping subtraction to avoid
            // overflow checks, which we will catch later.
            // let start = self.output.len().wrapping_sub(offset as usize);
            let start = self.output.len() - offset as usize;

            // We'll do a bound check to avoid panicking.
            if self.output.len() < start+match_length { // TODO handle special case
                for i in start..start + match_length {
                    let b = self.output[i];
                    self.output.push(b);
                }
            }else{
                copy_on_self(&mut self.output, start, match_length);
            }

            if self.curr == self.end_pos { break; }
        }

        Ok(())
    }
}

/// Decompress all bytes of `input` into `output`.
// #[inline(never)]
pub fn decompress_into(input: &[u8], output: &mut Vec<u8>) -> Result<(), Error> {
    // Decode into our vector.
    Decoder {
        curr: input.as_ptr(),
        end_pos: unsafe{input.as_ptr().add(input.len())},
        // input: input,
        // input_pos: 0,
        output: output,
        token: 0,
    }.complete()?;

    Ok(())
}

/// Decompress all bytes of `input`.
// #[inline(never)]
pub fn decompress(input: &[u8]) -> Result<Vec<u8>, Error> {
    // Allocate a vector to contain the decompressed stream.
    let mut vec = Vec::with_capacity(4096);

    decompress_into(input, &mut vec)?;

    Ok(vec)
}

fn copy_from_src(output: &mut Vec<u8>, source: *const u8, num_items: usize) {
    output.reserve(num_items);
    unsafe{
        let dst_ptr = output.as_mut_ptr().offset(output.len() as isize);

        ptr::copy_nonoverlapping(source, dst_ptr, num_items);

        output.set_len(output.len() + num_items);
        
    }
}



// #[inline(never)]
fn copy_on_self(vec: &mut Vec<u8>, start: usize, num_items: usize) {
    vec.reserve(num_items);
    unsafe {
        std::ptr::copy_nonoverlapping(vec.as_ptr().add(start), vec.as_mut_ptr().add(vec.len()), num_items);
        vec.set_len(vec.len() + num_items);
    }
}
