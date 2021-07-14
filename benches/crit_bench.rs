#![allow(dead_code)]
extern crate criterion;

use std::io::{Read, Write};

use self::criterion::*;

const COMPRESSION1K: &'static [u8] = include_bytes!("compression_1k.txt");
const COMPRESSION34K: &'static [u8] = include_bytes!("compression_34k.txt");
const COMPRESSION65K: &'static [u8] = include_bytes!("compression_65k.txt");
const COMPRESSION66K: &'static [u8] = include_bytes!("compression_66k_JSON.txt");
// const COMPRESSION10MB: &'static [u8] = include_bytes!("dickens.txt");
// const COMPRESSION95K_VERY_GOOD_LOGO: &'static [u8] = include_bytes!("../logo.jpg");

const ALL: &[&[u8]] = &[
    COMPRESSION1K as &[u8],
    COMPRESSION34K as &[u8],
    COMPRESSION65K as &[u8],
    COMPRESSION66K as &[u8],
    // COMPRESSION10MB as &[u8],
    // COMPRESSION95K_VERY_GOOD_LOGO as &[u8],
];

fn compress_lz4_fear(input: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    if input.len() <= 0xFFFF {
        lz_fear::raw::compress2(input, 0, &mut lz_fear::raw::U16Table::default(), &mut buf)
            .unwrap();
    } else {
        lz_fear::raw::compress2(input, 0, &mut lz_fear::raw::U32Table::default(), &mut buf)
            .unwrap();
    }
    buf
}

fn decompress_lz4_fear(input: &[u8]) -> Vec<u8> {
    let mut vec = Vec::new();
    lz_fear::raw::decompress_raw(input, &[], &mut vec, std::usize::MAX).unwrap();
    vec
}

fn compress_snap(input: &[u8]) -> Vec<u8> {
    snap::raw::Encoder::new().compress_vec(input).unwrap()
}

fn decompress_snap(input: &[u8]) -> Vec<u8> {
    snap::raw::Decoder::new().decompress_vec(input).unwrap()
}

#[cfg(feature = "frame")]
fn compress_snap_frame(input: &[u8]) -> Vec<u8> {
    let mut fe = snap::write::FrameEncoder::new(Vec::new());
    fe.write_all(input).unwrap();
    fe.into_inner().unwrap()
}

#[cfg(feature = "frame")]
fn decompress_snap_frame(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut fe = snap::read::FrameDecoder::new(input);
    fe.read_to_end(&mut out).unwrap();
    out
}

fn lz4_cpp_block_decompress(input: &[u8], decomp_len: usize) -> Result<Vec<u8>, lzzzz::Error> {
    let mut out = vec![0u8; decomp_len];
    lzzzz::lz4::decompress(input, &mut out)?;
    Ok(out)
}

fn lz4_cpp_block_compress(input: &[u8]) -> Result<Vec<u8>, lzzzz::Error> {
    let mut out = Vec::new();
    lzzzz::lz4::compress_to_vec(input, &mut out, lzzzz::lz4::ACC_LEVEL_DEFAULT).unwrap();
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
        .block_size(lzzzz::lz4f::BlockSize::Max64KB)
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
) -> Result<Vec<u8>, lz4_flex::frame::Error> {
    let buffer = Vec::new();
    let mut enc = lz4_flex::frame::FrameEncoder::with_frame_info(frame_info, buffer);
    enc.write_all(input)?;
    Ok(enc.finish()?)
}

#[cfg(feature = "frame")]
pub fn lz4_flex_frame_decompress(input: &[u8]) -> Result<Vec<u8>, lz4_flex::frame::Error> {
    let mut de = lz4_flex::frame::FrameDecoder::new(input);
    let mut out = Vec::new();
    de.read_to_end(&mut out)?;
    Ok(out)
}

pub fn lz4_flex_master_frame_compress_with(
    frame_info: lz4_flex_master::frame::FrameInfo,
    input: &[u8],
) -> Result<Vec<u8>, lz4_flex_master::frame::Error> {
    let buffer = Vec::new();
    let mut enc = lz4_flex_master::frame::FrameEncoder::with_frame_info(frame_info, buffer);
    enc.write_all(input)?;
    Ok(enc.finish()?)
}

pub fn lz4_flex_master_frame_decompress(
    input: &[u8],
) -> Result<Vec<u8>, lz4_flex_master::frame::Error> {
    let mut de = lz4_flex_master::frame::FrameDecoder::new(input);
    let mut out = Vec::new();
    de.read_to_end(&mut out)?;
    Ok(out)
}

