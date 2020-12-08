//! The decompression algorithm.
use byteorder::{ByteOrder, LittleEndian};

#[macro_use]
extern crate quick_error;

// const FASTLOOP_SAFE_DISTANCE : usize = 64;

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
// const COMPRESSION10MB: &[u8] = include_bytes!("../../benches/compression_34k.txt");
const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/compression_66k_JSON.txt");
//
fn main() {
    let compressed = lz4_flex::compress(COMPRESSION10MB as &[u8]);
    // for _ in 0..100000 {
    decompress(&compressed).unwrap();
    // }
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
    output: &'a mut Vec<u8>,
    /// The current block's "token".
    ///
    /// This token contains to 4-bit "fields", a higher and a lower, representing the literals'
    /// length and the back reference's length, respectively. LSIC is used if either are their
    /// maximal values.
    token: u8,
    // match_usage: Vec<&'static str>,
    match_unused: u32,
    match_full: u32,
    match_fit: u32,
    match_7bit_fit: u32,
    literal_unused: u32,
    literal_full: u32,
    literal_fit: u32,
    token_not_fit: u32,
    token_fit: u32,
    offset_length_1: u32,
    offset_length_2: u32,
    offset_length_3: u32,
    offset_length_4: u32,
    offset_length_5: u32,
    offset_length_6: u32,
    offset_length_7: u32,
    offset_length_8: u32,
    offset_length_other: u32,
}

impl<'a> Decoder<'a> {
    /// Internal (partial) function for `take`.
    // #[inline(never)]
    fn move_cursor(&mut self, input: &'a [u8], n: usize) -> Result<(), Error> {
        // Check if we have enough bytes left.
        if input.len() < self.input_pos + n {
            // No extra bytes. This is clearly not expected, so we return an error.
            Err(Error::ExpectedAnotherByte)
        } else {
            // Take the first n bytes.
            // let res = &input[self.input_pos..self.input_pos+n];
            // Shift the stream to left, so that it is no longer the first byte.
            // *input = &input[n..];
            self.input_pos += n;
            // Return the former first byte.
            // res
            Ok(())
        }
    }

    // /// Pop n bytes from the start of the input stream.
    // // #[inline(never)]
    // fn take(&mut self, n: usize) -> &[u8] {
    //     self.move_cursor(&self.input, n)
    // }

    /// Write a buffer to the output stream.
    ///
    /// The reason this doesn't take `&mut self` is that we need partial borrowing due to the rules
    /// of the borrow checker. For this reason, we instead take some number of segregated
    /// references so we can read and write them independently.
    // #[inline(never)]
    fn output(output: &mut Vec<u8>, buf: &[u8]) {
        // We use simple memcpy to extend the vector.
        output.extend_from_slice(&buf);
    }

