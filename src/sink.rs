#[allow(unused_imports)]
use alloc::vec::Vec;

use crate::fastcpy::slice_copy;

/// Returns a Sink implementation appropriate for outputing up to `required_capacity`
/// bytes at `vec[offset..offset+required_capacity]`.
/// It can be either a `SliceSink` (pre-filling the vec with zeroes if necessary)
/// when the `safe-decode` feature is enabled, or `VecSink` otherwise.
/// The argument `pos` defines the initial output position in the Sink.
#[inline]
#[cfg(feature = "frame")]
pub fn vec_sink_for_compression(
    vec: &mut Vec<u8>,
    offset: usize,
    pos: usize,
    required_capacity: usize,
) -> SliceSink {
    {
        vec.resize(offset + required_capacity, 0);
        SliceSink::new(&mut vec[offset..], pos)
    }
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
    {
        vec.resize(offset + required_capacity, 0);
        SliceSink::new(&mut vec[offset..], pos)
    }
}

pub trait Sink {
    /// Returns a raw ptr to the first unfilled byte of the Sink. Analogous to `[pos..].as_ptr()`.
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn pos_mut_ptr(&mut self) -> *mut u8;

    /// read byte at position
    #[allow(dead_code)]
    fn byte_at(&mut self, pos: usize) -> u8;

    /// Pushes a byte to the end of the Sink.
    #[cfg(feature = "safe-encode")]
    fn push(&mut self, byte: u8);

    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn base_mut_ptr(&mut self) -> *mut u8;

    fn pos(&self) -> usize;

    fn capacity(&self) -> usize;

    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn set_pos(&mut self, new_pos: usize);

    #[cfg(feature = "safe-decode")]
    fn extend_with_fill(&mut self, byte: u8, len: usize);

    /// Extends the Sink with `data`.
    fn extend_from_slice(&mut self, data: &[u8]);

    fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize);

    /// Copies `len` bytes starting from `start` to the end of the Sink.
    /// # Panics
    /// Panics if `start` >= `pos`.
    #[cfg(feature = "safe-decode")]
    fn extend_from_within(&mut self, start: usize, wild_len: usize, copy_len: usize);

    #[cfg(feature = "safe-decode")]
    fn extend_from_within_overlapping(&mut self, start: usize, num_bytes: usize);
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
        // SAFETY: Caller guarantees that all elements of `output[..pos]` are initialized.
        let _ = &mut output[..pos]; // bounds check pos
        SliceSink { output, pos }
    }
}

impl<'a> Sink for SliceSink<'a> {
    /// Returns a raw ptr to the first unfilled byte of the Sink. Analogous to `[pos..].as_ptr()`.
    #[inline]
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn pos_mut_ptr(&mut self) -> *mut u8 {
        self.base_mut_ptr().add(self.pos()) as *mut u8
    }

    /// Pushes a byte to the end of the Sink.
    #[inline]
    fn byte_at(&mut self, pos: usize) -> u8 {
        self.output[pos]
    }

    /// Pushes a byte to the end of the Sink.
    #[inline]
    #[cfg(feature = "safe-encode")]
    fn push(&mut self, byte: u8) {
        self.output[self.pos] = byte;
        self.pos += 1;
    }

    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn base_mut_ptr(&mut self) -> *mut u8 {
        self.output.as_mut_ptr()
    }

    #[inline]
    fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.output.len()
    }

    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    #[inline]
    unsafe fn set_pos(&mut self, new_pos: usize) {
        debug_assert!(new_pos <= self.capacity());
        self.pos = new_pos;
    }

    #[inline]
    #[cfg(feature = "safe-decode")]
    fn extend_with_fill(&mut self, byte: u8, len: usize) {
        self.output[self.pos..self.pos + len].fill(byte);
        self.pos += len;
    }

    /// Extends the Sink with `data`.
    #[inline]
    fn extend_from_slice(&mut self, data: &[u8]) {
        self.extend_from_slice_wild(data, data.len())
    }

    #[inline]
    fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize) {
        assert!(copy_len <= data.len());
        slice_copy(data, &mut self.output[self.pos..(self.pos) + data.len()]);
        self.pos += copy_len;
    }

    /// Copies `len` bytes starting from `start` to the end of the Sink.
    /// # Panics
    /// Panics if `start` >= `pos`.
    #[inline]
    #[cfg(feature = "safe-decode")]
    fn extend_from_within(&mut self, start: usize, wild_len: usize, copy_len: usize) {
        self.output.copy_within(start..start + wild_len, self.pos);
        self.pos += copy_len;
    }

    #[inline]
    #[cfg(feature = "safe-decode")]
    #[cfg_attr(nightly, optimize(size))] // to avoid loop unrolling
    fn extend_from_within_overlapping(&mut self, start: usize, num_bytes: usize) {
        let offset = self.pos - start;
        for i in start + offset..start + offset + num_bytes {
            self.output[i] = self.output[i - offset];
        }
        self.pos += num_bytes;
    }
}

