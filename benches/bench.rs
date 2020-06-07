#![feature(test)]
extern crate test;

const COMPRESSION1K: &'static [u8] = include_bytes!("compression_1k.txt");
const COMPRESSION34K: &'static [u8] = include_bytes!("compression_34k.txt");
const COMPRESSION65K: &'static [u8] = include_bytes!("compression_65k.txt");
const COMPRESSION66K: &'static [u8] = include_bytes!("compression_66k_JSON.txt");
const COMPRESSION95K_VERY_GOOD_LOGO: &'static [u8] = include_bytes!("../logo.jpg");

#[bench]
fn bench_compress_lz4_1k(b: &mut test::Bencher) {
    b.iter(|| lz4_flex::compress(&COMPRESSION1K))
}
#[bench]
fn bench_compress_lz4_34_k_text(b: &mut test::Bencher) {
    b.iter(|| lz4_flex::compress(&COMPRESSION34K))
}
#[bench]
fn bench_compress_lz4_65_k_text(b: &mut test::Bencher) {
    b.iter(|| lz4_flex::compress(&COMPRESSION65K))
}
#[bench]
fn bench_compress_lz4_66_k_json(b: &mut test::Bencher) {
    b.iter(|| lz4_flex::compress(&COMPRESSION66K))
}
#[bench]
fn bench_compress_lz4_95_k_very_good_logo(b: &mut test::Bencher) {
    b.iter(|| lz4_flex::compress(&COMPRESSION95K_VERY_GOOD_LOGO))
}

#[bench]
fn bench_decompress_lz4_1k(b: &mut test::Bencher) {
    let comp = lz4_flex::compress(&COMPRESSION1K);
    b.iter(|| lz4_flex::decompress(&comp, COMPRESSION1K.len()))
}
#[bench]
fn bench_decompress_lz4_34_k_text(b: &mut test::Bencher) {
    let comp = lz4_flex::compress(&COMPRESSION34K);
    b.iter(|| lz4_flex::decompress(&comp, COMPRESSION34K.len()))
}
#[bench]
fn bench_decompress_lz4_65_k_text(b: &mut test::Bencher) {
    let comp = lz4_flex::compress(&COMPRESSION65K);
    b.iter(|| lz4_flex::decompress(&comp, COMPRESSION65K.len()))
}
#[bench]
fn bench_decompress_lz4_66_k_json(b: &mut test::Bencher) {
    let comp = lz4_flex::compress(&COMPRESSION66K);
    b.iter(|| lz4_flex::decompress(&comp, COMPRESSION66K.len()))
}
#[bench]
fn bench_decompress_lz4_95_k_very_good_logo(b: &mut test::Bencher) {
    let comp = lz4_flex::compress(&COMPRESSION95K_VERY_GOOD_LOGO);
    b.iter(|| lz4_flex::decompress(&comp, COMPRESSION95K_VERY_GOOD_LOGO.len()))
}
