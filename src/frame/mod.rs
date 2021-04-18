use std::{
    fmt,
    io::{self, Read, Write},
};

pub(crate) mod compress;
pub(crate) mod decompress;
pub(crate) mod header;

pub use compress::FrameEncoder;
pub use decompress::FrameDecoder;

#[derive(Debug)]
pub enum Error {
    SkippableFrame(u32),
    CompressionError(crate::block::CompressError),
    DecompressionError(crate::block::DecompressError),
    UnimplementedBlocksize(u8),
    UnsupportedVersion(u8),
    IoError(io::Error),
    WrongMagicNumber,
    ReservedBitsSet,
    ContentChecksumError,
    ContentLengthError { expected: u64, actual: u64 },
    BlockChecksumError,
    HeaderChecksumError,
    BlockTooBig,
    LinkedBlocksNotSupported,
    InvalidBlockInfo,
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

pub fn compress(input: &[u8]) -> Vec<u8> {
    let buffer = Vec::with_capacity(
        header::MAX_FRAME_INFO_SIZE
            + header::BLOCK_INFO_SIZE
            + crate::block::compress::get_maximum_output_size(input.len()),
    );
    let mut enc = FrameEncoder::new(buffer);
    enc.write_all(input).unwrap();
    enc.finish().unwrap()
}

pub fn decompress(input: &[u8]) -> Result<Vec<u8>, Error> {
    let mut de = FrameDecoder::new(input);
    // Preallocate the Vec with 1.5x the size of input, it may resize but it amortizes enough.
    // The upside is that we don't have to worry about DOS attacks, etc..
    let mut out = Vec::with_capacity(input.len() * 3 / 2);
    de.read_to_end(&mut out)?;
    Ok(out)
}
