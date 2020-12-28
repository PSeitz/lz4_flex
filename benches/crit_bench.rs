extern crate criterion;

use self::criterion::*;
use lz4::block::compress as lz4_linked_block_compress;
use lz_fear::raw::compress2;
use lz_fear::raw::decompress_raw;
use lz_fear::raw::U16Table;
use lz_fear::raw::U32Table;

const COMPRESSION1K: &'static [u8] = include_bytes!("compression_1k.txt");
const COMPRESSION34K: &'static [u8] = include_bytes!("compression_34k.txt");
const COMPRESSION65K: &'static [u8] = include_bytes!("compression_65k.txt");
const COMPRESSION66K: &'static [u8] = include_bytes!("compression_66k_JSON.txt");
const COMPRESSION95K_VERY_GOOD_LOGO: &'static [u8] = include_bytes!("../logo.jpg");

const ALL: &[&[u8]] = &[
    COMPRESSION1K as &[u8],
    COMPRESSION34K as &[u8],
    COMPRESSION65K as &[u8],
    COMPRESSION66K as &[u8],
    COMPRESSION95K_VERY_GOOD_LOGO as &[u8],
];

fn compress_lz4_fear(input: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    if input.len() <= 0xFFFF {
        compress2(input, 0, &mut U16Table::default(), &mut buf).unwrap();
    } else {
        compress2(input, 0, &mut U32Table::default(), &mut buf).unwrap();
    }
    buf
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
        group.bench_with_input(
            BenchmarkId::new("lz4_redox_rust", input_bytes),
            &input,
            |b, i| b.iter(|| lz4_compress::compress(&i)),
        );
        group.bench_with_input(
            BenchmarkId::new("lz4_fear_rust", input_bytes),
            &input,
            |b, i| b.iter(|| compress_lz4_fear(&i)),
        );

        group.bench_with_input(BenchmarkId::new("lz4_cpp", input_bytes), &input, |b, i| {
            b.iter(|| lz4_linked_block_compress(&i, None, false))
        });
    }

    group.finish();
}

pub fn decompress_fear(input: &[u8]) -> Vec<u8> {
    let mut vec = Vec::new();
    decompress_raw(input, &[], &mut vec, std::usize::MAX).unwrap();
    vec
}

fn bench_decompression_throughput(c: &mut Criterion) {
    let plot_config = PlotConfiguration::default().summary_scale(AxisScale::Linear);

    let mut group = c.benchmark_group("Decompress");
    group.plot_config(plot_config);

    for input in ALL.iter() {
        let input_bytes = input.len() as u64;
        group.throughput(Throughput::Bytes(input_bytes));

        // let comp_flex = lz4_flex::compress(&input);
        // let comp_fear_rust = compress_lz4_fear(&input);
        // let comp_rust = lz4_compress::compress(&input);

        let comp_lz4 = lz4::block::compress(&input, None, false).unwrap();
        let comp_lz4_fl = lz4_flex::compress(&input);

        group.bench_with_input(
            BenchmarkId::new("lz4_flexx_rust", input_bytes),
            &comp_lz4_fl,
            |b, i| b.iter(|| lz4_flex::decompress(&i, input.len())),
        );
        group.bench_with_input(
            BenchmarkId::new("lz4_redox_rust", input_bytes),
            &comp_lz4,
            |b, i| b.iter(|| lz4_compress::decompress(&i)),
        );
        group.bench_with_input(
            BenchmarkId::new("lz4_fear_rust", input_bytes),
            &comp_lz4,
            |b, i| b.iter(|| decompress_fear(&i)),
        );

        group.bench_with_input(
            BenchmarkId::new("lz4_cpp", input_bytes),
            &comp_lz4,
            |b, i| {
                b.iter(|| {
                    let output = lz4::block::decompress(&i, Some(input.len() as i32));
                    output
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_decompression_throughput,
    bench_compression_throughput
);
criterion_main!(benches);
