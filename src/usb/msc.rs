//! USB Mass Storage class driver for the SCSI Transparent Command Set over
//! Bulk-Only Transport. The BOT envelope lives in `bot.rs`.

use super::bot::{self, BotInterface, BulkOnlyTransport};
use super::protocol::EnumeratedDevice;
use crate::delay::delay_ms;
use crate::{tick, uart};

const INTERFACE_SUBCLASS_SCSI_TRANSPARENT: u8 = 0x06;
const SCSI_TEST_UNIT_READY: u8 = 0x00;
const SCSI_REQUEST_SENSE: u8 = 0x03;
const SCSI_INQUIRY: u8 = 0x12;
const SCSI_READ_10: u8 = 0x28;
const SCSI_WRITE_10: u8 = 0x2A;
const SCSI_SYNCHRONIZE_CACHE_10: u8 = 0x35;
/// READ(10)/WRITE(10) byte 1 bit 3, Force Unit Access: the command must use
/// the medium rather than the device's cache. On a read that is the only way
/// to see what would survive unplugging.
const CDB_FLAG_FUA: u8 = 0x08;
/// SPC sense key 5, ILLEGAL REQUEST: the device is refusing the command as
/// asked, rather than failing to carry it out. Which additional sense code
/// comes with it is not worth branching on -- a device that does not
/// implement SYNCHRONIZE CACHE(10) may answer `0x20` (INVALID COMMAND
/// OPERATION CODE) or, as one of this project's real USB sticks does,
/// `0x24` (INVALID FIELD IN CDB). Either way asking again is pointless.
const SENSE_KEY_ILLEGAL_REQUEST: u8 = 0x05;
const SCSI_READ_CAPACITY_10: u8 = 0x25;
const CSW_STATUS_PASSED: u8 = 0x00;
const BLOCK_BYTES: usize = 512;
/// The shortest observed run to an unresponsive device completed 33
/// READ(10)s. Re-establish the BOT boundary at half that distance while EP0
/// still answers, without resetting the root port or unrelated HID devices.
const READS_PER_BOT_RESYNC: u8 = 16;
const INQUIRY_RESPONSE_LEN: usize = 36;
const REQUEST_SENSE_RESPONSE_LEN: usize = 18;
const READ_CAPACITY_10_RESPONSE_LEN: usize = 8;
/// Largest VPD page this reads. The serial-number and device-identification
/// pages are tens of bytes in practice; a longer page is truncated, which is
/// harmless for a fingerprint as long as the truncation is consistent.
const VPD_RESPONSE_MAX: usize = 64;
/// Peripheral type, page code, and the 16-bit page length.
const VPD_HEADER_LEN: usize = 4;
const READ_CAPACITY_10_NEEDS_CAPACITY_16: u32 = 0xFFFF_FFFF;
const READY_POLL_INTERVAL_MS: u32 = 100;
/// SPC sense key 2, "the logical unit is not ready".
const SENSE_KEY_NOT_READY: u8 = 0x02;
/// SPC additional sense code `0x3A`, MEDIUM NOT PRESENT. Every one of its
/// qualifiers (tray closed, tray open, loadable, auxiliary memory
/// accessible) means the same thing to a filesystem probe: there is nothing
/// in the drive, and waiting will not put anything there.
const ASC_MEDIUM_NOT_PRESENT: u8 = 0x3A;

/// How long an attached device took to become usable as a filesystem, in
/// milliseconds measured from the first SCSI command sent to it.
///
/// A device that answers control transfers is not yet a device a filesystem
/// can be mounted from: it may still be spinning up, reporting a unit
/// attention from the power-on it just went through, or waiting for media.
/// The boot-time storage selection has to budget for that separately from
/// enumeration, which is what `docs/USB_MSC_BOOT_MARGIN_PLAN.md` sizes.
/// Why [`UsbMassStorage::measure_ready_and_first_read`] stopped.
///
/// A boot-time storage probe needs more than a yes/no: `NoMedium` is a
/// device that is working correctly and simply has no card in it, which
/// should hand the boot straight to the next medium, while `NotReady` is a
/// device that ran out of budget and might have succeeded with more time.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum ReadyOutcome {
    /// The budget ran out while the unit still reported not ready.
    #[default]
    NotReady,
    /// The unit's own sense data says there is no medium in it. Reported
    /// as soon as the first REQUEST SENSE says so, without spending the
    /// rest of the budget.
    NoMedium,
    /// A BOT transfer failed in a way `bot`'s reset recovery could not put
    /// right, so no further command would mean anything.
    TransportFailed,
    /// The unit reported ready but READ CAPACITY(10) failed.
    CapacityFailed,
    /// Capacity was read but the first READ(10) of LBA 0 failed.
    ReadFailed,
    /// Ready, sized, and LBA 0 read back -- usable as a filesystem.
    Usable,
}

