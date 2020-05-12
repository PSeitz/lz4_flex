/// Duplicate code here for analysis with VTune
extern crate lz4_flex;


const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/dickens.txt");

fn main() {
    // use cpuprofiler::PROFILER;
    // PROFILER.lock().unwrap().start("./my-prof.profile").unwrap();
    for _ in 0..100 {
        compress(COMPRESSION10MB as &[u8]);
    }
    // PROFILER.lock().unwrap().stop().unwrap();
}


use byteorder::{NativeEndian, ByteOrder};
use std::io::Write;

/// Duplication dictionary size.
///
/// Every four bytes is assigned an entry. When this number is lower, fewer entries exists, and
/// thus collisions are more likely, hurting the compression ratio.
// const DICTIONARY_SIZE: usize = 4096;
const DICTIONARY_SIZE: usize = 4096 / 4  * 64;


/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md#end-of-block-restrictions
/// The last match must start at least 12 bytes before the end of block. The last match is part of the penultimate sequence. 
/// It is followed by the last sequence, which contains only literals.
///
/// Note that, as a consequence, an independent block < 13 bytes cannot be compressed, because the match must copy "something", 
/// so it needs at least one prior byte.
///
/// When a block can reference data from another block, it can start immediately with a match and no literal, so a block of 12 bytes can be compressed.
const MFLIMIT: u32 = 12;
/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md#end-of-block-restrictions
/// Minimum length of a block
///
/// MFLIMIT + 1 for the token.
static LZ4_MIN_LENGTH: u32 = MFLIMIT+1;

const MATCH_LENGTH_MASK: u32 = (1_u32 << 4) - 1; // 0b1111 / 15
const MINMATCH: usize = 4;
const LZ4_HASHLOG: u32 = 12;

/// Switch for the hashtable size byU16
static LZ4_64KLIMIT: u32 = (64 * 1024) + (MFLIMIT - 1);

// #[derive(Debug)]
// enum HashtableType {
//     ByPtr, // TODO ununsed
//     ByU32, // TODO ununsed
//     ByU16  // 64k limit hashtable size
// }

/// A LZ4 block.
///
/// This defines a single compression "unit", consisting of two parts, a number of raw literals,
/// and possibly a pointer to the already encoded buffer from which to copy.
#[derive(Debug)]
struct Block {
    /// The length (in bytes) of the literals section.
    lit_len: usize,
    /// The duplicates section if any.
    ///
    /// Only the last block in a stream can lack of the duplicates section.
    dup: Option<Duplicate>,
}

/// A consecutive sequence of bytes found in already encoded part of the input.
#[derive(Copy, Clone, Debug)]
struct Duplicate {
    /// The number of bytes before our cursor, where the duplicate starts.
    offset: u16,
    /// The length beyond the four first bytes.
    ///
    /// Adding four to this number yields the actual length.
    extra_bytes: usize,
}


fn hash(sequence:u32) -> u32 {
    let res = (sequence.wrapping_mul(2654435761_u32))
            >> ((MINMATCH as u32 * 8) - (LZ4_HASHLOG + 1));
    res
}

/// An LZ4 encoder.
struct Encoder<'a, W:Write> {
    /// The raw uncompressed input.
    input: &'a [u8],
    /// The compressed output.
    output: W,
    /// The number of bytes from the input that are encoded.
    cur: usize,
    /// The number of bytes written to the output.
    bytes_written: usize,
    /// The dictionary of previously encoded sequences.
    ///
    /// This is used to find duplicates in the stream so they are not written multiple times.
    ///
    /// Every four bytes are hashed, and in the resulting slot their position in the input buffer
    /// is placed. This way we can easily look up a candidate to back references.
    dict: [u32; DICTIONARY_SIZE],
}

impl<'a, W:Write> Encoder<'a, W> {
    /// Go forward by some number of bytes.
    ///
    /// This will update the cursor and dictionary to reflect the now processed bytes.
    ///
    /// This returns `false` if all the input bytes are processed.
    #[inline(never)]
    fn go_forward(&mut self, steps: usize) -> bool {
        // Go over all the bytes we are skipping and update the cursor and dictionary.
        for _ in 0..steps {
            // Insert the cursor position into the dictionary.
            self.insert_cursor();
            // Increment the cursor.
            self.cur += 1;
        }

        // Return `true` if there's more to read.
        self.cur <= self.input.len()
    }

