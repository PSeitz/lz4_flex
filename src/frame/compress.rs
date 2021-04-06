use std::{
    fmt,
    hash::Hasher,
    io::{self, Write},
};
use twox_hash::XxHash32;

use crate::block::{
    compress::compress_internal,
    hashtable::{HashTable, HashTableU32},
};

use super::header::{BlockInfo, BlockMode, FrameInfo, BLOCK_INFO_SIZE, MAX_FRAME_INFO_SIZE};
use super::Error;

/// A writer for compressing a Snappy stream.
///
/// This `FrameEncoder` wraps any other writer that implements `io::Write`.
/// Bytes written to this writer are compressed using the [Snappy frame
/// format](https://github.com/google/snappy/blob/master/framing_format.txt)
/// (file extension `sz`, MIME type `application/x-snappy-framed`).
///
/// Writes are buffered automatically, so there's no need to wrap the given
/// writer in a `std::io::BufWriter`.
///
/// The writer will be flushed automatically when it is dropped. If an error
/// occurs, it is ignored.
pub struct FrameEncoder<W: io::Write> {
    /// Our buffer of uncompressed bytes.
    src: Vec<u8>,
    srcs: usize,
    srce: usize,
    ext_dict_offset: usize,
    ext_dict_len: usize,
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
    /// When false, the stream identifier (with magic bytes) must precede the
    /// next write.
    wrote_frame_info: bool,
    frame_info: FrameInfo,
}

impl<W: io::Write> FrameEncoder<W> {
    /// Create a new writer for streaming Snappy compression.
    pub fn with_frame_info(frame_info: FrameInfo, wtr: W) -> Self {
        let max_block_size = frame_info.block_size.get_size();
        let src_size = if frame_info.block_mode == BlockMode::Linked {
            max_block_size * 2 + crate::block::WINDOW_SIZE
        } else {
            max_block_size
        };
        let (dict_size, dict_bitshift) = crate::block::hashtable::get_table_size(u32::MAX as _);
        FrameEncoder {
            src: vec![0; src_size],
            w: wtr,
            compression_table: HashTableU32::new(dict_size, dict_bitshift),
            content_hasher: XxHash32::with_seed(0),
            content_len: 0,
            dst: vec![0; max_block_size],
            wrote_frame_info: false,
            frame_info,
            srcs: 0,
            srce: 0,
            ext_dict_offset: 0,
            ext_dict_len: 0,
            src_stream_offset: 0,
        }
    }

    pub fn new(wtr: W) -> Self {
        Self::with_frame_info(Default::default(), wtr)
    }

    pub fn frame_info(&mut self) -> &FrameInfo {
        &self.frame_info
    }

    /// Consumes this encoder, flushing internal buffer and writing stream terminator.
    pub fn finish(mut self) -> Result<W, Error> {
        self.try_finish()?;
        Ok(self.w)
    }

