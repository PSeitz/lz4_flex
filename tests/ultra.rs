//! Tests for the optimal-parse `ultra` block compressor.
#![cfg(feature = "ultra")]

use lz4_flex::block::{
    compress as compress_fast, compress_into_with_mode, compress_prepend_size_with_mode,
    compress_with_mode, decompress, decompress_size_prepended, decompress_size_prepended_with_dict,
    decompress_with_dict, get_maximum_output_size, CompressionMode,
};

const COMPRESSION1K: &[u8] = include_bytes!("../benches/compression_1k.txt");
const COMPRESSION34K: &[u8] = include_bytes!("../benches/compression_34k.txt");
const COMPRESSION65K: &[u8] = include_bytes!("../benches/compression_65k.txt");
const COMPRESSION66K_JSON: &[u8] = include_bytes!("../benches/compression_66k_JSON.txt");
const COMPRESSION10MB: &[u8] = include_bytes!("../benches/dickens.txt");

const CORPUS: &[(&str, &[u8])] = &[
    ("1k", COMPRESSION1K),
    ("34k", COMPRESSION34K),
    ("65k", COMPRESSION65K),
    ("66k_json", COMPRESSION66K_JSON),
    ("10mb", COMPRESSION10MB),
];

/// `CompressionMode::Ultra` — the suffix-array / lz4ultra compressor (no dictionary).
fn ultra(data: &[u8]) -> Vec<u8> {
    compress_with_mode(data, b"", CompressionMode::Ultra)
}

/// `CompressionMode::Hc` — the lz4hc compressor (no dictionary).
fn hc(data: &[u8]) -> Vec<u8> {
    compress_with_mode(data, b"", CompressionMode::Hc)
}

/// Decode a raw block with the reference C lz4 implementation, to prove the bytes are spec-valid.
fn lz4_cpp_decompress(input: &[u8], decomp_len: usize) -> Vec<u8> {
    let mut out = vec![0u8; decomp_len];
    let n = lzzzz::lz4::decompress(input, &mut out).expect("C lz4 failed to decode ultra block");
    assert_eq!(n, decomp_len, "C lz4 produced wrong length");
    out
}

/// Round-trip `input` through a given mode and verify both decoders reproduce it exactly.
fn check_roundtrip_mode(name: &str, input: &[u8], mode: CompressionMode) {
    let compressed = compress_with_mode(input, b"", mode);

    // 1. lz4_flex's own decoder.
    let decompressed = decompress(&compressed, input.len())
        .unwrap_or_else(|e| panic!("[{name}/{mode:?}] lz4_flex decode failed: {e:?}"));
    assert_eq!(
        decompressed, input,
        "[{name}/{mode:?}] lz4_flex roundtrip mismatch"
    );

    // 2. The reference C lz4 decoder (interoperability / spec-conformance).
    if !input.is_empty() {
        let cpp = lz4_cpp_decompress(&compressed, input.len());
        assert_eq!(cpp, input, "[{name}/{mode:?}] C lz4 roundtrip mismatch");
    }
}

/// Round-trip through both optimal-parse engines (Ultra and Hc).
fn check_roundtrip(name: &str, input: &[u8]) {
    check_roundtrip_mode(name, input, CompressionMode::Ultra);
    check_roundtrip_mode(name, input, CompressionMode::Hc);
}

#[test]
fn roundtrip_corpus() {
    for (name, data) in CORPUS {
        check_roundtrip(name, data);
    }
}

#[test]
fn ratio_beats_or_matches_fast() {
    // Optimal parsing must not lose to the greedy/lazy parser on real data.
    for (name, data) in CORPUS {
        let ultra = ultra(data).len();
        let fast = compress_fast(data).len();
        assert!(
            ultra <= fast,
            "[{name}] ultra ({ultra}) should be <= fast ({fast})"
        );
        println!(
            "[{name}] raw={} fast={fast} ultra={ultra} (ultra {:.1}% of fast)",
            data.len(),
            ultra as f64 / fast as f64 * 100.0
        );
    }
}

#[test]
fn edge_cases() {
    let cases: Vec<Vec<u8>> = vec![
        vec![],
        vec![0],
        vec![42],
        b"a".to_vec(),
        b"ab".to_vec(),
        b"abc".to_vec(),
        b"abcd".to_vec(),
        b"hello".to_vec(),
        b"Hello people, what's up?".to_vec(), // < 13 bytes path exercised by shorter ones above
        vec![7u8; 1],
        vec![7u8; 12],
        vec![7u8; 13],
        vec![7u8; 1000],    // highly repetitive
        vec![0u8; 100_000], // long run -> long matches / varlen lengths
        b"aaaaaaaaaaaaaaaabbbbbbbbbbbbbbbbaaaaaaaaaaaaaaaa".to_vec(),
    ];
    for (i, c) in cases.iter().enumerate() {
        check_roundtrip(&format!("edge{i}"), c);
    }
}

#[test]
fn incompressible_random() {
    // Deterministic pseudo-random (xorshift) so the test is reproducible.
    let mut state: u64 = 0x1234_5678_9abc_def0;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for &len in &[1usize, 100, 1000, 50_000] {
        let data: Vec<u8> = (0..len).map(|_| (next() & 0xff) as u8).collect();
        check_roundtrip(&format!("rand{len}"), &data);
    }
}

