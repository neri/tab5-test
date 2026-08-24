//! [`BlockDevice`] adapter over `sdmmc.rs`.
//!
//! Thin by design. Card activation, the IDMAC descriptor chain, the cache
//! writeback and invalidate around DMA buffers, and the bus width and clock
//! negotiation all stay in `sdmmc.rs`; this file only maps that driver's
//! `bool` results and 32-bit LBAs onto the common interface, splits transfers
//! the descriptor chain cannot take in one go, and refuses writes.
//!
//! Rewriting the driver to implement the trait directly would have dragged
//! all of that medium-specific handling up into a layer whose whole purpose
//! is not to know about it.

use super::block::{BlockDevice, BlockError, BlockGeometry, SUPPORTED_BLOCK_BYTES, check_range};
use super::fingerprint;
use crate::sdmmc::{self, MAX_TRANSFER_BYTES, SdCard};
use crate::uart;

pub struct SdBlockDevice {
    card: SdCard,
    geometry: BlockGeometry,
}

impl SdBlockDevice {
    /// Activates the card and wraps it.
    ///
    /// `None` when activation fails, or when the card reports a CSD version
    /// 1.0 structure, whose capacity `sdmmc.rs` does not decode. Without a
    /// capacity there is no block count, and without a block count no range
    /// check is possible -- and an unchecked range is exactly what the layers
    /// above are relying on this type to provide. Such a card stays reachable
    /// from the raw `sdread`/`sdreadn` commands, which address it directly.
    pub fn open() -> Option<Self> {
        let card = sdmmc::init()?;
        let Some(capacity_bytes) = card.capacity_bytes else {
            uart::log(b"FS: SD card capacity unknown (CSD v1); not usable as a block device\r\n");
            return None;
        };
        let block_count = capacity_bytes / SUPPORTED_BLOCK_BYTES as u64;
        if block_count == 0 {
            return None;
        }
        Some(Self {
            card,
            geometry: BlockGeometry {
                block_bytes: SUPPORTED_BLOCK_BYTES,
                block_count,
            },
        })
    }
}

impl SdBlockDevice {
    /// Adds the card's identity to a fingerprint.
    ///
    /// The CID is read once during activation and does not change while the
    /// card stays in the slot, so this costs no bus traffic -- unlike the
    /// USB side, where the equivalent is several SCSI commands.
    pub fn add_identity(&self, builder: &mut fingerprint::Builder) {
        builder.sd_cid(&self.card.cid);
    }
}

impl BlockDevice for SdBlockDevice {
    fn geometry(&self) -> BlockGeometry {
        self.geometry
    }

    fn read_blocks(&mut self, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        check_range(&self.geometry, lba, buffer.len())?;
        // The range check placed the whole transfer inside a medium whose
        // block count came from a `u64` capacity, so a card larger than
        // 2 TiB would reach here with an LBA the driver's `u32` argument
        // cannot express. Nothing that size exists on this bus today; the
        // check is here so that if one ever appears it fails loudly instead
        // of reading a truncated address.
        let mut lba = u32::try_from(lba).map_err(|_| BlockError::OutOfRange)?;

        // The driver's descriptor chain covers a bounded transfer, so a
        // longer request is split here rather than pushed back to the
        // filesystem layer, which has no business knowing the chain depth.
        for chunk in buffer.chunks_mut(MAX_TRANSFER_BYTES) {
            if !sdmmc::read_blocks(&self.card, lba, chunk) {
                return Err(BlockError::DeviceError);
            }
            lba += (chunk.len() / SUPPORTED_BLOCK_BYTES as usize) as u32;
        }
        Ok(())
    }

    /// Always fails with [`BlockError::WriteSuppressed`], without issuing a
    /// command.
    ///
    /// The method exists so the interface is the same for every medium and
    /// the day SD writes are enabled is a policy change rather than a trait
    /// change. Until then the filesystem layer mounts this card read-only,
    /// and a write arriving here means something above it is wrong -- so the
    /// request is logged with its address, which is what makes the mistake
    /// findable, rather than dropped.
    fn write_blocks(&mut self, lba: u64, buffer: &[u8]) -> Result<(), BlockError> {
        let blocks = self.geometry.blocks_for(buffer.len()).unwrap_or(0);
        uart::log(b"FS: SD write suppressed (read-only mount policy)\r\n");
        uart::log_hex(b"FS:   LBA=", lba as u32);
        uart::log_hex(b"FS:   blocks=", blocks as u32);
        Err(BlockError::WriteSuppressed)
    }

    /// Nothing is buffered on this side, and no command is sent for the same
    /// reason `write_blocks` sends none.
    fn flush(&mut self) -> Result<(), BlockError> {
        Ok(())
    }
}
