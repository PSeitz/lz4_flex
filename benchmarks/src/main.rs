use glob::glob;
use std::time::Duration;
use std::{io, time::Instant};

fn main() {
    for entry in glob("bench_files/*").expect("Failed to read glob pattern") {
        let file_name = entry.unwrap().to_str().unwrap().to_string();
        bench_file(&file_name).unwrap();
    }
}

fn bench_file(file: &str) -> io::Result<()> {
    bench_compression(file)?;
    bench_decompression(file)?;
    Ok(())
}

fn bench_compression(file: &str) -> io::Result<()> {
    let file_content = std::fs::read(file)?;
    let mb = file_content.len() as f32 / 1_000_000 as f32;

    let mut out = Vec::new();
    let print_info = format!("{file} - Compression");

    bench(mb, &print_info, || {
        out.clear();
        compress(&file_content, &mut out);
    });

    Ok(())
}

fn bench_decompression(file: &str) -> io::Result<()> {
    let file_content = std::fs::read(file)?;
    let mb = file_content.len() as f32 / 1_000_000 as f32;

    let mut compressed = Vec::new();
    compress(&file_content, &mut compressed);

    let mut out = Vec::new();
    let print_info = format!("{file} - Decompression");

    bench(mb, &print_info, || {
        out.clear();
        decompress(&compressed, &mut out);
    });

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

fn bench<F>(mb: f32, print_info: &str, mut do_stuff: F)
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
        if secs_since_start > 5.0 || loops > 1000 {
            break;
        }
    }
}