    /// Insert the batch under the cursor into the dictionary.
    fn insert_cursor(&mut self) {
        // Make sure that there is at least one batch remaining.
        if self.remaining_batch() {
            // Insert the cursor into the table.
            self.dict[self.get_cur_hash() as usize] = self.cur as u32 + 1;
        }
    }

    /// Check if there are any remaining batches.
    fn remaining_batch(&self) -> bool {
        self.cur + 4 < self.input.len()
    }

    /// Get the hash of the current four bytes below the cursor.
    ///
    /// This is guaranteed to be below `DICTIONARY_SIZE`.
    // fn get_cur_hash(&self) -> usize {
    //     // Use PCG transform to generate a relatively good hash of the four bytes batch at the
    //     // cursor.
    //     let mut x = self.get_batch_at_cursor().wrapping_mul(0xa4d94a4f);
    //     let a = x >> 16;
    //     let b = x >> 30;
    //     x ^= a >> b;
    //     x = x.wrapping_mul(0xa4d94a4f);

    //     x as usize % DICTIONARY_SIZE
    // }

    /// Get the hash of the current four bytes below the cursor.
    ///
    /// This is guaranteed to be below `DICTIONARY_SIZE`.
    fn get_cur_hash(&self) -> u32 {

        let sequence:u32 = self.get_batch_at_cursor();
        let res = hash(sequence);
        res

    }


    /// Read a 4-byte "batch" from some position.
    ///
    /// This will read a native-endian 4-byte integer from some position.
    fn get_batch(&self, n: usize) -> u32 {
        debug_assert!(self.remaining_batch(), "Reading a partial batch.");

        NativeEndian::read_u32(&self.input[n..])
    }

    /// Read the batch at the cursor.
    fn get_batch_at_cursor(&self) -> u32 {
        self.get_batch(self.cur)
    }

    /// Find a duplicate of the current batch.
    ///
    /// If any duplicate is found, a tuple `(position, size - 4)` is returned.
    #[inline(never)]
    fn find_duplicate(&self) -> Option<Duplicate> {
        // If there is no remaining batch, we return none.
        if !self.remaining_batch() {
            return None;
        }


        let next_batch = NativeEndian::read_u32(&self.input[self.cur..]);
        let curr_hash = hash(next_batch);
        // Find a candidate in the dictionary by hashing the current four bytes.
        // let candidate = self.dict[curr_hash as usize];
        let candidate = unsafe{*self.dict.get_unchecked(curr_hash as usize)};

        // Three requirements to the candidate exists:
        // - The candidate is not the trap value (0xFFFFFFFF), which represents an empty bucket.
        // - We should not return a position which is merely a hash collision, so w that the
        //   candidate actually matches what we search for.
        // - We can address up to 16-bit offset, hence we are only able to address the candidate if
        //   its offset is less than or equals to 0xFFFF.
        if candidate != 0
           && self.get_batch(candidate as usize  - 1) == next_batch
           && self.cur + MINMATCH < self.input.len()
           && self.cur - (candidate as usize  - 1) <= 0xFFFF {
            // Calculate the "extension bytes", i.e. the duplicate bytes beyond the batch. These
            // are the number of prefix bytes shared between the match and needle.
            // let ext = self.input[self.cur + 4..]
            //     .iter()
            //     .zip(&self.input[candidate + 4..])
            //     .take_while(|&(a, b)| a == b)
            //     .count();

            let mut ext = 0;

            // TODO check LittleEndian

            // 4byte step
            let in_len = self.input.len();
            let stepsize = 4;
            while in_len > self.cur + MINMATCH + ext + stepsize  {
                let input_block = NativeEndian::read_u32(&self.input[self.cur + MINMATCH + ext..]);
                let candidate_block = NativeEndian::read_u32(&self.input[candidate as usize - 1 + MINMATCH + ext..]);
                if input_block != candidate_block{
                    break;
                }
                ext += stepsize;
            }

            let stepsize = 2;
            if in_len > self.cur + MINMATCH + ext + stepsize {
                let input_block = NativeEndian::read_u16(&self.input[self.cur + MINMATCH + ext..]);
                let candidate_block = NativeEndian::read_u16(&self.input[candidate as usize - 1 + MINMATCH + ext..]);
                if input_block == candidate_block{
                    ext +=stepsize;
                }
            }
            let stepsize = 1;
            if in_len > self.cur + MINMATCH + ext + stepsize {
                let input_block = &self.input[self.cur + MINMATCH + ext..];
                let candidate_block = &self.input[candidate as usize  - 1 + MINMATCH + ext..];
                if input_block == candidate_block{
                    ext +=stepsize;
                }
            }


            Some(Duplicate {
                offset: (self.cur - (candidate as usize - 1)) as u16,
                extra_bytes: ext,
            })
        } else { None }
    }

