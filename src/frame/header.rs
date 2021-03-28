use super::Error;
use std::{convert::TryInto, fmt::Debug, io::Read, mem::size_of};

mod flags {
    pub const VERSION_MASK: u8 = 0b11000000;
    pub const SUPPORTED_VERSION: u8 = 0b11000000;
    pub const BLOCK_SIZE_MASK: u8 = 0b01110000;
    pub const BLOCK_SIZE_MASK_RSHIFT: u8 = 4;

    pub const INDEPENDENT_BLOCKS: u8 = 0b00100000;
    pub const BLOCK_CHECKSUMS: u8 = 0b00010000;
    pub const CONTENT_SIZE: u8 = 0b00001000;
    pub const CONTENT_CHECKSUM: u8 = 0b00000100;
    pub const DICTIONARY_ID: u8 = 0b00000001;

    pub const UNCOMPRESSED_SIZE: u32 = 0xF0000000;
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BlockSize {
    Max64KB = 4,
    Max256KB = 5,
    Max1MB = 6,
    Max4MB = 7,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BlockMode {
    Independent,
    Linked,
}

#[allow(dead_code)]
impl BlockSize {
    pub fn get_size(&self) -> usize {
        match self {
            BlockSize::Max64KB => 64 * 1024,
            BlockSize::Max256KB => 256 * 1024,
            BlockSize::Max1MB => 1024 * 1024,
            BlockSize::Max4MB => 4 * 1024 * 1024,
        }
    }
}

/// Frame Descriptor
/// FLG     BD      (Content Size)  (Dictionary ID)     HC
/// 1 byte  1 byte  0 - 8 bytes     0 - 4 bytes         1 byte
#[derive(Debug, Clone)]
pub(crate) struct FrameInfo {
    pub content_size: Option<u64>,
    pub dict_id: Option<u32>,
    pub block_size: BlockSize,
    pub block_mode: BlockMode,
    pub block_checksums: bool,
    pub content_checksum: bool,
}

impl Default for FrameInfo {
    fn default() -> Self {
        Self {
            content_size: None,
            dict_id: None,
            block_size: BlockSize::Max64KB,
            block_mode: BlockMode::Independent,
            block_checksums: false,
            content_checksum: false,
        }
    }
}

impl FrameInfo {
    pub(crate) fn required_size(first_byte: u8) -> usize {
        let mut required = 1usize;
        if first_byte & flags::CONTENT_SIZE != 0 {
            required += 8;
        }
        if first_byte & flags::DICTIONARY_ID != 0 {
            required += 4
        }
        required
    }

    pub(crate) fn read(mut input: &[u8]) -> Result<FrameInfo, Error> {
        let mut flags = [0u8, 0];
        input.read_exact(&mut flags).map_err(Error::IoError)?;
        let flag_byte = flags[0];
        let bd_byte = flags[1];

        if flag_byte & flags::VERSION_MASK != flags::SUPPORTED_VERSION {
            // version is always 01
            return Err(Error::UnsupportedVersion(flag_byte & flags::VERSION_MASK));
        }

        let block_mode = if flag_byte & flags::INDEPENDENT_BLOCKS != 0 {
            BlockMode::Independent
        } else {
            BlockMode::Linked
        };
        let content_checksum = flag_byte & flags::CONTENT_CHECKSUM != 0;
        let block_checksums = flag_byte & flags::BLOCK_CHECKSUMS != 0;

        let block_size = match bd_byte & flags::BLOCK_SIZE_MASK >> flags::BLOCK_SIZE_MASK_RSHIFT {
            i @ 0..=3 => return Err(Error::UnimplementedBlocksize(i)),
            4 => BlockSize::Max64KB,
            5 => BlockSize::Max256KB,
            6 => BlockSize::Max1MB,
            7 => BlockSize::Max4MB,
            _ => unreachable!(),
        };

        let mut content_size = None;
        if flag_byte & flags::CONTENT_SIZE != 0 {
            let mut buffer = [0u8; size_of::<u64>()];
            input.read_exact(&mut buffer).unwrap();
            content_size = Some(u64::from_le_bytes(buffer.try_into().unwrap()));
        }

        let mut dict_id = None;
        if flag_byte & flags::DICTIONARY_ID != 0 {
            let mut buffer = [0u8; size_of::<u32>()];
            input.read_exact(&mut buffer).map_err(Error::IoError)?;
            dict_id = Some(u32::from_le_bytes(buffer.try_into().unwrap()));
        }

        {
            let mut buffer = [0u8; 1];
            input.read_exact(&mut buffer).map_err(Error::IoError)?;
            // TODO: checksum
            // ChecksumError
        }

        Ok(FrameInfo {
            block_mode,
            block_size,
            content_size,
            dict_id,
            block_checksums,
            content_checksum,
        })
    }
}

pub(crate) enum BlockInfo {
    Compressed(usize),
    Uncompressed(usize),
    EndMark,
}

impl BlockInfo {
    pub(crate) fn read(mut input: &[u8]) -> Result<Self, Error> {
        let mut size_buffer = [0u8; size_of::<u32>()];
        input.read_exact(&mut size_buffer).map_err(Error::IoError)?;
        let size = u32::from_le_bytes(size_buffer.try_into().unwrap());
        if size == 0 {
            Ok(BlockInfo::EndMark)
        } else if size & flags::UNCOMPRESSED_SIZE != 0 {
            Ok(BlockInfo::Uncompressed(
                (size & !flags::UNCOMPRESSED_SIZE) as usize,
            ))
        } else {
            Ok(BlockInfo::Compressed(size as usize))
        }
    }
}