#[test]
fn prepend_size_roundtrip() {
    for (name, data) in CORPUS {
        for mode in [CompressionMode::Ultra, CompressionMode::Hc] {
            let compressed = compress_prepend_size_with_mode(data, b"", mode);
            let out = decompress_size_prepended(&compressed).unwrap();
            assert_eq!(
                &out, data,
                "[{name}/{mode:?}] prepend-size roundtrip mismatch"
            );
        }
    }
}

#[test]
fn compress_into_roundtrip() {
    for (name, data) in CORPUS {
        let mut buf = vec![0u8; get_maximum_output_size(data.len())];

        // Both engines, no dictionary.
        for mode in [CompressionMode::Ultra, CompressionMode::Hc] {
            let n = compress_into_with_mode(data, &mut buf, b"", mode).unwrap();
            assert_eq!(
                decompress(&buf[..n], data.len()).unwrap(),
                *data,
                "[{name}/{mode:?}] into"
            );
        }

        // With a dictionary (first half as dict, compress the second half).
        if data.len() >= 64 {
            let (dict, body) = data.split_at(data.len() / 2);
            let mut dbuf = vec![0u8; get_maximum_output_size(body.len())];
            let n = compress_into_with_mode(body, &mut dbuf, dict, CompressionMode::Ultra).unwrap();
            assert_eq!(
                decompress_with_dict(&dbuf[..n], body.len(), dict).unwrap(),
                body,
                "[{name}] into_with_dict"
            );
        }
    }
}

#[test]
fn compress_into_too_small_errors() {
    let data = COMPRESSION34K;
    let mut tiny = vec![0u8; 8];
    assert!(compress_into_with_mode(data, &mut tiny, b"", CompressionMode::Ultra).is_err());
    assert!(compress_into_with_mode(data, &mut tiny, b"", CompressionMode::Hc).is_err());
}

#[test]
fn with_dict_roundtrip() {
    // Use the first half as a dictionary, compress the second half against it.
    for (name, data) in CORPUS {
        if data.len() < 64 {
            continue;
        }
        let (dict, body) = data.split_at(data.len() / 2);

        for mode in [CompressionMode::Ultra, CompressionMode::Hc] {
            let compressed = compress_with_mode(body, dict, mode);
            let out = decompress_with_dict(&compressed, body.len(), dict).unwrap();
            assert_eq!(&out, body, "[{name}/{mode:?}] with-dict roundtrip mismatch");

            let compressed = compress_prepend_size_with_mode(body, dict, mode);
            let out = decompress_size_prepended_with_dict(&compressed, dict).unwrap();
            assert_eq!(
                &out, body,
                "[{name}/{mode:?}] with-dict prepend-size roundtrip mismatch"
            );
        }
    }
}

#[cfg(feature = "frame")]
mod frame_ultra {
    use super::*;
    use lz4_flex::frame::{BlockMode, FrameDecoder, FrameEncoder, FrameInfo};
    use std::io::{Read, Write};

    fn frame_compress_mode(data: &[u8], block_mode: BlockMode, mode: CompressionMode) -> Vec<u8> {
        let mut fi = FrameInfo::new();
        fi.block_mode = block_mode;
        fi.block_size = lz4_flex::frame::BlockSize::Max64KB; // small blocks -> exercise linking
        let mut enc = FrameEncoder::with_frame_info(fi, Vec::new());
        enc.set_compression_mode(mode);
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn flex_frame_decompress(compressed: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        FrameDecoder::new(compressed).read_to_end(&mut out).unwrap();
        out
    }

    fn cpp_frame_decompress(compressed: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        lzzzz::lz4f::decompress_to_vec(compressed, &mut out).unwrap();
        out
    }

    #[test]
    fn all_modes_roundtrip_independent_and_linked() {
        // Every mode must round-trip in both block modes, through both decoders.
        // Linked + Hc is the first exercise of the lz4hc with-prefix path.
        for (name, data) in CORPUS {
            for block_mode in [BlockMode::Independent, BlockMode::Linked] {
                for mode in [CompressionMode::Ultra, CompressionMode::Hc] {
                    let compressed = frame_compress_mode(data, block_mode, mode);
                    assert_eq!(
                        &flex_frame_decompress(&compressed),
                        data,
                        "[{name}/{block_mode:?}/{mode:?}] flex frame roundtrip mismatch"
                    );
                    assert_eq!(
                        &cpp_frame_decompress(&compressed),
                        data,
                        "[{name}/{block_mode:?}/{mode:?}] C lz4 frame roundtrip mismatch"
                    );
                }
            }
        }
    }

    #[test]
    fn frame_ultra_beats_fast() {
        for (name, data) in CORPUS {
            let ultra = frame_compress_mode(data, BlockMode::Linked, CompressionMode::Ultra).len();
            let fast = frame_compress_mode(data, BlockMode::Linked, CompressionMode::Fast).len();
            assert!(
                ultra <= fast,
                "[{name}] ultra frame ({ultra}) should be <= fast frame ({fast})"
            );
            println!("[{name}] fast_frame={fast} ultra_frame={ultra}");
        }
    }

    #[test]
    fn multi_write_small_chunks() {
        // Feed the encoder in small pieces to exercise block boundaries / linked rotation.
        let data = COMPRESSION65K;
        for mode in [CompressionMode::Ultra, CompressionMode::Hc] {
            let mut fi = FrameInfo::new();
            fi.block_mode = BlockMode::Linked;
            fi.block_size = lz4_flex::frame::BlockSize::Max64KB;
            let mut enc = FrameEncoder::with_frame_info(fi, Vec::new());
            enc.set_compression_mode(mode);
            for chunk in data.chunks(1000) {
                enc.write_all(chunk).unwrap();
            }
            let compressed = enc.finish().unwrap();
            assert_eq!(flex_frame_decompress(&compressed), data, "{mode:?}");
            assert_eq!(cpp_frame_decompress(&compressed), data, "{mode:?}");
        }
    }
}

mod fuzz {
    use super::*;
    use proptest::prelude::*;

