//! USB Mass Storage Bulk-Only Transport (BOT) shared by the SCSI-transparent
//! USB-memory class driver.
//!
//! This layer owns only the BOT envelope: configuration, Bulk endpoint
//! transfers, endpoint data toggles, CBW/CSW framing, and recovery requests.
//! The command set carried in a CDB (SCSI transparent or UFI) belongs to the
//! class driver above it.

use super::hcd::{self, CompletionWait, Endpoint, HCCHAR_EPTYPE_BULK, PacketOutcome, Route};
use super::protocol::{self, ControlPipe, EnumeratedDevice, REQUEST_SET_CONFIGURATION};
use crate::delay::delay_ms;
use crate::startup;
use crate::uart;

pub const INTERFACE_CLASS_MASS_STORAGE: u8 = 0x08;
pub const INTERFACE_PROTOCOL_BULK_ONLY: u8 = 0x50;

const ENDPOINT_TYPE_BULK: u8 = 0x02;
const CBW_LEN: usize = 31;
const CSW_LEN: usize = 13;
const CBW_SIGNATURE: [u8; 4] = *b"USBC";
const CSW_SIGNATURE_U32: u32 = u32::from_le_bytes(*b"USBS");
const CBW_FLAGS_DATA_IN: u8 = 0x80;
const CBW_FLAGS_DATA_OUT: u8 = 0x00;
const BULK_SPLIT_ROUNDS: u32 = 20_000;
const BULK_PACKET_RETRIES: u32 = 20;
const BULK_TIMEOUT_RETRIES: u32 = 4;
const BULK_PACKET_RETRY_DELAY_MS: u32 = 50;
const MAX_BULK_MPS: usize = 512;

const REQUEST_TYPE_HOST_TO_DEVICE_CLASS_INTERFACE: u8 = 0x21;
const REQUEST_TYPE_HOST_TO_DEVICE_ENDPOINT: u8 = 0x02;
const REQUEST_MASS_STORAGE_RESET: u8 = 0xFF;
const REQUEST_CLEAR_FEATURE: u8 = 0x01;
const FEATURE_ENDPOINT_HALT: u16 = 0;

/// The two Bulk endpoints attached to one Mass Storage interface.
#[derive(Clone, Copy)]
pub struct BotInterface {
    pub interface_number: u8,
    pub bulk_in_endpoint: u8,
    pub bulk_in_mps: u16,
    pub bulk_out_endpoint: u8,
    pub bulk_out_mps: u16,
}

/// The successful result of one complete BOT command.
pub struct CommandResult {
    pub transferred: usize,
    pub status: u8,
}

/// Finds a Mass Storage interface for one exact command-set subclass and
/// Bulk-Only Transport, returning its Bulk endpoints.
pub fn find_interface(config: &[u8], subclass: u8) -> Option<BotInterface> {
    let mut offset = 0usize;
    while offset + 2 <= config.len() {
        let length = config[offset] as usize;
        if length < 2 || offset + length > config.len() {
            break;
        }
        if config[offset + 1] == protocol::DESCRIPTOR_TYPE_INTERFACE && length >= 9 {
            let is_target_interface = config[offset + 5] == INTERFACE_CLASS_MASS_STORAGE
                && config[offset + 6] == subclass
                && config[offset + 7] == INTERFACE_PROTOCOL_BULK_ONLY;
            if is_target_interface
                && let Some(interface) =
                    scan_bulk_endpoints(config, offset + length, config[offset + 2])
            {
                return Some(interface);
            }
        }
        offset += length;
    }
    None
}

fn scan_bulk_endpoints(config: &[u8], start: usize, interface_number: u8) -> Option<BotInterface> {
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
            break;
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
    Some(BotInterface {
        interface_number,
        bulk_in_endpoint,
        bulk_in_mps,
        bulk_out_endpoint,
        bulk_out_mps,
    })
}

/// One configured BOT session. Endpoint data-toggle state belongs here and
/// persists across commands until the device is reconfigured or recovered.
pub struct BulkOnlyTransport {
    device_address: u8,
    route: Route,
    control_mps: u16,
    interface: BotInterface,
    in_toggle: bool,
    out_toggle: bool,
    next_tag: u32,
    last_recovery_succeeded: bool,
    /// Commands that have needed BOT Reset Recovery without a successful
    /// command in between. See `execute_command`.
    consecutive_recoveries: u32,
    /// Whether this session has already said it is skipping commands.
    reported_unusable: bool,
    /// This device's BOT session can no longer be trusted.  This is kept
    /// separate from `hcd::bus_unusable`: a device that no longer answers
    /// Reset Recovery does not prove that the shared host controller (and
    /// therefore unrelated HID devices) is broken.
    unusable: bool,
    packet_retries: u32,
}

