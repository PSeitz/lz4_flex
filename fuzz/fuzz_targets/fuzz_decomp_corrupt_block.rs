#![no_main]
use libfuzzer_sys::fuzz_target;
use std::convert::TryInto;

use lz4_flex::block::{decompress_size_prepended, decompress_size_prepended_with_dict};
fuzz_target!(|data: &[u8]| {
    if data.len() >= 4 {
        let size = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if size > 20_000_000 {
            return;
        }
    }
    // should not panic
    let _ = decompress_size_prepended(&data);
    let _ = decompress_size_prepended_with_dict(&data, &data);
});
