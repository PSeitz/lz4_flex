//! LZ4 Block Format
//!
//! As defined in <https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md>
//!
//! Currently for no_std support only the block format is supported.
//!
//! # Example: block format roundtrip
//! ```
//! use lz4_flex::block::{compress_prepend_size, decompress_size_prepended};
//! let input: &[u8] = b"Hello people, what's up?";
//! let compressed = compress_prepend_size(input);
//! let uncompressed = decompress_size_prepended(&compressed).unwrap();
//! assert_eq!(input, uncompressed);
//! ```
//!

#[cfg_attr(feature = "safe-encode", forbid(unsafe_code))]
pub(crate) mod compress;
pub(crate) mod hashtable;

#[cfg(feature = "safe-decode")]
#[cfg_attr(feature = "safe-decode", forbid(unsafe_code))]
pub(crate) mod decompress_safe;
#[cfg(feature = "safe-decode")]
pub(crate) use decompress_safe as decompress;

#[cfg(not(feature = "safe-decode"))]
pub(crate) mod decompress;

pub use compress::*;
pub use decompress::*;

use core::convert::TryInto;
use core::fmt;

pub(crate) const WINDOW_SIZE: usize = 64 * 1024;

/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md#end-of-block-restrictions
/// The last match must start at least 12 bytes before the end of block. The last match is part of
/// the penultimate sequence. It is followed by the last sequence, which contains only literals.
///
/// Note that, as a consequence, an independent block < 13 bytes cannot be compressed, because the
/// match must copy "something", so it needs at least one prior byte.
///
/// When a block can reference data from another block, it can start immediately with a match and no
/// literal, so a block of 12 bytes can be compressed.
const MFLIMIT: usize = 12;

/// The last 5 bytes of input are always literals. Therefore, the last sequence contains at least 5
/// bytes.
const LAST_LITERALS: usize = 5;

/// Due the way the compression loop is arrange we may read up to (register_size - 2) bytes from the
/// current position. So we must end the matches 6 bytes before the end, 1 more than required by the
/// spec.
const END_OFFSET: usize = LAST_LITERALS + 1;

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

/// An error representing invalid compressed data.
#[derive(Debug)]
#[non_exhaustive]
pub enum DecompressError {
    OutputTooSmall {
        expected: usize,
        actual: usize,
    },
    UncompressedSizeDiffers {
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

#[derive(Debug)]
#[non_exhaustive]
pub enum CompressError {
    OutputTooSmall,
}

impl fmt::Display for DecompressError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DecompressError::OutputTooSmall { expected, actual } => {
                write!(
                    f,
                    "provided output is too small for the decompressed data, actual {}, expected \
                     {}",
                    actual, expected
                )
            }
            DecompressError::LiteralOutOfBounds => {
                f.write_str("literal is out of bounds of the input")
            }
            DecompressError::ExpectedAnotherByte => {
                f.write_str("expected another byte, found none")
            }
            DecompressError::OffsetOutOfBounds => {
                f.write_str("the offset to copy is not contained in the decompressed buffer")
            }
            DecompressError::UncompressedSizeDiffers { actual, expected } => {
                write!(
                    f,
                    "the expected decompressed size differs, actual {}, expected {}",
                    actual, expected
                )
            }
        }
    }
}

impl fmt::Display for CompressError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CompressError::OutputTooSmall => f.write_str(
                "output is too small for the compressed data, use get_maximum_output_size to \
                 reserve enough space",
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for DecompressError {}

#[cfg(feature = "std")]
impl std::error::Error for CompressError {}

/// This can be used in conjunction with `decompress_size_prepended`.
/// It will read the first 4 bytes as little-endian encoded length, and return
/// the rest of the bytes after the length encoding.
#[inline]
pub fn uncompressed_size(input: &[u8]) -> Result<(usize, &[u8]), DecompressError> {
    let size = input.get(..4).ok_or(DecompressError::ExpectedAnotherByte)?;
    let size: &[u8; 4] = size.try_into().unwrap();
    let uncompressed_size = u32::from_le_bytes(*size) as usize;
    let rest = &input[4..];
    Ok((uncompressed_size, rest))
}
