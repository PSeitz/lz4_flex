use std::hint::black_box;
use std::time::{Duration, Instant};

use lz4_flex::block::{
    compress_hc_dest_size, compress_hc_dest_size_with_table, decompress, CompressHcDestSizeTable,
};

const LEVEL: u8 = 12;
const SAMPLE_TIME: Duration = Duration::from_millis(200);
const SAMPLES: usize = 3;
const CORPUS_SIZE: usize = 1 << 20;

#[derive(Clone)]
struct RawCorpus {
    name: &'static str,
    bytes: Vec<u8>,
}

struct ResultRow {
    throughput_mib_s: f64,
    ratio: f64,
    input_bytes: u64,
    stored_bytes: u64,
    calls: u64,
    elapsed: Duration,
    retained_workspace_bytes: Option<usize>,
}

enum Encoder {
    RustStateless,
    RustReused(CompressHcDestSizeTable),
    LibLz4,
}

impl Encoder {
    fn name(&self) -> &'static str {
        match self {
            Self::RustStateless => "lz4_flex_stateless",
            Self::RustReused(_) => "lz4_flex_reused",
            Self::LibLz4 => "liblz4_hc",
        }
    }

    fn retained_workspace_bytes(&self) -> Option<usize> {
        match self {
            Self::RustReused(table) => Some(table.memory_usage_bytes()),
            Self::RustStateless | Self::LibLz4 => None,
        }
    }

    fn compress(&mut self, input: &[u8], output: &mut [u8]) -> (usize, usize) {
        match self {
            Self::RustStateless => compress_hc_dest_size(input, output, LEVEL),
            Self::RustReused(table) => {
                compress_hc_dest_size_with_table(input, output, LEVEL, table)
            }
            Self::LibLz4 => lzzzz::lz4_hc::compress_fill(input, output, lzzzz::lz4_hc::CLEVEL_MAX)
                .expect("liblz4 fixed-output compression"),
        }
    }

    fn decode(&self, compressed: &[u8], expected_size: usize) -> Vec<u8> {
        match self {
            Self::LibLz4 => {
                let mut decoded = vec![0; expected_size];
                let written = lzzzz::lz4::decompress(compressed, &mut decoded)
                    .expect("liblz4 must decode its output");
                decoded.truncate(written);
                decoded
            }
            Self::RustStateless | Self::RustReused(_) => {
                decompress(compressed, expected_size).expect("Rust must decode its output")
            }
        }
    }
}

fn main() {
    let corpora = raw_corpora();
    for corpus in &corpora {
        for capacity in [4 * 1024, 8 * 1024, 16 * 1024, 32 * 1024] {
            benchmark_case(corpus, capacity);
        }
    }
}

fn benchmark_case(corpus: &RawCorpus, output_capacity: usize) {
    let mut encoders = [
        Encoder::RustStateless,
        Encoder::RustReused(CompressHcDestSizeTable::new()),
        Encoder::LibLz4,
    ];
    validate_case(corpus, output_capacity, &mut encoders);

    let mut rows: Vec<Vec<ResultRow>> = (0..encoders.len()).map(|_| Vec::new()).collect();
    for sample in 0..SAMPLES {
        // Alternate which implementation runs first to reduce order bias.
        for offset in 0..encoders.len() {
            let index = (offset + sample) % encoders.len();
            rows[index].push(measure(
                &mut encoders[index],
                &corpus.bytes,
                output_capacity,
                SAMPLE_TIME,
            ));
        }
    }

    for (encoder, samples) in encoders.iter().zip(rows.iter_mut()) {
        samples.sort_by(|left, right| left.throughput_mib_s.total_cmp(&right.throughput_mib_s));
        let row = &samples[SAMPLES / 2];
        let retained_workspace_bytes = row
            .retained_workspace_bytes
            .map_or_else(|| "unmeasured".to_owned(), |bytes| bytes.to_string());
        eprintln!(
            "result,corpus={},codec={},level={LEVEL},corpus_bytes={},output_capacity={},input_bytes={},stored_bytes={},ratio={:.5},calls={},seconds={:.6},mib_per_sec={:.3},sample_min_mib_per_sec={:.3},sample_max_mib_per_sec={:.3},retained_workspace_bytes={}",
            corpus.name,
            encoder.name(),
            corpus.bytes.len(),
            output_capacity,
            row.input_bytes,
            row.stored_bytes,
            row.ratio,
            row.calls,
            row.elapsed.as_secs_f64(),
            row.throughput_mib_s,
            samples[0].throughput_mib_s,
            samples[SAMPLES - 1].throughput_mib_s,
            retained_workspace_bytes,
        );
    }

    let stateless = rows[0][SAMPLES / 2].throughput_mib_s;
    let reused = rows[1][SAMPLES / 2].throughput_mib_s;
    let c = rows[2][SAMPLES / 2].throughput_mib_s;
    eprintln!(
        "comparison,corpus={},level={LEVEL},output_capacity={},stateless_speedup_vs_liblz4={:.3},reused_speedup_vs_liblz4={:.3}",
        corpus.name,
        output_capacity,
        stateless / c,
        reused / c,
    );
}

