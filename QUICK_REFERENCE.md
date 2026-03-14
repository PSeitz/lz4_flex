# LZ4_FLEX Quick Reference Guide

## What is LZ4_FLEX?
A high-performance LZ4 compression library in pure Rust with optional safe/unsafe variants.
- **Compression Speed:** 1.2-1.6 GiB/s (safe/unsafe)
- **Decompression Speed:** 2.3-6.0 GiB/s (safe/unsafe)
- **Features:** Block format (in-memory), Frame format (streaming)

## Quick Start

### Basic Usage (Block Format)
```rust
use lz4_flex::block::{compress_prepend_size, decompress_size_prepended};

let input = b"Hello, world!";
let compressed = compress_prepend_size(input);
let decompressed = decompress_size_prepended(&compressed)?;
assert_eq!(input, &decompressed[..]);
```

### Streaming (Frame Format)
```rust
use std::io::{Read, Write};
use lz4_flex::frame::{FrameEncoder, FrameDecoder};

// Compress
let mut encoder = FrameEncoder::new(Vec::new());
encoder.write_all(b"data")?;
let compressed = encoder.finish()?;

// Decompress
let mut decoder = FrameDecoder::new(&compressed[..]);
let mut decompressed = Vec::new();
decoder.read_to_end(&mut decompressed)?;
```

## Building

```bash
cargo build --release                              # Safe+frame (default)
cargo build --no-default-features                 # Unsafe, no frame
cargo build --no-default-features --features frame # Unsafe + frame
```

## Testing

```bash
cargo test                                    # All tests
cargo test --no-default-features              # Unsafe code tests
cargo +nightly fuzz run fuzz_roundtrip         # Fuzzing
```

## Benchmarking

```bash
cargo bench                       # Safe mode
cargo bench --no-default-features # Unsafe mode
```

## Key Files

| File | Purpose |
|------|---------|
| `src/block/compress.rs` (999 lines) | Compression algorithm using hash tables |
| `src/block/decompress.rs` (543 lines) | Fast unsafe decompression |
| `src/block/decompress_safe.rs` (400 lines) | Safe decompression (bounds-checked) |
| `src/frame/compress.rs` (471 lines) | Streaming compression with checksums |
| `src/frame/decompress.rs` (448 lines) | Streaming decompression |
| `src/block/hashtable.rs` | Hash table for duplicate detection |
| `src/sink.rs` | Buffer abstraction for compression |

## Features

- `safe-encode` (default): Safe Rust for compression
- `safe-decode` (default): Safe Rust for decompression
- `frame` (default): Streaming LZ4 frame format (requires std)
- `std` (default): Standard library support
- `checked-decode`: Additional decompression safety checks

## Compression Algorithm

1. Hash each 4-byte sequence
2. Look up hash to find previous matches
3. Encode as: `[token][literal_length][literals][match_offset][match_length]`
4. Token byte: `[4-bit lit_len][4-bit match_len]`
5. If length >= 15: use extension bytes (255 + 255 + ... + remainder)

**Constants:**
- Minimum match: 4 bytes
- Maximum back-reference distance: 64 KB
- Window size: 64 KB

## Decompression Algorithm

1. Read token byte
2. Parse literal length (upper 4 bits)
3. Copy literals from input
4. Parse match offset (16-bit little-endian)
5. Parse match length (lower 4 bits)
6. Copy from output[pos - offset] (handles overlapping regions)
7. Repeat

**Fast Path Optimization:**
- If token fits in single byte AND safe distance from end
- Use 16-byte literal copy + 18-byte match copy

## Performance Characteristics

| Test | Unsafe | Safe | vs C (lzzz) |
|------|--------|------|------------|
| 66KB JSON compress | 1615 MiB/s | 1272 MiB/s | +10% |
| 66KB JSON decompress | 5512 MiB/s | 4540 MiB/s | +4% |
| 10MB text compress | 347 MiB/s | 259 MiB/s | +7% |
| 10MB text decompress | 2734 MiB/s | 2338 MiB/s | -1% |

## Common Performance TODOs

1. **Line 261 of compress.rs:** Remove bounds checks in backtrack_match
2. **Line 106 of decompress.rs:** Test fastcpy_unsafe in dictionary path
3. **Lines 248, 271 of frame/decompress.rs:** Lazy buffer initialization
4. **Line 534 of compress.rs:** Prevent compiler auto-vectorization of literal copy

## Error Types

**DecompressError:**
- `OutputTooSmall { expected, actual }`
- `LiteralOutOfBounds`
- `ExpectedAnotherByte`
- `OffsetOutOfBounds`

**CompressError:**
- `OutputTooSmall`

## Block Format Constraints

- Last match must start ≥12 bytes before end
- Last 5 bytes must be literals
- Independent blocks <13 bytes can't be compressed
- Maximum distance: 65,535 bytes

## Frame Format Features

- Magic number: `0x184D2204` (4 bytes)
- Optional block checksums (xxhash32)
- Optional content checksum
- Optional content size header
- Independent or linked blocks
- Configurable block sizes: 64KB, 256KB, 1MB, 4MB, 8MB

## CI/CD

**GitHub Actions (`.github/workflows/rust.yml`):**
- Tests with nightly Rust
- No-std compilation verification
- Fuzzing tests (safe + unsafe)
- Semver checking

## Dependencies

**Runtime:**
- `twox-hash` 2.0.0 (optional, frame format)
- `alloc` (required)

**Dev:**
- lzzzz (C LZ4 bindings for testing)
- snap (Snappy for comparison)
- proptest (property-based testing)
- binggan (benchmarking)

## Examples

Located in `/examples/`:
- `compress.rs` - Frame format compression
- `decompress.rs` - Frame format decompression
- `compress_block.rs` - Block format compression
- `decompress_block.rs` - Block format decompression

Run: `cargo run --example compress < input.txt > output.lz4`

## Fuzzing

```bash
cargo +nightly fuzz run fuzz_roundtrip -- -max_total_time=30
cargo +nightly fuzz run fuzz_decomp_corrupt_block -- -max_total_time=30
cargo +nightly fuzz run fuzz_roundtrip_cpp_compress -- -max_total_time=30
```

## Miri (Undefined Behavior Detection)

```bash
MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-disable-stacked-borrows" \
  cargo +nightly miri test --no-default-features --features frame
```

## Memory Usage

- Hash table: 4K entries × 2-4 bytes = 8-16 KB
- Window size: 64 KB (back-reference limit)
- Output buffer: input_size + overhead
- No streaming allocations

## Testing Datasets

Included in `/benches/`:
- compression_1k.txt (1 KB)
- compression_34k.txt (34 KB)
- compression_65k.txt (65 KB)
- compression_66k_JSON.txt (66 KB - JSON, highly compressible)
- dickens.txt (10 MB - English text)
- logo.jpg (95 KB - binary/image)

