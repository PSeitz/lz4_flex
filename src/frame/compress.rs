use std::{
    fmt,
    hash::Hasher,
    io::{self, Write},
};
use twox_hash::XxHash32;

use crate::compress_into;

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
    /// Our main internal state, split out for borrowck reasons (happily paid).
    ///
    /// Also, it's an `Option` so we can move out of it even though
    /// `FrameEncoder` impls `Drop`.
    inner: Option<Inner<W>>,
    /// Our buffer of uncompressed bytes. This isn't part of `inner` because
    /// we may write bytes directly from the caller if the given buffer was
    /// big enough. As a result, the main `write` implementation needs to
    /// accept either the internal buffer or the caller's bytes directly. Since
    /// `write` requires a mutable borrow, we satisfy the borrow checker by
    /// separating `src` from the rest of the state.
    src: Vec<u8>,
}

struct Inner<W> {
    /// The underlying writer.
    w: W,
    /// Xxhash32 used when content checksum is enabled.
    content_hasher: XxHash32,
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
    pub fn new(wtr: W) -> FrameEncoder<W> {
        let frame_info = FrameInfo::default();
        FrameEncoder {
            src: Vec::with_capacity(frame_info.block_size.get_size()),
            inner: Some(Inner {
                w: wtr,
                // enc: Encoder::new(),
                content_hasher: XxHash32::with_seed(0),
                dst: Vec::with_capacity(frame_info.block_size.get_size()),
                wrote_frame_info: false,
                frame_info,
            }),
        }
    }

    pub fn frame_info(&mut self) -> &FrameInfo {
        &self.inner.as_ref().unwrap().frame_info
    }

    pub fn frame_info_mut(&mut self) -> &mut FrameInfo {
        &mut self.inner.as_mut().unwrap().frame_info
    }

    /// Consumes this encoder, flushing the output stream.
    ///
    /// This will flush the underlying data stream, close off the compressed stream and,
    /// if successful, return the contained writer.
    pub fn finish(mut self) -> Result<W, Error> {
        self.try_finish()?;
        Ok(self.inner.take().unwrap().w)
    }

    /// Attempt to finish this output stream, writing out final chunks of data.
    ///
    /// Note that this function can only be used once data has finished being written to the output stream.
    /// After this function is called then further calls to write may result in a panic.
    pub fn try_finish(&mut self) -> Result<(), Error> {
        match self.flush() {
            Ok(()) => {
                let inner = self.inner.as_mut().unwrap();
                if inner.wrote_frame_info {
                    inner.wrote_frame_info = false;
                    let mut block_info_buffer = [0u8; BLOCK_INFO_SIZE];
                    BlockInfo::EndMark.write(&mut block_info_buffer[..])?;
                    self.inner
                        .as_mut()
                        .unwrap()
                        .w
                        .write_all(&block_info_buffer[..])?;
                }
                Ok(())
            }
            Err(err) => Err(err.into()),
        }
    }

    pub fn into_inner(mut self) -> W {
        self.inner.take().unwrap().w
    }

    /// Gets a reference to the underlying writer in this encoder.
    pub fn get_ref(&self) -> &W {
        &self.inner.as_ref().unwrap().w
    }

    /// Gets a reference to the underlying writer in this encoder.
    ///
    /// Note that mutating the output/input state of the stream may corrupt
    /// this encoder, so care must be taken when using this method.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner.as_mut().unwrap().w
    }
}

impl<W: io::Write> io::Write for FrameEncoder<W> {
    fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
        let mut total = 0;
        // If there isn't enough room to add buf to src, then add only a piece
        // of it, flush it and mush on.
        loop {
            let free = self.src.capacity() - self.src.len();
            // n is the number of bytes extracted from buf.
            let n = if buf.len() <= free {
                break;
            } else if self.src.is_empty() {
                // If buf is bigger than our entire buffer then avoid
                // the indirection and write the buffer directly.
                self.inner.as_mut().unwrap().write(buf)?
            } else {
                self.src.extend_from_slice(&buf[..free]);
                self.flush()?;
                free
            };
            buf = &buf[n..];
            total += n;
        }
        // We're only here if buf.len() will fit within the available space of
        // self.src.
        debug_assert!(buf.len() <= (self.src.capacity() - self.src.len()));
        self.src.extend_from_slice(buf);
        total += buf.len();
        // We should never expand or contract self.src.
        debug_assert_eq!(self.src.capacity(), self.frame_info().block_size.get_size());
        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.src.is_empty() {
            return Ok(());
        }
        self.inner.as_mut().unwrap().write(&self.src)?;
        self.src.truncate(0);
        Ok(())
    }
}

impl<W: io::Write> Inner<W> {
    fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
        if !self.wrote_frame_info {
            self.wrote_frame_info = true;
            if self.frame_info.block_mode == BlockMode::Linked {
                return Err(Error::LinkedBlocksNotSupported.into());
            }
            let mut frame_info_buffer = [0u8; MAX_FRAME_INFO_SIZE];
            let size = self.frame_info.write(&mut frame_info_buffer)?;
            self.w.write_all(&frame_info_buffer[..size])?;
        }

        let mut total = 0;
        while !buf.is_empty() {
            // Advance buf and get our block.
            let mut src = buf;
            if src.len() > self.frame_info.block_size.get_size() {
                src = &src[..self.frame_info.block_size.get_size()];
            }
            buf = &buf[src.len()..];

            self.dst.truncate(0);
            compress_into(src, &mut self.dst);

            let (block_info, buf_to_write) = if self.dst.len() < src.len() {
                (BlockInfo::Compressed(self.dst.len() as _), &self.dst[..])
            } else {
                (BlockInfo::Uncompressed(src.len() as _), src)
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

            total += src.len();
        }
        Ok(total)
    }
}

impl<W: fmt::Debug + io::Write> fmt::Debug for FrameEncoder<W> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FrameEncoder")
            .field("inner", &self.inner)
            .field("src", &"[...]")
            .finish()
    }
}

impl<W: fmt::Debug + io::Write> fmt::Debug for Inner<W> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Inner")
            .field("w", &self.w)
            // .field("enc", &self.enc)
            .field("content_hasher", &self.content_hasher)
            .field("dst", &"[...]")
            .field("wrote_frame_info", &self.wrote_frame_info)
            .field("frame_info", &self.frame_info)
            .finish()
    }
}
