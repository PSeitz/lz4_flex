//! High compression algorithm implementation.
//!
//! This module implements the LZ4 high compression algorithm using separate
//! implementations for the mid, hash-chain, and optimal parsing strategies.
//!
//! It includes three compression strategies:
//! - `compress_hc`: The shared public entry point
//! - `Mid`: Intermediate compression for levels 0-2
//! - `HashChain` / `Optimal`: Deeper HC parsing for levels 3-12

use crate::block::CompressError;
#[cfg(not(feature = "safe-encode"))]
use crate::sink::PtrSink;
use crate::sink::Sink;
#[cfg(feature = "safe-encode")]
use crate::sink::SliceSink;
#[allow(unused_imports)]
use alloc::vec;
use alloc::vec::Vec;

mod hash_chain;
mod mid;
mod optimal;
#[cfg(test)]
mod tests;

use hash_chain::{compress_hash_chain_internal, HashTableHCU32};
use mid::{compress_mid_internal, HashTableMid};
use optimal::compress_opt_internal;

const HASHTABLE_SIZE_HC: usize = 1 << 15;
const MAX_DISTANCE_HC: usize = 1 << 16;

// LZ4MID constants (for levels 1-2)
const LZ4MID_HASH_LOG: usize = 15;
const LZ4MID_HASHTABLE_SIZE: usize = 1 << LZ4MID_HASH_LOG;

const OPTIMAL_MATCH_LENGTH: usize = 32;
const MATCH_LENGTH_MASK: usize = 31;

/// Size of the optimal parsing buffer
const LZ4_OPT_NUM: usize = 1 << 12; // 4096

/// Number of trailing literals to consider after last match
const TRAILING_LITERALS: usize = 3;

/// Run mask for literal/match length encoding
const RUN_MASK: usize = 15;

/// Which high-compression strategy applies for a given level (after clamping to 12).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HcCompressionStrategy {
    /// Levels 0–2: intermediate (lz4mid-style) compressor.
    Mid,
    /// Levels 3–9: hash-chain HC.
    HashChain,
    /// Levels 10–12: optimal parsing.
    Optimal,
}

/// Resolved parameters for HC compression (mid, hash-chain, or optimal).
///
/// Call [`hc_level_params`] once per block / public API entry, then pass this through internal
/// helpers so level is not re-mapped repeatedly.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HcLevelParams {
    pub(crate) strategy: HcCompressionStrategy,
    /// Hash-chain search budget for [`HcCompressionStrategy::HashChain`] and [`HcCompressionStrategy::Optimal`]
    /// Zero in [`HcCompressionStrategy::Mid`].
    pub(crate) max_attempts: usize,
    /// Only for [`HcCompressionStrategy::Optimal`]: encode immediately when the first match is at least this long.
    pub(crate) sufficient_match_len: usize,
    /// Only for [`HcCompressionStrategy::Optimal`]: exhaustive refinement (level 12).
    pub(crate) full_optimal_update: bool,
}

/// Map a compression level to strategy and parameters. `level` should be clamped with `min(12)` first.
#[inline]
pub(crate) const fn hc_level_params(level: u8) -> HcLevelParams {
    match level {
        // The C reference (v1.10.0+) remaps level 0 to 9 (default) and only
        // exposes level 2 as the lz4mid entry point (LZ4HC_CLEVEL_MIN = 2).
        // We treat 0–2 uniformly as Mid for a simpler "0 = fastest" mapping.
        0..=2 => HcLevelParams {
            strategy: HcCompressionStrategy::Mid,
            max_attempts: 0,
            sufficient_match_len: 0,
            full_optimal_update: false,
        },
        3..=9 => HcLevelParams {
            strategy: HcCompressionStrategy::HashChain,
            max_attempts: 1usize << (level - 1),
            sufficient_match_len: 0,
            full_optimal_update: false,
        },
        10 => HcLevelParams {
            strategy: HcCompressionStrategy::Optimal,
            max_attempts: 96,
            sufficient_match_len: 64,
            full_optimal_update: false,
        },
        11 => HcLevelParams {
            strategy: HcCompressionStrategy::Optimal,
            max_attempts: 512,
            sufficient_match_len: 128,
            full_optimal_update: false,
        },
        // 12, or defensive fallback if `level` was not clamped
        _ => HcLevelParams {
            strategy: HcCompressionStrategy::Optimal,
            max_attempts: 16384,
            sufficient_match_len: LZ4_OPT_NUM,
            full_optimal_update: true,
        },
    }
}

