use twox_hash::XxHash32;

use super::Error;
use std::{
    fmt::Debug,
    hash::Hasher,
    io,
    io::{Read, Write},
};

const FLG_RESERVED_MASK: u8 = 0b00000010;
const FLG_VERSION_MASK: u8 = 0b11000000;
const FLG_SUPPORTED_VERSION_BITS: u8 = 0b01000000;

const FLG_INDEPENDENT_BLOCKS: u8 = 0b00100000;
const FLG_BLOCK_CHECKSUMS: u8 = 0b00010000;
const FLG_CONTENT_SIZE: u8 = 0b00001000;
const FLG_CONTENT_CHECKSUM: u8 = 0b00000100;
const FLG_DICTIONARY_ID: u8 = 0b00000001;

const BD_RESERVED_MASK: u8 = !BD_BLOCK_SIZE_MASK;
const BD_BLOCK_SIZE_MASK: u8 = 0b01110000;
const BD_BLOCK_SIZE_MASK_RSHIFT: u8 = 4;

const BLOCK_UNCOMPRESSED_SIZE_BIT: u32 = 0x80000000;

const LZ4F_MAGIC_NUMBER: u32 = 0x184D2204;
pub(crate) const LZ4F_LEGACY_MAGIC_NUMBER: u32 = 0x184C2102;
const LZ4F_SKIPPABLE_MAGIC_RANGE: std::ops::RangeInclusive<u32> = 0x184D2A50..=0x184D2A5F;

pub(crate) const MAGIC_NUMBER_SIZE: usize = 4;
pub(crate) const MIN_FRAME_INFO_SIZE: usize = 7;
pub(crate) const MAX_FRAME_INFO_SIZE: usize = 19;
pub(crate) const BLOCK_INFO_SIZE: usize = 4;

#[derive(Clone, Copy, PartialEq, Debug)]
/// Different predefines blocksizes to choose when compressing data.
#[derive(Default)]
pub enum BlockSize {
    /// Will detect optimal frame size based on the size of the first write call
    #[default]
    Auto = 0,
    /// The default block size.
    Max64KB = 4,
    /// 256KB block size.
    Max256KB = 5,
    /// 1MB block size.
    Max1MB = 6,
    /// 4MB block size.
    Max4MB = 7,
    /// 8MB block size.
    Max8MB = 8,
}

