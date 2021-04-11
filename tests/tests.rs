//! Tests.

#[macro_use]
extern crate more_asserts;

use lz4_compress::compress as lz4_rust_compress;
use lz4_flex::{
    block::{compress_prepend_size, decompress_size_prepended},
    compress, decompress,
    frame::{FrameDecoder, FrameEncoder},
};

use std::{io::prelude::*, str};

const COMPRESSION1K: &'static [u8] = include_bytes!("../benches/compression_1k.txt");
const COMPRESSION34K: &'static [u8] = include_bytes!("../benches/compression_34k.txt");
const COMPRESSION65: &'static [u8] = include_bytes!("../benches/compression_65k.txt");
const COMPRESSION66JSON: &'static [u8] = include_bytes!("../benches/compression_66k_JSON.txt");
const COMPRESSION10MB: &'static [u8] = include_bytes!("../benches/dickens.txt");

fn lz4_cpp_block_compress(input: &[u8]) -> Result<Vec<u8>, lzzzz::Error> {
    let mut out = Vec::new();
    lzzzz::lz4::compress_to_vec(input, &mut out, lzzzz::lz4::ACC_LEVEL_DEFAULT).unwrap();
    Ok(out)
}

fn lz4_cpp_frame_compress(input: &[u8]) -> Result<Vec<u8>, lzzzz::Error> {
    let pref = lzzzz::lz4f::PreferencesBuilder::new()
        .block_mode(lzzzz::lz4f::BlockMode::Linked)
        .block_size(lzzzz::lz4f::BlockSize::Max64KB)
        .build();
    let mut out = Vec::new();
    lzzzz::lz4f::compress_to_vec(input, &mut out, &pref).unwrap();
    Ok(out)
}

fn lz4_cpp_block_decompress(input: &[u8], decomp_len: usize) -> Result<Vec<u8>, lzzzz::Error> {
    let mut out = vec![0u8; decomp_len];
    lzzzz::lz4::decompress(input, &mut out)?;
    Ok(out)
}

fn lz4_cpp_frame_decompress(input: &[u8]) -> Result<Vec<u8>, lzzzz::lz4f::Error> {
    let mut out = Vec::new();
    lzzzz::lz4f::decompress_to_vec(input, &mut out)?;
    Ok(out)
}

fn compress_frame(input: &[u8]) -> Vec<u8> {
    let mut enc = FrameEncoder::new(Vec::new());
    enc.write_all(input).unwrap();
    enc.finish().unwrap()
}

fn decompress_frame(input: &[u8]) -> Result<Vec<u8>, lz4_flex::frame::Error> {
    let mut de = FrameDecoder::new(input);
    let mut out = Vec::new();
    de.read_to_end(&mut out)?;
    Ok(out)
}

/// Test that the compressed string decompresses to the original string.
fn inverse(bytes: impl AsRef<[u8]>) {
    let bytes = bytes.as_ref();
    // compress with rust, decompress with rust
    let compressed_flex = compress(bytes);
    let decompressed = decompress(&compressed_flex, bytes.len()).unwrap();
    assert_eq!(decompressed, bytes);

    // compress with rust, decompress with rust, prepend size
    let compressed_flex = compress_prepend_size(bytes);
    let decompressed = decompress_size_prepended(&compressed_flex).unwrap();
    assert_eq!(decompressed, bytes);

    // Frame format
    // compress with rust, decompress with rust
    let compressed_flex = compress_frame(bytes);
    let decompressed = decompress_frame(&compressed_flex).unwrap();
    assert_eq!(decompressed, bytes);

    lz4_cpp_compatibility(bytes);
}

/// disabled in miri case
#[cfg(miri)]
fn lz4_cpp_compatibility(_bytes: &[u8]) {}

