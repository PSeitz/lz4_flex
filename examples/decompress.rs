use std::io;
fn main() {
    #[cfg(feature = "frame")]
    {
        let stdin = io::stdin();
        let stdout = io::stdout();
        // Wrap the stdin reader in a LZ4 FrameDecoder.
        let mut rdr = lz4_flex::frame::FrameDecoder::new(stdin.lock());
        let mut wtr = stdout.lock();
        io::copy(&mut rdr, &mut wtr).expect("I/O operation failed");
    }
}
