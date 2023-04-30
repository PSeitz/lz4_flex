//! Tests.

#[macro_use]
extern crate more_asserts;

use std::iter;

use lz4_compress::compress as lz4_rust_compress;
#[cfg(feature = "frame")]
use lz4_flex::frame::BlockMode;
use lz4_flex::{
    block::{compress_prepend_size, decompress_size_prepended},
    compress as compress_block, decompress,
};

const COMPRESSION1K: &[u8] = include_bytes!("../benches/compression_1k.txt");
const COMPRESSION34K: &[u8] = include_bytes!("../benches/compression_34k.txt");
const COMPRESSION65: &[u8] = include_bytes!("../benches/compression_65k.txt");
const COMPRESSION66JSON: &[u8] = include_bytes!("../benches/compression_66k_JSON.txt");
const COMPRESSION10MB: &[u8] = include_bytes!("../benches/dickens.txt");
const DECOMPRESSION10MB: &[u8] = include_bytes!("../benches/dickens.lz4");

fn lz4_cpp_block_compress(input: &[u8]) -> Result<Vec<u8>, lzzzz::Error> {
    let mut out = Vec::new();
    lzzzz::lz4::compress_to_vec(input, &mut out, lzzzz::lz4::ACC_LEVEL_DEFAULT).unwrap();
    Ok(out)
}

fn lz4_cpp_block_decompress(input: &[u8], decomp_len: usize) -> Result<Vec<u8>, lzzzz::Error> {
    let mut out = vec![0u8; decomp_len];
    lzzzz::lz4::decompress(input, &mut out)?;
    Ok(out)
}

#[cfg(feature = "frame")]
fn lz4_cpp_frame_compress(input: &[u8], independent: bool) -> Result<Vec<u8>, lzzzz::Error> {
    let pref = lzzzz::lz4f::PreferencesBuilder::new()
        .block_mode(if independent {
            lzzzz::lz4f::BlockMode::Independent
        } else {
            lzzzz::lz4f::BlockMode::Linked
        })
        .build();
    let mut out = Vec::new();
    lzzzz::lz4f::compress_to_vec(input, &mut out, &pref).unwrap();
    Ok(out)
}

#[cfg(feature = "frame")]
fn lz4_cpp_frame_decompress(input: &[u8]) -> Result<Vec<u8>, lzzzz::lz4f::Error> {
    let mut out = Vec::new();
    lzzzz::lz4f::decompress_to_vec(input, &mut out)?;
    Ok(out)
}

#[cfg(feature = "frame")]
pub fn lz4_flex_frame_compress_with(
    frame_info: lz4_flex::frame::FrameInfo,
    input: &[u8],
) -> Result<Vec<u8>, std::io::Error> {
    let buffer = Vec::new();
    let mut enc = lz4_flex::frame::FrameEncoder::with_frame_info(frame_info, buffer);
    std::io::Write::write_all(&mut enc, input)?;
    Ok(enc.finish()?)
}

#[cfg(feature = "frame")]
pub fn lz4_flex_frame_decompress(input: &[u8]) -> Result<Vec<u8>, lz4_flex::frame::Error> {
    let mut de = lz4_flex::frame::FrameDecoder::new(input);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut de, &mut out)?;
    Ok(out)
}

/// Test that the compressed string decompresses to the original string.
fn test_roundtrip(bytes: impl AsRef<[u8]>) {
    let bytes = bytes.as_ref();
    // compress with rust, decompress with rust
    let compressed_flex = compress_block(bytes);
    let decompressed = decompress(&compressed_flex, bytes.len()).unwrap();
    assert_eq!(decompressed, bytes);

    // compress with rust, decompress with rust, prepend size
    let compressed_flex = compress_prepend_size(bytes);
    let decompressed = decompress_size_prepended(&compressed_flex).unwrap();
    assert_eq!(decompressed, bytes);

    // Frame format
    // compress with rust, decompress with rust
    #[cfg(feature = "frame")]
    for bm in &[BlockMode::Independent, BlockMode::Linked] {
        let mut frame_info = lz4_flex::frame::FrameInfo::new();
        frame_info.block_mode = *bm;
        let compressed_flex = lz4_flex_frame_compress_with(frame_info, bytes).unwrap();
        let decompressed = lz4_flex_frame_decompress(&compressed_flex).unwrap();
        assert_eq!(decompressed, bytes);
    }

    lz4_cpp_compatibility(bytes);
}

