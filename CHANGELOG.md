0.9.3 (2022-05-23) 
==================
Guard against usize overflows/underflows and raw pointer undefined behavior
* https://github.com/PSeitz/lz4_flex/pull/50

0.9.2 (2021-11-16) 
==================
Fixes imports bug from 0.9.1 for no-default-features
* https://github.com/PSeitz/lz4_flex/pull/25

0.9.1 (2021-11-15) - YANKED
==================
Fix no_std support for safe-decode
* https://github.com/PSeitz/lz4_flex/pull/24

0.9.0 (2021-09-25)
==================
Fix unsoundness in the the api in regards to unitialized data. (thanks to @arthurprs)
* https://github.com/PSeitz/lz4_flex/pull/22

0.8.0 (2021-05-17)
==================
Support for the lz4 frame format, with a massive amount of improvements. (big thanks to @arthurprs)
~40% faster safe decompression, ~10% faster safe compression.
* PR-13 https://github.com/PSeitz/lz4_flex/pull/13

Added the possibility to compress/decompress into a u8 slice with
for `compress_into` and `decompress_into`. Previously it only accepted
a `Vec` with caused some double allocations for some scenarios. It's 
around 10% faster for compressible data with safe-decoding, which makes sense, since
push has additional overhead to check the capacity which is not required.

* BUG-11 (https://github.com/PSeitz/lz4_flex/issues/11)
	Allow to compress/decompress into a u8 slice


