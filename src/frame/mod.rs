//! LZ4 Frame Format
//!
//! As defined in <https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md>

use std::{
    fmt,
    io::{self, Read, Write},
};

pub(crate) mod compress;
pub(crate) mod decompress;
pub(crate) mod header;

pub use compress::FrameEncoder;
pub use decompress::FrameDecoder;
pub use header::{BlockMode, BlockSize, FrameInfo};

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Compression error.
    CompressionError(crate::block::CompressError),
    /// Decompression error.
    DecompressionError(crate::block::DecompressError),
    /// An io::Error was encountered.
    IoError(io::Error),
    /// Unsupported block size.
    UnimplementedBlocksize(u8),
    /// Unsupported frame version.
    UnsupportedVersion(u8),
    /// Wrong magic number for the LZ4 frame format.
    WrongMagicNumber,
    /// Reserved bits set.
    ReservedBitsSet,
    /// Block header is malformed.
    InvalidBlockInfo,
    /// Read a block larger than specified in the Frame header.
    BlockTooBig,
    /// The Frame header checksum doesn't match.
    HeaderChecksumError,
    /// The block checksum doesn't match.
    BlockChecksumError,
    /// The content checksum doesn't match.
    ContentChecksumError,
    /// Read an skippable frame.
    /// The caller may read the specified amount of bytes from the underlying io::Read.
    SkippableFrame(u32),
    /// External dictionaries are not supported.
    DictionaryNotSupported,
    /// Wrong dictionary for decompression.
    WrongDictionary {
        expected: Option<u32>,
        actual: Option<u32>,
    },
    /// Content length differs.
    ContentLengthError { expected: u64, actual: u64 },
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        io::Error::new(io::ErrorKind::Other, e)
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::IoError(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for Error {}

/// Compress all bytes of `input` into a `Vec`.
pub fn compress(input: &[u8]) -> Result<Vec<u8>, Error> {
    compress_with(FrameInfo::default(), input)
}

/// Compress all bytes of `input` into a `Vec` with the specified Frame configuration.
pub fn compress_with(frame_info: FrameInfo, input: &[u8]) -> Result<Vec<u8>, Error> {
    let buffer = Vec::with_capacity(
        header::MAX_FRAME_INFO_SIZE
            + header::BLOCK_INFO_SIZE
            + crate::block::compress::get_maximum_output_size(input.len()),
    );
    let mut enc = FrameEncoder::with_frame_info(frame_info, buffer)?;
    enc.write_all(input)?;
    Ok(enc.finish()?)
}

/// Decompress all bytes of `input` into a new vec.
pub fn decompress(input: &[u8]) -> Result<Vec<u8>, Error> {
    let mut de = FrameDecoder::new(input);
    // Preallocate the output with 2x the input size, it may resize but it amortizes enough.
    // The upside is that we don't have to worry about DOS attacks, etc..
    let mut out = Vec::with_capacity(input.len() * 2);
    de.read_to_end(&mut out)?;
    Ok(out)
}

/// Compress `input` into `output`.
pub fn compress_into(input: &mut impl Read, output: &mut impl Write) -> Result<(), Error> {
    compress_into_with(Default::default(), input, output)
}

/// Compress `input` into `output` with the specified Frame configuration.
pub fn compress_into_with(
    frame_info: FrameInfo,
    input: &mut impl Read,
    output: &mut impl Write,
) -> Result<(), Error> {
    let mut enc = FrameEncoder::with_frame_info(frame_info, output)?;
    io::copy(input, &mut enc)?;
    enc.finish()?;
    Ok(())
}

/// Decompresses `input` into `output`.
pub fn decompress_into(input: &mut impl Read, output: &mut impl Write) -> Result<(), Error> {
    let mut de = FrameDecoder::new(input);
    io::copy(&mut de, output)?;
    Ok(())
}
