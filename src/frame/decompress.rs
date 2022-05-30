use std::{
    fmt,
    hash::Hasher,
    io::{self, BufRead},
    mem::size_of,
};
use twox_hash::XxHash32;

use super::header::{BlockInfo, BlockMode, FrameInfo, MAX_FRAME_INFO_SIZE, MIN_FRAME_INFO_SIZE};
use super::Error;
use crate::{
    block::WINDOW_SIZE,
    sink::{vec_sink_for_decompression, SliceSink},
};

/// A reader for decompressing the LZ4 frame format
///
/// This Decoder wraps any other reader that implements `io::Read`.
/// Bytes read will be decompressed according to the [LZ4 frame format](
/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md).
///
/// # Example 1
/// Deserializing json values out of a compressed file.
///
/// ```no_run
/// let compressed_input = std::fs::File::open("datafile").unwrap();
/// let mut decompressed_input = lz4_flex::frame::FrameDecoder::new(compressed_input);
/// let json: serde_json::Value = serde_json::from_reader(decompressed_input).unwrap();
/// ```
///
/// # Example
/// Deserializing multiple json values out of a compressed file
///
/// ```no_run
/// let compressed_input = std::fs::File::open("datafile").unwrap();
/// let mut decompressed_input = lz4_flex::frame::FrameDecoder::new(compressed_input);
/// loop {
///     match serde_json::from_reader::<_, serde_json::Value>(&mut decompressed_input) {
///         Ok(json) => { println!("json {:?}", json); }
///         Err(e) if e.is_eof() => break,
///         Err(e) => panic!("{}", e),
///     }
/// }
/// ```
pub struct FrameDecoder<R: io::Read> {
    /// The underlying reader.
    r: R,
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

impl<R: io::Read> FrameDecoder<R> {
    /// Creates a new Decoder for the specified reader.
    pub fn new(rdr: R) -> FrameDecoder<R> {
        FrameDecoder {
            r: rdr,
            src: Default::default(),
            dst: Default::default(),
            ext_dict_offset: 0,
            ext_dict_len: 0,
            dst_start: 0,
            dst_end: 0,
            current_frame_info: None,
            content_hasher: XxHash32::with_seed(0),
            content_len: 0,
        }
    }

    pub fn frame_info(&mut self) -> Option<&FrameInfo> {
        self.current_frame_info.as_ref()
    }

    /// Gets a reference to the underlying reader in this decoder.
    pub fn get_ref(&self) -> &R {
        &self.r
    }

    /// Gets a mutable reference to the underlying reader in this decoder.
    ///
    /// Note that mutation of the stream may result in surprising results if
    /// this decoder is continued to be used.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.r
    }

