//! USB Mass Storage class driver (Bulk-Only Transport), built on top of
//! `protocol.rs`'s generic control transfers and `hcd.rs`'s raw
//! channel/packet primitive (used directly for Bulk transfers, which are
//! not control transfers and so do not go through `protocol.rs` at all) --
//! the storage-class counterpart to `hid_keyboard.rs`/`hub.rs`.
//!
//! Staged per `USB_MSC_PLAN.md`: `find_msc_interface` (Bulk IN/OUT endpoint
//! discovery) is Stage 1. `UsbMassStorage::bulk_transfer_in`/
//! `bulk_transfer_out` (the Bulk transfer primitive and per-endpoint
//! data-toggle bookkeeping) are Stage 2. Bulk-Only Transport's CBW/CSW
//! framing and the first SCSI command, INQUIRY (`UsbMassStorage::inquiry`),
//! are Stage 3.

use super::hcd::{self, Endpoint, HCCHAR_EPTYPE_BULK, PacketOutcome};
use super::protocol::{self, ControlPipe, EnumeratedDevice, REQUEST_SET_CONFIGURATION};
use crate::delay::delay_ms;
use crate::uart;

// ------------------------------------------------------------------------
// Stage 1: interface/endpoint discovery
// ------------------------------------------------------------------------

// Mass Storage interface descriptor values (USB Mass Storage Class Bulk-Only
// Transport spec, section 2): SCSI transparent command set, Bulk-Only
// Transport signaling. Other subclass/protocol combinations (UFI, CBI, ATAPI
// command sets) exist but are not targeted here -- USB flash drives
// universally report this combination.
const INTERFACE_CLASS_MASS_STORAGE: u8 = 0x08;
const INTERFACE_SUBCLASS_SCSI_TRANSPARENT: u8 = 0x06;
const INTERFACE_PROTOCOL_BULK_ONLY: u8 = 0x50;

/// `bmAttributes` bits[1:0]: transfer type (USB2.0 table 9-13). Bulk-Only
/// Transport uses exactly one Bulk IN and one Bulk OUT endpoint; there is no
/// separate status/interrupt endpoint like HID's.
const ENDPOINT_TYPE_BULK: u8 = 0x02;

/// SCSI READ CAPACITY(10) result (see `UsbMassStorage::read_capacity`).
/// `last_lba` is the highest valid LBA, *not* a block count -- the device
/// has `last_lba + 1` blocks of `block_length` bytes each.
pub struct ReadCapacity {
    pub last_lba: u32,
    pub block_length: u32,
}

pub struct MscInterface {
    pub bulk_in_endpoint: u8,
    pub bulk_in_mps: u16,
    pub bulk_out_endpoint: u8,
    pub bulk_out_mps: u16,
}

/// Walks a configuration descriptor's interface/endpoint chain (see
/// `protocol::EnumeratedDevice::config_bytes`) looking for a Bulk-Only
/// Transport Mass Storage interface's Bulk IN and Bulk OUT endpoints.
/// Ignores every other interface, the same way
/// `hid_keyboard::find_hid_keyboard` ignores non-HID ones.
pub fn find_msc_interface(config: &[u8]) -> Option<MscInterface> {
    let mut offset = 0usize;
    while offset + 2 <= config.len() {
        let length = config[offset] as usize;
        if length < 2 || offset + length > config.len() {
            break;
        }
        let descriptor_type = config[offset + 1];
        if descriptor_type == protocol::DESCRIPTOR_TYPE_INTERFACE && length >= 9 {
            let interface_class = config[offset + 5];
            let interface_subclass = config[offset + 6];
            let interface_protocol = config[offset + 7];
            let is_target_interface = interface_class == INTERFACE_CLASS_MASS_STORAGE
                && interface_subclass == INTERFACE_SUBCLASS_SCSI_TRANSPARENT
                && interface_protocol == INTERFACE_PROTOCOL_BULK_ONLY;
            if is_target_interface && let Some(msc) = scan_bulk_endpoints(config, offset + length) {
                return Some(msc);
            }
        }
        offset += length;
    }
    None
}

