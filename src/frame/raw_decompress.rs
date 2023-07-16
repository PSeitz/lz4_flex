use alloc::collections::VecDeque;
use std::{convert::TryInto, hash::Hasher, io, mem::size_of};
use twox_hash::XxHash32;

use super::header::{
    BlockInfo, BlockMode, FrameInfo, LZ4F_LEGACY_MAGIC_NUMBER, MAGIC_NUMBER_SIZE,
    MAX_FRAME_INFO_SIZE, MIN_FRAME_INFO_SIZE,
};
use super::Error;
use crate::{
    block::WINDOW_SIZE,
    sink::{vec_sink_for_decompression, SliceSink},
};

/// A struct for decompressing the LZ4 frame format given pushed byte data
///
/// # Example 1
/// Deserializing json values out of a compressed file.
///
/// ```no_run
/// let mut decoder = lz4_flex::frame::Decoder::new();
/// let compressed_input: Vec<u8> = std::fs::read("datafile").unwrap();
/// decoder.push(&compressed_input);
/// let mut decompressed = Vec::new();
/// while let Ok(v) = decoder.next_block() {
///     decompressed.extend(v);
/// }
/// let json: serde_json::Value = serde_json::from_slice(&decompressed).unwrap();
/// ```\
/// ```
#[derive(Debug, Default)]
pub struct Decoder {
    /// The bytes we have been given so far
    raw: VecDeque<u8>,
    /// The FrameInfo of the frame currently being decoded.
    /// It starts as `None` and is filled with the FrameInfo is read from the input.
    /// It's reset to `None` once the frame EndMarker is read from the input.
    current_frame_info: Option<FrameInfo>,
    /// Xxhash32 used when content checksum is enabled.
    content_hasher: XxHash32,
    /// Total length of decompressed output for the current frame.
    content_len: u64,
    /// The compressed bytes buffer, taken from the underlying reader.
    src: Vec<u8>,
    /// The decompressed bytes buffer. Bytes are decompressed from src to dst
    /// before being passed back to the caller.
    dst: Vec<u8>,
    /// Index into dst and length: starting point of bytes previously output
    /// that are still part of the decompressor window.
    ext_dict_offset: usize,
    ext_dict_len: usize,
    /// Index into dst: starting point of bytes not yet read by caller.
    dst_start: usize,
    /// Index into dst: ending point of bytes not yet read by caller.
    dst_end: usize,
}

impl Decoder {
    /// Creates a new Decoder for the specified reader.
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// Provide compressed data to decompress
    pub fn push(&mut self, bytes: &[u8]) {
        self.raw.extend(bytes);
    }
    fn read_exact(raw: &mut VecDeque<u8>, buffer: &mut [u8]) -> Result<(), Error> {
        if raw.len() < buffer.len() {
            return Err(Error::NeedMoreInput(buffer.len() - raw.len()));
        }
        for o in buffer.iter_mut() {
            *o = raw.pop_front().unwrap();
        }
        Ok(())
    }

    fn read_frame_info(&mut self) -> Result<usize, Error> {
        let mut buffer = [0u8; MAX_FRAME_INFO_SIZE];

        if self.raw.is_empty() {
            return Ok(0);
        }
        Self::read_exact(&mut self.raw, &mut buffer[0..MAGIC_NUMBER_SIZE])?;

        if u32::from_le_bytes(buffer[0..MAGIC_NUMBER_SIZE].try_into().unwrap())
            != LZ4F_LEGACY_MAGIC_NUMBER
        {
            if self.raw.is_empty() {
                return Ok(0);
            }
            Self::read_exact(
                &mut self.raw,
                &mut buffer[MAGIC_NUMBER_SIZE..MIN_FRAME_INFO_SIZE],
            )?;
        }
        let required = FrameInfo::read_size(&buffer[..MIN_FRAME_INFO_SIZE])?;
        if required != MIN_FRAME_INFO_SIZE && required != MAGIC_NUMBER_SIZE {
            Self::read_exact(&mut self.raw, &mut buffer[MIN_FRAME_INFO_SIZE..required])?;
        }

        let frame_info = FrameInfo::read(&buffer[..required])?;
        if frame_info.dict_id.is_some() {
            // Unsupported right now so it must be None
            return Err(Error::DictionaryNotSupported);
        }

        let max_block_size = frame_info.block_size.get_size();
        let dst_size = if frame_info.block_mode == BlockMode::Linked {
            // In linked mode we consume the output (bumping dst_start) but leave the
            // beginning of dst to be used as a prefix in subsequent blocks.
            // That is at least until we have at least `max_block_size + WINDOW_SIZE`
            // bytes in dst, then we setup an ext_dict with the last WINDOW_SIZE bytes
            // and the output goes to the beginning of dst again.
            // Since we always want to be able to write a full block (up to max_block_size)
            // we need a buffer with at least `max_block_size * 2 + WINDOW_SIZE` bytes.
            max_block_size * 2 + WINDOW_SIZE
        } else {
            max_block_size
        };
        self.src.clear();
        self.dst.clear();
        self.src.reserve_exact(max_block_size);
        self.dst.reserve_exact(dst_size);
        self.current_frame_info = Some(frame_info);
        self.content_hasher = XxHash32::with_seed(0);
        self.content_len = 0;
        self.ext_dict_len = 0;
        self.dst_start = 0;
        self.dst_end = 0;
        Ok(required)
    }

