extern crate lz4_flex;



// const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/dickens.txt");
const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/compression_66k_JSON.txt");

fn main() {

    // use cpuprofiler::PROFILER;
    // let compressed = lz4_flex::compress(COMPRESSION10MB as &[u8]);
    // PROFILER.lock().unwrap().start("./my-prof.profile").unwrap();
    // for _ in 0..1000 {
    //     lz4_flex::decompress(&compressed).unwrap();
    // }
    // PROFILER.lock().unwrap().stop().unwrap();
    
}
