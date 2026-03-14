# LZ4_FLEX Code Walkthrough

This document shows key code snippets and explains the core algorithms.

## Compression Algorithm Walkthrough

### Hash Table Initialization

**File: `src/block/hashtable.rs`**

```rust
// 4K entry hashtable using 16-bit values (8 KB total)
pub struct HashTable4KU16 {
    dict: Box<[u16; 4096]>,
}

impl HashTable4KU16 {
    pub fn new() -> Self {
        // Optimized allocation: uses alloc_zeroed instead of alloc + memset
        let dict = alloc::vec![0; 4096]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        Self { dict }
    }
}

// Hash function: converts 4-byte sequence to 4K table index
fn hash(sequence: u32) -> u32 {
    (sequence.wrapping_mul(2654435761_u32)) >> 16  // FNV-like hash
}
```

### Compression Main Loop

**File: `src/block/compress.rs` (lines 318-489)**

```rust
pub(crate) fn compress_internal<T: HashTable, const USE_DICT: bool, S: Sink>(
    input: &[u8],
    input_pos: usize,
    output: &mut S,
    dict: &mut T,
    ext_dict: &[u8],
    input_stream_offset: usize,
) -> Result<usize, CompressError> {
    // ... validation ...
    
    let mut literal_start = input_pos;
    let mut cur = input_pos;
    
    loop {
        let mut non_match_count = 1 << INCREASE_STEPSIZE_BITSHIFT;  // 32
        
        // Inner loop: search for matches
        loop {
            let step_size = non_match_count >> INCREASE_STEPSIZE_BITSHIFT;
            non_match_count += 1;
            
            cur = next_cur;
            next_cur += step_size;
            
            if cur > end_pos_check {  // Past safe zone
                handle_last_literals(output, input, literal_start);
                return Ok(output.pos() - output_start_pos);
            }
            
            // Hash the 4-byte sequence at current position
            let hash = T::get_hash_at(input, cur);
            let candidate = dict.get_at(hash);  // Look up in hash table
            dict.put_at(hash, cur + input_stream_offset);  // Store current position
            
            // Check if we can reach this candidate (within 64KB window)
            if input_stream_offset + cur - candidate > MAX_DISTANCE {
                continue;
            }
            
            // Verify the bytes actually match (not just hash collision)
            let cand_bytes: u32 = get_batch(candidate_source, candidate);
            let curr_bytes: u32 = get_batch(input, cur);
            
            if cand_bytes == curr_bytes {
                break;  // Found a match!
            }
        }
        
        // Extend match backwards to include preceding literals
        backtrack_match(
            input,
            &mut cur,
            literal_start,
            candidate_source,
            &mut candidate,
        );
        
        let lit_len = cur - literal_start;
        
        // Extend match forwards to find complete duplicate length
        cur += MINMATCH;  // Skip already-matched 4 bytes
        candidate += MINMATCH;
        let duplicate_length = count_same_bytes(input, &mut cur, candidate_source, candidate);
        
        // Encode: [token][literal_len?][literals][offset][match_len?]
        let token = token_from_literal_and_match_length(lit_len, duplicate_length);
        
        push_byte(output, token);
        if lit_len >= 0xF {
            write_integer(output, lit_len - 0xF);
        }
        copy_literals_wild(output, input, literal_start, lit_len);
        push_u16(output, offset);  // 16-bit offset in little-endian
        if duplicate_length >= 0xF {
            write_integer(output, duplicate_length - 0xF);
        }
        
        literal_start = cur;
    }
}
```

### Byte Matching

**File: `src/block/compress.rs` (lines 98-145)**