/// Scans forward from just past an MSC interface descriptor for its Bulk IN
/// and Bulk OUT endpoint descriptors, stopping at the next interface
/// descriptor (or the end of the buffer). Returns `None` if either
/// direction is missing -- a malformed or non-BOT descriptor set this
/// project does not know how to drive.
fn scan_bulk_endpoints(config: &[u8], start: usize) -> Option<MscInterface> {
    let mut bulk_in: Option<(u8, u16)> = None;
    let mut bulk_out: Option<(u8, u16)> = None;
    let mut offset = start;
    while offset + 2 <= config.len() {
        let length = config[offset] as usize;
        if length < 2 || offset + length > config.len() {
            break;
        }
        let descriptor_type = config[offset + 1];
        if descriptor_type == protocol::DESCRIPTOR_TYPE_INTERFACE {
            break; // next interface started; stop looking
        }
        if descriptor_type == protocol::DESCRIPTOR_TYPE_ENDPOINT && length >= 7 {
            let endpoint_address = config[offset + 2];
            let attributes = config[offset + 3];
            let mps = u16::from_le_bytes([config[offset + 4], config[offset + 5]]);
            if attributes & 0x03 == ENDPOINT_TYPE_BULK {
                if endpoint_address & 0x80 != 0 {
                    bulk_in.get_or_insert((endpoint_address, mps));
                } else {
                    bulk_out.get_or_insert((endpoint_address, mps));
                }
            }
        }
        offset += length;
        if bulk_in.is_some() && bulk_out.is_some() {
            break;
        }
    }

    let (bulk_in_endpoint, bulk_in_mps) = bulk_in?;
    let (bulk_out_endpoint, bulk_out_mps) = bulk_out?;
    Some(MscInterface {
        bulk_in_endpoint,
        bulk_in_mps,
        bulk_out_endpoint,
        bulk_out_mps,
    })
}

// ------------------------------------------------------------------------
// Stage 3: Bulk-Only Transport CBW/CSW framing
// ------------------------------------------------------------------------

const CBW_LEN: usize = 31;
const CSW_LEN: usize = 13;
const CBW_SIGNATURE: [u8; 4] = *b"USBC";
const CSW_SIGNATURE_U32: u32 = u32::from_le_bytes(*b"USBS");
const CBW_FLAGS_DATA_IN: u8 = 0x80;
const CBW_FLAGS_DATA_OUT: u8 = 0x00;
const CSW_STATUS_PASSED: u8 = 0x00;

/// Builds a Command Block Wrapper (BOT spec section 5.1): fixed 31-byte
/// header plus up to 16 bytes of SCSI CDB. `cdb` longer than 16 bytes is
/// truncated -- no SCSI command this driver issues needs more.
fn build_cbw(tag: u32, data_transfer_length: u32, flags: u8, cdb: &[u8]) -> [u8; CBW_LEN] {
    let cdb_len = cdb.len().min(16);
    let mut cbw = [0u8; CBW_LEN];
    cbw[0..4].copy_from_slice(&CBW_SIGNATURE);
    cbw[4..8].copy_from_slice(&tag.to_le_bytes());
    cbw[8..12].copy_from_slice(&data_transfer_length.to_le_bytes());
    cbw[12] = flags;
    cbw[13] = 0; // LUN 0 -- single-LUN devices only
    cbw[14] = cdb_len as u8;
    cbw[15..15 + cdb_len].copy_from_slice(&cdb[..cdb_len]);
    cbw
}

struct CommandStatus {
    tag: u32,
    status: u8,
}

/// Parses a Command Status Wrapper (BOT spec section 5.2): signature, tag
/// (checked against the CBW's by the caller), residue (not used yet -- no
/// caller cares whether a short transfer was expected), and status (0 =
/// Passed, 1 = Failed, 2 = Phase Error).
fn parse_csw(bytes: &[u8; CSW_LEN]) -> Option<CommandStatus> {
    let signature = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if signature != CSW_SIGNATURE_U32 {
        uart::log_hex(b"USB MSC: bad CSW signature=", signature);
        return None;
    }
    Some(CommandStatus {
        tag: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        status: bytes[12],
    })
}