#[derive(Clone, Copy, Default)]
pub struct ReadyTiming {
    /// TEST UNIT READY first reported a ready unit after this long.
    pub ready_ms: u32,
    /// TEST UNIT READY commands it took to get there, including the first.
    pub attempts: u32,
    /// READ CAPACITY(10) completed by this point.
    pub capacity_ms: u32,
    /// The first 512-byte READ(10) of LBA 0 -- what a filesystem probe
    /// actually needs -- completed by this point.
    pub first_read_ms: u32,
    /// How the sequence ended. The milliseconds above are the point at
    /// which it did.
    pub outcome: ReadyOutcome,
}

impl ReadyTiming {
    /// Whether a filesystem could be mounted from this device.
    pub fn usable(&self) -> bool {
        self.outcome == ReadyOutcome::Usable
    }
}

/// SCSI READ CAPACITY(10) result. `last_lba` is inclusive.
pub struct ReadCapacity {
    pub last_lba: u32,
    pub block_length: u32,
}

pub type MscInterface = BotInterface;

pub fn find_msc_interface(config: &[u8]) -> Option<MscInterface> {
    bot::find_interface(config, INTERFACE_SUBCLASS_SCSI_TRANSPARENT)
}

/// What a [`UsbMassStorage::synchronize_cache`] call achieved.
///
/// "Unsupported" has to be distinguishable from "failed": a device that
/// never implements the command is not misbehaving, but it also means a
/// write cannot be proven to have reached the medium that way, so the
/// caller has to fall back on Force Unit Access reads.
///
/// **Neither is evidence that a write failed.** Whether the data is on the
/// medium is what a Force Unit Access read answers; a flush that the device
/// would not perform says nothing about the write that preceded it, and
/// callers must not report one as the other.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CacheSync {
    Flushed,
    Unsupported,
    Failed,
}

pub struct UsbMassStorage {
    bot: BulkOnlyTransport,
    read_retries: u32,
    reads_since_resync: u8,
    maintenance_resyncs: u32,
    /// Set once the device has answered SYNCHRONIZE CACHE(10) with ILLEGAL
    /// REQUEST/INVALID COMMAND. Asking again would fail the same way, and
    /// every failed command leaves sense data that has to be collected
    /// before the next one -- cheaper and safer to stop asking.
    cache_sync_unsupported: bool,
}

impl UsbMassStorage {
    pub fn attach(device: &EnumeratedDevice) -> Option<Self> {
        let interface = find_msc_interface(device.config_bytes())?;
        let bot = BulkOnlyTransport::attach(device, interface)?;
        Some(Self {
            bot,
            read_retries: 0,
            reads_since_resync: 0,
            maintenance_resyncs: 0,
            cache_sync_unsupported: false,
        })
    }

    pub fn inquiry(&mut self) -> Option<[u8; INQUIRY_RESPONSE_LEN]> {
        let mut data = [0u8; INQUIRY_RESPONSE_LEN];
        let cdb = [SCSI_INQUIRY, 0, 0, 0, INQUIRY_RESPONSE_LEN as u8, 0];
        let result = self.bot.execute_command(&cdb, true, &mut data)?;
        if result.status != CSW_STATUS_PASSED {
            uart::log_hex(
                b"USB MSC: INQUIRY failed, CSW status=",
                result.status as u32,
            );
            return None;
        }
        if result.transferred < data.len() {
            uart::log(b"USB MSC: short INQUIRY response\r\n");
            return None;
        }
        Some(data)
    }