```rust
#[inline]
#[cfg(feature = "safe-encode")]
fn count_same_bytes(
    input: &[u8], 
    cur: &mut usize, 
    source: &[u8], 
    candidate: usize
) -> usize {
    const USIZE_SIZE: usize = core::mem::size_of::<usize>();
    let cur_slice = &input[*cur..input.len() - END_OFFSET];
    let cand_slice = &source[candidate..];
    
    let mut num = 0;
    // Compare usize-sized chunks for better performance
    for (block1, block2) in cur_slice.chunks_exact(USIZE_SIZE)
        .zip(cand_slice.chunks_exact(USIZE_SIZE))
    {
        let input_block = usize::from_ne_bytes(block1.try_into().unwrap());
        let match_block = usize::from_ne_bytes(block2.try_into().unwrap());
        
        if input_block == match_block {
            num += USIZE_SIZE;
        } else {
            // Found difference - count matching bytes via bit operations
            let diff = input_block ^ match_block;
            num += (diff.to_le().trailing_zeros() / 8) as usize;
            *cur += num;
            return num;
        }
    }
    
    // Handle remaining bytes (1-7)
    num += count_same_bytes_tail(cur_slice, cand_slice, num);
    *cur += num;
    num
}
```

## Decompression Algorithm Walkthrough

### Fast Path (Hot Loop)

**File: `src/block/decompress.rs` (lines 259-326)**

```rust
// Check if we can use optimized fast path
if does_token_fit(token)  // both nibbles < 15
    && (input_ptr as usize) <= input_ptr_safe as usize
    && output_ptr < safe_output_ptr
{
    // Token fits: literal and match lengths both < 15, no overflow bytes needed
    let literal_length = (token >> 4) as usize;
    let mut match_length = MINMATCH + (token & 0xF) as usize;
    
    // Copy literal section - bulk 16-byte copy (may overread safely)
    unsafe {
        core::ptr::copy_nonoverlapping(input_ptr, output_ptr, 16);
        input_ptr = input_ptr.add(literal_length);
        output_ptr = output_ptr.add(literal_length);
    }
    
    // Read 16-bit match offset (little-endian)
    let offset = read_u16_ptr(&mut input_ptr) as usize;
    
    let output_len = unsafe { output_ptr.offset_from(output_base) as usize };
    let offset = offset.min(output_len + ext_dict.len());
    
    // Calculate source pointer for the match
    let start_ptr = unsafe { output_ptr.sub(offset) };
    
    // Copy match - handle overlapping regions
    if offset >= match_length {
        // Non-overlapping: simple copy of 18 bytes
        unsafe {
            core::ptr::copy(start_ptr, output_ptr, 18);
            output_ptr = output_ptr.add(match_length);
        }
    } else {
        // Overlapping: must copy byte-by-byte
        unsafe {
            duplicate_overlapping(&mut output_ptr, start_ptr, match_length);
        }
    }
    
    continue;  // Back to fast path
}

// Slow path for complex tokens (see below...)
```

### Variable-Length Integer Decoding

**File: `src/block/decompress.rs` (lines 131-162)**

```rust
pub(super) fn read_integer_ptr(
    input_ptr: &mut *const u8,
    input_ptr_end: *const u8,
) -> Result<usize, DecompressError> {
    let mut n: usize = 0;
    
    loop {
        // Read next byte
        if *input_ptr >= input_ptr_end {
            return Err(DecompressError::ExpectedAnotherByte);
        }
        
        let extra = unsafe { input_ptr.read() };
        *input_ptr = unsafe { input_ptr.add(1) };
        n += extra as usize;
        
        // If byte < 255, we're done. Otherwise, continue.
        // Example: 255 + 255 + 10 = 520
        if extra != 0xFF {
            break;
        }
    }
    
    Ok(n)
}
```

### Overlapping Copy (Self-Referential)

**File: `src/block/decompress.rs` (lines 56-87)**