// Standard CLEAR_FEATURE(ENDPOINT_HALT) (USB2.0 9.4.1/9.4.5): host-to-device,
// standard, *endpoint* recipient -- distinct from `hub.rs`'s class/other-
// recipient feature requests, and from `protocol::build_standard_out_setup`
// (hardcoded to the device recipient, used for SET_CONFIGURATION/
// SET_ADDRESS). Required by the BOT spec after a STALL: the endpoint stays
// halted, and USB2.0 9.4.5 only guarantees its data toggle resets to DATA0
// once this clears it.
const REQUEST_TYPE_HOST_TO_DEVICE_ENDPOINT: u8 = 0x02;
const REQUEST_CLEAR_FEATURE: u8 = 0x01;
const FEATURE_ENDPOINT_HALT: u16 = 0;

fn build_clear_endpoint_halt_setup(endpoint_address: u8) -> [u8; 8] {
    [
        REQUEST_TYPE_HOST_TO_DEVICE_ENDPOINT,
        REQUEST_CLEAR_FEATURE,
        (FEATURE_ENDPOINT_HALT & 0xFF) as u8,
        (FEATURE_ENDPOINT_HALT >> 8) as u8,
        endpoint_address,
        0,
        0,
        0,
    ]
}

// SCSI Primary Commands (SPC) and Block Commands (SBC) used here.
const SCSI_TEST_UNIT_READY: u8 = 0x00;
const SCSI_REQUEST_SENSE: u8 = 0x03;
const SCSI_INQUIRY: u8 = 0x12;
const SCSI_READ_10: u8 = 0x28;
const SCSI_READ_CAPACITY_10: u8 = 0x25;

/// Fixed at 512 bytes, matching `sdmmc.rs`'s `BLOCK_BYTES` -- USB flash
/// drives universally report this as their READ CAPACITY(10) block length,
/// and keeping it fixed here (rather than plumbing the value `read_capacity`
/// returns through every call) lets `read_blocks` match
/// `sdmmc::read_block`/`read_blocks`'s exact signature and contract, which
/// is the point of `USB_MSC_PLAN.md` Stage 5.
const BLOCK_BYTES: usize = 512;

/// Standard INQUIRY data, fixed portion (SPC): enough to reach the Product
/// Revision Level field. Vendor Identification is bytes 8-15, Product
/// Identification is bytes 16-31, Product Revision Level is bytes 32-35
/// (`shell.rs`'s `cmd_usbmsc` reads these fixed offsets directly, the same
/// way `shell.rs`'s `cmd_sdmbr` reads fixed MBR offsets).
const INQUIRY_RESPONSE_LEN: usize = 36;

/// Fixed-format sense data (SPC): only enough to reach the Sense Key
/// (byte 2, low nibble) and the two byte-12/13 ASC/ASCQ fields --
/// `request_sense` does not decode a full sense-code table, just enough to
/// log something more specific than "not ready".
const REQUEST_SENSE_RESPONSE_LEN: usize = 18;

/// READ CAPACITY(10) response (SBC): 4-byte Returned Logical Block Address
/// (the *last* valid LBA, not the block count) plus 4-byte Block Length in
/// Bytes, both big-endian (SCSI data is big-endian; unlike CBW/CSW, which
/// the BOT spec defines as little-endian).
const READ_CAPACITY_10_RESPONSE_LEN: usize = 8;
/// Sentinel value of the Returned Logical Block Address field meaning "this
/// device is larger than READ CAPACITY(10) can express -- use READ
/// CAPACITY(16) instead" (SBC). That command is not implemented here.
const READ_CAPACITY_10_NEEDS_CAPACITY_16: u32 = 0xFFFF_FFFF;

/// Bulk transfers let NAKs retry in hardware until success or a real error,
/// same reasoning as `protocol.rs`'s `CONTROL_TIMEOUT_ITERATIONS`: this is a
/// generous wall-clock bound, not a retry budget of our own.
///
/// 10x `CONTROL_TIMEOUT_ITERATIONS`: a READ(10) data phase means the device
/// actually has to touch flash (wear-leveling lookup, ECC, and on some
/// drives an internal "still settling after enumeration" delay), unlike a
/// control transfer or a status-only Bulk command (INQUIRY/TEST UNIT
/// READY/READ CAPACITY), which are typically served instantly from
/// firmware. Confirmed too small at the smaller value on real hardware: a
/// `usbread` run immediately after enumeration hit a Bulk IN timeout on the
/// READ(10) data phase specifically (CBW send and CSW receive were never
/// the ones timing out).
const BULK_TIMEOUT_ITERATIONS: u32 = 20_000_000;

