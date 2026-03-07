#![no_main]
use libfuzzer_sys::fuzz_target;
use std::io::{Read, Write};

const ONE_MB: usize = 1024 * 1024;

#[derive(Clone, Debug, arbitrary::Arbitrary)]
pub struct Input {
    sample: Vec<u8>,
    data_size_seed: usize,
    chunk_size_seed: usize,
    /// Compression level seed (0-12 maps to levels 1-12, with 0 being fast/level 1)
    #[cfg(feature = "hc")]
    compression_level_seed: u8,
}

fuzz_target!(|input: Input| {
    let Input {
        sample,
        data_size_seed,
        chunk_size_seed,
        #[cfg(feature = "hc")]
        compression_level_seed,
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

    // Compression levels to test: 1 (fast), and with hc feature: 2 (mid), 3-9 (hc), 10-12 (opt)
    #[cfg(feature = "hc")]
    let levels: &[u32] = match compression_level_seed % 5 {
        0 => &[1],       // fast
        1 => &[2],       // mid
        2 => &[3, 6, 9], // hc (sample)
        3 => &[10, 12],  // opt (sample)
        _ => &[1, 2, 9, 12], // mix
    };
    #[cfg(not(feature = "hc"))]
    let levels: &[u32] = &[1];

    for level in levels {
        // HC levels only support Independent block mode
        let block_modes: &[lz4_flex::frame::BlockMode] = if *level > 1 {
            &[lz4_flex::frame::BlockMode::Independent]
        } else {
            &[
                lz4_flex::frame::BlockMode::Independent,
                lz4_flex::frame::BlockMode::Linked,
            ]
        };

        for bm in block_modes {
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

                    #[cfg(feature = "hc")]
                    let mut enc = lz4_flex::frame::FrameEncoder::with_compression_level(
                        fi,
                        Vec::with_capacity(data_size),
                        *level as u8,
                    );
                    #[cfg(not(feature = "hc"))]
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
                    assert_eq!(data, decompressed, "Failed at level {}", level);
                }
            }
        }
    }
});
