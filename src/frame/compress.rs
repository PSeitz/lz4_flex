use super::MAGIC_NUMBER;
use super::{BlockSize, Checksum, END_MARK};
use crate::block::compress::compress as compress_block;
use std::io::Read;
use std::io::Write;

/// Configure compression settings.
pub struct CompressionSettings {
    #[allow(dead_code)]
    independent_blocks: bool,
    /// add block checksum
    #[allow(dead_code)]
    block_checksums: Checksum,
    /// add content checksum
    #[allow(dead_code)]
    content_checksum: Checksum,
    /// sets the block size, see [`BlockSize`]
    block_size: BlockSize,
}

impl<'a> Default for CompressionSettings {
    fn default() -> Self {
        Self {
            independent_blocks: true,
            block_checksums: Checksum::NoChecksum,
            content_checksum: Checksum::NoChecksum,
            block_size: BlockSize::Default,
        }
    }
}

/// Compress all bytes of `input` into `output`.
#[allow(dead_code)]
#[inline]
pub fn compress_with_settings<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    settings: &CompressionSettings,
) -> std::io::Result<()> {
    // Write Frame header
    let buf = MAGIC_NUMBER.to_le_bytes();
    output.write(&buf)?;

    // Flag Byte bits
    let version_bits = 0b01000000; // version "01"
    let _flg_byte = version_bits;
    let _bit_indenpence = 0b00100000;
    // let block_checksum = 	0b00010000;
    let _content_size_flag = 0b00001000;
    // let content_checksum = 	0b00000100;
    // let ununsed = 		0b00000010;
    let _dict_id_flag = 0b00000001;

    let mut buffer = vec![0; settings.block_size.get_size()];
    loop {
        let n = input.read(&mut buffer[..])?;
        if n == 0 {
            break;
        }
        let compressed = compress_block(&buffer[..n]);
        output.write_all(&compressed)?;
    }
    output.write(&END_MARK.to_le_bytes())?;

    Ok(())
}
