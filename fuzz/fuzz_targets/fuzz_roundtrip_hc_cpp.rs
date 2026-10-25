#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

/// Input for HC compression fuzzing with C++ decompression
#[derive(Arbitrary, Debug)]
struct HcInput {
    /// Compression level (will be mapped to 1-9 for HC algorithm)
    level: u8,
    /// Data to compress
    data: Vec<u8>,
}

fuzz_target!(|input: HcInput| {
    // Map level to valid HC range 1-9
    let level = (input.level % 9) + 1;

    // Compress with Rust HC algorithm
    let compressed = lz4_flex::block::compress_hc_to_vec(&input.data, level);

    // Decompress with C++ lz4 and verify roundtrip
    if !compressed.is_empty() && !input.data.is_empty() {
        let mut decompressed = vec![0u8; input.data.len()];
        match lzzzz::lz4::decompress(&compressed, &mut decompressed) {
            Ok(size) => {
                assert_eq!(
                    input.data.len(),
                    size,
                    "C++ decompressed size mismatch for level {}: expected {}, got {}",
                    level,
                    input.data.len(),
                    size
                );
                assert_eq!(
                    input.data.as_slice(),
                    &decompressed[..size],
                    "C++ roundtrip data mismatch for level {}",
                    level
                );
            }
            Err(e) => {
                panic!("C++ decompression failed for level {}: {:?}", level, e);
            }
        }
    }
});
