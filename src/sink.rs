use alloc::vec::Vec;
#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
use core::mem::MaybeUninit;

/// Returns a Sink implementation appropriate for outputing up to `required_capacity`
/// bytes at `vec[offset..offset+required_capacity]`.
/// It can be either a `SliceSink` (pre-filling the vec with zeroes if necessary)
/// when the `safe-decode` feature is enabled, or `VecSink` otherwise.
/// The argument `pos` defines the initial output position in the Sink.
#[inline]
pub fn vec_sink_for_compression(
    vec: &mut Vec<u8>,
    offset: usize,
    pos: usize,
    required_capacity: usize,
) -> impl Sink + '_ {
    #[cfg(not(feature = "safe-encode"))]
    return {
        assert!(vec.capacity() >= offset + required_capacity);
        VecSink::new(vec, offset, pos)
    };

    #[cfg(feature = "safe-encode")]
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
) -> impl Sink + '_ {
    #[cfg(not(feature = "safe-decode"))]
    return {
        assert!(vec.capacity() >= offset + required_capacity);
        crate::sink::VecSink::new(vec, offset, pos)
    };

    #[cfg(feature = "safe-decode")]
    return {
        vec.resize(offset + required_capacity, 0);
        SliceSink::new(&mut vec[offset..], pos)
    };
}

/// Sink is used as target to de/compress data into a preallocated and possibly uninitialized memory
/// space.
///
/// # Handling of Capacity
/// Extend methods will panic if there's insufficient capacity left in the Sink.
///
/// # Invariants
///   - Bytes `[..pos()]` are always initialized.
pub trait Sink {
    /// The bytes that are considered filled.
    fn filled_slice(&self) -> &[u8];

    /// The current position (aka. len) of the the Sink.
    fn pos(&self) -> usize;

    /// The total capacity of the Sink.
    fn capacity(&self) -> usize;

    /// Forces the length of the vector to `new_pos`.
    /// The caller is responsible for ensuring all bytes up to `new_pos` are properly initialized.
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn set_pos(&mut self, new_pos: usize);

    /// Returns a raw ptr to the first byte of the Sink. Analogous to `[0..].as_ptr()`.
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn base_mut_ptr(&mut self) -> *mut u8;

    /// Returns a raw ptr to the first unfilled byte of the Sink. Analogous to `[pos..].as_ptr()`.
    #[inline]
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn pos_mut_ptr(&mut self) -> *mut u8 {
        self.base_mut_ptr().add(self.pos()) as *mut u8
    }

    /// Pushes a byte to the end of the Sink.
    #[inline]
    fn push(&mut self, byte: u8) {
        self.extend_from_slice(&[byte])
    }

    /// Pushes `len` elements of `byte` to the end of the Sink.
    fn extend_with_fill(&mut self, byte: u8, len: usize);

    /// Extends the Sink with `data` but only advances the sink by `copy_len`.
    /// # Panics
    /// Panics if `copy_len` > `data.len()`
    fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize);

    /// Extends the Sink with `data`.
    #[inline]
    fn extend_from_slice(&mut self, data: &[u8]) {
        self.extend_from_slice_wild(data, data.len())
    }

    /// Copies `wild_len` bytes starting from `start` to the end of the Sink.
    /// The position of the Sink is increased by `copy_len` NOT `wild_len`;
    /// # Panics
    /// Panics if `start + copy_len` > `pos`.
    /// Panics if `copy_len` > `wild_len`.
    fn extend_from_within_wild(&mut self, start: usize, wild_len: usize, copy_len: usize);

    /// Copies `len` bytes starting from `start` to the end of the Sink.
    /// # Panics
    /// Panics if `start` >= `pos`.
    #[inline]
    fn extend_from_within(&mut self, start: usize, len: usize) {
        self.extend_from_within_wild(start, len, len)
    }

    /// Copies `len` bytes starting from `start` to the end of the Sink.
    /// Contrary to `extend_from_within`, the copy output can overlap with
    /// the copy source bytes.
    /// In addition, a copy with `start` == `pos` is valid and will
    /// fill the sink `len` zero bytes.
    /// # Panics
    /// Panics if `start` > `pos`.
    fn extend_from_within_overlapping(&mut self, start: usize, len: usize);
}

/// A Sink baked by a &[u8]
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

impl<'a> Sink for SliceSink<'a> {
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    unsafe fn base_mut_ptr(&mut self) -> *mut u8 {
        self.output.as_mut_ptr()
    }

    #[inline]
    fn filled_slice(&self) -> &[u8] {
        &self.output[..self.pos]
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
    fn extend_with_fill(&mut self, byte: u8, len: usize) {
        self.output[self.pos..self.pos + len].fill(byte);
        self.pos += len;
    }

    #[inline]
    fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize) {
        assert!(copy_len <= data.len());
        self.output[self.pos..self.pos + data.len()].copy_from_slice(data);
        self.pos += copy_len;
    }

    #[inline]
    fn extend_from_within_wild(&mut self, start: usize, wild_len: usize, copy_len: usize) {
        // Safety checks so that set_pos later don't expose uninitialized data
        assert!(copy_len <= wild_len);
        assert!(start + copy_len <= self.pos);
        self.output.copy_within(start..start + wild_len, self.pos);
        self.pos += copy_len;
    }

