#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

/// Fixed-output HC compression of arbitrary data into an arbitrary budget.
#[derive(Arbitrary, Debug)]
struct Input {
    level: u8,
    budget: u16,
    data: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let level = input.level % 13;
    let size = input.budget as usize;
    // A guard past the end shows a write beyond the slice given.
    let mut buffer = vec![0xA5u8; size + 64];
    let (taken, written) =
        lz4_flex::block::compress_hc_dest_size(&input.data, &mut buffer[..size], level);
    assert!(buffer[size..].iter().all(|&b| b == 0xA5), "overrun");
    assert!(written <= size);
    assert!(taken <= input.data.len());
    if size > 0 {
        let back = lz4_flex::block::decompress(&buffer[..written], taken).expect("decodes");
        assert_eq!(back, &input.data[..taken], "reads back other bytes");
    } else {
        assert_eq!((taken, written), (0, 0));
    }
});
