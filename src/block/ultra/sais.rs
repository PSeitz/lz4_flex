//! Suffix array construction via the SA-IS algorithm.
//!
//! The choice of suffix-array algorithm does not affect the compressed output: the downstream
//! LCP-interval match finder only depends on the order of the sorted suffixes and their
//! longest-common-prefix lengths, both fully determined by the input. Suffixes that are a prefix of
//! another sort first, because of the appended sentinel.
//!
//! Safe, O(n) Rust.

// SA-IS is inherently index-arithmetic (type recurrences, induced sorting, bucket scans); rewriting
// these loops as iterators would obscure the algorithm.
#![allow(clippy::needless_range_loop)]

use alloc::vec;
use alloc::vec::Vec;

/// Build the suffix array of `input`.
///
/// Returns a `Vec<i32>` of length `input.len()` where `sa[r]` is the start position of the
/// `r`-th smallest suffix. Suffixes are ordered as if an implicit end-of-string symbol smaller
/// than every byte terminated the input (so a suffix that is a strict prefix of another sorts
/// first).
///
/// # Panics
/// Debug-asserts that `input.len() < i32::MAX`. Inputs at or above 2 GiB are not supported (the
/// LZ4 block format caps match offsets at 64 KiB, so this is never a practical limitation).
pub(crate) fn build_suffix_array(input: &[u8]) -> Vec<i32> {
    debug_assert!(input.len() < i32::MAX as usize);
    let n0 = input.len();
    if n0 == 0 {
        return Vec::new();
    }
    // Map bytes to 1..=256 and append a 0 sentinel that is strictly smaller than every real
    // symbol. This makes the last position the unique smallest suffix, as SA-IS requires, and
    // lets inputs that legitimately contain a 0 byte coexist with the sentinel.
    let mut s: Vec<u32> = Vec::with_capacity(n0 + 1);
    s.extend(input.iter().map(|&b| b as u32 + 1));
    s.push(0);

    let mut sa = vec![-1i32; n0 + 1];
    sais(&s, &mut sa, 256);

    // sa[0] is the sentinel suffix (position n0); drop it to return the n0 real suffixes.
    debug_assert_eq!(sa[0], n0 as i32);
    sa.split_off(1)
}

/// Histogram of the alphabet symbols in `s` (`counts[c]` = number of occurrences of symbol `c`).
/// Computed once per recursion level; bucket boundaries are then derived from it in O(k) instead of
/// rescanning `s` on every induced-sort pass.
fn count_symbols(s: &[u32], k: usize) -> Vec<i32> {
    let mut counts = vec![0i32; k + 1];
    for &c in s {
        counts[c as usize] += 1;
    }
    counts
}

/// Fill `bkt[0..=k]` with bucket boundaries derived from `counts`. With `end == true` each entry is
/// the exclusive end of its bucket; with `end == false` each entry is the inclusive start (head).
fn get_buckets(counts: &[i32], bkt: &mut [i32], end: bool) {
    let mut sum = 0i32;
    for (b, &c) in bkt.iter_mut().zip(counts.iter()) {
        sum += c;
        *b = if end { sum } else { sum - c };
    }
}

/// Induced sort of the L-type suffixes (left-to-right scan, placing at bucket heads).
fn induce_l(s: &[u32], sa: &mut [i32], t: &[bool], bkt: &mut [i32], counts: &[i32]) {
    get_buckets(counts, bkt, false);
    for i in 0..sa.len() {
        let j = sa[i] - 1;
        if j >= 0 && !t[j as usize] {
            let c = s[j as usize] as usize;
            sa[bkt[c] as usize] = j;
            bkt[c] += 1;
        }
    }
}

/// Induced sort of the S-type suffixes (right-to-left scan, placing at bucket ends).
fn induce_s(s: &[u32], sa: &mut [i32], t: &[bool], bkt: &mut [i32], counts: &[i32]) {
    get_buckets(counts, bkt, true);
    for i in (0..sa.len()).rev() {
        let j = sa[i] - 1;
        if j >= 0 && t[j as usize] {
            let c = s[j as usize] as usize;
            bkt[c] -= 1;
            sa[bkt[c] as usize] = j;
        }
    }
}