```rust
#[inline]
#[cfg_attr(feature = "nightly", optimize(size))]  // Prevent unrolling
unsafe fn duplicate_overlapping(
    output_ptr: &mut *mut u8,
    mut start: *const u8,
    match_length: usize,
) {
    // Safety: Write zero to handle edge case where output_ptr == start
    // This matches the C reference implementation behavior
    output_ptr.write(0u8);
    let dst_ptr_end = output_ptr.add(match_length);
    
    // Copy byte-by-byte - allows self-referential copies
    // Example: offset=1, match_len=5, data=[A] -> [A,A,A,A,A]
    while output_ptr.add(1) < dst_ptr_end {
        // Manual unroll (2 iterations) to prevent compiler unrolling
        core::ptr::copy(start, *output_ptr, 1);
        start = start.add(1);
        *output_ptr = output_ptr.add(1);
        
        core::ptr::copy(start, *output_ptr, 1);
        start = start.add(1);
        *output_ptr = output_ptr.add(1);
    }
    
    if *output_ptr < dst_ptr_end {
        core::ptr::copy(start, *output_ptr, 1);
        *output_ptr = output_ptr.add(1);
    }
}
```

## Frame Format Implementation

### Frame Encoder

**File: `src/frame/compress.rs`**

```rust
pub struct FrameEncoder<W: Write> {
    writer: W,
    context: CompressionContext,
    frame_info: FrameInfo,
}

impl<W: Write> FrameEncoder<W> {
    pub fn new(writer: W) -> Self {
        let frame_info = FrameInfo::new();
        Self {
            writer,
            context: CompressionContext::new(),
            frame_info,
        }
    }
    
    pub fn with_frame_info(writer: W, frame_info: FrameInfo) -> Self {
        Self {
            writer,
            context: CompressionContext::new(),
            frame_info,
        }
    }
}

impl<W: Write> Write for FrameEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // 1. Write frame header (if first write)
        // 2. Compress input into blocks
        // 3. Write block header (size, checksum)
        // 4. Write compressed block
        // 5. Return bytes written
        Ok(buf.len())
    }
    
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl<W: Write> FrameEncoder<W> {
    pub fn finish(mut self) -> io::Result<Vec<u8>> {
        // Write end-of-frame marker (4 bytes of zeros)
        // Write content checksum if enabled
        // Return completed compressed data
        Ok(Vec::new())
    }
}
```

### Frame Header Format

**File: `src/frame/header.rs` (lines 27-100)**

```rust
const LZ4F_MAGIC_NUMBER: u32 = 0x184D2204;  // Magic number (4 bytes)

// Frame descriptor layout:
// Byte 0: FLG (flags)
//   - Bits 7-6: Version (01 = v1)
//   - Bit 5: Independent blocks (0=linked, 1=independent)
//   - Bit 4: Block checksums enabled
//   - Bit 3: Content size present
//   - Bit 2: Content checksum enabled
//   - Bit 1: Reserved (must be 0)
//   - Bit 0: Dictionary ID present
// Byte 1: BD (block size info)
//   - Bits 7-4: Block size ID (4=64KB, 5=256KB, 6=1MB, 7=4MB, 8=8MB)
//   - Bits 3-0: Reserved (must be 0)
// [4-8 bytes]: Content size (optional, if FLG bit 3 = 1)
// [4 bytes]: Dictionary ID (optional, if FLG bit 0 = 1)
// [1 byte]: FLG checksum (CRC32 of FLG and BD bytes)

#[derive(Clone, Copy, Debug)]
pub enum BlockSize {
    Auto = 0,
    Max64KB = 4,
    Max256KB = 5,
    Max1MB = 6,
    Max4MB = 7,
    Max8MB = 8,
}

#[derive(Clone, Copy, Debug)]
pub enum BlockMode {
    Independent,  // Blocks don't reference previous blocks
    Linked,       // Blocks can reference previous blocks
}
```

## Error Handling

### Decompression Error Handling

**File: `src/block/mod.rs` (lines 79-96)**

```rust
pub enum DecompressError {
    /// Output buffer too small for decompressed data
    OutputTooSmall {
        expected: usize,
        actual: usize,
    },
    /// Literal is out of bounds of the input
    LiteralOutOfBounds,
    /// Expected another byte, but none found.
    ExpectedAnotherByte,
    /// Deduplication offset out of bounds (not in buffer).
    OffsetOutOfBounds,
}

// Usage:
match decompress(&compressed, uncompressed_size) {
    Ok(data) => println!("Decompressed: {}", String::from_utf8_lossy(&data)),
    Err(DecompressError::OutputTooSmall { expected, actual }) => {
        eprintln!("Need {} bytes, got {}", expected, actual);
    },
    Err(e) => eprintln!("Error: {}", e),
}
```

