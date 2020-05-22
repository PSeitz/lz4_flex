/// Duplicate code here for analysis with VTune
extern crate lz4_flex;


const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/dickens.txt");

fn main() {
    // use cpuprofiler::PROFILER;
    // PROFILER.lock().unwrap().start("./my-prof.profile").unwrap();
    for _ in 0..100 {
        compress(COMPRESSION10MB as &[u8]);
    }
    // compress(COMPRESSION10MB as &[u8]);
    // PROFILER.lock().unwrap().stop().unwrap();
}

const MFLIMIT: u32 = 12;
static LZ4_MIN_LENGTH: u32 = MFLIMIT+1;
const MINMATCH: usize = 4;
#[allow(dead_code)]
const LZ4_HASHLOG: u32 = 12;

const MAXD_LOG: usize = 16;
const MAX_DISTANCE: usize = (1 << MAXD_LOG) - 1;
const END_OFFSET: usize = 5;
/// Switch for the hashtable size byU16
// #[allow(dead_code)]
// static LZ4_64KLIMIT: u32 = (64 * 1024) + (MFLIMIT - 1);


pub(crate) fn hash(sequence:u32) -> u32 {
    let res = (sequence.wrapping_mul(2654435761_u32))
            >> (1 + (MINMATCH as u32 * 8) - (LZ4_HASHLOG + 1));
    res
}




/// Duplication dictionary size.
///
/// Every four bytes is assigned an entry. When this number is lower, fewer entries exists, and
/// thus collisions are more likely, hurting the compression ratio.
/// Duplication dictionary size.
///
/// Every four bytes is assigned an entry. When this number is lower, fewer entries exists, and
/// thus collisions are more likely, hurting the compression ratio.
/// Duplication dictionary size.
///
/// Every four bytes is assigned an entry. When this number is lower, fewer entries exists, and
/// thus collisions are more likely, hurting the compression ratio.
const DICTIONARY_SIZE: usize = 4096 * 16;


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

/// An LZ4 encoder.
struct Encoder<'b> {
    /// The raw uncompressed input.
    input: *const u8,
    input_size: usize,
    /// The compressed output.
    output: &'b mut Vec<u8>,
    /// The number of bytes from the input that are encoded.
    cur: usize,
    /// The dictionary of previously encoded sequences.
    ///
    /// This is used to find duplicates in the stream so they are not written multiple times.
    ///
    /// Every four bytes are hashed, and in the resulting slot their position in the input buffer
    /// is placed. This way we can easily look up a candidate to back references.
    dict: [usize; DICTIONARY_SIZE],
}

impl<'b> Encoder<'b> {
    /// Get the hash of the current four bytes below the cursor.
    ///
    /// This is guaranteed to be below `DICTIONARY_SIZE`.
    fn get_cur_hash(&self) -> usize {
        hash(self.get_batch_at_cursor()) as usize
    }

    fn get_hash_at(&self, pos: usize) -> usize {
        hash(self.get_batch(pos)) as usize
    }

    /// Read a 4-byte "batch" from some position.
    ///
    /// This will read a native-endian 4-byte integer from some position.
    fn get_batch(&self, n: usize) -> u32 {
        let mut batch:u32 = 0;
        unsafe{std::ptr::copy_nonoverlapping(self.input.add(n), &mut batch as *mut u32 as *mut u8, 4);} 
        batch
        // NativeEndian::read_u32(&self.input[n..])
    }

    /// Read the batch at the cursor.
    fn get_batch_at_cursor(&self) -> u32 {
        self.get_batch(self.cur)
    }

    /// Write an integer to the output in LSIC format.
    fn write_integer(&mut self, mut n: usize) -> std::io::Result<()> {
        // Write the 0xFF bytes as long as the integer is higher than said value.
        while n >= 0xFF {
            n -= 0xFF;
            self.output.push(0xFF);
        }

        // Write the remaining byte.
        self.output.push(n as u8);
        Ok(())
    }