fn bench_block_compression_throughput(c: &mut Criterion) {
    let plot_config = PlotConfiguration::default().summary_scale(AxisScale::Linear);

    let mut group = c.benchmark_group("BlockCompress");
    group.plot_config(plot_config);

    for input in ALL.iter() {
        let input_bytes = input.len() as u64;
        group.throughput(Throughput::Bytes(input_bytes));

        group.bench_with_input(
            BenchmarkId::new("lz4_flex_rust", input_bytes),
            &input,
            |b, i| b.iter(|| lz4_flex::compress(&i)),
        );
        // an empty slice that the compiler can't infer the size
        //let empty_vec = std::env::args()
        //.skip(1000000)
        //.next()
        //.unwrap_or_default()
        //.into_bytes();
        //group.bench_with_input(
        //BenchmarkId::new("lz4_flex_rust_with_dict", input_bytes),
        //&input,
        //|b, i| b.iter(|| lz4_flex::block::compress_with_dict(&i, &empty_vec)),
        //);
        group.bench_with_input(
            BenchmarkId::new("lz4_flex_rust_master", input_bytes),
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
        //     b.iter(|| lz4_cpp_block_compress(&i))
        // });

        // group.bench_with_input(BenchmarkId::new("snap", input_bytes), &input, |b, i| {
        //     b.iter(|| compress_snap(&i))
        // });
    }

    group.finish();
}

fn bench_block_decompression_throughput(c: &mut Criterion) {
    let plot_config = PlotConfiguration::default().summary_scale(AxisScale::Linear);

    let mut group = c.benchmark_group("BlockDecompress");
    group.plot_config(plot_config);

    for input in ALL.iter() {
        let input_bytes = input.len() as u64;

        let comp_lz4 = lz4_cpp_block_compress(&input).unwrap();
        group.throughput(Throughput::Bytes(input.len() as _));

        group.bench_with_input(
            BenchmarkId::new("lz4_flex_rust", input_bytes),
            &comp_lz4,
            |b, i| b.iter(|| lz4_flex::decompress(&i, input.len())),
        );
        // an empty slice that the compiler can't infer the size
        //let empty_vec = std::env::args()
        //.skip(1000000)
        //.next()
        //.unwrap_or_default()
        //.into_bytes();
        //group.bench_with_input(
        //BenchmarkId::new("lz4_flex_rust_with_dict", input_bytes),
        //&comp_lz4,
        //|b, i| b.iter(|| lz4_flex::block::decompress_with_dict(&i, input.len(), &empty_vec)),
        //);
        group.bench_with_input(
            BenchmarkId::new("lz4_flex_rust_master", input_bytes),
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
        //     |b, i| b.iter(|| decompress_lz4_fear(&i)),
        // );

        // group.bench_with_input(
        //     BenchmarkId::new("lz4_cpp", input_bytes),
        //     &comp_lz4,
        //     |b, i| b.iter(|| lz4_cpp_block_decompress(&i, input.len())),
        // );

        // let comp_snap = compress_snap(&input);
        // group.bench_with_input(BenchmarkId::new("snap", input_bytes), &comp_snap, |b, i| {
        //     b.iter(|| decompress_snap(&i))
        // });
    }

    group.finish();
}

#[cfg(feature = "frame")]
fn bench_frame_decompression_throughput(c: &mut Criterion) {
    let plot_config = PlotConfiguration::default().summary_scale(AxisScale::Linear);

    let mut group = c.benchmark_group("FrameDecompress");
    group.plot_config(plot_config);

    for input in ALL.iter() {
        let input_bytes = input.len() as u64;

        let comp_lz4_indep = lz4_cpp_frame_compress(&input, true).unwrap();
        let comp_lz4_linked = lz4_cpp_frame_compress(&input, false).unwrap();
        group.throughput(Throughput::Bytes(input.len() as _));

        group.bench_with_input(
            BenchmarkId::new("lz4_flex_rust_indep", input_bytes),
            &comp_lz4_indep,
            |b, i| b.iter(|| lz4_flex_frame_decompress(&i)),
        );
        // group.bench_with_input(
        //     BenchmarkId::new("lz4_flex_rust_linked", input_bytes),
        //     &comp_lz4_linked,
        //     |b, i| b.iter(|| lz4_flex_frame_decompress(&i)),
        // );
        group.bench_with_input(
            BenchmarkId::new("lz4_flex_master_rust_indep", input_bytes),
            &comp_lz4_indep,
            |b, i| b.iter(|| lz4_flex_master_frame_decompress(&i)),
        );
        // group.bench_with_input(
        //     BenchmarkId::new("lz4_flex_master_rust_linked", input_bytes),
        //     &comp_lz4_linked,
        //     |b, i| b.iter(|| lz4_flex_master_frame_decompress(&i)),
        // );

        // group.bench_with_input(
        //     BenchmarkId::new("lz4_cpp_indep", input_bytes),
        //     &comp_lz4_indep,
        //     |b, i| b.iter(|| lz4_cpp_frame_decompress(&i)),
        // );
        // group.bench_with_input(
        //     BenchmarkId::new("lz4_cpp_linked", input_bytes),
        //     &comp_lz4_linked,
        //     |b, i| b.iter(|| lz4_cpp_frame_decompress(&i)),
        // );

        // let comp_snap = compress_snap_frame(&input);
        // group.bench_with_input(BenchmarkId::new("snap", input_bytes), &comp_snap, |b, i| {
        //     b.iter(|| decompress_snap_frame(&i))
        // });
    }

    group.finish();
}

#[cfg(feature = "frame")]
fn bench_frame_compression_throughput(c: &mut Criterion) {
    let plot_config = PlotConfiguration::default().summary_scale(AxisScale::Linear);

    let mut group = c.benchmark_group("FrameCompress");
    group.plot_config(plot_config);

    for input in ALL.iter() {
        let input_bytes = input.len() as u64;
        group.throughput(Throughput::Bytes(input_bytes));

        group.bench_with_input(
            BenchmarkId::new("lz4_flex_rust_indep", input_bytes),
            &input,
            |b, i| {
                b.iter(|| {
                    let mut frame_info = lz4_flex::frame::FrameInfo::new();
                    frame_info.block_mode = lz4_flex::frame::BlockMode::Independent;
                    lz4_flex_frame_compress_with(frame_info, i)
                })
            },
        );
        // group.bench_with_input(
        //     BenchmarkId::new("lz4_flex_rust_linked", input_bytes),
        //     &input,
        //     |b, i| {
        //         b.iter(|| {
        //             let mut frame_info = lz4_flex::frame::FrameInfo::new();
        //             frame_info.block_mode = lz4_flex::frame::BlockMode::Linked;
        //             lz4_flex_frame_compress_with(frame_info, i)
        //         })
        //     },
        // );

        group.bench_with_input(
            BenchmarkId::new("lz4_flex_master_rust_indep", input_bytes),
            &input,
            |b, i| {
                b.iter(|| {
                    let mut frame_info = lz4_flex_master::frame::FrameInfo::new();
                    frame_info.block_mode = lz4_flex_master::frame::BlockMode::Independent;
                    lz4_flex_master_frame_compress_with(frame_info, i)
                })
            },
        );
        // group.bench_with_input(
        //     BenchmarkId::new("lz4_flex_master_rust_linked", input_bytes),
        //     &input,
        //     |b, i| {
        //         b.iter(|| {
        //             let mut frame_info = lz4_flex_master::frame::FrameInfo::new();
        //             frame_info.block_mode = lz4_flex_master::frame::BlockMode::Linked;
        //             lz4_flex_master_frame_compress_with(frame_info, i)
        //         })
        //     },
        // );

        // group.bench_with_input(
        //     BenchmarkId::new("lz4_cpp_indep", input_bytes),
        //     &input,
        //     |b, i| b.iter(|| lz4_cpp_frame_compress(i, true)),
        // );
        // group.bench_with_input(
        //     BenchmarkId::new("lz4_cpp_linked", input_bytes),
        //     &input,
        //     |b, i| b.iter(|| lz4_cpp_frame_compress(i, false)),
        // );

        // group.bench_with_input(BenchmarkId::new("snap", input_bytes), &input, |b, i| {
        //     b.iter(|| compress_snap_frame(i))
        // });
    }

    group.finish();
}

criterion_group!(
    block_benches,
    bench_block_decompression_throughput,
    bench_block_compression_throughput,
);

#[cfg(feature = "frame")]
criterion_group!(
    frame_benches,
    bench_frame_decompression_throughput,
    bench_frame_compression_throughput,
);

#[cfg(not(feature = "frame"))]
criterion_main!(block_benches);

#[cfg(feature = "frame")]
criterion_main!(block_benches, frame_benches);
