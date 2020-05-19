#[macro_use] extern crate criterion;
use std::io::{Read, Write};
use self::criterion::*;
use lz4::block::{compress as lz4_linked_block_compress,decompress as lz4_linked_block_decompress};
use std::io;
// use lz4_flex::{decompress, decompress_into, compress, compress_into};


const COMPRESSION1K: &'static [u8] = include_bytes!("compression_1k.txt");
const COMPRESSION34K: &'static [u8] = include_bytes!("compression_34k.txt");
const COMPRESSION65K: &'static [u8] = include_bytes!("compression_65k.txt");
const COMPRESSION66K: &'static [u8] = include_bytes!("compression_66k_JSON.txt");
const COMPRESSION10MB: &'static [u8] = include_bytes!("dickens.txt");

const ALL: &[&[u8]] = &[COMPRESSION1K as &[u8], COMPRESSION34K as &[u8], COMPRESSION65K as &[u8], COMPRESSION66K as &[u8]];
// const ALL: [&[u8]; 4] = [COMPRESSION1K as &[u8], COMPRESSION34K as &[u8], COMPRESSION65K as &[u8], COMPRESSION10MB as &[u8]];
// const ALL: [&[u8]; 1] = [COMPRESSION66K as &[u8]];
// const ALL: [&[u8]; 1] = [COMPRESSION65K as &[u8]];

fn bench_compression_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("Compress");
     
    for input in ALL.iter() {
        let input_bytes = input.len() as u64;
        group.throughput(Throughput::Bytes(input_bytes));

        group.bench_with_input(BenchmarkId::new("lz4_flexx", input_bytes), &input,
            |b, i| b.iter(|| lz4_flex::compress(&i) ));
        group.bench_with_input(BenchmarkId::new("lz4_rust", input_bytes), &input,
            |b, i| b.iter(|| lz4_compress::compress(&i) ));

        group.bench_with_input(BenchmarkId::new("lz4_linked", input_bytes), &input,
            |b, i| b.iter(|| {
                lz4_linked_block_compress(&i, None, false)
                // let mut cache = vec![];
                // let mut encoder = lz4::EncoderBuilder::new().level(0).build(&mut cache).unwrap();
                // let mut read = **i;
                // io::copy(&mut read, &mut encoder).unwrap();
                // let (_output, _result) = encoder.finish();
            } ));
    }

    group.finish();
}

fn bench_decompression_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("Decompress");
     
    for input in ALL.iter() {
        let input_bytes = input.len() as u64;
        group.throughput(Throughput::Bytes(input_bytes));

        let comp_flex = lz4_flex::compress(&input);
        let comp2 = lz4_compress::compress(&input);

        // let mut cache = vec![];
        // let mut encoder = lz4::EncoderBuilder::new().level(2).build(&mut cache).unwrap();
        // let mut read = *input;
        // io::copy(&mut read, &mut encoder).unwrap();
        // let (_comp_lz4, _result) = encoder.finish();
        // let comp_lz4: &[u8] = comp_lz4;


        let comp_lz4 = lz4::block::compress(&input, None, true).unwrap();

        // println!("comp_flex.len() {:?}", comp_flex.len());
        // println!("lz4_linked.len() {:?}", hmm.len());
        // let mut brotli:Vec<u8> = vec![];

        // {
        //     let mut writer = brotli::CompressorWriter::new(
        //     &mut brotli,
        //     4096,
        //     11,
        //     22);
        //     writer.write_all(&input).unwrap();
        // }

        group.bench_with_input(BenchmarkId::new("lz4_flexx", input_bytes), &comp_flex,
            |b, i| b.iter(|| lz4_flex::decompress(&i) ));
        group.bench_with_input(BenchmarkId::new("lz4_flexx_unchecked", input_bytes), &comp_flex,
            |b, i| b.iter(|| lz4_flex::decompress_unchecked(&i) ));
        group.bench_with_input(BenchmarkId::new("lz4_rust", input_bytes), &comp2,
            |b, i| b.iter(|| lz4_compress::decompress(&i) ));

        group.bench_with_input(BenchmarkId::new("lz4_linked", input_bytes), &comp_lz4,
            |b, i| b.iter(|| {
                    
                let output = lz4::block::decompress(&i, None);
                output

                // let mut output:Vec<u8> = vec![];
                // let mut waa = *i;
                // let mut decoder = lz4::Decoder::new(&mut waa).unwrap();
                // io::copy(&mut decoder, &mut output).unwrap();
                // output
            } ));
        // group.bench_with_input(BenchmarkId::new("brotli", input_bytes), &brotli,
        //     |b, i| b.iter(|| {
                
        //         let mut output:Vec<u8> = vec![];
        //         // let mut writer = brotli::Compressor::new(&mut output, 4096 /* buffer size */, 2 as u32, 20);

        //         let mut reader = brotli::Decompressor::new(
        //             &mut output,
        //             4096, // buffer size
        //         );

        //         // let mut waa = *i;
        //         // let mut decoder = lz4::Decoder::new(&mut waa).unwrap();
        //         // io::copy(&mut decoder, &mut output).unwrap();
        //         output
        //     } ));
    }

    group.finish();
}

// criterion_group!(benches, bench_simple, bench_nested, bench_throughput);
criterion_group!(benches, bench_decompression_throughput, bench_compression_throughput);
criterion_main!(benches);
