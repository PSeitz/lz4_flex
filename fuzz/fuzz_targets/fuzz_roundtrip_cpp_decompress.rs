#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let compressed = lz4_flex::decompress(data);
    let mut decompressed = vec![0u8; data.len()];
    lzzzz::lz4::decompress(&compressed, &mut decompressed).unwrap();
    assert_eq!(data, decompressed.as_slice());
});
