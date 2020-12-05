// use quickbench::bench_gen_env;

// const COMPRESSION1K: &'static [u8] = include_bytes!("compression_1k.txt");
// const COMPRESSION34K: &'static [u8] = include_bytes!("compression_34k.txt");
// const COMPRESSION65K: &'static [u8] = include_bytes!("compression_65k.txt");
// const COMPRESSION66K: &'static [u8] = include_bytes!("compression_66k_JSON.txt");
// const COMPRESSION95K_VERY_GOOD_LOGO: &'static [u8] = include_bytes!("../logo.jpg");

// fn main() {

//     // let inputs = [COMPRESSION1K
//     //     COMPRESSION34K
//     //     COMPRESSION65K
//     //     COMPRESSION66K
//     //     COMPRESSION95K_VERY_GOOD_LOGO];

//     println!("{}", bench_gen_env("COMPRESSION1K", COMPRESSION1K.len(),  || &COMPRESSION1K, |xs| lz4_flex::compress(&xs)));
//     println!("{}", bench_gen_env("COMPRESSION34K", COMPRESSION34K.len(),  || &COMPRESSION34K, |xs| lz4_flex::compress(&xs)));
//     println!("{}", bench_gen_env("COMPRESSION65K", COMPRESSION65K.len(),  || &COMPRESSION65K, |xs| lz4_flex::compress(&xs)));
//     println!("{}", bench_gen_env("COMPRESSION66K", COMPRESSION66K.len(),  || &COMPRESSION66K, |xs| lz4_flex::compress(&xs)));
//     println!("{}", bench_gen_env("COMPRESSION95K_VERY_GOOD_LOGO", COMPRESSION95K_VERY_GOOD_LOGO.len(),  || &COMPRESSION95K_VERY_GOOD_LOGO, |xs| lz4_flex::compress(&xs)));

//     let compression1_k_compressed = lz4_flex::compress_prepend_size(&COMPRESSION1K);
//     let compression34_k_compressed = lz4_flex::compress_prepend_size(&COMPRESSION34K);
//     let compression65_k_compressed = lz4_flex::compress_prepend_size(&COMPRESSION65K);
//     let compression66_k_compressed = lz4_flex::compress_prepend_size(&COMPRESSION66K);
//     let compression95_k_very_good_logo_compressed = lz4_flex::compress_prepend_size(&COMPRESSION95K_VERY_GOOD_LOGO);

//     println!("{}", bench_gen_env("DECOMPRESSION1K", compression1_k_compressed.len(),  || &compression1_k_compressed, |xs| lz4_flex::decompress_size_prepended(&xs)));
//     println!("{}", bench_gen_env("DECOMPRESSION34K", compression34_k_compressed.len(),  || &compression34_k_compressed, |xs| lz4_flex::decompress_size_prepended(&xs)));
//     println!("{}", bench_gen_env("DECOMPRESSION65K", compression65_k_compressed.len(),  || &compression65_k_compressed, |xs| lz4_flex::decompress_size_prepended(&xs)));
//     println!("{}", bench_gen_env("DECOMPRESSION66K", compression66_k_compressed.len(),  || &compression66_k_compressed, |xs| lz4_flex::decompress_size_prepended(&xs)));
//     println!("{}", bench_gen_env("DECOMPRESSION95K_VERY_GOOD_LOGO", compression95_k_very_good_logo_compressed.len(),  || &compression95_k_very_good_logo_compressed, |xs| lz4_flex::decompress_size_prepended(&xs)));
// }

// #![feature(test)]
// extern crate test;

// const COMPRESSION1K: &'static [u8] = include_bytes!("compression_1k.txt");
// const COMPRESSION34K: &'static [u8] = include_bytes!("compression_34k.txt");
// const COMPRESSION65K: &'static [u8] = include_bytes!("compression_65k.txt");
// const COMPRESSION66K: &'static [u8] = include_bytes!("compression_66k_JSON.txt");
// const COMPRESSION95K_VERY_GOOD_LOGO: &'static [u8] = include_bytes!("../logo.jpg");

// #[bench]
// fn bench_compress_lz4_1k(b: &mut test::Bencher) {
//     b.iter(|| lz4_flex::compress(&COMPRESSION1K))
// }
// #[bench]
// fn bench_compress_lz4_34_k_text(b: &mut test::Bencher) {
//     b.iter(|| lz4_flex::compress(&COMPRESSION34K))
// }
// #[bench]
// fn bench_compress_lz4_65_k_text(b: &mut test::Bencher) {
//     b.iter(|| lz4_flex::compress(&COMPRESSION65K))
// }
// #[bench]
// fn bench_compress_lz4_66_k_json(b: &mut test::Bencher) {
//     b.iter(|| lz4_flex::compress(&COMPRESSION66K))
// }
// #[bench]
// fn bench_compress_lz4_95_k_very_good_logo(b: &mut test::Bencher) {
//     b.iter(|| lz4_flex::compress(&COMPRESSION95K_VERY_GOOD_LOGO))
// }

// #[bench]
// fn bench_decompress_lz4_1k(b: &mut test::Bencher) {
//     let comp = lz4_flex::compress(&COMPRESSION1K);
//     b.iter(|| lz4_flex::decompress(&comp, COMPRESSION1K.len()))
// }
// #[bench]
// fn bench_decompress_lz4_34_k_text(b: &mut test::Bencher) {
//     let comp = lz4_flex::compress(&COMPRESSION34K);
//     b.iter(|| lz4_flex::decompress(&comp, COMPRESSION34K.len()))
// }
// #[bench]
// fn bench_decompress_lz4_65_k_text(b: &mut test::Bencher) {
//     let comp = lz4_flex::compress(&COMPRESSION65K);
//     b.iter(|| lz4_flex::decompress(&comp, COMPRESSION65K.len()))
// }
// #[bench]
// fn bench_decompress_lz4_66_k_json(b: &mut test::Bencher) {
//     let comp = lz4_flex::compress(&COMPRESSION66K);
//     b.iter(|| lz4_flex::decompress(&comp, COMPRESSION66K.len()))
// }
// #[bench]
// fn bench_decompress_lz4_95_k_very_good_logo(b: &mut test::Bencher) {
//     let comp = lz4_flex::compress(&COMPRESSION95K_VERY_GOOD_LOGO);
//     b.iter(|| lz4_flex::decompress(&comp, COMPRESSION95K_VERY_GOOD_LOGO.len()))
// }
