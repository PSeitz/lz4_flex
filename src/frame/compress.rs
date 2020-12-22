use crate::block::compress::compress as compress_block;
use std::io::Read;
use std::io::Write;

/// Compress all bytes of `input` into `output`.
#[allow(dead_code)]
#[inline]
pub fn compress<R: Read, W: Write>(input: &mut R, output: &mut W) -> std::io::Result<()> {
    // Write Frame header
    let buf = 0x184D2204_u32.to_le_bytes(); // magic number LZ4 Header
    output.write_all(&buf)?;

    // Flag Byte bits
    let version_bits = 0b01000000; // version "01"
    let _flg_byte = version_bits;
    let _bit_indenpence = 0b00100000;
    // let block_checksum = 	0b00010000;
    let _content_size_flag = 0b00001000;
    // let content_checksum = 	0b00000100;
    // let ununsed = 		0b00000010;
    let _dict_id_flag = 0b00000001;

    let mut buffer = [0; u16::MAX as usize];
    loop {
        let n = input.read(&mut buffer[..])?;
        if n == 0 {
            break;
        }
        let compressed = compress_block(&buffer[..n]);
        output.write_all(&compressed)?;
    }

    Ok(())
}
