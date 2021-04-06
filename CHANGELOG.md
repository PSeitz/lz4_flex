1.0.0 (2021-04-06)
==================
Added the possibility to compress/decompress into a u8 slice with
`Sink` for `compress_into`, `compress_into_with_table` and 
`decompress_into`. Previously it only accepted a `Vec` with caused 
some double allocations for some scenarios. It's around 10% faster 
for compressible data with safe-decoding, which makes sense, since
push has additional overhead to check the capacity which is not required.
If using this, make sure to allocate enough space for compression and decompression.

* BUG-11 (https://github.com/PSeitz/lz4_flex/issues/11)
	Allow to compress decompress into a u8 slice
