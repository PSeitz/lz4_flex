//! SIMD optimizations for LZ4 compression/decompression
//!
//! This module provides WASM SIMD128 optimized versions of hot path operations.

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
use core::arch::wasm32::*;

/// Count matching bytes between two slices using SIMD.
/// Returns the number of bytes that match from the start.
///
/// This version uses WASM SIMD128 to compare 16 bytes at a time.
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
pub fn count_same_bytes_simd(a: &[u8], b: &[u8]) -> usize {
    let len = a.len().min(b.len());
    if len < 16 {
        return count_same_bytes_scalar(a, b);
    }

    let mut offset = 0;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    // Process 16 bytes at a time with SIMD
    while offset + 16 <= len {
        unsafe {
            let va = v128_load(a_ptr.add(offset) as *const v128);
            let vb = v128_load(b_ptr.add(offset) as *const v128);
            
            // Compare bytes: result is 0xFF for equal, 0x00 for different
            let eq = i8x16_eq(va, vb);
            
            // Get bitmask: bit N is 1 if lane N is all 1s (i.e., bytes match)
            let mask = i8x16_bitmask(eq) as u32;
            
            if mask == 0xFFFF {
                // All 16 bytes match, continue
                offset += 16;
            } else {
                // Some bytes don't match, find the first mismatch
                // mask has a 0 bit where bytes differ
                // We want to find the first 0 bit (first mismatch)
                let first_diff = (!mask).trailing_zeros() as usize;
                return offset + first_diff;
            }
        }
    }

    // Handle remaining bytes with scalar code
    while offset < len {
        if a[offset] != b[offset] {
            return offset;
        }
        offset += 1;
    }

    offset
}

/// Scalar fallback for counting matching bytes
#[inline]
pub fn count_same_bytes_scalar(a: &[u8], b: &[u8]) -> usize {
    let len = a.len().min(b.len());
    
    // Use usize-width comparisons for better performance
    const STEP: usize = core::mem::size_of::<usize>();
    let mut offset = 0;

    // Compare usize-width chunks
    while offset + STEP <= len {
        let a_word = unsafe { 
            (a.as_ptr().add(offset) as *const usize).read_unaligned()
        };
        let b_word = unsafe {
            (b.as_ptr().add(offset) as *const usize).read_unaligned()
        };
        
        if a_word != b_word {
            let diff = a_word ^ b_word;
            return offset + (diff.to_le().trailing_zeros() / 8) as usize;
        }
        offset += STEP;
    }

    // Handle remaining bytes
    while offset < len {
        if a[offset] != b[offset] {
            return offset;
        }
        offset += 1;
    }

    offset
}

/// Wild copy: copy `len` bytes from src to dst, potentially overwriting up to 15 extra bytes.
/// This is safe when the caller ensures dst has at least len + 15 bytes available.
///
/// Uses SIMD for 16-byte aligned copies.
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
pub unsafe fn wild_copy_16_simd(src: *const u8, dst: *mut u8, len: usize) {
    let mut offset = 0;
    
    // Copy 16 bytes at a time
    while offset < len {
        let v = v128_load(src.add(offset) as *const v128);
        v128_store(dst.add(offset) as *mut v128, v);
        offset += 16;
    }
}

/// Copy exactly 16 bytes using SIMD
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
pub unsafe fn copy_16_simd(src: *const u8, dst: *mut u8) {
    let v = v128_load(src as *const v128);
    v128_store(dst as *mut v128, v);
}

/// Copy exactly 32 bytes using SIMD (2x 16-byte copies)
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
pub unsafe fn copy_32_simd(src: *const u8, dst: *mut u8) {
    let v0 = v128_load(src as *const v128);
    let v1 = v128_load(src.add(16) as *const v128);
    v128_store(dst as *mut v128, v0);
    v128_store(dst.add(16) as *mut v128, v1);
}

/// Handle overlapping copy with small offsets using SIMD shuffle patterns.
/// For offset 1: broadcast the byte
/// For offset 2: duplicate 2-byte pattern
/// For offset 4: duplicate 4-byte pattern
/// For offset 8: duplicate 8-byte pattern
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
pub unsafe fn duplicate_pattern_simd(dst: *mut u8, src: *const u8, offset: usize, len: usize) {
    match offset {
        1 => {
            // Broadcast single byte to all lanes
            let b = *src;
            let v = i8x16_splat(b as i8);
            let mut written = 0;
            while written < len {
                v128_store(dst.add(written) as *mut v128, v);
                written += 16;
            }
        }
        2 => {
            // Broadcast 2-byte pattern
            let pattern = (src as *const u16).read_unaligned();
            let v = i16x8_splat(pattern as i16);
            let mut written = 0;
            while written < len {
                v128_store(dst.add(written) as *mut v128, v);
                written += 16;
            }
        }
        4 => {
            // Broadcast 4-byte pattern
            let pattern = (src as *const u32).read_unaligned();
            let v = i32x4_splat(pattern as i32);
            let mut written = 0;
            while written < len {
                v128_store(dst.add(written) as *mut v128, v);
                written += 16;
            }
        }
        8 => {
            // Broadcast 8-byte pattern
            let pattern = (src as *const u64).read_unaligned();
            let v = i64x2_splat(pattern as i64);
            let mut written = 0;
            while written < len {
                v128_store(dst.add(written) as *mut v128, v);
                written += 16;
            }
        }
        _ => {
            // For offsets >= 16 or other offsets, use regular copy
            // This should be handled before calling this function
            core::ptr::copy_nonoverlapping(src, dst, len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_same_bytes_scalar() {
        let a = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let b = [1, 2, 3, 4, 5, 0, 7, 8, 9, 10];
        assert_eq!(count_same_bytes_scalar(&a, &b), 5);

        let a = [1, 2, 3, 4, 5, 6, 7, 8];
        let b = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(count_same_bytes_scalar(&a, &b), 8);

        let a = [0; 32];
        let b = [0; 32];
        assert_eq!(count_same_bytes_scalar(&a, &b), 32);

        let a = [1, 2, 3];
        let b = [9, 2, 3];
        assert_eq!(count_same_bytes_scalar(&a, &b), 0);
    }

    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    #[test]
    fn test_count_same_bytes_simd() {
        // 32 matching bytes
        let a = [1u8; 32];
        let b = [1u8; 32];
        assert_eq!(count_same_bytes_simd(&a, &b), 32);

        // Mismatch at position 20
        let mut a = [1u8; 32];
        let mut b = [1u8; 32];
        b[20] = 0;
        assert_eq!(count_same_bytes_simd(&a, &b), 20);

        // Mismatch at position 0
        a[0] = 0;
        b[0] = 1;
        assert_eq!(count_same_bytes_simd(&a, &b), 0);
    }
}

