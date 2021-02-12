/// The dictionary will replace normal lookups to search for duplicates
///
/// A dictionary can improve compression ratio and speed.
///
/// Maximum allowed size is 16kb (u16::MAX), because this is the maxium offset allowed in the format.
///

use crate::block::compress::get_hash_at;
use crate::block::hashtable::HashTable;
use crate::block::hashtable::HashTableU16;
use crate::block::hashtable::get_table_size;
use alloc::vec::Vec;

#[derive(Debug)]
pub struct Dict {
    data: Vec<u8>,
    hashtable: HashTableU16
}

impl Dict {
    #[inline]
    pub fn new(mut data: Vec<u8>) -> Self {
        data.truncate(u16::MAX as usize - 1);
        // assert!(data.len() <= u16::MAX as usize);
        let (dict_size, dict_bitshift) = get_table_size(u16::MAX as usize - 1);
        let mut hashtable = HashTableU16::new(dict_size, dict_bitshift);
        for i in 0..std::cmp::max(data.len(), 4) - 4 { // -4, because data<u16::MAX a 4 byte hasher is used
            let hash = get_hash_at(&data, i);
            hashtable.put_at(hash, i);
        }

        { // TODO only in safe-decode
            data.resize(u16::MAX as usize + 32, 0);
        }
        Dict{
            data, hashtable
        }
    }

    #[inline]
    pub fn get_data(&self) -> &[u8] {
        &self.data
    }

    #[inline]
    pub fn get_hashtable(&self) -> &HashTableU16 {
        &self.hashtable
    }

}

#[cfg(test)]
mod tests_dict {
    use super::*;
    #[test]
    fn test_dict_simple() {
        let mut s = Vec::with_capacity(10);
        s.extend(&[5, 5, 5, 3, 3, 3, 3]);
        Dict::new(s);
    }
}