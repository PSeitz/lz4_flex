#![no_main]
use libfuzzer_sys::fuzz_target;

use lz4_flex::block::{decompress_into, decompress_into_with_dict, DecompressError};

// dict content does not matter
static DICT: [u8; 1024] = [0_u8; 1024];

#[derive(Debug, arbitrary::Arbitrary)]
struct FuzzData {
    input: Vec<u8>,
    #[arbitrary(with = |u: &mut arbitrary::Unstructured| u.int_in_range(0..=DICT.len()))]
    dict_size: usize,
}

fuzz_target!(|fuzz_data: FuzzData| {
    let input = fuzz_data.input;
    let dict = &DICT[..fuzz_data.dict_size];
    // create an output buffer which is presumably large enough for the decompressed result
    let mut output = vec![0u8; 512.max(input.len() * 4)];

    fn decompress(input: &[u8], output: &mut [u8], dict: &[u8]) -> Result<usize, DecompressError> {
        if dict.is_empty() {
            decompress_into(input, output)
        } else {
            decompress_into_with_dict(input, output, dict)
        }
    }

    let decompressed1;
    if let Ok(decompressed_len) = decompress(&input, &mut output, &dict) {
        decompressed1 = output[..decompressed_len].to_owned();
    } else {
        // Skip if decompression failed
        return;
    }

    // Pre-fill output buffer with arbitrary data and repeat decompression; should not have any effect on decompression result
    output.fill(255);
    let decompressed_len = decompress(&input, &mut output, &dict).unwrap();
    let decompressed2 = &output[..decompressed_len];

    assert_eq!(decompressed1, decompressed2);
});
