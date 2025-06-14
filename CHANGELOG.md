0.11.4 (2025-06-14)
==================
- Upgrade to twox-hash 2.0[#175](https://github.com/PSeitz/lz4_flex/pull/175)
- Better `no_std` compatibility [#180](https://github.com/PSeitz/lz4_flex/pull/180)

0.11.3 (2024-03-30)
==================
- Fix support for `--deny=unsafe_code` compilation [#152](https://github.com/PSeitz/lz4_flex/pull/152)
- make `get_maximum_output_size` const [#153](https://github.com/PSeitz/lz4_flex/pull/153)

0.11.2 (2024-01-11)
==================
- Include license file in the published crate

0.11.1 (2023-06-19)
==================

- [**breaking**] remove `unchecked-decode`
Remove `unchecked-decode` feature-flag, because of feature unification:
https://doc.rust-lang.org/cargo/reference/features.html#feature-unification

0.11.0 (2023-06-18)
==================

### Documentation

- Docs: add decompress block example

### Fixes
- Handle empty input in Frame Format [#120](https://github.com/PSeitz/lz4_flex/pull/120)
```
Empty input was ignored previously and didn't write anything. Now an empty Frame is written. This improves compatibility with the reference implementation and some corner cases.
```

- Fix: Small dict leads to panic [#133](https://github.com/PSeitz/lz4_flex/pull/133)
```
compress_into_with_dict panicked when the dict passed was smaller than 4 bytes. A match has the minimum length of 4 bytes, smaller dicts will be ignored now.
```

### Features

- [**breaking**] invert checked-decode to unchecked-decode [#134](https://github.com/PSeitz/lz4_flex/pull/134)
```
invert `checked-decode` feature flag to `unchecked-decode`
Previously setting `default-features=false` removed the bounds checks from the
`checked-decode` feature flag. `unchecked-decode` inverts this, so it will needs to be
deliberately deactivated.

To migrate, just remove the `checked-decode` feature flag.
```
- Allow to pass buffer larger than size [#78](https://github.com/PSeitz/lz4_flex/pull/78)
```
This removes an unnecessary check in the decompression, when the passed buffer is too big.
```
- Add auto_finish to FrameEncoder [#95](https://github.com/PSeitz/lz4_flex/pull/95) [#100](https://github.com/PSeitz/lz4_flex/pull/100)
```
Empty input was ignored previously and didn't write anything. Now an empty Frame is written. This improves compatibility with the reference implementation and some corner cases.
```
- Autodetect frame blocksize [#81](https://github.com/PSeitz/lz4_flex/pull/81)
```
The default blocksize of FrameInfo is now auto instead of 64kb, it will detect the blocksize
depending of the size of the first write call. This increases
compression ratio and speed for use cases where the data is larger than
64kb.
```
- Add fluent API style construction for FrameInfo [#99](https://github.com/PSeitz/lz4_flex/pull/99) (thanks @CosmicHorrorDev)
```
This adds in fluent API style construction for FrameInfo. Now you can do

let info = FrameInfo::new()
    .block_size(BlockSize::Max1MB)
    .content_checksum(true);
```

### Performance
- Perf: Faster Decompression [#113](https://github.com/PSeitz/lz4_flex/pull/113) [#112](https://github.com/PSeitz/lz4_flex/pull/112)
```
Replace calls to memcpy with custom function
```

- Perf: optimize wildcopy [#109](https://github.com/PSeitz/lz4_flex/pull/109)
```
The initial check in the the 16 byte wild copy is unnecessary, since it is already done before calling the method.
```

- Perf: faster duplicate_overlapping [#114](https://github.com/PSeitz/lz4_flex/pull/114)
```
Replace the aggressive compiler unrolling after the
failed attempt #69 (wrote out of bounds in some cases)

The unrolling is avoided by manually unrolling less aggressive.
Decompression performance is slightly improved by ca 4%, except the
smallest test case.

```
- Perf: simplify extend_from_within_overlapping [#72](https://github.com/PSeitz/lz4_flex/pull/72)
```
extend_from_within_overlapping is used in safe decompression when
overlapping data has been detected. The prev version had unnecessary
assertions/safe guard, since this method is only used in safe code.
Removing the temporary &mut slice also simplified assembly output.

uiCA Code Analyzer

Prev
Tool 	    Skylake	IceLake 	Tiger Lake 	Rocket Lake
uiCA Cycles 28.71 	30.67 		28.71 		27.57

Simplified
Tool 	    Skylake	IceLake 	TigerLake 	Rocket Lake
uiCA Cycles 13.00 	15.00 		13.00 		11.00
```
- Perf: remove unnecessary assertions
```
those assertions are only used in safe code and therefore unnecessary
```
- Perf: improve safe decompression performance 8-18% [#73](https://github.com/PSeitz/lz4_flex/pull/73)
```
Improve safe decompression speed by 8-18%

Reduce multiple slice fetches. every slice access, also nested ones
, carries some overhead. In the hot loop a fixed &[u8;16] is fetched to
operate on. This is purely done to pass that info to the compiler.

Remove error handling that only carries overhead. As we are in safe
mode we can rely on bounds checks if custom error handling only adds overhead.
In normal operation no error should occur.

The strategy to identify improvements was by counting the lines of
assembly. A rough heuristic, but seems effective.
cargo asm --release --example decompress_block decompress_block::main |
wc -l
```
- Perf: improve safe frame compression performance 7-15% [#74](https://github.com/PSeitz/lz4_flex/pull/74)
```
The frame encoding uses a fixed size hashtable.
By creating a special hashtable with a Box<[u32; 4096]> size,
in combination with the bit shift of 4, which is also moved into a constant,
the compiler can remove the bounds checks.
For that to happen, the compiler also needs to recognize the `>> 48` right
shift from the hash algorithm (u64 >> 52 <= 4096), which is the case. Yey

It also means we can use less `unsafe` for the unsafe version
```
- Perf: switch to use only 3 kinds of hashtable [#77](https://github.com/PSeitz/lz4_flex/pull/77)
```
use only hashtables with fixed sizes and bit shifts, that allow to
remove bounds checks.
```

### Refactor

- Refactor: remove VecSink [#71](https://github.com/PSeitz/lz4_flex/pull/71)
```
remove VecSink since it can be fully replaced with a slice
this will reduce code bloat from generics
```
### Testing

- Tests: add proptest roundtrip [#69](https://github.com/PSeitz/lz4_flex/pull/69)

0.10.0 (2023-01-30)
==================
### Features
Add support of decoding legacy frames, used by linux kernel (thanks to @yestyle)
* https://github.com/PSeitz/lz4_flex/pull/66

0.9.5 (2022-09-03)
==================
Add into_inner() to FrameDecoder
* https://github.com/PSeitz/lz4_flex/pull/56 (thanks to @james-rms)

0.9.4 (2022-07-31) 
==================
Change uncompressed_size visibility to pub

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
Fix unsoundness in the the api in regards to uninitialized data. (thanks to @arthurprs)
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


