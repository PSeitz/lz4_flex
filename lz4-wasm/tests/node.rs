//! Test suite for the Web and headless browsers.

#![cfg(target_arch = "wasm32")]

extern crate wasm_bindgen_test;
use wasm_bindgen_test::*;
use lz4_flex::block::compress::compress_prepend_size;
use lz4_flex::block::decompress::decompress_size_prepended;

#[wasm_bindgen_test]
fn test_compression() {
    let input = "some text, with content";
    let compressed = compress_prepend_size(input.as_bytes());
    let decompressed = decompress_size_prepended(&compressed).unwrap();
    assert_eq!(decompressed.as_slice(), input.as_bytes());
}
