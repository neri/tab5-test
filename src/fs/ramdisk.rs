//! [`BlockDevice`] over the fixed PSRAM span `psram::Psram::ram_disk`
//! reserves, and the only medium this firmware mounts read-write.
//!
//! Reads and writes are plain CPU copies. Nothing else reaches this span --
//! no DMA engine is pointed at it -- so unlike `sdmmc.rs`'s IDMAC buffers it
//! needs no cache writeback or invalidate: the CPU sees its own stores
//! through the same cache that produced them.
//!
//! The contents live only until the next reset. `flush` says so explicitly.

use core::sync::atomic::{AtomicBool, Ordering};

use super::block::{BlockDevice, BlockError, BlockGeometry, SUPPORTED_BLOCK_BYTES, check_range};
use crate::psram::Psram;

/// Set once the single instance has been handed out. The span is a fixed
/// address rather than an allocation, so nothing else would stop a second
/// caller from creating an alias to it; this makes the constructor safe by
/// refusing instead.
static CLAIMED: AtomicBool = AtomicBool::new(false);

pub struct RamBlockDevice {
    base: *mut u8,
    geometry: BlockGeometry,
}

impl RamBlockDevice {
    /// Takes the PSRAM RAM disk span, once per boot.
    ///
    /// `None` if the mapping is too small for the reservation, or if the
    /// device has already been taken.
    pub fn claim(psram: &Psram) -> Option<Self> {
        let (base, bytes) = psram.ram_disk()?;
        if bytes < SUPPORTED_BLOCK_BYTES as usize {
            return None;
        }
        if CLAIMED.swap(true, Ordering::SeqCst) {
            return None;
        }
        Some(Self {
            base,
            geometry: BlockGeometry {
                block_bytes: SUPPORTED_BLOCK_BYTES,
                block_count: (bytes / SUPPORTED_BLOCK_BYTES as usize) as u64,
            },
        })
    }

    /// Byte offset of `lba` within the span. Only called after
    /// `check_range` has placed the whole transfer inside the geometry.
    fn offset(&self, lba: u64) -> usize {
        lba as usize * self.geometry.block_bytes as usize
    }
}

impl BlockDevice for RamBlockDevice {
    fn geometry(&self) -> BlockGeometry {
        self.geometry
    }

    fn read_blocks(&mut self, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        check_range(&self.geometry, lba, buffer.len())?;
        let offset = self.offset(lba);
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.base.add(offset),
                buffer.as_mut_ptr(),
                buffer.len(),
            );
        }
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buffer: &[u8]) -> Result<(), BlockError> {
        check_range(&self.geometry, lba, buffer.len())?;
        let offset = self.offset(lba);
        unsafe {
            core::ptr::copy_nonoverlapping(buffer.as_ptr(), self.base.add(offset), buffer.len());
        }
        Ok(())
    }

    /// Always succeeds. The stores are already in the image the next read
    /// will see, which is all this device promises -- the contents are gone
    /// at the next reset or power loss, and `flush` does not change that.
    fn flush(&mut self) -> Result<(), BlockError> {
        Ok(())
    }
}