/// Delay between `wait_until_ready`'s TEST UNIT READY polls. No spec value
/// to anchor this to (unlike, say, SD's `bPwrOn2PwrGood`) -- picked in the
/// same spirit as this project's other polling intervals, short enough not
/// to waste much of the retry budget on any single poll.
const READY_POLL_INTERVAL_MS: u32 = 100;

pub struct UsbMassStorage {
    device_address: u8,
    low_speed_via_hub: bool,
    /// EP0's max packet size, for the `CLEAR_FEATURE(ENDPOINT_HALT)`
    /// control transfer issued on a Bulk STALL -- separate from the Bulk
    /// endpoints' own MPS values.
    control_mps: u16,
    bulk_in_endpoint: u8,
    bulk_in_mps: u16,
    bulk_out_endpoint: u8,
    bulk_out_mps: u16,
    /// Next PID to send on each Bulk endpoint (`true` = DATA1). Unlike a
    /// control transfer's data stage (`protocol.rs`'s `data_stage_in`,
    /// which always starts a fresh transfer at DATA1), a Bulk endpoint's
    /// toggle persists across transfers for as long as the endpoint stays
    /// configured and un-halted, so it has to live here rather than reset
    /// per call.
    in_toggle: bool,
    out_toggle: bool,
    /// `dCBWTag`/`dCSWTag` of the next command; incremented every call so a
    /// stale CSW left over from a previous command cannot be mistaken for
    /// the current one.
    next_tag: u32,
}

impl UsbMassStorage {
    /// Takes a device that `protocol::enumerate_device` has already
    /// addressed and, if it has a Bulk-Only Transport Mass Storage
    /// interface, activates its configuration before returning a handle
    /// ready for `inquiry` and later commands.
    pub fn attach(device: &EnumeratedDevice) -> Option<Self> {
        let msc = find_msc_interface(device.config_bytes())?;
        let pipe = device.control_pipe();

        let setup = protocol::build_standard_out_setup(
            REQUEST_SET_CONFIGURATION,
            device.configuration_value as u16,
            0,
        );
        if !protocol::control_transfer_out_no_data(&pipe, &setup) {
            uart::log(b"USB MSC: SET_CONFIGURATION failed\r\n");
            return None;
        }

        Some(Self {
            device_address: device.device_address,
            low_speed_via_hub: device.low_speed_via_hub,
            control_mps: device.max_packet_size0 as u16,
            bulk_in_endpoint: msc.bulk_in_endpoint,
            bulk_in_mps: msc.bulk_in_mps,
            bulk_out_endpoint: msc.bulk_out_endpoint,
            bulk_out_mps: msc.bulk_out_mps,
            // Every endpoint's data toggle resets to DATA0 on
            // SET_CONFIGURATION (USB2.0 9.4.5), same as `UsbKeyboard::attach`.
            in_toggle: false,
            out_toggle: false,
            next_tag: 0,
        })
    }

    /// SCSI INQUIRY (SPC, opcode 0x12): the simplest command with a data
    /// phase, used here as the first real exercise of the Bulk-Only
    /// Transport framing. Returns the 36-byte standard INQUIRY data if the
    /// device accepted the command and returned a full response.
    pub fn inquiry(&mut self) -> Option<[u8; INQUIRY_RESPONSE_LEN]> {
        let mut data = [0u8; INQUIRY_RESPONSE_LEN];
        let cdb: [u8; 6] = [SCSI_INQUIRY, 0, 0, 0, INQUIRY_RESPONSE_LEN as u8, 0];
        let (transferred, status) = self.execute_command(&cdb, true, &mut data)?;
        if status != CSW_STATUS_PASSED {
            uart::log_hex(b"USB MSC: INQUIRY failed, CSW status=", status as u32);
            return None;
        }
        if transferred < data.len() {
            uart::log(b"USB MSC: short INQUIRY response\r\n");
            return None;
        }
        Some(data)
    }

    /// SCSI TEST UNIT READY (SPC, opcode 0x00): no data phase, just a CSW.
    /// Returns `Some(true)` if the device reports ready (CSW Passed),
    /// `Some(false)` if it reports Failed (typically "not ready", e.g. an
    /// empty card reader slot -- callers can follow up with
    /// `request_sense` for the reason), or `None` if the command itself
    /// could not be completed (Bulk/BOT-level failure).
    pub fn test_unit_ready(&mut self) -> Option<bool> {
        let cdb: [u8; 6] = [SCSI_TEST_UNIT_READY, 0, 0, 0, 0, 0];
        let (_transferred, status) = self.execute_command(&cdb, true, &mut [])?;
        Some(status == CSW_STATUS_PASSED)
    }

