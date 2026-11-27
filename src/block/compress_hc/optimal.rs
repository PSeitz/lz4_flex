use crate::block::{
    encode_sequence, handle_last_literals, CompressError, LAST_LITERALS, LZ4_MIN_LENGTH, MFLIMIT,
    MINMATCH,
};
use crate::sink::Sink;
use alloc::vec;

use super::hash_chain::{find_longer_hash_chain_match, HashTableHCU32};
use super::{HcCompressionStrategy, HcLevelParams, LZ4_OPT_NUM, RUN_MASK, TRAILING_LITERALS};

/// Optimal parsing state for a single position.
/// Matches C's LZ4HC_optimal_t layout (4x i32 = 16 bytes).
/// Using i32 for match offset/length instead of u16 avoids costly widening conversions
/// on every access in the hot optimal-parsing loop (15-20% regression with u16).
/// The 4099-entry optimal-parsing buffer is ~64KB.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct OptimalState {
    /// Best known encoded size (byte-cost model) to reach this position.
    path_cost: i32,
    /// Length of the literal run immediately before this state (optimal-parse bookkeeping).
    lit_len: i32,
    /// Copy offset for the sequence ending here; `0` means literal step.
    match_offset: i32,
    /// Match length for this step; `1` means a single literal byte.
    match_len: i32,
}

impl OptimalState {
    const SENTINEL: Self = Self {
        path_cost: i32::MAX,
        lit_len: 0,
        match_offset: 0,
        match_len: 0,
    };
}

/// Calculate the cost in bytes of encoding literals
#[inline]
fn literals_price(lit_len: i32) -> i32 {
    let mut price = lit_len;
    if lit_len >= RUN_MASK as i32 {
        price += 1 + (lit_len - RUN_MASK as i32) / 255;
    }
    price
}

/// Calculate the cost in bytes of encoding a sequence (literals + match)
#[inline]
fn sequence_price(lit_len: i32, match_len: i32) -> i32 {
    // token + 16-bit offset
    let mut price: i32 = 1 + 2;

    // literal length encoding
    price += literals_price(lit_len);

    // match length encoding (match_len >= MINMATCH)
    let match_length_code = match_len - MINMATCH as i32;
    if match_length_code >= 15 {
        price += 1 + (match_length_code - 15) / 255;
    }

    price
}

/// Reverse the optimal parse path: walk backward from `start_state_index`, swapping each
/// state's `(match_len, match_offset)` with the values from the next step forward.
/// After this, `optimal_states[0..last_match_state_index)` can be read forward to emit sequences.
#[inline]
fn reverse_optimal_parse_path(
    optimal_states: &mut [OptimalState],
    start_state_index: usize,
    mut match_length: i32,
    mut match_offset: i32,
) {
    let mut optimal_state_index = start_state_index;
    loop {
        let next_match_length = optimal_states[optimal_state_index].match_len;
        let next_match_offset = optimal_states[optimal_state_index].match_offset;
        optimal_states[optimal_state_index].match_len = match_length;
        optimal_states[optimal_state_index].match_offset = match_offset;
        match_length = next_match_length;
        match_offset = next_match_offset;
        if (next_match_length as usize) > optimal_state_index {
            break;
        }
        optimal_state_index -= next_match_length as usize;
    }
}

/// Emit LZ4 sequences from optimal states `optimal_states[0..last_match_state_index)`.
/// `match_len == 1` means one literal step.
#[inline]
fn encode_optimal_parse_path(
    optimal_states: &[OptimalState],
    last_match_state_index: usize,
    input: &[u8],
    literal_start: &mut usize,
    cur: &mut usize,
    output: &mut impl Sink,
) {
    let mut optimal_state_index = 0usize;
    while optimal_state_index < last_match_state_index {
        let match_length = optimal_states[optimal_state_index].match_len as usize;
        let match_offset = optimal_states[optimal_state_index].match_offset as u16;

        if match_length == 1 {
            *cur += 1;
            optimal_state_index += 1;
            continue;
        }

        encode_sequence(
            &input[*literal_start..*cur],
            output,
            match_offset,
            match_length - MINMATCH,
        );

        *cur += match_length;
        *literal_start = *cur;
        optimal_state_index += match_length;
    }
}

#[inline]
fn initialize_literal_states(optimal_states: &mut [OptimalState], literal_length: i32) {
    for literal_step in 0..MINMATCH as i32 {
        let state = &mut optimal_states[literal_step as usize];
        state.match_len = 1;
        state.match_offset = 0;
        state.lit_len = literal_length + literal_step;
        state.path_cost = literals_price(literal_length + literal_step);
    }
}

