#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // fuzzed code goes here
    let mut compressed = Vec::new();
    lzzzz::lz4::compress_to_vec(data, &mut compressed, lzzzz::lz4::ACC_LEVEL_DEFAULT).unwrap();
    let decompressed = lz4_flex::decompress(&compressed, data.len()).unwrap();
    assert_eq!(data, decompressed.as_slice());
});
