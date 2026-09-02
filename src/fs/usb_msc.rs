//! [`BlockDevice`] adapter over `usb/msc.rs`.
//!
//! Like `sd.rs`, this only translates. BOT session state, Reset Recovery,
//! the packet-level retries and the READ(10)-only replay policy all stay
//! below it, where the knowledge of which commands are safe to repeat lives.
//!
//! Unlike the SD and RAM devices this one borrows rather than owns: the Mass
//! Storage session belongs to `UsbHost`, which is the single owner of the
//! bus. The adapter is therefore a view built for one operation and dropped,
//! which is also what lets a rescan replace the session underneath without
//! any filesystem-side handle pointing at the old one.

use super::block::{BlockDevice, BlockError, BlockGeometry, SUPPORTED_BLOCK_BYTES, check_range};
use super::fingerprint;
use crate::usb::{
    CacheSync, UsbMassStorage, VPD_PAGE_DEVICE_ID, VPD_PAGE_UNIT_SERIAL, WriteOutcome,
};

/// Bytes per READ(10). READ(10) itself allows 65,535 blocks, but 4 KiB is the
/// transfer size the acceptance testing in
/// `docs/USB_WRITE_STABILITY_PLAN.md` actually covers, and the bulk layer
/// splits everything into single MPS packets underneath regardless. Larger
/// requests are split here.
const MAX_READ_BYTES: usize = 4096;

/// Blocks per WRITE(10). **One.**
///
/// Not a transfer budget like [`MAX_READ_BYTES`] but a limit on the *shape*
/// of the transfer. A WRITE(10) covering several blocks puts that many
/// 512-byte packets into a single data OUT phase, and this transport does
/// not come back from it: the packets are all accepted and the device then
/// never produces a CSW, so the bulk IN times out and the BOT session has to
/// be rebuilt.
///
/// Observed *deterministically* -- three single-block WRITE(10)s during one
/// `mkdir` went through, and the first eight-block one failed, in three runs
/// out of three. That matters
/// because the transport's other write failures
/// (`docs/USB_WRITE_STABILITY_PLAN.md`) are intermittent, and nothing before
/// the filesystem write path ever issued a multi-block WRITE(10):
/// `usbwritetest` writes one block and `usbzero` loops one block at a time,
/// so the shape had never been exercised.
///
/// One is the only value real hardware has accepted. Two is untested. This
/// constant is where to experiment once the OUT data phase is understood.
const MAX_WRITE_BLOCKS: usize = 1;
const MAX_WRITE_BYTES: usize = MAX_WRITE_BLOCKS * SUPPORTED_BLOCK_BYTES as usize;

/// One transfer's worth of outgoing data, staged so the BOT layer can have
/// the `&mut [u8]` its packet primitive shares with the receive direction.
///
/// The alternative would be casting the mutability back on to a
/// `BlockDevice::write_blocks` buffer, which is exactly the aliasing this
/// interface's shared slice is there to rule out. The copy is one block, so
/// it is nothing beside the transfer it feeds.
///
/// Aligned to a cache line because the controller's DMA reads straight out
/// of it. `usb/hcd.rs` writes back the CPU's cache over the transfer buffer
/// before starting a channel and **drops the result**, on the stated
/// grounds that every buffer DMA touches on that side declares an
/// alignment; the ROM cache routine refuses a span that starts mid-line, and
/// a refused writeback sends the device whatever was in RAM instead of what
/// was just written (`docs/KNOWN_ISSUES.md`). A plain `[u8; N]` has an
/// alignment of one, so this has to say so. 64 is
/// `psram::CACHE_LINE_BYTES`, which also satisfies the core's own word
/// alignment for the QTD data pointer. Every packet the BOT layer cuts out
/// of this is at an MPS multiple from the base, so all of them inherit it.
#[repr(C, align(64))]
struct WriteStaging([u8; MAX_WRITE_BYTES]);