/// disabled in miri case
#[cfg(miri)]
fn lz4_cpp_compatibility(_bytes: &[u8]) {}

#[cfg(not(miri))]
fn lz4_cpp_compatibility(bytes: &[u8]) {
    // compress with lz4 cpp, decompress with rust
    if !bytes.is_empty() {
        // lz4_cpp_block_compress will return empty output for empty input but
        // that's in the bindings and not the linked library.
        let compressed = lz4_cpp_block_compress(bytes).unwrap();
        let decompressed = decompress(&compressed, bytes.len()).unwrap();
        assert_eq!(decompressed, bytes);
    }

    // compress with rust, decompress with lz4 cpp
    let compressed_flex = compress_block(bytes);
    let decompressed = lz4_cpp_block_decompress(&compressed_flex, bytes.len()).unwrap();
    assert_eq!(decompressed, bytes);

    // Frame format
    #[cfg(feature = "frame")]
    {
        // compress with lz4 cpp, decompress with rust
        let compressed = lz4_cpp_frame_compress(bytes, true).unwrap();
        let decompressed = lz4_flex_frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, bytes);
        let compressed = lz4_cpp_frame_compress(bytes, false).unwrap();
        let decompressed = lz4_flex_frame_decompress(&compressed).unwrap();
        assert_eq!(decompressed, bytes);

        // compress with rust, decompress with lz4 cpp
        if !bytes.is_empty() {
            // compress_frame won't write a header if nothing is written to it
            // which is more in line with io::Write interface?
            for bm in &[BlockMode::Independent, BlockMode::Linked] {
                let mut frame_info = lz4_flex::frame::FrameInfo::new();
                frame_info.block_mode = *bm;
                let compressed_flex = lz4_flex_frame_compress_with(frame_info, bytes).unwrap();
                let decompressed = lz4_cpp_frame_decompress(&compressed_flex).unwrap();
                assert_eq!(decompressed, bytes);
            }
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn compare_compression() {
    print_compression_ration(COMPRESSION1K, "1k");
    print_compression_ration(COMPRESSION34K, "34k");
    print_compression_ration(COMPRESSION66JSON, "66k JSON");
    print_compression_ration(COMPRESSION10MB, "10mb");
}

#[test]
fn test_minimum_compression_ratio_block() {
    let compressed = compress_block(COMPRESSION34K);
    let ratio = compressed.len() as f64 / COMPRESSION34K.len() as f64;
    assert_lt!(ratio, 0.585); // TODO check why compression is not deterministic (fails in ci for
                              // 0.58)
    let compressed = compress_block(COMPRESSION65);
    let ratio = compressed.len() as f64 / COMPRESSION65.len() as f64;
    assert_lt!(ratio, 0.574);

    let compressed = compress_block(COMPRESSION66JSON);
    let ratio = compressed.len() as f64 / COMPRESSION66JSON.len() as f64;
    assert_lt!(ratio, 0.229);
}

#[cfg(feature = "frame")]
#[test]
fn test_minimum_compression_ratio_frame() {
    use lz4_flex::frame::FrameInfo;

    let get_ratio = |input| {
        let compressed = lz4_flex_frame_compress_with(FrameInfo::new(), input).unwrap();

        let ratio = compressed.len() as f64 / input.len() as f64;
        ratio
    };

    let ratio = get_ratio(COMPRESSION34K);
    assert_lt!(ratio, 0.585);

    let ratio = get_ratio(COMPRESSION65);
    assert_lt!(ratio, 0.574);

    let ratio = get_ratio(COMPRESSION66JSON);
    assert_lt!(ratio, 0.235);
}

use lz_fear::raw::compress2;
use lz_fear::raw::U16Table;
use lz_fear::raw::U32Table;

fn compress_lz4_fear(input: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    if input.len() <= 0xFFFF {
        compress2(input, 0, &mut U16Table::default(), &mut buf).unwrap();
    } else {
        compress2(input, 0, &mut U32Table::default(), &mut buf).unwrap();
    }
    buf
}

fn print_compression_ration(input: &'static [u8], name: &str) {
    println!("\nComparing for {}", name);
    let name = "";
    let compressed = compress_block(input);
    // println!("{:?}", compressed);
    println!(
        "lz4_flex block Compression Ratio {:?} {:?}",
        name,
        compressed.len() as f64 / input.len() as f64
    );
    let decompressed = decompress(&compressed, input.len()).unwrap();
    assert_eq!(decompressed, input);

    let compressed = lz4_cpp_block_compress(input).unwrap();
    // println!("{:?}", compressed);
    println!(
        "Lz4 Cpp block Compression Ratio {:?} {:?}",
        name,
        compressed.len() as f64 / input.len() as f64
    );
    let decompressed = decompress(&compressed, input.len()).unwrap();

    assert_eq!(decompressed, input);
    let compressed = lz4_rust_compress(input);
    println!(
        "lz4_rust_compress block Compression Ratio {:?} {:?}",
        name,
        compressed.len() as f64 / input.len() as f64
    );

    assert_eq!(decompressed, input);
    let compressed = compress_lz4_fear(input);
    println!(
        "lz4_fear_compress block Compression Ratio {:?} {:?}",
        name,
        compressed.len() as f64 / input.len() as f64
    );

    let compressed = snap::raw::Encoder::new().compress_vec(input).unwrap();
    println!(
        "snap Compression Ratio {:?} {:?}",
        name,
        compressed.len() as f64 / input.len() as f64
    );

    #[cfg(feature = "frame")]
    {
        let mut frame_info = lz4_flex::frame::FrameInfo::new();
        frame_info.block_mode = BlockMode::Independent;
        //frame_info.block_size = lz4_flex::frame::BlockSize::Max4MB;
        let compressed = lz4_flex_frame_compress_with(frame_info, input).unwrap();
        println!(
            "lz4_flex frame indep Compression Ratio {:?} {:?}",
            name,
            compressed.len() as f64 / input.len() as f64
        );

        let mut frame_info = lz4_flex::frame::FrameInfo::new();
        frame_info.block_mode = BlockMode::Linked;
        let compressed = lz4_flex_frame_compress_with(frame_info, input).unwrap();
        println!(
            "lz4_flex frame linked Compression Ratio {:?} {:?}",
            name,
            compressed.len() as f64 / input.len() as f64
        );

        let compressed = lz4_cpp_frame_compress(input, true).unwrap();
        println!(
            "lz4 cpp frame indep Compression Ratio {:?} {:?}",
            name,
            compressed.len() as f64 / input.len() as f64
        );

        let compressed = lz4_cpp_frame_compress(input, false).unwrap();
        println!(
            "lz4 cpp frame linked Compression Ratio {:?} {:?}",
            name,
            compressed.len() as f64 / input.len() as f64
        );
    }
}

// #[test]
// fn test_ratio() {
//     const COMPRESSION66K: &'static [u8] = include_bytes!("../benches/compression_65k.txt");
//     let compressed = compress(COMPRESSION66K);
//     println!("Compression Ratio 66K {:?}", compressed.len() as f64/ COMPRESSION66K.len()  as
// f64);     let _decompressed = decompress(&compressed).unwrap();

//     let mut vec = Vec::with_capacity(10 + (COMPRESSION66K.len() as f64 * 1.1) as usize);
//     let input = COMPRESSION66K;

//     let bytes_written = compress_into_2(input, &mut vec, 256, 8).unwrap();
//     println!("dict size 256 {:?}", bytes_written as f64/ COMPRESSION66K.len()  as f64);
//     let bytes_written = compress_into_2(input, &mut vec, 512, 7).unwrap();
//     println!("dict size 512 {:?}", bytes_written as f64/ COMPRESSION66K.len()  as f64);
//     let bytes_written = compress_into_2(input, &mut vec, 1024, 6).unwrap();
//     println!("dict size 1024 {:?}", bytes_written as f64/ COMPRESSION66K.len()  as f64);
//     let bytes_written = compress_into_2(input, &mut vec, 2048, 5).unwrap();
//     println!("dict size 2048 {:?}", bytes_written as f64/ COMPRESSION66K.len()  as f64);
//     let bytes_written = compress_into_2(input, &mut vec, 4096, 4).unwrap();
//     println!("dict size 4096 {:?}", bytes_written as f64/ COMPRESSION66K.len()  as f64);
//     let bytes_written = compress_into_2(input, &mut vec, 8192, 3).unwrap();
//     println!("dict size 8192 {:?}", bytes_written as f64/ COMPRESSION66K.len()  as f64);
//     let bytes_written = compress_into_2(input, &mut vec, 16384, 2).unwrap();
//     println!("dict size 16384 {:?}", bytes_written as f64/ COMPRESSION66K.len()  as f64);
//     let bytes_written = compress_into_2(input, &mut vec, 32768, 1).unwrap();
//     println!("dict size 32768 {:?}", bytes_written as f64/ COMPRESSION66K.len()  as f64);

//     // let bytes_written = compress_into_2(input, &mut vec).unwrap();

// }

#[cfg(test)]
mod checked_decode {
    use super::*;

    #[cfg_attr(not(feature = "checked-decode"), ignore)]
    #[test]
    fn error_case_1() {
        let _err = decompress_size_prepended(&[122, 1, 0, 1, 0, 10, 1, 0]);
    }
    #[cfg_attr(not(feature = "checked-decode"), ignore)]
    #[test]
    fn error_case_2() {
        let _err = decompress_size_prepended(&[
            44, 251, 49, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
    }
    #[cfg_attr(not(feature = "checked-decode"), ignore)]
    #[test]
    fn error_case_3() {
        let _err = decompress_size_prepended(&[
            7, 0, 0, 0, 0, 0, 0, 11, 0, 0, 7, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 1, 0, 0,
        ]);
    }

    #[cfg_attr(not(feature = "checked-decode"), ignore)]
    #[test]
    fn error_case_4() {
        let _err = decompress_size_prepended(&[0, 61, 0, 0, 0, 7, 0]);
    }

    #[cfg_attr(not(feature = "checked-decode"), ignore)]
    #[test]
    fn error_case_5() {
        let _err = decompress_size_prepended(&[8, 0, 0, 0, 4, 0, 0, 0]);
    }
}

#[test]
fn test_end_offset() {
    // the last 5 bytes need to be literals, so the last match block is not allowed to match to the
    // end
    test_roundtrip("AAAAAAAAAAAAAAAAAAAAAAAAaAAAAAAAAAAAAAAAAAAAAAAAA");
    test_roundtrip("AAAAAAAAAAAAAAAAAAAAAAAABBBBBBBBBaAAAAAAAAAAAAAAAAAAAAAAAA");
}
#[test]
fn small_compressible_1() {
    test_roundtrip("AAAAAAAAAAAAAAAAAAAAAAAABBBBBBBBBaAAAAAAAAAAAAAAAAAAAAAAAABBBBBBBBBa");
}
#[test]
fn small_compressible_2() {
    test_roundtrip("AAAAAAAAAAAZZZZZZZZAAAAAAAA");
}

#[test]
fn small_compressible_3() {
    test_roundtrip("AAAAAAAAAAAZZZZZZZZAAAAAAAA");
}

#[test]
fn shakespear1() {
    test_roundtrip("to live or not to live");
}
#[test]
fn shakespear2() {
    test_roundtrip("Love is a wonderful terrible thing");
}
#[test]
fn shakespear3() {
    test_roundtrip("There is nothing either good or bad, but thinking makes it so.");
}
#[test]
fn shakespear4() {
    test_roundtrip("I burn, I pine, I perish.");
}

#[test]
fn text_text() {
    test_roundtrip("Save water, it doesn't grow on trees.");
    test_roundtrip("The panda bear has an amazing black-and-white fur.");
    test_roundtrip("The average panda eats as much as 9 to 14 kg of bamboo shoots a day.");
    test_roundtrip("You are 60% water. Save 60% of yourself!");
    test_roundtrip("To cute to die! Save the red panda!");
}

#[test]
fn not_compressible() {
    test_roundtrip("as6yhol.;jrew5tyuikbfewedfyjltre22459ba");
    test_roundtrip("jhflkdjshaf9p8u89ybkvjsdbfkhvg4ut08yfrr");
}
#[test]
fn short_1() {
    test_roundtrip("ahhd");
    test_roundtrip("ahd");
    test_roundtrip("x-29");
    test_roundtrip("x");
    test_roundtrip("k");
    test_roundtrip(".");
    test_roundtrip("ajsdh");
    test_roundtrip("aaaaaa");
}

#[test]
fn short_2() {
    test_roundtrip("aaaaaabcbcbcbc");
}

#[test]
fn empty_string() {
    test_roundtrip("");
}

#[test]
fn nulls() {
    test_roundtrip("\0\0\0\0\0\0\0\0\0\0\0\0\0");
}

#[test]
fn bug_fuzz() {
    let data = &[
        8, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 46, 0, 0, 8, 0, 138,
    ];
    test_roundtrip(data);
}
#[test]
fn bug_fuzz_2() {
    let data = &[
        122, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 65, 0, 0, 128, 10, 1, 10, 1, 0, 122,
    ];
    test_roundtrip(data);
}
#[test]
fn bug_fuzz_3() {
    let data = &[
        36, 16, 0, 0, 79, 177, 176, 176, 171, 1, 0, 255, 207, 79, 79, 79, 79, 79, 1, 1, 49, 0, 16,
        0, 79, 79, 79, 79, 79, 1, 0, 255, 36, 79, 79, 79, 79, 79, 1, 0, 255, 207, 79, 79, 79, 79,
        79, 1, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 8, 207, 1, 207, 207, 79, 199,
        79, 79, 40, 79, 1, 1, 1, 1, 1, 1, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
        15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 79, 15, 15, 14, 15, 15, 15, 15, 15, 15,
        15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 61, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 0,
        48, 45, 0, 1, 0, 0, 1, 0,
    ];
    test_roundtrip(data);
}
#[test]
fn bug_fuzz_4() {
    let data = &[147];
    test_roundtrip(data);
}
#[test]
fn buf_fuzz_5() {
    let data = &[
        255, 255, 255, 255, 253, 235, 156, 140, 8, 0, 140, 45, 169, 0, 27, 128, 48, 0, 140, 0, 0,
        255, 255, 255, 253, 235, 156, 140, 8, 61, 255, 255, 255, 255, 65, 239, 254,
    ];

    test_roundtrip(data);
}

#[test]
fn test_so_many_zeros() {
    let data: Vec<u8> = iter::repeat(0).take(30_000).collect();
    test_roundtrip(data);
}

#[test]
fn compression_works() {
    let s = r#"An iterator that knows its exact length.
        Many Iterators don't know how many times they will iterate, but some do. If an iterator knows how many times it can iterate, providing access to that information can be useful. For example, if you want to iterate backwards, a good start is to know where the end is.
        When implementing an ExactSizeIterator, you must also implement Iterator. When doing so, the implementation of size_hint must return the exact size of the iterator.
        The len method has a default implementation, so you usually shouldn't implement it. However, you may be able to provide a more performant implementation than the default, so overriding it in this case makes sense."#;

    test_roundtrip(s);
    assert!(compress_block(s.as_bytes()).len() < s.len());
}

// #[test]
// fn multi_compress() {
//     let s1 = r#"An iterator that knows its exact length.performant implementation than the
// default, so overriding it in this case makes sense."#;     let s2 = r#"An iterator that knows its
// exact length.performant implementation than the default, so overriding it in this case makes
// sense."#;     let mut out = vec![];
//     compress_into()
//     inverse(s);
//     assert!(compress(s.as_bytes()).len() < s.len());
// }

#[ignore]
#[test]
fn big_compression() {
    let mut s = Vec::with_capacity(80_000_000);

    for n in 0..80_000_000 {
        s.push((n as u8).wrapping_mul(0xA).wrapping_add(33) ^ 0xA2);
    }

    test_roundtrip(s);
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_text_10mb() {
    test_roundtrip(COMPRESSION10MB);
}
#[test]
fn test_json_66k() {
    test_roundtrip(COMPRESSION66JSON);
}
#[test]
fn test_text_65k() {
    test_roundtrip(COMPRESSION65);
}
#[test]
fn test_text_34k() {
    test_roundtrip(COMPRESSION34K);
}

#[test]
fn test_text_1k() {
    test_roundtrip(COMPRESSION1K);
}

use proptest::{prelude::*, test_runner::FileFailurePersistence};

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource("regressions"))),
        ..Default::default()
    })]

    #[test]
    #[cfg_attr(miri, ignore)]
    fn proptest_roundtrip(v in vec_of_vec()) {
        let data: Vec<u8>  = v.iter().flat_map(|v|v.into_iter()).cloned().collect::<Vec<_>>();
        test_roundtrip(&data);  // sum of the sum of all vectors.
    }

}

