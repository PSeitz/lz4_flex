#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // should not panic
    let _ = lz4_flex::frame::decompress(&data);
    let mut other = Vec::with_capacity(12 + data.len());
    // prepend magic number
    other.clear();
    other.extend_from_slice(&[0x04u8, 0x22, 0x4d, 0x18]);
    other.extend_from_slice(data);
    let _ = lz4_flex::frame::decompress(&other);
    // prepend magic number and correct frame header
    other.clear();
    other.extend_from_slice(&[0x04u8, 0x22, 0x4d, 0x18, 0x60, 0x40, 0x82]);
    other.extend_from_slice(data);
    let _ = lz4_flex::frame::decompress(&other);
    // prepend magic number, correct frame header and block len
    other.clear();
    other.extend_from_slice(&[0x04u8, 0x22, 0x4d, 0x18, 0x60, 0x40, 0x82]);
    other.extend_from_slice(&(data.len() as u32).to_le_bytes());
    other.extend_from_slice(data);
    let _ = lz4_flex::frame::decompress(&other);
});