    /// Attempt to finish this output stream, flushing internal buffer and writing stream terminator.
    pub fn try_finish(&mut self) -> Result<(), Error> {
        match self.flush() {
            Ok(()) if self.wrote_frame_info => {
                self.wrote_frame_info = false;
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

                Ok(())
            }
            Ok(()) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

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
}

impl<W: io::Write> io::Write for FrameEncoder<W> {
    fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
        let mut total = 0;
        while !buf.is_empty() {
            let free = if self.ext_dict_len == 0 {
                self.src.len() - self.srce
            } else {
                self.ext_dict_offset - self.srce
            };
            if free == 0 {
                self.write_block()?;
                continue;
            }

            // number of bytes to be extracted from buf.
            let n = free.min(buf.len());
            self.src[self.srce..self.srce + n].copy_from_slice(&buf[..n]);
            buf = &buf[n..];
            self.srce += n;
            total += n;
        }
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        while self.srcs != self.srce {
            self.write_block()?;
        }
        Ok(())
    }
}

impl<W: io::Write> FrameEncoder<W> {
    fn write_block(&mut self) -> io::Result<usize> {
        if !self.wrote_frame_info {
            self.wrote_frame_info = true;
            let mut frame_info_buffer = [0u8; MAX_FRAME_INFO_SIZE];
            let size = self.frame_info.write(&mut frame_info_buffer)?;
            self.w.write_all(&frame_info_buffer[..size])?;

            if self.content_len != 0 {
                // This is the second or later frame for this Encoder,
                // reset compressor state for the new frame.
                self.content_len = 0;
                self.content_hasher = XxHash32::with_seed(0);
                self.compression_table.clear();
            }
        }

        let max_block_size = self.frame_info.block_size.get_size();
        let (src, compressed_result) = if self.srcs != self.srce {
            if self.frame_info.block_mode == BlockMode::Linked {
                // Reposition the compression table if we're anywhere near an overflowing hazard
                if self.src_stream_offset + self.src.len() >= u32::MAX as usize / 2 {
                    self.compression_table
                        .reposition((self.src_stream_offset - self.ext_dict_len) as _);
                    self.src_stream_offset = self.ext_dict_len;
                }
                let src = &self.src[..self.srce.min(self.srcs + max_block_size)];
                let res = compress_internal(
                    src,
                    self.srcs,
                    &mut self.dst,
                    &mut self.compression_table,
                    &self.src[self.ext_dict_offset..self.ext_dict_offset + self.ext_dict_len],
                    self.src_stream_offset,
                );
                (&src[self.srcs..], res)
            } else {
                debug_assert_eq!(self.srcs, 0);
                debug_assert_eq!(self.src.len(), max_block_size);
                let src = &self.src[..self.srce];
                if self.content_len != 0 {
                    self.compression_table.clear();
                }
                let res =
                    compress_internal(src, 0, &mut self.dst, &mut self.compression_table, b"", 0);
                (src, res)
            }
        } else {
            (&b""[..], Ok(0))
        };

        let (block_info, buf_to_write) = match compressed_result {
            Ok(comp_len) if comp_len < src.len() => {
                (BlockInfo::Compressed(comp_len as _), &self.dst[..comp_len])
            }
            _ => (BlockInfo::Uncompressed(src.len() as _), src),
        };

        let mut block_info_buffer = [0u8; BLOCK_INFO_SIZE];
        block_info.write(&mut block_info_buffer[..])?;
        self.w.write_all(&block_info_buffer[..])?;
        self.w.write_all(buf_to_write)?;
        if self.frame_info.block_checksums {
            let mut block_hasher = XxHash32::with_seed(0);
            block_hasher.write(buf_to_write);
            let block_checksum = block_hasher.finish() as u32;
            self.w.write_all(&block_checksum.to_le_bytes())?;
        }
        self.content_len += src.len() as u64;

        self.srcs += src.len();
        if self.srcs == self.srce {
            if self.frame_info.block_mode == BlockMode::Linked {
                if self.srce + max_block_size > self.src.len() {
                    // The ext_dict will become the last WINDOW_SIZE bytes
                    debug_assert!(self.srce >= max_block_size + crate::block::WINDOW_SIZE);
                    self.ext_dict_offset = self.srce - crate::block::WINDOW_SIZE;
                    self.ext_dict_len = crate::block::WINDOW_SIZE;
                    self.src_stream_offset += self.srce;
                    // Input goes in the beginning of the buffer again.
                    self.srcs = 0;
                    self.srce = 0;
                } else if self.srcs + self.ext_dict_len > crate::block::WINDOW_SIZE {
                    // Shrink ext_dict in favor of input prefix.
                    let delta = self.ext_dict_len.min(self.srcs);
                    self.ext_dict_offset += delta;
                    self.ext_dict_len -= delta;
                }
            } else {
                self.srcs = 0;
                self.srce = 0;
            }
        }
        Ok(src.len())
    }
}

impl<W: fmt::Debug + io::Write> fmt::Debug for FrameEncoder<W> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FrameEncoder")
            .field("w", &self.w)
            // .field("enc", &self.enc)
            .field("content_hasher", &self.content_hasher)
            .field("content_len", &self.content_len)
            .field("dst", &"[...]")
            .field("wrote_frame_info", &self.wrote_frame_info)
            .field("frame_info", &self.frame_info)
            .field("src", &"[...]")
            .field("srcs", &self.srcs)
            .field("srce", &self.srce)
            .field("ext_dict_offset", &self.ext_dict_offset)
            .field("ext_dict_len", &self.ext_dict_len)
            .field("src_stream_offset", &self.src_stream_offset)
            .finish()
    }
}