impl BlockSize {
    /// Try to find optimal size based on passed buffer length.
    pub(crate) fn from_buf_length(buf_len: usize) -> Self {
        let mut blocksize = BlockSize::Max4MB;

        for candidate in [BlockSize::Max256KB, BlockSize::Max64KB] {
            if buf_len > candidate.get_size() {
                return blocksize;
            }
            blocksize = candidate;
        }
        BlockSize::Max64KB
    }
    pub(crate) fn get_size(&self) -> usize {
        match self {
            BlockSize::Auto => unreachable!(),
            BlockSize::Max64KB => 64 * 1024,
            BlockSize::Max256KB => 256 * 1024,
            BlockSize::Max1MB => 1024 * 1024,
            BlockSize::Max4MB => 4 * 1024 * 1024,
            BlockSize::Max8MB => 8 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
/// The two `BlockMode` operations that can be set on (`FrameInfo`)[FrameInfo]
#[derive(Default)]
pub enum BlockMode {
    /// Every block is compressed independently. The default.
    #[default]
    Independent,
    /// Blocks can reference data from previous blocks.
    ///
    /// Effective when the stream contains small blocks.
    Linked,
}

// From: https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md
//
// General Structure of LZ4 Frame format
// -------------------------------------
//
// | MagicNb | F. Descriptor | Block | (...) | EndMark | C. Checksum |
// |:-------:|:-------------:| ----- | ----- | ------- | ----------- |
// | 4 bytes |  3-15 bytes   |       |       | 4 bytes | 0-4 bytes   |
//
// Frame Descriptor
// ----------------
//
// | FLG     | BD      | (Content Size) | (Dictionary ID) | HC      |
// | ------- | ------- |:--------------:|:---------------:| ------- |
// | 1 byte  | 1 byte  |  0 - 8 bytes   |   0 - 4 bytes   | 1 byte  |
//
// __FLG byte__
//
// |  BitNb  |  7-6  |   5   |    4     |  3   |    2     |    1     |   0  |
// | ------- |-------|-------|----------|------|----------|----------|------|
// |FieldName|Version|B.Indep|B.Checksum|C.Size|C.Checksum|*Reserved*|DictID|
//
// __BD byte__
//
// |  BitNb  |     7    |     6-5-4     |  3-2-1-0 |
// | ------- | -------- | ------------- | -------- |
// |FieldName|*Reserved*| Block MaxSize |*Reserved*|
//
// Data Blocks
// -----------
//
// | Block Size |  data  | (Block Checksum) |
// |:----------:| ------ |:----------------:|
// |  4 bytes   |        |   0 - 4 bytes    |
//
#[derive(Debug, Default, Clone)]
/// The metadata for de/compressing with lz4 frame format.
pub struct FrameInfo {
    /// If set, includes the total uncompressed size of data in the frame.
    pub content_size: Option<u64>,
    /// The identifier for the dictionary that must be used to correctly decode data.
    /// The compressor and the decompressor must use exactly the same dictionary.
    ///
    /// Note that this is currently unsupported and for this reason it's not pub.
    pub(crate) dict_id: Option<u32>,
    /// The maximum uncompressed size of each data block.
    pub block_size: BlockSize,
    /// The block mode.
    pub block_mode: BlockMode,
    /// If set, includes a checksum for each data block in the frame.
    pub block_checksums: bool,
    /// If set, includes a content checksum to verify that the full frame contents have been
    /// decoded correctly.
    pub content_checksum: bool,
    /// If set, use the legacy frame format
    pub legacy_frame: bool,
}

impl FrameInfo {
    /// Create a new `FrameInfo`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to include the total uncompressed size of data in the frame.
    pub fn content_size(mut self, content_size: Option<u64>) -> Self {
        self.content_size = content_size;
        self
    }

    /// The maximum uncompressed size of each data block.
    pub fn block_size(mut self, block_size: BlockSize) -> Self {
        self.block_size = block_size;
        self
    }

    /// The block mode.
    pub fn block_mode(mut self, block_mode: BlockMode) -> Self {
        self.block_mode = block_mode;
        self
    }

    /// If set, includes a checksum for each data block in the frame.
    pub fn block_checksums(mut self, block_checksums: bool) -> Self {
        self.block_checksums = block_checksums;
        self
    }

    /// If set, includes a content checksum to verify that the full frame contents have been
    /// decoded correctly.
    pub fn content_checksum(mut self, content_checksum: bool) -> Self {
        self.content_checksum = content_checksum;
        self
    }

    /// If set, use the legacy frame format.
    pub fn legacy_frame(mut self, legacy_frame: bool) -> Self {
        self.legacy_frame = legacy_frame;
        self
    }

    pub(crate) fn read_size(input: &[u8]) -> Result<usize, Error> {
        let mut required = MIN_FRAME_INFO_SIZE;
        let magic_num = u32::from_le_bytes(input[0..4].try_into().unwrap());
        if magic_num == LZ4F_LEGACY_MAGIC_NUMBER {
            return Ok(MAGIC_NUMBER_SIZE);
        }

        if input.len() < required {
            return Ok(required);
        }

        if LZ4F_SKIPPABLE_MAGIC_RANGE.contains(&magic_num) {
            return Ok(8);
        }
        if magic_num != LZ4F_MAGIC_NUMBER {
            return Err(Error::WrongMagicNumber);
        }

        if input[4] & FLG_CONTENT_SIZE != 0 {
            required += 8;
        }
        if input[4] & FLG_DICTIONARY_ID != 0 {
            required += 4
        }
        Ok(required)
    }

    pub(crate) fn write_size(&self) -> usize {
        let mut required = MIN_FRAME_INFO_SIZE;
        if self.content_size.is_some() {
            required += 8;
        }
        if self.dict_id.is_some() {
            required += 4;
        }
        required
    }

    pub(crate) fn write(&self, output: &mut [u8]) -> Result<usize, Error> {
        let write_size = self.write_size();
        if output.len() < write_size {
            return Err(Error::IoError(io::ErrorKind::UnexpectedEof.into()));
        }
        let mut buffer = [0u8; MAX_FRAME_INFO_SIZE];
        assert!(write_size <= buffer.len());
        buffer[0..4].copy_from_slice(&LZ4F_MAGIC_NUMBER.to_le_bytes());
        buffer[4] = FLG_SUPPORTED_VERSION_BITS;
        if self.block_checksums {
            buffer[4] |= FLG_BLOCK_CHECKSUMS;
        }
        if self.content_checksum {
            buffer[4] |= FLG_CONTENT_CHECKSUM;
        }
        if self.block_mode == BlockMode::Independent {
            buffer[4] |= FLG_INDEPENDENT_BLOCKS;
        }
        buffer[5] = (self.block_size as u8) << BD_BLOCK_SIZE_MASK_RSHIFT;

        // Optional section
        let mut offset = 6;
        if let Some(size) = self.content_size {
            buffer[4] |= FLG_CONTENT_SIZE;
            buffer[offset..offset + 8].copy_from_slice(&size.to_le_bytes());
            offset += 8;
        }
        if let Some(dict_id) = self.dict_id {
            buffer[4] |= FLG_DICTIONARY_ID;
            buffer[offset..offset + 4].copy_from_slice(&dict_id.to_le_bytes());
            offset += 4;
        }

        // Header checksum
        let mut hasher = XxHash32::with_seed(0);
        hasher.write(&buffer[4..offset]);
        let header_checksum = (hasher.finish() >> 8) as u8;
        buffer[offset] = header_checksum;
        offset += 1;

        debug_assert_eq!(offset, write_size);
        output[..write_size].copy_from_slice(&buffer[..write_size]);
        Ok(write_size)
    }

    pub(crate) fn read(mut input: &[u8]) -> Result<FrameInfo, Error> {
        let original_input = input;
        // 4 byte Magic
        let magic_num = {
            let mut buffer = [0u8; 4];
            input.read_exact(&mut buffer)?;
            u32::from_le_bytes(buffer)
        };
        if magic_num == LZ4F_LEGACY_MAGIC_NUMBER {
            return Ok(FrameInfo {
                block_size: BlockSize::Max8MB,
                legacy_frame: true,
                ..FrameInfo::default()
            });
        }
        if LZ4F_SKIPPABLE_MAGIC_RANGE.contains(&magic_num) {
            let mut buffer = [0u8; 4];
            input.read_exact(&mut buffer)?;
            let user_data_len = u32::from_le_bytes(buffer);
            return Err(Error::SkippableFrame(user_data_len));
        }
        if magic_num != LZ4F_MAGIC_NUMBER {
            return Err(Error::WrongMagicNumber);
        }

        // fixed size section
        let [flg_byte, bd_byte] = {
            let mut buffer = [0u8, 0];
            input.read_exact(&mut buffer)?;
            buffer
        };

        if flg_byte & FLG_VERSION_MASK != FLG_SUPPORTED_VERSION_BITS {
            // version is always 01
            return Err(Error::UnsupportedVersion(flg_byte & FLG_VERSION_MASK));
        }

        if flg_byte & FLG_RESERVED_MASK != 0 || bd_byte & BD_RESERVED_MASK != 0 {
            return Err(Error::ReservedBitsSet);
        }

        let block_mode = if flg_byte & FLG_INDEPENDENT_BLOCKS != 0 {
            BlockMode::Independent
        } else {
            BlockMode::Linked
        };
        let content_checksum = flg_byte & FLG_CONTENT_CHECKSUM != 0;
        let block_checksums = flg_byte & FLG_BLOCK_CHECKSUMS != 0;

        let block_size = match (bd_byte & BD_BLOCK_SIZE_MASK) >> BD_BLOCK_SIZE_MASK_RSHIFT {
            i @ 0..=3 => return Err(Error::UnsupportedBlocksize(i)),
            4 => BlockSize::Max64KB,
            5 => BlockSize::Max256KB,
            6 => BlockSize::Max1MB,
            7 => BlockSize::Max4MB,
            _ => unreachable!(),
        };

        // var len section
        let mut content_size = None;
        if flg_byte & FLG_CONTENT_SIZE != 0 {
            let mut buffer = [0u8; 8];
            input.read_exact(&mut buffer).unwrap();
            content_size = Some(u64::from_le_bytes(buffer));
        }

        let mut dict_id = None;
        if flg_byte & FLG_DICTIONARY_ID != 0 {
            let mut buffer = [0u8; 4];
            input.read_exact(&mut buffer)?;
            dict_id = Some(u32::from_le_bytes(buffer));
        }

        // 1 byte header checksum
        let expected_checksum = {
            let mut buffer = [0u8; 1];
            input.read_exact(&mut buffer)?;
            buffer[0]
        };
        let mut hasher = XxHash32::with_seed(0);
        hasher.write(&original_input[4..original_input.len() - input.len() - 1]);
        let header_hash = (hasher.finish() >> 8) as u8;
        if header_hash != expected_checksum {
            return Err(Error::HeaderChecksumError);
        }

        Ok(FrameInfo {
            content_size,
            dict_id,
            block_size,
            block_mode,
            block_checksums,
            content_checksum,
            legacy_frame: false,
        })
    }
}

#[derive(Debug)]
pub(crate) enum BlockInfo {
    Compressed(u32),
    Uncompressed(u32),
    EndMark,
}

impl BlockInfo {
    pub(crate) fn read(mut input: &[u8]) -> Result<Self, Error> {
        let mut size_buffer = [0u8; 4];
        input.read_exact(&mut size_buffer)?;
        let size = u32::from_le_bytes(size_buffer);
        if size == 0 {
            Ok(BlockInfo::EndMark)
        } else if size & BLOCK_UNCOMPRESSED_SIZE_BIT != 0 {
            Ok(BlockInfo::Uncompressed(size & !BLOCK_UNCOMPRESSED_SIZE_BIT))
        } else {
            Ok(BlockInfo::Compressed(size))
        }
    }

    pub(crate) fn write(&self, mut output: &mut [u8]) -> Result<usize, Error> {
        let value = match self {
            BlockInfo::Compressed(len) if *len == 0 => return Err(Error::InvalidBlockInfo),
            BlockInfo::Compressed(len) | BlockInfo::Uncompressed(len)
                if *len & BLOCK_UNCOMPRESSED_SIZE_BIT != 0 =>
            {
                return Err(Error::InvalidBlockInfo)
            }
            BlockInfo::Compressed(len) => *len,
            BlockInfo::Uncompressed(len) => *len | BLOCK_UNCOMPRESSED_SIZE_BIT,
            BlockInfo::EndMark => 0,
        };
        output.write_all(&value.to_le_bytes())?;
        Ok(4)
    }
}
