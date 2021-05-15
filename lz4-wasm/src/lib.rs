mod utils;

use lz4_flex::block::compress_prepend_size;
use lz4_flex::block::decompress_size_prepended;
use wasm_bindgen::prelude::*;

// When the `wee_alloc` feature is enabled, use `wee_alloc` as the global
// allocator.
#[cfg(feature = "wee_alloc")]
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[wasm_bindgen]
pub fn compress(input: &[u8]) -> Vec<u8>{
    compress_prepend_size(input)
}

#[wasm_bindgen(catch)]
pub fn decompress(input: &[u8]) -> Result<Vec<u8>, JsValue> {
    decompress_size_prepended(input).map_err(|e|JsValue::from_str(&e.to_string()))
}