    #[inline]
    fn read_checksum(&mut self) -> Result<u32, io::Error> {
        let mut checksum_buffer = [0u8; size_of::<u32>()];
        Self::read_exact(&mut self.raw, &mut checksum_buffer[..])?;
        let checksum = u32::from_le_bytes(checksum_buffer);
        Ok(checksum)
    }

    #[inline]
    fn check_block_checksum(data: &[u8], expected_checksum: u32) -> Result<(), io::Error> {
        let mut block_hasher = XxHash32::with_seed(0);
        block_hasher.write(data);
        let calc_checksum = block_hasher.finish() as u32;
        if calc_checksum != expected_checksum {
            return Err(Error::BlockChecksumError.into());
        }
        Ok(())
    }

    fn read_block(&mut self) -> Result<usize, Error> {
        if self.current_frame_info.is_none() && self.read_frame_info()? == 0 {
            return Ok(0);
        }

        debug_assert_eq!(self.dst_start, self.dst_end);
        let frame_info = self.current_frame_info.clone().unwrap();

        // Adjust dst buffer offsets to decompress the next block
        let max_block_size = frame_info.block_size.get_size();
        if frame_info.block_mode == BlockMode::Linked {
            // In linked mode we consume the output (bumping dst_start) but leave the
            // beginning of dst to be used as a prefix in subsequent blocks.
            // That is at least until we have at least `max_block_size + WINDOW_SIZE`
            // bytes in dst, then we setup an ext_dict with the last WINDOW_SIZE bytes
            // and the output goes to the beginning of dst again.
            debug_assert_eq!(self.dst.capacity(), max_block_size * 2 + WINDOW_SIZE);
            if self.dst_start + max_block_size > self.dst.capacity() {
                // Output might not fit in the buffer.
                // The ext_dict will become the last WINDOW_SIZE bytes
                debug_assert!(self.dst_start >= max_block_size + WINDOW_SIZE);
                self.ext_dict_offset = self.dst_start - WINDOW_SIZE;
                self.ext_dict_len = WINDOW_SIZE;
                // Output goes in the beginning of the buffer again.
                self.dst_start = 0;
                self.dst_end = 0;
            } else if self.dst_start + self.ext_dict_len > WINDOW_SIZE {
                // There's more than WINDOW_SIZE bytes of lookback adding the prefix and ext_dict.
                // Since we have a limited buffer we must shrink ext_dict in favor of the prefix,
                // so that we can fit up to max_block_size bytes between dst_start and ext_dict
                // start.
                let delta = self
                    .ext_dict_len
                    .min(self.dst_start + self.ext_dict_len - WINDOW_SIZE);
                self.ext_dict_offset += delta;
                self.ext_dict_len -= delta;
                debug_assert!(self.dst_start + self.ext_dict_len >= WINDOW_SIZE)
            }
        } else {
            debug_assert_eq!(self.ext_dict_len, 0);
            debug_assert_eq!(self.dst.capacity(), max_block_size);
            self.dst_start = 0;
            self.dst_end = 0;
        }

        // Read and decompress block
        let block_info = {
            let mut buffer = [0u8; 4];
            if let Err(err) = Self::read_exact(&mut self.raw, &mut buffer) {
                if let Error::NeedMoreInput(_) = err {
                    return Ok(0);
                } else {
                    return Err(err);
                }
            }
            BlockInfo::read(&buffer)?
        };
        match block_info {
            BlockInfo::Uncompressed(len) => {
                let len = len as usize;
                if len > max_block_size {
                    return Err(Error::BlockTooBig);
                }
                // TODO: Attempt to avoid initialization of read buffer when
                // https://github.com/rust-lang/rust/issues/42788 stabilizes
                Self::read_exact(
                    &mut self.raw,
                    vec_resize_and_get_mut(&mut self.dst, self.dst_start, self.dst_start + len),
                )?;
                if frame_info.block_checksums {
                    let expected_checksum = self.read_checksum()?;
                    Self::check_block_checksum(
                        &self.dst[self.dst_start..self.dst_start + len],
                        expected_checksum,
                    )?;
                }

                self.dst_end += len;
                self.content_len += len as u64;
            }
            BlockInfo::Compressed(len) => {
                let len = len as usize;
                if len > max_block_size {
                    return Err(Error::BlockTooBig);
                }
                // TODO: Attempt to avoid initialization of read buffer when
                // https://github.com/rust-lang/rust/issues/42788 stabilizes
                Self::read_exact(&mut self.raw, vec_resize_and_get_mut(&mut self.src, 0, len))?;
                if frame_info.block_checksums {
                    let expected_checksum = self.read_checksum()?;
                    Self::check_block_checksum(&self.src[..len], expected_checksum)?;
                }

                let with_dict_mode =
                    frame_info.block_mode == BlockMode::Linked && self.ext_dict_len != 0;
                let decomp_size = if with_dict_mode {
                    debug_assert!(self.dst_start + max_block_size <= self.ext_dict_offset);
                    let (head, tail) = self.dst.split_at_mut(self.ext_dict_offset);
                    let ext_dict = &tail[..self.ext_dict_len];

                    debug_assert!(head.len() - self.dst_start >= max_block_size);
                    crate::block::decompress::decompress_internal::<true, _>(
                        &self.src[..len],
                        &mut SliceSink::new(head, self.dst_start),
                        ext_dict,
                    )
                } else {
                    // Independent blocks OR linked blocks with only prefix data
                    debug_assert!(self.dst.capacity() - self.dst_start >= max_block_size);
                    crate::block::decompress::decompress_internal::<false, _>(
                        &self.src[..len],
                        &mut vec_sink_for_decompression(
                            &mut self.dst,
                            0,
                            self.dst_start,
                            self.dst_start + max_block_size,
                        ),
                        b"",
                    )
                }
                .map_err(Error::DecompressionError)?;

                self.dst_end += decomp_size;
                self.content_len += decomp_size as u64;
            }

            BlockInfo::EndMark => {
                if let Some(expected) = frame_info.content_size {
                    if self.content_len != expected {
                        return Err(Error::ContentLengthError {
                            expected,
                            actual: self.content_len,
                        });
                    }
                }
                if frame_info.content_checksum {
                    let expected_checksum = self.read_checksum()?;
                    let calc_checksum = self.content_hasher.finish() as u32;
                    if calc_checksum != expected_checksum {
                        return Err(Error::ContentChecksumError);
                    }
                }
                self.current_frame_info = None;
                return Ok(0);
            }
        }

        // Content checksum, if applicable
        if frame_info.content_checksum {
            self.content_hasher
                .write(&self.dst[self.dst_start..self.dst_end]);
        }

        Ok(self.dst_end - self.dst_start)
    }

    /// Read the next block of decompressed data.
    /// 
    /// When using this function, unless it is known that all data has already
    /// been pushed, the user should check for an `Error::NeedMoreData` to see
    /// if more data is needed.
    pub fn next_block(&mut self) -> Result<&[u8], Error> {
        self.read_block()?;
        let start = self.dst_start;
        self.dst_start = self.dst_end;
        Ok(&self.dst[start..self.dst_end])
    }
}

/// Similar to `v.get_mut(start..end) but will adjust the len if needed.
#[inline]
fn vec_resize_and_get_mut(v: &mut Vec<u8>, start: usize, end: usize) -> &mut [u8] {
    if end > v.len() {
        v.resize(end, 0)
    }
    &mut v[start..end]
}