    /// Write an integer to the output in LSIC format.
    fn write_integer(&mut self, mut n: usize) -> std::io::Result<()> {
        // Write the 0xFF bytes as long as the integer is higher than said value.
        while n >= 0xFF {
            n -= 0xFF;
            self.bytes_written += self.output.write(&[0xFF])?;
        }

        // Write the remaining byte.
        self.bytes_written += self.output.write(&[n as u8])?;
        Ok(())
    }

    /// Read the block of the top of the stream.
    #[inline(never)]
    fn pop_block(&mut self) -> Block {
        // The length of the literals section.
        let mut lit = 0;

        loop {
            // Search for a duplicate.
            if let Some(dup) = self.find_duplicate() {
                // We found a duplicate, so the literals section is over...

                // Move forward. Note that `ext` is actually the steps minus 4, because of the
                // minimum matchlenght, so we need to add 4.
                self.go_forward(dup.extra_bytes + 4);

                return Block {
                    lit_len: lit,
                    dup: Some(dup),
                };
            }

            // Try to move forward.
            if !self.go_forward(1) {
                // We reached the end of the stream, and no duplicates section follows.
                return Block {
                    lit_len: lit,
                    dup: None,
                };
            }

            // No duplicates found yet, so extend the literals section.
            lit += 1;
        }
    }

    /// Complete the encoding into `self.output`.
    #[inline(never)]
    fn complete(&mut self) -> std::io::Result<usize> {
        // Construct one block at a time.
        loop {
            // The start of the literals section.
            let start = self.cur;

            // Read the next block into two sections, the literals and the duplicates.
            let block = self.pop_block();

            // Generate the higher half of the token.
            let mut token = if block.lit_len < 0xF {
                // Since we can fit the literals length into it, there is no need for saturation.
                (block.lit_len as u8) << 4
            } else {
                // We were unable to fit the literals into it, so we saturate to 0xF. We will later
                // write the extensional value through LSIC encoding.
                0xF0
            };

            // Generate the lower half of the token, the duplicates length.
            let dup_extra_len = block.dup.map_or(0, |x| x.extra_bytes);
            token |= if dup_extra_len < 0xF {
                // We could fit it in.
                dup_extra_len as u8
            } else {
                // We were unable to fit it in, so we default to 0xF, which will later be extended
                // by LSIC encoding.
                0xF
            };

            // Push the token to the output stream.
            self.bytes_written += self.output.write(&[token])?;

            // If we were unable to fit the literals length into the token, write the extensional
            // part through LSIC.
            if block.lit_len >= 0xF {
                self.write_integer(block.lit_len - 0xF)?;
            }

            // Now, write the actual literals.
            let write_slice = &self.input[start..start + block.lit_len];
            self.output.write_all(write_slice)?;
            self.bytes_written += write_slice.len();

            if let Some(Duplicate { offset, .. }) = block.dup {
                // Wait! There's more. Now, we encode the duplicates section.

                // write the offset in little endian.
                self.bytes_written += self.output.write(&[offset as u8])?;
                self.bytes_written += self.output.write(&[(offset >> 8) as u8])?;

                // If we were unable to fit the duplicates length into the token, write the
                // extensional part through LSIC.
                if dup_extra_len >= 0xF {
                    self.write_integer(dup_extra_len - 0xF)?;
                }
            } else {
                break;
            }
        }
        Ok(self.bytes_written)
    }
}

/// Compress all bytes of `input` into `output`.
#[inline(never)]
pub fn compress_into<W:Write>(input: &[u8], output: W) -> std::io::Result<usize> {
    Encoder {
        bytes_written: 0,
        input: input,
        output: output,
        cur: 0,
        dict: [0; DICTIONARY_SIZE],
    }.complete()
}