    /// Reads a Vital Product Data page: `0x80` is the unit serial number,
    /// `0x83` the device identification list.
    ///
    /// These are the closest thing SCSI has to a medium identity, and unlike
    /// USB's `iSerialNumber` they describe the storage device rather than the
    /// enclosure. Both are optional, and plenty of USB sticks answer neither.
    /// A device that does not support them answers CHECK CONDITION, which is
    /// reported as `None` -- absence, not failure, and the caller records
    /// that this source had nothing to say rather than treating the medium as
    /// unidentifiable.
    ///
    /// Returns the page's payload, without the four-byte header.
    pub fn vital_product_data(&mut self, page: u8, out: &mut [u8]) -> Option<usize> {
        let mut data = [0u8; VPD_RESPONSE_MAX];
        // EVPD set in byte 1, page code in byte 2, allocation length in 3-4.
        let cdb = [
            SCSI_INQUIRY,
            1,
            page,
            (VPD_RESPONSE_MAX >> 8) as u8,
            VPD_RESPONSE_MAX as u8,
            0,
        ];
        let result = self.bot.execute_command(&cdb, true, &mut data)?;
        if result.status != CSW_STATUS_PASSED {
            // Not logged as an error: declining to answer is a legal reply,
            // and the sense data still has to be collected so the next
            // command does not start on top of it.
            let _ = self.collect_sense(b"USB MSC: INQUIRY(EVPD)");
            return None;
        }
        if result.transferred < VPD_HEADER_LEN {
            return None;
        }
        // Byte 1 echoes the page code; a device that answered with a
        // different page has not answered this question.
        if data[1] != page {
            return None;
        }
        let length = u16::from_be_bytes([data[2], data[3]]) as usize;
        let available = result.transferred - VPD_HEADER_LEN;
        let length = length.min(available).min(out.len());
        out[..length].copy_from_slice(&data[VPD_HEADER_LEN..VPD_HEADER_LEN + length]);
        Some(length)
    }

    pub fn test_unit_ready(&mut self) -> Option<bool> {
        let mut no_data = [];
        let result =
            self.bot
                .execute_command(&[SCSI_TEST_UNIT_READY, 0, 0, 0, 0, 0], true, &mut no_data)?;
        Some(result.status == CSW_STATUS_PASSED)
    }

    pub fn wait_until_ready(&mut self, attempts: u32) -> bool {
        for attempt in 0..attempts.max(1) {
            match self.test_unit_ready() {
                Some(true) => return true,
                Some(false) => {}
                // A transport failure has already run BOT Reset Recovery in
                // execute_command. Continue only when that full sequence
                // succeeded; otherwise a new CBW would enter an unknown
                // device-side BOT phase.
                None if self.bot.last_recovery_succeeded() => {}
                None => return false,
            }
            if attempt + 1 < attempts {
                delay_ms(READY_POLL_INTERVAL_MS);
            }
        }
        false
    }

    /// Runs the sequence a boot-time filesystem probe would run -- wait for
    /// the unit to report ready, read the capacity, read LBA 0 -- and reports
    /// how long each step took.
    ///
    /// `budget_ms` bounds only the TEST UNIT READY polling; the two transfers
    /// after it carry `bot`'s own transfer timeouts. Read-only, so it is safe
    /// to repeat on a device the user has not asked to write to.
    pub fn measure_ready_and_first_read(&mut self, budget_ms: u32) -> ReadyTiming {
        let start = tick::now_ms();
        let mut timing = ReadyTiming::default();

        loop {
            timing.attempts += 1;
            match self.test_unit_ready() {
                Some(true) => break,
                // "Not ready" alone does not say whether waiting helps. Ask
                // the unit why: an empty card reader answers this way for as
                // long as it is plugged in, and spending the whole budget on
                // it only delays the boot that has to fall back to another
                // medium anyway.
                Some(false) => {
                    if self.medium_absent() {
                        timing.ready_ms = elapsed_ms(start);
                        timing.outcome = ReadyOutcome::NoMedium;
                        return timing;
                    }
                }
                // execute_command has already run BOT Reset Recovery. Only a
                // completed recovery leaves the device in a known BOT phase.
                None if self.bot.last_recovery_succeeded() => {}
                None => {
                    timing.ready_ms = elapsed_ms(start);
                    timing.outcome = ReadyOutcome::TransportFailed;
                    return timing;
                }
            }
            if elapsed_ms(start) >= budget_ms {
                timing.ready_ms = elapsed_ms(start);
                return timing;
            }
            delay_ms(READY_POLL_INTERVAL_MS);
        }
        timing.ready_ms = elapsed_ms(start);

        if self.read_capacity().is_none() {
            timing.capacity_ms = elapsed_ms(start);
            timing.outcome = ReadyOutcome::CapacityFailed;
            return timing;
        }
        timing.capacity_ms = elapsed_ms(start);

        let mut block = [0u8; BLOCK_BYTES];
        let read_ok = self.read_blocks(0, &mut block);
        timing.first_read_ms = elapsed_ms(start);
        timing.outcome = if read_ok {
            ReadyOutcome::Usable
        } else {
            ReadyOutcome::ReadFailed
        };
        timing
    }

