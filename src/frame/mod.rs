//! LZ4 Frame Format
//!
//! As defined in <https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md>
//!
//! # Example: compress data on `stdin` with frame format
//! This program reads data from `stdin`, compresses it and emits it to `stdout`.
//! This example can be found in `examples/compress.rs`:
//! ```no_run
//! use std::io;
//! let stdin = io::stdin();
//! let stdout = io::stdout();
//! let mut rdr = stdin.lock();
//! // Wrap the stdout writer in a LZ4 Frame writer.
//! let mut wtr = lz4_flex::frame::FrameEncoder::new(stdout.lock());
//! io::copy(&mut rdr, &mut wtr).expect("I/O operation failed");
//! wtr.finish().unwrap();
//! ```
//!

use std::{fmt, io};

#[cfg_attr(feature = "safe-encode", forbid(unsafe_code))]
pub(crate) mod compress;
#[cfg_attr(feature = "safe-decode", forbid(unsafe_code))]
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
    UnsupportedBlocksize(u8),
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
    /// Content length differs.
    ContentLengthError { expected: u64, actual: u64 },
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::IoError(e) => e,
            Error::CompressionError(_)
            | Error::DecompressionError(_)
            | Error::SkippableFrame(_)
            | Error::DictionaryNotSupported => io::Error::new(io::ErrorKind::Other, e),
            Error::WrongMagicNumber
            | Error::UnsupportedBlocksize(..)
            | Error::UnsupportedVersion(..)
            | Error::ReservedBitsSet
            | Error::InvalidBlockInfo
            | Error::BlockTooBig
            | Error::HeaderChecksumError
            | Error::ContentChecksumError
            | Error::BlockChecksumError
            | Error::ContentLengthError { .. } => io::Error::new(io::ErrorKind::InvalidData, e),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        match e.get_ref().map(|e| e.downcast_ref::<Error>()) {
            Some(_) => *e.into_inner().unwrap().downcast::<Error>().unwrap(),
            None => Error::IoError(e),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for Error {}