fn vec_of_vec() -> impl Strategy<Value = Vec<Vec<u8>>> {
    const N: u8 = 200;

    let length = 0..N;
    length.prop_flat_map(vec_from_length)
}

fn vec_from_length(length: u8) -> impl Strategy<Value = Vec<Vec<u8>>> {
    const K: usize = u8::MAX as usize;
    let mut result = vec![];
    for index in 1..length {
        let inner = proptest::collection::vec(0..index, 0..K);
        result.push(inner);
    }
    result
}

#[cfg(feature = "frame")]
mod frame {
    use lz4_flex::frame::BlockSize;

    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn concatenated() {
        let mut enc = lz4_flex::frame::FrameEncoder::new(Vec::new());
        enc.write_all(COMPRESSION1K).unwrap();
        enc.try_finish().unwrap();
        enc.write_all(COMPRESSION34K).unwrap();
        let compressed = enc.finish().unwrap();

        let mut dec = lz4_flex::frame::FrameDecoder::new(&*compressed);
        let mut uncompressed = Vec::new();
        dec.read_to_end(&mut uncompressed).unwrap();
        assert_eq!(&*uncompressed, COMPRESSION1K);
        uncompressed.clear();
        dec.read_to_end(&mut uncompressed).unwrap();
        assert_eq!(&*uncompressed, COMPRESSION34K);
    }

