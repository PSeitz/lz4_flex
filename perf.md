# Performance notes for the two-hash-tables strategy (level 2)

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

## Reject 4-byte hash collisions before count_same_bytes (safe-encode)

The remaining standout safe-encode regression was the 96 KB incompressible input.
That case almost never encodes matches; it mostly probes the 4-byte table, then
fails immediately in `count_same_bytes`. In C that miss path is cheap. In safe
Rust, `count_same_bytes` still has to build bounded slices/chunk iterators before
it can discover the first machine word differs.

Fix: on the **4-byte-table** path only, add an explicit 4-byte equality check
before calling `count_same_bytes`.

I intentionally did **not** do the same for the 8-byte table: the two-hash-tables strategy's
"8-byte" hash actually uses only the lower 56 bits, so an 8-byte equality
precheck would change parsing decisions and compressed sizes.

Default `cargo bench level_2` medians after the change:

```
725            Median: 6.63 MB/s   (+1%)   Output: 552
34308          Median: 142.56 MB/s (+2%)   Output: 17_389
64723          Median: 180.48 MB/s (+8%)   Output: 32_255
66675          Median: 337.24 MB/s (=)     Output: 14_125
9991663        Median: 170.56 MB/s (=)     Output: 5_272_298
96274          Median: 366.23 MB/s (+11%)  Output: 96_519
```

This helps the public safe path across the board, with the biggest win on the
incompressible image. Table-reuse results are mixed: the 96 KB incompressible
case improves from ~658 MB/s to ~744 MB/s, while the 66 KB highly-compressible
JSON input regresses somewhat.