#[cfg(not(miri))]
fn lz4_cpp_compatibility(bytes: &[u8]) {
    // compress with lz4 cpp, decompress with rust
    let compressed = lz4_cpp_block_compress(bytes).unwrap();
    let decompressed = decompress(&compressed, bytes.len()).unwrap();
    assert_eq!(decompressed, bytes);

    // compress with rust, decompress with lz4 cpp
    let compressed_flex = compress(bytes);
    let decompressed = lz4_cpp_block_decompress(&compressed_flex, bytes.len()).unwrap();
    assert_eq!(decompressed, bytes);

    // Frame format
    // compress with lz4 cpp, decompress with rust
    let compressed = lz4_cpp_frame_compress(bytes).unwrap();
    let decompressed = decompress_frame(&compressed).unwrap();
    assert_eq!(decompressed, bytes);

    if bytes.len() != 0 {
        // compress with rust, decompress with lz4 cpp
        let compressed_flex = compress_frame(bytes);
        let decompressed = lz4_cpp_frame_decompress(&compressed_flex).unwrap();
        assert_eq!(decompressed, bytes);
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
fn test_minimum_compression_ratio() {
    let compressed = compress(COMPRESSION34K);
    let ratio = compressed.len() as f64 / COMPRESSION34K.len() as f64;
    assert_lt!(ratio, 0.585); // TODO check why compression is not deterministic (fails in ci for 0.58)
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
    let compressed = compress(input);
    // println!("{:?}", compressed);
    println!(
        "lz4_flex Compression Ratio {:?} {:?}",
        name,
        compressed.len() as f64 / input.len() as f64
    );
    let decompressed = decompress(&compressed, input.len()).unwrap();
    assert_eq!(decompressed, input);

    let compressed = lz4_cpp_block_compress(input).unwrap();
    // println!("{:?}", compressed);
    println!(
        "Lz4 Cpp Compression Ratio {:?} {:?}",
        name,
        compressed.len() as f64 / input.len() as f64
    );
    let decompressed = decompress(&compressed, input.len()).unwrap();

    assert_eq!(decompressed, input);
    let compressed = lz4_rust_compress(input);
    println!(
        "lz4_rust_compress Compression Ratio {:?} {:?}",
        name,
        compressed.len() as f64 / input.len() as f64
    );

    assert_eq!(decompressed, input);
    let compressed = compress_lz4_fear(input);
    println!(
        "lz4_fear_compress Compression Ratio {:?} {:?}",
        name,
        compressed.len() as f64 / input.len() as f64
    );
}

// #[test]
// fn test_ratio() {
//     const COMPRESSION66K: &'static [u8] = include_bytes!("../benches/compression_65k.txt");
//     let compressed = compress(COMPRESSION66K);
//     println!("Compression Ratio 66K {:?}", compressed.len() as f64/ COMPRESSION66K.len()  as f64);
//     let _decompressed = decompress(&compressed).unwrap();

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

// the last 5 bytes need to be literals, so the last match block is not allowed to match to the end

// #[test]
// fn test_end_offset() {
//     inverse(&[122, 1, 0, 1, 0, 10, 1, 0]);
//     // inverse("AAAAAAAAAAAAAAAAAAAAAAAABBBBBBBBBaAAAAAAAAAAAAAAAAAAAAAAAA");
// }

#[cfg(test)]
mod checked_decode {
    use super::*;

    #[cfg_attr(not(feature = "checked_decode"), ignore)]
    #[test]
    fn error_case_1() {
        let _err = decompress_size_prepended(&[122, 1, 0, 1, 0, 10, 1, 0]);
    }
    #[cfg_attr(not(feature = "checked_decode"), ignore)]
    #[test]
    fn error_case_2() {
        let _err = decompress_size_prepended(&[
            44, 251, 49, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
    }
    #[cfg_attr(not(feature = "checked_decode"), ignore)]
    #[test]
    fn error_case_3() {
        let _err = decompress_size_prepended(&[
            7, 0, 0, 0, 0, 0, 0, 11, 0, 0, 7, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 1, 0, 0,
        ]);
    }
}

#[test]
fn test_end_offset() {
    inverse("AAAAAAAAAAAAAAAAAAAAAAAAaAAAAAAAAAAAAAAAAAAAAAAAA");
    // inverse("AAAAAAAAAAAAAAAAAAAAAAAABBBBBBBBBaAAAAAAAAAAAAAAAAAAAAAAAA");
}
#[test]
fn small_compressible_1() {
    inverse("AAAAAAAAAAAAAAAAAAAAAAAABBBBBBBBBaAAAAAAAAAAAAAAAAAAAAAAAABBBBBBBBBa");
}
#[test]
fn small_compressible_2() {
    inverse("AAAAAAAAAAAZZZZZZZZAAAAAAAA");
}

#[test]
fn small_compressible_3() {
    inverse("AAAAAAAAAAAZZZZZZZZAAAAAAAA");
}

#[test]
fn shakespear1() {
    inverse("to live or not to live");
}
#[test]
fn shakespear2() {
    inverse("Love is a wonderful terrible thing");
}
#[test]
fn shakespear3() {
    inverse("There is nothing either good or bad, but thinking makes it so.");
}
#[test]
fn shakespear4() {
    inverse("I burn, I pine, I perish.");
}

#[test]
fn text_text() {
    inverse("Save water, it doesn't grow on trees.");
    inverse("The panda bear has an amazing black-and-white fur.");
    inverse("The average panda eats as much as 9 to 14 kg of bamboo shoots a day.");
    inverse("You are 60% water. Save 60% of yourself!");
    inverse("To cute to die! Save the red panda!");
}

#[test]
fn not_compressible() {
    inverse("as6yhol.;jrew5tyuikbfewedfyjltre22459ba");
    inverse("jhflkdjshaf9p8u89ybkvjsdbfkhvg4ut08yfrr");
}
#[test]
fn short_1() {
    inverse("ahhd");
    inverse("ahd");
    inverse("x-29");
    inverse("x");
    inverse("k");
    inverse(".");
    inverse("ajsdh");
    inverse("aaaaaa");
}

#[test]
fn short_2() {
    inverse("aaaaaabcbcbcbc");
}

#[test]
fn empty_string() {
    inverse("");
}

#[test]
fn nulls() {
    inverse("\0\0\0\0\0\0\0\0\0\0\0\0\0");
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
    inverse(data);
}
#[test]
fn bug_fuzz_2() {
    let data = &[
        122, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 65, 0, 0, 128, 10, 1, 10, 1, 0, 122,
    ];
    inverse(data);
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
    inverse(data);
}
#[test]
fn compression_works() {
    let s = r#"An iterator that knows its exact length.
        Many Iterators don't know how many times they will iterate, but some do. If an iterator knows how many times it can iterate, providing access to that information can be useful. For example, if you want to iterate backwards, a good start is to know where the end is.
        When implementing an ExactSizeIterator, you must also implement Iterator. When doing so, the implementation of size_hint must return the exact size of the iterator.
        The len method has a default implementation, so you usually shouldn't implement it. However, you may be able to provide a more performant implementation than the default, so overriding it in this case makes sense."#;

    inverse(s);
    assert!(compress(s.as_bytes()).len() < s.len());
}

// #[test]
// fn multi_compress() {
//     let s1 = r#"An iterator that knows its exact length.performant implementation than the default, so overriding it in this case makes sense."#;
//     let s2 = r#"An iterator that knows its exact length.performant implementation than the default, so overriding it in this case makes sense."#;
//     let mut out = vec![];
//     compress_into()
//     inverse(s);
//     assert!(compress(s.as_bytes()).len() < s.len());
// }

#[ignore]
#[test]
fn big_compression() {
    let mut s = Vec::with_capacity(80_000000);

    for n in 0..80_000000 {
        s.push((n as u8).wrapping_mul(0xA).wrapping_add(33) ^ 0xA2);
    }

    inverse(s);
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_text_10mb() {
    inverse(COMPRESSION10MB);
}
#[test]
fn test_json_66k() {
    inverse(COMPRESSION66JSON);
}
#[test]
fn test_text_65k() {
    inverse(COMPRESSION65);
}
#[test]
fn test_text_34k() {
    inverse(COMPRESSION34K);
}

#[test]
fn test_text_1k() {
    inverse(COMPRESSION1K);
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
            compress(COMPRESSION1K).len(),
        );
        print_ratio(
            "Ratio 34k flex",
            COMPRESSION34K.len(),
            compress(COMPRESSION34K).len(),
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
