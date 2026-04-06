use super::*;
use crate::block::decompress;
use crate::sink::SliceSink;
use alloc::vec;
use alloc::vec::Vec;

#[test]
fn test_compress_hc_basic() {
    let input = b"Hello, this is a test string that should be compressed!";
    let mut output = vec![0u8; input.len() * 2]; // Ensure enough space
    let mut sink = SliceSink::new(&mut output, 0);

    let result = compress_hc(input, &mut sink, 17);
    assert!(result.is_ok());

    let compressed_size = result.unwrap();
    assert!(compressed_size > 0);
    let result = decompress(&output[..compressed_size], input.len());
    assert!(result.is_ok());
    assert_eq!(&input[..], &result.unwrap()[..])
}

#[test]
fn test_compress_hc_small_input() {
    let input = b"Hi"; // Too small to compress
    let mut output = vec![0u8; 100];
    let mut sink = SliceSink::new(&mut output, 0);

    let result = compress_hc(input, &mut sink, 17);
    assert!(result.is_ok());

    let compressed_size = result.unwrap();
    assert!(compressed_size > 0);
    let result = decompress(&output[..compressed_size], input.len());
    assert!(result.is_ok());
    assert_eq!(&input[..], &result.unwrap()[..])
}

#[test]
fn test_compress_hc_repeated_pattern() {
    let input = b"AAAAAAAAAAABBBBBAAABBBBBBBAAAAAAA"; // Highly compressible
    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);

    let result = compress_hc(input, &mut sink, 17);
    assert!(result.is_ok());

    let compressed_size = result.unwrap();
    assert!(compressed_size > 0);
    let result = decompress(&output[..compressed_size], input.len() * 8);
    assert!(result.is_ok());
    assert_eq!(&input[..], &result.unwrap()[..])
}

#[test]
fn test_compress_hc_level_10() {
    // Level 10 uses optimal parsing
    let input = b"Hello, this is a test string that should be compressed!";
    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);

    let result = compress_hc(input, &mut sink, 10);
    assert!(result.is_ok());

    let compressed_size = result.unwrap();
    assert!(compressed_size > 0);
    let result = decompress(&output[..compressed_size], input.len());
    assert!(result.is_ok());
    assert_eq!(&input[..], &result.unwrap()[..])
}

#[test]
fn test_compress_hc_level_10_small_input() {
    let input = b"Hi"; // Too small to compress
    let mut output = vec![0u8; 100];
    let mut sink = SliceSink::new(&mut output, 0);

    let result = compress_hc(input, &mut sink, 10);
    assert!(result.is_ok());

    let compressed_size = result.unwrap();
    assert!(compressed_size > 0);
    let result = decompress(&output[..compressed_size], input.len());
    assert!(result.is_ok());
    assert_eq!(&input[..], &result.unwrap()[..])
}

#[test]
fn test_compress_hc_level_10_repeated_pattern() {
    let input = b"AAAAAAAAAAABBBBBAAABBBBBBBAAAAAAA"; // Highly compressible
    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);

    let result = compress_hc(input, &mut sink, 10);
    assert!(result.is_ok());

    let compressed_size = result.unwrap();
    assert!(compressed_size > 0);
    let result = decompress(&output[..compressed_size], input.len() * 8);
    assert!(result.is_ok());
    assert_eq!(&input[..], &result.unwrap()[..])
}

#[test]
fn test_compress_hc_level_11() {
    let input = b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.";
    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);

    let result = compress_hc(input, &mut sink, 11);
    assert!(result.is_ok());

    let compressed_size = result.unwrap();
    assert!(compressed_size > 0);
    let result = decompress(&output[..compressed_size], input.len());
    assert!(result.is_ok());
    assert_eq!(&input[..], &result.unwrap()[..])
}

#[test]
fn test_compress_hc_level_12() {
    let input = b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.";
    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);

    let result = compress_hc(input, &mut sink, 12);
    assert!(result.is_ok());

    let compressed_size = result.unwrap();
    assert!(compressed_size > 0);
    let result = decompress(&output[..compressed_size], input.len());
    assert!(result.is_ok());
    assert_eq!(&input[..], &result.unwrap()[..])
}

#[test]
fn test_compress_hc_level_12_better_than_level_9() {
    // Level 12 (optimal) should produce same or smaller output than level 9 (HC)
    let input: Vec<u8> = (0..1000)
        .map(|i| {
            let patterns = [b"ABCD", b"EFGH", b"IJKL", b"MNOP"];
            patterns[(i / 50) % 4][i % 4]
        })
        .collect();

    let mut output_hc = vec![0u8; input.len() * 2];
    let mut sink_hc = SliceSink::new(&mut output_hc, 0);
    let hc_size = compress_hc(&input, &mut sink_hc, 9).unwrap();

    let mut output_opt = vec![0u8; input.len() * 2];
    let mut sink_opt = SliceSink::new(&mut output_opt, 0);
    let opt_size = compress_hc(&input, &mut sink_opt, 12).unwrap();

    // Optimal should produce same or smaller output
    assert!(
        opt_size <= hc_size,
        "Level 12 ({}) should be <= Level 9 ({})",
        opt_size,
        hc_size
    );

    // Both should decompress correctly
    let result_hc = decompress(&output_hc[..hc_size], input.len());
    assert!(result_hc.is_ok());
    assert_eq!(&input[..], &result_hc.unwrap()[..]);

    let result_opt = decompress(&output_opt[..opt_size], input.len());
    assert!(result_opt.is_ok());
    assert_eq!(&input[..], &result_opt.unwrap()[..]);
}