    /// Write an already decompressed match to the output stream.
    ///
    /// This is used for the essential part of the algorithm: deduplication. We start at some
    /// position `start` and then keep pushing the following element until we've added
    /// `match_length` elements.
    // #[inline(never)]
    fn duplicate(&mut self, start: usize, match_length: usize) {
        // We cannot simply use memcpy or `extend_from_slice`, because these do not allow
        // self-referential copies: http://ticki.github.io/img/lz4_runs_encoding_diagram.svg
        // `reserve` enough space on the vector to safely copy self referential data.
        if self.output.len() < start + match_length {
            // TODO handle special case
            for i in start..start + match_length {
                let b = self.output[i];
                self.output.push(b);
            }
        } else {
            copy_on_self(&mut self.output, start, match_length);
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
    fn read_integer(&mut self) -> Result<u32, Error> {
        // We start at zero and count upwards.
        let mut n: u32 = 0;
        // If this byte takes value 255 (the maximum value it can take), another byte is read
        // and added to the sum. This repeats until a byte lower than 255 is read.
        while {
            // We add the next byte until we get a byte which we add to the counting variable.
            // self.move_cursor(&self.input, 1)?;
            // check alread done in move_cursor
            let extra = *unsafe { self.input.get_unchecked(self.input_pos) };
            self.input_pos += 1;
            n += extra as u32;

            // We continue if we got 255.
            extra == 0xFF
        } {}
        Ok(n)
    }

    /// Read a little-endian 16-bit integer from the input stream.
    // #[inline(never)]
    fn read_u16(&mut self) -> Result<u16, Error> {
        // We use byteorder to read an u16 in little endian.

        let num = LittleEndian::read_u16(&self.input[self.input_pos..]);

        self.move_cursor(&self.input, 2)?;
        Ok(num)
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
    fn read_literal_section(&mut self) -> Result<(), Error> {
        // The higher token is the literals part of the token. It takes a value from 0 to 15.
        let mut literal = (self.token >> 4) as usize;

        match literal {
            0 => self.literal_unused += 1,
            15 => self.literal_full += 1,
            _ => self.literal_fit += 1,
        }

        // If the initial value is 15, it is indicated that another byte will be read and added to
        // it.
        if literal == 15 {
            // The literal length took the maximal value, indicating that there is more than 15
            // literal bytes. We read the extra integer.
            literal += self.read_integer()? as usize;
        }

        // println!("{:?}", literal);
        // Now we know the literal length. The number will be used to indicate how long the
        // following literal copied to the output buffer is.

        // Read the literals segment and output them without processing.
        let block = &self.input[self.input_pos..self.input_pos + literal];
        self.move_cursor(&self.input, literal)?;
        Self::output(&mut self.output, block);
        Ok(())
    }

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
    fn read_duplicate_section(&mut self) -> Result<(), Error> {
        // Now, we will obtain the offset which we will use to copy from the output. It is an
        // 16-bit integer.
        let offset = self.read_u16()?;
        // Obtain the initial match length. The match length is the length of the duplicate segment
        // which will later be copied from data previously decompressed into the output buffer. The
        // initial length is derived from the second part of the token (the lower nibble), we read
        // earlier. Since having a match length of less than 4 would mean negative compression
        // ratio, we start at 4.
        let mut match_length = (4 + (self.token & 0xF)) as usize;

        match match_length {
            0 => self.match_unused += 1,
            19 => self.match_full += 1,
            _ => self.match_fit += 1,
        }

        // The intial match length can maximally be 19. As with the literal length, this indicates
        // that there are more bytes to read.
        if match_length == 4 + 15 {
            // The match length took the maximal value, indicating that there is more bytes. We
            // read the extra integer.
            match_length += self.read_integer()? as usize;
        }

        if match_length < 256 {
            self.match_7bit_fit += 1;
        }

        // We now copy from the already decompressed buffer. This allows us for storing duplicates
        // by simply referencing the other location.

        // Calculate the start of this duplicate segment. We use wrapping subtraction to avoid
        // overflow checks, which we will catch later.
        let start = self.output.len().wrapping_sub(offset as usize);

        // We'll do a bound check to avoid panicking.
        if start < self.output.len() {
            if self.output.len() < start + match_length {

                // dbg!(self.offset_length_1);
                // dbg!(self.offset_length_2);
                // dbg!(self.offset_length_3);
                // dbg!(self.offset_length_4);
                // dbg!(self.offset_length_5);
                // dbg!(self.offset_length_6);
                // dbg!(self.offset_length_7);
                // dbg!(self.offset_length_8);
                // dbg!(self.offset_length_other);

                match offset {
                    1 => self.offset_length_1+=1,
                    2 => self.offset_length_2+=1,
                    3 => self.offset_length_3+=1,
                    4 => self.offset_length_4+=1,
                    5 => self.offset_length_5+=1,
                    6 => self.offset_length_6+=1,
                    7 => self.offset_length_7+=1,
                    8 => self.offset_length_8+=1,
                    _ => self.offset_length_other+=1,
                };

            }
            // Write the duplicate segment to the output buffer.
            self.duplicate(start, match_length);

            Ok(())
        } else {
            Err(Error::OffsetOutOfBounds)
        }
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
    // #[inline(never)]
    fn complete(&mut self) -> Result<(), Error> {
        // Exhaust the decoder by reading and decompressing all blocks until the remaining buffer
        // is empty.
        let in_len = self.input.len();
        // while in_len - self.input_pos >= FASTLOOP_SAFE_DISTANCE {
        //     // Read the token. The token is the first byte in a block. It is divided into two 4-bit
        //     // subtokens, the higher and the lower.

        //     self.token = unsafe{*self.input.get_unchecked(self.input_pos)};
        //     self.input_pos+=1;

        //     // Now, we read the literals section.
        //     let mut literal = (self.token >> 4) as usize;
        //     if literal == 15 {
        //         literal += self.read_integer()? as usize;
        //     }

        //     // Now we know the literal length. The number will be used to indicate how long the
        //     // following literal copied to the output buffer is.

        //     // Read the literals segment and output them without processing.
        //     let block = &self.input[self.input_pos..self.input_pos+literal];
        //     self.input_pos+=literal;
        //     Self::output(&mut self.output, block);

        //     // unsafe{std::ptr::copy_nonoverlapping(self.input.as_ptr().add(self.input_pos), self.output.as_mut_ptr(), literal);}
        //     // self.input_pos+=literal;

        //     // self.read_literal_section()?;

        //     // If the input stream is emptied, we break out of the loop. This is only the case
        //     // in the end of the stream, since the block is intact otherwise.
        //     if in_len == self.input_pos { break; }

        //     // Now, we read the duplicates section.
        //     // self.read_duplicate_section()?;
        //     // let offset = self.read_u16()?;

        //     let mut offset:u16 = 0;
        //     unsafe{std::ptr::copy_nonoverlapping(self.input.as_ptr().add(self.input_pos), &mut offset as *mut u16 as *mut u8, 2);} // TODO check isLittleEndian
        //     self.input_pos+=2;
        //     // Obtain the initial match length. The match length is the length of the duplicate segment
        //     // which will later be copied from data previously decompressed into the output buffer. The
        //     // initial length is derived from the second part of the token (the lower nibble), we read
        //     // earlier. Since having a match length of less than 4 would mean negative compression
        //     // ratio, we start at 4.
        //     let mut match_length = (4 + (self.token & 0xF)) as u32;

        //     // The intial match length can maximally be 19. As with the literal length, this indicates
        //     // that there are more bytes to read.
        //     if match_length == 4 + 15 {
        //         // The match length took the maximal value, indicating that there is more bytes. We
        //         // read the extra integer.

        //         // If this byte takes value 255 (the maximum value it can take), another byte is read
        //         // and added to the sum. This repeats until a byte lower than 255 is read.
        //         while {
        //             // We add the next byte until we get a byte which we add to the counting variable.
        //             // self.move_cursor(&self.input, 1)?;
        //             // check alread done in move_cursor
        //             let extra = *unsafe{self.input.get_unchecked(self.input_pos)};
        //             self.input_pos+=1;
        //             match_length += extra as u32;

        //             // We continue if we got 255.
        //             extra == 0xFF
        //         } {}

        //         // match_length += self.read_integer()? as usize;
        //     }

        //     // We now copy from the already decompressed buffer. This allows us for storing duplicates
        //     // by simply referencing the other location.

        //     // Calculate the start of this duplicate segment. We use wrapping subtraction to avoid
        //     // overflow checks, which we will catch later.
        //     let start = self.output.len() - offset as usize;

        //     // We'll do a bound check to avoid panicking.
        //     self.duplicate(start, match_length as usize);
        // }

        while in_len != self.input_pos {
            // Read the token. The token is the first byte in a block. It is divided into two 4-bit
            // subtokens, the higher and the lower.
            // self.token = self.take(1)[0];

            self.move_cursor(&self.input, 1)?;

            // check alread done in move_cursor
            self.token = unsafe { *self.input.get_unchecked(self.input_pos - 1) };

            // Now, we read the literals section.
            self.read_literal_section()?;

            let literal = (self.token >> 4) as usize;
            let match_length = (self.token & 0xF) as usize;
            if match_length == 15 || literal == 15 {
                self.token_not_fit += 1;
            } else {
                self.token_fit += 1;
            }

            // If the input stream is emptied, we break out of the loop. This is only the case
            // in the end of the stream, since the block is intact otherwise.
            if in_len == self.input_pos {
                break;
            }

            // Now, we read the duplicates section.
            self.read_duplicate_section()?;
        }

        dbg!(self.match_unused);
        dbg!(self.match_full);
        dbg!(self.match_fit);
        dbg!(self.match_7bit_fit);

        dbg!(self.literal_unused);
        dbg!(self.literal_full);
        dbg!(self.literal_fit);
        dbg!(self.token_not_fit);
        dbg!(self.token_fit);

        dbg!(self.offset_length_1);
        dbg!(self.offset_length_2);
        dbg!(self.offset_length_3);
        dbg!(self.offset_length_4);
        dbg!(self.offset_length_5);
        dbg!(self.offset_length_6);
        dbg!(self.offset_length_7);
        dbg!(self.offset_length_8);
        dbg!(self.offset_length_other);

        dbg!(self.literal_unused);
        dbg!(self.literal_full);
        dbg!(self.literal_fit);
        dbg!(self.token_not_fit);
        dbg!(self.token_fit);

        Ok(())
    }
}

/// Decompress all bytes of `input` into `output`.
// #[inline(never)]
pub fn decompress_into(input: &[u8], output: &mut Vec<u8>) -> Result<(), Error> {
    // Decode into our vector.
    Decoder {
        input,
        input_pos: 0,
        output,
        token: 0,
        match_unused: 0,
        match_full: 0,
        match_fit: 0,
        match_7bit_fit: 0,
        literal_unused: 0,
        literal_full: 0,
        literal_fit: 0,
        token_not_fit: 0,
        token_fit: 0,
        offset_length_1: 0,
        offset_length_2: 0,
        offset_length_3: 0,
        offset_length_4: 0,
        offset_length_5: 0,
        offset_length_6: 0,
        offset_length_7: 0,
        offset_length_8: 0,
        offset_length_other: 0,
    }
    .complete()?;

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

// #[inline(never)]
fn copy_on_self(vec: &mut Vec<u8>, start: usize, num_items: usize) {
    vec.reserve(num_items);
    unsafe {
        std::ptr::copy_nonoverlapping(
            vec.as_ptr().add(start),
            vec.as_mut_ptr().add(vec.len()),
            num_items,
        );
        vec.set_len(vec.len() + num_items);
    }
}
