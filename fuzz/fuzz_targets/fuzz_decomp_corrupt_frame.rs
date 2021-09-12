#![no_main]
use libfuzzer_sys::fuzz_target;

pub fn lz4_flex_frame_decompress(input: &[u8]) -> Result<Vec<u8>, lz4_flex::frame::Error> {
    let mut de = lz4_flex::frame::FrameDecoder::new(input);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut de, &mut out)?;
    Ok(out)
}

fuzz_target!(|data: &[u8]| {
    // should not panic
    let _ = lz4_flex_frame_decompress(&data);
    let mut buffer = Vec::with_capacity(24 + data.len() * 2);
    for prefix in &[
        &[][..],                         // no prefix
        &[0x04u8, 0x22, 0x4d, 0x18][..], // magic number
    ] {
        buffer.clear();
        buffer.extend_from_slice(prefix);
        buffer.extend_from_slice(data);
        let _ = lz4_flex_frame_decompress(&buffer);
    }
    // magic number and correct frame header
    for prefix in &[
        &[0x04u8, 0x22, 0x4d, 0x18, 0x60, 0x40, 0x82][..], // independent
        &[0x04u8, 0x22, 0x4d, 0x18, 0x40, 0x40, 0xC0][..], // linked
    ] {
        buffer.clear();
        buffer.extend_from_slice(prefix);
        buffer.extend_from_slice(data);
        let _ = lz4_flex_frame_decompress(&buffer);
        // use prefix then 2 valid blocks of data
        buffer.clear();
        buffer.extend_from_slice(prefix);
        buffer.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buffer.extend_from_slice(data);
        buffer.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buffer.extend_from_slice(data);
        let _ = lz4_flex_frame_decompress(&buffer);
    }
});