    /// Whether the unit's sense data says it is not ready *because it is
    /// empty*, as opposed to still starting up.
    ///
    /// A failed REQUEST SENSE answers `false`: an unreadable reason is not
    /// evidence of an absent medium, and the caller's budget still bounds
    /// how long it keeps asking.
    fn medium_absent(&mut self) -> bool {
        let Some(sense) = self.request_sense() else {
            return false;
        };
        // SPC fixed-format sense data: byte 2 carries the sense key in its
        // low nibble, byte 12 the additional sense code.
        (sense[2] & 0x0F) == SENSE_KEY_NOT_READY && sense[12] == ASC_MEDIUM_NOT_PRESENT
    }

    pub fn request_sense(&mut self) -> Option<[u8; REQUEST_SENSE_RESPONSE_LEN]> {
        let mut data = [0u8; REQUEST_SENSE_RESPONSE_LEN];
        let cdb = [
            SCSI_REQUEST_SENSE,
            0,
            0,
            0,
            REQUEST_SENSE_RESPONSE_LEN as u8,
            0,
        ];
        let result = self.bot.execute_command(&cdb, true, &mut data)?;
        if result.status != CSW_STATUS_PASSED {
            uart::log_hex(
                b"USB MSC: REQUEST SENSE failed, CSW status=",
                result.status as u32,
            );
            return None;
        }
        if result.transferred < data.len() {
            uart::log(b"USB MSC: short REQUEST SENSE response\r\n");
            return None;
        }
        Some(data)
    }

