use std::{fmt, io};

pub mod compress;
pub mod decompress;
pub mod header;

#[derive(Debug)]
pub enum Error {
    DecompressionError(crate::block::DecompressError),
    UnimplementedBlocksize(u8),
    UnsupportedVersion(u8),
    IoError(std::io::Error),
    ChecksumError,
    BlockTooBig,
    LinkedBlocksNotSupported,
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        io::Error::new(io::ErrorKind::Other, e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for Error {}
