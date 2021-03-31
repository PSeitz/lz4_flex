use std::{convert::TryInto, hash::Hasher, io, mem::size_of};
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
    /// A Snappy decoder that we reuse that does the actual block based
    /// decompression.
    // dec: Decoder,
    /// Xxxhash32 used when content checksum is enabled.
    content_hasher: XxHash32,
    /// The compressed bytes buffer, taken from the underlying reader.
    src: Vec<u8>,
    /// The decompressed bytes buffer. Bytes are decompressed from src to dst
    /// before being passed back to the caller.
    dst: Vec<u8>,
    /// Index into dst: starting point of bytes not yet given back to caller.
    dsts: usize,
    /// Index into dst: ending point of bytes not yet given back to caller.
    dste: usize,
    /// Previous uncompressed frame, used in linked block mode.
    // prev_dst: Vec<u8>,
    /// Whether we've read the a stream header or not.
    /// Also cleared once frame end marker is read and Ok(0) is returned.
    frame_info: Option<FrameInfo>,
}

impl<R: io::Read> FrameDecoder<R> {
    /// Create a new reader for streaming Snappy decompression.
    pub fn new(rdr: R) -> FrameDecoder<R> {
        FrameDecoder {
            r: rdr,
            // dec: Decoder::new(),
            src: Default::default(),
            dst: Default::default(),
            dsts: 0,
            dste: 0,
            frame_info: None,
            content_hasher: XxHash32::with_seed(0),
        }
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

    fn read_checksum(&mut self) -> Result<u32, io::Error> {
        let mut checksum_buffer = [0u8; size_of::<u32>()];
        self.r.read_exact(&mut checksum_buffer[..])?;
        let checksum = u32::from_le_bytes(checksum_buffer.try_into().unwrap());
        Ok(checksum)
    }

    fn check_block_checksum(&self, data: &[u8], expected_checksum: u32) -> Result<(), io::Error> {
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
        if self.frame_info.is_none() {
            let mut buffer = [0u8; header::MAX_FRAME_INFO_SIZE];
            // TODO: handle read Ok(0)
            self.r.read_exact(&mut buffer[..7])?;
            let required = FrameInfo::read_size(&buffer[..7])?;
            if required != 7 {
                self.r.read_exact(&mut buffer[7..required])?;
            }
            let frame_info = FrameInfo::read(&buffer[..required])?;
            self.dst.resize(frame_info.block_size.get_size(), 0);
            self.src.resize(frame_info.block_size.get_size(), 0);
            if frame_info.block_mode == BlockMode::Linked {
                return Err(Error::LinkedBlocksNotSupported.into());
            }
            self.frame_info = Some(frame_info);
        }
        loop {
            if self.dsts < self.dste {
                let len = std::cmp::min(self.dste - self.dsts, buf.len());
                let dste = self.dsts.checked_add(len).unwrap();
                buf[..len].copy_from_slice(&self.dst[self.dsts..dste]);
                self.dsts = dste;
                return Ok(len);
            }
            let block_info = {
                let mut buffer = [0u8; 4];
                self.r.read_exact(&mut buffer)?;
                BlockInfo::read(&mut buffer)?
            };
            match block_info {
                BlockInfo::Uncompressed(len) => {
                    let len = len as usize;
                    self.r.read_exact(&mut self.dst[..len])?;
                    if self.frame_info.as_ref().unwrap().block_checksums {
                        let expected_checksum = self.read_checksum()?;
                        self.check_block_checksum(&self.dst[..len], expected_checksum)?;
                    }
                    self.dsts = 0;
                    self.dste = len;
                }
                BlockInfo::Compressed(len) => {
                    let len = len as usize;
                    if len > self.src.len() {
                        return Err(Error::BlockTooBig.into());
                    }
                    self.r.read_exact(&mut self.src[..len])?;
                    if self.frame_info.as_ref().unwrap().block_checksums {
                        let expected_checksum = self.read_checksum()?;
                        self.check_block_checksum(&self.src[..len], expected_checksum)?;
                    }
                    let dst =
                        crate::block::decompress::decompress(&self.src[..len], self.dst.len())
                            .map_err(Error::DecompressionError)?;
                    self.dst[..dst.len()].copy_from_slice(&dst[..]);
                    self.dsts = 0;
                    self.dste = dst.len();
                }

                BlockInfo::EndMark => {
                    if self.frame_info.as_ref().unwrap().content_checksum {
                        let expected_checksum = self.read_checksum()?;
                        let calc_checksum = self.content_hasher.finish() as u32;
                        if calc_checksum != expected_checksum {
                            return Err(Error::ContentChecksumError.into());
                        }
                    }
                    self.frame_info = None;
                    return Ok(0);
                }
            };

            if self.frame_info.as_ref().unwrap().content_checksum {
                self.content_hasher.write(&self.dst[..self.dste]);
            }
        }
    }
}

impl<R: std::fmt::Debug + io::Read> std::fmt::Debug for FrameDecoder<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("FrameDecoder")
            .field("r", &self.r)
            // .field("dec", &self.dec)
            .field("content_hasher", &self.content_hasher)
            .field("src", &"[...]")
            .field("dst", &"[...]")
            .field("dsts", &self.dsts)
            .field("dste", &self.dste)
            .field("frame_info", &self.frame_info)
            .finish()
    }
}