    #[inline]
    fn extend_from_within_overlapping(&mut self, start: usize, len: usize) {
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

#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
/// A sink backed by a Vec<u8>
///
/// Due to the intricacies of uninitialized memory in Rust this
/// implementation requires using unsafe code even though it is
/// fully bounds checked and never exposes uninitialized bytes.
pub struct VecSink<'a> {
    /// The backing vec
    output: &'a mut Vec<u8>,
    /// The output base ptr, a valid pointer within `output` data
    output_ptr: *mut u8,
    /// Number of bytes written after `output_ptr`
    pos: usize,
    /// Number of bytes available after `output_ptr`
    capacity: usize,
}

#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
impl<'a> VecSink<'a> {
    /// Creates a `Sink` backed by the Vec bytes at `vec[offset..vec.capacity()]`.
    /// `pos` defines the initial output position (counted from `offset`) in the Sink.
    /// Note that the bytes at `vec[output.len()..]` are actually uninitialized and will
    /// not be readable until written.
    /// When the `Sink` is dropped the Vec len will be adjusted to `offset` + `Sink.pos`.
    /// # Panics
    /// Panics if `pos` is out of bounds.
    #[inline]
    pub fn new(output: &'a mut Vec<u8>, offset: usize, pos: usize) -> VecSink<'a> {
        // The truncation also works as bounds checking for offset and pos.
        output.truncate(offset + pos);
        VecSink {
            capacity: output.capacity() - offset,
            output_ptr: unsafe { output.as_mut_ptr().add(offset) },
            output,
            pos,
        }
    }
}
#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
impl<'a> VecSink<'a> {
    #[inline]
    fn buffer_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        // SAFETY: VecSink output_ptr points to at least capacity bytes in length
        unsafe {
            core::slice::from_raw_parts_mut(self.output_ptr as *mut MaybeUninit<u8>, self.capacity)
        }
    }
}

#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
impl<'a> Sink for VecSink<'a> {
    unsafe fn base_mut_ptr(&mut self) -> *mut u8 {
        self.output_ptr
    }

    #[inline]
    fn filled_slice(&self) -> &[u8] {
        // SAFETY: Sink safety invariant is that all bytes up to pos are initialized.
        debug_assert!(self.pos <= self.capacity);
        unsafe { core::slice::from_raw_parts(self.output_ptr, self.pos) }
    }

    #[inline]
    fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    unsafe fn set_pos(&mut self, new_pos: usize) {
        self.pos = new_pos
    }

    #[inline]
    fn extend_with_fill(&mut self, byte: u8, len: usize) {
        let pos = self.pos;
        self.buffer_mut()[pos..pos + len].fill(MaybeUninit::new(byte));
        self.pos += len;
    }

    #[inline]
    fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize) {
        assert!(copy_len <= data.len());
        let pos = self.pos;
        self.buffer_mut()[pos..pos + data.len()].copy_from_slice(slice_as_uninit_ref(data));
        self.pos += copy_len;
    }

    #[inline]
    fn extend_from_within_wild(&mut self, start: usize, wild_len: usize, copy_len: usize) {
        // Safety checks so that pos adjustment doesn't expose uninitialized data
        assert!(copy_len <= wild_len);
        assert!(start + copy_len <= self.pos);
        let pos = self.pos;
        self.buffer_mut().copy_within(start..start + wild_len, pos);
        self.pos += copy_len;
    }

    #[inline]
    fn extend_from_within_overlapping(&mut self, start: usize, len: usize) {
        // Sink safety invariant guarantees that the first `pos` items are always initialized.
        assert!(start <= self.pos);
        let offset = self.pos - start;
        let pos = self.pos;
        let out = &mut self.buffer_mut()[start..pos + len];
        out[offset] = MaybeUninit::new(0); // ensures that a copy w/ start == pos becomes a zero fill
        for i in offset..out.len() {
            out[i] = out[i - offset];
        }
        self.pos += len;
    }
}

#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
impl<'a> Drop for VecSink<'a> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            let offset = self.output_ptr.offset_from(self.output.as_ptr()) as usize;
            self.output.set_len(offset + self.pos);
        }
    }
}

#[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
#[inline]
fn slice_as_uninit_ref(slice: &[u8]) -> &[MaybeUninit<u8>] {
    // SAFETY: `&[T]` is guaranteed to have the same layout as `&[MaybeUninit<T>]`
    unsafe { core::slice::from_raw_parts(slice.as_ptr() as *const MaybeUninit<u8>, slice.len()) }
}

#[cfg(test)]
mod tests {
    use crate::sink::SliceSink;
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    use crate::sink::VecSink;

    use super::{Sink, Vec};

    #[test]
    fn test_sink_slice() {
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

    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    #[test]
    fn test_sink_vec() {
        let mut data = Vec::with_capacity(5);
        data.push(255); // not visible to the sink
        data.push(0);
        {
            let mut sink = VecSink::new(&mut data, 1, 1);
            assert_eq!(sink.pos(), 1);
            assert_eq!(sink.capacity(), 4);
            assert_eq!(sink.filled_slice(), &[0]);
            sink.extend_from_slice(&[1, 2, 3]);
            assert_eq!(sink.pos(), 4);
            assert_eq!(sink.filled_slice(), &[0, 1, 2, 3]);
        }
        assert_eq!(data.as_slice(), &[255, 0, 1, 2, 3]);
    }
}
