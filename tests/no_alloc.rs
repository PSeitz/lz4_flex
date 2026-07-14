//! Roundtrip without the `alloc` feature, using only the `_into` APIs.
//! Run with: cargo test --no-default-features --test no_alloc
#![cfg(not(feature = "alloc"))]

use lz4_flex::block::{
    compress_into, compress_into_with_table, decompress_into, get_maximum_output_size,
    CompressTable,
};

static mut TABLE: CompressTable = CompressTable::large();

#[test]
fn heapless_roundtrip() {
    let input = b"Hello people, what's up? Hello people, what's up? Hello!";
    let mut compressed = [0u8; 256];
    assert!(get_maximum_output_size(input.len()) <= compressed.len());

    // stack-allocated hash table
    let len = compress_into(input, &mut compressed).unwrap();
    let mut decompressed = [0u8; 256];
    let dlen = decompress_into(&compressed[..len], &mut decompressed).unwrap();
    assert_eq!(&decompressed[..dlen], input);

    // statically allocated hash table
    let table = unsafe { &mut *core::ptr::addr_of_mut!(TABLE) };
    let len = compress_into_with_table(input, &mut compressed, table).unwrap();
    let dlen = decompress_into(&compressed[..len], &mut decompressed).unwrap();
    assert_eq!(&decompressed[..dlen], input);
}
