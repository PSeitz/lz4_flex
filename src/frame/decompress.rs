use std::{convert::TryInto, fmt, hash::Hasher, io, mem::size_of};
use twox_hash::XxHash32;

use super::header::{self, BlockInfo, BlockMode, FrameInfo};
use super::Error;

/// A reader for decompressing the LZ4 framed format, as defined in:
/// https://github.com/lz4/lz4/blob/dev/doc/lz4_Frame_format.md
///
/// This reader can potentially make many small reads from the underlying
/// stream depending on its format, therefore, passing in a buffered reader
/// may be beneficial.
pub struct FrameDecoder<R: io::Read> {
    /// The underlying reader.
    r: R,
    /// Whether we've read the a stream header or not.
    /// Also cleared once frame end marker is read and Ok(0) is returned.
    frame_info: Option<FrameInfo>,
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
    /// Index into dst: starting point of bytes not yet given back to caller.
    dsts: usize,
    /// Index into dst: ending point of bytes not yet given back to caller.
    dste: usize,
}

impl<R: io::Read> FrameDecoder<R> {
    /// Create a new reader for streaming Snappy decompression.
    pub fn new(rdr: R) -> FrameDecoder<R> {
        FrameDecoder {
            r: rdr,
            src: Default::default(),
            dst: Default::default(),
            ext_dict_offset: 0,
            ext_dict_len: 0,
            dsts: 0,
            dste: 0,
            frame_info: None,
            content_hasher: XxHash32::with_seed(0),
            content_len: 0,
        }
    }

    pub fn frame_info(&mut self) -> Option<&FrameInfo> {
        self.frame_info.as_ref()
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
        let mut buffer = [0u8; header::MAX_FRAME_INFO_SIZE];
        match self.r.read(&mut buffer[..7])? {
            0 => return Ok(0),
            7 => (),
            read => self.r.read_exact(&mut buffer[read..7])?,
        }
        let required = FrameInfo::read_size(&buffer[..7])?;
        if required != 7 {
            self.r.read_exact(&mut buffer[7..required])?;
        }
        let frame_info = FrameInfo::read(&buffer[..required])?;
        self.src.resize(frame_info.block_size.get_size(), 0);
        let mut dst_size = frame_info.block_size.get_size();
        if frame_info.block_mode == BlockMode::Linked {
            dst_size = dst_size * 2 + crate::block::WINDOW_SIZE;
        }
        self.dst.resize(dst_size, 0);
        self.frame_info = Some(frame_info);
        self.content_hasher = XxHash32::with_seed(0);
        self.content_len = 0;
        Ok(required)
    }

    fn read_checksum(r: &mut R) -> Result<u32, io::Error> {
        let mut checksum_buffer = [0u8; size_of::<u32>()];
        r.read_exact(&mut checksum_buffer[..])?;
        let checksum = u32::from_le_bytes(checksum_buffer.try_into().unwrap());
        Ok(checksum)
    }

    fn check_block_checksum(data: &[u8], expected_checksum: u32) -> Result<(), io::Error> {
        let mut block_hasher = XxHash32::with_seed(0);
        block_hasher.write(data);
        let calc_checksum = block_hasher.finish() as u32;
        Ok(if calc_checksum != expected_checksum {
            return Err(Error::BlockChecksumError.into());
        })
    }
}

impl<R: io::Read> io::Read for FrameDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.frame_info.is_none() && self.read_frame_info()? == 0 {
            return Ok(0);
        }
        let frame_info = self.frame_info.as_ref().unwrap();
        loop {
            // Fill read buffer if there's uncompressed data left
            if self.dsts < self.dste {
                let len = std::cmp::min(self.dste - self.dsts, buf.len());
                let dste = self.dsts.checked_add(len).unwrap();
                buf[..len].copy_from_slice(&self.dst[self.dsts..dste]);
                self.dsts = dste;
                return Ok(len);
            }

            // Adjust dst buffer offsets to uncompress the next block
            let max_block_size = frame_info.block_size.get_size();
            if frame_info.block_mode == BlockMode::Linked {
                if self.dsts + max_block_size > self.dst.len() {
                    // Output might not fit in the buffer.
                    // The ext_dict will become the last WINDOW_SIZE bytes
                    debug_assert!(self.dsts >= max_block_size + crate::block::WINDOW_SIZE);
                    self.ext_dict_offset = self.dsts - crate::block::WINDOW_SIZE;
                    self.ext_dict_len = crate::block::WINDOW_SIZE;
                    // Output goes in the beginning of the buffer again.
                    self.dsts = 0;
                } else if self.dsts + self.ext_dict_len > crate::block::WINDOW_SIZE {
                    // Shrink ext_dict in favor of output prefix.
                    let delta = self.ext_dict_len.min(self.dsts);
                    self.ext_dict_offset += delta;
                    self.ext_dict_len -= delta;
                }
            } else {
                self.dsts = 0;
            }

            let block_info = {
                let mut buffer = [0u8; 4];
                self.r.read_exact(&mut buffer)?;
                BlockInfo::read(&mut buffer)?
            };
            match block_info {
                BlockInfo::Uncompressed(len) => {
                    let len = len as usize;
                    if len > max_block_size {
                        return Err(Error::BlockTooBig.into());
                    }
                    self.r
                        .read_exact(&mut self.dst[self.dsts..self.dsts + len])?;
                    if frame_info.block_checksums {
                        let expected_checksum = Self::read_checksum(&mut self.r)?;
                        Self::check_block_checksum(
                            &self.dst[self.dsts..self.dsts + len],
                            expected_checksum,
                        )?;
                    }

                    self.dste = self.dsts + len;
                    self.content_len += len as u64;
                }
                BlockInfo::Compressed(len) => {
                    let len = len as usize;
                    if len > max_block_size {
                        return Err(Error::BlockTooBig.into());
                    }
                    self.r.read_exact(&mut self.src[..len])?;
                    if frame_info.block_checksums {
                        let expected_checksum = Self::read_checksum(&mut self.r)?;
                        Self::check_block_checksum(&self.src[..len], expected_checksum)?;
                    }

                    let with_dict_mode =
                        frame_info.block_mode == BlockMode::Linked && self.ext_dict_len != 0;
                    let decomp_size = if with_dict_mode {
                        let (head, tail) = self.dst.split_at_mut(self.dsts + max_block_size);
                        let ext_dict = &tail[self.ext_dict_offset - head.len()
                            ..self.ext_dict_offset - head.len() + self.ext_dict_len];

                        let mut sink: crate::block::Sink = head.into();
                        sink.set_pos(self.dsts);
                        crate::block::decompress::decompress_into_with_dict(
                            &self.src[..len],
                            &mut sink,
                            ext_dict,
                        )
                    } else {
                        // Independent blocks OR linked blocks with only prefix data
                        let mut sink: crate::block::Sink = (&mut self.dst).into();
                        sink.set_pos(self.dsts);
                        crate::block::decompress::decompress_into(&self.src[..len], &mut sink)
                    }
                    .map_err(Error::DecompressionError)?;

                    self.dste = self.dsts + decomp_size;
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
                    self.frame_info = None;
                    return Ok(0);
                }
            };

            if frame_info.content_checksum {
                self.content_hasher.write(&self.dst[self.dsts..self.dste]);
            }
        }
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
            .field("dsts", &self.dsts)
            .field("dste", &self.dste)
            .field("ext_dict_offset", &self.ext_dict_offset)
            .field("ext_dict_len", &self.ext_dict_len)
            .field("frame_info", &self.frame_info)
            .finish()
    }
}
