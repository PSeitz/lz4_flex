//! Pure-Rust (no FFI) round-trips that exercise the decoder's overlapping/self-referential copy,
//! so they can run under miri to validate the unsafe `duplicate_overlapping` pointer code.

use lz4_flex::block::{compress, decompress};

fn roundtrip(data: &[u8]) {
    let c = compress(data);
    let d = decompress(&c, data.len()).unwrap();
    assert_eq!(d, data);
}

#[test]
fn overlapping_matches_roundtrip() {
    // Various small offsets and lengths produce overlapping (offset < match_length) copies, which
    // route through duplicate_overlapping / extend_from_within_overlapping.
    for &period in &[1usize, 2, 3, 5, 7, 8, 9, 15, 16, 17, 31, 64, 300] {
        for &len in &[0usize, 1, 13, 18, 19, 64, 255, 256, 1000, 5000] {
            let data: Vec<u8> = (0..len).map(|i| (i % period) as u8).collect();
            roundtrip(&data);
        }
    }
    // A few mixed/odd buffers.
    roundtrip(&[7u8; 4096]);
    roundtrip(b"abcabcabcabcabcabcXYZabcabcabcabcabcabc");
    let mut mixed = Vec::new();
    mixed.extend_from_slice(b"hello world ");
    mixed.extend(std::iter::repeat(0xABu8).take(500));
    mixed.extend_from_slice(b" hello world hello world");
    roundtrip(&mixed);
}
