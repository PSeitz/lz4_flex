#[macro_use] extern crate criterion;
use self::criterion::*;

use std::io;
// use lz4_flex::{decompress, decompress_into, compress, compress_into};


const COMPRESSION1K: &'static [u8] = include_bytes!("compression_1k.txt");
const COMPRESSION34K: &'static [u8] = include_bytes!("compression_34k.txt");
const COMPRESSION65K: &'static [u8] = include_bytes!("compression_65k.txt");
const COMPRESSION10MB: &'static [u8] = include_bytes!("dickens.txt");

const ALL: &[&[u8]] = &[COMPRESSION1K as &[u8], COMPRESSION34K as &[u8], COMPRESSION65K as &[u8]];
// const ALL: [&[u8]; 4] = [COMPRESSION1K as &[u8], COMPRESSION34K as &[u8], COMPRESSION65K as &[u8], COMPRESSION10MB as &[u8]];
// const ALL: [&[u8]; 1] = [COMPRESSION65K as &[u8]];



fn bench_compression_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("Compress");
     
    for input in ALL.iter() {
        let input_bytes = input.len() as u64;
        group.throughput(Throughput::Bytes(input_bytes));

        group.bench_with_input(BenchmarkId::new("lz4_flexx", input_bytes), &input,
            |b, i| b.iter(|| lz4_flex::compress(&i) ));
        group.bench_with_input(BenchmarkId::new("lz4_compress", input_bytes), &input,
            |b, i| b.iter(|| lz4_compress::compress(&i) ));

        group.bench_with_input(BenchmarkId::new("lz4", input_bytes), &input,
            |b, i| b.iter(|| {
                let mut cache = vec![];
                let mut encoder = lz4::EncoderBuilder::new().level(2).build(&mut cache).unwrap();
                let mut read = **i;
                io::copy(&mut read, &mut encoder).unwrap();
                let (_output, _result) = encoder.finish();
                // output
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

        let mut cache = vec![];
        let mut encoder = lz4::EncoderBuilder::new().level(2).build(&mut cache).unwrap();
        let mut read = *input;
        io::copy(&mut read, &mut encoder).unwrap();
        let (comp_lz4, _result) = encoder.finish();
        let hmm: &[u8] = comp_lz4;

        group.bench_with_input(BenchmarkId::new("lz4_flexx", input_bytes), &comp_flex,
            |b, i| b.iter(|| lz4_flex::decompress(&i) ));
        group.bench_with_input(BenchmarkId::new("lz4_compress", input_bytes), &comp2,
            |b, i| b.iter(|| lz4_compress::decompress(&i) ));

        group.bench_with_input(BenchmarkId::new("lz4", input_bytes), &hmm,
            |b, i| b.iter(|| {
                
                let mut output:Vec<u8> = vec![];
                let mut waa = *i;
                let mut decoder = lz4::Decoder::new(&mut waa).unwrap();
                io::copy(&mut decoder, &mut output).unwrap();
                output
            } ));
    }

    group.finish();
}

// criterion_group!(benches, bench_simple, bench_nested, bench_throughput);
criterion_group!(benches, bench_decompression_throughput, bench_compression_throughput);
criterion_main!(benches);
