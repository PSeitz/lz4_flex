#![allow(dead_code)]

#[allow(unused)]
use std::io::{Read, Write};

use binggan::plugins::*;
use binggan::*;

use lz_fear::raw::compress2;
use lz_fear::raw::decompress_raw;
use lz_fear::raw::U16Table;
use lz_fear::raw::U32Table;

const COMPRESSION1K: &[u8] = include_bytes!("compression_1k.txt");
const COMPRESSION34K: &[u8] = include_bytes!("compression_34k.txt");
const COMPRESSION65K: &[u8] = include_bytes!("compression_65k.txt");
const COMPRESSION66K: &[u8] = include_bytes!("compression_66k_JSON.txt");
const COMPRESSION10MB: &[u8] = include_bytes!("dickens.txt");
const COMPRESSION95K_VERY_GOOD_LOGO: &[u8] = include_bytes!("../logo.jpg");

#[global_allocator]
//pub static GLOBAL: PeakMemAlloc<std::alloc::System> = PeakMemAlloc::new(std::alloc::System);
pub static GLOBAL: PeakMemAlloc<jemallocator::Jemalloc> = PeakMemAlloc::new(jemallocator::Jemalloc);

const ALL: &[&[u8]] = &[
    COMPRESSION1K as &[u8],
    COMPRESSION34K as &[u8],
    COMPRESSION65K as &[u8],
    COMPRESSION66K as &[u8],
    COMPRESSION10MB as &[u8],
    COMPRESSION95K_VERY_GOOD_LOGO as &[u8],
];

fn main() {
    #[cfg(feature = "frame")]
    {
        let data_sets = get_frame_datasets();
        frame_decompress(&data_sets);
        frame_compress(InputGroup::new_with_inputs(data_sets));
    }

    let named_data = ALL
        .iter()
        .map(|data| (data.len().to_string(), data.to_vec()))
        .collect();
    block_compress(InputGroup::new_with_inputs(named_data));
    block_decompress();
}

#[cfg(feature = "frame")]
fn frame_decompress(data_sets: &[(String, Vec<u8>)]) {
    let mut runner = BenchRunner::with_name("frame_decompress");
    runner
        .add_plugin(PerfCounterPlugin::default())
        .add_plugin(PeakMemAllocPlugin::new(&GLOBAL));
    for (name, data_set) in data_sets {
        let compressed_independent = lz4_cpp_frame_compress(data_set, true).unwrap();
        let compressed_linked = lz4_cpp_frame_compress(data_set, false).unwrap();
        let comp_snap = compress_snap_frame(data_set);
        let mut group = runner.new_group();
        group.set_name(name);
        group.set_input_size(data_set.len());

        group.register_with_input("lz4 flex independent", &compressed_independent, move |i| {
            black_box(lz4_flex_frame_decompress(i).unwrap());
            Some(())
        });
        group.register_with_input("lz4 c90 independent", &compressed_independent, move |i| {
            black_box(lz4_cpp_frame_decompress(i).unwrap());
            Some(())
        });
        group.register_with_input("lz4 flex linked", &compressed_linked, move |i| {
            black_box(lz4_flex_frame_decompress(i).unwrap());
            Some(())
        });
        group.register_with_input("lz4 c90 linked", &compressed_linked, move |i| {
            black_box(lz4_cpp_frame_decompress(i).unwrap());
            Some(())
        });
        group.register_with_input("snap", &comp_snap, move |i| {
            black_box(decompress_snap_frame(i));
            Some(())
        });

        group.run();
    }
}

#[cfg(feature = "frame")]
fn frame_compress(mut runner: InputGroup<Vec<u8>, usize>) {
    runner.set_name("frame_compress");
    runner.add_plugin(PeakMemAllocPlugin::new(&GLOBAL));

    runner.throughput(|data| data.len());
    runner.register("lz4 flex independent", move |i| {
        let mut frame_info = lz4_flex::frame::FrameInfo::new();
        frame_info.block_size = lz4_flex::frame::BlockSize::Max256KB;
        frame_info.block_mode = lz4_flex::frame::BlockMode::Independent;
        let out = black_box(lz4_flex_frame_compress_with(frame_info, i).unwrap());
        Some(out.len())
    });
    runner.register("lz4 c90 indep", move |i| {
        let out = black_box(lz4_cpp_frame_compress(i, true).unwrap());
        Some(out.len())
    });
    runner.register("lz4 flex linked", move |i| {
        let mut frame_info = lz4_flex::frame::FrameInfo::new();
        frame_info.block_size = lz4_flex::frame::BlockSize::Max256KB;
        frame_info.block_mode = lz4_flex::frame::BlockMode::Linked;
        let out = black_box(lz4_flex_frame_compress_with(frame_info, i).unwrap());
        Some(out.len())
    });
    runner.register("lz4 c90 linked", move |i| {
        let out = black_box(lz4_cpp_frame_compress(i, false).unwrap());
        Some(out.len())
    });
    runner.register("snap", move |i| {
        let out = compress_snap_frame(i);
        Some(out.len())
    });

    runner.run();
}

