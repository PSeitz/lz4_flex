[![Crate](https://img.shields.io/crates/v/lz4_flex.svg)](https://crates.io/crates/lz4_flex)
[![Documentation](https://docs.rs/lz4_flex/badge.svg)](https://docs.rs/crate/lz4_flex/)


# lz4_flex

Pure rust implementation of lz4 compression and decompression.

This is based on [redox-os' lz4 compression](https://crates.io/crates/lz4-compress).
The redox implementation is quite slow with only around 300MB/s decompression, 200MB/s compression and the api ist quite limited.
It's planned to address these shortcomings.


Usage: 
```rust
use lz4_compression::prelude::{ decompress, compress };

fn main(){
    let uncompressed_data: &[u8] = b"Hello people, what's up?";

    let compressed_data = compress(uncompressed_data);
    let decompressed_data = decompress(&compressed_data).unwrap();

    assert_eq!(uncompressed_data, decompressed_data.as_slice());
}
```
