//! USB Mass Storage Bulk-Only Transport (BOT) shared by the SCSI-transparent
//! USB-memory class driver.
//!
//! This layer owns only the BOT envelope: configuration, Bulk endpoint
//! transfers, endpoint data toggles, CBW/CSW framing, and recovery requests.
//! The command set carried in a CDB (SCSI transparent or UFI) belongs to the
//! class driver above it.

use super::hcd::{self, Endpoint, HCCHAR_EPTYPE_BULK, PacketOutcome, Route};
use super::protocol::{self, ControlPipe, EnumeratedDevice, REQUEST_SET_CONFIGURATION};
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
const BULK_TIMEOUT_ITERATIONS: u32 = 20_000_000;
const BULK_SPLIT_ROUNDS: u32 = 20_000;

const REQUEST_TYPE_HOST_TO_DEVICE_ENDPOINT: u8 = 0x02;
const REQUEST_CLEAR_FEATURE: u8 = 0x01;
const FEATURE_ENDPOINT_HALT: u16 = 0;

/// The two Bulk endpoints attached to one Mass Storage interface.
#[derive(Clone, Copy)]
pub struct BotInterface {
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
                && let Some(interface) = scan_bulk_endpoints(config, offset + length)
            {
                return Some(interface);
            }
        }
        offset += length;
    }
    None
}

fn scan_bulk_endpoints(config: &[u8], start: usize) -> Option<BotInterface> {
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
}

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
        self.next_tag = self.next_tag.wrapping_add(1);
        let tag = self.next_tag;
        let flags = if direction_in {
            CBW_FLAGS_DATA_IN
        } else {
            CBW_FLAGS_DATA_OUT
        };
        let mut cbw = build_cbw(tag, data.len() as u32, flags, cdb);
        if !self.bulk_transfer_out(&mut cbw) {
            uart::log(b"USB BOT: CBW send failed\r\n");
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

    fn bulk_transfer_out(&mut self, data: &mut [u8]) -> bool {
        let mps = self.interface.bulk_out_mps.max(1) as usize;
        let mut offset = 0usize;
        while offset < data.len() {
            let chunk_len = (data.len() - offset).min(mps);
            let endpoint = self.out_endpoint();
            let outcome = hcd::run_packet(
                &endpoint,
                false,
                self.out_toggle,
                BULK_TIMEOUT_ITERATIONS,
                BULK_SPLIT_ROUNDS,
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
                    uart::log(b"USB BOT: bulk OUT timed out\r\n");
                    return false;
                }
                PacketOutcome::Error => {
                    uart::log(b"USB BOT: bulk OUT transaction error\r\n");
                    self.clear_endpoint_halt(self.interface.bulk_out_endpoint);
                    return false;
                }
            }
        }
        true
    }

    fn bulk_transfer_in(&mut self, buffer: &mut [u8]) -> Option<usize> {
        let mps = self.interface.bulk_in_mps.max(1) as usize;
        let mut received = 0usize;
        while received < buffer.len() {
            let chunk_len = (buffer.len() - received).min(mps);
            let endpoint = self.in_endpoint();
            let outcome = hcd::run_packet(
                &endpoint,
                false,
                self.in_toggle,
                BULK_TIMEOUT_ITERATIONS,
                BULK_SPLIT_ROUNDS,
                false,
                false,
                &mut buffer[received..received + chunk_len],
            );
            match outcome {
                PacketOutcome::Ok(n) => {
                    self.in_toggle = !self.in_toggle;
                    received += n;
                    if n < chunk_len {
                        break;
                    }
                }
                PacketOutcome::Timeout => {
                    uart::log(b"USB BOT: bulk IN timed out\r\n");
                    return None;
                }
                PacketOutcome::Error => {
                    uart::log(b"USB BOT: bulk IN transaction error\r\n");
                    self.clear_endpoint_halt(self.interface.bulk_in_endpoint);
                    return None;
                }
            }
        }
        Some(received)
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