    fn read_frame_info(&mut self) -> Result<usize, io::Error> {
        let mut buffer = [0u8; MAX_FRAME_INFO_SIZE];
        match self.r.read(&mut buffer[..MIN_FRAME_INFO_SIZE])? {
            0 => return Ok(0),
            MIN_FRAME_INFO_SIZE => (),
            read => self.r.read_exact(&mut buffer[read..MIN_FRAME_INFO_SIZE])?,
        }
        let required = FrameInfo::read_size(&buffer[..MIN_FRAME_INFO_SIZE])?;
        if required != MIN_FRAME_INFO_SIZE {
            self.r
                .read_exact(&mut buffer[MIN_FRAME_INFO_SIZE..required])?;
        }
        let frame_info = FrameInfo::read(&buffer[..required])?;
        if frame_info.dict_id.is_some() {
            // Unsupported right now so it must be None
            return Err(Error::DictionaryNotSupported.into());
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
    fn read_checksum(r: &mut R) -> Result<u32, io::Error> {
        let mut checksum_buffer = [0u8; size_of::<u32>()];
        r.read_exact(&mut checksum_buffer[..])?;
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

    fn read_block(&mut self) -> io::Result<usize> {
        debug_assert_eq!(self.dst_start, self.dst_end);
        let frame_info = self.current_frame_info.as_ref().unwrap();

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
            self.r.read_exact(&mut buffer)?;
            BlockInfo::read(&buffer)?
        };
        match block_info {
            BlockInfo::Uncompressed(len) => {
                let len = len as usize;
                if len > max_block_size {
                    return Err(Error::BlockTooBig.into());
                }
                // TODO: Attempt to avoid initialization of read buffer when
                // https://github.com/rust-lang/rust/issues/42788 stabilizes
                self.r.read_exact(vec_resize_and_get_mut(
                    &mut self.dst,
                    self.dst_start,
                    self.dst_start + len,
                ))?;
                if frame_info.block_checksums {
                    let expected_checksum = Self::read_checksum(&mut self.r)?;
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
                    return Err(Error::BlockTooBig.into());
                }
                // TODO: Attempt to avoid initialization of read buffer when
                // https://github.com/rust-lang/rust/issues/42788 stabilizes
                self.r
                    .read_exact(vec_resize_and_get_mut(&mut self.src, 0, len))?;
                if frame_info.block_checksums {
                    let expected_checksum = Self::read_checksum(&mut self.r)?;
                    Self::check_block_checksum(&self.src[..len], expected_checksum)?;
                }

                let with_dict_mode =
                    frame_info.block_mode == BlockMode::Linked && self.ext_dict_len != 0;
                let decomp_size = if with_dict_mode {
                    debug_assert!(self.dst_start + max_block_size <= self.ext_dict_offset);
                    let (head, tail) = self.dst.split_at_mut(self.ext_dict_offset);
                    let ext_dict = &tail[..self.ext_dict_len];

                    debug_assert!(head.len() - self.dst_start >= max_block_size);
                    crate::block::decompress::decompress_internal::<_, true>(
                        &self.src[..len],
                        &mut SliceSink::new(head, self.dst_start),
                        ext_dict,
                    )
                } else {
                    // Independent blocks OR linked blocks with only prefix data
                    debug_assert!(self.dst.capacity() - self.dst_start >= max_block_size);
                    crate::block::decompress::decompress_internal::<_, false>(
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
                        }
                        .into());
                    }
                }
                if frame_info.content_checksum {
                    let expected_checksum = Self::read_checksum(&mut self.r)?;
                    let calc_checksum = self.content_hasher.finish() as u32;
                    if calc_checksum != expected_checksum {
                        return Err(Error::ContentChecksumError.into());
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

    fn read_more(&mut self) -> io::Result<usize> {
        if self.current_frame_info.is_none() && self.read_frame_info()? == 0 {
            return Ok(0);
        }
        self.read_block()
    }
}

impl<R: io::Read> io::Read for FrameDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            // Fill read buffer if there's uncompressed data left
            if self.dst_start < self.dst_end {
                let read_len = std::cmp::min(self.dst_end - self.dst_start, buf.len());
                let dst_read_end = self.dst_start + read_len;
                buf[..read_len].copy_from_slice(&self.dst[self.dst_start..dst_read_end]);
                self.dst_start = dst_read_end;
                return Ok(read_len);
            }
            if self.read_more()? == 0 {
                return Ok(0);
            }
        }
    }

    fn read_to_string(&mut self, buf: &mut String) -> io::Result<usize> {
        let mut written = 0;
        loop {
            match self.fill_buf() {
                Ok(b) if b.is_empty() => return Ok(written),
                Ok(b) => {
                    let s = std::str::from_utf8(b).map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "stream did not contain valid UTF-8",
                        )
                    })?;
                    buf.push_str(s);
                    let len = s.len();
                    self.consume(len);
                    written += len;
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let mut written = 0;
        loop {
            match self.fill_buf() {
                Ok(b) if b.is_empty() => return Ok(written),
                Ok(b) => {
                    buf.extend_from_slice(b);
                    let len = b.len();
                    self.consume(len);
                    written += len;
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

impl<R: io::Read> io::BufRead for FrameDecoder<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.dst_start == self.dst_end {
            self.read_more()?;
        }
        Ok(&self.dst[self.dst_start..self.dst_end])
    }

    fn consume(&mut self, amt: usize) {
        assert!(amt <= self.dst_end - self.dst_start);
        self.dst_start += amt;
    }
}

impl<R: fmt::Debug + io::Read> fmt::Debug for FrameDecoder<R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FrameDecoder")
            .field("r", &self.r)
            .field("content_hasher", &self.content_hasher)
            .field("content_len", &self.content_len)
            .field("src", &"[...]")
            .field("dst", &"[...]")
            .field("dst_start", &self.dst_start)
            .field("dst_end", &self.dst_end)
            .field("ext_dict_offset", &self.ext_dict_offset)
            .field("ext_dict_len", &self.ext_dict_len)
            .field("current_frame_info", &self.current_frame_info)
            .finish()
    }
}

/// Similar to `v.get_mut(start..end) but will adjust the len if needed.
/// Panics if there's not enough capacity.
#[inline]
fn vec_resize_and_get_mut(v: &mut Vec<u8>, start: usize, end: usize) -> &mut [u8] {
    if end > v.len() {
        v.resize(end, 0)
    }
    &mut v[start..end]
}