/// How many recoveries in a row are allowed before the session is declared
/// beyond repair. Two: one recovery that holds is normal after a transient
/// transfer failure, while a second failure immediately after a "successful"
/// recovery means the sequence is not fixing whatever is actually wrong.
const RECOVERY_ATTEMPT_LIMIT: u32 = 2;

impl BulkOnlyTransport {
    pub fn attach(device: &EnumeratedDevice, interface: BotInterface) -> Option<Self> {
        let setup = protocol::build_standard_out_setup(
            REQUEST_SET_CONFIGURATION,
            device.configuration_value as u16,
            0,
        );
        if !protocol::control_transfer_out_no_data(&device.control_pipe(), &setup) {
            uart::log(b"USB BOT: SET_CONFIGURATION failed\r\n");
            return None;
        }

        Some(Self {
            device_address: device.device_address,
            route: device.route,
            control_mps: device.max_packet_size0 as u16,
            interface,
            in_toggle: false,
            out_toggle: false,
            next_tag: 0,
            last_recovery_succeeded: false,
            consecutive_recoveries: 0,
            reported_unusable: false,
            unusable: false,
            packet_retries: 0,
        })
    }

    /// Runs one BOT command: CBW OUT, an optional data phase, then CSW IN.
    /// A nonzero CSW status is left for the class driver to interpret.
    pub fn execute_command(
        &mut self,
        cdb: &[u8],
        direction_in: bool,
        data: &mut [u8],
    ) -> Option<CommandResult> {
        self.last_recovery_succeeded = false;
        if self.unusable || hcd::bus_unusable() {
            // This BOT session, or the controller below it, is already known
            // to be beyond what this layer can repair. Every command
            // attempted in the meantime costs a full transfer timeout plus
            // another failed recovery. Fail immediately instead; the
            // caller's own error path is the same either way.
            if !self.reported_unusable {
                self.reported_unusable = true;
                uart::log(
                    b"USB BOT: session is unusable, skipping commands until re-enumeration\r\n",
                );
            }
            return None;
        }
        let result = self.execute_command_once(cdb, direction_in, data);
        if result.is_some() {
            // A command that got through is the only evidence that the
            // session is healthy, so it is the only thing that clears the
            // count.
            self.consecutive_recoveries = 0;
        }
        if result.is_none() {
            // Once a CBW has been accepted, a transport failure can leave
            // the device waiting in any BOT phase and both endpoint toggles
            // are unknown. Clearing just the endpoint that reported the
            // error is not sufficient. Restore the controller-local state,
            // then perform the BOT Reset Recovery sequence before allowing
            // a later command to use this persistent session.
            hcd::recover_channel_after_packet_failure();
            self.last_recovery_succeeded = self.reset_recovery();
            if self.last_recovery_succeeded {
                uart::log(b"USB BOT: reset recovery complete\r\n");
                // "Complete" only means the reset sequence's control
                // transfers went through. The device can answer those and
                // still not move a single bulk packet, in which case every
                // command fails, recovers, and fails again -- which looks
                // like progress in the log while nothing works. Recovery
                // that does not survive the next command is not recovery.
                self.consecutive_recoveries = self.consecutive_recoveries.saturating_add(1);
                if self.consecutive_recoveries >= RECOVERY_ATTEMPT_LIMIT {
                    uart::log(
                        b"USB BOT: recovery is not holding; this session needs re-enumeration\r\n",
                    );
                    self.unusable = true;
                }
            } else {
                // Recovery talks to the device over its control endpoint,
                // so failing it means the device is not answering there
                // either -- the channel itself halts perfectly well in this
                // case, which is why nothing below this layer can notice.
                // Nothing addressed to this device will work again until
                // the registry rebuilds its session.
                uart::log(b"USB BOT: reset recovery failed; this session needs re-enumeration\r\n");
                self.unusable = true;
            }
        }
        result
    }

    /// Whether the immediately preceding failed command restored the BOT
    /// session to a state in which a command-specific retry is safe.
    pub fn last_recovery_succeeded(&self) -> bool {
        self.last_recovery_succeeded && !self.unusable && !hcd::bus_unusable()
    }

    /// True when this device must be re-enumerated before another BOT
    /// command is attempted. This deliberately says nothing about other
    /// devices on the same bus.
    pub fn needs_reinit(&self) -> bool {
        self.unusable || hcd::bus_unusable()
    }