/// Compress all bytes of `input`.
#[inline(never)]
pub fn compress(input: &[u8]) -> Vec<u8> {
    // In most cases, the compression won't expand the size, so we set the input size as capacity.
    let mut vec = Vec::with_capacity(input.len());

    compress_into(input, &mut vec).unwrap();

    vec
}

// #[test]
// fn yoops() {
//     const COMPRESSION66K: &'static [u8] = include_bytes!("../../benches/compression_66k_JSON.txt");
// }

#[test]
fn test_bug() {

    let input: &[u8] = &[10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18];
    let out = compress(&input);
    dbg!(&out);

}

#[test]
fn test_compare() {

    let mut input: &[u8] = &[10, 12, 14, 16];

    let mut cache = vec![];
    let mut encoder = lz4::EncoderBuilder::new().level(2).build(&mut cache).unwrap();
    // let mut read = *input;
    std::io::copy(&mut input, &mut encoder).unwrap();
    let (comp_lz4, _result) = encoder.finish();

    println!("{:?}", comp_lz4);

    let input: &[u8] = &[10, 12, 14, 16];
    let out = compress(&input);
    dbg!(&out);

}

// #[test]
// fn test_concat() {
//     let mut out = vec![];
//     compress_into(&[0], &mut out).unwrap();
//     compress_into(&[0], &mut out).unwrap();
//     dbg!(&out);

//     let mut out = vec![];
//     compress_into(&[0, 0], &mut out).unwrap();
//     dbg!(&out);
// }



// // TODO 64bit systems only currently
// /**
//  * __ffs - find first bit in word.
//  * @word: The word to search
//  *
//  * Undefined if no bit exists, so code should check against 0 first.
//  */
// fn __ffs(mut input: u32) -> u32
// {
//     let mut num: u32 = 0;
//     println!("{:b}", input);
//     println!("{:b}",  0xffff);
//     if (input & 0xffff) == 0 {
//         num += 16;
//         input >>= 16;
//     }
//     println!("{:b}", input);
//     println!("0{:b}",  0xff);
//     if (input & 0xff) == 0 {
//         num += 8;
//         input >>= 8;
//     }
//     println!("000{:b}", input);
//     println!("{:b}",  0xf);
//     if (input & 0xf) == 0 {
//         num += 4;
//         input >>= 4;
//     }
//     if (input & 0x3) == 0 {
//         num += 2;
//         input >>= 2;
//     }
//     if (input & 0x1) == 0 { 
//         num += 1;
//     }
//     return num;
// }

// TODO 64bit systems only currently
/**
 * __ffs - find first bit in word.
 * @word: The word to search
 *
 * Undefined if no bit exists, so code should check against 0 first.
 */
fn __ffs(input: u32) -> u32
{
    let mut num: u32 = 0;
    if (input & 0b00000000_11111111) == 0 {
        num += 1;
    }else{
        return num;
    }
    if (input & 0b11111111_00000000) == 0 {
        num += 1;
    }else{
        return num;
    }
    if (input & 0b11111111_00000000_00000000) == 0 {
        num += 1;
    }else{
        return num;
    }
    if (input & 0b11111111_00000000_00000000_00000000) == 0 {
        num += 1;
    }
    
    return num;
}

#[test]
fn test_ffs() {
    let input_block = [1, 3, 7, 17];
    let candidate_block = [1, 3, 7, 16];
    
    let input_block_u32 = byteorder::NativeEndian::read_u32(&input_block);
    // println!("input_block_u32: {:b}", input_block_u32);
    
    let candidate_block_u32 = byteorder::NativeEndian::read_u32(&candidate_block);
    // println!("candidate_block: {:b}", candidate_block_u32);
    // let diff = LZ4_read_ARCH(pMatch) ^ LZ4_read_ARCH(pIn);
    
    let diff = input_block_u32 ^ candidate_block_u32;

    // input_block_u32: 00010001 00000111 00000011 00000001
    // candidate_block: 00010000 00000111 00000011 00000001
    // XOR              00000001 00000000 00000000 00000000


    // XOR              00000000 00000000 00000000 01000000

    assert_eq!(__ffs(diff), 3);
}