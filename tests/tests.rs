//! Tests.

#[macro_use]
extern crate more_asserts;
// extern crate test;

// use crate::block::compress::compress_into_2;
use lz4::block::{compress as lz4_cpp_block_compress, decompress as lz4_cpp_block_decompress};
use lz4_compress::compress as lz4_rust_compress;
use lz4_flex::block::{compress_prepend_size, decompress_size_prepended};
use lz4_flex::{compress, decompress};
use std::str;

const COMPRESSION1K: &'static [u8] = include_bytes!("../benches/compression_1k.txt");
const COMPRESSION34K: &'static [u8] = include_bytes!("../benches/compression_34k.txt");
const COMPRESSION65: &'static [u8] = include_bytes!("../benches/compression_65k.txt");
const COMPRESSION66JSON: &'static [u8] = include_bytes!("../benches/compression_66k_JSON.txt");
// const COMPRESSION10MB: &'static [u8] = include_bytes!("../benches/dickens.txt");

// #[bench]
// fn bench_compression_small(b: &mut test::Bencher) {
//     b.iter(|| {
//         let _compressed = compress("To cute to die! Save the red panda!".as_bytes());
//     })
// }

// #[bench]
// fn bench_compression_medium(b: &mut test::Bencher) {
//     b.iter(|| {
//         let _compressed = compress(r#"An iterator that knows its exact length.
//         Many Iterators don't know how many times they will iterate, but some do. If an iterator knows how many times it can iterate, providing access to that information can be useful. For example, if you want to iterate backwards, a good start is to know where the end is.
//         When implementing an ExactSizeIterator, you must also implement Iterator. When doing so, the implementation of size_hint must return the exact size of the iterator.
//         The len method has a default implementation, so you usually shouldn't implement it. However, you may be able to provide a more performant implementation than the default, so overriding it in this case makes sense."#.as_bytes());
//     })
// }

// #[bench]
// fn bench_compression_65k(b: &mut test::Bencher) {
//     b.iter(|| {
//         compress(COMPRESSION65);
//     })
// }

// #[ignore]
// #[bench]
// fn bench_compression_10_mb(b: &mut test::Bencher) {
//     b.iter(|| {
//         compress(COMPRESSION10MB);
//     })
// }

// #[bench]
// fn bench_decompression_small(b: &mut test::Bencher) {
//     let comp = compress("To cute to die! Save the red panda!".as_bytes());
//     b.iter(|| {
//         decompress(&comp)
//     })
// }

// #[bench]
// fn bench_decompression_medium(b: &mut test::Bencher) {
//     let comp = compress(r#"An iterator that knows its exact length.
//         Many Iterators don't know how many times they will iterate, but some do. If an iterator knows how many times it can iterate, providing access to that information can be useful. For example, if you want to iterate backwards, a good start is to know where the end is.
//         When implementing an ExactSizeIterator, you must also implement Iterator. When doing so, the implementation of size_hint must return the exact size of the iterator.
//         The len method has a default implementation, so you usually shouldn't implement it. However, you may be able to provide a more performant implementation than the default, so overriding it in this case makes sense."#.as_bytes());
//     b.iter(|| {
//         decompress(&comp)
//     })
// }

// #[bench]
// fn bench_decompression_10_mb(b: &mut test::Bencher) {
//     let comp = compress(COMPRESSION10MB);
//     b.iter(|| {
//         decompress(&comp)
//     })
// }

/// Test that the compressed string decompresses to the original string.
fn inverse(s: &str) {
    inverse_bytes(s.as_bytes());
}

/// Test that the compressed string decompresses to the original string.
fn inverse_bytes(bytes: &[u8]) {
    // compress with rust, decompress with rust
    let compressed_flex = compress(bytes);
    let decompressed = decompress(&compressed_flex, bytes.len()).unwrap();
    assert_eq!(decompressed, bytes);

    lz4_cpp_compatibility(bytes);

    // compress with rust, decompress with rust
    let compressed_flex = compress(bytes);
    let decompressed = decompress(&compressed_flex, bytes.len()).unwrap();
    assert_eq!(decompressed, bytes);

    // compress with rust, decompress with rust, prepend size
    let compressed_flex = compress_prepend_size(bytes);
    let decompressed = decompress_size_prepended(&compressed_flex).unwrap();
    assert_eq!(decompressed, bytes);
}


/// disabled in miri case
#[cfg(miri)]
fn lz4_cpp_compatibility(_bytes: &[u8]) {}

#[cfg(not(miri))]
fn lz4_cpp_compatibility(bytes: &[u8]) {
    let compressed_flex = compress(bytes);

    // compress with lz4 cpp, decompress with rust
    let compressed = lz4_cpp_block_compress(bytes, None, false).unwrap();
    let decompressed = decompress(&compressed, bytes.len()).unwrap();
    assert_eq!(decompressed, bytes);

    if bytes.len() != 0 {
        // compress with rust, decompress with lz4 cpp
            let decompressed =
        lz4_cpp_block_decompress(&compressed_flex, Some(bytes.len() as i32)).unwrap();
        assert_eq!(decompressed, bytes);
    }

}