/// PtrSink is used as target to de/compress data into a preallocated and possibly uninitialized
/// `&[u8]`
/// space.
///
///
#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
pub struct PtrSink {
    /// The working slice, which may contain uninitialized bytes
    output: *mut u8,
    /// Number of bytes in start of `output` guaranteed to be initialized
    pos: usize,
    /// Number of bytes in output available
    cap: usize,
}

#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
impl PtrSink {
    /// Creates a `Sink` backed by the given byte slice.
    /// `pos` defines the initial output position in the Sink.
    /// # Panics
    /// Panics if `pos` is out of bounds.
    #[inline]
    pub fn from_vec(output: &mut Vec<u8>, pos: usize) -> Self {
        // SAFETY: Bytes behind pointer may be uninitialized.
        Self {
            output: output.as_mut_ptr(),
            pos,
            cap: output.capacity(),
        }
    }
}

#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
impl Sink for PtrSink {
    /// Returns a raw ptr to the first unfilled byte of the Sink. Analogous to `[pos..].as_ptr()`.
    #[inline]
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn pos_mut_ptr(&mut self) -> *mut u8 {
        self.base_mut_ptr().add(self.pos()) as *mut u8
    }

    /// Pushes a byte to the end of the Sink.
    #[inline]
    fn byte_at(&mut self, pos: usize) -> u8 {
        unsafe { self.output.add(pos).read() }
    }

    /// Pushes a byte to the end of the Sink.
    #[inline]
    #[cfg(feature = "safe-encode")]
    fn push(&mut self, byte: u8) {
        unsafe {
            self.pos_mut_ptr().write(byte);
        }
        self.pos += 1;
    }

    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn base_mut_ptr(&mut self) -> *mut u8 {
        self.output
    }

    #[inline]
    fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.cap
    }

    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    #[inline]
    unsafe fn set_pos(&mut self, new_pos: usize) {
        debug_assert!(new_pos <= self.capacity());
        self.pos = new_pos;
    }

    #[inline]
    #[cfg(feature = "safe-decode")]
    fn extend_with_fill(&mut self, _byte: u8, _len: usize) {
        unreachable!();
    }

    /// Extends the Sink with `data`.
    #[inline]
    fn extend_from_slice(&mut self, data: &[u8]) {
        self.extend_from_slice_wild(data, data.len())
    }

    #[inline]
    fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize) {
        assert!(copy_len <= data.len());
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.pos_mut_ptr(), copy_len);
        }
        self.pos += copy_len;
    }

    /// Copies `len` bytes starting from `start` to the end of the Sink.
    /// # Panics
    /// Panics if `start` >= `pos`.
    #[inline]
    #[cfg(feature = "safe-decode")]
    fn extend_from_within(&mut self, _start: usize, _wild_len: usize, _copy_len: usize) {
        unreachable!();
    }

    #[inline]
    #[cfg(feature = "safe-decode")]
    fn extend_from_within_overlapping(&mut self, _start: usize, _num_bytes: usize) {
        unreachable!();
    }
}

#[cfg(test)]
mod tests {

    #[test]
    #[cfg(any(feature = "safe-encode", feature = "safe-decode"))]
    fn test_sink_slice() {
        use crate::sink::Sink;
        use crate::sink::SliceSink;
        use alloc::vec::Vec;
        let mut data = Vec::new();
        data.resize(5, 0);
        let sink = SliceSink::new(&mut data, 1);
        assert_eq!(sink.pos(), 1);
        assert_eq!(sink.capacity(), 5);
    }
}
