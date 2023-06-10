use alloc::boxed::Box;
use core::convert::TryInto;

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

pub trait HashTable {
    fn get_at(&self, pos: usize) -> usize;
    fn put_at(&mut self, pos: usize, val: usize);
    fn clear(&mut self);
    #[inline]
    #[cfg(target_pointer_width = "64")]
    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        hash5(super::compress::get_batch_arch(input, pos)) as usize
    }
    #[inline]
    #[cfg(target_pointer_width = "32")]
    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        hash(super::compress::get_batch(input, pos)) as usize
    }
}

const HASHTABLE_SIZE_4K: usize = 4 * 1024;
const HASHTABLE_BIT_SHIFT_4K: usize = 4;

#[derive(Debug)]
#[repr(align(64))]
pub struct HashTable4KU16 {
    dict: Box<[u16; HASHTABLE_SIZE_4K]>,
}
impl HashTable4KU16 {
    #[inline]
    pub fn new() -> Self {
        // This generates more efficient assembly in contrast to Box::new(slice), because of an
        // optmized call alloc_zeroed, vs. alloc + memset
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

#[derive(Debug)]
pub struct HashTable4K {
    dict: Box<[u32; HASHTABLE_SIZE_4K]>,
}
impl HashTable4K {
    #[inline]
    pub fn new() -> Self {
        let dict = alloc::vec![0; HASHTABLE_SIZE_4K]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        Self { dict }
    }

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

#[derive(Debug)]
pub struct HashTable8K {
    dict: Box<[u32; HASHTABLE_SIZE_8K]>,
}
#[allow(dead_code)]
impl HashTable8K {
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