    /// Re-establishes the BOT command boundary while the device is still
    /// responsive. This is the same class reset and endpoint-toggle reset as
    /// failure recovery, but is invoked between commands rather than after a
    /// transport has already stopped answering.
    pub fn resynchronize(&mut self) -> bool {
        self.last_recovery_succeeded = false;
        if self.unusable || hcd::bus_unusable() {
            return false;
        }
        hcd::recover_channel_after_packet_failure();
        if self.reset_recovery() {
            self.consecutive_recoveries = 0;
            self.reported_unusable = false;
            true
        } else {
            uart::log(b"USB BOT: proactive resynchronization failed\r\n");
            self.unusable = true;
            false
        }
    }

    /// Number of QTD suffixes resubmitted after status 1 or timeout.
    pub fn packet_retry_count(&self) -> u32 {
        self.packet_retries
    }

    pub fn bulk_in_mps(&self) -> u16 {
        self.interface.bulk_in_mps
    }

    fn execute_command_once(
        &mut self,
        cdb: &[u8],
        direction_in: bool,
        data: &mut [u8],
    ) -> Option<CommandResult> {
        self.next_tag = self.next_tag.wrapping_add(1);
        let tag = self.next_tag;
        let flags = if direction_in {
            CBW_FLAGS_DATA_IN
        } else {
            CBW_FLAGS_DATA_OUT
        };
        let mut cbw = build_cbw(tag, data.len() as u32, flags, cdb);
        if !self.bulk_transfer_out(b"CBW", &mut cbw) {
            uart::log(b"USB BOT: CBW send failed\r\n");
            return None;
        }

        let transferred = if data.is_empty() {
            0
        } else if direction_in {
            self.bulk_transfer_in(b"data IN", data)?
        } else {
            if !self.bulk_transfer_out(b"data OUT", data) {
                return None;
            }
            data.len()
        };

        let mut csw = [0u8; CSW_LEN];
        let csw_received = self.bulk_transfer_in(b"CSW", &mut csw)?;
        if csw_received < CSW_LEN {
            uart::log(b"USB BOT: short CSW\r\n");
            return None;
        }
        let status = parse_csw(&csw)?;
        if status.tag != tag {
            uart::log(b"USB BOT: CSW tag mismatch\r\n");
            return None;
        }
        Some(CommandResult {
            transferred,
            status: status.status,
        })
    }

    /// `phase` names which part of the BOT sequence this transfer is, so a
    /// failure log says whether the command block, the data, or the status
    /// wrapper is what did not get through. The three fail for different
    /// reasons and the distinction is the first thing anyone reading the
    /// log needs.
    fn bulk_transfer_out(&mut self, phase: &[u8], data: &mut [u8]) -> bool {
        let mps = self.interface.bulk_out_mps.max(1) as usize;
        let mut offset = 0usize;
        while offset < data.len() {
            let chunk_len = (data.len() - offset).min(mps);
            let endpoint = self.out_endpoint();
            let outcome = self.run_bulk_packet(
                &endpoint,
                self.out_toggle,
                &mut data[offset..offset + chunk_len],
            );
            match outcome {
                PacketOutcome::Ok(_) => {
                    self.out_toggle = !self.out_toggle;
                    offset += chunk_len;
                }
                PacketOutcome::Timeout(_) => {
                    log_phase_failure(b"bulk OUT timed out", phase);
                    return false;
                }
                PacketOutcome::PacketError(_) => {
                    log_phase_failure(b"bulk OUT packet retries exhausted", phase);
                    return false;
                }
                PacketOutcome::Error => {
                    log_phase_failure(b"bulk OUT transaction error", phase);
                    return false;
                }
            }
        }
        true
    }