    #[test]
    fn checksums() {
        for &input in &[COMPRESSION34K, COMPRESSION66JSON] {
            // Block checksum
            let mut frame_info = lz4_flex::frame::FrameInfo::new();
            frame_info.block_checksums = true;
            let mut compressed = lz4_flex_frame_compress_with(frame_info, input).unwrap();
            // roundtrip
            let uncompressed = lz4_flex_frame_decompress(&compressed).unwrap();
            assert_eq!(uncompressed, input);
            // corrupt last block checksum, which is at 8th to 4th last bytes of the compressed
            // output
            let compressed_len = compressed.len();
            compressed[compressed_len - 5] ^= 0xFF;
            match lz4_flex_frame_decompress(&compressed) {
                Err(lz4_flex::frame::Error::BlockChecksumError) => (),
                r => panic!("{:?}", r),
            }

            // Content checksum
            let mut frame_info = lz4_flex::frame::FrameInfo::new();
            frame_info.content_checksum = true;
            let mut compressed = lz4_flex_frame_compress_with(frame_info, input).unwrap();
            // roundtrip
            let uncompressed = lz4_flex_frame_decompress(&compressed).unwrap();
            assert_eq!(uncompressed, input);

            // corrupt content checksum, which is the last 4 bytes of the compressed output
            let compressed_len = compressed.len();
            compressed[compressed_len - 1] ^= 0xFF;
            match lz4_flex_frame_decompress(&compressed) {
                Err(lz4_flex::frame::Error::ContentChecksumError) => (),
                r => panic!("{:?}", r),
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn block_size() {
        let mut last_compressed_len = usize::MAX;
        for block_size in &[
            BlockSize::Max64KB,
            BlockSize::Max256KB,
            BlockSize::Max1MB,
            BlockSize::Max4MB,
        ] {
            let mut frame_info = lz4_flex::frame::FrameInfo::new();
            frame_info.block_size = *block_size;
            let compressed = lz4_flex_frame_compress_with(frame_info, COMPRESSION10MB).unwrap();

            // roundtrip
            let uncompressed = lz4_flex_frame_decompress(&compressed).unwrap();
            assert_eq!(uncompressed, COMPRESSION10MB);

            // For a large enough input (eg. the 10MB input) we should get strictly
            // better compression by increasing the block size.
            assert!(compressed.len() < last_compressed_len);
            last_compressed_len = compressed.len();
        }
    }

    #[test]
    fn content_size() {
        let mut frame_info = lz4_flex::frame::FrameInfo::new();
        frame_info.content_size = Some(COMPRESSION1K.len() as u64);
        let mut compressed = lz4_flex_frame_compress_with(frame_info, COMPRESSION1K).unwrap();

        // roundtrip
        let uncompressed = lz4_flex_frame_decompress(&compressed).unwrap();
        assert_eq!(uncompressed, COMPRESSION1K);

        // corrupt the len in the compressed bytes
        {
            // We'll generate a valid FrameInfo and copy it to the test data
            let mut frame_info = lz4_flex::frame::FrameInfo::new();
            frame_info.content_size = Some(3);
            let dummy_compressed = lz4_flex_frame_compress_with(frame_info, b"123").unwrap();
            // `15` (7 + 8) is the size of the header plus the content size in the compressed bytes
            compressed[..15].copy_from_slice(&dummy_compressed[..15]);
        }
        match lz4_flex_frame_decompress(&compressed) {
            Err(lz4_flex::frame::Error::ContentLengthError { expected, actual }) => {
                assert_eq!(expected, 3);
                assert_eq!(actual, 725);
            }
            r => panic!("{:?}", r),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn legacy_frame() {
        let uncompressed = lz4_flex_frame_decompress(DECOMPRESSION10MB).unwrap();
        assert_eq!(uncompressed, COMPRESSION10MB);
    }
}

#[cfg(test)]
mod test_compression {
    use super::*;

    fn print_ratio(text: &str, val1: usize, val2: usize) {
        println!(
            "{:?} {:.3} {} -> {}",
            text,
            val1 as f32 / val2 as f32,
            val1,
            val2
        );
    }

    #[test]
    fn test_comp_flex() {
        print_ratio(
            "Ratio 1k flex",
            COMPRESSION1K.len(),
            compress_block(COMPRESSION1K).len(),
        );
        print_ratio(
            "Ratio 34k flex",
            COMPRESSION34K.len(),
            compress_block(COMPRESSION34K).len(),
        );
    }

    mod lz4_linked {
        use super::*;
        fn get_compressed_size(input: &[u8]) -> usize {
            let output = lz4_cpp_block_compress(input).unwrap();
            output.len()
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn test_comp_lz4_linked() {
            print_ratio(
                "Ratio 1k C",
                COMPRESSION1K.len(),
                get_compressed_size(COMPRESSION1K),
            );
            print_ratio(
                "Ratio 34k C",
                COMPRESSION34K.len(),
                get_compressed_size(COMPRESSION34K),
            );
        }
    }
}
