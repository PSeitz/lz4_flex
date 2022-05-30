use std::{
    fmt,
    hash::Hasher,
    io::{self, Write},
};
use twox_hash::XxHash32;

use crate::{
    block::{
        compress::compress_internal,
        hashtable::{HashTable, HashTableU32},
    },
    sink::vec_sink_for_compression,
};

use super::header::{BlockInfo, BlockMode, FrameInfo, BLOCK_INFO_SIZE, MAX_FRAME_INFO_SIZE};
use super::Error;
use crate::block::WINDOW_SIZE;

/// A writer for compressing a LZ4 stream.
///
/// This `FrameEncoder` wraps any other writer that implements `io::Write`.
/// Bytes written to this writer are compressed using the [LZ4 frame
/// format](https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md).
///
/// Writes are buffered automatically, so there's no need to wrap the given
/// writer in a `std::io::BufWriter`.
///
/// To ensure a well formed stream the encoder must be finalized by calling
/// either `finish` or `try_finish()` methods.
///
/// # Example 1
/// Serializing json values into a compressed file.
///
/// ```no_run
/// let compressed_file = std::fs::File::create("datafile").unwrap();
/// let mut compressor = lz4_flex::frame::FrameEncoder::new(compressed_file);
/// serde_json::to_writer(&mut compressor, &serde_json::json!({ "an": "object" })).unwrap();
/// compressor.finish().unwrap();
/// ```
///
/// # Example 2
/// Serializing multiple json values into a compressed file using linked blocks.
///
/// ```no_run
/// let compressed_file = std::fs::File::create("datafile").unwrap();
/// let mut frame_info = lz4_flex::frame::FrameInfo::new();
/// frame_info.block_mode = lz4_flex::frame::BlockMode::Linked;
/// let mut compressor = lz4_flex::frame::FrameEncoder::with_frame_info(frame_info, compressed_file);
/// for i in 0..10u64 {
///     serde_json::to_writer(&mut compressor, &serde_json::json!({ "i": i })).unwrap();
/// }
/// compressor.finish().unwrap();
/// ```
pub struct FrameEncoder<W: io::Write> {
    /// Our buffer of uncompressed bytes.
    src: Vec<u8>,
    /// Index into src: starting point of bytes not yet compressed
    src_start: usize,
    /// Index into src: end point of bytes not not yet compressed
    src_end: usize,
    /// Index into src: starting point of external dictionary (applicable in Linked block mode)
    ext_dict_offset: usize,
    /// Length of external dictionary
    ext_dict_len: usize,
    /// Counter of bytes already compressed to the compression_table
    /// _Not_ the same as `content_len` as this is reset every to 2GB.
    src_stream_offset: usize,
    /// Encoder table
    compression_table: HashTableU32,
    /// The underlying writer.
    w: W,
    /// Xxhash32 used when content checksum is enabled.
    content_hasher: XxHash32,
    /// Number of bytes compressed
    content_len: u64,
    /// The compressed bytes buffer. Bytes are compressed from src (usually)
    /// to dst before being written to w.
    dst: Vec<u8>,
    /// Whether we have an open frame in the output.
    is_frame_open: bool,
    /// The frame information to be used in this encoder.
    frame_info: FrameInfo,
}

impl<W: io::Write> FrameEncoder<W> {
    /// Creates a new Encoder with the specified FrameInfo.
    pub fn with_frame_info(frame_info: FrameInfo, wtr: W) -> Self {
        let max_block_size = frame_info.block_size.get_size();
        let src_size = if frame_info.block_mode == BlockMode::Linked {
            // In linked mode we consume the input (bumping src_start) but leave the
            // beginning of src to be used as a prefix in subsequent blocks.
            // That is at least until we have at least `max_block_size + WINDOW_SIZE`
            // bytes in src, then we setup an ext_dict with the last WINDOW_SIZE bytes
            // and the input goes to the beginning of src again.
            // Since we always want to be able to write a full block (up to max_block_size)
            // we need a buffer with at least `max_block_size * 2 + WINDOW_SIZE` bytes.
            max_block_size * 2 + WINDOW_SIZE
        } else {
            max_block_size
        };
        let src = Vec::with_capacity(src_size);
        let dst = Vec::with_capacity(crate::block::compress::get_maximum_output_size(
            max_block_size,
        ));

        // 16 KB hash table for matches, same as the reference implementation.
        let (dict_size, dict_bitshift) = (4 * 1024, 4);

        FrameEncoder {
            src,
            w: wtr,
            compression_table: HashTableU32::new(dict_size, dict_bitshift),
            content_hasher: XxHash32::with_seed(0),
            content_len: 0,
            dst,
            is_frame_open: false,
            frame_info,
            src_start: 0,
            src_end: 0,
            ext_dict_offset: 0,
            ext_dict_len: 0,
            src_stream_offset: 0,
        }
    }

    /// Creates a new Encoder with the default settings.
    pub fn new(wtr: W) -> Self {
        Self::with_frame_info(Default::default(), wtr)
    }

