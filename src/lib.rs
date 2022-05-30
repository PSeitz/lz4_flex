//! Pure Rust, high performance implementation of LZ4 compression.
//!
//! A detailed explanation of the algorithm can be found [here](http://ticki.github.io/blog/how-lz4-works/).
//!
//! # Overview
//!
//! This crate provides two ways to use lz4. The first way is through the
//! [`frame::FrameDecoder`](frame/struct.FrameDecoder.html)
//! and
//! [`frame::FrameEncoder`](frame/struct.FrameEncoder.html)
//! types, which implement the `std::io::Read` and `std::io::Write` traits with the
//! lz4 frame format. Unless you have a specific reason to the contrary, you
//! should only use the lz4 frame format. Specifically, the lz4 frame format
//! permits streaming compression or decompression.
//!
//! The second way is through the
//! [`decompress_size_prepended`](fn.decompress_size_prepended.html)
//! and
//! [`compress_prepend_size`](fn.compress_prepend_size.html)
//! functions. These functions provide access to the lz4 block format, and
//! don't support a streaming interface directly. You should only use these types
//! if you know you specifically need the lz4 block format.
//!
//! # Example: compress data on `stdin` with frame format
//! This program reads data from `stdin`, compresses it and emits it to `stdout`.
//! This example can be found in `examples/compress.rs`:
//! ```no_run
//! use std::io;
//! let stdin = io::stdin();
//! let stdout = io::stdout();
//! let mut rdr = stdin.lock();
//! // Wrap the stdout writer in a LZ4 Frame writer.
//! let mut wtr = lz4_flex::frame::FrameEncoder::new(stdout.lock());
//! io::copy(&mut rdr, &mut wtr).expect("I/O operation failed");
//! wtr.finish().unwrap();
//! ```
//! # Example: decompress data on `stdin` with frame format
//! This program reads data from `stdin`, decompresses it and emits it to `stdout`.
//! This example can be found in `examples/decompress.rs`:
//! ```no_run
//! use std::io;
//! let stdin = io::stdin();
//! let stdout = io::stdout();
//! // Wrap the stdin reader in a LZ4 FrameDecoder.
//! let mut rdr = lz4_flex::frame::FrameDecoder::new(stdin.lock());
//! let mut wtr = stdout.lock();
//! io::copy(&mut rdr, &mut wtr).expect("I/O operation failed");
//! ```
//!
//! # Example: block format roundtrip
//! ```
//! use lz4_flex::{compress_prepend_size, decompress_size_prepended};
//! let input: &[u8] = b"Hello people, what's up?";
//! let compressed = compress_prepend_size(input);
//! let uncompressed = decompress_size_prepended(&compressed).unwrap();
//! assert_eq!(input, uncompressed);
//! ```
//!
//! ## Feature Flags
//!
//! - `safe-encode` uses only safe rust for encode. _enabled by default_
//! - `safe-decode` uses only safe rust for encode. _enabled by default_
//! - `checked-decode` will add additional checks if `safe-decode` is not enabled, to avoid out of
//!   bounds access. This should be enabled for untrusted input.
//! - `frame` support for LZ4 frame format. _implies `std`, enabled by default_
//! - `std` enables dependency on the standard library. _enabled by default_
//!
//! For maximum performance use `no-default-features`.
//!
//! For no_std support only the [`block format`](block/index.html) is supported.
//!
//!

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg_attr(test, macro_use)]
extern crate alloc;

#[cfg(test)]
#[macro_use]
extern crate more_asserts;

pub mod block;
#[cfg(feature = "frame")]
pub mod frame;

pub use block::{compress, compress_into, compress_prepend_size};

pub use block::{decompress, decompress_into, decompress_size_prepended};

#[cfg_attr(
    all(feature = "safe-encode", feature = "safe-decode"),
    forbid(unsafe_code)
)]
pub(crate) mod sink;
