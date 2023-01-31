use glob::glob;
use std::os::unix::prelude::MetadataExt;
use std::time::Duration;
use std::{env, fs};
use std::{io, time::Instant};

fn main() {
    let bench_type = env::args().skip(1).next().unwrap_or("".to_string());
    for entry in glob("bench_files/*").expect("Failed to read glob pattern") {
        let file_name = entry.unwrap().to_str().unwrap().to_string();
        let fs = fs::metadata(&file_name).unwrap().size();
        let break_condition = BreakCondition::Loops((3_000_000_000 / fs) as u32);

        bench_file(&file_name, &bench_type, break_condition).unwrap();
    }
}

fn bench_file(file: &str, bench_type: &str, break_condition: BreakCondition) -> io::Result<()> {
    if bench_type.is_empty() || bench_type == "compression" {
        bench_compression(file, break_condition)?;
    }
    if bench_type.is_empty() || bench_type == "decompression" {
        bench_decompression(file, break_condition)?;
    }
    Ok(())
}

fn bench_compression(file: &str, break_condition: BreakCondition) -> io::Result<()> {
    let file_content = std::fs::read(file)?;
    let mb = file_content.len() as f32 / 1_000_000 as f32;

    let mut out = Vec::new();
    let print_info = format!("{file} - Compression");

    bench(
        mb,
        &print_info,
        || {
            out.clear();
            compress(&file_content, &mut out);
        },
        break_condition,
    );

    Ok(())
}

fn bench_decompression(file: &str, break_condition: BreakCondition) -> io::Result<()> {
    let file_content = std::fs::read(file)?;
    let mb = file_content.len() as f32 / 1_000_000 as f32;

    let mut compressed = Vec::new();
    compress(&file_content, &mut compressed);

    let mut out = Vec::new();
    let print_info = format!("{file} - Decompression");

    bench(
        mb,
        &print_info,
        || {
            out.clear();
            decompress(&compressed, &mut out);
        },
        break_condition,
    );

    Ok(())
}

fn decompress(input: &[u8], out: &mut Vec<u8>) {
    let mut rdr = lz4_flex::frame::FrameDecoder::new(input);
    io::copy(&mut rdr, out).expect("I/O operation failed");
}

fn compress(input: &[u8], out: &mut Vec<u8>) {
    let mut wtr = lz4_flex::frame::FrameEncoder::new(out);

    io::copy(&mut &input[..], &mut wtr).expect("I/O operation failed");
    wtr.finish().unwrap();
}

#[derive(Copy, Clone)]
enum BreakCondition {
    AfterSecs(f32),
    Loops(u32),
}

impl BreakCondition {
    fn should_break(&self, secs_since_start: f32, loops: u32) -> bool {
        match self {
            BreakCondition::AfterSecs(max_secs) => {
                if secs_since_start > *max_secs {
                    return true;
                }
            }
            BreakCondition::Loops(max_loops) => {
                if loops >= *max_loops {
                    return true;
                }
            }
        }
        return false;
    }
}

fn bench<F>(mb: f32, print_info: &str, mut do_stuff: F, break_condition: BreakCondition)
where
    F: FnMut(),
{
    let start = Instant::now();
    let mut last_print = Instant::now();

    let mut loops = 0;
    loop {
        let start_loop = Instant::now();

        do_stuff();

        let after_comp = Instant::now();

        let elapsed_since_last_print: Duration = after_comp - last_print;
        if elapsed_since_last_print.as_secs_f32() > 1.0 {
            let elapsed_since_loop_start: Duration = after_comp - start_loop;
            let through_put = mb as f32 / elapsed_since_loop_start.as_secs_f32();
            println!("{print_info} {:.2} Mb/s", through_put);
            last_print = Instant::now();
        }

        loops += 1;
        let secs_since_start = (Instant::now() - start).as_secs_f32();

        if break_condition.should_break(secs_since_start, loops) {
            break;
        }
    }
}
