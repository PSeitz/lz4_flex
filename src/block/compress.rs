//! The compression algorithm.
//!
//! We make use of hash tables to find duplicates. This gives a reasonable compression ratio with a
//! high performance. It has fixed memory usage, which contrary to other approachs, makes it less
//! memory hungry.

use crate::block::wild_copy_from_src;
use crate::block::END_OFFSET;
use crate::block::MFLIMIT;
use crate::block::MAX_DISTANCE;
use crate::block::LZ4_MIN_LENGTH;
use crate::block::MINMATCH;
use crate::block::hash;

/// Duplication dictionary size.
///
/// Every four bytes is assigned an entry. When this number is lower, fewer entries exists, and
/// thus collisions are more likely, hurting the compression ratio.
// const DICTIONARY_SIZE: usize = 4096 * 16;
const DICTIONARY_SIZE: usize = 4096;


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
struct Encoder{
    /// The raw uncompressed input.
    input: *const u8,
    input_size: usize,
    /// The compressed output.
    // output: &'b mut Vec<u8>,
    output_ptr: *mut u8,
    /// The number of bytes from the input that are encoded.
    cur: usize,
    /// Shift the hash value for the dictionary to the right, so match the dictionary size.
    dict_bitshift: u8,
    /// The dictionary of previously encoded sequences.
    ///
    /// This is used to find duplicates in the stream so they are not written multiple times.
    ///
    /// Every four bytes are hashed, and in the resulting slot their position in the input buffer
    /// is placed. This way we can easily look up a candidate to back references.
    dict: Vec<usize>,
}

impl Encoder {
    /// Get the hash of the current four bytes below the cursor.
    ///
    /// This is guaranteed to be below `DICTIONARY_SIZE`.
    fn get_cur_hash(&self) -> usize {
        hash(self.get_batch_at_cursor()) as usize >> self.dict_bitshift
    }

    fn get_hash_at(&self, pos: usize) -> usize {
        hash(self.get_batch(pos)) as usize >> self.dict_bitshift
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
            unsafe{
                self.output_ptr.write(0xFF);
                self.output_ptr = self.output_ptr.add(1);
            }
        }

        // Write the remaining byte.
        unsafe{
            self.output_ptr.write(n as u8);
            self.output_ptr = self.output_ptr.add(1);
        }
        Ok(())
    }

    /// Read the batch at the cursor.
    unsafe fn count_same_bytes(&self, first: *const u8, second: *const u8) -> usize {
        let mut pos = 0;

        // compare 4/8 bytes blocks depending on the arch
        const STEP_SIZE: usize = std::mem::size_of::<usize>();
        while pos + STEP_SIZE + END_OFFSET < self.input_size  {
            let diff = read_usize_ptr(first.add(pos)) ^ read_usize_ptr(second.add(pos));

            if diff == 0{
                pos += STEP_SIZE;
                continue;
            }else {
                return pos + get_common_bytes(diff) as usize;
            }
        }

        // compare 4 bytes block
        #[cfg(target_pointer_width = "64")]{
            if pos + 4 + END_OFFSET < self.input_size  {
                let diff = read_u32_ptr(first.add(pos)) ^ read_u32_ptr(second.add(pos));

                if diff == 0{
                    return pos + 4;
                }else {
                    return pos + (diff.trailing_zeros() >> 3) as usize
                }
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
    fn complete(&mut self) -> std::io::Result<usize> {
        let out_ptr_start = self.output_ptr;
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
            push_unsafe(&mut self.output_ptr, token);
            // self.output.push(token);
            if lit_len >= 0xF {
                self.write_integer(lit_len - 0xF)?;
            }

            // Now, write the actual literals.
            // self.output.extend_from_slice(&self.input);
            copy_into_vec(&mut self.output_ptr, self.input, self.input_size);
            return Ok(self.output_ptr as usize - out_ptr_start as usize);
        }

        let mut start = self.cur;
        let hash = self.get_cur_hash();
        self.dict[hash] = self.cur;
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
            push_unsafe(&mut self.output_ptr, token);
            // If we were unable to fit the literals length into the token, write the extensional
            // part through LSIC.
            if lit_len >= 0xF {
                self.write_integer(lit_len - 0xF)?;
            }

            // Now, write the actual literals.
            unsafe{
                wild_copy_from_src(self.input.add(start), self.output_ptr, lit_len); // TODO add wildcopy check 8byte
                self.output_ptr = self.output_ptr.add(lit_len);
            }

            // unsafe{copy_into_vec(&mut self.output_ptr, self.input.add(start), lit_len)};

            if match_length != usize::MAX {
                // Wait! There's more. Now, we encode the duplicates section.
                
                // write the offset in little endian.
                unsafe{
                    std::ptr::copy_nonoverlapping(&offset.to_le() as *const u16 as *const u8, self.output_ptr, 2);
                    self.output_ptr = self.output_ptr.add(2);
                } 
                // If we were unable to fit the duplicates length into the token, write the
                // extensional part through LSIC.
                if match_length >= 0xF {
                    self.write_integer(match_length - 0xF)?;
                }
            } else {
                break;
            }
            start = self.cur;
        }
        Ok(self.output_ptr as usize - out_ptr_start as usize)
    }
}

