//! USB Mass Storage class driver for the SCSI Transparent Command Set over
//! Bulk-Only Transport. The BOT envelope lives in `bot.rs`.

use super::bot::{self, BotInterface, BulkOnlyTransport};
use super::protocol::EnumeratedDevice;
use crate::delay::delay_ms;
use crate::uart;

const INTERFACE_SUBCLASS_SCSI_TRANSPARENT: u8 = 0x06;
const SCSI_TEST_UNIT_READY: u8 = 0x00;
const SCSI_REQUEST_SENSE: u8 = 0x03;
const SCSI_INQUIRY: u8 = 0x12;
const SCSI_READ_10: u8 = 0x28;
const SCSI_READ_CAPACITY_10: u8 = 0x25;
const CSW_STATUS_PASSED: u8 = 0x00;
const BLOCK_BYTES: usize = 512;
const INQUIRY_RESPONSE_LEN: usize = 36;
const REQUEST_SENSE_RESPONSE_LEN: usize = 18;
const READ_CAPACITY_10_RESPONSE_LEN: usize = 8;
const READ_CAPACITY_10_NEEDS_CAPACITY_16: u32 = 0xFFFF_FFFF;
const READY_POLL_INTERVAL_MS: u32 = 100;

/// SCSI READ CAPACITY(10) result. `last_lba` is inclusive.
pub struct ReadCapacity {
    pub last_lba: u32,
    pub block_length: u32,
}

pub type MscInterface = BotInterface;

pub fn find_msc_interface(config: &[u8]) -> Option<MscInterface> {
    bot::find_interface(config, INTERFACE_SUBCLASS_SCSI_TRANSPARENT)
}

pub struct UsbMassStorage {
    bot: BulkOnlyTransport,
}

impl UsbMassStorage {
    pub fn attach(device: &EnumeratedDevice) -> Option<Self> {
        let interface = find_msc_interface(device.config_bytes())?;
        let bot = BulkOnlyTransport::attach(device, interface)?;
        Some(Self { bot })
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
                None => return false,
            }
            if attempt + 1 < attempts {
                delay_ms(READY_POLL_INTERVAL_MS);
            }
        }
        false
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
        let cdb = [
            SCSI_READ_10,
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
        let Some(result) = self.bot.execute_command(&cdb, true, buffer) else {
            return false;
        };
        if result.status != CSW_STATUS_PASSED {
            uart::log_hex(
                b"USB MSC: READ(10) failed, CSW status=",
                result.status as u32,
            );
            return false;
        }
        if result.transferred < buffer.len() {
            uart::log(b"USB MSC: short READ(10) response\r\n");
            return false;
        }
        true
    }
}
