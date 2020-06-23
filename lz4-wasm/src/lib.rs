mod utils;

use lz4_flex::block::compress::compress_prepend_size;
use lz4_flex::block::decompress::decompress_size_prepended;
use wasm_bindgen::prelude::*;

// When the `wee_alloc` feature is enabled, use `wee_alloc` as the global
// allocator.
#[cfg(feature = "wee_alloc")]
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[wasm_bindgen]
extern {
    fn alert(s: &str);
}

#[wasm_bindgen]
pub fn greet() {
    alert("Hello, lz4-wasm!");
}

#[wasm_bindgen]
pub fn compress(input: &str) -> Vec<u8>{
    compress_prepend_size(input.as_bytes())
}

#[wasm_bindgen]
pub fn decompress(input: &[u8]) -> Vec<u8>{
    decompress_size_prepended(input).unwrap()
}
