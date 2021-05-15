//! Test suite for the Web and headless browsers.

#![cfg(target_arch = "wasm32")]

extern crate wasm_bindgen_test;
use wasm_bindgen_test::*;
use lz4_flex::block::compress_prepend_size;
use lz4_flex::block::decompress_size_prepended;
wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn pass() {
    assert_eq!(1 + 1, 2);
}

#[wasm_bindgen_test]
fn test_compression() {
    let input = "some text, with content";
    let compressed = compress_prepend_size(input.as_bytes());
    let decompressed = decompress_size_prepended(&compressed).unwrap();
    assert_eq!(decompressed.as_slice(), input.as_bytes());
}
