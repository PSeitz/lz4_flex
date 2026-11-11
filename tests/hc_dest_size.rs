//! Fixed-output compression: what it takes decodes back, it never overruns,
//! it fills the output, and it stays close to liblz4's `destSize` mode.

use lz4_flex::block::{compress_hc_dest_size, decompress};

fn text(len: usize, seed: u64) -> Vec<u8> {
    const WORDS: [&str; 16] = [
        "system", "vendor", "service", "class", "user", "group", "oneshot", "disabled", "socket",
        "property", "trigger", "start", "write", "chmod", "0755", "/data",
    ];
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len + 16);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(WORDS[(state % 16) as usize].as_bytes());
        out.push(if state % 7 == 0 { b'\n' } else { b' ' });
    }
    out.truncate(len);
    out
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn samples() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for len in [
        0, 1, 4, 5, 12, 13, 14, 15, 16, 17, 64, 255, 256, 4095, 4096, 4097, 70_000,
    ] {
        out.push((format!("text{len}"), text(len, len as u64)));
        out.push((format!("noise{len}"), noise(len, len as u64)));
        out.push((format!("zeros{len}"), vec![0u8; len]));
    }
    let mut mixed = text(100_000, 3);
    mixed.extend(noise(70_003, 4));
    mixed.extend(vec![7u8; 90_000]);
    mixed.extend(text(50_000, 5));
    out.push(("mixed".into(), mixed));
    out
}

/// The format's rules on how a block ends, read from the block.
fn assert_end_rules(label: &str, block: &[u8], len: usize) {
    let (mut at, mut out) = (0usize, 0usize);
    let mut last_match_start = None;
    let count = |at: &mut usize, nibble: usize| {
        let mut n = nibble;
        if nibble == 15 {
            loop {
                let b = block[*at];
                *at += 1;
                n += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        n
    };
    loop {
        let token = block[at] as usize;
        at += 1;
        let literals = count(&mut at, token >> 4);
        at += literals;
        out += literals;
        if at == block.len() {
            if let Some(start) = last_match_start {
                assert!(literals >= 5, "{label}: last literals {literals}");
                assert!(len - start >= 12, "{label}: last match at {start} of {len}");
            }
            return;
        }
        at += 2;
        let length = count(&mut at, token & 15) + 4;
        last_match_start = Some(out);
        out += length;
    }
}

#[test]
fn what_is_taken_decodes_back_and_the_output_is_never_overrun() {
    for (name, data) in samples() {
        for size in [1usize, 2, 3, 16, 17, 64, 512, 4096, 32768, 1 << 20] {
            for level in [1u8, 3, 9, 10, 12] {
                let label = format!("{name} into {size} at level {level}");
                // A guard past the end shows a write beyond the slice given.
                let mut buffer = vec![0xA5u8; size + 64];
                let (taken, written) = compress_hc_dest_size(&data, &mut buffer[..size], level);
                assert!(
                    buffer[size..].iter().all(|&b| b == 0xA5),
                    "{label}: overrun"
                );
                assert!(written <= size, "{label}: wrote {written}");
                assert!(taken <= data.len(), "{label}: took {taken}");
                let back = decompress(&buffer[..written], taken).expect(&label);
                assert!(back == data[..taken], "{label}: other bytes");
                assert_end_rules(&label, &buffer[..written], taken);
            }
        }
    }
}

#[test]
fn liblz4_reads_what_it_writes() {
    for (name, data) in samples() {
        for size in [64usize, 4096, 32768] {
            let mut dst = vec![0u8; size];
            let (taken, written) = compress_hc_dest_size(&data, &mut dst, 12);
            let mut out = vec![0u8; taken];
            let n = lzzzz::lz4::decompress(&dst[..written], &mut out)
                .unwrap_or_else(|e| panic!("{name} into {size}: {e:?}"));
            assert_eq!(n, taken, "{name} into {size}");
            assert!(out == data[..taken], "{name} into {size}");
        }
    }
}

#[test]
fn a_large_enough_output_takes_everything() {
    for (name, data) in samples() {
        let mut dst = vec![0u8; data.len() + data.len() / 200 + 64];
        let (taken, written) = compress_hc_dest_size(&data, &mut dst, 12);
        assert_eq!(taken, data.len(), "{name}");
        assert_eq!(decompress(&dst[..written], taken).unwrap(), data, "{name}");
    }
}

#[test]
fn a_full_output_is_nearly_filled() {
    let data = text(400_000, 9);
    let mut dst = vec![0u8; 32768];
    let (taken, written) = compress_hc_dest_size(&data, &mut dst, 12);
    assert!(written > 32768 - 64, "wrote {written}");
    assert!(taken > 32768 && taken < data.len(), "took {taken}");
}

#[test]
fn an_empty_output_takes_nothing() {
    assert_eq!(compress_hc_dest_size(b"data", &mut [], 12), (0, 0));
}

/// Pieces cut one after another reassemble the input.
#[test]
fn consecutive_pieces_cover_the_input() {
    let (_, data) = samples().pop().unwrap();
    let mut dst = vec![0u8; 8192];
    let (mut pos, mut back) = (0usize, Vec::new());
    while pos < data.len() {
        let (taken, written) = compress_hc_dest_size(&data[pos..], &mut dst, 9);
        assert!(taken > 0);
        back.extend(decompress(&dst[..written], taken).unwrap());
        pos += taken;
    }
    assert!(back == data);
}

/// The output on a 32 KiB budget stays within 2% of liblz4's `destSize` on a
/// large mixed input corpus.
#[test]
fn the_ratio_is_within_two_percent_of_liblz4() {
    use lzzzz::lz4_hc;
    let mut data = Vec::new();
    for i in 0..40u64 {
        data.extend(text(9000 + i as usize * 311, i));
        data.extend(noise(3000, i + 100));
        data.extend(text(30_000, i + 200));
    }
    type DestSize<'a> = dyn FnMut(&[u8], &mut [u8]) -> (usize, usize) + 'a;
    let run = |compress: &mut DestSize| {
        let (mut pos, mut total) = (0usize, 0usize);
        let mut dst = vec![0u8; 32768];
        while pos < data.len() {
            let end = data.len().min(pos + (1 << 20));
            let (taken, written) = compress(&data[pos..end], &mut dst);
            assert!(taken > 0);
            pos += taken;
            total += written;
        }
        total
    };
    let ours = run(&mut |src, dst| compress_hc_dest_size(src, dst, 12));
    let theirs = run(&mut |src, dst| {
        let mut cursor = std::io::Cursor::new(src);
        #[allow(deprecated)]
        let written = lz4_hc::compress_partial(&mut cursor, dst, lz4_hc::CLEVEL_MAX).unwrap();
        (cursor.position() as usize, written)
    });
    println!(
        "ours {ours}, liblz4 {theirs}: {:+.2}%",
        (ours as f64 / theirs as f64 - 1.0) * 100.0
    );
    assert!(
        ours as f64 <= theirs as f64 * 1.02,
        "ours {ours}, liblz4 {theirs}"
    );
}
