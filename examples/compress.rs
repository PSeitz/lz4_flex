use std::io;
fn main() {
    #[cfg(feature = "frame")]
    {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut rdr = stdin.lock();
        // Wrap the stdout writer in a LZ4 Frame writer.
        let mut wtr = lz4_flex::frame::FrameEncoder::new(stdout.lock());
        io::copy(&mut rdr, &mut wtr).expect("I/O operation failed");
        let _stdout = wtr.finish().unwrap();
    }
}
