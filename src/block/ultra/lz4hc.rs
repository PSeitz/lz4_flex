//! A fast optimal-parse compressor: a hash-chain match finder feeding a price-based dynamic program.
//! It emits a standard LZ4 block directly, runs several times faster than the suffix-array path at a
//! comparable ratio, and is what [`super::CompressionMode::Hc`] selects. Output matches `lz4hc -12`.
//!
//! We only ever search a single contiguous window (callers fold a dictionary in as a prefix) and
//! always from the current position forwards, so there's no external-dictionary or backward-match
//! handling here. Long runs are kept fast by stopping as soon as a maximal-length match is found
//! (it's already optimal); general repetitive data relies on the chain-swap step below.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use crate::block::compress::{
    push_byte, push_u16, token_from_literal_and_match_length, write_integer,
};
use crate::block::{CompressError, LAST_LITERALS, MFLIMIT};
use crate::sink::Sink;

use super::MIN_MATCH_SIZE;

const HASH_LOG: u32 = 15;
const HASH_SIZE: usize = 1 << HASH_LOG; // 32768
const CHAIN_SIZE: usize = 1 << 16; // 65536, indexed by (pos as u16)
const BASE: u32 = 1 << 16; // index offset so an empty hash slot (0) reads as "too far"
const DISTANCE_MAX: u32 = crate::block::MAX_DISTANCE as u32;

const LZ4_OPT_NUM: usize = 1 << 12; // 4096
const TRAILING_LITERALS: usize = 3;
const OPT_SIZE: usize = LZ4_OPT_NUM + TRAILING_LITERALS;

// Max-effort search depth and the match length past which we stop looking for a better one.
const NB_SEARCHES: i32 = 16384;
const SUFFICIENT_LEN: i32 = (LZ4_OPT_NUM - 1) as i32;

#[inline]
fn read16(w: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([w[i], w[i + 1]])
}
#[inline]
fn read32(w: &[u8], i: usize) -> u32 {
    // Native-endian. Only used for hashing and equality checks, so endianness doesn't affect the
    // result.
    crate::block::compress::get_batch(w, i)
}
#[inline]
fn read64(w: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(w[i..i + 8].try_into().unwrap())
}
#[inline]
fn hash4(w: &[u8], i: usize) -> usize {
    (read32(w, i).wrapping_mul(2_654_435_761) >> (32 - HASH_LOG)) as usize
}

/// Common-prefix length of `w[a..]` and `w[b..]`, where `a` (the "in" side) may not reach `a_limit`.
#[inline]
fn count(w: &[u8], mut a: usize, mut b: usize, a_limit: usize) -> usize {
    let start = a;
    while a + 8 <= a_limit {
        let d = read64(w, a) ^ read64(w, b);
        if d != 0 {
            return a - start + (d.trailing_zeros() / 8) as usize;
        }
        a += 8;
        b += 8;
    }
    while a < a_limit && w[a] == w[b] {
        a += 1;
        b += 1;
    }
    a - start
}

#[derive(Clone, Copy)]
struct Opt {
    price: i32,
    off: i32,
    mlen: i32,
    litlen: i32,
}

/// Reusable hash-chain compressor state.
pub(crate) struct HcCompressor {
    hash: Box<[u32; HASH_SIZE]>,
    chain: Box<[u16; CHAIN_SIZE]>,
    opt: Vec<Opt>,
}

#[inline]
fn literals_price(litlen: i32) -> i32 {
    litlen + super::varlen_extra_bytes(litlen as usize) as i32
}

#[inline]
fn sequence_price(litlen: i32, mlen: i32) -> i32 {
    // token + 16-bit offset + literals + the match length's varlen bytes (encoded = mlen - MIN_MATCH_SIZE).
    1 + 2
        + literals_price(litlen)
        + super::varlen_extra_bytes((mlen - MIN_MATCH_SIZE) as usize) as i32
}