## Performance Optimization Examples

### 1. Token Fitting Check

**File: `src/block/decompress.rs` (lines 188-195)**

```rust
#[inline]
fn does_token_fit(token: u8) -> bool {
    // Check if literal length < 15 AND match length < 15
    // If true, no extension bytes needed - saves branch in hot path
    !((token & 0xF0) == 0xF0 || (token & 0x0F) == 0x0F)
}
```

### 2. Literal Copy with Overread

**File: `src/block/compress.rs` (lines 527-545)**

```rust
#[inline]
#[cfg(not(feature = "safe-encode"))]
fn copy_literals_wild(output: &mut impl Sink, input: &[u8], input_start: usize, len: usize) {
    unsafe {
        // Copy more bytes than needed, but bounds are checked
        // This prevents compiler from generating slow memcpy
        let start_ptr = input.as_ptr().add(input_start);
        match len {
            0..=8 => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), 8),
            9..=16 => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), 16),
            17..=24 => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), 24),
            _ => core::ptr::copy_nonoverlapping(start_ptr, output.pos_mut_ptr(), len),
        }
        output.set_pos(output.pos() + len);
    }
}
```

### 3. Hash Function

**File: `src/block/hashtable.rs` (lines 19-34)**

```rust
#[inline]
fn hash(sequence: u32) -> u32 {
    // FNV-1 like hash: multiply by prime, shift to get 16-bit index
    // This distributes values well across hash table
    (sequence.wrapping_mul(2654435761_u32)) >> 16
}

#[cfg(target_pointer_width = "64")]
#[inline]
fn hash5(sequence: usize) -> u32 {
    // 64-bit hash for better distribution
    let primebytes = if cfg!(target_endian = "little") {
        889523592379_usize
    } else {
        11400714785074694791_usize
    };
    (((sequence << 24).wrapping_mul(primebytes)) >> 48) as u32
}
```

## Safe vs Unsafe Variants

### Safe Encoding (Default)

**File: `src/block/compress.rs` (lines 41-43, 492-495)**

```rust
#[cfg(feature = "safe-encode")]
pub(super) fn get_batch(input: &[u8], n: usize) -> u32 {
    u32::from_ne_bytes(input[n..n + 4].try_into().unwrap())  // Bounds checked
}

#[cfg(feature = "safe-encode")]
fn push_byte(output: &mut impl Sink, el: u8) {
    output.push(el);  // Uses trait method (no unsafe)
}
```

### Unsafe Encoding (Optional)

**File: `src/block/compress.rs` (lines 34-36, 498-504)**

```rust
#[cfg(not(feature = "safe-encode"))]
pub(super) fn get_batch(input: &[u8], n: usize) -> u32 {
    unsafe { read_u32_ptr(input.as_ptr().add(n)) }  // Raw pointer read
}

#[cfg(not(feature = "safe-encode"))]
fn push_byte(output: &mut impl Sink, el: u8) {
    unsafe {
        core::ptr::write(output.pos_mut_ptr(), el);  // Direct memory write
        output.set_pos(output.pos() + 1);
    }
}
```

## No-std Support

**File: `src/lib.rs` (lines 71-78)**

```rust
#![deny(warnings)]
#![deny(missing_docs)]
#![cfg_attr(not(feature = "std"), no_std)]  // Conditional no_std
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(feature = "nightly", feature(optimize_attribute))]

#[cfg_attr(test, macro_use)]
extern crate alloc;  // Always require alloc, optionally std
```

This allows:
- Block format: works with just `alloc` (no `std`)
- Frame format: requires `std::io::{Read, Write}`