    /// Polls TEST UNIT READY up to `attempts` times, sleeping
    /// `READY_POLL_INTERVAL_MS` between tries, until the device reports
    /// ready. Returns `false` either because it never became ready in time
    /// or because a poll hit a real BOT/Bulk failure (`test_unit_ready`
    /// returning `None`), which is not "not ready yet" and not worth
    /// retrying.
    ///
    /// Real USB flash drives are not always immediately ready to service a
    /// READ(10)/WRITE(10) right after `SET_CONFIGURATION` -- firmware can
    /// still be settling internally. Skipping this before the first
    /// READ(10) is what `BULK_TIMEOUT_ITERATIONS`'s doc comment describes:
    /// confirmed on real hardware, a `usbread` run standalone right after
    /// enumeration timed out on the READ(10) data phase, while the same
    /// command reliably succeeded once `usbmsc` -- which exercises
    /// INQUIRY/TEST UNIT READY/READ CAPACITY first -- had just run. This is
    /// the same "poll until ready" step a real BOT class driver performs
    /// before its first READ/WRITE, not a workaround specific to this host
    /// stack.
    pub fn wait_until_ready(&mut self, attempts: u32) -> bool {
        for attempt in 0..attempts.max(1) {
            match self.test_unit_ready() {
                Some(true) => return true,
                Some(false) => {}
                None => return false,
            }
            if attempt + 1 < attempts {
                delay_ms(READY_POLL_INTERVAL_MS);
            }
        }
        false
    }

    /// SCSI REQUEST SENSE (SPC, opcode 0x03): reads the sense data left
    /// behind by the previous command's non-Passed status. Returns the raw
    /// fixed-format sense data; `shell.rs`'s `cmd_usbmsc` reads only the
    /// Sense Key (byte 2, low nibble) out of it.
    pub fn request_sense(&mut self) -> Option<[u8; REQUEST_SENSE_RESPONSE_LEN]> {
        let mut data = [0u8; REQUEST_SENSE_RESPONSE_LEN];
        let cdb: [u8; 6] = [SCSI_REQUEST_SENSE, 0, 0, 0, REQUEST_SENSE_RESPONSE_LEN as u8, 0];
        let (transferred, status) = self.execute_command(&cdb, true, &mut data)?;
        if status != CSW_STATUS_PASSED {
            uart::log_hex(b"USB MSC: REQUEST SENSE failed, CSW status=", status as u32);
            return None;
        }
        if transferred < data.len() {
            uart::log(b"USB MSC: short REQUEST SENSE response\r\n");
            return None;
        }
        Some(data)
    }

