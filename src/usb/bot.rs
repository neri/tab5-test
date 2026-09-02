//! USB Mass Storage Bulk-Only Transport (BOT) shared by the SCSI-transparent
//! USB-memory class driver.
//!
//! This layer owns only the BOT envelope: configuration, Bulk endpoint
//! transfers, endpoint data toggles, CBW/CSW framing, and recovery requests.
//! The command set carried in a CDB (SCSI transparent or UFI) belongs to the
//! class driver above it.

use super::hcd::{
    self, CompletionWait, Endpoint, HCCHAR_EPTYPE_BULK, PacketOutcome, Route, TransferProgress,
};
use super::protocol::{self, ControlPipe, EnumeratedDevice, REQUEST_SET_CONFIGURATION};
use crate::delay::delay_ms;
use crate::startup;
use crate::uart;
use tab5_bot_protocol::{self as bot_protocol, DataDirection, TransferSummary, ValidationError};

pub const INTERFACE_CLASS_MASS_STORAGE: u8 = 0x08;
pub const INTERFACE_PROTOCOL_BULK_ONLY: u8 = 0x50;

const ENDPOINT_TYPE_BULK: u8 = 0x02;
const CBW_LEN: usize = 31;
const CSW_LEN: usize = bot_protocol::CSW_LEN;
const CBW_SIGNATURE: [u8; 4] = *b"USBC";
const CBW_FLAGS_DATA_IN: u8 = 0x80;
const CBW_FLAGS_DATA_OUT: u8 = 0x00;
const BULK_SPLIT_ROUNDS: u32 = 20_000;
const BULK_PACKET_RETRIES: u32 = 20;
const BULK_TIMEOUT_RETRIES: u32 = 4;
const BULK_PACKET_RETRY_DELAY_MS: u32 = 50;
/// Recovery time after the class-specific Mass Storage Reset before the
/// first endpoint request. U-Boot uses 150 ms between Reset Recovery steps;
/// Linux waits even longer after the reset request. Some Full-Speed bridges
/// do not answer the immediate CLEAR_FEATURE otherwise.
const BOT_RESET_SETTLE_MS: u32 = 150;
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

/// Stage 0 baseline counters for one BOT session
/// (`docs/USB_BOT_HCD_REFACTOR_PLAN.md`).
///
/// These are per attachment, not per boot: a session that has been rebuilt
/// has genuinely started over, and carrying the old session's numbers into
/// the new one would make an acceptance run look worse than the bus it ran
/// on.
#[derive(Clone, Copy, Default)]
pub struct TransportObservation {
    pub packet_error_retries: u32,
    pub timeout_retries: u32,
    /// Resubmissions refused by the transfer-length contract.
    pub retries_refused: u32,
    /// Of those, the ones whose descriptor reported bytes already moved.
    pub retries_after_progress: u32,
    pub retry_progress_bytes: u32,
    /// Commands that got as far as sending a CBW. A command refused before
    /// that -- by a failed host cleanup, or by an unusable session -- never
    /// reached the device at all, which is the distinction the FIFO-flush
    /// fault injection gates on. A CBW that then failed mid-transfer is
    /// still counted: part of it may have landed.
    pub commands_started: u32,
    /// BOT Reset Recovery sequences started after a real transport failure.
    pub reset_recoveries: u32,
    /// How many of those the device did not answer.
    pub reset_recovery_failures: u32,
    /// Host cleanups abandoned because a FIFO flush did not finish. Every
    /// one of these stopped a packet from being resent, or retired the
    /// session rather than running Reset Recovery through a stuck FIFO.
    pub cleanup_failures: u32,
    /// Status wrappers whose received length was not exactly 13 bytes.
    pub csw_short: u32,
    /// Full-length wrappers whose signature was not `USBS`.
    pub csw_bad_signature: u32,
    /// Valid wrappers carrying another command's tag.
    pub csw_tag_mismatch: u32,
    /// CSWs reporting BOT Phase Error (status 2).
    pub csw_phase_error: u32,
    /// CSWs using an undefined status value.
    pub csw_invalid_status: u32,
    /// CSWs whose residue contradicts the host-observed data phase.
    pub csw_residue_mismatch: u32,
    /// Complete CSWs received in place of the data-IN phase.
    pub csw_early: u32,
}

