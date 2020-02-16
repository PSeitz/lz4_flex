extern crate lz4_flex;



const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/dickens.txt");

fn main() {

    use cpuprofiler::PROFILER;
    let compressed = lz4_flex::compress(COMPRESSION10MB as &[u8]);
    PROFILER.lock().unwrap().start("./my-prof.profile").unwrap();
    lz4_flex::decompress(&compressed).unwrap();
    PROFILER.lock().unwrap().stop().unwrap();
    
}
