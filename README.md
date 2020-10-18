![Rust](https://github.com/PSeitz/lz4_flex/workflows/Rust/badge.svg)


# lz4_flex

![lz4_flex_logo](https://raw.githubusercontent.com/PSeitz/lz4_flex/master/logo.jpg)

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

## Benchmarks
The benchmark is run with criterion on set of test files are in the folder benches. 

Currently 3 implementations are compared, this one, the redox version and the c++ version via rust bindings

`cargo bench`


## Fuzzer
This fuzz target fuzzes, and asserts compression and decompression returns the original input.
`cargo fuzz run fuzz_target_1`