    /// Read the batch at the cursor.
    unsafe fn count_same_bytes(&self, first: *const u8, second: *const u8) -> usize {
        // let end_limit = first.len() - 5;
        let mut pos = 0;

        const STEP_SIZE: usize = 8;
        // compare 8 bytes blocks
        while pos + STEP_SIZE + END_OFFSET < self.input_size  {
            let diff = read_u64_ptr(first.add(pos)) ^ read_u64_ptr(second.add(pos));

            if diff == 0{
                pos += STEP_SIZE;
                continue;
            }else {
                return pos + get_common_bytes(diff) as usize;
            }
        }

        // compare 4 bytes block
        if pos + 4 + END_OFFSET < self.input_size  {
            let diff = read_u32_ptr(first.add(pos)) ^ read_u32_ptr(second.add(pos));

            if diff == 0{
                return pos + 4;
            }else {
                return pos + (diff.trailing_zeros() >> 3) as usize
            }
        }
        // compare 2 bytes block
        if pos + 2 + END_OFFSET < self.input_size  {
            let diff = read_u16_ptr(first.add(pos)) ^ read_u16_ptr(second.add(pos));

            if diff == 0{
                return pos + 2;
            }else {
                return pos + (diff.trailing_zeros() >> 3) as usize
            }
        }

        // TODO add end_pos_check, last 5 bytes should be literals
        if first.read() == second.read(){
            pos +=1;
        }

        pos
    }



    /// Complete the encoding into `self.output`.
    #[inline(never)]
    fn complete(&mut self) -> std::io::Result<usize> {

        /* Input too small, no compression (all literals) */
        if self.input_size < LZ4_MIN_LENGTH as usize {
            let lit_len = self.input_size;
            let token = if lit_len < 0xF {
                // Since we can fit the literals length into it, there is no need for saturation.
                (lit_len as u8) << 4
            } else {
                // We were unable to fit the literals into it, so we saturate to 0xF. We will later
                // write the extensional value through LSIC encoding.
                0xF0
            };
            self.output.push(token);
            if lit_len >= 0xF {
                self.write_integer(lit_len - 0xF)?;
            }

            // Now, write the actual literals.
            // self.output.extend_from_slice(&self.input);
            copy_into_vec(&mut self.output, self.input, self.input_size);
            return Ok(self.output.len());
        }

        let mut start = self.cur;
        self.dict[self.get_cur_hash()] = self.cur;
        self.cur += 1;
        let mut forward_hash = self.get_cur_hash();

        let end_pos_check = self.input_size - MFLIMIT as usize;
        // Construct one block at a time.
        loop {
            // The start of the literals section.

            // Read the next block into two sections, the literals and the duplicates.

            let mut match_length = usize::MAX;
            let mut offset: u16 = 0;

            let mut next_cur = self.cur;
            loop {

                let hash = forward_hash;
                self.cur = next_cur;
                next_cur += 1;
                if self.cur < end_pos_check {

                    // Find a candidate in the dictionary by hashing the current four bytes.
                    let candidate = self.dict[hash];
                    self.dict[hash] = self.cur;
                    forward_hash = self.get_hash_at(next_cur);
                    // Three requirements to the candidate exists:
                    // - We should not return a position which is merely a hash collision, so w that the
                    //   candidate actually matches what we search for.
                    // - We can address up to 16-bit offset, hence we are only able to address the candidate if
                    //   its offset is less than or equals to 0xFFFF.

                    if candidate + MAX_DISTANCE > self.cur && 
                        self.get_batch(candidate) == self.get_batch_at_cursor()
                    {

                        let duplicate = candidate;
                        match_length = unsafe{ self.count_same_bytes(self.input.add(self.cur+MINMATCH), self.input.add(duplicate+MINMATCH)) };

                        offset = (self.cur - candidate) as u16;
                        break;
                    }

                }else {
                    self.cur = self.input_size;
                    break;
                }

            }
            
            let lit_len = self.cur - start;

            // Generate the higher half of the token.
            let mut token = if lit_len < 0xF {
                // Since we can fit the literals length into it, there is no need for saturation.
                (lit_len as u8) << 4
            } else {
                // We were unable to fit the literals into it, so we saturate to 0xF. We will later
                // write the extensional value through LSIC encoding.
                0xF0
            };

            // Generate the lower half of the token, the duplicates length.
            if match_length != usize::MAX {
                
                self.cur += match_length + 4;
                // self.go_forward_2(match_length + 4);
                token |= if match_length < 0xF {
                    // We could fit it in.
                    match_length as u8
                } else {
                    // We were unable to fit it in, so we default to 0xF, which will later be extended
                    // by LSIC encoding.
                    0xF
                };
            }

            // Push the token to the output stream.
            // self.output.push(token);
            unsafe{
                std::ptr::write(self.output.as_mut_ptr().add(self.output.len()), token);
                self.output.set_len(self.output.len() + 1);
            }

            // If we were unable to fit the literals length into the token, write the extensional
            // part through LSIC.
            if lit_len >= 0xF {
                self.write_integer(lit_len - 0xF)?;
            }

            // Now, write the actual literals.
            // let write_slice = &self.input[start..start + lit_len];
            // self.output.write_all(write_slice)?;
            unsafe{copy_into_vec(&mut self.output, self.input.add(start), lit_len)};

            if match_length != usize::MAX {
                // Wait! There's more. Now, we encode the duplicates section.
                
                // write the offset in little endian.
                unsafe{
                    std::ptr::copy_nonoverlapping(&offset as *const u16 as *const u8, self.output.as_mut_ptr().add(self.output.len()), 2); // TODO only little endian supported here
                    self.output.set_len(self.output.len() +2);
                } 
                // If we were unable to fit the duplicates length into the token, write the
                // extensional part through LSIC.
                if match_length >= 0xF {
                     dbg!(match_length - 0xF);
                    self.write_integer(match_length - 0xF)?;
                }
            } else {
                break;
            }
            start = self.cur;
        }
        Ok(self.output.len())
    }
}

