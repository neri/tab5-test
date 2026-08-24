//! [`BlockDevice`] adapter over `usb/msc.rs`.
//!
//! Like `sd.rs`, this only translates. BOT session state, Reset Recovery, the
//! proactive resynchronization every sixteen reads, the packet-level retries
//! and the READ(10) replay policy all stay below it, where the knowledge of
//! which commands are safe to repeat lives.
//!
//! Unlike the SD and RAM devices this one borrows rather than owns: the Mass
//! Storage session belongs to `UsbHost`, which is the single owner of the
//! bus. The adapter is therefore a view built for one operation and dropped,
//! which is also what lets a rescan replace the session underneath without
//! any filesystem-side handle pointing at the old one.

use super::block::{BlockDevice, BlockError, BlockGeometry, SUPPORTED_BLOCK_BYTES, check_range};
use super::fingerprint;
use crate::uart;
use crate::usb::UsbMassStorage;

/// Bytes per READ(10). READ(10) itself allows 65,535 blocks, but 4 KiB is
/// the transfer size the repeated-read acceptance testing in
/// `docs/USB_WRITE_STABILITY_PLAN.md` actually covers, and the bulk layer
/// splits everything into single MPS packets underneath regardless. Larger
/// requests are split here.
const MAX_TRANSFER_BYTES: usize = 4096;

/// VPD pages worth asking for, and the space kept for one.
const VPD_UNIT_SERIAL: u8 = 0x80;
const VPD_DEVICE_ID: u8 = 0x83;
const VPD_MAX: usize = 64;

pub struct UsbMscBlockDevice<'a> {
    storage: &'a mut UsbMassStorage,
    geometry: BlockGeometry,
}

impl<'a> UsbMscBlockDevice<'a> {
    /// Asks the device for its geometry with READ CAPACITY(10) and wraps it.
    ///
    /// Two bus commands, so this is per-operation cost that a mount holding
    /// the geometry will want to skip; that constructor arrives with the
    /// mount table.
    pub fn probe(storage: &'a mut UsbMassStorage) -> Result<Self, BlockError> {
        if storage.needs_reinit() {
            return Err(BlockError::TemporarilyUnavailable);
        }
        // An empty card reader is attached, enumerated and perfectly
        // healthy; it simply has no medium. That is worth saying as its own
        // error rather than as a failed command, because the answer for the
        // caller is to insert something, not to retry or rescan.
        match storage.test_unit_ready() {
            Some(true) => {}
            Some(false) => return Err(BlockError::NotReady),
            None => return Err(BlockError::DeviceError),
        }
        let Some(capacity) = storage.read_capacity() else {
            return Err(BlockError::DeviceError);
        };
        // `last_lba` is the address of the last block, so the count is one
        // more -- computed in `u64` because a device reporting `u32::MAX`
        // here would otherwise wrap to zero blocks.
        let geometry = BlockGeometry {
            block_bytes: capacity.block_length,
            block_count: capacity.last_lba as u64 + 1,
        };
        Ok(Self { storage, geometry })
    }

    /// Adds whatever the device will say about its own identity.
    ///
    /// Three commands, none of them required to succeed. The standard
    /// INQUIRY strings are almost always there; the two VPD pages are
    /// optional and plenty of USB sticks answer neither. Each one that does
    /// answer is recorded as a source, so a later comparison knows how much
    /// it is comparing.
    pub fn add_identity(&mut self, builder: &mut fingerprint::Builder) {
        if let Some(response) = self.storage.inquiry() {
            builder.inquiry(&response);
        }
        let mut page = [0u8; VPD_MAX];
        if let Some(length) = self.storage.vital_product_data(VPD_UNIT_SERIAL, &mut page) {
            builder.unit_serial(&page[..length]);
        }
        if let Some(length) = self.storage.vital_product_data(VPD_DEVICE_ID, &mut page) {
            builder.device_id(&page[..length]);
        }
    }

    /// Maps a failed transfer onto the shared error set.
    ///
    /// A dead BOT session is [`BlockError::TemporarilyUnavailable`], not
    /// removal: the device may well still be plugged in, and only a rescan
    /// comparing fingerprints can say whether the medium is the same one.
    /// Calling it removed here would invalidate open handles that are about
    /// to come back.
    fn transfer_error(&self) -> BlockError {
        if self.storage.needs_reinit() {
            BlockError::TemporarilyUnavailable
        } else {
            BlockError::DeviceError
        }
    }
}

impl BlockDevice for UsbMscBlockDevice<'_> {
    fn geometry(&self) -> BlockGeometry {
        self.geometry
    }

    fn read_blocks(&mut self, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        if self.geometry.block_bytes != SUPPORTED_BLOCK_BYTES {
            return Err(BlockError::UnsupportedBlockSize);
        }
        check_range(&self.geometry, lba, buffer.len())?;
        if self.storage.needs_reinit() {
            return Err(BlockError::TemporarilyUnavailable);
        }
        // READ(10) carries a 32-bit LBA. A device reporting more blocks than
        // that would need READ(16), which `msc.rs` does not implement.
        let mut lba = u32::try_from(lba).map_err(|_| BlockError::OutOfRange)?;

        for chunk in buffer.chunks_mut(MAX_TRANSFER_BYTES) {
            if !self.storage.read_blocks(lba, chunk) {
                return Err(self.transfer_error());
            }
            lba += (chunk.len() / SUPPORTED_BLOCK_BYTES as usize) as u32;
        }
        Ok(())
    }

    /// Always fails with [`BlockError::WriteSuppressed`], for the reasons
    /// given on `sd.rs`'s equivalent. `usbwritetest` and `usbzero` still
    /// write, but they call `msc.rs` directly and are not reached from here.
    fn write_blocks(&mut self, lba: u64, buffer: &[u8]) -> Result<(), BlockError> {
        let blocks = self.geometry.blocks_for(buffer.len()).unwrap_or(0);
        uart::log(b"FS: USB MSC write suppressed (read-only mount policy)\r\n");
        uart::log_hex(b"FS:   LBA=", lba as u32);
        uart::log_hex(b"FS:   blocks=", blocks as u32);
        Err(BlockError::WriteSuppressed)
    }

    /// No SYNCHRONIZE CACHE(10) is sent: this mount never writes, so there is
    /// nothing of ours in the device's cache to push out.
    fn flush(&mut self) -> Result<(), BlockError> {
        Ok(())
    }
}
