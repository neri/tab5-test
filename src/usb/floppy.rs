//! USB Floppy driver for UFI over Control/Bulk/Interrupt (CBI).

use super::hcd::{self, CompletionWait, Endpoint, PacketOutcome, HCCHAR_EPTYPE_BULK};
use super::protocol::{self, ControlPipe, EnumeratedDevice, REQUEST_SET_CONFIGURATION};
use crate::uart;

const INTERFACE_CLASS_MASS_STORAGE: u8 = 0x08;
const INTERFACE_SUBCLASS_UFI: u8 = 0x04;
const INTERFACE_PROTOCOL_CBI: u8 = 0x00;
const ENDPOINT_TYPE_BULK: u8 = 0x02;
const ENDPOINT_TYPE_INTERRUPT: u8 = 0x03;
const REQUEST_TYPE_HOST_TO_DEVICE_CLASS_INTERFACE: u8 = 0x21;
const REQUEST_CBI_ADSC: u8 = 0x00;
const SCSI_TEST_UNIT_READY: u8 = 0x00;
const SCSI_REQUEST_SENSE: u8 = 0x03;
const SCSI_READ_CAPACITY_10: u8 = 0x25;
const SCSI_READ_10: u8 = 0x28;
const BLOCK_BYTES: usize = 512;
const UFI_CDB_BYTES: usize = 12;
const CBI_STATUS_BYTES: usize = 2;
const BULK_TIMEOUT_ITERATIONS: u32 = 20_000_000;
const BULK_SPLIT_ROUNDS: u32 = 20_000;

#[derive(Clone, Copy)]
pub struct FloppyInterface {
    pub interface_number: u8,
    pub bulk_in_endpoint: u8,
    pub bulk_in_mps: u16,
    pub bulk_out_endpoint: u8,
    pub bulk_out_mps: u16,
    pub status_in_endpoint: u8,
    pub status_in_mps: u16,
}

pub enum MediaProbe {
    Ready1440Fat12,
    NotReady { asc: u8, ascq: u8 },
    UnsupportedCapacity { last_lba: u32, block_length: u32 },
    InvalidBootSector,
    TransportError,
}

pub fn find_ufi_interface(config: &[u8]) -> Option<FloppyInterface> {
    let mut offset = 0usize;
    while offset + 2 <= config.len() {
        let length = config[offset] as usize;
        if length < 2 || offset + length > config.len() {
            break;
        }
        let target = config[offset + 1] == protocol::DESCRIPTOR_TYPE_INTERFACE
            && length >= 9
            && config[offset + 5] == INTERFACE_CLASS_MASS_STORAGE
            && config[offset + 6] == INTERFACE_SUBCLASS_UFI
            && config[offset + 7] == INTERFACE_PROTOCOL_CBI;
        if target
            && let Some(interface) = scan_endpoints(config, offset + length, config[offset + 2])
        {
            return Some(interface);
        }
        offset += length;
    }
    None
}

fn scan_endpoints(config: &[u8], start: usize, interface_number: u8) -> Option<FloppyInterface> {
    let mut bulk_in: Option<(u8, u16)> = None;
    let mut bulk_out: Option<(u8, u16)> = None;
    let mut status_in: Option<(u8, u16)> = None;
    let mut offset = start;
    while offset + 2 <= config.len() {
        let length = config[offset] as usize;
        if length < 2 || offset + length > config.len() {
            break;
        }
        if config[offset + 1] == protocol::DESCRIPTOR_TYPE_INTERFACE {
            break;
        }
        if config[offset + 1] == protocol::DESCRIPTOR_TYPE_ENDPOINT && length >= 7 {
            let address = config[offset + 2];
            let kind = config[offset + 3] & 0x03;
            let mps = u16::from_le_bytes([config[offset + 4], config[offset + 5]]);
            match (kind, address & 0x80 != 0) {
                (ENDPOINT_TYPE_BULK, true) => {
                    bulk_in.get_or_insert((address, mps));
                }
                (ENDPOINT_TYPE_BULK, false) => {
                    bulk_out.get_or_insert((address, mps));
                }
                (ENDPOINT_TYPE_INTERRUPT, true) => {
                    status_in.get_or_insert((address, mps));
                }
                _ => {}
            }
        }
        offset += length;
    }
    Some(FloppyInterface {
        interface_number,
        bulk_in_endpoint: bulk_in?.0,
        bulk_in_mps: bulk_in?.1,
        bulk_out_endpoint: bulk_out?.0,
        bulk_out_mps: bulk_out?.1,
        status_in_endpoint: status_in?.0,
        status_in_mps: status_in?.1,
    })
}

