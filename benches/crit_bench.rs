extern crate criterion;

use self::criterion::*;
use lz_fear::raw::compress2;
use lz_fear::raw::decompress_raw;
use lz_fear::raw::U16Table;
use lz_fear::raw::U32Table;

const COMPRESSION1K: &'static [u8] = include_bytes!("compression_1k.txt");
const COMPRESSION34K: &'static [u8] = include_bytes!("compression_34k.txt");
const COMPRESSION65K: &'static [u8] = include_bytes!("compression_65k.txt");
const COMPRESSION66K: &'static [u8] = include_bytes!("compression_66k_JSON.txt");
const COMPRESSION10MB: &'static [u8] = include_bytes!("dickens.txt");
const COMPRESSION95K_VERY_GOOD_LOGO: &'static [u8] = include_bytes!("../logo.jpg");

const ALL: &[&[u8]] = &[
    // COMPRESSION1K as &[u8],
    COMPRESSION34K as &[u8],
    // COMPRESSION65K as &[u8],
    COMPRESSION66K as &[u8],
    // COMPRESSION10MB as &[u8],
    // COMPRESSION95K_VERY_GOOD_LOGO as &[u8],
];

fn lz4_linked_block_compress(input: &[u8]) -> Result<Vec<u8>, lzzzz::Error> {
    let mut out = Vec::new();
    lzzzz::lz4::compress_to_vec(input, &mut out, lzzzz::lz4::ACC_LEVEL_DEFAULT).unwrap();
    Ok(out)
}

fn compress_lz4_fear(input: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    if input.len() <= 0xFFFF {
        compress2(input, 0, &mut U16Table::default(), &mut buf).unwrap();
    } else {
        compress2(input, 0, &mut U32Table::default(), &mut buf).unwrap();
    }
    buf
}

fn compress_snap(input: &[u8]) -> Vec<u8> {
    snap::raw::Encoder::new().compress_vec(input).unwrap()
}

fn bench_compression_throughput(c: &mut Criterion) {
    let plot_config = PlotConfiguration::default().summary_scale(AxisScale::Linear);

    let mut group = c.benchmark_group("Compress");
    group.plot_config(plot_config);

    for input in ALL.iter() {
        let input_bytes = input.len() as u64;
        group.throughput(Throughput::Bytes(input_bytes));

        group.bench_with_input(
            BenchmarkId::new("lz4_flexx_rust", input_bytes),
            &input,
            |b, i| b.iter(|| lz4_flex::compress(&i)),
        );
        // an empty slice that the compiler can't infer the size
        let empty_vec = std::env::args()
            .skip(1000000)
            .next()
            .unwrap_or_default()
            .into_bytes();
        group.bench_with_input(
            BenchmarkId::new("lz4_flexx_rust_with_dict", input_bytes),
            &input,
            |b, i| b.iter(|| lz4_flex::compress_with_dict(&i, &empty_vec)),
        );
        group.bench_with_input(
            BenchmarkId::new("lz4_flexx_rust_master", input_bytes),
            &input,
            |b, i| b.iter(|| lz4_flex_master::compress(&i)),
        );
        // group.bench_with_input(
        //     BenchmarkId::new("lz4_redox_rust", input_bytes),
        //     &input,
        //     |b, i| b.iter(|| lz4_compress::compress(&i)),
        // );
        // group.bench_with_input(
        //     BenchmarkId::new("lz4_fear_rust", input_bytes),
        //     &input,
        //     |b, i| b.iter(|| compress_lz4_fear(&i)),
        // );

        // group.bench_with_input(BenchmarkId::new("lz4_cpp", input_bytes), &input, |b, i| {
        //     b.iter(|| lz4_linked_block_compress(&i))
        // });

        // group.bench_with_input(BenchmarkId::new("snap", input_bytes), &input, |b, i| {
        //     b.iter(|| compress_snap(&i))
        // });
    }

    group.finish();
}

pub fn decompress_fear(input: &[u8]) -> Vec<u8> {
    let mut vec = Vec::new();
    decompress_raw(input, &[], &mut vec, std::usize::MAX).unwrap();
    vec
}

pub fn decompress_snap(input: &[u8]) -> Vec<u8> {
    snap::raw::Decoder::new().decompress_vec(input).unwrap()
}

fn lz4_cpp_block_decompress(input: &[u8], decomp_len: usize) -> Result<Vec<u8>, lzzzz::Error> {
    let mut out = vec![0u8; decomp_len];
    lzzzz::lz4::decompress(input, &mut out)?;
    Ok(out)
}

fn bench_decompression_throughput(c: &mut Criterion) {
    let plot_config = PlotConfiguration::default().summary_scale(AxisScale::Linear);

    let mut group = c.benchmark_group("Decompress");
    group.plot_config(plot_config);

    for input in ALL.iter() {
        let input_bytes = input.len() as u64;

        // let comp_flex = lz4_flex::compress(&input);
        // let comp_fear_rust = compress_lz4_fear(&input);
        // let comp_rust = lz4_compress::compress(&input);

        let comp_lz4 = lz4_linked_block_compress(&input).unwrap();
        group.throughput(Throughput::Bytes(input.len() as _));

        group.bench_with_input(
            BenchmarkId::new("lz4_flexx_rust", input_bytes),
            &comp_lz4,
            |b, i| b.iter(|| lz4_flex::decompress(&i, input.len())),
        );
        // an empty slice that the compiler can't infer the size
        let empty_vec = std::env::args()
            .skip(1000000)
            .next()
            .unwrap_or_default()
            .into_bytes();
        group.bench_with_input(
            BenchmarkId::new("lz4_flexx_rust_with_dict", input_bytes),
            &comp_lz4,
            |b, i| b.iter(|| lz4_flex::decompress_with_dict(&i, input.len(), &empty_vec)),
        );
        group.bench_with_input(
            BenchmarkId::new("lz4_flexx_rust_master", input_bytes),
            &comp_lz4,
            |b, i| b.iter(|| lz4_flex_master::decompress(&i, input.len())),
        );
        // group.bench_with_input(
        //     BenchmarkId::new("lz4_redox_rust", input_bytes),
        //     &comp_lz4,
        //     |b, i| b.iter(|| lz4_compress::decompress(&i)),
        // );
        // group.bench_with_input(
        //     BenchmarkId::new("lz4_fear_rust", input_bytes),
        //     &comp_lz4,
        //     |b, i| b.iter(|| decompress_fear(&i)),
        // );

        // group.bench_with_input(
        //     BenchmarkId::new("lz4_cpp", input_bytes),
        //     &comp_lz4,
        //     |b, i| b.iter(|| lz4_cpp_block_decompress(&i, input.len())),
        // );

        // let comp_snap = compress_snap(&input);
        // group.throughput(Throughput::Bytes(comp_snap.len() as _));
        // group.bench_with_input(BenchmarkId::new("snap", input_bytes), &comp_snap, |b, i| {
        //     b.iter(|| decompress_snap(&i))
        // });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_decompression_throughput,
    bench_compression_throughput
);
criterion_main!(benches);
