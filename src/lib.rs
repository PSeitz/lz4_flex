//! Pure Rust implementation of LZ4 compression.
//!
//! A detailed explanation of the algorithm can be found [here](http://ticki.github.io/blog/how-lz4-works/).

#![feature(test)]
#![warn(missing_docs)]

extern crate byteorder;
#[macro_use]
extern crate quick_error;

mod decompress;
mod compress;
#[cfg(test)]
mod tests;

pub use decompress::{decompress_into, decompress};
pub use compress::{compress_into, compress};