/// The successful result of one complete BOT command.
pub struct CommandResult {
    pub transferred: usize,
    pub status: u8,
    pub residue: u32,
    pub expected: usize,
}

impl CommandResult {
    /// Whether a fixed-length command transferred and processed every byte.
    /// Variable-length commands such as INQUIRY VPD deliberately inspect
    /// `transferred` and their own response header instead.
    pub fn has_exact_data(&self) -> bool {
        self.transferred == self.expected && self.residue == 0
    }
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
    /// Stage 0 observation counters (`docs/USB_BOT_HCD_REFACTOR_PLAN.md`).
    /// A single "retries" total cannot say whether the transport is losing
    /// packets or the device is simply slow, and the two led to opposite
    /// conclusions about removing the proactive cleanup.
    packet_error_retries: u32,
    timeout_retries: u32,
    /// Resubmissions the retry budget still allowed but the transfer-length
    /// contract refused. Split from the two below because "the descriptor
    /// says bytes moved" and "the descriptor says nothing" are different
    /// findings, and reporting the second as the first made a run print
    /// `progressed+10 bytes=0`, which reads as a contradiction.
    retries_refused: u32,
    /// Of those, the ones whose descriptor did report bytes already moved.
    retries_after_progress: u32,
    retry_progress_bytes: u32,
    commands_started: u32,
    reset_recoveries: u32,
    reset_recovery_failures: u32,
    cleanup_failures: u32,
    csw_short: u32,
    csw_bad_signature: u32,
    csw_tag_mismatch: u32,
    csw_phase_error: u32,
    csw_invalid_status: u32,
    csw_residue_mismatch: u32,
    csw_early: u32,
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
            packet_error_retries: 0,
            timeout_retries: 0,
            retries_refused: 0,
            retries_after_progress: 0,
            retry_progress_bytes: 0,
            commands_started: 0,
            reset_recoveries: 0,
            reset_recovery_failures: 0,
            cleanup_failures: 0,
            csw_short: 0,
            csw_bad_signature: 0,
            csw_tag_mismatch: 0,
            csw_phase_error: 0,
            csw_invalid_status: 0,
            csw_residue_mismatch: 0,
            csw_early: 0,
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
            // count -- and the only thing that lets the session say again
            // that it has started skipping commands. Neither was ever
            // cleared by the proactive boundary cleanup that used to run
            // here: it ran whether or not anything was wrong, so letting it
            // reset the count made a session that recovered, failed,
            // recovered and failed again look like four independent first
            // failures.
            self.consecutive_recoveries = 0;
            self.reported_unusable = false;
        }
        if result.is_none() {
            // The phase says which USB transfer failed, but without the CDB
            // it is impossible to tell a READ/WRITE failure from the
            // no-data commands around it (TEST UNIT READY, cache flush,
            // capacity and identity probes). Failures are rare, so keep the
            // full command context rather than trying to rate-limit it.
            uart::log_hex(
                b"USB BOT: failed command opcode=",
                cdb.first().copied().unwrap_or(0xFF) as u32,
            );
            uart::log_hex(b"USB BOT:   command tag=", self.next_tag);
            uart::log_u32(b"USB BOT:   data bytes=", data.len() as u32);
            uart::log_u32(b"USB BOT:   direction IN=", u32::from(direction_in));
            // Once a CBW has been accepted, a transport failure can leave
            // the device waiting in any BOT phase and both endpoint toggles
            // are unknown. Clearing just the endpoint that reported the
            // error is not sufficient. Restore the controller-local state,
            // then perform the BOT Reset Recovery sequence before allowing
            // a later command to use this persistent session.
            let cleanup = hcd::recover_failed_packet(hcd::FailureScope::Abandoned);
            if let Some(fifo) = cleanup.failed_fifo() {
                // Reset Recovery is a pair of control transfers followed by
                // more bulk traffic, all of it through the FIFO that just
                // refused to empty. Running it would put a fresh SETUP on
                // top of the previous transfer's residue and report whatever
                // came back as a recovered session.
                self.cleanup_failures = self.cleanup_failures.saturating_add(1);
                uart::log(b"USB BOT: host cleanup could not flush the ");
                uart::log(fifo.name().as_bytes());
                uart::log(b" FIFO; this session needs re-enumeration\r\n");
                self.last_recovery_succeeded = false;
                self.unusable = true;
                return result;
            }
            if cleanup.skipped_for_periodic() {
                // Worth saying explicitly on the failure path: this recovery
                // deliberately left the shared FIFOs alone so that a keyboard
                // or mouse on the same controller keeps its session. If the
                // recovery below then does not hold, leftover receive residue
                // is one of the candidates.
                uart::log(b"USB BOT: recovery left the shared FIFOs to the periodic endpoints\r\n");
            }
            self.reset_recoveries = self.reset_recoveries.saturating_add(1);
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
                self.reset_recovery_failures = self.reset_recovery_failures.saturating_add(1);
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

    /// Number of one-packet QTDs resubmitted after status 1 or timeout.
    pub fn packet_retry_count(&self) -> u32 {
        self.packet_retries
    }

    /// The Stage 0 baseline counters for this session
    /// (`docs/USB_BOT_HCD_REFACTOR_PLAN.md`).
    pub fn observation(&self) -> TransportObservation {
        TransportObservation {
            packet_error_retries: self.packet_error_retries,
            timeout_retries: self.timeout_retries,
            retries_refused: self.retries_refused,
            retries_after_progress: self.retries_after_progress,
            retry_progress_bytes: self.retry_progress_bytes,
            commands_started: self.commands_started,
            reset_recoveries: self.reset_recoveries,
            reset_recovery_failures: self.reset_recovery_failures,
            cleanup_failures: self.cleanup_failures,
            csw_short: self.csw_short,
            csw_bad_signature: self.csw_bad_signature,
            csw_tag_mismatch: self.csw_tag_mismatch,
            csw_phase_error: self.csw_phase_error,
            csw_invalid_status: self.csw_invalid_status,
            csw_residue_mismatch: self.csw_residue_mismatch,
            csw_early: self.csw_early,
        }
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
        self.commands_started = self.commands_started.saturating_add(1);
        hcd::set_transfer_label(hcd::TransferLabel::CommandBlock);
        let Some(cbw_sent) = self.bulk_transfer_out(b"CBW", &mut cbw) else {
            uart::log(b"USB BOT: CBW send failed\r\n");
            return None;
        };
        if bot_protocol::validate_exact_out(CBW_LEN, cbw_sent).is_err() {
            uart::log_u32(b"USB BOT: short CBW, actual=", cbw_sent as u32);
            return None;
        }

        let expected = data.len();
        let direction = if data.is_empty() {
            DataDirection::None
        } else if direction_in {
            DataDirection::In
        } else {
            DataDirection::Out
        };
        let mut early_csw = None;
        let transferred = if data.is_empty() {
            0usize
        } else if direction_in {
            hcd::set_transfer_label(hcd::TransferLabel::DataIn);
            match self.bulk_transfer_in(b"data IN", data, true)? {
                BulkInResult::Data(received) => received,
                BulkInResult::EarlyCsw { csw, transferred } => {
                    self.csw_early = self.csw_early.saturating_add(1);
                    uart::log(b"USB BOT: CSW arrived during data IN\r\n");
                    early_csw = Some(csw);
                    transferred
                }
            }
        } else {
            hcd::set_transfer_label(hcd::TransferLabel::DataOut);
            let sent = self.bulk_transfer_out(b"data OUT", data)?;
            if bot_protocol::validate_exact_out(data.len(), sent).is_err() {
                uart::log_u32(b"USB BOT: short data OUT, actual=", sent as u32);
                return None;
            }
            sent
        };

        let mut csw_packet = BulkInStaging {
            bytes: [0u8; MAX_BULK_MPS],
        };
        let csw_received = if let Some(csw) = early_csw {
            csw_packet.bytes[..CSW_LEN].copy_from_slice(&csw);
            CSW_LEN
        } else {
            let mps = self.interface.bulk_in_mps.max(1) as usize;
            if mps > MAX_BULK_MPS {
                uart::log(b"USB BOT: unsupported Bulk IN MPS\r\n");
                return None;
            }
            hcd::set_transfer_label(hcd::TransferLabel::CommandStatus);
            match self.bulk_transfer_in(b"CSW", &mut csw_packet.bytes[..mps], false)? {
                BulkInResult::Data(received) => received,
                BulkInResult::EarlyCsw { .. } => unreachable!(),
            }
        };
        let transfer = TransferSummary {
            direction,
            expected,
            host_actual: transferred,
        };
        let status =
            match bot_protocol::validate_csw(&csw_packet.bytes[..csw_received], tag, transfer) {
                Ok(status) => status,
                Err(error) => {
                    self.note_csw_validation_error(error);
                    log_csw(&csw_packet.bytes, csw_received, tag);
                    return None;
                }
            };
        Some(CommandResult {
            transferred,
            status: status.status.as_u8(),
            residue: status.residue,
            expected,
        })
    }

    fn note_csw_validation_error(&mut self, error: ValidationError) {
        match error {
            ValidationError::Length { received } => {
                self.csw_short = self.csw_short.saturating_add(1);
                uart::log_u32(b"USB BOT: invalid CSW length=", received as u32);
            }
            ValidationError::BadSignature { received } => {
                self.csw_bad_signature = self.csw_bad_signature.saturating_add(1);
                uart::log_hex(b"USB BOT: bad CSW signature=", received);
            }
            ValidationError::TagMismatch { expected, received } => {
                self.csw_tag_mismatch = self.csw_tag_mismatch.saturating_add(1);
                uart::log(b"USB BOT: CSW tag mismatch\r\n");
                uart::log_hex(b"USB BOT:   validator expected tag=", expected);
                uart::log_hex(b"USB BOT:   validator received tag=", received);
            }
            ValidationError::PhaseError => {
                self.csw_phase_error = self.csw_phase_error.saturating_add(1);
                uart::log(b"USB BOT: CSW reports Phase Error\r\n");
            }
            ValidationError::InvalidStatus { received } => {
                self.csw_invalid_status = self.csw_invalid_status.saturating_add(1);
                uart::log_hex(b"USB BOT: invalid CSW status=", received as u32);
            }
            ValidationError::ResidueExceedsExpected { .. }
            | ValidationError::HostActualExceedsExpected { .. }
            | ValidationError::InLengthMismatch { .. }
            | ValidationError::OutLengthMismatch { .. } => {
                self.csw_residue_mismatch = self.csw_residue_mismatch.saturating_add(1);
                uart::log(b"USB BOT: CSW residue contradicts host transfer length\r\n");
            }
        }
    }

    /// `phase` names which part of the BOT sequence this transfer is, so a
    /// failure log says whether the command block, the data, or the status
    /// wrapper is what did not get through. The three fail for different
    /// reasons and the distinction is the first thing anyone reading the
    /// log needs.
    fn bulk_transfer_out(&mut self, phase: &[u8], data: &mut [u8]) -> Option<usize> {
        let mps = self.interface.bulk_out_mps.max(1) as usize;
        let mut offset = 0usize;
        while offset < data.len() {
            let chunk_len = (data.len() - offset).min(mps);
            let endpoint = self.out_endpoint();
            let outcome = self.run_bulk_packet(
                phase,
                &endpoint,
                self.out_toggle,
                &mut data[offset..offset + chunk_len],
            );
            match outcome {
                PacketOutcome::Ok(sent) => {
                    // The HCD refuses a short OUT before it gets here, so
                    // this is the whole chunk. Advancing by what actually
                    // moved rather than by what was asked for keeps that a
                    // property of the code instead of a comment.
                    debug_assert_eq!(sent, chunk_len);
                    self.advance_out_toggle(sent, mps);
                    offset += sent;
                }
                PacketOutcome::Timeout(_) => {
                    log_phase_failure(b"bulk OUT timed out", phase);
                    return None;
                }
                PacketOutcome::PacketError(_) => {
                    log_phase_failure(b"bulk OUT packet retries exhausted", phase);
                    return None;
                }
                PacketOutcome::CacheSyncFailed => {
                    log_phase_failure(b"bulk OUT DMA cache sync refused", phase);
                    return None;
                }
                PacketOutcome::Error => {
                    log_phase_failure(b"bulk OUT transaction error", phase);
                    return None;
                }
            }
        }
        Some(offset)
    }

    fn bulk_transfer_in(
        &mut self,
        phase: &[u8],
        buffer: &mut [u8],
        detect_early_csw: bool,
    ) -> Option<BulkInResult> {
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
            let endpoint = self.in_endpoint();
            // Descriptor-DMA IN lengths must be zero or a multiple of MPS.
            // Always receive one complete packet into staging. Besides
            // satisfying that hardware rule, delaying publication until the
            // bytes have been classified prevents an early or stale CSW from
            // becoming command payload.
            let outcome =
                self.run_bulk_packet(phase, &endpoint, self.in_toggle, &mut staging.bytes[..mps]);
            match outcome {
                PacketOutcome::Ok(n) => {
                    self.advance_in_toggle(n, mps);
                    if detect_early_csw && bot_protocol::looks_like_csw(&staging.bytes[..n]) {
                        let mut csw = [0u8; CSW_LEN];
                        csw.copy_from_slice(&staging.bytes[..CSW_LEN]);
                        return Some(BulkInResult::EarlyCsw {
                            csw,
                            transferred: received,
                        });
                    }
                    if n > remaining {
                        uart::log(b"USB BOT: Bulk IN response exceeds requested length\r\n");
                        uart::log_u32(b"USB BOT:   requested bytes=", remaining as u32);
                        uart::log_u32(b"USB BOT:   received bytes=", n as u32);
                        let (hcint, qtd_control) = hcd::last_channel0_reap();
                        uart::log_hex(b"USB BOT:   reap HCINT=", hcint);
                        uart::log_hex(b"USB BOT:   reap QTD control=", qtd_control);
                        // READ CAPACITY(10) asks for 8 bytes while a CSW
                        // is 13 bytes and starts with "USBS". Seeing
                        // 0x53425355 here proves that the device and host
                        // disagree about the current BOT phase; words 1
                        // through 3 then expose the CSW tag, residue and
                        // status. A capacity-shaped first 8 bytes followed
                        // by zeroes points instead at HCD byte accounting.
                        // Four bounded lines are enough for either case
                        // and cannot turn a malformed response into a
                        // large UART dump.
                        uart::log_hex(
                            b"USB BOT:   bytes[0..3] LE=",
                            staging_word(&staging.bytes, n, 0),
                        );
                        uart::log_hex(
                            b"USB BOT:   bytes[4..7] LE=",
                            staging_word(&staging.bytes, n, 4),
                        );
                        uart::log_hex(
                            b"USB BOT:   bytes[8..11] LE=",
                            staging_word(&staging.bytes, n, 8),
                        );
                        uart::log_hex(
                            b"USB BOT:   bytes[12..15] LE=",
                            staging_word(&staging.bytes, n, 12),
                        );
                        return None;
                    }
                    buffer[received..received + n].copy_from_slice(&staging.bytes[..n]);
                    received += n;
                    if n < mps {
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
                PacketOutcome::CacheSyncFailed => {
                    log_phase_failure(b"bulk IN DMA cache sync refused", phase);
                    return None;
                }
                PacketOutcome::Error => {
                    log_phase_failure(b"bulk IN transaction error", phase);
                    return None;
                }
            }
        }
        Some(BulkInResult::Data(received))
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

    fn advance_out_toggle(&mut self, transferred: usize, mps: usize) {
        debug_assert_ne!(transferred, 0);
        let packets = transferred.div_ceil(mps);
        if packets & 1 != 0 {
            self.out_toggle = !self.out_toggle;
        }
    }

    /// Descriptor DMA reports QTD status 1 for a packet-level failure,
    /// including excessive NAK. BOT now calls this with at most one MPS, so
    /// the QTD can safely be replayed with the same DATA PID: a lost ACK can
    /// produce only a duplicate, which the endpoint acknowledges without
    /// consuming twice.
    fn run_bulk_packet(
        &mut self,
        phase: &[u8],
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
                // A packet the core *reported* as failed is safe to resend
                // whatever its descriptor says about byte counts, and the
                // descriptor frequently says something impossible: this core
                // returned a remainder of 100,489 for a 64-byte OUT and 128
                // for a 31-byte command block. It does not matter. A USB
                // packet is atomic -- the device accepts it whole or not at
                // all -- and a QTD here carries exactly one packet, so a
                // reported failure means the device did not take it, or took
                // it and the handshake was lost. Resending with the same
                // DATA PID covers both: the endpoint discards a duplicate
                // whose toggle it has already advanced past. Ten write
                // rounds out of ten verified this by reading the medium back
                // with Force Unit Access.
                PacketOutcome::PacketError(progress) if can_retry_error => {
                    packet_error_retries += 1;
                    self.packet_retries = self.packet_retries.wrapping_add(1);
                    self.packet_error_retries = self.packet_error_retries.saturating_add(1);
                    if packet_error_retries == 1 {
                        log_packet_retry(
                            b"packet error",
                            phase,
                            endpoint,
                            pid_data1,
                            packet_error_retries,
                        );
                    }
                    // The Full-Speed path needs the controller/FIFO cleanup
                    // even though the channel halted and the QTD was reaped.
                    // Removing it made the same status-1 QTD repeat until all
                    // 20 retries were exhausted (Stage 3, real-device round
                    // 2). Keep the same DATA PID across the cleanup.
                    let cleanup =
                        hcd::recover_failed_packet(hcd::FailureScope::ReportedPacketError {
                            is_in: endpoint.is_in,
                        });
                    if self.note_failed_cleanup(cleanup) {
                        return PacketOutcome::PacketError(progress);
                    }
                    delay_ms(BULK_PACKET_RETRY_DELAY_MS);
                }
                PacketOutcome::PacketError(progress) => {
                    return PacketOutcome::PacketError(progress);
                }
                PacketOutcome::Timeout(progress)
                    if can_retry_timeout && progress.safe_to_retry() =>
                {
                    timeout_retries += 1;
                    self.packet_retries = self.packet_retries.wrapping_add(1);
                    self.timeout_retries = self.timeout_retries.saturating_add(1);
                    if timeout_retries == 1 {
                        log_packet_retry(b"timeout", phase, endpoint, pid_data1, timeout_retries);
                    }
                    let cleanup = hcd::recover_failed_packet(hcd::FailureScope::Abandoned);
                    if self.note_failed_cleanup(cleanup) {
                        return PacketOutcome::Timeout(progress);
                    }
                    delay_ms(BULK_PACKET_RETRY_DELAY_MS);
                }
                PacketOutcome::Timeout(progress) => {
                    self.note_unsafe_retry(
                        b"timeout",
                        phase,
                        endpoint,
                        progress,
                        can_retry_timeout,
                    );
                    return PacketOutcome::Timeout(progress);
                }
                // Not retried: the same staging buffer would be refused
                // again, and each attempt costs a full transfer timeout.
                PacketOutcome::CacheSyncFailed => return PacketOutcome::CacheSyncFailed,
                PacketOutcome::Error => return PacketOutcome::Error,
            }
        }
    }

    /// Records a cleanup that could not finish, returning whether the
    /// caller must stop.
    ///
    /// A retry after a failed cleanup is not a retry: the resent packet
    /// would go out through a FIFO still holding the bytes of the one that
    /// just failed. Stopping here hands the original outcome back to
    /// `execute_command`, whose own cleanup then fails the same way and
    /// retires the session.
    fn note_failed_cleanup(&mut self, cleanup: hcd::CleanupOutcome) -> bool {
        let Some(fifo) = cleanup.failed_fifo() else {
            return false;
        };
        self.cleanup_failures = self.cleanup_failures.saturating_add(1);
        uart::log(b"USB BOT: packet cleanup could not flush the ");
        uart::log(fifo.name().as_bytes());
        uart::log(b" FIFO; not resending this packet\r\n");
        true
    }

    /// Records an **abandoned** packet the budget would still have allowed
    /// to be retried, but whose bytes cannot be accounted for.
    ///
    /// Reported packet errors do not come here: see the retry arms above.
    ///
    /// This is the case Stage 2 of `docs/USB_BOT_HCD_REFACTOR_PLAN.md`
    /// exists to stop. Before it, a 64-byte OUT that reported
    /// `requested=64 actual=64` on a timeout was resubmitted four times,
    /// putting the same bytes on the bus again each time. The command now
    /// fails instead, and the count is what says how often that shape
    /// occurs on a given bus.
    fn note_unsafe_retry(
        &mut self,
        reason: &[u8],
        phase: &[u8],
        endpoint: &Endpoint,
        progress: TransferProgress,
        budget_remained: bool,
    ) {
        if !budget_remained {
            return;
        }
        self.retries_refused = self.retries_refused.saturating_add(1);
        if progress.count() > 0 {
            self.retries_after_progress = self.retries_after_progress.saturating_add(1);
            self.retry_progress_bytes = self
                .retry_progress_bytes
                .saturating_add(progress.count() as u32);
        }
        uart::log(b"USB BOT: refusing to resubmit after ");
        uart::log(reason);
        uart::log(b" during ");
        uart::log(phase);
        uart::log(b"\r\n");
        uart::log_u32(b"USB BOT:   direction IN=", u32::from(endpoint.is_in));
        if progress.is_known() {
            uart::log_u32(
                b"USB BOT:   bytes already transferred=",
                progress.count() as u32,
            );
        } else {
            uart::log(b"USB BOT:   bytes already transferred are UNKNOWN\r\n");
        }
        // The descriptor this decision was based on. A refusal that cannot
        // be traced back to the word that produced it is a refusal nobody
        // can tell apart from a bug in the rule -- which is exactly what the
        // first version of that rule turned out to be.
        let (hcint, qtd_control) = hcd::last_channel0_reap();
        uart::log_hex(b"USB BOT:   reap HCINT=", hcint);
        uart::log_hex(b"USB BOT:   reap QTD control=", qtd_control);
        uart::log(b"USB BOT:   resending would risk putting them on the bus twice\r\n");
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
        delay_ms(BOT_RESET_SETTLE_MS);
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
///
/// This is **not** a DMA buffer and needs no cache-line alignment: since
/// Stage 1 of `docs/USB_BOT_HCD_REFACTOR_PLAN.md` the HCD stages every
/// packet in a buffer it owns and aligns, and copies the received bytes out
/// to whatever slice this layer passes. What this buffer is still for is
/// the descriptor-DMA rule that an IN transfer size must be zero or a whole
/// multiple of MPS: a 13-byte status wrapper has to be *asked for* as one
/// full packet, which is a length decision, not an alignment one.
#[repr(C, align(4))]
struct BulkInStaging {
    bytes: [u8; MAX_BULK_MPS],
}

enum BulkInResult {
    Data(usize),
    EarlyCsw {
        csw: [u8; CSW_LEN],
        transferred: usize,
    },
}

/// Returns one diagnostic word from a received staging packet. Bytes beyond
/// the HCD-reported length stay zero so a short CSW remains easy to recognize.
fn staging_word(bytes: &[u8; MAX_BULK_MPS], received: usize, offset: usize) -> u32 {
    let mut word = [0u8; 4];
    if offset < received {
        let count = (received - offset).min(word.len());
        word[..count].copy_from_slice(&bytes[offset..offset + count]);
    }
    u32::from_le_bytes(word)
}

/// `CompletionWait::Interrupt` interprets one iteration as eight CPU
/// cycles. Keep each QTD attempt near one second. Four timeout retries give
/// flash media about five seconds overall while allowing a frozen channel to
/// be halted and resubmitted without resetting the entire BOT session.
fn bulk_timeout_iterations() -> u32 {
    startup::cpu_hz().saturating_div(8).max(20_000_000)
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

/// Dumps a status wrapper that did not match its command.
///
/// A tag mismatch used to be reported by its decoded fields alone, which
/// cannot distinguish a wrapper this host built a wrong expectation for
/// from thirteen bytes that were never a wrapper at all. The whole response
/// goes out as four little-endian words, zero-filled past what was actually
/// received, so `0x53425355` in word 0 stays recognizable as `USBS` and a
/// data-phase payload stays recognizable as not being one.
fn log_csw(bytes: &[u8], received: usize, expected_tag: u32) {
    uart::log_u32(b"USB BOT:   CSW bytes received=", received as u32);
    uart::log_hex(b"USB BOT:   expected tag=", expected_tag);
    uart::log_hex(b"USB BOT:   signature=", csw_word(bytes, received, 0));
    uart::log_hex(b"USB BOT:   tag=", csw_word(bytes, received, 4));
    uart::log_u32(b"USB BOT:   residue=", csw_word(bytes, received, 8));
    uart::log_hex(b"USB BOT:   status=", csw_word(bytes, received, 12) & 0xFF);
}

/// One little-endian word of a received status wrapper. Bytes the HCD did
/// not report as received stay zero, so a short response is not padded with
/// whatever the previous command left in the buffer.
fn csw_word(bytes: &[u8], received: usize, offset: usize) -> u32 {
    let mut word = [0u8; 4];
    let end = received.min(bytes.len()).min(CSW_LEN);
    if offset < end {
        let count = (end - offset).min(word.len());
        word[..count].copy_from_slice(&bytes[offset..offset + count]);
    }
    u32::from_le_bytes(word)
}

/// Writes one `USB BOT: <what> during <phase>` line.
fn log_phase_failure(what: &[u8], phase: &[u8]) {
    uart::log(b"USB BOT: ");
    uart::log(what);
    uart::log(b" during ");
    uart::log(phase);
    uart::log(b"\r\n");
}

/// Describes only the first retry of a troubled packet.
///
/// The byte count is deliberately absent: a reported packet error is resent
/// regardless of it, and this core's descriptor for one is often impossible
/// anyway. The raw `HCINT` and descriptor are printed instead, so the retry
/// can be judged from what the hardware actually said.
fn log_packet_retry(
    reason: &[u8],
    phase: &[u8],
    endpoint: &Endpoint,
    pid_data1: bool,
    attempt: u32,
) {
    uart::log(b"USB BOT: retrying bulk packet after ");
    uart::log(reason);
    uart::log(b" during ");
    uart::log(phase);
    uart::log(b"\r\n");
    uart::log_u32(b"USB BOT:   direction IN=", u32::from(endpoint.is_in));
    uart::log_u32(b"USB BOT:   DATA1=", u32::from(pid_data1));
    let (hcint, qtd_control) = hcd::last_channel0_reap();
    uart::log_hex(b"USB BOT:   reap HCINT=", hcint);
    uart::log_hex(b"USB BOT:   reap QTD control=", qtd_control);
    let (hcchar, hctsiz, hcdma) = hcd::channel0_diagnostic_registers();
    uart::log_hex(b"USB BOT:   channel HCCHAR=", hcchar);
    uart::log_hex(b"USB BOT:   channel HCTSIZ=", hctsiz);
    uart::log_hex(b"USB BOT:   channel HCDMA=", hcdma);
    uart::log_u32(b"USB BOT:   retry attempt=", attempt);
}
