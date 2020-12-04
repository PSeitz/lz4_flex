![Rust](https://github.com/PSeitz/lz4_flex/workflows/Rust/badge.svg)
[![Docs](https://docs.rs/lz4_flex/badge.svg)](https://docs.rs/crate/lz4_flex/)
[![Crates.io](https://img.shields.io/crates/v/lz4_flex.svg)](https://crates.io/crates/lz4_flex)

# lz4_flex

![lz4_flex_logo](https://raw.githubusercontent.com/PSeitz/lz4_flex/master/logo.jpg)

Configurable, pure rust, high performance implementation of LZ4 compression with fast compile times. Originally based on [redox-os' lz4 compression](https://crates.io/crates/lz4-compress), but now a complete rewrite.

## Features
- Very good logo
- LZ4 Block format
- High performance
- 1s clean release build time
- feature flags to configure safe/unsafe code usage

## Usage: 

By default compression and decompression uses no usafe via the default feature flags "safe-encode" and "safe-decode". If you need more performance you can disable them (e.g. with no-default-features).

```rust
use lz4_flex::{compress_prepend_size, decompress_size_prepended};

fn main(){
    let input: &[u8] = b"Hello people, what's up?";
    let compressed = compress_prepend_size(input);
    let uncompressed = decompress_size_prepended(&compressed).unwrap();
    assert_eq!(input, uncompressed);
}
```

## Benchmarks
The benchmark is run with criterion on set of test files are in the folder benches.

Currently 3 implementations are compared, this one, the [redox-version](https://crates.io/crates/lz4-compress), [lz-fear](https://github.com/main--/rust-lz-fear) and the [c++ version via rust bindings](https://crates.io/crates/lz4). The lz4-flexx version is running in unsafe mode.

`cargo bench`

- lz4_redox_rust: https://crates.io/crates/lz4-compress
- lz4_cpp: https://crates.io/crates/lz4
- lz-fear: https://github.com/main--/rust-lz-fear


### Results v0.4 04-12-2020
Since v0.4 the implementation is using no unsafe, this benchmark covers this.

Executed on Core i7-6700 Win10 WSL.

![Compress](./compress_bench_safe.svg)

![Decompress](./decompress_bench_safe.svg)

### Results v0.3 18-10-2020
Executed on Macbook Pro 2017 i7


![Compress](./compress_bench.svg)

![Decompress](./decompress_bench.svg)


## Fuzzer
This fuzz target fuzzes, and asserts compression and decompression returns the original input.
`cargo fuzz run fuzz_roundtrip`

This fuzz target fuzzes, and asserts compression with cpp and decompression returns the original input.
`cargo fuzz run fuzz_roundtrip_cpp_compress`



## TODO
- Frame format
- High compression
- no `unsafe` version for decompression