/// Core recursive SA-IS. `s` is the string over alphabet `0..=k` (the last symbol must be the
/// unique smallest), and the suffix array is written into `sa` (which must have length `s.len()`).
fn sais(s: &[u32], sa: &mut [i32], k: usize) {
    let n = s.len();
    if n == 1 {
        sa[0] = 0;
        return;
    }

    // Classify each suffix as S-type (true) or L-type (false).
    // S-type: s[i..] is lexicographically smaller than s[i+1..].
    let mut t = vec![false; n];
    t[n - 1] = true; // the sentinel suffix is S-type by definition
    for i in (0..n - 1).rev() {
        t[i] = s[i] < s[i + 1] || (s[i] == s[i + 1] && t[i + 1]);
    }
    let is_lms = |i: usize| -> bool { i > 0 && t[i] && !t[i - 1] };

    let counts = count_symbols(s, k);
    let mut bkt = vec![0i32; k + 1];

    // Stage 1: place LMS suffixes at the ends of their buckets, then induce.
    for x in sa.iter_mut() {
        *x = -1;
    }
    get_buckets(&counts, &mut bkt, true);
    for i in 1..n {
        if is_lms(i) {
            let c = s[i] as usize;
            bkt[c] -= 1;
            sa[bkt[c] as usize] = i as i32;
        }
    }
    induce_l(s, sa, &t, &mut bkt, &counts);
    induce_s(s, sa, &t, &mut bkt, &counts);

    // Compact all sorted LMS positions into the front of `sa`.
    let mut n1 = 0usize;
    for i in 0..n {
        if is_lms(sa[i] as usize) {
            sa[n1] = sa[i];
            n1 += 1;
        }
    }
    // LMS positions are non-adjacent, so n1 <= n/2 <= n - n1: the name area below can't overlap.
    for x in sa[n1..n].iter_mut() {
        *x = -1;
    }

    // Name the LMS substrings: equal substrings get the same name.
    let mut name = 0i32;
    let mut prev: i32 = -1;
    for i in 0..n1 {
        let pos = sa[i] as usize;
        let mut diff = false;
        let mut d = 0usize;
        loop {
            if prev < 0 {
                diff = true;
                break;
            }
            let pp = prev as usize;
            if s[pos + d] != s[pp + d] || t[pos + d] != t[pp + d] {
                diff = true;
                break;
            }
            if d > 0 && (is_lms(pos + d) || is_lms(pp + d)) {
                // Reached the end of both LMS substrings without a difference: they're equal.
                break;
            }
            d += 1;
        }
        if diff {
            name += 1;
            prev = pos as i32;
        }
        sa[n1 + pos / 2] = name - 1;
    }
    // Compact the names into the tail to form the reduced string.
    let mut j = n - 1;
    for i in (n1..n).rev() {
        if sa[i] >= 0 {
            sa[j] = sa[i];
            j -= 1;
        }
    }

    // Stage 2: solve the reduced problem.
    let (left, s1) = sa.split_at_mut(n - n1); // s1 has length n1
    if (name as usize) == n1 {
        // Every LMS substring is unique: the suffix array is directly the inverse of the names.
        for i in 0..n1 {
            left[s1[i] as usize] = i as i32;
        }
    } else {
        let reduced: Vec<u32> = s1.iter().map(|&x| x as u32).collect();
        sais(&reduced, &mut left[..n1], (name - 1) as usize);
    }

    // Stage 3: induce the final suffix array from the sorted LMS suffixes.
    // Recover the LMS positions in text order.
    let mut p1: Vec<i32> = Vec::with_capacity(n1);
    for i in 1..n {
        if is_lms(i) {
            p1.push(i as i32);
        }
    }
    // Map the reduced-string suffix array back to original LMS positions.
    for i in 0..n1 {
        sa[i] = p1[sa[i] as usize];
    }
    for x in sa[n1..n].iter_mut() {
        *x = -1;
    }
    // Place the sorted LMS suffixes at their bucket ends (reverse order), then induce.
    get_buckets(&counts, &mut bkt, true);
    for i in (0..n1).rev() {
        let jpos = sa[i];
        sa[i] = -1;
        let c = s[jpos as usize] as usize;
        bkt[c] -= 1;
        sa[bkt[c] as usize] = jpos;
    }
    induce_l(s, sa, &t, &mut bkt, &counts);
    induce_s(s, sa, &t, &mut bkt, &counts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Reference O(n^2 log n) suffix array for cross-checking.
    fn naive_suffix_array(input: &[u8]) -> Vec<i32> {
        let mut sa: Vec<i32> = (0..input.len() as i32).collect();
        sa.sort_by(|&a, &b| input[a as usize..].cmp(&input[b as usize..]));
        sa
    }

    fn check(input: &[u8]) {
        let got = build_suffix_array(input);
        let want = naive_suffix_array(input);
        assert_eq!(got, want, "suffix array mismatch for input {input:?}");
    }

    #[test]
    fn empty() {
        assert!(build_suffix_array(b"").is_empty());
    }

    #[test]
    fn single() {
        check(b"a");
        check(&[0u8]);
        check(&[255u8]);
    }

    #[test]
    fn classic_strings() {
        check(b"banana");
        check(b"mississippi");
        check(b"abracadabra");
        check(b"aaaaaa");
        check(b"the quick brown fox jumps over the lazy dog");
    }

    #[test]
    fn with_zero_bytes() {
        check(&[0, 1, 0, 2, 0, 0, 1, 0]);
        check(&[0, 0, 0, 0, 0]);
        check(&[5, 0, 5, 0, 5]);
    }

    #[test]
    fn pseudo_random() {
        // Deterministic xorshift so the test is reproducible without rand.
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for &len in &[2usize, 3, 7, 16, 31, 64, 100, 257, 1000, 4096] {
            for alphabet in &[2u8, 4, 16, 255] {
                let data: Vec<u8> = (0..len)
                    .map(|_| (next() % *alphabet as u64) as u8)
                    .collect();
                check(&data);
            }
        }
    }
}
