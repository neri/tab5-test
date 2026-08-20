//! Driver for M5Stack's dedicated Tab5 Keyboard on Ext.Port1.
//!
//! The 70-key keyboard is an I2C peripheral at `0x6D` on GPIO0/1.  Its HID
//! mode supplies an HID modifier byte and usage ID for each press; translating
//! that pair through `input::key_from_hid_usage` makes it indistinguishable to
//! application code from a USB HID or CardKB key.

use crate::i2c::{self, I2cError, SoftI2c};
use crate::input::{Key, key_from_hid_usage};

const ADDRESS: u8 = 0x6D;
const REG_EVENT_NUM: u8 = 0x02;
const REG_MODE_KEYBOARD: u8 = 0x10;
const REG_HID_EVENT: u8 = 0x30;
const REG_FIRMWARE_VERSION: u8 = 0xFE;
const MODE_HID: u8 = 1;
const EMPTY_EVENT: [u8; 2] = [0xFF; 2];
const MAX_EVENTS_PER_POLL: usize = 32;

pub struct Tab5Keyboard {
    bus: &'static SoftI2c,
}

impl Tab5Keyboard {
    /// Probes the keyboard, switches it to HID mode, and drops stale events
    /// left from before the host started.  A missing keyboard only NACKs and
    /// therefore leaves the input manager usable.
    pub fn init() -> Option<Self> {
        let bus = i2c::tab5_keyboard_bus();
        let mut version = [0u8; 1];
        bus.write_read(ADDRESS, &[REG_FIRMWARE_VERSION], &mut version)
            .ok()?;
        bus.write(ADDRESS, &[REG_MODE_KEYBOARD, MODE_HID]).ok()?;
        bus.write(ADDRESS, &[REG_EVENT_NUM, 0]).ok()?;
        Some(Self { bus })
    }

    /// Verifies that the keyboard is still in HID mode.  A brief power loss
    /// during a fast unplug/replug can reset the keyboard without leaving an
    /// observed I2C NACK; in that case restore the mode and discard events
    /// produced before the host's common `Key` mapping was active again.
    pub fn ensure_hid_mode(&mut self) -> Result<(), I2cError> {
        let mut mode = [0u8; 1];
        self.bus
            .write_read(ADDRESS, &[REG_MODE_KEYBOARD], &mut mode)?;
        if mode[0] == MODE_HID {
            return Ok(());
        }
        self.bus.write(ADDRESS, &[REG_MODE_KEYBOARD, MODE_HID])?;
        self.bus.write(ADDRESS, &[REG_EVENT_NUM, 0])
    }

    /// Returns the next press from the device's HID event queue.  Release
    /// events use keycode zero and are consumed here, as are unsupported HID
    /// usages, so callers receive the same press-only `Key` stream as USB.
    pub fn poll(&mut self) -> Result<Option<Key>, I2cError> {
        for _ in 0..MAX_EVENTS_PER_POLL {
            let mut event = [0u8; 2];
            self.bus.write_read(ADDRESS, &[REG_HID_EVENT], &mut event)?;
            if event == EMPTY_EVENT {
                return Ok(None);
            }
            if let Some(key) = key_from_hid_usage(event[1], event[0]) {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }
}
