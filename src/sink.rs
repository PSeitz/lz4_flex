#[cfg(any(feature = "frame"))]
use alloc::vec::Vec;
//#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
//use core::mem::MaybeUninit;

/// Returns a Sink implementation appropriate for outputing up to `required_capacity`
/// bytes at `vec[offset..offset+required_capacity]`.
/// It can be either a `SliceSink` (pre-filling the vec with zeroes if necessary)
/// when the `safe-decode` feature is enabled, or `VecSink` otherwise.
/// The argument `pos` defines the initial output position in the Sink.
#[inline]
#[cfg(any(feature = "frame"))]
pub fn vec_sink_for_compression(
    vec: &mut Vec<u8>,
    offset: usize,
    pos: usize,
    required_capacity: usize,
) -> SliceSink {
    return {
        vec.resize(offset + required_capacity, 0);
        SliceSink::new(&mut vec[offset..], pos)
    };
}

/// Returns a Sink implementation appropriate for outputing up to `required_capacity`
/// bytes at `vec[offset..offset+required_capacity]`.
/// It can be either a `SliceSink` (pre-filling the vec with zeroes if necessary)
/// when the `safe-decode` feature is enabled, or `VecSink` otherwise.
/// The argument `pos` defines the initial output position in the Sink.
#[cfg(feature = "frame")]
#[inline]
pub fn vec_sink_for_decompression(
    vec: &mut Vec<u8>,
    offset: usize,
    pos: usize,
    required_capacity: usize,
) -> SliceSink {
    return {
        vec.resize(offset + required_capacity, 0);
        SliceSink::new(&mut vec[offset..], pos)
    };
}

/// SliceSink is used as target to de/compress data into a preallocated and possibly uninitialized
/// `&[u8]`
/// space.
///
/// # Handling of Capacity
/// Extend methods will panic if there's insufficient capacity left in the Sink.
///
/// # Invariants
///   - Bytes `[..pos()]` are always initialized.
pub struct SliceSink<'a> {
    /// The working slice, which may contain uninitialized bytes
    output: &'a mut [u8],
    /// Number of bytes in start of `output` guaranteed to be initialized
    pos: usize,
}

impl<'a> SliceSink<'a> {
    /// Creates a `Sink` backed by the given byte slice.
    /// `pos` defines the initial output position in the Sink.
    /// # Panics
    /// Panics if `pos` is out of bounds.
    #[inline]
    pub fn new(output: &'a mut [u8], pos: usize) -> Self {
        // SAFETY: Caller guarantees that all elements of `output` are initialized.
        let _ = &mut output[..pos]; // bounds check pos
        SliceSink { output, pos }
    }
}

impl<'a> SliceSink<'a> {
    /// Returns a raw ptr to the first unfilled byte of the Sink. Analogous to `[pos..].as_ptr()`.
    #[inline]
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    pub(crate) unsafe fn pos_mut_ptr(&mut self) -> *mut u8 {
        self.base_mut_ptr().add(self.pos()) as *mut u8
    }

    /// Pushes a byte to the end of the Sink.
    #[inline]
    #[cfg(any(feature = "safe-encode"))]
    pub(crate) fn push(&mut self, byte: u8) {
        self.output[self.pos] = byte;
        self.pos += 1;
    }

    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    pub(crate) unsafe fn base_mut_ptr(&mut self) -> *mut u8 {
        self.output.as_mut_ptr()
    }

    #[inline]
    #[cfg(any(feature = "safe-decode"))]
    pub(crate) fn filled_slice(&self) -> &[u8] {
        &self.output[..self.pos]
    }

    #[inline]
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    pub(crate) fn capacity(&self) -> usize {
        self.output.len()
    }

    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    #[inline]
    pub(crate) unsafe fn set_pos(&mut self, new_pos: usize) {
        debug_assert!(new_pos <= self.capacity());
        self.pos = new_pos;
    }

    #[inline]
    #[cfg(any(feature = "safe-decode"))]
    pub(crate) fn extend_with_fill(&mut self, byte: u8, len: usize) {
        self.output[self.pos..self.pos + len].fill(byte);
        self.pos += len;
    }

    /// Extends the Sink with `data`.
    #[inline]
    pub(crate) fn extend_from_slice(&mut self, data: &[u8]) {
        self.extend_from_slice_wild(data, data.len())
    }

    #[inline]
    pub(crate) fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize) {
        assert!(copy_len <= data.len());
        self.output[self.pos..self.pos + data.len()].copy_from_slice(data);
        self.pos += copy_len;
    }

    /// Copies `len` bytes starting from `start` to the end of the Sink.
    /// # Panics
    /// Panics if `start` >= `pos`.
    #[inline]
    #[cfg(any(feature = "safe-decode"))]
    pub(crate) fn extend_from_within(&mut self, start: usize, wild_len: usize, copy_len: usize) {
        assert!(copy_len <= wild_len);
        assert!(start + copy_len <= self.pos);
        self.output.copy_within(start..start + wild_len, self.pos);
        self.pos += copy_len;
    }

    #[inline]
    #[cfg(any(feature = "safe-decode"))]
    pub(crate) fn extend_from_within_overlapping(&mut self, start: usize, len: usize) {
        // Sink safety invariant guarantees that the first `pos` items are always initialized.
        assert!(start <= self.pos);
        let offset = self.pos - start;
        let out = &mut self.output[start..self.pos + len];
        out[offset] = 0; // ensures that a copy w/ start == pos becomes a zero fill
        for i in offset..out.len() {
            out[i] = out[i - offset];
        }
        self.pos += len;
    }
}

#[cfg(test)]
mod tests {

    #[test]
    #[cfg(any(feature = "safe-encode", feature = "safe-decode"))]
    fn test_sink_slice() {
        use super::Vec;
        use crate::sink::SliceSink;
        let mut data = Vec::new();
        data.resize(5, 0);
        let mut sink = SliceSink::new(&mut data, 1);
        assert_eq!(sink.pos(), 1);
        assert_eq!(sink.capacity(), 5);
        assert_eq!(sink.filled_slice(), &[0]);
        sink.extend_from_slice(&[1, 2, 3]);
        assert_eq!(sink.pos(), 4);
        assert_eq!(sink.filled_slice(), &[0, 1, 2, 3]);
    }
}
