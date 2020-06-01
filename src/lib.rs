//! Pure Rust implementation of LZ4 compression.
//!
//! A detailed explanation of the algorithm can be found [here](http://ticki.github.io/blog/how-lz4-works/).

extern crate byteorder;
#[macro_use]
extern crate quick_error;

mod block;
#[cfg(test)]
mod tests;

pub use block::compress::{compress, compress_into};
pub use block::decompress::{decompress, decompress_into};
