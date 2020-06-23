// extern crate criterion;

// use self::criterion::*;
// use lz4::block::compress as lz4_linked_block_compress;

// const COMPRESSION1K: &'static [u8] = include_bytes!("compression_1k.txt");
// const COMPRESSION34K: &'static [u8] = include_bytes!("compression_34k.txt");
// const COMPRESSION65K: &'static [u8] = include_bytes!("compression_65k.txt");
// const COMPRESSION66K: &'static [u8] = include_bytes!("compression_66k_JSON.txt");
// const COMPRESSION95K_VERY_GOOD_LOGO: &'static [u8] = include_bytes!("../logo.jpg");

// const ALL: &[&[u8]] = &[
//     COMPRESSION1K as &[u8],
//     COMPRESSION34K as &[u8],
//     COMPRESSION65K as &[u8],
//     COMPRESSION66K as &[u8],
//     COMPRESSION95K_VERY_GOOD_LOGO as &[u8],
// ];

// fn bench_compression_throughput(c: &mut Criterion) {
//     let mut group = c.benchmark_group("Compress");

//     for input in ALL.iter() {
//         let input_bytes = input.len() as u64;
//         group.throughput(Throughput::Bytes(input_bytes));

//         group.bench_with_input(
//             BenchmarkId::new("lz4_flexx", input_bytes),
//             &input,
//             |b, i| b.iter(|| lz4_flex::compress(&i)),
//         );
//         group.bench_with_input(BenchmarkId::new("lz4_rust", input_bytes), &input, |b, i| {
//             b.iter(|| lz4_compress::compress(&i))
//         });

//         group.bench_with_input(
//             BenchmarkId::new("lz4_linked", input_bytes),
//             &input,
//             |b, i| b.iter(|| lz4_linked_block_compress(&i, None, false)),
//         );
//     }

//     group.finish();
// }

// fn bench_decompression_throughput(c: &mut Criterion) {
//     let mut group = c.benchmark_group("Decompress");

//     for input in ALL.iter() {
//         let input_bytes = input.len() as u64;
//         group.throughput(Throughput::Bytes(input_bytes));

//         let comp_flex = lz4_flex::compress(&input);
//         let comp2 = lz4_compress::compress(&input);

//         let comp_lz4 = lz4::block::compress(&input, None, true).unwrap();

//         group.bench_with_input(
//             BenchmarkId::new("lz4_flexx", input_bytes),
//             &comp_flex,
//             |b, i| b.iter(|| lz4_flex::decompress(&i, input.len())),
//         );
//         group.bench_with_input(BenchmarkId::new("lz4_rust", input_bytes), &comp2, |b, i| {
//             b.iter(|| lz4_compress::decompress(&i))
//         });

//         group.bench_with_input(
//             BenchmarkId::new("lz4_linked", input_bytes),
//             &comp_lz4,
//             |b, i| {
//                 b.iter(|| {
//                     let output = lz4::block::decompress(&i, None);
//                     output
//                 })
//             },
//         );
//     }

//     group.finish();
// }

// criterion_group!(
//     benches,
//     bench_decompression_throughput,
//     bench_compression_throughput
// );
// criterion_main!(benches);
