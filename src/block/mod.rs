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

#[cfg(feature = "ultra")]
#[cfg_attr(feature = "safe-encode", forbid(unsafe_code))]
pub(crate) mod ultra;

pub use compress::*;
pub use decompress::*;

/// Selects which compressor the `compress_*_with_mode` entry points (and the
/// [frame encoder][crate::frame::FrameEncoder::set_compression_mode]) use.
///
/// [`Fast`](Self::Fast) is the default greedy/lazy compressor (exactly [`compress`]); the other two
/// are optimal-parse "ultra" engines that compress better at the cost of speed/memory.
#[cfg(feature = "ultra")]
#[cfg_attr(docsrs, doc(cfg(feature = "ultra")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionMode {
    /// Fast greedy/lazy compressor — identical to [`compress`]. The default.
    #[default]
    Fast,
    /// Optimal-parse suffix-array compressor. Best ratio and a decode-optimised stream (favours
    /// decompression speed), but the slowest and most memory-hungry.
    Ultra,
    /// Hash-chain optimal compressor (matches `lz4hc -12`): ratio close to [`Ultra`](Self::Ultra),
    /// several times faster, far less memory.
    Hc,
}

#[cfg(feature = "ultra")]
impl CompressionMode {
    /// `(favor, finder)` for the optimal-parse engines; `None` for [`Fast`](Self::Fast).
    #[inline]
    pub(crate) fn ultra_engine(self) -> Option<(ultra::Favor, ultra::Finder)> {
        match self {
            CompressionMode::Fast => None,
            CompressionMode::Ultra => {
                Some((ultra::Favor::DecompressionSpeed, ultra::Finder::SuffixArray))
            }
            CompressionMode::Hc => Some((ultra::Favor::Ratio, ultra::Finder::Lz4Hc)),
        }
    }
}

/// Compress `input` with the chosen [`CompressionMode`], optionally using `dict` for lookback
/// (pass `b""` for none). With [`CompressionMode::Fast`] this is exactly [`compress`] /
/// [`compress_with_dict`]. The result is a standard LZ4 block decodable by [`decompress`].
#[cfg(feature = "ultra")]
#[cfg_attr(docsrs, doc(cfg(feature = "ultra")))]
pub fn compress_with_mode(input: &[u8], dict: &[u8], mode: CompressionMode) -> alloc::vec::Vec<u8> {
    match mode.ultra_engine() {
        None if dict.is_empty() => compress(input),
        None => compress_with_dict(input, dict),
        Some((favor, finder)) => ultra::compress_into_vec(input, false, dict, favor, finder),
    }
}

/// Like [`compress_with_mode`], prepending the uncompressed size as a little-endian `u32` (pairs
/// with [`decompress_size_prepended`] / [`decompress_size_prepended_with_dict`]).
#[cfg(feature = "ultra")]
#[cfg_attr(docsrs, doc(cfg(feature = "ultra")))]
pub fn compress_prepend_size_with_mode(
    input: &[u8],
    dict: &[u8],
    mode: CompressionMode,
) -> alloc::vec::Vec<u8> {
    match mode.ultra_engine() {
        None if dict.is_empty() => compress_prepend_size(input),
        None => compress_prepend_size_with_dict(input, dict),
        Some((favor, finder)) => ultra::compress_into_vec(input, true, dict, favor, finder),
    }
}

/// Like [`compress_with_mode`], writing into the pre-allocated `output` (size it with
/// [`get_maximum_output_size`]). Returns the number of bytes written.
#[cfg(feature = "ultra")]
#[cfg_attr(docsrs, doc(cfg(feature = "ultra")))]
pub fn compress_into_with_mode(
    input: &[u8],
    output: &mut [u8],
    dict: &[u8],
    mode: CompressionMode,
) -> Result<usize, CompressError> {
    match mode.ultra_engine() {
        None if dict.is_empty() => compress_into(input, output),
        None => compress_into_with_dict(input, output, dict),
        Some((favor, finder)) => {
            let mut sink = crate::sink::SliceSink::new(output, 0);
            ultra::compress_one_shot(input, dict, favor, finder, &mut sink)
        }
    }
}

use core::{error::Error, fmt};

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
    /// The provided output is too small
    OutputTooSmall {
        /// Minimum expected output size
        expected: usize,
        /// Actual size of output
        actual: usize,
    },
    /// Literal is out of bounds of the input
    LiteralOutOfBounds,
    /// Expected another byte, but none found.
    ExpectedAnotherByte,
    /// Match offset is 0
    OffsetZero,
    /// Deduplication offset out of bounds (not in buffer).
    OffsetOutOfBounds,
}

#[derive(Debug)]
#[non_exhaustive]
/// Errors that can happen during compression.
pub enum CompressError {
    /// The provided output is too small.
    OutputTooSmall,
}

impl fmt::Display for DecompressError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DecompressError::OutputTooSmall { expected, actual } => {
                write!(
                    f,
                    "provided output is too small for the decompressed data, actual {actual}, expected \
                     {expected}"
                )
            }
            DecompressError::LiteralOutOfBounds => {
                f.write_str("literal is out of bounds of the input")
            }
            DecompressError::ExpectedAnotherByte => {
                f.write_str("expected another byte, found none")
            }
            DecompressError::OffsetZero => f.write_str("0 is not a valid match offset"),
            DecompressError::OffsetOutOfBounds => {
                f.write_str("the offset to copy is not contained in the decompressed buffer")
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

impl Error for DecompressError {}

impl Error for CompressError {}

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

#[test]
#[cfg(target_pointer_width = "64")] // only relevant for 64bit CPUs
fn large_integer_roundtrip() {
    let u32_max = usize::try_from(u32::MAX).unwrap();
    let value = u32_max + u32_max / 2;

    let mut buf = vec![0u8; value / 255 + 1];
    let mut sink = crate::sink::SliceSink::new(&mut buf, 0);
    self::compress::write_integer(&mut sink, value);

    #[cfg(feature = "safe-decode")]
    let value_decompressed = self::decompress_safe::read_integer(&buf, &mut 0).unwrap();

    #[cfg(not(feature = "safe-decode"))]
    let value_decompressed = {
        let mut ptr_range = buf.as_ptr_range();
        self::decompress::read_integer_ptr(&mut ptr_range.start, ptr_range.end).unwrap()
    };

    assert_eq!(value, value_decompressed);
}
