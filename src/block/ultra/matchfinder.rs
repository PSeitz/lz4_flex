//! LCP-interval match finder.
//!
//! For every position in the input it finds the single longest match (and a back-offset achieving
//! it). It works off a suffix array (built in [`super::sais`]) plus an LCP array, organised into
//! "LCP intervals". Because any prefix of a match at offset `D` is itself a match at offset `D`,
//! storing just the longest match per position (together with its offset) is enough for the optimal
//! parser to consider every useful sub-length.
//!
//! The 64-bit interval word gives positions a full 32 bits; the LCP field and visited flag are
//! shifted up accordingly.

// Index-arithmetic loops; iterator rewrites would obscure the algorithm.
#![allow(clippy::needless_range_loop)]

use alloc::vec::Vec;

use super::sais::build_suffix_array;
use super::{Match, LAST_LITERALS, LAST_MATCH_OFFSET, MAX_OFFSET, MIN_MATCH_SIZE};

const LCP_BITS: u32 = 15;
/// Match lengths are clamped to this (`1 << (LCP_BITS - 1)`).
pub(super) const LCP_MAX: i32 = 1 << (LCP_BITS - 1); // 16384

const LCP_SHIFT: u32 = 32;
const POS_MASK: u64 = (1u64 << LCP_SHIFT) - 1; // bits [0, 32)
const LCP_MASK: u64 = ((1u64 << LCP_BITS) - 1) << LCP_SHIFT; // bits [32, 47)
const VISITED_FLAG: u64 = 1u64 << (LCP_SHIFT + LCP_BITS); // bit 47
const EXCL_VISITED_MASK: u64 = VISITED_FLAG - 1; // bits [0, 47)

/// Scratch buffers for the match finder, reused across blocks to avoid reallocation.
pub(super) struct MatchFinder {
    /// LCP-interval linkage, and later visited markers (`pos | VISITED_FLAG`).
    intervals: Vec<u64>,
    /// Per text-position link into the interval tree.
    pos_data: Vec<u64>,
    /// Stack of currently-open intervals during interval construction.
    open: Vec<u64>,
    /// Permuted-LCP scratch (doubles as the Phi array while computing it).
    phi: Vec<i32>,
    /// (position, clamped LCP) packed per suffix-array rank, consumed by `build_intervals`.
    sa_lcp: Vec<u64>,
}

impl MatchFinder {
    pub(super) fn new() -> Self {
        MatchFinder {
            intervals: Vec::new(),
            pos_data: Vec::new(),
            open: Vec::new(),
            phi: Vec::new(),
            sa_lcp: Vec::new(),
        }
    }

    fn reserve(&mut self, n: usize) {
        self.intervals.clear();
        self.intervals.resize(n, 0);
        self.pos_data.clear();
        self.pos_data.resize(n, 0);
        self.phi.clear();
        self.phi.resize(n, 0);
        self.sa_lcp.clear();
        self.sa_lcp.resize(n, 0);
        // Open-interval stack holds strictly increasing clamped LCP values plus a base entry.
        let cap = LCP_MAX as usize + 2;
        self.open.clear();
        self.open.resize(cap, 0);
    }

    /// Build the suffix array, LCP, and LCP-interval structures over `window`.
    fn build(&mut self, window: &[u8]) {
        let n = window.len();
        self.reserve(n);

        let sa = build_suffix_array(window); // length n

        // Permuted-LCP: `plcp[i]` is the LCP of the suffix at text position `i` with its predecessor
        // in suffix-array order. `phi` doubles as `plcp` in place. `phi`/`sa_lcp` are reused across
        // blocks (resized in `reserve`).
        let phi = &mut self.phi;
        phi[sa[0] as usize] = -1;
        for r in 1..n {
            phi[sa[r] as usize] = sa[r - 1];
        }
        let mut cur_len = 0i32;
        for i in 0..n {
            let p = phi[i];
            if p < 0 {
                phi[i] = 0;
                continue;
            }
            let p = p as usize;
            let max_len = if i > p { n - i } else { n - p } as i32;
            while cur_len < max_len && window[i + cur_len as usize] == window[p + cur_len as usize]
            {
                cur_len += 1;
            }
            phi[i] = cur_len;
            if cur_len > 0 {
                cur_len -= 1;
            }
        }

        // Pack (position, clamped LCP) per suffix-array rank into the `sa_lcp` array.
        let phi = &self.phi;
        let sa_lcp = &mut self.sa_lcp;
        sa_lcp[0] = (sa[0] as u64) & POS_MASK; // first rank has LCP 0
        for r in 1..n {
            let idx = sa[r] as usize;
            let mut len = phi[idx];
            if len < MIN_MATCH_SIZE {
                len = 0;
            }
            if len > LCP_MAX {
                len = LCP_MAX;
            }
            sa_lcp[r] = (idx as u64) | ((len as u64) << LCP_SHIFT);
        }

        self.build_intervals();
    }

