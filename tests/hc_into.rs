//! `compress_hc_into`: the HC compressor into a caller's slice.
use lz4_flex::block::{
    compress_hc_into, compress_hc_to_vec, decompress, get_maximum_output_size, CompressError,
};

fn input() -> Vec<u8> {
    b"the quick brown fox jumps over the lazy dog. ".repeat(500)
}

#[test]
fn it_writes_what_the_vec_form_writes_at_every_level() {
    let input = input();
    for level in 0..=12 {
        let mut output = vec![0u8; get_maximum_output_size(input.len())];
        let written = compress_hc_into(&input, &mut output, level).unwrap();
        assert_eq!(output[..written], compress_hc_to_vec(&input, level)[..]);
        assert_eq!(decompress(&output[..written], input.len()).unwrap(), input);
    }
}

#[test]
fn an_output_just_big_enough_is_filled() {
    let input = input();
    let want = compress_hc_to_vec(&input, 12);
    let mut output = vec![0u8; want.len()];
    assert_eq!(
        compress_hc_into(&input, &mut output, 12).unwrap(),
        want.len()
    );
    assert_eq!(output, want);
}

#[test]
fn an_output_too_small_is_an_error_and_not_a_panic() {
    let input = input();
    let want = compress_hc_to_vec(&input, 12);
    let mut output = vec![0u8; want.len() - 1];
    assert!(matches!(
        compress_hc_into(&input, &mut output, 12),
        Err(CompressError::OutputTooSmall)
    ));
}

/// Short literals then a match, for the block compressors' literal copy, which
/// reads eight bytes at a time. Run under Miri without `safe-encode`.
#[test]
fn a_short_literal_run_before_a_match_stays_inside_its_slice() {
    let input = b"abcd0123456789abcdefgh0123456789abcdefgh-end-of-the-block-text".to_vec();
    assert_eq!(
        decompress(&lz4_flex::block::compress(&input), input.len()).unwrap(),
        input
    );
    for level in [1, 9, 12] {
        let block = compress_hc_to_vec(&input, level);
        assert_eq!(decompress(&block, input.len()).unwrap(), input);
    }
}