/// Compress all bytes of `input` into `output`.
#[inline(never)]
pub fn compress_into(input: &[u8], output: &mut Vec<u8>) -> std::io::Result<usize> {
    Encoder {
        input: input.as_ptr(),
        input_size: input.len(),
        output: output,
        cur: 0,
        dict: [0; DICTIONARY_SIZE],
    }.complete()
}

/// Compress all bytes of `input`.
#[inline(never)]
pub fn compress(input: &[u8]) -> Vec<u8> {
    // In most cases, the compression won't expand the size, so we set the input size as capacity.
    let mut vec = Vec::with_capacity((input.len() as f64 * 1.1) as usize);

    compress_into(input, &mut vec).unwrap();

    vec
}

fn copy_into_vec(vec: &mut Vec<u8>, start: *const u8, num_items: usize) {
    // vec.reserve(num_items);
    unsafe {
        std::ptr::copy_nonoverlapping(start, vec.as_mut_ptr().add(vec.len()), num_items);
        vec.set_len(vec.len() + num_items);
    }
}


fn read_u64_ptr(input: *const u8) -> usize {
    let mut num:usize = 0;
    unsafe{std::ptr::copy_nonoverlapping(input, &mut num as *mut usize as *mut u8, 8);} 
    num
}
fn read_u32_ptr(input: *const u8) -> u32 {
    let mut num:u32 = 0;
    unsafe{std::ptr::copy_nonoverlapping(input, &mut num as *mut u32 as *mut u8, 4);} 
    num
}

fn read_u16_ptr(input: *const u8) -> u16 {
    let mut num:u16 = 0;
    unsafe{std::ptr::copy_nonoverlapping(input, &mut num as *mut u16 as *mut u8, 2);} 
    num
}

// fn read_u64(input: &[u8]) -> usize {
//     let mut num:usize = 0;
//     unsafe{std::ptr::copy_nonoverlapping(input.as_ptr(), &mut num as *mut usize as *mut u8, 8);} 
//     num
// }
// fn read_u32(input: &[u8]) -> u32 {
//     let mut num:u32 = 0;
//     unsafe{std::ptr::copy_nonoverlapping(input.as_ptr(), &mut num as *mut u32 as *mut u8, 4);} 
//     num
// }

// fn read_u16(input: &[u8]) -> u16 {
//     let mut num:u16 = 0;
//     unsafe{std::ptr::copy_nonoverlapping(input.as_ptr(), &mut num as *mut u16 as *mut u8, 2);} 
//     num
// }




fn get_common_bytes(diff: usize) -> u32 {
    let tr_zeroes = diff.trailing_zeros();
    // right shift by 3, because we are only interested in 8 bit blocks
    tr_zeroes >> 3
}
