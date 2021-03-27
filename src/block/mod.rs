/*!

<https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md>

LZ4 Format
Token 1 byte[Literal Length, Match Length (Neg Offset)]   -- 0-15, 0-15
[Optional Literal Length bytes] [Literal] [Optional Match Length bytes]

100 bytes match length

[Token] 4bit
15 token
[Optional Match Length bytes] 1byte
85

Compression
match [10][4][6][100]  .....      in [10][4][6][40]
3

*/

#[cfg_attr(feature = "safe-encode", forbid(unsafe_code))]
pub mod compress;
pub mod hashtable;

#[cfg_attr(feature = "safe-decode", forbid(unsafe_code))]
pub mod decompress_safe;
#[cfg(feature = "safe-decode")]
pub use decompress_safe as decompress;

#[cfg(not(feature = "safe-decode"))]
pub mod decompress;

pub use compress::compress_prepend_size;

#[cfg(feature = "safe-decode")]
pub use decompress_safe::decompress_size_prepended;

#[cfg(not(feature = "safe-decode"))]
pub use decompress::decompress_size_prepended;

use core::convert::TryInto;
use core::{fmt, ptr};

/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md#end-of-block-restrictions
/// The last match must start at least 12 bytes before the end of block. The last match is part of the penultimate sequence.
/// It is followed by the last sequence, which contains only literals.
///
/// Note that, as a consequence, an independent block < 13 bytes cannot be compressed, because the match must copy "something",
/// so it needs at least one prior byte.
///
/// When a block can reference data from another block, it can start immediately with a match and no literal, so a block of 12 bytes can be compressed.
const MFLIMIT: usize = 12;

/// The last 5 bytes of input are always literals. Therefore, the last sequence contains at least 5 bytes.
const END_OFFSET: usize = 5;

/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md#end-of-block-restrictions
/// Minimum length of a block
///
/// MFLIMIT + 1 for the token.
const LZ4_MIN_LENGTH: usize = MFLIMIT + 1;

const MAXD_LOG: usize = 16;
const MAX_DISTANCE: usize = (1 << MAXD_LOG) - 1;

#[allow(dead_code)]
const MATCH_LENGTH_MASK: u32 = (1_u32 << 4) - 1; // 0b1111 / 15

/// The minimum length of a duplicate
const MINMATCH: usize = 4;

#[allow(dead_code)]
const FASTLOOP_SAFE_DISTANCE: usize = 64;

/// Switch for the hashtable size byU16
#[allow(dead_code)]
static LZ4_64KLIMIT: usize = (64 * 1024) + (MFLIMIT - 1);

// fn wild_copy_from_src(mut source: *const u8, mut dst_ptr: *mut u8, num_items: usize) {
//     unsafe {
//         let dst_ptr_end = dst_ptr.add(num_items);
//         while (dst_ptr as usize) < dst_ptr_end as usize {
//             ptr::copy_nonoverlapping(source, dst_ptr, 16);
//             source = source.add(16);
//             dst_ptr = dst_ptr.add(16);
//         }
//     }
// }

#[allow(dead_code)]
fn wild_copy_from_src_32(mut source: *const u8, mut dst_ptr: *mut u8, num_items: usize) {
    unsafe {
        let dst_ptr_end = dst_ptr.add(num_items);
        while (dst_ptr as usize) < dst_ptr_end as usize {
            ptr::copy_nonoverlapping(source, dst_ptr, 32);
            source = source.add(32);
            dst_ptr = dst_ptr.add(32);
        }
    }
}
#[allow(dead_code)]
fn wild_copy_from_src_16(mut source: *const u8, mut dst_ptr: *mut u8, num_items: usize) {
    unsafe {
        let dst_ptr_end = dst_ptr.add(num_items);
        while (dst_ptr as usize) < dst_ptr_end as usize {
            ptr::copy_nonoverlapping(source, dst_ptr, 16);
            source = source.add(16);
            dst_ptr = dst_ptr.add(16);
        }
    }
}

#[allow(dead_code)]
fn wild_copy_from_src_8(mut source: *const u8, mut dst_ptr: *mut u8, num_items: usize) {
    unsafe {
        let dst_ptr_end = dst_ptr.add(num_items);
        while (dst_ptr as usize) < dst_ptr_end as usize {
            ptr::copy_nonoverlapping(source, dst_ptr, 8);
            source = source.add(8);
            dst_ptr = dst_ptr.add(8);
        }
    }
}

/// An error representing invalid compressed data.
#[derive(Debug)]
pub enum DecompressError {
    /// Literal is out of bounds of the input
    OutputTooSmall {
        expected_size: usize,
        actual_size: usize,
    },
    /// Literal is out of bounds of the input
    LiteralOutOfBounds,
    /// Output is empty, but it should contain data.
    UnexpectedOutputEmpty,
    /// Expected another byte, but none found.
    ExpectedAnotherByte,
    /// Deduplication offset out of bounds (not in buffer).
    OffsetOutOfBounds,
}

impl fmt::Display for DecompressError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DecompressError::OutputTooSmall {
                expected_size,
                actual_size,
            } => {
                if *expected_size == 0 {
                    write!(
                        f,
                        "output ({:?}) is too small for the decompressed data",
                        actual_size
                    )
                } else {
                    write!(
                        f,
                        "output ({:?}) is too small for the decompressed data, {:?}",
                        actual_size, expected_size
                    )
                }
            }
            DecompressError::LiteralOutOfBounds => {
                f.write_str("literal is out of bounds of the input")
            }
            DecompressError::UnexpectedOutputEmpty => {
                f.write_str("Output is empty, but it should contain data")
            }
            DecompressError::ExpectedAnotherByte => {
                f.write_str("expected another byte, found none")
            }
            DecompressError::OffsetOutOfBounds => {
                f.write_str("the offset to copy is not contained in the decompressed buffer")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for DecompressError {}

#[inline]
fn uncompressed_size(input: &[u8]) -> Result<(usize, &[u8]), DecompressError> {
    let size = input.get(..4).ok_or(DecompressError::ExpectedAnotherByte)?;
    let size: &[u8; 4] = size.try_into().unwrap();
    let uncompressed_size = u32::from_le_bytes(*size) as usize;
    let rest = &input[4..];
    Ok((uncompressed_size, rest))
}
