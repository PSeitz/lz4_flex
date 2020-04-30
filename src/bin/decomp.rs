extern crate lz4_flex;



// const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/dickens.txt");
const COMPRESSION10MB: &'static [u8] = include_bytes!("../../benches/compression_66k_JSON.txt");

fn main() {

    let compressed = lz4_flex::compress(COMPRESSION10MB as &[u8]);
    for _ in 0..100000 {
        lz4_flex::decompress(&compressed).unwrap();
    }
    
}
