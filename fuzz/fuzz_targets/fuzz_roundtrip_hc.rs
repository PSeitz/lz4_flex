#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

/// Input for HC compression fuzzing with configurable level
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

    // Compress with HC algorithm
    let compressed = lz4_flex::block::compress_hc_to_vec(&input.data, level);

    // Decompress and verify roundtrip
    if !compressed.is_empty() {
        let max_output = input.data.len().max(compressed.len() * 10 + 100);
        match lz4_flex::block::decompress(&compressed, max_output) {
            Ok(decompressed) => {
                assert_eq!(
                    input.data.len(),
                    decompressed.len(),
                    "Decompressed size mismatch for level {}: expected {}, got {}",
                    level,
                    input.data.len(),
                    decompressed.len()
                );
                assert_eq!(
                    input.data.as_slice(),
                    decompressed.as_slice(),
                    "Roundtrip data mismatch for level {}",
                    level
                );
            }
            Err(e) => {
                panic!("Decompression failed for level {}: {:?}", level, e);
            }
        }
    }
});
