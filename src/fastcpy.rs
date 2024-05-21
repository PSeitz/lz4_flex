//! # FastCpy
//!
//! The Rust Compiler calls `memcpy` for slices of unknown length.
//! This crate provides a faster implementation of `memcpy` for slices up to 32bytes (64bytes with `avx`).
//! If you know most of you copy operations are not too big you can use `fastcpy` to speed up your program.
//!
//! `fastcpy` is designed to contain not too much assembly, so the overhead is low.
//!
//! As fall back the standard `memcpy` is called
//!
//! ## Double Copy Trick
//! `fastcpy` employs a double copy trick to copy slices of length 4-32bytes (64bytes with `avx`).
//! E.g. Slice of length 6 can be copied with two uncoditional copy operations.
//!
//! /// [1, 2, 3, 4, 5, 6]
//! /// [1, 2, 3, 4]
//! ///       [3, 4, 5, 6]
//!

#[inline]
pub fn slice_copy(src: &[u8], dst: &mut [u8]) {
    #[inline(never)]
    #[cold]
    #[track_caller]
    fn len_mismatch_fail(dst_len: usize, src_len: usize) -> ! {
        panic!(
            "source slice length ({}) does not match destination slice length ({})",
            src_len, dst_len,
        );
    }

    if src.len() != dst.len() {
        len_mismatch_fail(src.len(), dst.len());
    }
    let len = src.len();

    if src.is_empty() {
        return;
    }

    if len < 4 {
        short_copy(src, dst);
        return;
    }

    if len < 8 {
        double_copy_trick::<4>(src, dst);
        return;
    }

    if len <= 16 {
        double_copy_trick::<8>(src, dst);
        return;
    }

    if len <= 32 {
        double_copy_trick::<16>(src, dst);
        return;
    }

    /// The code will use the vmovdqu instruction to copy 32 bytes at a time.
    #[cfg(target_feature = "avx")]
    {
        if len <= 64 {
            double_copy_trick::<32>(src, dst);
            return;
        }
    }

    // For larger sizes we use the default, which calls memcpy
    // memcpy does some virtual memory tricks to copy large chunks of memory.
    //
    // The theory should be that the checks above don't cost much relative to the copy call for
    // larger copies.
    // The bounds checks in `copy_from_slice` are elided.
    dst.copy_from_slice(src);
}

#[inline(always)]
fn short_copy(src: &[u8], dst: &mut [u8]) {
    let len = src.len();

    // length 1-3
    dst[0] = src[0];
    if len >= 2 {
        double_copy_trick::<2>(src, dst);
    }
}

#[inline(always)]
/// [1, 2, 3, 4, 5, 6]
/// [1, 2, 3, 4]
///       [3, 4, 5, 6]
fn double_copy_trick<const SIZE: usize>(src: &[u8], dst: &mut [u8]) {
    dst[0..SIZE].copy_from_slice(&src[0..SIZE]);
    dst[src.len() - SIZE..].copy_from_slice(&src[src.len() - SIZE..]);
}

#[cfg(test)]
mod tests {
    use super::slice_copy;
    use alloc::vec::Vec;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn test_fast_short_slice_copy(left: Vec<u8>) {
            let mut right = vec![0u8; left.len()];
            slice_copy(&left, &mut right);
            prop_assert_eq!(&left, &right);
        }
    }

    #[test]
    fn test_fast_short_slice_copy_edge_cases() {
        for len in 0..(512 * 2) {
            let left = (0..len).map(|i| i as u8).collect::<Vec<_>>();
            let mut right = vec![0u8; len];
            slice_copy(&left, &mut right);
            assert_eq!(left, right);
        }
    }

    #[test]
    fn test_fail2() {
        let left = vec![
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31, 32,
        ];
        let mut right = vec![0u8; left.len()];
        slice_copy(&left, &mut right);
        assert_eq!(left, right);
    }

    #[test]
    fn test_fail() {
        let left = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut right = vec![0u8; left.len()];
        slice_copy(&left, &mut right);
        assert_eq!(left, right);
    }
}