#[inline]
fn initialize_first_match_states(
    optimal_states: &mut [OptimalState],
    literal_length: i32,
    first_match_length: usize,
    first_match_offset: u16,
) -> usize {
    debug_assert!(first_match_length < LZ4_OPT_NUM);
    for match_length in MINMATCH..=first_match_length {
        let state = &mut optimal_states[match_length];
        state.match_len = match_length as i32;
        state.match_offset = first_match_offset as i32;
        state.lit_len = literal_length;
        state.path_cost = sequence_price(literal_length, match_length as i32);
    }
    first_match_length
}

#[inline]
fn fill_trailing_literal_states(
    optimal_states: &mut [OptimalState],
    last_match_state_index: usize,
) {
    let base_path_cost = optimal_states[last_match_state_index].path_cost;
    for trailing_literal_length in 1..=TRAILING_LITERALS as i32 {
        let state_index = last_match_state_index + trailing_literal_length as usize;
        if state_index >= optimal_states.len() {
            break;
        }
        let state = &mut optimal_states[state_index];
        state.match_len = 1;
        state.match_offset = 0;
        state.lit_len = trailing_literal_length;
        state.path_cost = base_path_cost + literals_price(trailing_literal_length);
    }
}

#[inline]
fn should_skip_optimal_search(
    optimal_states: &[OptimalState],
    scan_offset: usize,
    full_optimal_update: bool,
) -> bool {
    if full_optimal_update {
        optimal_states[scan_offset + 1].path_cost <= optimal_states[scan_offset].path_cost
            && optimal_states[scan_offset + MINMATCH].path_cost
                < optimal_states[scan_offset].path_cost + 3
    } else {
        optimal_states[scan_offset + 1].path_cost <= optimal_states[scan_offset].path_cost
    }
}

#[inline]
fn minimum_match_length_to_improve(
    full_optimal_update: bool,
    last_match_state_index: usize,
    scan_offset: usize,
) -> usize {
    if full_optimal_update {
        MINMATCH - 1
    } else {
        last_match_state_index - scan_offset
    }
}

#[inline]
fn should_encode_optimal_match_early(
    match_length: usize,
    scan_offset: usize,
    sufficient_match_len: usize,
) -> bool {
    match_length >= sufficient_match_len || match_length + scan_offset >= LZ4_OPT_NUM
}

#[inline]
fn update_literal_states_before_match(optimal_states: &mut [OptimalState], scan_offset: usize) {
    let base_literal_length = optimal_states[scan_offset].lit_len;
    let base_path_cost = optimal_states[scan_offset].path_cost;
    let base_literal_cost = literals_price(base_literal_length);

    for literal_step in 1..MINMATCH as i32 {
        let state_index = scan_offset + literal_step as usize;
        let path_cost =
            base_path_cost - base_literal_cost + literals_price(base_literal_length + literal_step);
        if path_cost < optimal_states[state_index].path_cost {
            let state = &mut optimal_states[state_index];
            state.match_len = 1;
            state.match_offset = 0;
            state.lit_len = base_literal_length + literal_step;
            state.path_cost = path_cost;
        }
    }
}

#[inline]
fn match_price_from_state(
    optimal_states: &[OptimalState],
    scan_offset: usize,
    match_length: i32,
) -> (i32, i32) {
    if optimal_states[scan_offset].match_len == 1 {
        let literal_length = optimal_states[scan_offset].lit_len;
        let previous_sequence_cost = if scan_offset as i32 > literal_length {
            optimal_states[scan_offset - literal_length as usize].path_cost
        } else {
            0
        };
        (
            literal_length,
            previous_sequence_cost + sequence_price(literal_length, match_length),
        )
    } else {
        (
            0,
            optimal_states[scan_offset].path_cost + sequence_price(0, match_length),
        )
    }
}

#[inline]
fn update_match_states_from_position(
    optimal_states: &mut [OptimalState],
    scan_offset: usize,
    new_match_length: usize,
    new_match_offset: u16,
    last_match_state_index: &mut usize,
) {
    let capped_match_length = new_match_length.min(LZ4_OPT_NUM - scan_offset - 1);
    for match_length in MINMATCH..=capped_match_length {
        let state_index = scan_offset + match_length;
        let (literal_prefix_length, path_cost) =
            match_price_from_state(optimal_states, scan_offset, match_length as i32);

        if state_index > *last_match_state_index + TRAILING_LITERALS
            || path_cost <= optimal_states[state_index].path_cost
        {
            if match_length == capped_match_length && *last_match_state_index < state_index {
                *last_match_state_index = state_index;
            }
            let state = &mut optimal_states[state_index];
            state.match_len = match_length as i32;
            state.match_offset = new_match_offset as i32;
            state.lit_len = literal_prefix_length;
            state.path_cost = path_cost;
        }
    }
}

#[inline]
fn finish_optimal_parse_path(
    optimal_states: &mut [OptimalState],
    last_match_state_index: usize,
    input: &[u8],
    literal_start: &mut usize,
    cur: &mut usize,
    output: &mut impl Sink,
) {
    let match_length = optimal_states[last_match_state_index].match_len;
    let match_offset = optimal_states[last_match_state_index].match_offset;
    reverse_optimal_parse_path(
        optimal_states,
        last_match_state_index - match_length as usize,
        match_length,
        match_offset,
    );
    encode_optimal_parse_path(
        optimal_states,
        last_match_state_index,
        input,
        literal_start,
        cur,
        output,
    );
}

