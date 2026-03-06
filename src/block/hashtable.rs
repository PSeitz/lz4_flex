#[allow(unused_imports)]
use alloc::boxed::Box;

/// The Hashtable trait used by the compression to store hashed bytes to their position.
/// `val` can be maximum the size of the input in bytes.
///
/// `pos` can have a maximum value of u16::MAX or 65535
/// If the hashtable is smaller it needs to reduce the pos to its space, e.g. by right
/// shifting.
///
/// Duplication dictionary size.
///
/// Every four bytes is assigned an entry. When this number is lower, fewer entries exists, and
/// thus collisions are more likely, hurting the compression ratio.
///
/// hashes and right shifts to a maximum value of 16bit, 65535
/// The right shift is done in order to not exceed, the hashtables capacity
#[inline]
fn hash(sequence: u32) -> u32 {
    (sequence.wrapping_mul(2654435761_u32)) >> 16
}

/// hashes and right shifts to a maximum value of 16bit, 65535
/// The right shift is done in order to not exceed, the hashtables capacity
#[cfg(target_pointer_width = "64")]
#[inline]
fn hash5(sequence: usize) -> u32 {
    let primebytes = if cfg!(target_endian = "little") {
        889523592379_usize
    } else {
        11400714785074694791_usize
    };
    (((sequence << 24).wrapping_mul(primebytes)) >> 48) as u32
}

/// Trait for hash tables used during LZ4 compression.
pub trait HashTable {
    /// Returns the value stored at the given hash position.
    fn get_at(&self, pos: usize) -> usize;
    /// Stores a value at the given hash position.
    fn put_at(&mut self, pos: usize, val: usize);
    /// Resets all entries to zero.
    fn clear(&mut self);
    /// Computes a hash for the bytes at `pos` in `input`.
    #[inline]
    #[cfg(target_pointer_width = "64")]
    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        hash5(super::compress::get_batch_arch(input, pos)) as usize
    }
    /// Computes a hash for the bytes at `pos` in `input`.
    #[inline]
    #[cfg(target_pointer_width = "32")]
    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        hash(super::compress::get_batch(input, pos)) as usize
    }
}

const HASHTABLE_SIZE_4K: usize = 4 * 1024;
const HASHTABLE_BIT_SHIFT_4K: usize = 4;

/// A 4K entry hash table using 16-bit values.
#[derive(Debug)]
#[repr(align(64))]
pub struct HashTable4KU16 {
    dict: Box<[u16; HASHTABLE_SIZE_4K]>,
}
impl HashTable4KU16 {
    /// Creates a new zeroed hash table.
    #[inline]
    pub fn new() -> Self {
        // This generates more efficient assembly in contrast to Box::new(slice), because of an
        // optimized call alloc_zeroed, vs. alloc + memset
        // try_into is optimized away
        let dict = alloc::vec![0; HASHTABLE_SIZE_4K]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        Self { dict }
    }
}
impl HashTable for HashTable4KU16 {
    #[inline]
    fn get_at(&self, hash: usize) -> usize {
        self.dict[hash >> HASHTABLE_BIT_SHIFT_4K] as usize
    }
    #[inline]
    fn put_at(&mut self, hash: usize, val: usize) {
        self.dict[hash >> HASHTABLE_BIT_SHIFT_4K] = val as u16;
    }
    #[inline]
    fn clear(&mut self) {
        self.dict.fill(0);
    }
    #[inline]
    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        hash(super::get_batch(input, pos)) as usize
    }
}

/// A 4K entry hash table using 32-bit values.
#[derive(Debug)]
pub struct HashTable4K {
    dict: Box<[u32; HASHTABLE_SIZE_4K]>,
}
impl HashTable4K {
    /// Creates a new zeroed hash table.
    #[inline]
    pub fn new() -> Self {
        let dict = alloc::vec![0; HASHTABLE_SIZE_4K]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        Self { dict }
    }

    /// Shifts all entries down by `offset`, clamping at zero.
    #[cold]
    #[allow(dead_code)]
    pub fn reposition(&mut self, offset: u32) {
        for i in self.dict.iter_mut() {
            *i = i.saturating_sub(offset);
        }
    }
}
impl HashTable for HashTable4K {
    #[inline]
    fn get_at(&self, hash: usize) -> usize {
        self.dict[hash >> HASHTABLE_BIT_SHIFT_4K] as usize
    }
    #[inline]
    fn put_at(&mut self, hash: usize, val: usize) {
        self.dict[hash >> HASHTABLE_BIT_SHIFT_4K] = val as u32;
    }
    #[inline]
    fn clear(&mut self) {
        self.dict.fill(0);
    }
}

const HASHTABLE_SIZE_8K: usize = 8 * 1024;
const HASH_TABLE_BIT_SHIFT_8K: usize = 3;

/// An 8K entry hash table using 32-bit values.
#[derive(Debug)]
pub struct HashTable8K {
    dict: Box<[u32; HASHTABLE_SIZE_8K]>,
}
#[allow(dead_code)]
impl HashTable8K {
    /// Creates a new zeroed hash table.
    #[inline]
    pub fn new() -> Self {
        let dict = alloc::vec![0; HASHTABLE_SIZE_8K]
            .into_boxed_slice()
            .try_into()
            .unwrap();

        Self { dict }
    }
}
impl HashTable for HashTable8K {
    #[inline]
    fn get_at(&self, hash: usize) -> usize {
        self.dict[hash >> HASH_TABLE_BIT_SHIFT_8K] as usize
    }
    #[inline]
    fn put_at(&mut self, hash: usize, val: usize) {
        self.dict[hash >> HASH_TABLE_BIT_SHIFT_8K] = val as u32;
    }
    #[inline]
    fn clear(&mut self) {
        self.dict.fill(0);
    }
}
