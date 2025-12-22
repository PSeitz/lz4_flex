// Direct block API test - no frame overhead
use lz4_flex::block::{compress, decompress};

static mut OUTPUT_BUFFER: [u8; 2 * 1024 * 1024] = [0u8; 2 * 1024 * 1024];
static mut DECOMP_BUFFER: [u8; 2 * 1024 * 1024] = [0u8; 2 * 1024 * 1024];

#[no_mangle]
pub extern "C" fn compress_block(input_ptr: *const u8, input_len: usize) -> usize {
    let input = unsafe { core::slice::from_raw_parts(input_ptr, input_len) };
    let compressed = compress(input);
    let len = compressed.len();
    unsafe {
        OUTPUT_BUFFER[..len].copy_from_slice(&compressed);
    }
    len
}

#[no_mangle]
pub extern "C" fn decompress_block(input_ptr: *const u8, input_len: usize, expected_size: usize) -> usize {
    let input = unsafe { core::slice::from_raw_parts(input_ptr, input_len) };
    match decompress(input, expected_size) {
        Ok(decompressed) => {
            let len = decompressed.len();
            unsafe {
                DECOMP_BUFFER[..len].copy_from_slice(&decompressed);
            }
            len
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn get_output_ptr() -> *const u8 {
    unsafe { OUTPUT_BUFFER.as_ptr() }
}

#[no_mangle]
pub extern "C" fn get_decomp_ptr() -> *const u8 {
    unsafe { DECOMP_BUFFER.as_ptr() }
}

#[no_mangle]
pub extern "C" fn test_roundtrip() -> i32 {
    let input = b"Hello World! This is a test of LZ4 compression with SIMD optimizations. \
                  We need enough data to trigger the compression algorithm properly. \
                  Hello World! This is a test of LZ4 compression with SIMD optimizations. \
                  We need enough data to trigger the compression algorithm properly.";
    
    let compressed = compress(input);
    match decompress(&compressed, input.len()) {
        Ok(decompressed) if decompressed == input => 1,
        _ => 0,
    }
}
