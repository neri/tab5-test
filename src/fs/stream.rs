//! Byte-addressed [`hadris_io`] `Read`/`Seek` over a [`BlockDevice`], with a
//! small sector cache.
//!
//! The filesystem driver asks for bytes at arbitrary offsets; the medium
//! moves whole blocks. This is the single place that bridges the two, so no
//! filesystem code ever computes an LBA and no block device ever sees a byte
//! offset.
//!
//! The cache is what makes that bridge affordable. A FAT driver reads a
//! directory entry here, a FAT entry there, a few bytes of a boot sector --
//! each of them a handful of bytes out of a 512-byte block, and often out of
//! the block it just read. Without a cache every one of those would be a
//! fresh SD command or USB transfer. It holds one aligned run of blocks
//! rather than a set of scattered ones because the access pattern that
//! matters is sequential: walking a cluster chain or reading a file's data
//! runs forward, so a run that covers the next few blocks is hit far more
//! often than a scattered set of the same size would be.
//!
//! This is also the choke point the plan asks for elsewhere: everything the
//! filesystem driver reads passes through one place, which is where a media
//! generation check and the block-error translation go.

use alloc::boxed::Box;

use hadris_io::{Error as IoError, Read, Seek, SeekFrom, Write};

use super::block::{BlockDevice, BlockError};

/// Blocks held at once. Eight 512-byte blocks is the 4 KiB per mount the plan
/// budgets: enough that a FAT sector and the directory sector next to it are
/// usually both resident, small enough that several mounts do not add up to
/// anything meaningful against PSRAM.
const CACHE_BLOCKS: u64 = 8;

/// The cache buffer, aligned so it can be an SD DMA destination directly.
///
/// `sdmmc.rs` stages an unaligned buffer through one of its own, which is
/// correct but copies every block. Declaring the alignment here means the
/// card's IDMAC writes straight into the cache instead.
#[repr(C, align(64))]
struct CacheBuffer([u8; (CACHE_BLOCKS * 512) as usize]);

/// A block device's error on its way through the filesystem driver.
///
/// `hadris_io` carries a source error unchanged, so the reason a read failed
/// -- a removed card, a suppressed write, a range check -- survives the trip
/// through the library and comes back out at the VFS instead of being
/// flattened into "I/O error".
#[derive(Clone, Copy, Debug)]
pub struct StreamError(pub BlockError);

impl core::fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(super::block::error_name(self.0))
    }
}

impl core::error::Error for StreamError {}

impl embedded_io::Error for StreamError {
    fn kind(&self) -> embedded_io::ErrorKind {
        match self.0 {
            BlockError::OutOfRange => embedded_io::ErrorKind::InvalidInput,
            BlockError::WriteSuppressed => embedded_io::ErrorKind::PermissionDenied,
            BlockError::MediaRemoved | BlockError::MediaChanged => {
                embedded_io::ErrorKind::NotConnected
            }
            _ => embedded_io::ErrorKind::Other,
        }
    }
}

pub struct BlockStream<'a> {
    device: &'a mut dyn BlockDevice,
    block_bytes: u64,
    /// Total addressable bytes. Reads stop here rather than asking the device
    /// for a block it would refuse.
    length: u64,
    position: u64,
    cache: Box<CacheBuffer>,
    /// First block held, or `None` when the cache is empty.
    cached_lba: Option<u64>,
    /// How many of `cache`'s blocks are valid. Less than `CACHE_BLOCKS` only
    /// for the run at the very end of a device that is not a multiple of it.
    cached_blocks: u64,
}

impl<'a> BlockStream<'a> {
    /// Wraps `device`, allocating the cache.
    ///
    /// The cache is on the heap rather than inline because a `BlockStream`
    /// is handed to the filesystem library by value and moved around inside
    /// it; 4 KiB travelling with it would be 4 KiB of memcpy at every move.
    pub fn new(device: &'a mut dyn BlockDevice) -> Self {
        let geometry = device.geometry();
        let block_bytes = geometry.block_bytes as u64;
        let length = geometry.capacity_bytes().unwrap_or(0);
        Self {
            device,
            block_bytes,
            length,
            position: 0,
            cache: Box::new(CacheBuffer([0; (CACHE_BLOCKS * 512) as usize])),
            cached_lba: None,
            cached_blocks: 0,
        }
    }

