//! Optimal parse and block emission.
//!
//! Three passes over the per-position longest matches produced by [`super::matchfinder`]:
//! 1. [`optimize_matches`] — a backward dynamic program choosing, for every position, whether to
//!    emit a literal or a match (and at which length) to minimise the encoded size.
//! 2. [`optimize_command_count`] — a forward pass that turns tiny matches back into literals and
//!    joins adjacent equal-offset matches when it's free, reducing the command count (decompression
//!    speed) without growing the output.
//! 3. [`write_block`] — emits a standard LZ4 block, ending on a literals-only token.

use crate::block::compress::{
    push_byte, push_u16, token_from_literal_and_match_length, write_integer,
};
use crate::block::CompressError;
use crate::sink::Sink;

use super::{
    Favor, Match, LAST_LITERALS, LEAVE_ALONE_MATCH_SIZE, LITERALS_RUN_LEN, MATCH_RUN_LEN,
    MIN_MATCH_SIZE, MODESWITCH_PENALTY,
};

/// Extra *bits* to encode a literal run / match-code of `length` in the cost model — the byte cost
/// (shared [`super::varlen_extra_bytes`]) scaled to bits. Lengths here are always `>= 0`.
#[inline]
fn varlen_bits(length: i32) -> i32 {
    (super::varlen_extra_bytes(length as usize) as i32) << 3
}

/// Backward optimal parse. Rewrites `matches[start..end]` in place so that `matches[i].length` is
/// either `0` (emit a literal at `i`) or the chosen match length (with `matches[i].offset` the
/// offset to use). `cost`/`score` are scratch arrays of length `end`.
pub(super) fn optimize_matches(
    start: usize,
    end: usize,
    matches: &mut [Match],
    cost: &mut [i32],
    score: &mut [i32],
    favor: Favor,
) {
    let extra_match_score: i32 = match favor {
        Favor::Ratio => 1,
        Favor::DecompressionSpeed => 5,
    };

    cost[end - 1] = 8;
    score[end - 1] = 0;
    let mut last_literals_offset = end;

    // i from end-2 down to start (inclusive). Use isize so start == 0 terminates cleanly.
    let mut ii = end as isize - 2;
    while ii >= start as isize {
        let i = ii as usize;

        let literals_len = (last_literals_offset - i) as i32;
        let mut best_cost = 8 + cost[i + 1];
        let mut best_score = 1 + score[i + 1];
        if literals_len >= LITERALS_RUN_LEN && (literals_len - LITERALS_RUN_LEN) % 255 == 0 {
            // The literal run just crossed a variable-length encoding boundary.
            best_cost += 8;
        }
        if matches[i + 1].length >= MIN_MATCH_SIZE {
            best_cost += MODESWITCH_PENALTY;
        }
        let mut best_match_len: i32 = 0;
        let mut best_match_offset: u32 = 0;

        let cur_match = matches[i];
        if cur_match.length >= MIN_MATCH_SIZE {
            let offset = cur_match.offset;
            // Cap the match so the last LAST_LITERALS bytes stay literals.
            let mut match_len = cur_match.length;
            if (i + match_len as usize) > (end - LAST_LITERALS) {
                match_len = (end - LAST_LITERALS - i) as i32;
            }

            if cur_match.length >= LEAVE_ALONE_MATCH_SIZE {
                let cur_cost = 8
                    + 16
                    + varlen_bits(match_len - MIN_MATCH_SIZE)
                    + cost[i + match_len as usize]
                    + if matches[i + match_len as usize].length >= MIN_MATCH_SIZE {
                        MODESWITCH_PENALTY
                    } else {
                        0
                    };
                let cur_score = extra_match_score + score[i + match_len as usize];
                if best_cost > cur_cost || (best_cost == cur_cost && best_score > cur_score) {
                    best_cost = cur_cost;
                    best_score = cur_score;
                    best_match_len = match_len;
                    best_match_offset = offset;
                }
            } else {
                if let Favor::DecompressionSpeed = favor {
                    // If the match is just above the fast-path threshold, shorten it back to the
                    // threshold, trading a little ratio for decompression speed.
                    if match_len > (MATCH_RUN_LEN + MIN_MATCH_SIZE - 1)
                        && match_len <= 2 * (MATCH_RUN_LEN + MIN_MATCH_SIZE - 1)
                    {
                        match_len = MATCH_RUN_LEN + MIN_MATCH_SIZE - 1;
                    }
                }

                let mut k = match_len;
                while k >= MATCH_RUN_LEN + MIN_MATCH_SIZE {
                    let cur_cost = 8
                        + 16
                        + varlen_bits(k - MIN_MATCH_SIZE)
                        + cost[i + k as usize]
                        + if matches[i + k as usize].length >= MIN_MATCH_SIZE {
                            MODESWITCH_PENALTY
                        } else {
                            0
                        };
                    let cur_score = extra_match_score + score[i + k as usize];
                    if best_cost > cur_cost || (best_cost == cur_cost && best_score > cur_score) {
                        best_cost = cur_cost;
                        best_score = cur_score;
                        best_match_len = k;
                        best_match_offset = offset;
                    }
                    k -= 1;
                }
                while k >= MIN_MATCH_SIZE {
                    let cur_cost = 8 + 16 // no extra match-length bytes below MATCH_RUN_LEN
                        + cost[i + k as usize]
                        + if matches[i + k as usize].length >= MIN_MATCH_SIZE {
                            MODESWITCH_PENALTY
                        } else {
                            0
                        };
                    let cur_score = extra_match_score + score[i + k as usize];
                    if best_cost > cur_cost || (best_cost == cur_cost && best_score > cur_score) {
                        best_cost = cur_cost;
                        best_score = cur_score;
                        best_match_len = k;
                        best_match_offset = offset;
                    }
                    k -= 1;
                }
            }
        }

        if best_match_len >= MIN_MATCH_SIZE {
            last_literals_offset = i;
        }
        cost[i] = best_cost;
        score[i] = best_score;
        matches[i].length = best_match_len;
        matches[i].offset = best_match_offset;

        ii -= 1;
    }
}

