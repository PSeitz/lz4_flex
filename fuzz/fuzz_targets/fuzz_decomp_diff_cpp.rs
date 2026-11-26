#![no_main]
use libfuzzer_sys::fuzz_target;

#[derive(Debug, arbitrary::Arbitrary)]
struct FuzzData {
    input: Vec<u8>,
    #[arbitrary(with = |u: &mut arbitrary::Unstructured| u.int_in_range(0..=65535))]
    output_size: usize,
}

fuzz_target!(|data: FuzzData| {
    let mut ours = vec![0u8; data.output_size];
    let ours = lz4_flex::block::decompress_into(&data.input, &mut ours).map(|n| ours[..n].to_vec());
    let mut reference = vec![0u8; data.output_size];
    let reference =
        lzzzz::lz4::decompress(&data.input, &mut reference).map(|n| reference[..n].to_vec());
    if let (Ok(ours), Ok(reference)) = (&ours, &reference) {
        assert_eq!(ours, reference);
    }
});
