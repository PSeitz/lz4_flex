use twox_hash::XxHash32;

use super::Error;
use std::{
    convert::TryInto,
    fmt::Debug,
    hash::Hasher,
    io,
    io::{Read, Write},
    mem::size_of,
};

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

    pub const MAGIC_NUMBER: u32 = 0x184D2204;
    pub const SKIPPABLE_MAGIC: std::ops::RangeInclusive<u32> = 0x184D2A50..=0x184D2A5F;
}

pub(crate) const MAX_FRAME_INFO_SIZE: usize = 19;
pub(crate) const BLOCK_INFO_SIZE: usize = 4;

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

/*
From: https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md

General Structure of LZ4 Frame format
-------------------------------------

| MagicNb | F. Descriptor | Block | (...) | EndMark | C. Checksum |
|:-------:|:-------------:| ----- | ----- | ------- | ----------- |
| 4 bytes |  3-15 bytes   |       |       | 4 bytes | 0-4 bytes   |

Frame Descriptor
----------------

| FLG     | BD      | (Content Size) | (Dictionary ID) | HC      |
| ------- | ------- |:--------------:|:---------------:| ------- |
| 1 byte  | 1 byte  |  0 - 8 bytes   |   0 - 4 bytes   | 1 byte  |

__FLG byte__

|  BitNb  |  7-6  |   5   |    4     |  3   |    2     |    1     |   0  |
| ------- |-------|-------|----------|------|----------|----------|------|
|FieldName|Version|B.Indep|B.Checksum|C.Size|C.Checksum|*Reserved*|DictID|

__BD byte__

|  BitNb  |     7    |     6-5-4     |  3-2-1-0 |
| ------- | -------- | ------------- | -------- |
|FieldName|*Reserved*| Block MaxSize |*Reserved*|

Data Blocks
-----------

| Block Size |  data  | (Block Checksum) |
|:----------:| ------ |:----------------:|
|  4 bytes   |        |   0 - 4 bytes    |

*/
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
    pub(crate) fn read_size(mut input: &[u8]) -> Result<usize, Error> {
        let mut required = 7;
        if input.len() < 7 {
            return Ok(required);
        }

        let magic_num = {
            let mut buffer = [0u8; size_of::<u32>()];
            input.read_exact(&mut buffer)?;
            u32::from_le_bytes(buffer)
        };

        if flags::SKIPPABLE_MAGIC.contains(&magic_num) {
            return Ok(8);
        }
        if magic_num != flags::MAGIC_NUMBER {
            return Err(Error::WrongMagicNumber);
        }

        if input[4] & flags::CONTENT_SIZE != 0 {
            required += 8;
        }
        if input[4] & flags::DICTIONARY_ID != 0 {
            required += 4
        }
        Ok(required)
    }

    pub(crate) fn write_size(&self) -> usize {
        let mut required = 7;
        if self.content_size.is_some() {
            required += 8;
        }
        if self.dict_id.is_some() {
            required += 4;
        }
        required
    }

    pub(crate) fn write(&self, mut output: &mut [u8]) -> Result<usize, Error> {
        let write_size = self.write_size();
        if output.len() < write_size {
            return Err(Error::IoError(io::ErrorKind::UnexpectedEof.into()));
        }
        let mut buffer = [0u8; MAX_FRAME_INFO_SIZE];
        assert!(write_size <= buffer.len());
        buffer[0..4].copy_from_slice(&flags::MAGIC_NUMBER.to_le_bytes());
        buffer[4] = flags::SUPPORTED_VERSION;
        if self.block_mode == BlockMode::Independent {
            buffer[4] |= flags::INDEPENDENT_BLOCKS;
        }
        buffer[5] = (self.block_size as u8) << flags::BLOCK_SIZE_MASK_RSHIFT;
        if let Some(size) = self.content_size {
            buffer[4] |= flags::CONTENT_SIZE;
            buffer[5..5 + 8].copy_from_slice(&size.to_le_bytes());
        }
        if let Some(dict_id) = self.dict_id {
            buffer[4] |= flags::DICTIONARY_ID;
            let offset = if self.content_size.is_some() {
                5 + 8
            } else {
                5
            };
            buffer[offset..offset + 4].copy_from_slice(&dict_id.to_le_bytes());
        }
        let mut hasher = XxHash32::with_seed(0);
        hasher.write(&buffer[..write_size - 1]);
        let checksum = (hasher.finish() >> 8) as u8;
        buffer[write_size - 1] = checksum;
        output.write_all(&buffer[..write_size])?;
        Ok(self.write_size())
    }

    pub(crate) fn read(mut input: &[u8]) -> Result<FrameInfo, Error> {
        let original_input = input;
        // 4 byte Magic
        let magic_num = {
            let mut buffer = [0u8; size_of::<u32>()];
            input.read_exact(&mut buffer)?;
            u32::from_le_bytes(buffer)
        };
        if magic_num != flags::MAGIC_NUMBER {
            return Err(Error::WrongMagicNumber);
        }
        if flags::SKIPPABLE_MAGIC.contains(&magic_num) {
            let mut buffer = [0u8; size_of::<u32>()];
            input.read_exact(&mut buffer)?;
            let user_data_len = u32::from_le_bytes(buffer.try_into().unwrap());
            return Err(Error::SkippableFrame(user_data_len));
        }

        // fixed size section
        let (flag_byte, bd_byte) = {
            let mut buffer = [0u8, 0];
            input.read_exact(&mut buffer)?;
            (buffer[0], buffer[1])
        };

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

        // var len section
        let mut content_size = None;
        if flag_byte & flags::CONTENT_SIZE != 0 {
            let mut buffer = [0u8; size_of::<u64>()];
            input.read_exact(&mut buffer).unwrap();
            content_size = Some(u64::from_le_bytes(buffer.try_into().unwrap()));
        }

        let mut dict_id = None;
        if flag_byte & flags::DICTIONARY_ID != 0 {
            let mut buffer = [0u8; size_of::<u32>()];
            input.read_exact(&mut buffer)?;
            dict_id = Some(u32::from_le_bytes(buffer.try_into().unwrap()));
        }

        // 1 byte header checksum
        let expected_checksum = {
            let mut buffer = [0u8; 1];
            input.read_exact(&mut buffer)?;
            buffer[0]
        };
        let mut hasher = XxHash32::with_seed(0);
        hasher.write(&original_input[..original_input.len() - input.len()]);
        let header_hash = (hasher.finish() >> 8) as u8;
        if header_hash != expected_checksum {
            return Err(Error::HeaderChecksumError);
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
    Compressed(u32),
    Uncompressed(u32),
    EndMark,
}

impl BlockInfo {
    pub(crate) fn read(mut input: &[u8]) -> Result<Self, Error> {
        let mut size_buffer = [0u8; size_of::<u32>()];
        input.read_exact(&mut size_buffer)?;
        let size = u32::from_le_bytes(size_buffer.try_into().unwrap());
        if size == 0 {
            Ok(BlockInfo::EndMark)
        } else if size & flags::UNCOMPRESSED_SIZE != 0 {
            Ok(BlockInfo::Uncompressed(size & !flags::UNCOMPRESSED_SIZE))
        } else {
            Ok(BlockInfo::Compressed(size))
        }
    }

    pub(crate) fn write(&self, mut output: &mut [u8]) -> Result<usize, Error> {
        let value = match self {
            BlockInfo::Compressed(len) if *len == 0 => return Err(Error::InvalidBlockInfo),
            BlockInfo::Compressed(len) | BlockInfo::Uncompressed(len)
                if *len & flags::UNCOMPRESSED_SIZE != 0 =>
            {
                return Err(Error::InvalidBlockInfo)
            }
            BlockInfo::Compressed(len) => *len,
            BlockInfo::Uncompressed(len) => *len | flags::UNCOMPRESSED_SIZE,
            BlockInfo::EndMark => 0,
        };
        output.write_all(&value.to_le_bytes())?;
        Ok(4)
    }
}
