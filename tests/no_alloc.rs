//! Roundtrip without the `alloc` feature, using only the `_into` APIs.
//! Run with: cargo test --no-default-features --test no_alloc
#![cfg(not(feature = "alloc"))]

use lz4_flex::block::{
    compress_into, compress_into_with_table, decompress_into, get_maximum_output_size,
    CompressTable,
};

static mut TABLE: CompressTable = CompressTable::large();

#[test]
#[cfg(feature = "safe-encode")]
fn heapless_prefix_table_upgrade() {
    use lz4_flex::block::compress_prefix_into_with_table;

    let input = [42; 128 * 1024];
    let mut compressed = [0; 4096];
    let mut decompressed = [0; 128 * 1024];
    let mut table = CompressTable::small();
    for input in [&input[..], &input[..16]] {
        for capacity in [512, 4096] {
            let (used, written) =
                compress_prefix_into_with_table(input, &mut compressed[..capacity], &mut table)
                    .unwrap();
            assert!(matches!(table, CompressTable::Large(_)));
            if capacity == 4096 {
                assert_eq!(used, input.len());
            } else if input.len() > 65536 {
                assert!(used > 65536 && used < input.len());
            }
            let decoded = decompress_into(&compressed[..written], &mut decompressed).unwrap();
            assert_eq!(decoded, used);
            assert_eq!(&decompressed[..decoded], &input[..used]);
        }
    }
}

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
