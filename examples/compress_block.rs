fn main() {
    use lz4_flex::block::compress_prepend_size;
    let input: &[u8] = b"Hello people, what's up?";
    let _compressed = compress_prepend_size(input);
}
