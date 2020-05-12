//! https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md
pub mod compress;
pub mod decompress;


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