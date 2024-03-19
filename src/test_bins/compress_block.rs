fn main() {
    use lz4_flex::compress_prepend_size;
    let input = "Hello people, what's up?".to_string();
    let _compressed = compress_prepend_size(input.as_bytes());
}
