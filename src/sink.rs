use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use alloc::vec::Vec;

/// Sink is used as target to de/compress data into a preallocated and possibly uninitialized memory space.
/// Sink can be created from a `Vec` or a `Slice`. The new pos on the data after the operation
/// can be retrieved via `sink.pos()`.
///
/// # Handling of Capacity
/// Extend methods will panic if there's insufficient capacity left in the Sink.
///
/// # Safety invariants
///   - `self.output[.. self.pos]` is always initialized.
pub struct Sink<'a> {
    /// The working slice, which may contain uninitialized bytes
    output: &'a mut [MaybeUninit<u8>],
    /// The Sink write position.
    /// Also the number of bytes from the start of `output` guaranteed to be initialized.
    pos: usize,
}

impl<'a> Sink<'a> {
    /// Creates a `Sink` backed by the given byte slice.
    #[inline]
    pub fn new(output: &'a mut [u8], pos: usize) -> Self {
        // SAFETY: Caller guarantees that all elements of `output` are initialized.
        let _ = &mut output[..pos]; // bounds check pos
        Sink {
            output: slice_as_uninit_mut(output),
            pos,
        }
    }
}

impl<'a> Sink<'a> {
    /// Pushes a byte to the end of the Sink.
    #[cfg(any(feature = "safe-encode", feature = "safe-decode"))]
    #[inline]
    pub fn push(&mut self, byte: u8) {
        self.output[self.pos] = MaybeUninit::new(byte);
        self.pos += 1;
    }

    /// Pushes `len` elements of `byte` to the end of the Sink.
    /// # Panics
    /// Panics if `copy_len` > `data.len()`.
    #[cfg(any(feature = "safe-encode", feature = "safe-decode"))]
    #[inline]
    pub fn extend_with_fill(&mut self, byte: u8, len: usize) {
        self.output[self.pos..self.pos + len].fill(MaybeUninit::new(byte));
        self.pos += len;
    }

    /// Extends the Sink with `data` but only advances the sink by `copy_len`.
    /// # Panics
    /// Panics if `copy_len` > `data.len()`.
    #[inline]
    pub fn extend_from_slice_wild(&mut self, data: &[u8], copy_len: usize) {
        // SAFETY: The assertion prevent Sink from exposing uninitialized data.
        assert!(copy_len <= data.len());
        self.output[self.pos..self.pos + data.len()].copy_from_slice(slice_as_uninit_ref(data));
        self.pos += copy_len;
    }

    /// Extends the Sink with `data`.
    #[inline]
    pub fn extend_from_slice(&mut self, data: &[u8]) {
        self.extend_from_slice_wild(data, data.len())
    }

    /// Copies `wild_len` bytes starting from `start` to the end of the Sink.
    /// The position of the Sink is increased by `copy_len` NOT `wild_len`;
    /// # Panics
    /// Panics if `start + copy_len` > `pos`.
    /// Panics if `copy_len` > `wild_len`.
    #[cfg(any(feature = "safe-encode", feature = "safe-decode"))]
    #[inline]
    pub fn extend_from_within_wild(&mut self, start: usize, wild_len: usize, copy_len: usize) {
        // SAFETY: The assertions prevent Sink from exposing uninitialized data.
        assert!(copy_len <= wild_len);
        assert!(start + copy_len <= self.pos);
        self.output.copy_within(start..start + wild_len, self.pos);
        self.pos += copy_len;
    }

    /// Copies `len` bytes starting from `start` to the end of the Sink.
    /// # Panics
    /// Panics if `start` >= `pos`.
    #[cfg(any(feature = "safe-encode", feature = "safe-decode"))]
    #[inline]
    pub fn extend_from_within(&mut self, start: usize, len: usize) {
        self.extend_from_within_wild(start, len, len)
    }

    /// Copies `len` bytes starting from `start` to the end of the Sink.
    /// Contrary to `extend_from_within`, the copy destination can overlap with
    /// (self-reference) the copy source bytes.
    /// In addition, a copy with `start` == `pos` is valid and will
    /// fill the sink `len` zero bytes.
    /// # Panics
    /// Panics if `start` > `pos`.
    #[cfg(any(feature = "safe-encode", feature = "safe-decode"))]
    #[inline]
    pub fn extend_from_within_overlapping(&mut self, start: usize, len: usize) {
        // SAFETY: Sink safety invariant guarantees that the first `pos` items are initialized.
        // But also accept start == pos as described in the function documentation.
        assert!(start <= self.pos);
        let offset = self.pos - start;
        let out = &mut self.output[start..self.pos + len];
        // Ensures that a copy w/ start == pos becomes a zero fill.
        // This is the same behavior as the reference implementation.
        out[offset] = MaybeUninit::new(0);
        for i in offset..out.len() {
            out[i] = out[i - offset];
        }
        self.pos += len;
    }

