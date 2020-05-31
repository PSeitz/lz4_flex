extern crate lz4_flex;


fn main() {
    let s =r#"There is nothing either good or bad, but thinking makes it so."#;


    let compressed = lz4_flex::compress(s.as_bytes());
    lz4_flex::decompress(&compressed, s.len());



}

