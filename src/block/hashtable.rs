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
use alloc::vec::Vec;

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

    #[cfg(feature = "frame")]
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
    #[inline]
    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        hash(super::get_batch(input, pos)) as usize
    }
}

#[inline]
pub fn get_table_size(input_len: usize) -> (usize, usize) {
    let (dict_size, dict_bitshift) = match input_len {
        // U16 Positions
        0..=65535 => {
            // Considering we want a table with up to 16K bytes and each slot takes 2 bytes.
            // Calculate size the matching table size according to the input size,
            // so the overhead of "zeroing" the table is not too large for small inputs.
            let size = input_len.next_power_of_two().clamp(256, 16 * 1024) / 2;
            (size, 16 - size.trailing_zeros() as usize)
        }
        // U32 positions => 16KB table
        // Usize (U64) positions => 32KB table
        _ => (4096, 4),
    };
    (dict_size, dict_bitshift)
}

#[test]
fn test_get_table_size() {
    const MAX_HASH: usize = u16::MAX as usize;
    for i in 0..32 {
        let input_len = 2usize.pow(i);
        let (size, shift) = get_table_size(input_len);
        assert_eq!(size, (MAX_HASH >> shift) + 1);
    }
}
