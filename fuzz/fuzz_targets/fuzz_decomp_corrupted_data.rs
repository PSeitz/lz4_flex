#![no_main]
use std::convert::TryInto;
use libfuzzer_sys::fuzz_target;

use lz4_flex::decompress_size_prepended;
fuzz_target!(|data: &[u8]| {
	if data.len() >= 4 {
		let size = u32::from_le_bytes(data[0..4].try_into().unwrap());
		if size > 20_000_000 {
			return;
		}
	}
    // should not panic
    decompress_size_prepended(&data);
});
