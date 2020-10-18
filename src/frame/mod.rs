pub mod compress;
pub mod decompress;

pub use compress::compress;

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum BlockSize {
    Default = 0, // Default - 64KB
    Max64KB = 4,
    Max256KB = 5,
    Max1MB = 6,
    Max4MB = 7,
}

#[allow(dead_code)]
impl BlockSize {
    pub fn get_size(&self) -> usize {
        match self {
            &BlockSize::Default | &BlockSize::Max64KB => 64 * 1024,
            &BlockSize::Max256KB => 256 * 1024,
            &BlockSize::Max1MB => 1 * 1024 * 1024,
            &BlockSize::Max4MB => 4 * 1024 * 1024,
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
#[repr(C)]
pub enum BlockMode {
    Linked = 0,
    Independent,
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum ContentChecksum {
    NoChecksum = 0,
    ChecksumEnabled,
}

/// Frame Descriptor
/// FLG     BD      (Content Size)  (Dictionary ID)     HC
/// 1 byte  1 byte  0 - 8 bytes     0 - 4 bytes         1 byte
#[allow(dead_code)]
#[repr(C)]
pub(crate) struct LZ4FFrameInfo {
    pub content_size: Option<u64>,
    pub block_size_id: BlockSize,
    pub block_mode: BlockMode,
    pub content_checksum_flag: ContentChecksum,
    // pub reserved: [u32; 5],
}

#[allow(dead_code)]
impl LZ4FFrameInfo {
    fn read(input: &[u8]) -> LZ4FFrameInfo {
        // read flag bytes

        let flg_byte = input[0];
        assert!((flg_byte & 0b11000000) == 1); // version is always 01

        let block_mode = if (flg_byte & 1 << 5) == 1 {
            BlockMode::Independent
        } else {
            BlockMode::Linked
        };
        let content_checksum_flag = if (flg_byte & 1 << 2) == 1 {
            ContentChecksum::ChecksumEnabled
        } else {
            ContentChecksum::NoChecksum
        };

        // let content_size_included = (flg_byte & 1 << 4) == 1;

        LZ4FFrameInfo {
            block_mode,
            content_checksum_flag,
            block_size_id: BlockSize::Default, // TODO
            content_size: None,                // TODO
        }
    }

    /// writes flag byte
    ///
    /// FLG byte
    /// BitNumber   7-6         5                      4               3             2                 1           0
    /// FieldName   Version     Block Independence     Block-Checksum  Content-Size  Content-Checksum  Reserved    DictID
    ///
    /// Content-Size (Optional) - The size of the uncompressed data included within the frame will be present as an 8 bytes unsigned little endian value, after the flags
    fn write_flg_byte(&self) -> u32 {
        let version = 1 << 6; // always 01
        let mut res = version;
        res |= (self.block_mode as u32) << 5;
        if self.content_size.is_some() {
            res |= 1 << 3; // set the content_size flag
        }
        res |= (self.content_checksum_flag as u32) << 2;
        // res |= 0 << 4;
        res
    }

    /// writes block descriptor byte
    /// Block Maximum Size
    ///
    /// This information is useful to help the decoder allocate memory. Size here refers to the original (uncompressed) data size.
    /// Block Maximum Size is one value among the following table :
    /// 0       1       2       3       4       5       6       7
    /// N/A     N/A     N/A     N/A     64 KB   256 KB  1 MB    4 MB
    fn write_bd_byte(&self) -> u32 {
        1 << (self.block_size_id as u32)
    }
}