/// Minimise the number of commands without growing the output.
pub(super) fn optimize_command_count(
    window: &[u8],
    start: usize,
    end: usize,
    matches: &mut [Match],
) {
    let mut num_literals: i32 = 0;
    let mut i = start;
    while i < end {
        let match_len = matches[i].length;
        if match_len >= MIN_MATCH_SIZE {
            let mut reduce = false;

            if match_len <= 19 && (i + match_len as usize) < end {
                let encoded = match_len - MIN_MATCH_SIZE;
                let command_size = 8 + varlen_bits(num_literals) + 16 + varlen_bits(encoded);

                if matches[i + match_len as usize].length >= MIN_MATCH_SIZE {
                    // This match is followed by another match (no literals between). Re-encoding it
                    // as literals can't grow the output and removes a command.
                    if command_size >= (match_len << 3) + varlen_bits(num_literals + match_len) {
                        reduce = true;
                    }
                } else {
                    // This match is followed by some literals and then another match (or the end).
                    let mut cur_index = i + match_len as usize;
                    let mut next_num_literals: i32 = 0;
                    loop {
                        cur_index += 1;
                        next_num_literals += 1;
                        if !(cur_index < end && matches[cur_index].length < MIN_MATCH_SIZE) {
                            break;
                        }
                    }
                    if command_size
                        >= (match_len << 3)
                            + varlen_bits(num_literals + next_num_literals + match_len)
                            - varlen_bits(next_num_literals)
                    {
                        reduce = true;
                    }
                }
            }

            if reduce {
                for j in 0..match_len as usize {
                    matches[i + j].length = 0;
                }
                num_literals += match_len;
                i += match_len as usize;
            } else {
                // Try to join this match with the following one if they reproduce the same bytes.
                let next = matches[i + match_len as usize];
                let cur = matches[i];
                if (i + match_len as usize) < end
                    && cur.offset > 0
                    && match_len >= 2
                    && next.offset > 0
                    && next.length >= 2
                    && (match_len + next.length) >= LEAVE_ALONE_MATCH_SIZE
                    && (match_len + next.length) <= 65535
                    && (i + match_len as usize) >= cur.offset as usize
                    && (i + match_len as usize) >= next.offset as usize
                    && (i + match_len as usize + next.length as usize) <= end
                {
                    let base = i + match_len as usize;
                    let a = base - cur.offset as usize;
                    let b = base - next.offset as usize;
                    let len = next.length as usize;
                    if window[a..a + len] == window[b..b + len] {
                        matches[i].length += next.length;
                        matches[i + match_len as usize].offset = 0;
                        matches[i + match_len as usize].length = -1;
                        continue;
                    }
                }

                num_literals = 0;
                i += match_len as usize;
            }
        } else {
            num_literals += 1;
            i += 1;
        }
    }
}

/// Emit a standard LZ4 block for the optimally-parsed `matches[start..end]` into `out`.
/// Returns the number of bytes written.
pub(super) fn write_block(
    window: &[u8],
    start: usize,
    end: usize,
    matches: &[Match],
    out: &mut impl Sink,
) -> Result<usize, CompressError> {
    let start_pos = out.pos();
    let capacity = out.capacity();

    let mut num_literals: usize = 0;
    let mut first_literal_offset: usize = 0;

    let mut i = start;
    while i < end {
        let m = matches[i];
        if m.length >= MIN_MATCH_SIZE {
            let match_offset = m.offset as usize;
            let match_len = m.length as usize;
            let encoded = (m.length - MIN_MATCH_SIZE) as usize;
            debug_assert!((1..=super::MAX_OFFSET).contains(&match_offset));

            let command_bytes = 1
                + super::varlen_extra_bytes(num_literals)
                + num_literals
                + 2
                + super::varlen_extra_bytes(encoded);
            if out.pos() + command_bytes > capacity {
                return Err(CompressError::OutputTooSmall);
            }

            push_byte(
                out,
                token_from_literal_and_match_length(num_literals, encoded),
            );
            if num_literals >= 15 {
                write_integer(out, num_literals - 15);
            }
            if num_literals != 0 {
                out.extend_from_slice(
                    &window[first_literal_offset..first_literal_offset + num_literals],
                );
                num_literals = 0;
            }
            push_u16(out, match_offset as u16);
            if encoded >= 15 {
                write_integer(out, encoded - 15);
            }

            i += match_len;
        } else {
            if num_literals == 0 {
                first_literal_offset = i;
            }
            num_literals += 1;
            i += 1;
        }
    }

    // Final literals-only command (match nibble 0, no offset).
    let command_bytes = 1 + super::varlen_extra_bytes(num_literals) + num_literals;
    if out.pos() + command_bytes > capacity {
        return Err(CompressError::OutputTooSmall);
    }
    push_byte(out, token_from_literal_and_match_length(num_literals, 0));
    if num_literals >= 15 {
        write_integer(out, num_literals - 15);
    }
    if num_literals != 0 {
        out.extend_from_slice(&window[first_literal_offset..first_literal_offset + num_literals]);
    }

    Ok(out.pos() - start_pos)
}
