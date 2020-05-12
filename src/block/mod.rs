//! https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md
pub mod compress;
pub mod decompress;

#[allow(dead_code)]
const MFLIMIT: u32 = 12;
/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md#end-of-block-restrictions
/// Minimum length of a block
///
/// MFLIMIT + 1 for the token.
#[allow(dead_code)]
static LZ4_MIN_LENGTH: u32 = MFLIMIT+1;
#[allow(dead_code)]
const MATCH_LENGTH_MASK: u32 = (1_u32 << 4) - 1; // 0b1111 / 15
#[allow(dead_code)]
const MINMATCH: usize = 4;
#[allow(dead_code)]
const LZ4_HASHLOG: u32 = 12;

/// Switch for the hashtable size byU16
#[allow(dead_code)]
static LZ4_64KLIMIT: u32 = (64 * 1024) + (MFLIMIT - 1);


pub(crate) fn hash(sequence:u32) -> u32 {
    let res = (sequence.wrapping_mul(2654435761_u32))
            >> (7 + (MINMATCH as u32 * 8) - (LZ4_HASHLOG + 1));
    res
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