    pub fn read_capacity(&mut self) -> Option<ReadCapacity> {
        let mut data = [0u8; READ_CAPACITY_10_RESPONSE_LEN];
        let result = self.bot.execute_command(
            &[SCSI_READ_CAPACITY_10, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            true,
            &mut data,
        )?;
        if result.status != CSW_STATUS_PASSED {
            uart::log_hex(
                b"USB MSC: READ CAPACITY(10) failed, CSW status=",
                result.status as u32,
            );
            return None;
        }
        if result.transferred < data.len() {
            uart::log(b"USB MSC: short READ CAPACITY(10) response\r\n");
            return None;
        }
        let last_lba = u32::from_be_bytes(data[0..4].try_into().unwrap());
        if last_lba == READ_CAPACITY_10_NEEDS_CAPACITY_16 {
            uart::log(b"USB MSC: device needs READ CAPACITY(16), not implemented\r\n");
            return None;
        }
        Some(ReadCapacity {
            last_lba,
            block_length: u32::from_be_bytes(data[4..8].try_into().unwrap()),
        })
    }

    pub fn read_blocks(&mut self, lba: u32, buffer: &mut [u8]) -> bool {
        self.read_10(lba, buffer, 0)
    }

    /// True when this device's BOT session has failed and must be rebuilt
    /// before another command. A failed MSC session is not evidence that
    /// unrelated HID sessions on the shared bus are stale.
    pub fn needs_reinit(&self) -> bool {
        self.bot.needs_reinit()
    }

    /// Reads whole blocks with Force Unit Access, so the data comes from the
    /// medium instead of the device's cache.
    ///
    /// Only a verification read needs this. A write that has been accepted
    /// may still be sitting in the device's cache, and an ordinary read is
    /// free to be answered from that same cache -- which would make a
    /// write-then-read-back test agree with itself while the medium holds
    /// something else. Devices are allowed to reject FUA, so the caller has
    /// to be ready to fall back on [`Self::read_blocks`] and say so.
    pub fn read_blocks_from_medium(&mut self, lba: u32, buffer: &mut [u8]) -> bool {
        self.read_10(lba, buffer, CDB_FLAG_FUA)
    }

    fn read_10(&mut self, lba: u32, buffer: &mut [u8], flags: u8) -> bool {
        if buffer.is_empty() || buffer.len() % BLOCK_BYTES != 0 {
            uart::log(
                b"USB MSC: block transfer length must be a nonzero multiple of 512 bytes\r\n",
            );
            return false;
        }
        let block_count = buffer.len() / BLOCK_BYTES;
        if block_count > u16::MAX as usize {
            uart::log(b"USB MSC: too many blocks for one READ(10) transfer\r\n");
            return false;
        }
        if self.reads_since_resync >= READS_PER_BOT_RESYNC {
            if !self.proactive_resynchronize(b"USB MSC: proactive BOT resync before READ(10)\r\n") {
                return false;
            }
        }
        let cdb = [
            SCSI_READ_10,
            flags,
            (lba >> 24) as u8,
            (lba >> 16) as u8,
            (lba >> 8) as u8,
            lba as u8,
            0,
            (block_count >> 8) as u8,
            block_count as u8,
            0,
        ];
        let result = match self.bot.execute_command(&cdb, true, buffer) {
            Some(result) => result,
            None => {
                // execute_command has already completed BOT Reset Recovery.
                // READ(10) is read-only, so replaying it once is safe. Do not
                // put this retry in the generic BOT layer: a future write
                // command must not be replayed without command-specific
                // knowledge of whether its data reached the device.
                if !self.bot.last_recovery_succeeded() {
                    return false;
                }
                self.reads_since_resync = 0;
                self.read_retries = self.read_retries.wrapping_add(1);
                uart::log(b"USB MSC: retrying READ(10) after BOT recovery\r\n");
                let Some(result) = self.bot.execute_command(&cdb, true, buffer) else {
                    return false;
                };
                result
            }
        };
        if result.status != CSW_STATUS_PASSED {
            uart::log_hex(
                b"USB MSC: READ(10) failed, CSW status=",
                result.status as u32,
            );
            let _ = self.collect_sense(b"USB MSC: READ(10)");
            return false;
        }
        if result.transferred < buffer.len() {
            uart::log(b"USB MSC: short READ(10) response\r\n");
            return false;
        }
        self.reads_since_resync = self.reads_since_resync.saturating_add(1);
        true
    }

    /// Writes whole 512-byte blocks with SCSI WRITE(10).
    ///
    /// `buffer` is `&mut` for the same reason `sdmmc::write_blocks`' is: the
    /// BOT layer below hands the data straight to the controller's packet
    /// primitive, which owns the slice while the transfer runs.
    ///
    /// **A failed write is never retried automatically.** `read_blocks`
    /// replays a READ(10) once after BOT Reset Recovery because re-reading a
    /// block cannot change it; a WRITE(10) that failed mid-transport may
    /// have reached the medium in part, and replaying it would turn one
    /// uncertain block into an unknown number of them. The caller is told
    /// the write failed and decides what to do -- which is why this retry
    /// policy lives here rather than in `bot.rs`.
    pub fn write_blocks(&mut self, lba: u32, buffer: &mut [u8]) -> bool {
        if buffer.is_empty() || buffer.len() % BLOCK_BYTES != 0 {
            uart::log(
                b"USB MSC: block transfer length must be a nonzero multiple of 512 bytes\r\n",
            );
            return false;
        }
        let block_count = buffer.len() / BLOCK_BYTES;
        if block_count > u16::MAX as usize {
            uart::log(b"USB MSC: too many blocks for one WRITE(10) transfer\r\n");
            return false;
        }
        // WRITE is both rare and unsafe to replay after a transport failure.
        // Start every one at a freshly synchronized BOT boundary instead of
        // carrying accumulated endpoint/device state into the one command we
        // cannot recover by simply issuing again.
        if !self.proactive_resynchronize(b"USB MSC: proactive BOT resync before WRITE(10)\r\n") {
            return false;
        }
        let cdb = [
            SCSI_WRITE_10,
            0,
            (lba >> 24) as u8,
            (lba >> 16) as u8,
            (lba >> 8) as u8,
            lba as u8,
            0,
            (block_count >> 8) as u8,
            block_count as u8,
            0,
        ];
        let Some(result) = self.bot.execute_command(&cdb, false, buffer) else {
            uart::log(b"USB MSC: WRITE(10) transport failed, not retrying\r\n");
            return false;
        };
        if result.status != CSW_STATUS_PASSED {
            // The status alone does not say why: a write-protected device
            // and a dying one fail identically here. The sense data does,
            // and collecting it also clears the condition the device is
            // holding.
            uart::log_hex(
                b"USB MSC: WRITE(10) failed, CSW status=",
                result.status as u32,
            );
            let _ = self.collect_sense(b"USB MSC: WRITE(10)");
            return false;
        }
        true
    }

    fn proactive_resynchronize(&mut self, message: &[u8]) -> bool {
        uart::log(message);
        if !self.bot.resynchronize() {
            return false;
        }
        self.reads_since_resync = 0;
        self.maintenance_resyncs = self.maintenance_resyncs.wrapping_add(1);
        true
    }

    /// Asks the device to commit its write cache to the medium
    /// (SCSI SYNCHRONIZE CACHE(10), whole-medium form).
    ///
    /// A WRITE(10) that returns success has reached the *device*, not
    /// necessarily the medium: a device is free to answer from its cache and
    /// flush later, and a read-back is free to be answered from that same
    /// cache. So a write followed by a matching read proves nothing about
    /// what survives unplugging. Anything that writes has to flush before it
    /// tells the user the data is there.
    ///
    /// `false` from a device that does not implement the command is not a
    /// data-integrity failure by itself; callers report it rather than
    /// treating it as corruption.
    pub fn synchronize_cache(&mut self) -> CacheSync {
        if self.cache_sync_unsupported {
            return CacheSync::Unsupported;
        }
        let mut no_data = [];
        let cdb = [SCSI_SYNCHRONIZE_CACHE_10, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let Some(result) = self.bot.execute_command(&cdb, true, &mut no_data) else {
            uart::log(b"USB MSC: SYNCHRONIZE CACHE(10) transport failed\r\n");
            return CacheSync::Failed;
        };
        if result.status != CSW_STATUS_PASSED {
            uart::log_hex(
                b"USB MSC: SYNCHRONIZE CACHE(10) failed, CSW status=",
                result.status as u32,
            );
            let unsupported = self
                .collect_sense(b"USB MSC: SYNCHRONIZE CACHE(10)")
                .is_some_and(|sense| (sense[2] & 0x0F) == SENSE_KEY_ILLEGAL_REQUEST);
            if unsupported {
                uart::log(b"USB MSC: device refuses SYNCHRONIZE CACHE(10), not asking again\r\n");
                self.cache_sync_unsupported = true;
                return CacheSync::Unsupported;
            }
            return CacheSync::Failed;
        }
        CacheSync::Flushed
    }

    /// Reads and logs the sense data a CHECK CONDITION left behind.
    ///
    /// Doing this is not only diagnostics: a device holds that sense until
    /// somebody collects it, and the status byte alone never says whether a
    /// command failed because it is unsupported, because the medium is
    /// write protected, or because something went wrong this once.
    fn collect_sense(&mut self, context: &[u8]) -> Option<[u8; REQUEST_SENSE_RESPONSE_LEN]> {
        let sense = self.request_sense()?;
        uart::log(context);
        uart::log_hex(b" sense key=", (sense[2] & 0x0F) as u32);
        uart::log_hex(b"USB MSC: ASC=", sense[12] as u32);
        uart::log_hex(b"USB MSC: ASCQ=", sense[13] as u32);
        Some(sense)
    }

    /// Monotonic count of read-only READ(10) replays in this attachment.
    pub fn read_retry_count(&self) -> u32 {
        self.read_retries
    }

    /// Successful proactive BOT boundary resets in this attachment.
    pub fn maintenance_resync_count(&self) -> u32 {
        self.maintenance_resyncs
    }

    /// Monotonic count of lower-level QTD suffix resubmissions.
    pub fn packet_retry_count(&self) -> u32 {
        self.bot.packet_retry_count()
    }

    /// Enumerated Bulk IN maximum packet size, useful for confirming whether
    /// a diagnostic run actually re-enumerated in High- or Full-Speed mode.
    pub fn bulk_in_mps(&self) -> u16 {
        self.bot.bulk_in_mps()
    }
}

fn elapsed_ms(start: u64) -> u32 {
    tick::now_ms().saturating_sub(start) as u32
}