pub struct UsbFloppy {
    pipe: ControlPipe,
    interface: FloppyInterface,
    bulk_in_toggle: bool,
    status_in_toggle: bool,
}

impl UsbFloppy {
    pub fn attach(device: &EnumeratedDevice) -> Option<Self> {
        let interface = find_ufi_interface(device.config_bytes())?;
        let setup = protocol::build_standard_out_setup(
            REQUEST_SET_CONFIGURATION,
            device.configuration_value as u16,
            0,
        );
        if !protocol::control_transfer_out_no_data(&device.control_pipe(), &setup) {
            uart::log(b"USB Floppy: SET_CONFIGURATION failed\r\n");
            return None;
        }
        Some(Self {
            pipe: device.control_pipe(),
            interface,
            bulk_in_toggle: false,
            status_in_toggle: false,
        })
    }

    pub fn interface(&self) -> FloppyInterface {
        self.interface
    }

    pub fn probe_media(&mut self) -> MediaProbe {
        let Some(status) = self.command(&cdb(SCSI_TEST_UNIT_READY), &mut []) else {
            return MediaProbe::TransportError;
        };
        if status[0] != 0 {
            let (asc, ascq) = self.request_sense().unwrap_or((status[0], status[1]));
            return MediaProbe::NotReady { asc, ascq };
        }

        let mut capacity = [0u8; 8];
        let Some(status) = self.command(&cdb(SCSI_READ_CAPACITY_10), &mut capacity) else {
            return MediaProbe::TransportError;
        };
        if status[0] != 0 {
            return MediaProbe::NotReady {
                asc: status[0],
                ascq: status[1],
            };
        }
        let last_lba = u32::from_be_bytes(capacity[0..4].try_into().unwrap());
        let block_length = u32::from_be_bytes(capacity[4..8].try_into().unwrap());
        if last_lba != 2879 || block_length != BLOCK_BYTES as u32 {
            return MediaProbe::UnsupportedCapacity {
                last_lba,
                block_length,
            };
        }
        let mut boot_sector = [0u8; BLOCK_BYTES];
        let Some(status) = self.command(&read_10_cdb(0, 1), &mut boot_sector) else {
            return MediaProbe::TransportError;
        };
        if status[0] != 0 || !is_1440_fat12_boot_sector(&boot_sector) {
            return MediaProbe::InvalidBootSector;
        }
        MediaProbe::Ready1440Fat12
    }

    fn request_sense(&mut self) -> Option<(u8, u8)> {
        let mut sense = [0u8; 18];
        let _status = self.command(&cdb(SCSI_REQUEST_SENSE), &mut sense)?;
        Some((sense[12], sense[13]))
    }

    fn command(&mut self, cdb: &[u8; UFI_CDB_BYTES], data_in: &mut [u8]) -> Option<[u8; 2]> {
        let setup = [
            REQUEST_TYPE_HOST_TO_DEVICE_CLASS_INTERFACE,
            REQUEST_CBI_ADSC,
            0,
            0,
            self.interface.interface_number,
            0,
            UFI_CDB_BYTES as u8,
            0,
        ];
        let mut command = *cdb;
        if !protocol::control_transfer_out(&self.pipe, &setup, &mut command) {
            uart::log(b"USB Floppy: CBI ADSC failed\r\n");
            return None;
        }
        if !data_in.is_empty() && self.bulk_in(data_in).is_none() {
            return None;
        }
        self.status_in()
    }

