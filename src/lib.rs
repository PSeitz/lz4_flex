//! Pure Rust implementation of LZ4 compression.
//!
//! A detailed explanation of the algorithm can be found [here](http://ticki.github.io/blog/how-lz4-works/).

#![feature(test)]
#![warn(missing_docs)]

extern crate byteorder;
#[macro_use]
extern crate quick_error;

mod decompress;
mod decompress_old;
mod decompress_unchecked;
mod block;
#[cfg(test)]
mod tests;

pub use decompress::{decompress_into, decompress};
pub use block::compress::{compress_into, compress};

pub use decompress_unchecked::{decompress_into as decompress_into_unchecked, decompress as decompress_unchecked};

const ONLY_HIGH_BIT_U8: u16 = 0b_1000_0000_0000_0000;
pub const TOKEN_FULL_DUPLICATE_U16: u16 = 0b_0111_1111_1111_1111;

#[inline(always)]
pub fn set_high_bit_u16(input: u16) -> u16 {
    input | ONLY_HIGH_BIT_U8
}


#[inline(always)]
pub fn is_full(input: u16) -> bool {
    input & ONLY_HIGH_BIT_U8 != 0
}

#[inline(always)]
pub fn is_high_bit_set(input: u16) -> bool {
    input & ONLY_HIGH_BIT_U8 != 0
}