fn block_compress(mut runner: InputGroup<Vec<u8>, usize>) {
    runner.set_name("block_compress");
    // Set the peak mem allocator. This will enable peak memory reporting.
    runner.add_plugin(PeakMemAllocPlugin::new(&GLOBAL));

    runner.throughput(|data| data.len());
    runner.register("lz4 flex", move |i| {
        let out = black_box(lz4_flex::compress(i));
        Some(out.len())
    });
    runner.register("lz4 c90", move |i| {
        let out = black_box(lz4_cpp_block_compress(i).unwrap());
        Some(out.len())
    });
    runner.register("snap", move |i| {
        let out = black_box(compress_snap(i));
        Some(out.len())
    });

    runner.run();
}

fn block_decompress() {
    let mut runner = BenchRunner::with_name("block_decompress");
    // Set the peak mem allocator. This will enable peak memory reporting.
    runner.add_plugin(PeakMemAllocPlugin::new(&GLOBAL));
    for data_uncomp in ALL {
        let comp_lz4 = lz4_cpp_block_compress(data_uncomp).unwrap();
        let bundle = (comp_lz4, data_uncomp.len());

        let name = data_uncomp.len().to_string();
        let mut group = runner.new_group();
        group.set_name(name.clone());
        group.set_input_size(data_uncomp.len());

        group.register_with_input("lz4 flex", &bundle, move |i| {
            let size = black_box(lz4_flex::decompress(&i.0, i.1).unwrap());
            Some(size.len())
        });
        group.register_with_input("lz4 c90", &bundle, move |i| {
            let size = black_box(lz4_cpp_block_decompress(&i.0, i.1).unwrap());
            Some(size.len())
        });

        group.run();
    }
}

fn get_frame_datasets() -> Vec<(String, Vec<u8>)> {
    let paths = [
        "compression_1k.txt",
        "dickens.txt",
        "hdfs.json",
        "reymont.pdf",
        "xml_collection.xml",
    ];
    paths
        .iter()
        .map(|path| {
            let path_buf = std::path::Path::new("benches").join(path);
            let mut file = std::fs::File::open(path_buf).unwrap();
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).unwrap();
            (path.to_string(), buf)
        })
        .collect()
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

fn decompress_lz4_fear(input: &[u8]) -> Vec<u8> {
    let mut vec = Vec::new();
    decompress_raw(input, &[], &mut vec, usize::MAX).unwrap();
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
    let mut comp = lzzzz::lz4f::WriteCompressor::new(Vec::new(), pref).unwrap();
    comp.write_all(input).unwrap();
    let out = comp.into_inner();

    Ok(out)
}

#[cfg(feature = "frame")]
fn lz4_cpp_frame_decompress(mut input: &[u8]) -> Result<Vec<u8>, lzzzz::lz4f::Error> {
    let mut r = lzzzz::lz4f::ReadDecompressor::new(&mut input)?;
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).unwrap();

    Ok(buf)
}

#[cfg(feature = "frame")]
pub fn lz4_flex_frame_compress_with(
    frame_info: lz4_flex::frame::FrameInfo,
    input: &[u8],
) -> Result<Vec<u8>, lz4_flex::frame::Error> {
    let buffer = Vec::new();
    let mut enc = lz4_flex::frame::FrameEncoder::with_frame_info(frame_info, buffer);
    enc.write_all(input)?;
    enc.finish()
}

#[cfg(feature = "frame")]
pub fn lz4_flex_frame_decompress(input: &[u8]) -> Result<Vec<u8>, lz4_flex::frame::Error> {
    let mut de = lz4_flex::frame::FrameDecoder::new(input);
    let mut out = Vec::new();
    de.read_to_end(&mut out)?;
    Ok(out)
}

//pub fn lz4_flex_master_frame_compress_with(
//frame_info: lz4_flex_master::frame::FrameInfo,
//input: &[u8],
//) -> Result<Vec<u8>, lz4_flex_master::frame::Error> {
//let buffer = Vec::new();
//let mut enc = lz4_flex_master::frame::FrameEncoder::with_frame_info(frame_info, buffer);
//enc.write_all(input)?;
//Ok(enc.finish()?)
//}

//pub fn lz4_flex_master_frame_decompress(
//input: &[u8],
//) -> Result<Vec<u8>, lz4_flex_master::frame::Error> {
//let mut de = lz4_flex_master::frame::FrameDecoder::new(input);
//let mut out = Vec::new();
//de.read_to_end(&mut out)?;
//Ok(out)
//}
