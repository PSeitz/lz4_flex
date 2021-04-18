#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let compressed = lz4_flex::frame::compress(data);
    let decompressed = lz4_flex::frame::decompress(&compressed).unwrap();
    assert_eq!(data, decompressed.as_slice());
});