#[test]
#[cfg_attr(miri, ignore)]
fn yopa() {
    const COMPRESSION10MB: &'static [u8] = include_bytes!("../benches/dickens.txt");
    let compressed = compress(COMPRESSION10MB);
    decompress(&compressed, COMPRESSION10MB.len()).unwrap();

    lz4_cpp_block_compress(COMPRESSION10MB, None, false).unwrap();

    const COMPRESSION66K: &'static [u8] = include_bytes!("../benches/compression_65k.txt");
    let compressed = compress(COMPRESSION66K);
    decompress(&compressed, COMPRESSION66K.len()).unwrap();

    lz4_cpp_block_compress(COMPRESSION66K, None, false).unwrap();

    const COMPRESSION34K: &'static [u8] = include_bytes!("../benches/compression_34k.txt");
    let compressed = compress(COMPRESSION34K);
    decompress(&compressed, COMPRESSION34K.len()).unwrap();

    lz4_cpp_block_compress(COMPRESSION34K, None, false).unwrap();

    lz4_rust_compress(COMPRESSION34K);
}

#[test]
#[cfg_attr(miri, ignore)]
fn compare_compression() {
    print_compression_ration(include_bytes!("../benches/compression_34k.txt"), "34k");
    print_compression_ration(include_bytes!("../benches/compression_66k_JSON.txt"), "66k JSON");
}

#[test]
fn test_minimum_compression_ratio() {
    let input = include_bytes!("../benches/compression_34k.txt");
    let compressed = compress(input);
    let ratio = compressed.len() as f64 / input.len() as f64;
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

    let compressed = lz4_cpp_block_compress(input, None, false).unwrap();
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

    #[cfg_attr(not(feature="checked_decode"), ignore)]
    #[test]
    fn error_case_1() {
        let _err = decompress_size_prepended(&[122, 1, 0, 1, 0, 10, 1, 0]);
    }
    #[cfg_attr(not(feature="checked_decode"), ignore)]
    #[test]
    fn error_case_2() {
        let _err = decompress_size_prepended(&[44, 251, 49, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
    #[cfg_attr(not(feature="checked_decode"), ignore)]
    #[test]
    fn error_case_3() {
        let _err = decompress_size_prepended(&[7, 0, 0, 0, 0, 0, 0, 11, 0, 0, 7, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 1, 0, 0]);
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
    compress("AAAAAAAAAAAZZZZZZZZAAAAAAAA".as_bytes());
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
fn short() {
    inverse("ahhd");
    inverse("ahd");
    inverse("x-29");
    inverse("x");
    inverse("k");
    inverse(".");
    inverse("ajsdh");
    inverse("aaaaaa");
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
    let data = &[8, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 46, 0, 0, 8, 0, 138];
    inverse_bytes(data);
}
#[test]
fn bug_fuzz_2() {
    let data = &[122, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 65, 0, 0, 128, 10, 1, 10, 1, 0, 122];
    inverse_bytes(data);
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

    assert_eq!(&decompress(&compress(&s), s.len()).unwrap(), &s);
}

#[test]
fn test_json_66k() {
    inverse_bytes(COMPRESSION66JSON);
}
#[test]
fn test_text_65k() {
    inverse_bytes(COMPRESSION65);
}
#[test]
fn test_text_34k() {
    inverse_bytes(COMPRESSION34K);
}

#[cfg(test)]
mod test_compression {
    use super::*;

    fn print_ratio(text: &str, val1: usize, val2: usize) {
        println!("{:?} {:.2}", text, val1 as f32 / val2 as f32);
    }

    #[test]
    fn test_comp_flex() {
        print_ratio(
            "Ratio 1k",
            COMPRESSION1K.len(),
            compress(COMPRESSION1K).len(),
        );
        print_ratio(
            "Ratio 34k",
            COMPRESSION34K.len(),
            compress(COMPRESSION34K).len(),
        );
    }

    mod lz4_linked {
        use super::*;
        use std::io;
        fn get_compressed_size(mut input: &[u8]) -> usize {
            let mut cache = vec![];
            let mut encoder = lz4::EncoderBuilder::new()
                .level(2)
                .build(&mut cache)
                .unwrap();
            io::copy(&mut input, &mut encoder).unwrap();
            let (output, _result) = encoder.finish();
            output.len()
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn test_comp_lz4_linked() {
            print_ratio(
                "Ratio 1k",
                COMPRESSION1K.len(),
                get_compressed_size(COMPRESSION1K),
            );
            print_ratio(
                "Ratio 34k",
                COMPRESSION34K.len(),
                get_compressed_size(COMPRESSION34K),
            );
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_compare_compress() {
    let mut input: &[u8] = &[10, 12, 14, 16];

    let mut cache = vec![];
    let mut encoder = lz4::EncoderBuilder::new()
        .level(2)
        .build(&mut cache)
        .unwrap();
    // let mut read = *input;
    std::io::copy(&mut input, &mut encoder).unwrap();
    let (comp_lz4, _result) = encoder.finish();

    println!("{:?}", comp_lz4);

    let input: &[u8] = &[10, 12, 14, 16];
    let out = compress(&input);
    dbg!(&out);
}
