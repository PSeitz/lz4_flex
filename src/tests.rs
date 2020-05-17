//! Tests.

extern crate test;

use std::str;
use crate::{decompress, compress};

const COMPRESSION1K: &'static [u8] = include_bytes!("../benches/compression_1k.txt");
const COMPRESSION34K: &'static [u8] = include_bytes!("../benches/compression_34k.txt");
const COMPRESSION65: &'static [u8] = include_bytes!("../benches/compression_65k.txt");
const COMPRESSION10MB: &'static [u8] = include_bytes!("../benches/dickens.txt");

#[bench]
fn bench_compression_small(b: &mut test::Bencher) {
    b.iter(|| {
        let _compressed = compress("To cute to die! Save the red panda!".as_bytes());
    })
}

#[bench]
fn bench_compression_medium(b: &mut test::Bencher) {
    b.iter(|| {
        let _compressed = compress(r#"An iterator that knows its exact length.
        Many Iterators don't know how many times they will iterate, but some do. If an iterator knows how many times it can iterate, providing access to that information can be useful. For example, if you want to iterate backwards, a good start is to know where the end is.
        When implementing an ExactSizeIterator, you must also implement Iterator. When doing so, the implementation of size_hint must return the exact size of the iterator.
        The len method has a default implementation, so you usually shouldn't implement it. However, you may be able to provide a more performant implementation than the default, so overriding it in this case makes sense."#.as_bytes());
    })
}

#[bench]
fn bench_compression_65k(b: &mut test::Bencher) {
    b.iter(|| {
        compress(COMPRESSION65);
    })
}

#[ignore]
#[bench]
fn bench_compression_10_mb(b: &mut test::Bencher) {
    b.iter(|| {
        compress(COMPRESSION10MB);
    })
}

#[bench]
fn bench_decompression_small(b: &mut test::Bencher) {
    let comp = compress("To cute to die! Save the red panda!".as_bytes());
    b.iter(|| {
        decompress(&comp)
    })
}

#[bench]
fn bench_decompression_medium(b: &mut test::Bencher) {
    let comp = compress(r#"An iterator that knows its exact length.
        Many Iterators don't know how many times they will iterate, but some do. If an iterator knows how many times it can iterate, providing access to that information can be useful. For example, if you want to iterate backwards, a good start is to know where the end is.
        When implementing an ExactSizeIterator, you must also implement Iterator. When doing so, the implementation of size_hint must return the exact size of the iterator.
        The len method has a default implementation, so you usually shouldn't implement it. However, you may be able to provide a more performant implementation than the default, so overriding it in this case makes sense."#.as_bytes());
    b.iter(|| {
        decompress(&comp)
    })
}

#[bench]
fn bench_decompression_10_mb(b: &mut test::Bencher) {
    let comp = compress(COMPRESSION10MB);
    b.iter(|| {
        decompress(&comp)
    })
}

/// Test that the compressed string decompresses to the original string.
fn inverse(s: &str) {
    let compressed = compress(s.as_bytes());
    // println!("Compressed '{}' into {:?}", s, compressed);
    dbg!(&compressed);
    let decompressed = decompress(&compressed).unwrap();
    // println!("Decompressed it into {:?}", str::from_utf8(&decompressed).unwrap());
    assert_eq!(decompressed, s.as_bytes());
}

#[test]
fn yopa() {
    const COMPRESSION66K: &'static [u8] = include_bytes!("../benches/dickens.txt");
    let compressed = compress(COMPRESSION66K);
    println!("Compression Ratio {:?}", compressed.len() as f64/ COMPRESSION66K.len()  as f64);
    let _decompressed = decompress(&compressed).unwrap();
}

#[test]
fn shakespear() {
    inverse("to live or not to live");
    inverse("Love is a wonderful terrible thing");
    inverse("There is nothing either good or bad, but thinking makes it so.");
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
fn compression_works() {
    let s =r#"An iterator that knows its exact length.
        Many Iterators don't know how many times they will iterate, but some do. If an iterator knows how many times it can iterate, providing access to that information can be useful. For example, if you want to iterate backwards, a good start is to know where the end is.
        When implementing an ExactSizeIterator, you must also implement Iterator. When doing so, the implementation of size_hint must return the exact size of the iterator.
        The len method has a default implementation, so you usually shouldn't implement it. However, you may be able to provide a more performant implementation than the default, so overriding it in this case makes sense."#;

    inverse(s);

    assert!(compress(s.as_bytes()).len() < s.len());
}

#[ignore]
#[test]
fn big_compression() {
    let mut s = Vec::with_capacity(80_000000);

    for n in 0..80_000000 {
        s.push((n as u8).wrapping_mul(0xA).wrapping_add(33) ^ 0xA2);
    }

    assert_eq!(&decompress(&compress(&s)).unwrap(), &s);
}


#[cfg(test)]
mod test_compression {
    use super::*;

    fn print_ratio(text: &str, val1: usize, val2: usize) {
        println!("{:?} {:.2}", text, val1 as f32/val2 as f32);
    }

    #[test]
    fn test_comp_flex() {
        print_ratio("Ratio 1k", COMPRESSION1K.len(), compress(COMPRESSION1K).len());
        print_ratio("Ratio 34k", COMPRESSION34K.len(), compress(COMPRESSION34K).len());
    }

    mod lz4_linked {
        use super::*;
        use std::io;
        fn get_compressed_size(mut input: &[u8]) -> usize {
            let mut cache = vec![];
            let mut encoder = lz4::EncoderBuilder::new().level(2).build(&mut cache).unwrap();
            io::copy(&mut input, &mut encoder).unwrap();
            let (output, _result) = encoder.finish();
            output.len()
        }

        #[test]
        fn test_comp_lz4_linked() {
            print_ratio("Ratio 1k", COMPRESSION1K.len(), get_compressed_size(COMPRESSION1K));
            print_ratio("Ratio 34k", COMPRESSION34K.len(), get_compressed_size(COMPRESSION34K));

        }
    }

    
}