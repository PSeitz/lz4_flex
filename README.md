[![Crate](https://img.shields.io/crates/v/lz4_flex.svg)](https://crates.io/crates/lz4_flex)
[![Documentation](https://docs.rs/lz4_flex/badge.svg)](https://docs.rs/crate/lz4_flex/)


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


### Profiling with cpu-profiler

```bash
$ cargo build --release
$ ./target/release/profile_decomp;pprof -top ./target/release/profile_decomp my-prof.profile
```

Result after adding inline(never), v.0.1.0
```
Showing nodes accounting for 9.57s, 100% of 9.57s total
      flat  flat%   sum%        cum   cum%
     4.64s 48.48% 48.48%      4.64s 48.48%  lz4_flex::decompress::Decoder::duplicate::h30d28e9622eec324
     1.09s 11.39% 59.87%      5.76s 60.19%  lz4_flex::decompress::Decoder::read_duplicate_section::h21e977209a94329a
     1.04s 10.87% 70.74%      1.04s 10.87%  [libc-2.24.so]
     0.95s  9.93% 80.67%      2.87s 29.99%  lz4_flex::decompress::Decoder::read_literal_section::hcc6925a9cbe4bd2a
     0.94s  9.82% 90.49%      9.57s   100%  lz4_flex::decompress::Decoder::complete::h238ae70b89693ab9
     0.86s  8.99% 99.48%      0.86s  8.99%  lz4_flex::decompress::Decoder::output::h5dd9890d269a769e
     0.05s  0.52%   100%      0.05s  0.52%  lz4_flex::decompress::Decoder::read_integer::h62973a57e1274ad6
         0     0%   100%      9.57s   100%  <unknown>
         0     0%   100%      9.57s   100%  __libc_start_main
         0     0%   100%      9.57s   100%  __rust_maybe_catch_panic
         0     0%   100%      9.57s   100%  _start
         0     0%   100%      9.57s   100%  lz4_flex::decompress::decompress::hdbd516701c36a0a8
         0     0%   100%      9.57s   100%  main
         0     0%   100%      9.57s   100%  profile_decomp::main::h38dd0f27166c6bd2
         0     0%   100%      9.57s   100%  std::panic::catch_unwind::h794130e368adf375 (inline)
         0     0%   100%      9.57s   100%  std::panicking::try::do_call::h85d26496c32350c5
         0     0%   100%      9.57s   100%  std::panicking::try::hfbb56b98b87a45a4 (inline)
         0     0%   100%      9.57s   100%  std::rt::lang_start::_$u7b$$u7b$closure$u7d$$u7d$::hc277524635df0152
         0     0%   100%      9.57s   100%  std::rt::lang_start_internal::_$u7b$$u7b$closure$u7d$$u7d$::h50b0398997fe4311 (inline)
         0     0%   100%      9.57s   100%  std::rt::lang_start_internal::h628c81c720c4941a
```