    // Plain asserts (not prop_assert!) so this shared body can be reused across strategies;
    // proptest still catches the panic and shrinks the failing input. Correctness only: the
    // decode-speed-favouring Ultra path is not guaranteed to beat the greedy parser on small
    // adversarial inputs, so we don't assert a ratio bound here (see `ratio_beats_or_matches_fast`).
    fn roundtrip(data: &[u8]) {
        for mode in [CompressionMode::Ultra, CompressionMode::Hc] {
            let compressed = compress_with_mode(data, b"", mode);
            let out = decompress(&compressed, data.len()).unwrap();
            assert_eq!(out, data, "{mode:?} lz4_flex roundtrip mismatch");
            // Spec-conformance against the C decoder.
            if !data.is_empty() {
                let mut cpp = vec![0u8; data.len()];
                let n = lzzzz::lz4::decompress(&compressed, &mut cpp).unwrap();
                assert!(
                    n == data.len() && cpp == data,
                    "{mode:?} C lz4 roundtrip mismatch"
                );
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

        // Small alphabet -> lots of matches: stresses the match finder, DP and command-count join.
        #[test]
        #[cfg_attr(miri, ignore)]
        fn repetitive(data in proptest::collection::vec(0u8..6, 0..8000usize)) {
            roundtrip(&data);
        }

        // Full byte range, including incompressible stretches.
        #[test]
        #[cfg_attr(miri, ignore)]
        fn arbitrary(data in proptest::collection::vec(any::<u8>(), 0..4000usize)) {
            roundtrip(&data);
        }
    }
}

/// The lz4hc engine ([`CompressionMode::Hc`]) is **byte-identical** to the C `lz4hc -12` reference on
/// the corpus — the strongest possible validation that the lz4hc port is faithful. (It uses the
/// ratio-favouring price model, matching lz4hc-12's own default, unlike the decode-favouring Ultra.)
#[test]
fn hc_byte_identical_to_c_lz4hc() {
    for (name, data) in CORPUS {
        let ours = hc(data);
        let mut theirs = Vec::new();
        lzzzz::lz4_hc::compress_to_vec(data, &mut theirs, lzzzz::lz4_hc::CLEVEL_MAX).unwrap();
        assert_eq!(
            ours.len(),
            theirs.len(),
            "[{name}] Hc size differs from C lz4hc-12"
        );
        assert!(
            ours == theirs,
            "[{name}] Hc output differs from C lz4hc-12 (same size)"
        );
    }
}

/// The Ultra (suffix-array / lz4ultra) and Hc (lz4hc) engines agree on ratio within a small margin
/// (they can differ a little either way — the suffix array caps match length, lz4hc prices
/// differently, and Ultra favours decode speed). Both round-trip.
#[test]
fn ultra_and_hc_agree_closely() {
    for (name, data) in CORPUS {
        let def = ultra(data);
        let fast = hc(data);
        assert_eq!(
            decompress(&def, data.len()).unwrap(),
            *data,
            "[{name}] ultra roundtrip"
        );
        assert_eq!(
            decompress(&fast, data.len()).unwrap(),
            *data,
            "[{name}] hc roundtrip"
        );
        let tol = def.len() / 16 + 64;
        assert!(
            def.len() <= fast.len() + tol && fast.len() <= def.len() + tol,
            "[{name}] ultra {} and hc {} diverge too much",
            def.len(),
            fast.len()
        );
    }
}

mod fuzz_finders {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

        // Both engines must produce valid, correctly-decoding output on arbitrary repetitive data.
        #[test]
        #[cfg_attr(miri, ignore)]
        fn both_engines_roundtrip(data in proptest::collection::vec(0u8..8, 0..6000usize)) {
            let def = compress_with_mode(&data, b"", CompressionMode::Ultra);
            let fast = compress_with_mode(&data, b"", CompressionMode::Hc);
            prop_assert_eq!(decompress(&def, data.len()).unwrap(), data.clone());
            prop_assert_eq!(decompress(&fast, data.len()).unwrap(), data);
        }
    }
}
