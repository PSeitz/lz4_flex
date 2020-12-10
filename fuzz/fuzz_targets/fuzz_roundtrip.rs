#![no_main]
use libfuzzer_sys::fuzz_target;

use lz4_flex::decompress_size_prepended;
use lz4_flex::compress_prepend_size;
fuzz_target!(|data: &[u8]| {
    // fuzzed code goes here
    let compressed = compress_prepend_size(data);
    let decompressed = decompress_size_prepended(&compressed).unwrap();
    assert_eq!(data, decompressed);
});
