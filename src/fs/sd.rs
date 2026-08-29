//! [`BlockDevice`] adapter over `sdmmc.rs`.
//!
//! Thin by design. Card activation, the IDMAC descriptor chain, the cache
//! writeback and invalidate around DMA buffers, the DMA staging on the write
//! path, and the bus width and clock negotiation all stay in `sdmmc.rs`;
//! this file only maps that driver's `bool` results and 32-bit LBAs onto the
//! common interface and splits transfers the descriptor chain cannot take in
//! one go.
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

    fn write_blocks(&mut self, lba: u64, buffer: &[u8]) -> Result<(), BlockError> {
        check_range(&self.geometry, lba, buffer.len())?;
        // Same reasoning as `read_blocks`: the range check has already put
        // the transfer inside the medium, and this catches a card too large
        // for the driver's 32-bit address rather than silently truncating.
        let mut lba = u32::try_from(lba).map_err(|_| BlockError::OutOfRange)?;

        for chunk in buffer.chunks(MAX_TRANSFER_BYTES) {
            // A chunk that fails has left an unknown number of its blocks on
            // the card, and the ones before it are already there. Neither is
            // retried and neither is reported as success: the operation
            // failed, and the layer above tears the mount down rather than
            // trying to work out how much of it landed.
            if !sdmmc::write_blocks(&self.card, lba, chunk) {
                return Err(BlockError::DeviceError);
            }
            lba += (chunk.len() / SUPPORTED_BLOCK_BYTES as usize) as u32;
        }
        Ok(())
    }

    /// Nothing is buffered on this side, and no command is sent.
    ///
    /// This is not a weaker guarantee than the USB side's SYNCHRONIZE
    /// CACHE(10); it is the same one reached earlier. `sdmmc::write_blocks`
    /// does not return until CMD25 has completed *and* the card has released
    /// DAT0, which is the card saying it has finished programming what it
    /// was sent. There is no host-side write cache between here and that
    /// point for a flush to push out.
    fn flush(&mut self) -> Result<(), BlockError> {
        Ok(())
    }
}
