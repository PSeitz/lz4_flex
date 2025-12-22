//! Test binary for WASM SIMD verification
//! 
//! Compile with:
//! RUSTFLAGS="-C target-feature=+simd128" cargo build --example wasm_test --target wasm32-unknown-unknown --release --no-default-features

use lz4_flex::block::{compress, decompress_size_prepended, compress_prepend_size};

#[no_mangle]
pub extern "C" fn test_compress() -> usize {
    let input = b"Hello World! This is a test of LZ4 compression with SIMD optimizations. \
                  We need enough data to trigger the compression algorithm properly. \
                  Hello World! This is a test of LZ4 compression with SIMD optimizations. \
                  We need enough data to trigger the compression algorithm properly.";
    
    let compressed = compress(input);
    compressed.len()
}

#[no_mangle]
pub extern "C" fn test_roundtrip() -> i32 {
    let input = b"Hello World! This is a test of LZ4 compression with SIMD optimizations. \
                  We need enough data to trigger the compression algorithm properly. \
                  Hello World! This is a test of LZ4 compression with SIMD optimizations. \
                  We need enough data to trigger the compression algorithm properly.";
    
    let compressed = compress_prepend_size(input);
    let decompressed = decompress_size_prepended(&compressed).unwrap();
    
    if decompressed == input {
        1  // Success
    } else {
        0  // Failure
    }
}

fn main() {
    println!("Compressed size: {}", test_compress());
    println!("Roundtrip: {}", if test_roundtrip() == 1 { "OK" } else { "FAIL" });
}