/// Compress all bytes of `input` into `output`.
pub fn compress_into(input: &[u8], output: &mut Vec<u8>) -> std::io::Result<usize> {
    // TODO check dictionary sizes for input input_sizes
    let (dict_size, dict_bitshift) = match input.len() {
        0..=500 => (128, 9),
        500..=1_000 => (256, 8),
        1_000..=4_000 => (512, 7),
        4_000..=8_000 => (1024, 6),
        8_000..=16_000 => (2048, 5),
        16_000..=100_000 => (4096, 4),
        100_000..=400_000 => (8192, 3),
        _ => (16384, 2),
    };
    let dict = vec![0; dict_size];

    Encoder {
        input: input.as_ptr(),
        input_size: input.len(),
        output_ptr: output.as_mut_ptr(),
        dict_bitshift: dict_bitshift,
        cur: 0,
        dict,
    }.complete()
}

// /// Compress all bytes of `input` into `output`.
// pub fn compress_into_2(input: &[u8], output: &mut Vec<u8>, dict_size: usize, dict_bitshift: u8) -> std::io::Result<usize> {
//     let dict = vec![0; dict_size];
//     dbg!(input.len(), dict_size);
//     Encoder {
//         input: input.as_ptr(),
//         input_size: input.len(),
//         output_ptr: output.as_mut_ptr(),
//         dict_bitshift,
//         cur: 0,
//         dict,
//     }.complete()
// }

fn push_unsafe(output: &mut *mut u8, el: u8) {
    unsafe{
        std::ptr::write(*output, el);
        *output = output.add(1);
    }
}

/// Compress all bytes of `input`.
pub fn compress(input: &[u8]) -> Vec<u8> {
    // In most cases, the compression won't expand the size, so we set the input size as capacity.
    let mut vec = Vec::with_capacity(10 + (input.len() as f64 * 1.1) as usize);

    let bytes_written = compress_into(input, &mut vec).unwrap();
    unsafe{
        vec.set_len(bytes_written);
    }
    vec
}

fn copy_into_vec(out_ptr:&mut *mut u8, start: *const u8, num_items: usize) {
    // vec.reserve(num_items);
    unsafe {
        std::ptr::copy_nonoverlapping(start, *out_ptr, num_items);
        *out_ptr = out_ptr.add(num_items);
        // vec.set_len(vec.len() + num_items);
    }
}
// fn copy_into_vec(vec: &mut Vec<u8>, start: *const u8, num_items: usize) {
//     // vec.reserve(num_items);
//     unsafe {
//         std::ptr::copy_nonoverlapping(start, vec.as_mut_ptr().add(vec.len()), num_items);
//         vec.set_len(vec.len() + num_items);
//     }
// }


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

fn read_usize_ptr(input: *const u8) -> usize {
    let mut num:usize = 0;
    unsafe{std::ptr::copy_nonoverlapping(input, &mut num as *mut usize as *mut u8, std::mem::size_of::<usize>());} 
    num
}

fn read_u16_ptr(input: *const u8) -> u16 {
    let mut num:u16 = 0;
    unsafe{std::ptr::copy_nonoverlapping(input, &mut num as *mut u16 as *mut u8, 2);} 
    num
}

