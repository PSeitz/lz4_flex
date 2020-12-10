/*! Pure Rust, high performance implementation of LZ4 compression.

A detailed explanation of the algorithm can be found [here](http://ticki.github.io/blog/how-lz4-works/).


# Examples
```
use lz4_flex::{compress_prepend_size, decompress_size_prepended};
let input: &[u8] = b"Hello people, what's up?";
let compressed = compress_prepend_size(input);
let uncompressed = decompress_size_prepended(&compressed).unwrap();
assert_eq!(input, uncompressed);

```

## Feature Flags
There are 3 feature flags: safe-encode, safe-decode and checked-decode.

safe-decode and safe-encode only use safe rust code.

checked-decode will add aditional checks if safe-decode is not enabled, to avoid out of bounds access.

*/
#[macro_use]
extern crate quick_error;

pub mod block;
mod frame;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[macro_use]
extern crate more_asserts;

pub use block::compress::{compress, compress_into, compress_prepend_size};

#[cfg(feature = "safe-decode")]
pub use block::decompress_safe::{decompress, decompress_into, decompress_size_prepended};

#[cfg(not(feature = "safe-decode"))]
pub use block::decompress::{decompress, decompress_into, decompress_size_prepended};