    fn bulk_in(&mut self, buffer: &mut [u8]) -> Option<usize> {
        let mps = self.interface.bulk_in_mps.max(1) as usize;
        let mut received = 0usize;
        let endpoint = Endpoint {
            device_address: self.pipe.device_address,
            endpoint_number: self.interface.bulk_in_endpoint & 0x0F,
            endpoint_type: HCCHAR_EPTYPE_BULK,
            mps: self.interface.bulk_in_mps,
            is_in: true,
            route: self.pipe.route,
        };
        while received < buffer.len() {
            let chunk_len = (buffer.len() - received).min(mps);
            let outcome = hcd::run_packet(
                &endpoint,
                false,
                self.bulk_in_toggle,
                BULK_TIMEOUT_ITERATIONS,
                BULK_SPLIT_ROUNDS,
                CompletionWait::Interrupt,
                false,
                false,
                &mut buffer[received..received + chunk_len],
            );
            match outcome {
                PacketOutcome::Ok(n) if n == chunk_len => {
                    self.bulk_in_toggle = !self.bulk_in_toggle;
                    received += n;
                }
                PacketOutcome::Ok(_) => {
                    uart::log(b"USB Floppy: short bulk IN response\r\n");
                    return None;
                }
                PacketOutcome::Timeout => {
                    uart::log(b"USB Floppy: bulk IN timed out\r\n");
                    return None;
                }
                PacketOutcome::Error => {
                    uart::log(b"USB Floppy: bulk IN transaction error\r\n");
                    return None;
                }
            }
        }
        Some(received)
    }

    fn status_in(&mut self) -> Option<[u8; CBI_STATUS_BYTES]> {
        let endpoint = Endpoint {
            device_address: self.pipe.device_address,
            endpoint_number: self.interface.status_in_endpoint & 0x0F,
            // This DWC host needs the existing non-periodic BULK workaround
            // for manually-polled Interrupt IN endpoints (see `hid.rs`).
            endpoint_type: HCCHAR_EPTYPE_BULK,
            mps: self.interface.status_in_mps,
            is_in: true,
            route: self.pipe.route,
        };
        let mut status = [0u8; CBI_STATUS_BYTES];
        match hcd::run_packet(
            &endpoint,
            false,
            self.status_in_toggle,
            BULK_TIMEOUT_ITERATIONS,
            BULK_SPLIT_ROUNDS,
            CompletionWait::Interrupt,
            false,
            false,
            &mut status,
        ) {
            PacketOutcome::Ok(CBI_STATUS_BYTES) => {
                self.status_in_toggle = !self.status_in_toggle;
                Some(status)
            }
            PacketOutcome::Ok(_) => {
                uart::log(b"USB Floppy: short CBI status\r\n");
                None
            }
            PacketOutcome::Timeout => {
                uart::log(b"USB Floppy: CBI status timed out\r\n");
                None
            }
            PacketOutcome::Error => {
                uart::log(b"USB Floppy: CBI status transaction error\r\n");
                None
            }
        }
    }
}

fn cdb(opcode: u8) -> [u8; UFI_CDB_BYTES] {
    let mut cdb = [0u8; UFI_CDB_BYTES];
    cdb[0] = opcode;
    if opcode == SCSI_REQUEST_SENSE {
        cdb[4] = 18;
    }
    cdb
}

fn read_10_cdb(lba: u32, blocks: u16) -> [u8; UFI_CDB_BYTES] {
    let mut cdb = cdb(SCSI_READ_10);
    cdb[2..6].copy_from_slice(&lba.to_be_bytes());
    cdb[7..9].copy_from_slice(&blocks.to_be_bytes());
    cdb
}

fn is_1440_fat12_boot_sector(sector: &[u8; BLOCK_BYTES]) -> bool {
    sector[11..13] == 512u16.to_le_bytes()
        && sector[13] == 1
        && sector[14..16] == 1u16.to_le_bytes()
        && sector[16] == 2
        && sector[17..19] == 224u16.to_le_bytes()
        && sector[19..21] == 2880u16.to_le_bytes()
        && sector[22..24] == 9u16.to_le_bytes()
        && sector[510] == 0x55
        && sector[511] == 0xAA
}