    /// Returns a raw ptr to the first byte of the Sink. Analogous to `[0..].as_ptr()`.
    /// Note that the data under the pointer might be uninitialized.
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    #[inline]
    pub unsafe fn base_mut_ptr(&mut self) -> *mut u8 {
        // SAFETY: `MaybeUninit<T>` is guaranteed to have the same layout as `T`
        self.output.as_mut_ptr() as *mut u8
    }

    /// Returns a raw ptr to the first unfilled byte of the Sink. Analogous to `[pos..].as_ptr()`.
    /// Note that the data under the pointer might be uninitialized.
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    #[inline]
    pub unsafe fn pos_mut_ptr(&mut self) -> *mut u8 {
        // SAFETY: `MaybeUninit<T>` is guaranteed to have the same layout as `T`
        self.output.as_mut_ptr().add(self.pos) as *mut u8
    }

    /// The current position (aka. len) of the the Sink.
    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// The total capacity of the Sink.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.output.len()
    }

    /// Forces the length of the vector to `new_pos`.
    /// The caller is responsible for ensuring all bytes up to `new_pos` are properly initialized.
    #[cfg(not(all(feature = "safe-encode", feature = "safe-decode")))]
    #[inline]
    pub unsafe fn set_pos(&mut self, new_pos: usize) {
        debug_assert!(new_pos <= self.capacity());
        self.pos = new_pos;
    }

    /// Returns the initialized section of the Sink. Analogous to `&[..pos]`.
    #[cfg(any(test, feature = "safe-decode"))]
    #[inline]
    pub fn filled_slice(&self) -> &[u8] {
        // SAFETY: from the safety invariant.
        unsafe { core::slice::from_raw_parts_mut(self.output.as_ptr() as *mut u8, self.pos) }
    }
}

/// A Sink wrapper backed by a Vec<u8>.
pub struct VecSink<'a> {
    sink: Sink<'a>,
    offset: usize,
    vec_ptr: *mut Vec<u8>,
}

impl<'a> VecSink<'a> {
    /// Creates a `Sink` backed by the Vec bytes at `vec[offset..vec.capacity()]`.
    /// Note that the bytes at `vec[output.len()..]` are actually uninitialized and will
    /// not be readable until written.
    /// When the `Sink` is dropped the Vec len will be adjusted to `offset` + `Sink.pos`.
    #[inline]
    pub fn new(output: &'a mut Vec<u8>, offset: usize, pos: usize) -> Self {
        // SAFETY: Only the first `output.len` in `output` are initialized.
        // Assert that the range output[offset + pos] is all initialized data.
        let _ = &output[..offset + pos];

        // SAFETY: Derive the pointer first, for stacked borrows reasons.
        let vec_ptr = output as *mut Vec<u8>;

        // SAFETY: `Vec` guarantees that `capacity` elements are available from `as_mut_ptr`.
        // Only the first `output.len` elements are actually initialized but we use a slice of `MaybeUninit` for the entire range.
        // `MaybeUninit<T>` is guaranteed to have the same layout as `T`.
        let vec_with_spare = unsafe {
            core::slice::from_raw_parts_mut(
                output.as_mut_ptr() as *mut MaybeUninit<u8>,
                output.capacity(),
            )
        };
        VecSink {
            sink: Sink {
                output: &mut vec_with_spare[offset..],
                pos,
            },
            offset,
            vec_ptr,
        }
    }
}

impl<'a> Deref for VecSink<'a> {
    type Target = Sink<'a>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.sink
    }
}

impl<'a> DerefMut for VecSink<'a> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sink
    }
}

impl<'a> Drop for VecSink<'a> {
    #[inline]
    fn drop(&mut self) {
        unsafe { (&mut *self.vec_ptr).set_len(self.offset + self.sink.pos) }
    }
}

#[inline]
fn slice_as_uninit_ref(slice: &[u8]) -> &[MaybeUninit<u8>] {
    // SAFETY: `&[T]` is guaranteed to have the same layout as `&[MaybeUninit<T>]`
    unsafe { core::slice::from_raw_parts(slice.as_ptr() as *mut MaybeUninit<u8>, slice.len()) }
}

#[inline]
fn slice_as_uninit_mut(slice: &mut [u8]) -> &mut [MaybeUninit<u8>] {
    // SAFETY: `&mut [T]` is guaranteed to have the same layout as `&mut [MaybeUninit<T>]`
    unsafe {
        core::slice::from_raw_parts_mut(slice.as_mut_ptr() as *mut MaybeUninit<u8>, slice.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sink_slice() {
        let mut data = Vec::new();
        data.resize(5, 0);
        let mut sink = Sink::new(&mut data, 1);
        assert_eq!(sink.pos(), 1);
        assert_eq!(sink.capacity(), 5);
        assert_eq!(sink.filled_slice(), &[0]);
        sink.extend_from_slice(&[1, 2, 3]);
        assert_eq!(sink.pos(), 4);
        assert_eq!(sink.filled_slice(), &[0, 1, 2, 3]);
    }

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
