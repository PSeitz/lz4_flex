use anyhow::Result;
use argh::FromArgs;

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(FromArgs, Debug)]
/// [De]Compress data in the lz4 format.
struct Options {
    #[argh(switch)]
    /// delete original files (default: false)
    clean: bool,

    #[argh(switch, short = 'f')]
    /// overwrite output files
    force: bool,

    #[argh(switch, short = 'd')]
    /// force decompress
    decompress: bool,

    #[cfg(feature = "hc")]
    #[argh(option, short = 'l')]
    /// compression level 1-12 (1=fast, 2=mid, 3-9=HC, 10-12=optimal)
    level: Option<u8>,

    #[argh(option, short = 'B')]
    /// block size: 4=64KB, 5=256KB, 6=1MB, 7=4MB (default: 7)
    block_size: Option<u8>,

    #[argh(positional)]
    /// file to compress/decompress
    input_file: Option<PathBuf>,
    //#[argh(positional)]
    /// output file to write to. defaults to stdout
    #[argh(option, short = 'o')]
    out: Option<PathBuf>,
}
const LZ_ENDING: &str = "lz4";
const LZ_EXTENSION: &str = ".lz4";

fn main() -> Result<()> {
    let opts: Options = argh::from_env();

    #[cfg(feature = "hc")]
    let level = opts.level;
    #[cfg(not(feature = "hc"))]
    let level: Option<u8> = None;

    let input_file = opts.input_file.filter(|f| f.as_os_str() != "-");

    if let Some(file) = input_file {
        handle_file(
            &file,
            opts.out,
            opts.clean,
            opts.force,
            opts.decompress,
            level,
            opts.block_size,
            true,
        )?;
    } else {
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        let stdout;
        let mut out = match opts.out {
            Some(path) => Ok(File::create(path)?),
            None => {
                stdout = io::stdout();
                Err(stdout.lock())
            }
        };
        if opts.decompress {
            let mut decoder = lz4_flex::frame::FrameDecoder::new(&mut stdin);
            match &mut out {
                Ok(f) => io::copy(&mut decoder, f)?,
                Err(stdout) => io::copy(&mut decoder, stdout)?,
            };
        } else {
            let frame_info = create_frame_info(opts.block_size);
            match &mut out {
                Ok(f) => {
                    let mut wtr = create_encoder(frame_info, f, level);
                    io::copy(&mut stdin, &mut wtr)?;
                }
                Err(stdout) => {
                    let mut wtr = create_encoder(frame_info, stdout, level);
                    io::copy(&mut stdin, &mut wtr)?;
                }
            };
        }
    }

    Ok(())
}

fn create_frame_info(block_size: Option<u8>) -> lz4_flex::frame::FrameInfo {
    use lz4_flex::frame::BlockSize;
    let mut frame_info = lz4_flex::frame::FrameInfo::new();
    if let Some(bs) = block_size {
        frame_info.block_size = match bs {
            4 => BlockSize::Max64KB,
            5 => BlockSize::Max256KB,
            6 => BlockSize::Max1MB,
            7 => BlockSize::Max4MB,
            _ => BlockSize::Max4MB, // default to 4MB
        };
    } else {
        frame_info.block_size = BlockSize::Max4MB; // default to 4MB like C lz4
    }
    frame_info
}

fn create_encoder<W: io::Write>(
    frame_info: lz4_flex::frame::FrameInfo,
    wtr: W,
    level: Option<u8>,
) -> lz4_flex::frame::FrameEncoder<W> {
    match level {
        #[cfg(feature = "hc")]
        Some(lvl) => lz4_flex::frame::FrameEncoder::with_compression_level(frame_info, wtr, lvl),
        _ => lz4_flex::frame::FrameEncoder::with_frame_info(frame_info, wtr),
    }
}

fn handle_file(
    file: &Path,
    out: Option<PathBuf>,
    clean: bool,
    force: bool,
    force_decompress: bool,
    level: Option<u8>,
    block_size: Option<u8>,
    print_info: bool,
) -> Result<()> {
    let decompress = file.extension() == Some(LZ_ENDING.as_ref());
    if force_decompress && !decompress {
        anyhow::bail!("Can't determine an output filename")
    }
    let output = match out {
        Some(out) => out,
        None => {
            let output = if decompress {
                file.with_extension("")
            } else {
                let mut f = file.as_os_str().to_os_string();
                f.push(LZ_EXTENSION);
                f.into()
            };
            if print_info {
                println!(
                    "{} filename will be: {}",
                    if decompress {
                        "Decompressed"
                    } else {
                        "Compressed"
                    },
                    output.display()
                );
            }
            if !force && output.exists() {
                {
                    let stdout = io::stdout();
                    let mut stdout = stdout.lock();
                    write!(
                        stdout,
                        "{} already exists, do you want to overwrite? (y/N) ",
                        output.display()
                    )?;
                    stdout.flush()?;
                }
                let mut answer = String::new();
                io::stdin().read_line(&mut answer)?;
                if !answer.starts_with("y") {
                    println!("Not overwriting");
                    return Ok(());
                }
            }
            output
        }
    };

    if decompress {
        let in_file = File::open(file)?;
        let mut out_file = File::create(output)?;

        let mut rdr = lz4_flex::frame::FrameDecoder::new(in_file);
        io::copy(&mut rdr, &mut out_file)?;
    } else {
        let mut in_file = File::open(file)?;

        let out_file = File::create(&output)?;
        let frame_info = create_frame_info(block_size);
        let mut compressor = create_encoder(frame_info, TrackWriteSize::new(out_file), level);
        let input_size = io::copy(&mut in_file, &mut compressor)?;

        let output_size = compressor.finish()?.written;

        if print_info {
            println!(
                "Compressed {} bytes into {} ==> {:.2}%",
                input_size,
                output_size,
                output_size as f32 * 100.0 / input_size as f32
            );
        }
    }
    if clean {
        std::fs::remove_file(file)?;
    }

    Ok(())
}

struct TrackWriteSize<W: io::Write> {
    inner: W,
    written: u64,
}
impl<W: io::Write> TrackWriteSize<W> {
    fn new(inner: W) -> Self {
        TrackWriteSize { inner, written: 0 }
    }
}
impl<W: io::Write> io::Write for TrackWriteSize<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        let written = self.inner.write_vectored(bufs)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub fn lz4_flex_frame_compress_with(
    frame_info: lz4_flex::frame::FrameInfo,
    input: &[u8],
) -> io::Result<Vec<u8>> {
    let buffer = Vec::new();
    let mut enc = lz4_flex::frame::FrameEncoder::with_frame_info(frame_info, buffer);
    io::Write::write_all(&mut enc, input)?;
    Ok(enc.finish()?)
}

pub fn lz4_flex_frame_decompress(input: &[u8]) -> Result<Vec<u8>, lz4_flex::frame::Error> {
    let mut de = lz4_flex::frame::FrameDecoder::new(input);
    let mut out = Vec::new();
    io::Read::read_to_end(&mut de, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comp_cargo_toml() {
        handle_file(Path::new("../Cargo.toml"), None).unwrap();
    }
}
