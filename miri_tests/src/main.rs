use std::panic;

use lz4_flex::{block::decompress_size_prepended_with_dict, decompress_size_prepended};

fn main() {
    println!("Hello, world!");

    for _i in 0..1_000 {
        println!("Run {_i:?}");
        let mut data = gen_bytes();
        let dict = gen_bytes();
        println!("Loaded Bytes");
        if data.len() >= 4 {
            let size = u32::from_le_bytes(data[0..4].try_into().unwrap());
            let size = size % 16_000;
            data[0..4].copy_from_slice(size.to_le_bytes().as_ref());
        }
        // may panic, that's fine
        let _result = panic::catch_unwind(|| {
            let _ = decompress_size_prepended(&data);

            let _ = decompress_size_prepended_with_dict(&data, &dict);
        });
    }
}

fn gen_bytes() -> Vec<u8> {
    let num_bytes: u8 = rand::random();
    (0..num_bytes).map(|_| rand::random()).collect()
}
