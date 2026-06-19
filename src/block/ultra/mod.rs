//! Optimal-parse "ultra" LZ4 block compressor (feature `ultra`).
//!
//! Produces standard LZ4 blocks, identical in format to those written by
//! [`crate::block::compress`], but uses an optimal parser instead of the fast greedy/lazy one for a
//! better compression ratio.
//!
//! Two engines are available, selected via [`crate::block::CompressionMode`]:
//! - [`Ultra`](crate::block::CompressionMode::Ultra) ([`Finder::SuffixArray`]): a suffix array
//!   ([`sais`]) + LCP-interval match finder ([`matchfinder`]) feeding a backward dynamic-program
//!   optimal parse ([`optimize`]). Finds the true longest match at every position and produces a
//!   decode-optimised stream. The most thorough strategy but several times slower to compress and
//!   uses ~`O(36 * input_len)` bytes; for bounded memory on large inputs prefer the
//!   [frame API][crate::frame] with a fixed block size.
//! - [`Hc`](crate::block::CompressionMode::Hc) ([`Finder::Lz4Hc`], [`lz4hc`]): a hash-chain match
//!   finder with chain-swapping plus a price-based optimal parser. Ratio close to `Ultra`, several
//!   times faster, far less memory.

mod lz4hc;
mod matchfinder;
mod optimize;
mod sais;

use alloc::vec;
use alloc::vec::Vec;

use crate::block::compress::get_maximum_output_size;
use crate::block::{
    CompressError, LAST_LITERALS, MAX_DISTANCE as MAX_OFFSET, MFLIMIT as LAST_MATCH_OFFSET,
    WINDOW_SIZE,
};
use crate::sink::{Sink, SliceSink};

use lz4hc::HcCompressor;
use matchfinder::MatchFinder;

const MIN_MATCH_SIZE: i32 = crate::block::MINMATCH as i32;
/// Literal-length run value that triggers variable-length encoding (token nibble max).
const LITERALS_RUN_LEN: i32 = 15;
/// Match-length run value that triggers variable-length encoding (token nibble max).
const MATCH_RUN_LEN: i32 = 15;
/// Matches at least this long skip the per-sub-length cost search (kept whole) for speed.
const LEAVE_ALONE_MATCH_SIZE: i32 = 1100;
/// Cost (in bits) added when switching between literal and match runs, to break ties toward fewer
/// mode switches.
const MODESWITCH_PENALTY: i32 = 1;

/// Number of extra variable-length-encoding bytes an encoded length needs. The token's 4-bit nibble
/// holds up to the run value (15); anything larger spills into `(len - 15) / 255` continuation bytes
/// of `0xFF` plus one final byte.
pub(super) fn varlen_extra_bytes(encoded_len: usize) -> usize {
    if encoded_len >= LITERALS_RUN_LEN as usize {
        (encoded_len - LITERALS_RUN_LEN as usize) / 255 + 1
    } else {
        0
    }
}

/// One match candidate: `length` of `0` means "emit a literal here" once the parse is chosen, and
/// `-1` is the join marker used by the command-count optimiser. `offset` is the back-distance.
#[derive(Clone, Copy)]
struct Match {
    pub length: i32,
    pub offset: u32,
}

/// Whether the optimal parser should favour ratio or decompression speed when costs tie.
#[derive(Clone, Copy)]
pub(crate) enum Favor {
    /// Best compression ratio.
    Ratio,
    /// Trade a little ratio for faster decompression.
    DecompressionSpeed,
}

/// Which compression strategy to use.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Finder {
    /// Suffix-array match finder + optimal-parse DP: always finds the true longest match at every
    /// position. Robust and near-optimal, but slower and more memory-hungry.
    SuffixArray,
    /// Hash-chain match finder with chain-swapping plus a price-based optimal parser. Same ratio as
    /// the suffix array on real data, several times faster.
    Lz4Hc,
}

/// Reusable optimal-parse compressor context. Owns the per-block scratch buffers so that callers
/// compressing many blocks (e.g. the frame encoder) don't reallocate each time.
pub(crate) struct UltraCompressor {
    sa_finder: MatchFinder,
    // Lazily created on first use: its fixed 256 KB hash+chain arrays shouldn't be allocated for
    // callers that only ever use the default suffix-array path.
    hc: Option<HcCompressor>,
    matches: Vec<Match>,
    cost: Vec<i32>,
    score: Vec<i32>,
}

