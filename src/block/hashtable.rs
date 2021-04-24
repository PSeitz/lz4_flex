/// The Hashtable trait used by the compression to store hashed bytes to their position.
/// `val` can be maximum the size of the input in bytes.
///
/// `pos` can have a maximum value of u16::MAX or 65535
/// If the hashtable is smaller it needs to reduce the pos to its space, e.g. by right shifting.
///
/// Duplication dictionary size.
///
/// Every four bytes is assigned an entry. When this number is lower, fewer entries exists, and
/// thus collisions are more likely, hurting the compression ratio.
///
use alloc::vec::Vec;

pub trait HashTable {
    fn get_at(&self, pos: usize) -> usize;
    fn put_at(&mut self, pos: usize, val: usize);
    fn clear(&mut self);
}

#[derive(Debug)]
pub struct HashTableUsize {
    dict: Vec<usize>,
    /// Shift the hash value for the dictionary to the right, to match the dictionary size.
    dict_bitshift: usize,
}

impl HashTableUsize {
    #[inline]
    pub fn new(dict_size: usize, dict_bitshift: usize) -> Self {
        let dict = alloc::vec![0; dict_size];
        Self {
            dict,
            dict_bitshift,
        }
    }
}

impl HashTable for HashTableUsize {
    #[inline]
    #[cfg(feature = "safe-encode")]
    fn get_at(&self, hash: usize) -> usize {
        self.dict[hash >> self.dict_bitshift] as usize
    }
    #[inline]
    #[cfg(not(feature = "safe-encode"))]
    fn get_at(&self, hash: usize) -> usize {
        unsafe { *self.dict.get_unchecked(hash >> self.dict_bitshift) as usize }
    }

    #[inline]
    #[cfg(feature = "safe-encode")]
    fn put_at(&mut self, hash: usize, val: usize) {
        self.dict[hash >> self.dict_bitshift] = val;
    }
    #[inline]
    #[cfg(not(feature = "safe-encode"))]
    fn put_at(&mut self, hash: usize, val: usize) {
        (*unsafe { self.dict.get_unchecked_mut(hash >> self.dict_bitshift) }) = val;
    }

    #[inline]
    fn clear(&mut self) {
        self.dict.fill(0);
    }
}

#[derive(Debug)]
#[repr(align(64))]
pub struct HashTableU32 {
    dict: Vec<u32>,
    /// Shift the hash value for the dictionary to the right, to match the dictionary size.
    dict_bitshift: usize,
}
impl HashTableU32 {
    #[inline]
    pub fn new(dict_size: usize, dict_bitshift: usize) -> Self {
        let dict = alloc::vec![0; dict_size];
        Self {
            dict,
            dict_bitshift,
        }
    }

    #[cfg(feature="frame")]
    #[cold]
    pub fn reposition(&mut self, offset: u32) {
        for i in &mut self.dict {
            *i = i.saturating_sub(offset);
        }
    }
}
impl HashTable for HashTableU32 {
    #[inline]
    #[cfg(feature = "safe-encode")]
    fn get_at(&self, hash: usize) -> usize {
        self.dict[hash >> self.dict_bitshift] as usize
    }
    #[inline]
    #[cfg(not(feature = "safe-encode"))]
    fn get_at(&self, hash: usize) -> usize {
        unsafe { *self.dict.get_unchecked(hash >> self.dict_bitshift) as usize }
    }
    #[inline]
    #[cfg(feature = "safe-encode")]
    fn put_at(&mut self, hash: usize, val: usize) {
        self.dict[hash >> self.dict_bitshift] = val as u32;
    }
    #[inline]
    #[cfg(not(feature = "safe-encode"))]
    fn put_at(&mut self, hash: usize, val: usize) {
        (*unsafe { self.dict.get_unchecked_mut(hash >> self.dict_bitshift) }) = val as u32;
    }
    #[inline]
    fn clear(&mut self) {
        self.dict.fill(0);
    }
}

#[derive(Debug)]
#[repr(align(64))]
pub struct HashTableU16 {
    dict: Vec<u16>,
    /// Shift the hash value for the dictionary to the right, to match the dictionary size.
    dict_bitshift: usize,
}
impl HashTableU16 {
    #[inline]
    pub fn new(dict_size: usize, dict_bitshift: usize) -> Self {
        let dict = alloc::vec![0; dict_size];
        Self {
            dict,
            dict_bitshift,
        }
    }
}
impl HashTable for HashTableU16 {
    #[inline]
    #[cfg(feature = "safe-encode")]
    fn get_at(&self, hash: usize) -> usize {
        self.dict[hash >> self.dict_bitshift] as usize
    }
    #[inline]
    #[cfg(not(feature = "safe-encode"))]
    fn get_at(&self, hash: usize) -> usize {
        unsafe { *self.dict.get_unchecked(hash >> self.dict_bitshift) as usize }
    }
    #[inline]
    #[cfg(feature = "safe-encode")]
    fn put_at(&mut self, hash: usize, val: usize) {
        self.dict[hash >> self.dict_bitshift] = val as u16;
    }
    #[inline]
    #[cfg(not(feature = "safe-encode"))]
    fn put_at(&mut self, hash: usize, val: usize) {
        (*unsafe { self.dict.get_unchecked_mut(hash >> self.dict_bitshift) }) = val as u16;
    }
    #[inline]
    fn clear(&mut self) {
        self.dict.fill(0);
    }
}

#[inline]
pub fn get_table_size(input_len: usize) -> (usize, usize) {
    let (dict_size, dict_bitshift) = match input_len {
        0..=500 => (128, 9),
        501..=1_000 => (256, 8),
        1_001..=4_000 => (512, 7),
        4_001..=8_000 => (1024, 6),
        8_001..=16_000 => (2048, 5),
        16_001..=30_000 => (8192, 3),
        // 100_000..=400_000 => (8192, 3),
        _ => (16384, 2),
    };
    (dict_size, dict_bitshift)
}