/// Internal optimal parsing compression implementation
pub(super) fn compress_opt_internal(
    input: &[u8],
    input_pos: usize,
    output: &mut impl Sink,
    level_params: HcLevelParams,
    hash_table: &mut HashTableHCU32,
    ext_dict: &[u8],
    stream_offset: usize,
) -> Result<usize, CompressError> {
    let output_start_pos = output.pos();

    if input.len() - input_pos < LZ4_MIN_LENGTH {
        handle_last_literals(output, &input[input_pos..]);
        return Ok(output.pos() - output_start_pos);
    }

    let input_end = input.len();
    // Inclusive max main-loop `cur`: at least `MFLIMIT` bytes remain from `cur` to `input_end`.
    let end_pos_check = input_end - MFLIMIT;
    // Do not extend matches into the last `LAST_LITERALS` bytes (they are literals).
    let match_limit = input_end - LAST_LITERALS;

    debug_assert_eq!(
        level_params.strategy,
        HcCompressionStrategy::Optimal,
        "compress_opt_internal is only for optimal (levels 10–12)"
    );
    let HcLevelParams {
        sufficient_match_len,
        full_optimal_update,
        ..
    } = level_params;

    let mut literal_start = input_pos;
    let mut cur = input_pos;
    let mut optimal_states = vec![OptimalState::SENTINEL; LZ4_OPT_NUM + TRAILING_LITERALS];
    let sufficient_match_len = sufficient_match_len.min(LZ4_OPT_NUM - 1);

    while cur <= end_pos_check {
        let literal_length = (cur - literal_start) as i32;

        let (first_match_length, first_match_offset) = find_longer_hash_chain_match(
            hash_table,
            input,
            cur,
            match_limit,
            MINMATCH - 1,
            ext_dict,
            stream_offset,
        );
        if first_match_length == 0 {
            cur += 1;
            continue;
        }
        let first_match_length = first_match_length as usize;

        if first_match_length >= sufficient_match_len {
            encode_sequence(
                &input[literal_start..cur],
                output,
                first_match_offset,
                first_match_length - MINMATCH,
            );
            cur += first_match_length;
            literal_start = cur;
            continue;
        }

        initialize_literal_states(&mut optimal_states, literal_length);
        let mut last_match_state_index = initialize_first_match_states(
            &mut optimal_states,
            literal_length,
            first_match_length,
            first_match_offset,
        );
        fill_trailing_literal_states(&mut optimal_states, last_match_state_index);

        let mut encoded_prefix_early = false;
        let mut scan_offset = 1usize;
        while scan_offset < last_match_state_index {
            let scan_pos = cur + scan_offset;
            if scan_pos > end_pos_check {
                break;
            }

            if should_skip_optimal_search(&optimal_states, scan_offset, full_optimal_update) {
                scan_offset += 1;
                continue;
            }

            let minimum_match_length = minimum_match_length_to_improve(
                full_optimal_update,
                last_match_state_index,
                scan_offset,
            );
            let (new_match_length, new_match_offset) = find_longer_hash_chain_match(
                hash_table,
                input,
                scan_pos,
                match_limit,
                minimum_match_length,
                ext_dict,
                stream_offset,
            );
            if new_match_length == 0 {
                scan_offset += 1;
                continue;
            }
            let new_match_length = new_match_length as usize;

            if should_encode_optimal_match_early(
                new_match_length,
                scan_offset,
                sufficient_match_len,
            ) {
                last_match_state_index = scan_offset + 1;
                reverse_optimal_parse_path(
                    &mut optimal_states,
                    scan_offset,
                    new_match_length as i32,
                    new_match_offset as i32,
                );
                encode_optimal_parse_path(
                    &optimal_states,
                    last_match_state_index,
                    input,
                    &mut literal_start,
                    &mut cur,
                    output,
                );
                encoded_prefix_early = true;
                break;
            }

            update_literal_states_before_match(&mut optimal_states, scan_offset);
            update_match_states_from_position(
                &mut optimal_states,
                scan_offset,
                new_match_length,
                new_match_offset,
                &mut last_match_state_index,
            );
            fill_trailing_literal_states(&mut optimal_states, last_match_state_index);
            scan_offset += 1;
        }

        if encoded_prefix_early {
            continue;
        }

        finish_optimal_parse_path(
            &mut optimal_states,
            last_match_state_index,
            input,
            &mut literal_start,
            &mut cur,
            output,
        );

        // No optimal-state reset needed (matches C behavior).
    }

    handle_last_literals(output, &input[literal_start..input_end]);
    Ok(output.pos() - output_start_pos)
}
