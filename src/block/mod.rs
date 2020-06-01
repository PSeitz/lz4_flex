//! https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md
pub mod compress;
pub mod decompress;

/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md#end-of-block-restrictions
/// The last match must start at least 12 bytes before the end of block. The last match is part of the penultimate sequence.
/// It is followed by the last sequence, which contains only literals.
///
/// Note that, as a consequence, an independent block < 13 bytes cannot be compressed, because the match must copy "something",
/// so it needs at least one prior byte.
///
/// When a block can reference data from another block, it can start immediately with a match and no literal, so a block of 12 bytes can be compressed.
const MFLIMIT: u32 = 16;

/// The last 5 bytes of input are always literals. Therefore, the last sequence contains at least 5 bytes.
const END_OFFSET: usize = 7;

const LZ4_SKIPTRIGGER: usize = 4;

/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md#end-of-block-restrictions
/// Minimum length of a block
///
/// MFLIMIT + 1 for the token.
#[allow(dead_code)]
const LZ4_MIN_LENGTH: u32 = MFLIMIT + 1;

const MAXD_LOG: usize = 16;
const MAX_DISTANCE: usize = (1 << MAXD_LOG) - 1;

#[allow(dead_code)]
const MATCH_LENGTH_MASK: u32 = (1_u32 << 4) - 1; // 0b1111 / 15

const MINMATCH: usize = 4;
const LZ4_HASHLOG: u32 = 16;

#[allow(dead_code)]
const FASTLOOP_SAFE_DISTANCE: usize = 64;

/// Switch for the hashtable size byU16
#[allow(dead_code)]
static LZ4_64KLIMIT: u32 = (64 * 1024) + (MFLIMIT - 1);

// hashes and right shifts to a maximum value of 16bit, 65535
pub(crate) fn hash(sequence: u32) -> u32 {
    let res =
        (sequence.wrapping_mul(2654435761_u32)) >> (1 + (MINMATCH as u32 * 8) - (LZ4_HASHLOG + 1));
    res
}

fn wild_copy_from_src(mut source: *const u8, mut dst_ptr: *mut u8, num_items: usize) {
    unsafe {
        let dst_ptr_end = dst_ptr.add(num_items);
        while (dst_ptr as usize) < dst_ptr_end as usize {
            std::ptr::copy_nonoverlapping(source, dst_ptr, 16);
            source = source.add(16);
            dst_ptr = dst_ptr.add(16);
        }
    }
}

fn wild_copy_from_src_8(mut source: *const u8, mut dst_ptr: *mut u8, num_items: usize) {
    unsafe {
        let dst_ptr_end = dst_ptr.add(num_items);
        while (dst_ptr as usize) < dst_ptr_end as usize {
            std::ptr::copy_nonoverlapping(source, dst_ptr, 8);
            source = source.add(8);
            dst_ptr = dst_ptr.add(8);
        }
    }
}

// LZ4 Format
// Token 1 byte[Literal Length, Match Length (Neg Offset)]   -- 15, 15
// [Optional Literal Length bytes] [Literal] [Optional Match Length bytes]

// 100 bytes match length

// [Token] 4bit
// 15 token
// [Optional Match Length bytes] 1byte
// 85

// Compression
// match [10][4][6][100]  .....      in [10][4][6][40]
// 3
//