    fn bulk_transfer_in(&mut self, phase: &[u8], buffer: &mut [u8]) -> Option<usize> {
        let mps = self.interface.bulk_in_mps.max(1) as usize;
        if mps > MAX_BULK_MPS {
            uart::log(b"USB BOT: unsupported Bulk IN MPS\r\n");
            return None;
        }
        let mut staging = BulkInStaging {
            bytes: [0u8; MAX_BULK_MPS],
        };
        let mut received = 0usize;
        while received < buffer.len() {
            let remaining = buffer.len() - received;
            // Keep every descriptor to one USB packet. A 4 KiB QTD is valid
            // according to the DWC descriptor format, but repeated real-device
            // tests eventually leave the target NAKing that descriptor and
            // then EP0 itself. One-packet QTDs make every completion and DATA
            // PID transition explicit in software and bound retry ambiguity
            // to exactly one packet. Short responses still use the aligned
            // MPS-sized staging buffer below.
            let direct_len = if remaining >= mps { mps } else { 0 };
            let endpoint = self.in_endpoint();
            let outcome = if direct_len > 0 {
                self.run_bulk_packet(
                    &endpoint,
                    self.in_toggle,
                    &mut buffer[received..received + direct_len],
                )
            } else {
                // Descriptor-DMA IN lengths must be zero or a multiple of
                // MPS. Request one complete packet into internal-RAM staging
                // for CSW/INQUIRY/capacity and copy the actual short packet;
                // passing a 13-byte CSW buffer directly as a QTD is outside
                // the DWC contract even though it often appears to work.
                self.run_bulk_packet(&endpoint, self.in_toggle, &mut staging.bytes[..mps])
            };
            match outcome {
                PacketOutcome::Ok(n) => {
                    self.advance_in_toggle(n, mps);
                    if direct_len == 0 {
                        if n > remaining {
                            uart::log(b"USB BOT: Bulk IN response exceeds requested length\r\n");
                            return None;
                        }
                        buffer[received..received + n].copy_from_slice(&staging.bytes[..n]);
                        received += n;
                        break;
                    }
                    received += n;
                    if n < direct_len {
                        break;
                    }
                }
                PacketOutcome::Timeout(_) => {
                    log_phase_failure(b"bulk IN timed out", phase);
                    return None;
                }
                PacketOutcome::PacketError(_) => {
                    log_phase_failure(b"bulk IN packet retries exhausted", phase);
                    return None;
                }
                PacketOutcome::Error => {
                    log_phase_failure(b"bulk IN transaction error", phase);
                    return None;
                }
            }
        }
        Some(received)
    }

    fn advance_in_toggle(&mut self, transferred: usize, mps: usize) {
        // A zero-length short response is still one successful USB packet.
        let packets = if transferred == 0 {
            1
        } else {
            (transferred - 1) / mps + 1
        };
        if packets & 1 != 0 {
            self.in_toggle = !self.in_toggle;
        }
    }

    /// Descriptor DMA reports QTD status 1 for a packet-level failure,
    /// including excessive NAK. BOT now calls this with at most one MPS, so
    /// the QTD can safely be replayed with the same DATA PID: a lost ACK can
    /// produce only a duplicate, which the endpoint acknowledges without
    /// consuming twice.
    fn run_bulk_packet(
        &mut self,
        endpoint: &Endpoint,
        pid_data1: bool,
        buffer: &mut [u8],
    ) -> PacketOutcome {
        let mps = endpoint.mps.max(1) as usize;
        if buffer.len() > mps {
            uart::log(b"USB BOT: conservative QTD exceeds one packet\r\n");
            return PacketOutcome::Error;
        }
        let mut packet_error_retries = 0u32;
        let mut timeout_retries = 0u32;
        loop {
            let can_retry_error = packet_error_retries < BULK_PACKET_RETRIES;
            let can_retry_timeout = timeout_retries < BULK_TIMEOUT_RETRIES;
            let outcome = hcd::run_packet(
                endpoint,
                false,
                pid_data1,
                bulk_timeout_iterations(),
                BULK_SPLIT_ROUNDS,
                CompletionWait::Interrupt,
                can_retry_timeout,
                can_retry_error,
                buffer,
            );
            match outcome {
                PacketOutcome::Ok(transferred) => return PacketOutcome::Ok(transferred),
                PacketOutcome::PacketError(_) if can_retry_error => {
                    packet_error_retries += 1;
                    self.packet_retries = self.packet_retries.wrapping_add(1);
                    hcd::recover_channel_after_packet_failure();
                    delay_ms(BULK_PACKET_RETRY_DELAY_MS);
                }
                PacketOutcome::PacketError(transferred) => {
                    return PacketOutcome::PacketError(transferred);
                }
                PacketOutcome::Timeout(_) if can_retry_timeout => {
                    timeout_retries += 1;
                    self.packet_retries = self.packet_retries.wrapping_add(1);
                    hcd::recover_channel_after_packet_failure();
                    delay_ms(BULK_PACKET_RETRY_DELAY_MS);
                }
                PacketOutcome::Timeout(transferred) => return PacketOutcome::Timeout(transferred),
                PacketOutcome::Error => return PacketOutcome::Error,
            }
        }
    }

