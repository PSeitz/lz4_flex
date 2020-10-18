/*! Pure Rust, high performance implementation of LZ4 compression.

A detailed explanation of the algorithm can be found [here](http://ticki.github.io/blog/how-lz4-works/).


# Examples
```
use lz4_flex::{compress_prepend_size, decompress_size_prepended};
let input: &[u8] = b"Hello people, what's up?";
let compressed = compress_prepend_size(input);
let uncompressed = decompress_size_prepended(&compressed).unwrap();
assert_eq!(input, uncompressed);

# Feature Flags
There are two feature flags: default = safe-encode and safe-decode 
safe-decode is enabled by default. Currently it adds more checks to the unsafe code, but it still uses unsafe.

safe-encode will switch the compression to completely safe rust code.
```
*/
extern crate byteorder;
#[macro_use]
extern crate quick_error;

pub mod block;
mod frame;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[macro_use] 
extern crate more_asserts;

// use frame::compress::{compress as frame_compress};
pub use block::compress::{compress, compress_into, compress_prepend_size};
pub use block::decompress::{decompress, decompress_into, decompress_size_prepended};