#[test]
fn test_compress_hc_level_10_large_input() {
    // Test with a larger input to exercise the optimal algorithm
    let input: Vec<u8> = (0..10000).map(|i| ((i * 7 + 13) % 256) as u8).collect();

    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);

    let result = compress_hc(&input, &mut sink, 10);
    assert!(result.is_ok());

    let compressed_size = result.unwrap();
    assert!(compressed_size > 0);
    let result = decompress(&output[..compressed_size], input.len());
    assert!(result.is_ok());
    assert_eq!(&input[..], &result.unwrap()[..])
}

#[test]
fn test_compress_hc_level_10_all_same() {
    // Test with all same bytes - highly compressible
    let input = vec![0x42u8; 5000];
    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);

    let result = compress_hc(&input, &mut sink, 10);
    assert!(result.is_ok());

    let compressed_size = result.unwrap();
    assert!(compressed_size > 0);
    // Should compress very well
    assert!(
        compressed_size < input.len() / 10,
        "Should compress very well"
    );

    let result = decompress(&output[..compressed_size], input.len());
    assert!(result.is_ok());
    assert_eq!(&input[..], &result.unwrap()[..])
}

/// Exact compressed sizes for larger structured inputs. These inputs separate
/// the different HC strategies much better than tiny toy strings, so they are
/// useful as a regression net for compression ratio changes.
#[test]
fn test_compressed_sizes_exact() {
    fn get_size(input: &[u8], level: u8) -> usize {
        let mut output = vec![0u8; input.len() * 2 + 100];
        let mut sink = SliceSink::new(&mut output, 0);
        compress_hc(input, &mut sink, level).unwrap()
    }

    let html_like: Vec<u8> = (0..10_000)
        .flat_map(|i| match i % 7 {
            0 => b"<div class=\"item\">".to_vec(),
            1 => format!("content {i} here ").into_bytes(),
            2 => b"</div>\n".to_vec(),
            3 => b"<span style=\"color:red\">".to_vec(),
            4 => format!("value={i} ").into_bytes(),
            5 => b"</span>".to_vec(),
            _ => b"<br/>\n".to_vec(),
        })
        .collect();

    let json_like: Vec<u8> = (0..2_000)
        .flat_map(|i| {
            format!(
                "{{\"id\":{i},\"name\":\"user_{}\",\"score\":{},\"active\":true}},\n",
                i % 100,
                i * 7 % 1000,
            )
            .into_bytes()
        })
        .collect();

    let code_like: Vec<u8> = (0..3_000)
        .flat_map(|i| match i % 5 {
            0 => format!("    let value_{} = compute(input[{}]);\n", i % 50, i).into_bytes(),
            1 => b"    if value > threshold {\n".to_vec(),
            2 => format!("        result += value_{} * weight;\n", i % 50).into_bytes(),
            3 => b"    }\n".to_vec(),
            _ => format!("    // step {i}\n").into_bytes(),
        })
        .collect();

    // (level, html_like, json_like, code_like)
    let expected: &[(u8, usize, usize, usize)] = &[
        (1, 16_350, 16_183, 6_246),
        (4, 15_620, 16_198, 6_071),
        (9, 15_509, 15_698, 5_985),
        (10, 15_153, 15_102, 5_990),
        (12, 15_100, 15_083, 5_979),
    ];

    for &(level, expected_html_like, expected_json_like, expected_code_like) in expected {
        assert_eq!(
            get_size(&html_like, level),
            expected_html_like,
            "html_like @ level {level}"
        );
        assert_eq!(
            get_size(&json_like, level),
            expected_json_like,
            "json_like @ level {level}"
        );
        assert_eq!(
            get_size(&code_like, level),
            expected_code_like,
            "code_like @ level {level}"
        );
    }
}

#[test]
fn test_compress_hc_level_clamping() {
    // Test that levels are clamped correctly
    let input = b"The quick brown fox jumps over the lazy dog. The quick brown fox.";

    // Level 0 should be clamped to 1
    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);
    let result = compress_hc(input, &mut sink, 0);
    assert!(result.is_ok());
    let size_level_0 = result.unwrap();
    let decompressed = decompress(&output[..size_level_0], input.len()).unwrap();
    assert_eq!(&input[..], &decompressed[..]);

    // Level 20 should be clamped to 12
    let mut output = vec![0u8; input.len() * 2];
    let mut sink = SliceSink::new(&mut output, 0);
    let result = compress_hc(input, &mut sink, 20);
    assert!(result.is_ok());
    let size_level_20 = result.unwrap();
    let decompressed = decompress(&output[..size_level_20], input.len()).unwrap();
    assert_eq!(&input[..], &decompressed[..]);
}