/// A reusable compression table for the HC algorithm that avoids re-allocating
/// internal hash tables on every call.
///
/// This is useful when compressing many inputs in a loop (e.g. frame blocks).
/// Create one table and pass it to [`compress_hc_with_table`] repeatedly.
///
/// The table automatically selects the right internal variant (mid vs HC)
/// based on the compression level, upgrading transparently when needed.
///
/// # Example
/// ```
/// use lz4_flex::block::{compress_hc_to_vec_with_table, CompressTableHC};
///
/// let mut table = CompressTableHC::new();
/// for input in [b"block one".as_slice(), b"block two".as_slice()] {
///     let compressed = compress_hc_to_vec_with_table(input, 9, &mut table);
/// }
/// ```
pub struct CompressTableHC {
    inner: CompressTableHCInner,
}

enum CompressTableHCInner {
    Mid(HashTableMid),
    HC(HashTableHCU32),
}

impl Default for CompressTableHC {
    fn default() -> Self {
        Self::new()
    }
}

impl CompressTableHC {
    /// Create a new table. The internal variant is lazily chosen on first use.
    pub fn new() -> Self {
        CompressTableHC {
            inner: CompressTableHCInner::Mid(HashTableMid::new()),
        }
    }

    /// Get (or create) the Mid table, resetting it for a fresh block.
    fn reset_mid(&mut self) -> &mut HashTableMid {
        if !matches!(self.inner, CompressTableHCInner::Mid(_)) {
            self.inner = CompressTableHCInner::Mid(HashTableMid::new());
        }
        match &mut self.inner {
            CompressTableHCInner::Mid(mid) => {
                mid.reset();
                mid
            }
            _ => unreachable!(),
        }
    }

    /// Get (or create) the HC table, resetting it for a fresh block.
    fn reset_hc(&mut self, max_attempts: usize, input_len: usize) -> &mut HashTableHCU32 {
        if let CompressTableHCInner::HC(ht) = &mut self.inner {
            ht.reset(max_attempts, input_len);
        } else {
            self.inner = CompressTableHCInner::HC(HashTableHCU32::new(max_attempts, input_len));
        }
        match &mut self.inner {
            CompressTableHCInner::HC(ht) => ht,
            _ => unreachable!(),
        }
    }

    /// Prepare the table for a new linked block without clearing existing entries.
    /// Called by `FrameEncoder` between blocks in linked mode.
    ///
    /// `params` must be [`hc_level_params`] with the same clamped level used for the following
    /// [`compress_hc_linked`] call (typically `hc_level_params(level.min(12))`).
    #[cfg(feature = "frame")]
    pub(crate) fn prepare_linked_block(&mut self, params: HcLevelParams, block_start: usize) {
        match params.strategy {
            HcCompressionStrategy::Mid => match &mut self.inner {
                CompressTableHCInner::Mid(mid) => {
                    mid.prepare_linked_block();
                }
                _ => {
                    self.inner = CompressTableHCInner::Mid(HashTableMid::new());
                }
            },
            HcCompressionStrategy::HashChain | HcCompressionStrategy::Optimal => {
                let max_attempts = params.max_attempts;
                match &mut self.inner {
                    CompressTableHCInner::HC(ht) => {
                        ht.prepare_linked_block(max_attempts, block_start);
                    }
                    _ => {
                        let mut ht = HashTableHCU32::new(max_attempts, MAX_DISTANCE_HC);
                        ht.prepare_linked_block(max_attempts, block_start);
                        self.inner = CompressTableHCInner::HC(ht);
                    }
                }
            }
        }
    }

    /// Subtract `delta` from all stored positions to prevent overflow.
    #[cfg(feature = "frame")]
    pub(crate) fn reposition(&mut self, delta: usize) {
        match &mut self.inner {
            CompressTableHCInner::HC(ht) => ht.reposition(delta),
            CompressTableHCInner::Mid(mid) => mid.reposition(delta),
        }
    }
}