fn validate_case(corpus: &RawCorpus, output_capacity: usize, encoders: &mut [Encoder; 3]) {
    for encoder in encoders.iter_mut() {
        let mut output = vec![0; output_capacity];
        let mut position = 0;
        while position < corpus.bytes.len() {
            let source = &corpus.bytes[position..];
            let (taken, written) = encoder.compress(source, &mut output);
            assert!(taken > 0, "{} made no progress", encoder.name());
            assert!(
                written <= output_capacity,
                "{} overran output",
                encoder.name()
            );
            let decoded = encoder.decode(&output[..written], taken);
            assert_eq!(
                decoded.as_slice(),
                &source[..taken],
                "{} output differs",
                encoder.name()
            );
            position += taken;
        }
    }
}

fn measure(
    encoder: &mut Encoder,
    input: &[u8],
    output_capacity: usize,
    sample_time: Duration,
) -> ResultRow {
    let mut output = vec![0; output_capacity];
    let start = Instant::now();
    let mut input_bytes = 0u64;
    let mut stored_bytes = 0u64;
    let mut calls = 0u64;
    while start.elapsed() < sample_time {
        let mut position = 0;
        while position < input.len() {
            let (taken, written) = encoder.compress(&input[position..], &mut output);
            assert!(taken > 0, "{} made no progress", encoder.name());
            assert!(
                written <= output_capacity,
                "{} overran output",
                encoder.name()
            );
            position += taken;
            input_bytes += taken as u64;
            stored_bytes += written as u64;
            calls += 1;
        }
        black_box(stored_bytes);
    }
    let elapsed = start.elapsed();
    ResultRow {
        throughput_mib_s: input_bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64(),
        ratio: stored_bytes as f64 / input_bytes as f64,
        input_bytes,
        stored_bytes,
        calls,
        elapsed,
        retained_workspace_bytes: encoder.retained_workspace_bytes(),
    }
}

fn raw_corpora() -> Vec<RawCorpus> {
    let source = include_bytes!("binggan_bench.rs").as_slice();
    let json = include_bytes!("compression_66k_JSON.txt").as_slice();
    let mut mixed = Vec::with_capacity(CORPUS_SIZE);
    let mixed_noise = noise(CORPUS_SIZE, 0x4d595df4d0f33173);
    while mixed.len() < CORPUS_SIZE {
        mixed.extend_from_slice(&source[..source.len().min(32 * 1024)]);
        mixed.extend_from_slice(&json[..json.len().min(16 * 1024)]);
        mixed.extend_from_slice(&mixed_noise[..16 * 1024]);
    }
    mixed.truncate(CORPUS_SIZE);

    let mut long_pattern = noise(4 * 1024, 0x9e3779b97f4a7c15);
    long_pattern.resize(CORPUS_SIZE - 192, 0);
    for _ in 0..8 {
        long_pattern.extend_from_slice(b"raw-block-pattern");
    }
    long_pattern.truncate(CORPUS_SIZE);

    vec![
        RawCorpus {
            name: "json_file",
            bytes: repeat_to_len(json, CORPUS_SIZE),
        },
        RawCorpus {
            name: "source_file",
            bytes: repeat_to_len(source, CORPUS_SIZE),
        },
        RawCorpus {
            name: "mixed_raw_files",
            bytes: mixed,
        },
        RawCorpus {
            name: "high_entropy",
            bytes: noise(CORPUS_SIZE, 0x243f6a8885a308d3),
        },
        RawCorpus {
            name: "long_pattern",
            bytes: long_pattern,
        },
    ]
}

fn repeat_to_len(source: &[u8], length: usize) -> Vec<u8> {
    assert!(!source.is_empty());
    let mut bytes = Vec::with_capacity(length);
    while bytes.len() < length {
        let remaining = length - bytes.len();
        bytes.extend_from_slice(&source[..source.len().min(remaining)]);
    }
    bytes
}

fn noise(length: usize, mut state: u64) -> Vec<u8> {
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}
