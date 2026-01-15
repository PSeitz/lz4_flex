#![no_main]
use libfuzzer_sys::fuzz_target;

use lz4_flex::block::{decompress, decompress_with_dict};

// dict content does not matter
static DICT: [u8; 1024] = [0_u8; 1024];

#[derive(Debug, arbitrary::Arbitrary)]
struct FuzzData {
    input: Vec<u8>,
    #[arbitrary(with = |u: &mut arbitrary::Unstructured| u.int_in_range(0..=DICT.len()))]
    dict_size: usize,
    #[arbitrary(with = |u: &mut arbitrary::Unstructured| u.int_in_range(0..=65535))]
    output_size: usize,
}

fuzz_target!(|fuzz_data: FuzzData| {
    let input = fuzz_data.input;
    let dict = &DICT[..fuzz_data.dict_size];
    let output_size = fuzz_data.output_size;

    // use decompress functions which for the unsafe feature use an uninitialized Vec,
    // making this fuzz test interesting for MemorySanitizer
    let result = if dict.is_empty() {
        decompress(&input, output_size)
    } else {
        decompress_with_dict(&input, output_size, &dict)
    };
    // mainly verify that no panic had occurred, but ignore whether result was Ok or Err
    if let Ok(decomp) = result {
        // to detect invalid memory access have to consume each byte, otherwise if the decompression
        // function copies uninitialized memory in place, MemorySanitizer does not seem to report it;
        // and even that only works when fuzzing is run with `--dev`, otherwise the access here seems
        // to be optimized away, see also https://github.com/rust-fuzz/cargo-fuzz/issues/436
        for byte in decomp {
            std::hint::black_box(byte);
        }
    }
});
