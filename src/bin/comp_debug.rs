extern crate lz4_flex;


fn main() {
    let s =r#"AAAAAAAAAAAAAAAAAAAAAAAAaAAAAAAAAAAAAAAAAAAAAAAAA"#;


    lz4_flex::compress(s.as_bytes());

}