    /// Makes the run containing `lba` resident, and returns its offset within
    /// the cache.
    fn fill(&mut self, lba: u64) -> Result<usize, BlockError> {
        // Runs start on multiples of the run length, so the same block always
        // maps to the same run and a sequential walk refills once per run
        // instead of on every block.
        let run_start = lba - lba % CACHE_BLOCKS;
        let total_blocks = self.length / self.block_bytes;
        if lba >= total_blocks {
            return Err(BlockError::OutOfRange);
        }

        if self.cached_lba != Some(run_start) {
            // The last run of a device whose size is not a whole number of
            // runs is short; asking for the full run would be refused.
            let blocks = CACHE_BLOCKS.min(total_blocks - run_start);
            let bytes = (blocks * self.block_bytes) as usize;
            // The cache is only marked valid once the read succeeds, so a
            // failed transfer leaves a stale run readable rather than
            // half-overwritten bytes claiming to be the new one.
            self.cached_lba = None;
            self.device
                .read_blocks(run_start, &mut self.cache.0[..bytes])?;
            self.cached_lba = Some(run_start);
            self.cached_blocks = blocks;
        }

        Ok(((lba - run_start) * self.block_bytes) as usize)
    }
}

impl Read for BlockStream<'_> {
    type Error = StreamError;

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, IoError<StreamError>> {
        if buffer.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let lba = self.position / self.block_bytes;
        let within_block = (self.position % self.block_bytes) as usize;
        let offset = self
            .fill(lba)
            .map_err(|error| IoError::Source(StreamError(error)))?;

        // One call returns what is left of the cached run from this position,
        // capped by the caller's buffer and by the end of the device. Short
        // reads are what `Read` is defined to allow, and the library's
        // `read_exact` loops; returning the run's tail rather than one block
        // means a large sequential read comes back in 4 KiB pieces.
        let cached_end = (self.cached_blocks * self.block_bytes) as usize;
        let run_offset = offset + within_block;
        let available = cached_end - run_offset;
        let remaining = (self.length - self.position) as usize;
        let count = buffer.len().min(available).min(remaining);
        buffer[..count].copy_from_slice(&self.cache.0[run_offset..run_offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Write for BlockStream<'_> {
    type Error = StreamError;

    /// Writes at the current position, which need not be block-aligned.
    ///
    /// The medium moves whole blocks, so a partial block is a
    /// read-modify-write: the run containing the position is made resident,
    /// the caller's bytes are patched into it, and the blocks that changed
    /// go back. Filling first happens even when the write covers a whole
    /// block and the read is therefore redundant. That costs one extra
    /// transfer per aligned run, and buys a cache that is still valid
    /// afterwards rather than one that has to be invalidated and re-read on
    /// the next access -- which the FAT driver, alternating between file
    /// data and the FAT itself, would immediately do.
    ///
    /// Like `read`, one call covers what is left of the cached run. The
    /// caller loops.
    fn write(&mut self, buffer: &[u8]) -> Result<usize, IoError<StreamError>> {
        if buffer.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let lba = self.position / self.block_bytes;
        let within_block = (self.position % self.block_bytes) as usize;
        let offset = self
            .fill(lba)
            .map_err(|error| IoError::Source(StreamError(error)))?;

        let block_bytes = self.block_bytes as usize;
        let cached_end = (self.cached_blocks * self.block_bytes) as usize;
        let run_offset = offset + within_block;
        let available = cached_end - run_offset;
        let remaining = (self.length - self.position) as usize;
        let count = buffer.len().min(available).min(remaining);
        self.cache.0[run_offset..run_offset + count].copy_from_slice(&buffer[..count]);

        // Only the blocks the patch actually touched are written back.
        // Writing the whole run would put unchanged blocks on the medium
        // again, which on a card with a write-suppressing adapter is the
        // difference between one refused block and eight.
        let first_block = run_offset / block_bytes;
        let last_block = (run_offset + count - 1) / block_bytes;
        let start = first_block * block_bytes;
        let end = (last_block + 1) * block_bytes;
        let run_start = self.cached_lba.unwrap_or(0);
        self.device
            .write_blocks(run_start + first_block as u64, &self.cache.0[start..end])
            .map_err(|error| {
                // The cache no longer matches the medium: the patch is in it
                // and the write did not land. Dropping it means the next
                // read comes from the medium rather than from bytes that
                // were never stored.
                self.cached_lba = None;
                IoError::Source(StreamError(error))
            })?;

        self.position += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> Result<(), IoError<StreamError>> {
        self.device
            .flush()
            .map_err(|error| IoError::Source(StreamError(error)))
    }
}

impl Seek for BlockStream<'_> {
    type Error = StreamError;

    fn seek(&mut self, position: SeekFrom) -> Result<u64, IoError<StreamError>> {
        // Seeking past the end is not an error -- a subsequent read simply
        // returns nothing -- but a position that would be negative is, and it
        // is caught here rather than wrapping into an enormous offset.
        let next = match position {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(offset) => self.length.checked_add_signed(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
        };
        let Some(next) = next else {
            return Err(IoError::Source(StreamError(BlockError::OutOfRange)));
        };
        self.position = next;
        Ok(next)
    }
}