    /// Build LCP intervals from the (position, LCP) array.
    fn build_intervals(&mut self) {
        let sa_lcp = &self.sa_lcp;
        let n = sa_lcp.len();
        let intervals = &mut self.intervals;
        let pos_data = &mut self.pos_data;
        let open = &mut self.open;

        let mut top = 0usize;
        open[0] = 0;
        intervals[0] = 0;
        let mut next_interval_idx: u64 = 1;
        let mut prev_pos = sa_lcp[0] & POS_MASK;

        for r in 1..n {
            let next_pos = sa_lcp[r] & POS_MASK;
            let next_lcp = sa_lcp[r] & LCP_MASK;
            let top_lcp = open[top] & LCP_MASK;

            if next_lcp == top_lcp {
                // Continuing the deepest open interval.
                pos_data[prev_pos as usize] = open[top];
            } else if next_lcp > top_lcp {
                // Opening a new interval.
                top += 1;
                open[top] = next_lcp | next_interval_idx;
                next_interval_idx += 1;
                pos_data[prev_pos as usize] = open[top];
            } else {
                // Closing the deepest open interval (possibly several).
                pos_data[prev_pos as usize] = open[top];
                loop {
                    let closed_interval_idx = open[top] & POS_MASK;
                    top -= 1;
                    let superinterval_lcp = open[top] & LCP_MASK;

                    if next_lcp == superinterval_lcp {
                        intervals[closed_interval_idx as usize] = open[top];
                        break;
                    } else if next_lcp > superinterval_lcp {
                        top += 1;
                        open[top] = next_lcp | next_interval_idx;
                        next_interval_idx += 1;
                        intervals[closed_interval_idx as usize] = open[top];
                        break;
                    } else {
                        intervals[closed_interval_idx as usize] = open[top];
                    }
                }
            }
            prev_pos = next_pos;
        }

        // Close any still-open intervals.
        pos_data[prev_pos as usize] = open[top];
        while top > 0 {
            intervals[(open[top] & POS_MASK) as usize] = open[top - 1];
            top -= 1;
        }
    }

    /// Find matches at `n_offset`, storing up to `out.len()` of them (longest first). Always runs
    /// the full interval traversal (its lazy updates are needed for later positions) even when
    /// `out` is empty. Returns the number of matches stored.
    fn find_matches_at(&mut self, n_offset: usize, out: &mut [Match]) -> usize {
        let intervals = &mut self.intervals;
        let pos_data = &mut self.pos_data;
        let max_matches = out.len();

        let mut r = pos_data[n_offset];
        pos_data[n_offset] = 0;

        // Ascend to a visited interval / the root, linking unvisited intervals to this suffix.
        let mut super_ref;
        loop {
            super_ref = intervals[(r & POS_MASK) as usize];
            if super_ref & LCP_MASK == 0 {
                break;
            }
            intervals[(r & POS_MASK) as usize] = n_offset as u64 | VISITED_FLAG;
            r = super_ref;
        }

        if super_ref == 0 {
            if r != 0 {
                intervals[(r & POS_MASK) as usize] = n_offset as u64 | VISITED_FLAG;
            }
            return 0;
        }

        let mut match_pos = super_ref & EXCL_VISITED_MASK;
        let mut count = 0usize;
        loop {
            loop {
                super_ref = pos_data[match_pos as usize];
                if super_ref > r {
                    match_pos = intervals[(super_ref & POS_MASK) as usize] & EXCL_VISITED_MASK;
                } else {
                    break;
                }
            }
            intervals[(r & POS_MASK) as usize] = n_offset as u64 | VISITED_FLAG;
            pos_data[match_pos as usize] = r;

            if count < max_matches && match_pos < n_offset as u64 {
                let offset = n_offset - match_pos as usize;
                if offset <= MAX_OFFSET {
                    out[count] = Match {
                        length: (r >> LCP_SHIFT) as i32,
                        offset: offset as u32,
                    };
                    count += 1;
                }
            }

            if super_ref == 0 {
                break;
            }
            r = super_ref;
            match_pos = intervals[(r & POS_MASK) as usize] & EXCL_VISITED_MASK;
        }

        count
    }

    /// Find the longest match at every position in `window[prefix_len..]`, writing results into
    /// `matches[prefix_len..]`. Positions in `window[..prefix_len]` are treated as already-compressed
    /// lookback (their intervals are still traversed so the lazy updates stay consistent).
    pub(super) fn find_all_matches(
        &mut self,
        window: &[u8],
        prefix_len: usize,
        matches: &mut [Match],
    ) {
        let n = window.len();
        self.build(window);

        // Skip the prefix: scan for side effects only, discard the matches.
        for i in 0..prefix_len {
            self.find_matches_at(i, &mut []);
        }

        let mut scratch = [Match {
            length: 0,
            offset: 0,
        }];
        for i in prefix_len..n {
            let num = self.find_matches_at(i, &mut scratch);
            if num == 0 || i > n - LAST_MATCH_OFFSET {
                matches[i] = Match {
                    length: 0,
                    offset: 0,
                };
            } else {
                let mut m = scratch[0];
                let max_len = (n - LAST_LITERALS).saturating_sub(i) as i32;
                if m.length > max_len {
                    m.length = max_len;
                }
                matches[i] = m;
            }
        }
    }
}