impl HcCompressor {
    pub(crate) fn new() -> Self {
        let hash: Box<[u32; HASH_SIZE]> =
            vec![0u32; HASH_SIZE].into_boxed_slice().try_into().unwrap();
        let chain: Box<[u16; CHAIN_SIZE]> = vec![0xFFFFu16; CHAIN_SIZE]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        HcCompressor {
            hash,
            chain,
            opt: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.hash.fill(0);
        self.chain.fill(0xFFFF);
        if self.opt.len() != OPT_SIZE {
            self.opt = vec![
                Opt {
                    price: 0,
                    off: 0,
                    mlen: 0,
                    litlen: 0
                };
                OPT_SIZE
            ];
        }
    }

    /// Insert hash-chain entries for all positions in `[next..target)` (positions are absolute
    /// indices = window offset + BASE). `next_to_update` tracks how far we've inserted.
    #[inline]
    fn insert(&mut self, w: &[u8], next_to_update: &mut u32, target_idx: u32) {
        let mut idx = *next_to_update;
        while idx < target_idx {
            let pos = (idx - BASE) as usize;
            let h = hash4(w, pos);
            let mut delta = idx - self.hash[h];
            if delta > DISTANCE_MAX {
                delta = DISTANCE_MAX;
            }
            self.chain[idx as u16 as usize] = delta as u16;
            self.hash[h] = idx;
            idx += 1;
        }
        *next_to_update = target_idx;
    }

    /// Find the longest match at `ip_pos`, returning `(length, offset)` with `length <= min_len`
    /// meaning "no better match". `matchlimit = n - LAST_LITERALS`.
    fn find_match(
        &mut self,
        w: &[u8],
        next_to_update: &mut u32,
        ip_pos: usize,
        min_len: i32,
        matchlimit: usize,
        favor_dec_speed: bool,
    ) -> (i32, i32) {
        let ip_index = ip_pos as u32 + BASE;
        let lowest = if BASE + DISTANCE_MAX + 1 > ip_index {
            BASE
        } else {
            ip_index - DISTANCE_MAX
        };
        let pattern = read32(w, ip_pos);
        let mut longest = min_len;
        let mut offset = 0i32;
        let mut match_chain_pos: u32 = 0;
        let max_possible = (matchlimit - ip_pos) as i32;

        self.insert(w, next_to_update, ip_index);
        let mut match_index = self.hash[hash4(w, ip_pos)];
        let mut nb = NB_SEARCHES;

        while match_index >= lowest && nb > 0 {
            nb -= 1;
            let mut match_length = 0i32;
            let m_pos = (match_index - BASE) as usize;

            // When favouring decompression speed, skip matches with offset < 8: such matches force
            // the decoder's slow overlapping-copy path, so trading them away speeds decompression.
            // 2-byte filter at `longest-1`, then confirm 4-byte pattern.
            if !(favor_dec_speed && ip_index - match_index < 8)
                && read16(w, ip_pos + longest as usize - 1)
                    == read16(w, m_pos + longest as usize - 1)
                && read32(w, m_pos) == pattern
            {
                match_length = MIN_MATCH_SIZE + count(w, ip_pos + 4, m_pos + 4, matchlimit) as i32;
                if match_length > longest {
                    longest = match_length;
                    offset = (ip_index - match_index) as i32;
                    if longest >= max_possible {
                        break; // maximal match — already optimal (handles runs)
                    }
                }
            }

            // chain-swap: when a candidate ties the current best, jump along the match to the chain
            // link with the largest stride, skipping redundant inside-match candidates.
            if match_length == longest && match_index + longest as u32 <= ip_index {
                const K_TRIGGER: i32 = 4;
                let mut dist_to_next_match: u32 = 1;
                let end = longest - MIN_MATCH_SIZE + 1;
                let mut accel = 1i32 << K_TRIGGER;
                let mut pos = 0i32;
                while pos < end {
                    let candidate_dist =
                        self.chain[(match_index + pos as u32) as u16 as usize] as u32;
                    let step = accel >> K_TRIGGER;
                    accel += 1;
                    if candidate_dist > dist_to_next_match {
                        dist_to_next_match = candidate_dist;
                        match_chain_pos = pos as u32;
                        accel = 1 << K_TRIGGER;
                    }
                    pos += step;
                }
                if dist_to_next_match > 1 {
                    if dist_to_next_match > match_index {
                        break; // avoid overflow
                    }
                    match_index -= dist_to_next_match;
                    continue;
                }
            }

            match_index -= self.chain[(match_index + match_chain_pos) as u16 as usize] as u32;
        }

        (longest, offset)
    }

    /// Returns `(0,0)` unless there's a match strictly longer than `min_len`.
    #[inline]
    fn find_longer_match(
        &mut self,
        w: &[u8],
        next_to_update: &mut u32,
        ip_pos: usize,
        min_len: i32,
        matchlimit: usize,
        favor_dec_speed: bool,
    ) -> (i32, i32) {
        let (len, off) = self.find_match(
            w,
            next_to_update,
            ip_pos,
            min_len,
            matchlimit,
            favor_dec_speed,
        );
        if len <= min_len {
            return (0, 0);
        }
        // When favouring decompression speed, a match just over the fast-path threshold is shortened
        // back to 18 so the decoder stays on its branchless fast copy.
        if favor_dec_speed && len > 18 && len <= 36 {
            return (18, off);
        }
        (len, off)
    }

    /// Optimally compress `w[prefix_len..]` (using `w[..prefix_len]` as lookback) into `out`,
    /// emitting a standard LZ4 block. `favor_dec_speed` trades a little ratio for a decode-optimised
    /// stream. Returns the number of bytes written.
    pub(crate) fn compress(
        &mut self,
        w: &[u8],
        prefix_len: usize,
        favor_dec_speed: bool,
        out: &mut impl Sink,
    ) -> Result<usize, CompressError> {
        let n = w.len();
        let start_op = out.pos();
        self.reset();
        let mut next_to_update = BASE;

        let mut ip = prefix_len;
        let mut anchor = prefix_len; // start of the pending literal run

        if n > MFLIMIT {
            let mflimit = n - MFLIMIT; // last position a match may start at (inclusive: ip <= mflimit)
            let matchlimit = n - LAST_LITERALS;

            while ip <= mflimit {
                let llen = (ip - anchor) as i32;
                let (first_len, first_off) = self.find_longer_match(
                    w,
                    &mut next_to_update,
                    ip,
                    MIN_MATCH_SIZE - 1,
                    matchlimit,
                    favor_dec_speed,
                );
                if first_len == 0 {
                    ip += 1;
                    continue;
                }

                if first_len > SUFFICIENT_LEN {
                    self.encode_sequence(out, w, &mut ip, &mut anchor, first_len, first_off)?;
                    continue;
                }

                let mut last_match_pos = first_len as usize;
                let best_mlen;
                let best_off;

                for r_pos in 0..MIN_MATCH_SIZE as usize {
                    let cost = literals_price(llen + r_pos as i32);
                    self.opt[r_pos] = Opt {
                        mlen: 1,
                        off: 0,
                        litlen: llen + r_pos as i32,
                        price: cost,
                    };
                }
                for mlen in MIN_MATCH_SIZE as usize..=first_len as usize {
                    let cost = sequence_price(llen, mlen as i32);
                    self.opt[mlen] = Opt {
                        mlen: mlen as i32,
                        off: first_off,
                        litlen: llen,
                        price: cost,
                    };
                }
                for add_lit in 1..=TRAILING_LITERALS {
                    self.opt[last_match_pos + add_lit] = Opt {
                        mlen: 1,
                        off: 0,
                        litlen: add_lit as i32,
                        price: self.opt[last_match_pos].price + literals_price(add_lit as i32),
                    };
                }

                let mut cur = 1usize;
                let mut goto_encode = false;
                let mut imm_best_mlen = 0i32;
                let mut imm_best_off = 0i32;
                let mut imm_last_match_pos = 0usize;

                while cur < last_match_pos {
                    let cur_ptr = ip + cur;
                    if cur_ptr > mflimit {
                        break;
                    }
                    // skip test: this position can't improve on the path so far
                    if self.opt[cur + 1].price <= self.opt[cur].price
                        && self.opt[cur + MIN_MATCH_SIZE as usize].price < self.opt[cur].price + 3
                    {
                        cur += 1;
                        continue;
                    }

                    let (new_len, new_off) = self.find_longer_match(
                        w,
                        &mut next_to_update,
                        cur_ptr,
                        MIN_MATCH_SIZE - 1,
                        matchlimit,
                        favor_dec_speed,
                    );
                    if new_len == 0 {
                        cur += 1;
                        continue;
                    }

                    if new_len > SUFFICIENT_LEN || new_len as usize + cur >= LZ4_OPT_NUM {
                        imm_best_mlen = new_len;
                        imm_best_off = new_off;
                        imm_last_match_pos = cur + 1;
                        goto_encode = true;
                        break;
                    }

                    let base_litlen = self.opt[cur].litlen;
                    for litlen in 1..MIN_MATCH_SIZE as usize {
                        let price = self.opt[cur].price - literals_price(base_litlen)
                            + literals_price(base_litlen + litlen as i32);
                        let pos = cur + litlen;
                        if price < self.opt[pos].price {
                            self.opt[pos] = Opt {
                                mlen: 1,
                                off: 0,
                                litlen: base_litlen + litlen as i32,
                                price,
                            };
                        }
                    }

                    let match_ml = new_len;
                    for ml in MIN_MATCH_SIZE..=match_ml {
                        let pos = cur + ml as usize;
                        let (ll, price);
                        if self.opt[cur].mlen == 1 {
                            ll = self.opt[cur].litlen;
                            let base = if cur > ll as usize {
                                self.opt[cur - ll as usize].price
                            } else {
                                0
                            };
                            price = base + sequence_price(ll, ml);
                        } else {
                            ll = 0;
                            price = self.opt[cur].price + sequence_price(0, ml);
                        }
                        let favor_bias = favor_dec_speed as i32;
                        if pos > last_match_pos + TRAILING_LITERALS
                            || price <= self.opt[pos].price - favor_bias
                        {
                            if ml == match_ml && last_match_pos < pos {
                                last_match_pos = pos;
                            }
                            self.opt[pos] = Opt {
                                mlen: ml,
                                off: new_off,
                                litlen: ll,
                                price,
                            };
                        }
                    }
                    for add_lit in 1..=TRAILING_LITERALS {
                        self.opt[last_match_pos + add_lit] = Opt {
                            mlen: 1,
                            off: 0,
                            litlen: add_lit as i32,
                            price: self.opt[last_match_pos].price + literals_price(add_lit as i32),
                        };
                    }
                    cur += 1;
                }

                // `cur_back` is where the reverse traversal starts. For an immediate encode it is the
                // loop position `cur` (the match is recorded there); otherwise it is the start of the
                // final match, `last_match_pos - best_mlen`.
                let cur_back;
                if goto_encode {
                    best_mlen = imm_best_mlen;
                    best_off = imm_best_off;
                    last_match_pos = imm_last_match_pos;
                    cur_back = cur as i32;
                } else {
                    best_mlen = self.opt[last_match_pos].mlen;
                    best_off = self.opt[last_match_pos].off;
                    cur_back = last_match_pos as i32 - best_mlen;
                }

                // reverse traversal to recover the shortest path
                {
                    let mut candidate_pos = cur_back;
                    let mut selected_matchlength = best_mlen;
                    let mut selected_offset = best_off;
                    loop {
                        let next_matchlength = self.opt[candidate_pos as usize].mlen;
                        let next_offset = self.opt[candidate_pos as usize].off;
                        self.opt[candidate_pos as usize].mlen = selected_matchlength;
                        self.opt[candidate_pos as usize].off = selected_offset;
                        selected_matchlength = next_matchlength;
                        selected_offset = next_offset;
                        if next_matchlength > candidate_pos {
                            break;
                        }
                        candidate_pos -= next_matchlength;
                    }
                }

                let mut r_pos = 0usize;
                while r_pos < last_match_pos {
                    let ml = self.opt[r_pos].mlen;
                    let off = self.opt[r_pos].off;
                    if ml == 1 {
                        ip += 1;
                        r_pos += 1;
                        continue;
                    }
                    r_pos += ml as usize;
                    self.encode_sequence(out, w, &mut ip, &mut anchor, ml, off)?;
                }
            }
        }

        self.encode_last_literals(out, w, anchor, n)?;
        Ok(out.pos() - start_op)
    }

    /// Emit one (literals, match) sequence and advance `ip`/`anchor`.
    #[inline]
    fn encode_sequence(
        &self,
        out: &mut impl Sink,
        w: &[u8],
        ip: &mut usize,
        anchor: &mut usize,
        match_length: i32,
        offset: i32,
    ) -> Result<(), CompressError> {
        let lit_len = *ip - *anchor;
        let ml_code = (match_length - MIN_MATCH_SIZE) as usize;

        let bytes = 1
            + super::varlen_extra_bytes(lit_len)
            + lit_len
            + 2
            + super::varlen_extra_bytes(ml_code);
        if out.pos() + bytes > out.capacity() {
            return Err(CompressError::OutputTooSmall);
        }

        push_byte(out, token_from_literal_and_match_length(lit_len, ml_code));
        if lit_len >= 15 {
            write_integer(out, lit_len - 15);
        }
        out.extend_from_slice(&w[*anchor..*anchor + lit_len]);
        push_u16(out, offset as u16);
        if ml_code >= 15 {
            write_integer(out, ml_code - 15);
        }

        *ip += match_length as usize;
        *anchor = *ip;
        Ok(())
    }

    #[inline]
    fn encode_last_literals(
        &self,
        out: &mut impl Sink,
        w: &[u8],
        anchor: usize,
        n: usize,
    ) -> Result<(), CompressError> {
        let lit_len = n - anchor;
        let bytes = 1 + super::varlen_extra_bytes(lit_len) + lit_len;
        if out.pos() + bytes > out.capacity() {
            return Err(CompressError::OutputTooSmall);
        }
        push_byte(out, token_from_literal_and_match_length(lit_len, 0));
        if lit_len >= 15 {
            write_integer(out, lit_len - 15);
        }
        out.extend_from_slice(&w[anchor..n]);
        Ok(())
    }
}
