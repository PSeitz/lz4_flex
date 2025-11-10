use core::mem::MaybeUninit;

#[allow(unused_imports)]
use alloc::vec::Vec;

use crate::fastcpy::slice_copy;

/// Returns a Sink implementation appropriate for outputting up to `required_capacity`
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
) -> SliceSink<'_> {
    {
        vec.resize(offset + required_capacity, 0);
        SliceSink::new(&mut vec[offset..], pos)
    }
}

/// Returns a Sink implementation appropriate for outputting up to `required_capacity`
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
) -> SliceSink<'_> {
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
    output: &'a mut [MaybeUninit<u8>],
    /// Number of bytes in start of `output` guaranteed to be initialized
    pos: usize,
}

#[inline(always)]
pub(crate) unsafe fn cast_slice_mut<T, U>(slice: &mut [T]) -> &mut [U] {
    // Safety: caller ensures T's representation is compatible with U
    unsafe { core::slice::from_raw_parts_mut(slice.as_mut_ptr().cast::<U>(), slice.len()) }
}
#[inline(always)]
pub(crate) unsafe fn cast_slice<T, U>(slice: &[T]) -> &[U] {
    // Safety: caller ensures T's representation is compatible with U
    unsafe { core::slice::from_raw_parts(slice.as_ptr().cast::<U>(), slice.len()) }
}
#[inline(always)]
pub(crate) fn slice_ref_to_uninit(slice: &[u8]) -> &[MaybeUninit<u8>] {
    unsafe { cast_slice(slice) }
}
#[inline(always)]
pub(crate) fn slice_mut_to_uninit(slice: &mut [u8]) -> &mut [MaybeUninit<u8>] {
    unsafe { cast_slice_mut(slice) }
}

impl<'a> SliceSink<'a> {
    /// Creates a [`Sink`] backed by the given byte slice.
    /// `pos` defines the initial output position in the Sink.
    /// # Panics
    /// Panics if `pos` is out of bounds.
    #[inline]
    pub fn new(output: &'a mut [u8], pos: usize) -> Self {
        let _ = &mut output[..pos]; // bounds check pos

        // Safety: all bytes are initialized, therefore all bytes prior to `pos` must be initialized
        unsafe { Self::new_uninit(slice_mut_to_uninit(output), pos) }
    }
    /// Creates a [`Sink`] backed by the given byte slice.
    /// `pos` defines the initial output position in the Sink.
    /// # Panics
    /// Panics if `pos` is out of bounds.
    /// # Safety
    /// Caller must ensure that output[..pos] is initialized
    #[inline]
    pub unsafe fn new_uninit(output: &'a mut [MaybeUninit<u8>], pos: usize) -> Self {
        let _ = &mut output[..pos];
        SliceSink { output, pos }
    }
    /// Creates a [`Sink`] backed by the given byte slice, with the starting position set to 0.
    #[inline]
    #[allow(dead_code)]
    pub fn new_uninit_zero_pos(output: &'a mut [MaybeUninit<u8>]) -> Self {
        SliceSink { output, pos: 0 }
    }
    /// # Safety
    /// Caller must ensure that the bytes in the range `pos..(pos + amount)` are initialized
    #[inline]
    pub unsafe fn bump_pos(&mut self, amount: usize) {
        self.pos += amount;
    }
    /// Gets a mutable reference to the initialized portion of this [`SliceSink`]
    #[inline]
    pub fn init_part(&mut self) -> &mut [u8] {
        // Safety: by the construction invariant, all bytes in ..self.pos are guaranteed to be initialized
        unsafe { cast_slice_mut::<MaybeUninit<u8>, u8>(&mut self.output[..self.pos]) }
    }
}

impl Sink for SliceSink<'_> {
    /// Returns a raw ptr to the first unfilled byte of the Sink. Analogous to `[pos..].as_ptr()`.
    #[inline]
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn pos_mut_ptr(&mut self) -> *mut u8 {
        self.base_mut_ptr().add(self.pos()) as *mut u8
    }

    #[inline]
    fn byte_at(&mut self, pos: usize) -> u8 {
        self.init_part()[pos]
    }

    #[inline]
    #[cfg(feature = "safe-encode")]
    fn push(&mut self, byte: u8) {
        self.output[self.pos].write(byte);
        // Safety: byte at `self.pos` is initialized above
        unsafe {
            self.bump_pos(1);
        }
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
        self.output[self.pos..(self.pos + len)].fill(MaybeUninit::new(byte));
        // Safety: bytes in `self.pos..self.pos + len` are initialized above
        unsafe {
            self.bump_pos(len);
        }
    }

    #[inline]
    fn extend_from_slice(&mut self, data: &[u8]) {
        self.extend_from_slice_wild(data, data.len())
    }

    #[inline]
    fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize) {
        assert!(copy_len <= data.len());
        slice_copy(data, &mut self.output[self.pos..(self.pos) + data.len()]);
        // Safety: bytes in `self.pos..self.pos + copy_len` are initialized above
        unsafe {
            self.bump_pos(copy_len);
        }
    }

    #[inline]
    #[cfg(feature = "safe-decode")]
    fn extend_from_within(&mut self, start: usize, wild_len: usize, copy_len: usize) {
        assert!(start + copy_len <= self.pos);
        self.output.copy_within(start..(start + wild_len), self.pos);
        // Safety: assert ensures bytes in `..(start + copy_len)` are all initialized,
        // bytes in `self.pos..(self.pos + copy_len)` are written above.
        unsafe {
            self.bump_pos(copy_len);
        }
    }

    #[inline]
    #[cfg(feature = "safe-decode")]
    #[cfg_attr(feature = "nightly", optimize(size))] // to avoid loop unrolling
    fn extend_from_within_overlapping(&mut self, start: usize, num_bytes: usize) {
        let offset = self.pos - start;
        for i in start + offset..start + offset + num_bytes {
            self.output[i] = self.output[i - offset];
        }
        // Safety: bytes in `self.pos..self.pos + num_bytes` are initialized above
        unsafe {
            self.bump_pos(num_bytes);
        }
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
        let mut data = vec![0; 5];
        let sink = SliceSink::new(&mut data, 1);
        assert_eq!(sink.pos(), 1);
        assert_eq!(sink.capacity(), 5);
    }
}
