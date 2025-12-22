//! Benchmark to test SIMD vs scalar count_same_bytes performance
//!
//! Run with:
//! cargo run --example bench_simd --release

use std::time::Instant;

fn count_same_bytes_scalar(a: &[u8], b: &[u8]) -> usize {
    let len = a.len().min(b.len());
    
    const STEP: usize = core::mem::size_of::<usize>();
    let mut offset = 0;

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

    while offset < len {
        if a[offset] != b[offset] {
            return offset;
        }
        offset += 1;
    }

    offset
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
fn count_same_bytes_simd(a: &[u8], b: &[u8]) -> usize {
    use core::arch::wasm32::*;
    
    let len = a.len().min(b.len());
    if len < 16 {
        return count_same_bytes_scalar(a, b);
    }

    let mut offset = 0;
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();

    while offset + 16 <= len {
        unsafe {
            let va = v128_load(a_ptr.add(offset) as *const v128);
            let vb = v128_load(b_ptr.add(offset) as *const v128);
            let eq = i8x16_eq(va, vb);
            let mask = i8x16_bitmask(eq) as u32;
            
            if mask == 0xFFFF {
                offset += 16;
            } else {
                let first_diff = (!mask).trailing_zeros() as usize;
                return offset + first_diff;
            }
        }
    }

    while offset < len {
        if a[offset] != b[offset] {
            return offset;
        }
        offset += 1;
    }

    offset
}

fn main() {
    // Test data: two slices that match for the first N bytes
    let size = 1024 * 1024; // 1MB
    let match_point = 500_000; // Mismatch at 500KB
    
    let mut a = vec![0x42u8; size];
    let mut b = vec![0x42u8; size];
    b[match_point] = 0xFF; // Introduce a mismatch
    
    // Warm up
    for _ in 0..10 {
        let _ = count_same_bytes_scalar(&a, &b);
    }
    
    // Benchmark scalar
    let iterations = 100;
    let start = Instant::now();
    for _ in 0..iterations {
        let result = count_same_bytes_scalar(&a, &b);
        assert_eq!(result, match_point);
    }
    let scalar_time = start.elapsed();
    println!(
        "Scalar: {:?} per iteration ({:.2} GB/s)",
        scalar_time / iterations,
        (size as f64 * iterations as f64) / scalar_time.as_secs_f64() / 1e9
    );
    
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        // Warm up
        for _ in 0..10 {
            let _ = count_same_bytes_simd(&a, &b);
        }
        
        // Benchmark SIMD
        let start = Instant::now();
        for _ in 0..iterations {
            let result = count_same_bytes_simd(&a, &b);
            assert_eq!(result, match_point);
        }
        let simd_time = start.elapsed();
        println!(
            "SIMD: {:?} per iteration ({:.2} GB/s)",
            simd_time / iterations,
            (size as f64 * iterations as f64) / simd_time.as_secs_f64() / 1e9
        );
        println!("Speedup: {:.2}x", scalar_time.as_secs_f64() / simd_time.as_secs_f64());
    }
    
    #[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
    {
        println!("SIMD not available on this platform");
        println!("To test SIMD, compile for wasm32 with target-feature=+simd128");
    }
    
    // Also test with different match lengths
    println!("\n--- Match length tests ---");
    for match_len in [16, 64, 256, 1024, 4096, 16384, 65536] {
        let mut b2 = a.clone();
        if match_len < size {
            b2[match_len] = 0xFF;
        }
        
        let start = Instant::now();
        for _ in 0..1000 {
            let result = count_same_bytes_scalar(&a, &b2);
            assert_eq!(result, match_len.min(size));
        }
        let time = start.elapsed();
        println!(
            "Match len {}: {:?}/1000 iters ({:.2} bytes/ns)",
            match_len,
            time,
            (match_len as f64 * 1000.0) / time.as_nanos() as f64
        );
    }
}

