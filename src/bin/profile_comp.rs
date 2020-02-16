extern crate lz4_flex;



const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/dickens.txt");

fn main() {

    // use cpuprofiler::PROFILER;
    // PROFILER.lock().unwrap().start("./my-prof.profile").unwrap();
    for _ in 0..100 {
        lz4_flex::compress(COMPRESSION10MB as &[u8]);
    }
    // PROFILER.lock().unwrap().stop().unwrap();
    
}