/// Compress input data using the LZ4 high compression algorithm.
///
/// Allocates a fresh internal table on every call. For repeated compression
/// (e.g. compressing many blocks in a loop), prefer [`compress_hc_with_table`]
/// with a reusable [`CompressTableHC`] to avoid repeated allocation.
///
/// # Compression levels
/// - **Levels 1-2**: lz4mid intermediate algorithm
/// - **Levels 3-9**: HC hash chain algorithm with increasing search depth
/// - **Levels 10-12**: Optimal parsing (dynamic programming) for maximum compression
///
/// # Example
/// ```ignore
/// use lz4_flex::block::compress_hc;
/// let input = b"Hello, this is some data to compress!";
/// let mut output = vec![0u8; input.len() * 2];
/// let size = compress_hc(input, &mut output, 9).unwrap(); // HC algorithm
/// let size = compress_hc(input, &mut output, 12).unwrap(); // Optimal algorithm
/// ```
pub fn compress_hc(
    input: &[u8],
    output: &mut impl Sink,
    level: u8,
) -> Result<usize, CompressError> {
    let level = level.min(12);
    let params = hc_level_params(level);

    match params.strategy {
        HcCompressionStrategy::Optimal => {
            let mut hash_table = HashTableHCU32::new(params.max_attempts, input.len());
            compress_opt_internal(input, 0, output, params, &mut hash_table, &[], 0)
        }
        HcCompressionStrategy::HashChain => {
            let mut hash_table = HashTableHCU32::new(params.max_attempts, input.len());
            compress_hash_chain_internal(input, 0, output, &mut hash_table, &[], 0)
        }
        HcCompressionStrategy::Mid => {
            let mut mid_table = HashTableMid::new();
            compress_mid_internal::<false>(input, 0, output, &mut mid_table, &[], 0)
        }
    }
}

/// Compress input data using the LZ4 high compression algorithm, reusing a
/// [`CompressTableHC`] to avoid re-allocating internal hash tables.
///
/// The table is automatically reset before each call. If the level changes
/// between calls (e.g. mid vs HC), the table is transparently upgraded.
///
/// See [`compress_hc`] for compression level details.
///
/// # Example
/// ```ignore
/// use lz4_flex::block::{compress_hc_with_table, get_maximum_output_size, CompressTableHC};
///
/// let mut table = CompressTableHC::new();
/// let input = b"Hello, this is some data to compress with HC!";
/// let mut output = vec![0u8; get_maximum_output_size(input.len())];
/// let n = compress_hc_with_table(input, &mut output, 9, &mut table).unwrap();
/// ```
pub fn compress_hc_with_table(
    input: &[u8],
    output: &mut impl Sink,
    level: u8,
    table: &mut CompressTableHC,
) -> Result<usize, CompressError> {
    let level = level.min(12);
    let params = hc_level_params(level);

    match params.strategy {
        HcCompressionStrategy::Mid => {
            let mid_table = table.reset_mid();
            compress_mid_internal::<false>(input, 0, output, mid_table, &[], 0)
        }
        HcCompressionStrategy::HashChain => {
            let hash_table = table.reset_hc(params.max_attempts, input.len());
            compress_hash_chain_internal(input, 0, output, hash_table, &[], 0)
        }
        HcCompressionStrategy::Optimal => {
            let hash_table = table.reset_hc(params.max_attempts, input.len());
            compress_opt_internal(input, 0, output, params, hash_table, &[], 0)
        }
    }
}