    /// USB Mass Storage Bulk-Only Transport Reset Recovery:
    /// class-specific Mass Storage Reset, then clear both Bulk endpoint
    /// halts. CLEAR_FEATURE also returns the host's matching data toggle to
    /// DATA0, so a subsequent CBW starts from a synchronized session.
    fn reset_recovery(&mut self) -> bool {
        let reset_setup = [
            REQUEST_TYPE_HOST_TO_DEVICE_CLASS_INTERFACE,
            REQUEST_MASS_STORAGE_RESET,
            0,
            0,
            self.interface.interface_number,
            0,
            0,
            0,
        ];
        let reset_ok = protocol::control_transfer_out_no_data(&self.control_pipe(), &reset_setup);
        if !reset_ok {
            uart::log(b"USB BOT: Mass Storage Reset failed\r\n");
            return false;
        }
        let clear_in_ok = self.clear_endpoint_halt(self.interface.bulk_in_endpoint);
        let clear_out_ok = self.clear_endpoint_halt(self.interface.bulk_out_endpoint);
        clear_in_ok && clear_out_ok
    }

    fn clear_endpoint_halt(&mut self, endpoint_address: u8) -> bool {
        let setup = [
            REQUEST_TYPE_HOST_TO_DEVICE_ENDPOINT,
            REQUEST_CLEAR_FEATURE,
            (FEATURE_ENDPOINT_HALT & 0xFF) as u8,
            (FEATURE_ENDPOINT_HALT >> 8) as u8,
            endpoint_address,
            0,
            0,
            0,
        ];
        if !protocol::control_transfer_out_no_data(&self.control_pipe(), &setup) {
            uart::log(b"USB BOT: CLEAR_FEATURE(ENDPOINT_HALT) failed\r\n");
            return false;
        }
        if endpoint_address == self.interface.bulk_in_endpoint {
            self.in_toggle = false;
        } else if endpoint_address == self.interface.bulk_out_endpoint {
            self.out_toggle = false;
        }
        true
    }

    fn control_pipe(&self) -> ControlPipe {
        ControlPipe {
            device_address: self.device_address,
            mps: self.control_mps,
            route: self.route,
        }
    }

    fn in_endpoint(&self) -> Endpoint {
        Endpoint {
            device_address: self.device_address,
            endpoint_number: self.interface.bulk_in_endpoint & 0x0F,
            endpoint_type: HCCHAR_EPTYPE_BULK,
            mps: self.interface.bulk_in_mps,
            is_in: true,
            route: self.route,
        }
    }

    fn out_endpoint(&self) -> Endpoint {
        Endpoint {
            device_address: self.device_address,
            endpoint_number: self.interface.bulk_out_endpoint & 0x0F,
            endpoint_type: HCCHAR_EPTYPE_BULK,
            mps: self.interface.bulk_out_mps,
            is_in: false,
            route: self.route,
        }
    }
}

/// Scratch for the final short Bulk IN packet. High-Speed Bulk endpoints
/// have an MPS of at most 512 bytes; Full-Speed devices use at most 64.
#[repr(C, align(4))]
struct BulkInStaging {
    bytes: [u8; MAX_BULK_MPS],
}

/// `CompletionWait::Interrupt` interprets one iteration as eight CPU
/// cycles. Keep each QTD attempt near one second. Four timeout retries give
/// flash media about five seconds overall while allowing a frozen channel to
/// be halted and resubmitted without resetting the entire BOT session.
fn bulk_timeout_iterations() -> u32 {
    startup::cpu_hz().saturating_div(8).max(20_000_000)
}

struct CommandStatus {
    tag: u32,
    status: u8,
}

fn build_cbw(tag: u32, data_transfer_length: u32, flags: u8, cdb: &[u8]) -> [u8; CBW_LEN] {
    let cdb_len = cdb.len().min(16);
    let mut cbw = [0u8; CBW_LEN];
    cbw[0..4].copy_from_slice(&CBW_SIGNATURE);
    cbw[4..8].copy_from_slice(&tag.to_le_bytes());
    cbw[8..12].copy_from_slice(&data_transfer_length.to_le_bytes());
    cbw[12] = flags;
    cbw[13] = 0;
    cbw[14] = cdb_len as u8;
    cbw[15..15 + cdb_len].copy_from_slice(&cdb[..cdb_len]);
    cbw
}

fn parse_csw(bytes: &[u8; CSW_LEN]) -> Option<CommandStatus> {
    let signature = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if signature != CSW_SIGNATURE_U32 {
        uart::log_hex(b"USB BOT: bad CSW signature=", signature);
        return None;
    }
    Some(CommandStatus {
        tag: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        status: bytes[12],
    })
}

/// Writes one `USB BOT: <what> during <phase>` line.
fn log_phase_failure(what: &[u8], phase: &[u8]) {
    uart::log(b"USB BOT: ");
    uart::log(what);
    uart::log(b" during ");
    uart::log(phase);
    uart::log(b"\r\n");
}
