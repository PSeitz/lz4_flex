# Performance notes for lz4mid (level 2)

## Baseline

```
725            Median: 3.91 MB/s    Output: 552
34308          Median: 102.44 MB/s  Output: 17_240
64723          Median: 135.57 MB/s  Output: 31_850
66675          Median: 229.72 MB/s  Output: 14_105
9991663        Median: 188.99 MB/s  Output: 5_080_032
96274          Median: 330.10 MB/s  Output: 96_506
```

## Eliminate bounds checks in insert_match_hashes

Replaced individual `insert_8byte_hash`/`insert_4byte_hash` calls with a single
pre-bounded subslice (`&input[base..match_end + 6]`) and derived all offsets from
`span.len()` so the compiler can prove every inner access is in range.

Assembly went from 7 `slice_index_fail` paths / 13 conditional branches down to
1 panic path / 5 branches.

**No measurable impact.** The bounds checks were never-taken branches, always
perfectly predicted. The function is dominated by random hash table writes
(cache misses), not instruction overhead.

## Reduce hash table size from 2^15 to 2^14 (match C reference)

`LZ4MID_HASH_LOG` was 15 (32K entries per table, 256 KB total). The C reference
uses `LZ4HC_HASH_LOG - 1 = 14` (16K entries, 128 KB total). Halving the tables
makes them fit better in cache.

```
725            Median: 6.70 MB/s  (+71%)   Output: 552
34308          Median: 128.58 MB/s (+26%)  Output: 17_389
64723          Median: 160.45 MB/s (+18%)  Output: 32_255
66675          Median: 310.76 MB/s (+35%)  Output: 14_125
9991663        Median: 150.33 MB/s (-20%)  Output: 5_272_298
96274          Median: 332.34 MB/s (+1%)   Output: 96_519
```

Big wins on small/medium inputs. The 10 MB input regressed because the table is
too small for that working set — more collisions, worse matches, and the
compression ratio dropped (5.08 MB → 5.27 MB output). The other inputs show
slightly worse ratios too, as expected from fewer hash entries.

## Eliminate double count_same_bytes in finalize_match

`probe_candidate` already scans forward to find match_length. Then
`finalize_match` backtracks the start and re-scans forward from scratch —
duplicating all the work. The C reference avoids this: it just increments
matchLength during backtracking.

Fix: compute `match_length = probe_match_length + backtrack_amount` and
`match_end = cur + probe_match_length` directly, no second scan.

```
725            Median: 6.66 MB/s  (+4%)    Output: 552      (C90: 386 MB/s)
34308          Median: 134.94 MB/s (+5%)   Output: 17_389   (C90: 399 MB/s)
64723          Median: 166.20 MB/s (+4%)   Output: 32_255   (C90: 370 MB/s)
66675          Median: 313.55 MB/s (+1%)   Output: 14_125   (C90: 1093 MB/s)
9991663        Median: 158.07 MB/s (+5%)   Output: 5_272_298 (C90: 192 MB/s)
96274          Median: 329.06 MB/s (=)     Output: 96_519   (C90: 1476 MB/s)
```

Remaining gap to C90: ~2-3× on most inputs, ~3.5× on highly compressible data.

## Inline insert_match_hashes and encode_sequence

Both were called (not inlined) from the hot loop. Added `#[inline(always)]`.
Assembly went from 12 calls (including 2 function calls per match) to just
memcpy + handle_last_literals.

```
34308          Median: 139.03 MB/s (+8%)   Output: 17_389   (C90: 404 MB/s)
64723          Median: 172.36 MB/s (+4%)   Output: 32_255   (C90: 366 MB/s)
66675          Median: 332.80 MB/s (+6%)   Output: 14_125   (C90: 1094 MB/s)
9991663        Median: 167.21 MB/s (+6%)   Output: 5_272_298 (C90: 192 MB/s)
96274          Median: 319.42 MB/s (-3%)   Output: 96_519   (C90: 1446 MB/s)
```

## Remaining gap analysis

With unsafe-encode, 10MB input is at parity (190 vs 192 MB/s). Two sources remain:

1. **safe-encode overhead (~13%)**: 1325 asm lines vs 849 unsafe, 28 calls vs 8.
   All extra code is bounds-check panic paths. This is the tax for safe Rust.

2. **Table allocation**: C90 reuses a thread-local pre-allocated context with
   fast-reset. Rust allocates + zeros 128KB of hash tables every call via
   `compress_hc_to_vec`. For the 725-byte input this is 60× overhead.
   Users can avoid this with `compress_hc_to_vec_with_table`.

## Fast table reset (skip zeroing)

C90 doesn't zero its tables on reuse — stale entries just fail the match
check. Applied the same approach: added a bounds check in `resolve_candidate`
(`local_pos < input.len()`) and made `reset()` a no-op.

With table reuse + fast reset:

```
725            Median: 337 MB/s             Output: 552      (C90: 399 MB/s, 1.18×)
34308          Median: 303 MB/s             Output: 17_389   (C90: 401 MB/s, 1.32×)
64723          Median: 274 MB/s             Output: 32_255   (C90: 355 MB/s, 1.29×)
66675          Median: 858 MB/s             Output: 14_125   (C90: 1088 MB/s, 1.27×)
9991663        Median: 174 MB/s             Output: 5_272_298 (C90: 192 MB/s, 1.10×)
96274          Median: 681 MB/s             Output: 96_519   (C90: 1397 MB/s, 2.05×)
```

Gap is now 10-30% (safe-encode overhead), except 96KB incompressible (2×).