/// Compress with HC using linked-block mode. Called by the frame encoder.
/// `input` includes the prefix at `[0..input_pos]`, current block at `[input_pos..]`.
/// `table` must have been prepared via [`CompressTableHC::prepare_linked_block`] with the same
/// `params` (typically one [`hc_level_params`] call per block, shared with prepare).
#[cfg(feature = "frame")]
pub(crate) fn compress_hc_linked(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    params: HcLevelParams,
    table: &mut CompressTableHC,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    match params.strategy {
        HcCompressionStrategy::Mid => {
            let mid_table = match &mut table.inner {
                CompressTableHCInner::Mid(mid_table) => mid_table,
                _ => unreachable!(
                    "prepare_linked_block should have ensured Mid variant for mid levels"
                ),
            };
            if ext_dict.is_empty() {
                compress_mid_internal::<false>(
                    input,
                    input_pos,
                    output,
                    mid_table,
                    ext_dict,
                    stream_offset,
                )
            } else {
                compress_mid_internal::<true>(
                    input,
                    input_pos,
                    output,
                    mid_table,
                    ext_dict,
                    stream_offset,
                )
            }
        }
        HcCompressionStrategy::HashChain => {
            let hash_table = match &mut table.inner {
                CompressTableHCInner::HC(hash_table) => hash_table,
                _ => unreachable!(
                    "prepare_linked_block should have ensured HC variant for HC levels"
                ),
            };
            compress_hash_chain_internal(
                input,
                input_pos,
                output,
                hash_table,
                ext_dict,
                stream_offset,
            )
        }
        HcCompressionStrategy::Optimal => {
            let hash_table = match &mut table.inner {
                CompressTableHCInner::HC(hash_table) => hash_table,
                _ => unreachable!(
                    "prepare_linked_block should have ensured HC variant for optimal levels"
                ),
            };
            compress_opt_internal(
                input,
                input_pos,
                output,
                params,
                hash_table,
                ext_dict,
                stream_offset,
            )
        }
    }
}

/// Compress input data using the LZ4 high compression algorithm, returning a Vec.
///
/// This is a convenience function that allocates the output buffer internally.
/// See [`compress_hc`] for details on the compression algorithm and levels.
///
/// # Arguments
/// * `input` - The input data to compress
/// * `level` - Compression level (1-12), higher means better compression but slower
///
/// # Returns
/// A Vec containing the compressed data
///
/// # Example
/// ```
/// use lz4_flex::block::compress_hc_to_vec;
/// let input = b"Hello, this is some data to compress!";
/// let compressed = compress_hc_to_vec(input, 9); // HC algorithm
/// let compressed = compress_hc_to_vec(input, 12); // Optimal algorithm
/// ```
pub fn compress_hc_to_vec(input: &[u8], level: u8) -> Vec<u8> {
    let max_size = crate::block::compress::get_maximum_output_size(input.len());
    #[cfg(feature = "safe-encode")]
    {
        let mut output = vec![0u8; max_size];
        let mut sink = SliceSink::new(&mut output, 0);
        let compressed_size = compress_hc(input, &mut sink, level).unwrap();
        output.truncate(compressed_size);
        output
    }
    #[cfg(not(feature = "safe-encode"))]
    {
        let mut output = Vec::with_capacity(max_size);
        let compressed_size =
            compress_hc(input, &mut PtrSink::from_vec(&mut output, 0), level).unwrap();
        unsafe {
            output.set_len(compressed_size);
        }
        output.shrink_to_fit();
        output
    }
}

/// Compress input data using the LZ4 high compression algorithm, returning a Vec
/// and reusing a [`CompressTableHC`].
///
/// This is the reusable-table variant of [`compress_hc_to_vec`]. See
/// [`compress_hc_with_table`] for details on table reuse.
///
/// # Example
/// ```
/// use lz4_flex::block::{compress_hc_to_vec_with_table, CompressTableHC};
///
/// let mut table = CompressTableHC::new();
/// let compressed = compress_hc_to_vec_with_table(b"data to compress", 9, &mut table);
/// ```
pub fn compress_hc_to_vec_with_table(
    input: &[u8],
    level: u8,
    table: &mut CompressTableHC,
) -> Vec<u8> {
    let max_size = crate::block::compress::get_maximum_output_size(input.len());
    #[cfg(feature = "safe-encode")]
    {
        let mut output = vec![0u8; max_size];
        let mut sink = SliceSink::new(&mut output, 0);
        let compressed_size = compress_hc_with_table(input, &mut sink, level, table).unwrap();
        output.truncate(compressed_size);
        output
    }
    #[cfg(not(feature = "safe-encode"))]
    {
        let mut output = Vec::with_capacity(max_size);
        let compressed_size =
            compress_hc_with_table(input, &mut PtrSink::from_vec(&mut output, 0), level, table)
                .unwrap();
        unsafe {
            output.set_len(compressed_size);
        }
        output.shrink_to_fit();
        output
    }
}