    /// SCSI READ CAPACITY(10) (SBC, opcode 0x25): the device's last valid
    /// LBA and its block size. Returns `None` both on a BOT/command-level
    /// failure and when the device signals (via the
    /// `READ_CAPACITY_10_NEEDS_CAPACITY_16` sentinel) that it is larger
    /// than this command can express -- READ CAPACITY(16) is not
    /// implemented, so such a device's capacity cannot be read at all here.
    pub fn read_capacity(&mut self) -> Option<ReadCapacity> {
        let mut data = [0u8; READ_CAPACITY_10_RESPONSE_LEN];
        let cdb: [u8; 10] = [SCSI_READ_CAPACITY_10, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let (transferred, status) = self.execute_command(&cdb, true, &mut data)?;
        if status != CSW_STATUS_PASSED {
            uart::log_hex(b"USB MSC: READ CAPACITY(10) failed, CSW status=", status as u32);
            return None;
        }
        if transferred < data.len() {
            uart::log(b"USB MSC: short READ CAPACITY(10) response\r\n");
            return None;
        }
        let last_lba = u32::from_be_bytes(data[0..4].try_into().unwrap());
        if last_lba == READ_CAPACITY_10_NEEDS_CAPACITY_16 {
            uart::log(b"USB MSC: device needs READ CAPACITY(16), not implemented\r\n");
            return None;
        }
        let block_length = u32::from_be_bytes(data[4..8].try_into().unwrap());
        Some(ReadCapacity { last_lba, block_length })
    }

    /// SCSI READ(10) (SBC, opcode 0x28): reads consecutive 512-byte blocks
    /// starting at `lba` into `buffer`, whose length must be a nonzero
    /// multiple of 512 bytes -- the same contract as
    /// `sdmmc::read_block`/`read_blocks`, which is what lets a future
    /// `BlockDevice` abstraction (`USB_MSC_PLAN.md` Stage 6) dispatch to
    /// either with the same call shape.
    pub fn read_blocks(&mut self, lba: u32, buffer: &mut [u8]) -> bool {
        if buffer.is_empty() || buffer.len() % BLOCK_BYTES != 0 {
            uart::log(b"USB MSC: block transfer length must be a nonzero multiple of 512 bytes\r\n");
            return false;
        }
        let block_count = buffer.len() / BLOCK_BYTES;
        // READ(10)'s transfer-length field is 16 bits; a single shell
        // command's worth of blocks (see `sdmmc.rs`'s own `MAX_MULTI_BLOCKS`
        // cap) never comes close, so this only guards against a genuine
        // misuse of the API.
        if block_count > u16::MAX as usize {
            uart::log(b"USB MSC: too many blocks for one READ(10) transfer\r\n");
            return false;
        }

        let cdb: [u8; 10] = [
            SCSI_READ_10,
            0, // RDPROTECT/DPO/FUA: none requested
            (lba >> 24) as u8,
            (lba >> 16) as u8,
            (lba >> 8) as u8,
            lba as u8,
            0, // group number: none
            (block_count >> 8) as u8,
            block_count as u8,
            0, // control
        ];
        let Some((transferred, status)) = self.execute_command(&cdb, true, buffer) else {
            return false;
        };
        if status != CSW_STATUS_PASSED {
            uart::log_hex(b"USB MSC: READ(10) failed, CSW status=", status as u32);
            return false;
        }
        if transferred < buffer.len() {
            uart::log(b"USB MSC: short READ(10) response\r\n");
            return false;
        }
        true
    }

    /// Runs one full Bulk-Only Transport command: CBW out, optional data
    /// phase, CSW in. `direction_in`/`data` describe the data phase (`data`
    /// empty means no data phase at all, e.g. TEST UNIT READY). Returns the
    /// number of bytes actually moved in the data phase and the CSW status
    /// byte (0 = Passed) -- callers decide what a nonzero status means for
    /// their specific command.
    fn execute_command(&mut self, cdb: &[u8], direction_in: bool, data: &mut [u8]) -> Option<(usize, u8)> {
        self.next_tag = self.next_tag.wrapping_add(1);
        let tag = self.next_tag;
        let flags = if direction_in { CBW_FLAGS_DATA_IN } else { CBW_FLAGS_DATA_OUT };
        let mut cbw = build_cbw(tag, data.len() as u32, flags, cdb);
        if !self.bulk_transfer_out(&mut cbw) {
            uart::log(b"USB MSC: CBW send failed\r\n");
            return None;
        }

        let transferred = if data.is_empty() {
            0
        } else if direction_in {
            self.bulk_transfer_in(data)?
        } else {
            if !self.bulk_transfer_out(data) {
                return None;
            }
            data.len()
        };

        let mut csw = [0u8; CSW_LEN];
        let csw_received = self.bulk_transfer_in(&mut csw)?;
        if csw_received < CSW_LEN {
            uart::log(b"USB MSC: short CSW\r\n");
            return None;
        }
        let status = parse_csw(&csw)?;
        if status.tag != tag {
            uart::log(b"USB MSC: CSW tag mismatch\r\n");
            return None;
        }
        Some((transferred, status.status))
    }

    /// Sends `data` out the Bulk OUT endpoint, splitting into MPS-sized
    /// packets and toggling `out_toggle` after each one succeeds. On a
    /// transaction error (as opposed to a plain NAK timeout), attempts
    /// `CLEAR_FEATURE(ENDPOINT_HALT)` recovery per the BOT spec before
    /// giving up, so the *next* command has a chance of starting clean even
    /// though this one failed.
    fn bulk_transfer_out(&mut self, data: &mut [u8]) -> bool {
        let mps = self.bulk_out_mps.max(1) as usize;
        let mut offset = 0usize;
        while offset < data.len() {
            let chunk_len = (data.len() - offset).min(mps);
            let endpoint = self.out_endpoint();
            let outcome = hcd::run_packet(
                &endpoint,
                false,
                self.out_toggle,
                BULK_TIMEOUT_ITERATIONS,
                false,
                false,
                &mut data[offset..offset + chunk_len],
            );
            match outcome {
                PacketOutcome::Ok(_) => {
                    self.out_toggle = !self.out_toggle;
                    offset += chunk_len;
                }
                PacketOutcome::Timeout => {
                    uart::log(b"USB MSC: bulk OUT timed out\r\n");
                    return false;
                }
                PacketOutcome::Error => {
                    uart::log(b"USB MSC: bulk OUT transaction error\r\n");
                    self.recover_from_stall(self.bulk_out_endpoint);
                    return false;
                }
            }
        }
        true
    }

    /// Reads up to `buffer.len()` bytes from the Bulk IN endpoint, stopping
    /// early on a short packet (USB2.0 8.5.3, same convention as
    /// `protocol.rs`'s `data_stage_in`). Same toggle and STALL-recovery
    /// handling as `bulk_transfer_out`.
    fn bulk_transfer_in(&mut self, buffer: &mut [u8]) -> Option<usize> {
        let mps = self.bulk_in_mps.max(1) as usize;
        let mut received = 0usize;
        while received < buffer.len() {
            let chunk_len = (buffer.len() - received).min(mps);
            let endpoint = self.in_endpoint();
            let outcome = hcd::run_packet(
                &endpoint,
                false,
                self.in_toggle,
                BULK_TIMEOUT_ITERATIONS,
                false,
                false,
                &mut buffer[received..received + chunk_len],
            );
            match outcome {
                PacketOutcome::Ok(n) => {
                    self.in_toggle = !self.in_toggle;
                    received += n;
                    if n < chunk_len {
                        break; // short packet: device has no more data
                    }
                }
                PacketOutcome::Timeout => {
                    uart::log(b"USB MSC: bulk IN timed out\r\n");
                    return None;
                }
                PacketOutcome::Error => {
                    uart::log(b"USB MSC: bulk IN transaction error\r\n");
                    self.recover_from_stall(self.bulk_in_endpoint);
                    return None;
                }
            }
        }
        Some(received)
    }

    /// `CLEAR_FEATURE(ENDPOINT_HALT)` on `endpoint_address`, and -- only if
    /// the device actually acknowledges it -- resets the matching toggle to
    /// DATA0 (USB2.0 9.4.5 guarantees the reset only once this succeeds).
    /// Best-effort: if the control transfer itself fails, the endpoint's
    /// halt/toggle state is left exactly as uncertain as it already was,
    /// and the caller has already reported the original Bulk error.
    fn recover_from_stall(&mut self, endpoint_address: u8) {
        let pipe = self.control_pipe();
        let setup = build_clear_endpoint_halt_setup(endpoint_address);
        if !protocol::control_transfer_out_no_data(&pipe, &setup) {
            uart::log(b"USB MSC: CLEAR_FEATURE(ENDPOINT_HALT) failed\r\n");
            return;
        }
        if endpoint_address == self.bulk_in_endpoint {
            self.in_toggle = false;
        } else if endpoint_address == self.bulk_out_endpoint {
            self.out_toggle = false;
        }
    }

    fn control_pipe(&self) -> ControlPipe {
        ControlPipe {
            device_address: self.device_address,
            mps: self.control_mps,
            low_speed_via_hub: self.low_speed_via_hub,
        }
    }

    fn in_endpoint(&self) -> Endpoint {
        Endpoint {
            device_address: self.device_address,
            endpoint_number: self.bulk_in_endpoint & 0x0F,
            endpoint_type: HCCHAR_EPTYPE_BULK,
            mps: self.bulk_in_mps,
            is_in: true,
            low_speed_via_hub: self.low_speed_via_hub,
        }
    }

    fn out_endpoint(&self) -> Endpoint {
        Endpoint {
            device_address: self.device_address,
            endpoint_number: self.bulk_out_endpoint & 0x0F,
            endpoint_type: HCCHAR_EPTYPE_BULK,
            mps: self.bulk_out_mps,
            is_in: false,
            low_speed_via_hub: self.low_speed_via_hub,
        }
    }
}