    /// The frame information used by this Encoder.
    pub fn frame_info(&mut self) -> &FrameInfo {
        &self.frame_info
    }

    /// Consumes this encoder, flushing internal buffer and writing stream terminator.
    pub fn finish(mut self) -> Result<W, Error> {
        self.try_finish()?;
        Ok(self.w)
    }

    /// Attempt to finish this output stream, flushing internal buffer and writing stream
    /// terminator.
    pub fn try_finish(&mut self) -> Result<(), Error> {
        match self.flush() {
            Ok(()) if self.is_frame_open => self.end_frame(),
            Ok(()) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Returns the underlying writer _without_ flushing the stream.
    /// This may lave the output in an unfinished state.
    pub fn into_inner(self) -> W {
        self.w
    }

    /// Gets a reference to the underlying writer in this encoder.
    pub fn get_ref(&self) -> &W {
        &self.w
    }

    /// Gets a reference to the underlying writer in this encoder.
    ///
    /// Note that mutating the output/input state of the stream may corrupt
    /// this encoder, so care must be taken when using this method.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.w
    }

    /// Closes the frame by writing the end marker.
    fn end_frame(&mut self) -> Result<(), Error> {
        debug_assert!(self.is_frame_open);
        self.is_frame_open = false;
        if let Some(expected) = self.frame_info.content_size {
            if expected != self.content_len {
                return Err(Error::ContentLengthError {
                    expected,
                    actual: self.content_len,
                });
            }
        }

        let mut block_info_buffer = [0u8; BLOCK_INFO_SIZE];
        BlockInfo::EndMark.write(&mut block_info_buffer[..])?;
        self.w.write_all(&block_info_buffer[..])?;
        if self.frame_info.content_checksum {
            let content_checksum = self.content_hasher.finish() as u32;
            self.w.write_all(&content_checksum.to_le_bytes())?;
        }

        Ok(())
    }

    /// Begin the frame by writing the frame header.
    /// It'll also setup the encoder for compressing blocks for the the new frame.
    fn begin_frame(&mut self) -> io::Result<()> {
        self.is_frame_open = true;
        let mut frame_info_buffer = [0u8; MAX_FRAME_INFO_SIZE];
        let size = self.frame_info.write(&mut frame_info_buffer)?;
        self.w.write_all(&frame_info_buffer[..size])?;

        if self.content_len != 0 {
            // This is the second or later frame for this Encoder,
            // reset compressor state for the new frame.
            self.content_len = 0;
            self.src_stream_offset = 0;
            self.src.clear();
            self.src_start = 0;
            self.src_end = 0;
            self.ext_dict_len = 0;
            self.content_hasher = XxHash32::with_seed(0);
            self.compression_table.clear();
        }
        Ok(())
    }

    /// Consumes the src contents between src_start and src_end,
    /// which shouldn't exceed the max block size.
    fn write_block(&mut self) -> io::Result<()> {
        debug_assert!(self.is_frame_open);
        let max_block_size = self.frame_info.block_size.get_size();
        debug_assert!(self.src_end - self.src_start <= max_block_size);

        // Reposition the compression table if we're anywhere near an overflowing hazard
        if self.src_stream_offset + max_block_size + WINDOW_SIZE >= u32::MAX as usize / 2 {
            self.compression_table
                .reposition((self.src_stream_offset - self.ext_dict_len) as _);
            self.src_stream_offset = self.ext_dict_len;
        }

        // input to the compressor, which may include a prefix when blocks are linked
        let input = &self.src[..self.src_end];
        // the contents of the block are between src_start and src_end
        let src = &input[self.src_start..];

        let dst_required_size = crate::block::compress::get_maximum_output_size(src.len());

        let compress_result = if self.ext_dict_len != 0 {
            debug_assert_eq!(self.frame_info.block_mode, BlockMode::Linked);
            compress_internal::<_, _, true>(
                input,
                self.src_start,
                &mut vec_sink_for_compression(&mut self.dst, 0, 0, dst_required_size),
                &mut self.compression_table,
                &self.src[self.ext_dict_offset..self.ext_dict_offset + self.ext_dict_len],
                self.src_stream_offset,
            )
        } else {
            compress_internal::<_, _, false>(
                input,
                self.src_start,
                &mut vec_sink_for_compression(&mut self.dst, 0, 0, dst_required_size),
                &mut self.compression_table,
                b"",
                self.src_stream_offset,
            )
        };

        let (block_info, block_data) = match compress_result.map_err(Error::CompressionError)? {
            comp_len if comp_len < src.len() => {
                (BlockInfo::Compressed(comp_len as _), &self.dst[..comp_len])
            }
            _ => (BlockInfo::Uncompressed(src.len() as _), src),
        };

        // Write the (un)compressed block to the writer and the block checksum (if applicable).
        let mut block_info_buffer = [0u8; BLOCK_INFO_SIZE];
        block_info.write(&mut block_info_buffer[..])?;
        self.w.write_all(&block_info_buffer[..])?;
        self.w.write_all(block_data)?;
        if self.frame_info.block_checksums {
            let mut block_hasher = XxHash32::with_seed(0);
            block_hasher.write(block_data);
            let block_checksum = block_hasher.finish() as u32;
            self.w.write_all(&block_checksum.to_le_bytes())?;
        }

        // Content checksum, if applicable
        if self.frame_info.content_checksum {
            self.content_hasher.write(src);
        }

        // Buffer and offsets maintenance
        self.content_len += src.len() as u64;
        self.src_start += src.len();
        debug_assert_eq!(self.src_start, self.src_end);
        if self.frame_info.block_mode == BlockMode::Linked {
            // In linked mode we consume the input (bumping src_start) but leave the
            // beginning of src to be used as a prefix in subsequent blocks.
            // That is at least until we have at least `max_block_size + WINDOW_SIZE`
            // bytes in src, then we setup an ext_dict with the last WINDOW_SIZE bytes
            // and the input goes to the beginning of src again.
            debug_assert_eq!(self.src.capacity(), max_block_size * 2 + WINDOW_SIZE);
            if self.src_start >= max_block_size + WINDOW_SIZE {
                // The ext_dict will become the last WINDOW_SIZE bytes
                self.ext_dict_offset = self.src_end - WINDOW_SIZE;
                self.ext_dict_len = WINDOW_SIZE;
                // Input goes in the beginning of the buffer again.
                self.src_stream_offset += self.src_end;
                self.src_start = 0;
                self.src_end = 0;
            } else if self.src_start + self.ext_dict_len > WINDOW_SIZE {
                // There's more than WINDOW_SIZE bytes of lookback adding the prefix and ext_dict.
                // Since we have a limited buffer we must shrink ext_dict in favor of the prefix,
                // so that we can fit up to max_block_size bytes between dst_start and ext_dict
                // start.
                let delta = self
                    .ext_dict_len
                    .min(self.src_start + self.ext_dict_len - WINDOW_SIZE);
                self.ext_dict_offset += delta;
                self.ext_dict_len -= delta;
                debug_assert!(self.src_start + self.ext_dict_len >= WINDOW_SIZE)
            }
            debug_assert!(
                self.ext_dict_len == 0 || self.src_start + max_block_size <= self.ext_dict_offset
            );
        } else {
            // In independent block mode we consume the entire src buffer
            // which is sized equal to the frame max_block_size.
            debug_assert_eq!(self.ext_dict_len, 0);
            debug_assert_eq!(self.src.capacity(), max_block_size);
            self.src_start = 0;
            self.src_end = 0;
            // Advance stream offset so we don't have to reset the match dict
            // for the next block.
            self.src_stream_offset += src.len();
        }
        debug_assert!(self.src_start <= self.src_end);
        debug_assert!(self.src_start + max_block_size <= self.src.capacity());
        Ok(())
    }
}

impl<W: io::Write> io::Write for FrameEncoder<W> {
    fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
        if !self.is_frame_open && !buf.is_empty() {
            self.begin_frame()?;
        }
        let buf_len = buf.len();
        let max_block_size = self.frame_info.block_size.get_size();
        while !buf.is_empty() {
            let src_filled = self.src_end - self.src_start;
            let max_fill_len = max_block_size - src_filled;
            if max_fill_len == 0 {
                // make space by writing next block
                self.write_block()?;
                debug_assert_eq!(self.src_end, self.src_start);
                continue;
            }

            let fill_len = max_fill_len.min(buf.len());
            vec_copy_overwriting(&mut self.src, self.src_end, &buf[..fill_len]);
            buf = &buf[fill_len..];
            self.src_end += fill_len;
        }
        Ok(buf_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.src_start != self.src_end {
            self.write_block()?;
        }
        Ok(())
    }
}

impl<W: fmt::Debug + io::Write> fmt::Debug for FrameEncoder<W> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FrameEncoder")
            .field("w", &self.w)
            .field("frame_info", &self.frame_info)
            .field("is_frame_open", &self.is_frame_open)
            .field("content_hasher", &self.content_hasher)
            .field("content_len", &self.content_len)
            .field("dst", &"[...]")
            .field("src", &"[...]")
            .field("src_start", &self.src_start)
            .field("src_end", &self.src_end)
            .field("ext_dict_offset", &self.ext_dict_offset)
            .field("ext_dict_len", &self.ext_dict_len)
            .field("src_stream_offset", &self.src_stream_offset)
            .finish()
    }
}

/// Copy `src` into `v` starting from the `start` index, overwriting existing data if any.
#[inline]
fn vec_copy_overwriting(v: &mut Vec<u8>, start: usize, src: &[u8]) {
    debug_assert!(start + src.len() <= v.capacity());

    // By combining overwriting (copy_from_slice) and extending (extend_from_slice)
    // we can fill the ring buffer without initializing it (eg. filling with 0).
    let overwrite_len = (v.len() - start).min(src.len());
    v[start..start + overwrite_len].copy_from_slice(&src[..overwrite_len]);
    v.extend_from_slice(&src[overwrite_len..]);
}
