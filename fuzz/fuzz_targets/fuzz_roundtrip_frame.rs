#![no_main]
use libfuzzer_sys::fuzz_target;
use std::io::{Read, Write};

const ONE_MB: usize = 1024 * 1024;

#[derive(Clone, Debug, arbitrary::Arbitrary)]
pub struct Input {
    sample: Vec<u8>,
    data_size_seed: usize,
    chunk_size_seed: usize,
}

fuzz_target!(|input: Input| {
    let Input {
        sample,
        data_size_seed,
        chunk_size_seed,
    } = input;
    if sample.is_empty() {
        return;
    }
    let chunk_size = (chunk_size_seed % ONE_MB).max(1);
    let data_size = data_size_seed % ONE_MB;

    let mut data = Vec::with_capacity(data_size);
    while data.len() < data_size {
        data.extend_from_slice(&sample);
    }
    data.truncate(data_size);

    for bm in &[
        lz4_flex::frame::BlockMode::Independent,
        lz4_flex::frame::BlockMode::Linked,
    ] {
        for bs in &[
            lz4_flex::frame::BlockSize::Max64KB,
            lz4_flex::frame::BlockSize::Max256KB,
            lz4_flex::frame::BlockSize::Max1MB,
            lz4_flex::frame::BlockSize::Max4MB,
        ] {
            for check_sum in &[true, false] {
                // io::Write
                let mut fi = lz4_flex::frame::FrameInfo::default();
                fi.block_mode = *bm;
                fi.block_size = *bs;
                fi.block_checksums = *check_sum;
                fi.content_checksum = *check_sum;
                let mut enc = lz4_flex::frame::FrameEncoder::with_frame_info(
                    fi,
                    Vec::with_capacity(data_size),
                );
                for chunk in data.chunks(chunk_size) {
                    enc.write(chunk).unwrap();
                    // by flushing we force encoder to output a frame block
                    // if buffered data <= max block size
                    enc.flush().unwrap();
                }
                let compressed = enc.finish().unwrap();
                // io::Read
                let mut decompressed = Vec::new();
                decompressed.resize(data.len() + chunk_size, 0);
                let mut pos = 0;
                let mut dec = lz4_flex::frame::FrameDecoder::new(&*compressed);
                loop {
                    match dec.read(&mut decompressed[pos..pos + chunk_size]).unwrap() {
                        0 => {
                            decompressed.truncate(pos);
                            break;
                        }
                        i => {
                            pos += i;
                        }
                    }
                }
                assert_eq!(data, decompressed);
            }
        }
    }
});