impl UltraCompressor {
    pub(crate) fn new() -> Self {
        UltraCompressor {
            sa_finder: MatchFinder::new(),
            hc: None,
            matches: Vec::new(),
            cost: Vec::new(),
            score: Vec::new(),
        }
    }

    /// Optimally compress `window[prefix_len..]` into `out`, using `window[..prefix_len]` as
    /// already-emitted lookback (a prefix and/or external dictionary). Returns the number of bytes
    /// written. Offsets in the produced block are distances within `window`.
    pub(crate) fn compress_block(
        &mut self,
        window: &[u8],
        prefix_len: usize,
        favor: Favor,
        finder: Finder,
        out: &mut impl Sink,
    ) -> Result<usize, CompressError> {
        if let Finder::Lz4Hc = finder {
            let favor_dec_speed = matches!(favor, Favor::DecompressionSpeed);
            return self.hc.get_or_insert_with(HcCompressor::new).compress(
                window,
                prefix_len,
                favor_dec_speed,
                out,
            );
        }

        let n = window.len();
        if n == 0 {
            return optimize::write_block(window, 0, 0, &[], out);
        }

        let zero = Match {
            length: 0,
            offset: 0,
        };
        self.matches.clear();
        self.matches.resize(n, zero);
        self.cost.clear();
        self.cost.resize(n, 0);
        self.score.clear();
        self.score.resize(n, 0);

        self.sa_finder
            .find_all_matches(window, prefix_len, &mut self.matches);
        optimize::optimize_matches(
            prefix_len,
            n,
            &mut self.matches,
            &mut self.cost,
            &mut self.score,
            favor,
        );
        optimize::optimize_command_count(window, prefix_len, n, &mut self.matches);
        optimize::write_block(window, prefix_len, n, &self.matches, out)
    }
}

/// Optimally compress `input` (with optional `ext_dict` lookback) into `out` using the given
/// engine. Shared by the block `compress_*_with_mode` entry points and the frame encoder.
pub(crate) fn compress_one_shot(
    input: &[u8],
    ext_dict: &[u8],
    favor: Favor,
    finder: Finder,
    out: &mut impl Sink,
) -> Result<usize, CompressError> {
    let mut comp = UltraCompressor::new();
    // Only the last WINDOW_SIZE bytes of the dictionary are reachable (offsets are capped at 64 KiB);
    // dropping the rest doesn't change the output and bounds the suffix-array size.
    let dict = &ext_dict[ext_dict.len().saturating_sub(WINDOW_SIZE)..];
    if dict.is_empty() {
        comp.compress_block(input, 0, favor, finder, out)
    } else {
        let mut window = Vec::with_capacity(dict.len() + input.len());
        window.extend_from_slice(dict);
        window.extend_from_slice(input);
        comp.compress_block(&window, dict.len(), favor, finder, out)
    }
}

/// Optimally compress `input` into a freshly allocated `Vec` (optionally prepending the uncompressed
/// size as a little-endian `u32`, and optionally with an external dictionary).
pub(crate) fn compress_into_vec(
    input: &[u8],
    prepend_size: bool,
    ext_dict: &[u8],
    favor: Favor,
    finder: Finder,
) -> Vec<u8> {
    let prepend = if prepend_size { 4 } else { 0 };
    let max = get_maximum_output_size(input.len()) + prepend;
    let mut buf = vec![0u8; max];
    let written = {
        let (header, body) = buf.split_at_mut(prepend);
        if prepend_size {
            header.copy_from_slice(&(input.len() as u32).to_le_bytes());
        }
        let mut sink = SliceSink::new(body, 0);
        // The buffer is sized to `get_maximum_output_size`, which an optimal parse can never exceed
        // (it never does worse than emitting all literals), so this cannot fail.
        compress_one_shot(input, ext_dict, favor, finder, &mut sink)
            .expect("output buffer large enough")
    };
    buf.truncate(prepend + written);
    buf
}