/// TEST UNIT READY polls after a flush, at `msc.rs`'s own 100 ms interval.
/// The same budget `usbwritetest` waits on real hardware. A device is
/// normally ready on the first ask; this is for the one that goes away for a
/// moment while it commits.
const FLUSH_READY_ATTEMPTS: u32 = 10;

/// Space kept for one VPD page. The page codes themselves come from
/// `usb/msc.rs`, which is where the rule about only asking for pages the
/// device lists lives.
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
        if let Some(length) = self
            .storage
            .vital_product_data(VPD_PAGE_UNIT_SERIAL, &mut page)
        {
            builder.unit_serial(&page[..length]);
        }
        if let Some(length) = self
            .storage
            .vital_product_data(VPD_PAGE_DEVICE_ID, &mut page)
        {
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

        for chunk in buffer.chunks_mut(MAX_READ_BYTES) {
            if !self.storage.read_blocks(lba, chunk) {
                return Err(self.transfer_error());
            }
            lba += (chunk.len() / SUPPORTED_BLOCK_BYTES as usize) as u32;
        }
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buffer: &[u8]) -> Result<(), BlockError> {
        if self.geometry.block_bytes != SUPPORTED_BLOCK_BYTES {
            return Err(BlockError::UnsupportedBlockSize);
        }
        check_range(&self.geometry, lba, buffer.len())?;
        if self.storage.needs_reinit() {
            return Err(BlockError::TemporarilyUnavailable);
        }
        // WRITE(10) carries a 32-bit LBA, like READ(10).
        let mut lba = u32::try_from(lba).map_err(|_| BlockError::OutOfRange)?;

        let mut staging = WriteStaging([0; MAX_WRITE_BYTES]);
        for chunk in buffer.chunks(MAX_WRITE_BYTES) {
            let staged = &mut staging.0[..chunk.len()];
            staged.copy_from_slice(chunk);
            // `msc.rs` does not replay a failed WRITE(10) and neither does
            // this: a chunk that failed may have reached the medium in part,
            // and the chunks before it certainly did. Reporting the whole
            // request as failed is the only honest answer, and the mount is
            // torn down above rather than continued against a medium whose
            // contents are now unknown.
            match self.storage.write_blocks(lba, staged) {
                WriteOutcome::Written => {}
                WriteOutcome::WriteProtected => return Err(BlockError::WriteProtected),
                WriteOutcome::Failed => return Err(self.transfer_error()),
            }
            lba += (chunk.len() / SUPPORTED_BLOCK_BYTES as usize) as u32;
        }
        Ok(())
    }

    /// Asks the device to commit its write cache with SYNCHRONIZE CACHE(10).
    ///
    /// A WRITE(10) the device accepted may still be in its cache, so this is
    /// what a caller about to tell the user the file is written depends on.
    ///
    /// A device that refuses the command is treated as success. That is a
    /// deliberate limit of what this firmware promises: the write reached
    /// the device, nothing further can be asked of it, and refusing the
    /// operation over a flush the device does not implement would make such
    /// sticks unwritable rather than making them safer. `msc.rs` says so on
    /// the UART once per attachment, which is where the qualification lives.
    ///
    /// A flush that did happen is followed by waiting for the unit to report
    /// ready again. Committing a cache can take a device away for a moment,
    /// and returning before it comes back would leave the next command --
    /// often the caller's own read-back -- to fail on a device that is
    /// simply busy.
    fn flush(&mut self) -> Result<(), BlockError> {
        match self.storage.synchronize_cache() {
            CacheSync::Flushed => {
                if self.storage.wait_until_ready(FLUSH_READY_ATTEMPTS) {
                    Ok(())
                } else {
                    Err(self.transfer_error())
                }
            }
            // Nothing was committed, so there is nothing to come back from.
            CacheSync::Unsupported => Ok(()),
            CacheSync::Failed => Err(self.transfer_error()),
        }
    }
}