fn read_u64(input: &[u8]) -> usize {
    let mut num:usize = 0;
    unsafe{std::ptr::copy_nonoverlapping(input.as_ptr(), &mut num as *mut usize as *mut u8, 8);} 
    num
}
fn read_u32(input: &[u8]) -> u32 {
    let mut num:u32 = 0;
    unsafe{std::ptr::copy_nonoverlapping(input.as_ptr(), &mut num as *mut u32 as *mut u8, 4);} 
    num
}

fn read_u16(input: &[u8]) -> u16 {
    let mut num:u16 = 0;
    unsafe{std::ptr::copy_nonoverlapping(input.as_ptr(), &mut num as *mut u16 as *mut u8, 2);} 
    num
}




fn get_common_bytes(diff: usize) -> u32 {
    let tr_zeroes = diff.trailing_zeros();
    // right shift by 3, because we are only interested in 8 bit blocks
    tr_zeroes >> 3
}

#[test]
#[cfg(target_pointer_width = "64")]
fn test_get_common_bytes(){
    let num1 = read_u64(&[0,0,0,0,0,0,0,1]);
    let num2 = read_u64(&[0,0,0,0,0,0,0,2]);
    let diff = num1 ^ num2;

    assert_eq!(get_common_bytes(diff), 7);

    let num1 = read_u64(&[0,0,0,0,0,0,1,1]);
    let num2 = read_u64(&[0,0,0,0,0,0,0,2]);
    let diff = num1 ^ num2;
    assert_eq!(get_common_bytes(diff), 6);
    let num1 = read_u64(&[1,0,0,0,0,0,1,1]);
    let num2 = read_u64(&[0,0,0,0,0,0,0,2]);
    let diff = num1 ^ num2;
    assert_eq!(get_common_bytes(diff), 0);
}

#[test]
#[cfg(target_pointer_width = "32")]
fn test_get_common_bytes(){
    let num1 = read_u32(&[0,0,0,1]);
    let num2 = read_u32(&[0,0,0,2]);
    let diff = num1 ^ num2;

    assert_eq!(get_common_bytes(diff as usize), 3);

    let num1 = read_u32(&[0,0,1,1]);
    let num2 = read_u32(&[0,0,0,2]);
    let diff = num1 ^ num2;
    assert_eq!(get_common_bytes(diff as usize), 2);
    let num1 = read_u32(&[1,0,1,1]);
    let num2 = read_u32(&[0,0,0,2]);
    let diff = num1 ^ num2;
    assert_eq!(get_common_bytes(diff as usize), 0);
}


/// Write an integer to the output in LSIC format.
// fn write_integer(&mut self, mut n: usize) -> std::io::Result<()> {
//     // Write the 0xFF bytes as long as the integer is higher than said value.

//     unsafe{
//         std::ptr::copy_nonoverlapping(&u32::MAX as *const u32 as *const u8, self.output_ptr, 4);
//         // self.output_ptr.write_unaligned(0xFFFFFFFF);
//     }
//     while n >= 4 * 0xFF {
//         unsafe{ self.output_ptr = self.output_ptr.add(4); }
//         n -= 4 * 0xFF;
//         unsafe{
//             std::ptr::copy_nonoverlapping(&u32::MAX as *const u32 as *const u8, self.output_ptr, 4);
//             // self.output_ptr.write_unaligned(0xFFFFFFFF);
//         }
//     }

//     // Write the remaining byte.
//     unsafe{
//         self.output_ptr = self.output_ptr.add(n/255);
//         self.output_ptr.write((n % 255) as u8);
//         self.output_ptr = self.output_ptr.add(1);
//     }
//     Ok(())
// }


// #[test]
// fn test_count_same_bytes() {
//     // 8byte aligned block
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4];
//     assert_eq!(count_same_bytes(&first, &second), 16);

//     // 4byte aligned block
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4];
//     assert_eq!(count_same_bytes(&first, &second), 20);

//     // 2byte aligned block
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4];
//     assert_eq!(count_same_bytes(&first, &second), 22);

//     // 1byte aligned block
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5];
//     assert_eq!(count_same_bytes(&first, &second), 23);

//     // 1byte aligned block - last byte different
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6];
//     assert_eq!(count_same_bytes(&first, &second), 22);

//     // 1byte aligned block
//     let first:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 9, 5] ;
//     let second:&[u8] = &[1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6];
//     assert_eq!(count_same_bytes(&first, &second), 21);
// }


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
