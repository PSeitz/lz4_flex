use anyhow::Result;
use argh::FromArgs;

use std::fs::File;
use std::io;
use std::path::Path;
use std::path::PathBuf;

#[derive(FromArgs, Debug)]
/// Reach new heights.
struct Options {
    //#[argh(option, default = "false")]
    #[argh(switch)]
    /// delete original files (default: false)
    clean: bool,

    #[argh(switch, short = 'd')]
    /// force decompress
    decompress: bool,

    // #[argh(switch, short = 'f')]
    // /// overwrite_files
    // force: bool,
    #[argh(positional)]
    input_file: Option<PathBuf>,
    //#[argh(positional)]
    /// zoo
    #[argh(option, short = 'o')]
    out: Option<PathBuf>,
    /// file[s] to compress/decompress
    /// list of input files
    #[argh(option, short = 'f')]
    files: Vec<PathBuf>,
}
const LZ_ENDING: &'static str = "lz4";

//fn default_clean() -> bool {
//false
//}

fn main() -> Result<()> {
    let opts: Options = argh::from_env();

    if opts.input_file.is_none() && opts.files.is_empty() {
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        if opts.decompress {
            let mut decoder = lz4_flex::frame::FrameDecoder::new(&mut stdin);
            std::io::copy(&mut decoder, &mut stdout)?;
        } else {
            let mut wtr = lz4_flex::frame::FrameEncoder::new(&mut stdout);
            std::io::copy(&mut stdin, &mut wtr)?;
        }
    } else {
        if let Some(file) = opts.input_file {
            handle_file(&file, opts.out, opts.clean)?;
        }
    }

    Ok(())
}

fn handle_file(file: &Path, out: Option<PathBuf>, clean: bool) -> Result<()> {
    let decompress = file.extension() == Some(std::ffi::OsStr::new(LZ_ENDING));
    let output = out.as_ref().cloned().unwrap_or_else(|| {
        if decompress {
            let mut f = file.to_path_buf();
            f.set_extension("");
            f
        } else {
            let curr_extesion = file
                .extension()
                .map(|ext| ext.to_str().unwrap_or(""))
                .unwrap_or("");
            if curr_extesion != "" {
                file.with_extension(curr_extesion.to_string() + "." + LZ_ENDING)
            } else {
                file.with_extension(LZ_ENDING)
            }
        }
    });

    dbg!(decompress);
    if decompress {
        let in_file = File::open(file)?;
        let mut out_file = File::create(output)?;

        let mut rdr = lz4_flex::frame::FrameDecoder::new(in_file);
        std::io::copy(&mut rdr, &mut out_file)?;
    } else {
        let mut in_file = File::open(file)?;

        let out_file = File::create(output)?;
        let mut compressor = lz4_flex::frame::FrameEncoder::new(out_file);
        std::io::copy(&mut in_file, &mut compressor)?;
        compressor.finish().unwrap();
    }
    if clean {
        std::fs::remove_file(file)?;
    }

    Ok(())
}

pub fn lz4_flex_frame_compress_with(
    frame_info: lz4_flex::frame::FrameInfo,
    input: &[u8],
) -> Result<Vec<u8>, std::io::Error> {
    let buffer = Vec::new();
    let mut enc = lz4_flex::frame::FrameEncoder::with_frame_info(frame_info, buffer);
    std::io::Write::write_all(&mut enc, input)?;
    Ok(enc.finish()?)
}

#[cfg(feature = "frame")]
pub fn lz4_flex_frame_decompress(input: &[u8]) -> Result<Vec<u8>, lz4_flex::frame::Error> {
    let mut de = lz4_flex::frame::FrameDecoder::new(input);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut de, &mut out)?;
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
