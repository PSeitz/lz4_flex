#![no_main]
use libfuzzer_sys::fuzz_target;

use lz4_flex::block::decompress::decompress_size_prepended;
use lz4::block::compress as lz4_linked_block_compress;

fuzz_target!(|data: &[u8]| {
    // fuzzed code goes here
    let compressed = lz4_linked_block_compress(data, None, true).unwrap();
    let decompressed = decompress_size_prepended(&compressed).unwrap();
    assert_eq!(data, decompressed);
});
