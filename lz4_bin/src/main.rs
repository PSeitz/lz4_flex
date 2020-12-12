use lz4_flex::block::DecompressError;
use tokio::fs::File;
use tokio::prelude::*; // for write_all()
use argh::FromArgs;

#[macro_use]
extern crate quick_error;

#[derive(FromArgs, Debug)]
/// Reach new heights.
struct Options {
    // #[argh(switch, short = 'm')]
    // /// multiple input files
    // multiple: bool,

    // #[argh(switch, short = 'f')]
    // /// overwrite_files
    // force: bool,

    #[argh(positional)]
    file: String,

    #[argh(positional)]
    out: Option<String>,

}

quick_error! {
    #[derive(Debug)]
    pub enum IoWrapper {
        Io(err: io::Error) {
            from()
            display("I/O error: {}", err)
            source(err)
        }
        DecompressError(err: DecompressError) {
            from()
            display("DecompressError error: {}", err)
            source(err)
        }
    }
}


const LZ_ENDING: &'static str = ".lz4";

#[tokio::main]
async fn main() -> Result<(), IoWrapper> {

    let opts: Options = argh::from_env();

    let decompress = opts.file.ends_with(LZ_ENDING);
    let output = opts.out.as_ref().cloned().unwrap_or_else(||{
        if decompress{
            let mut f = opts.file.to_string();
            f.truncate(opts.file.len() - LZ_ENDING.len());
            f
        }else{
            opts.file.to_string() + LZ_ENDING
        }
    });

    if decompress{
        // let mut in_file = File::open(opts.file);
        // let mut file = File::create(output);
        let (in_file, file) = tokio::join!(File::open(opts.file), File::create(output));
        let mut contents = vec![];
        in_file?.read_to_end(&mut contents).await?;

        

        let compressed = lz4_flex::decompress_size_prepended(&contents)?;

        
        file?.write_all(&compressed).await?;

    }else{

        let mut in_file = File::open(opts.file).await?;
        let mut contents = vec![];
        in_file.read_to_end(&mut contents).await?;

        let compressed = lz4_flex::compress_prepend_size(&contents);

        let mut file = File::create(output).await?;
        file.write_all(&compressed).await?;
    }
    Ok(())
}


