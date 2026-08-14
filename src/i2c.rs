//! Bit-banged software I2C master.
//!
//! A generic interface built on top of `crate::gpio`'s open-drain-style pin
//! operations. `lcd.rs` (PI4IOE1 reset control) and `cardkb.rs` (CardKB
//! reads) each implemented the same state machine, differing only in their
//! SDA/SCL pin numbers and bit timing, so it is consolidated here.

use crate::gpio::{self, Pin};

/// One software I2C bus and its bit timing.
///
/// `delay_us` and `scl_wait_iterations` exist to pass through the values
/// each caller has measured and tuned on its own; there is no shared
/// default.
pub struct SoftI2c {
    sda: Pin,
    scl: Pin,
    delay_us: u32,
    scl_wait_iterations: u32,
}

impl SoftI2c {
    pub const fn new(sda: Pin, scl: Pin, delay_us: u32, scl_wait_iterations: u32) -> Self {
        Self {
            sda,
            scl,
            delay_us,
            scl_wait_iterations,
        }
    }

    pub fn start(&self) -> bool {
        gpio::release(self.sda);
        gpio::release(self.scl);
        self.delay();
        if !gpio::level(self.sda) || !self.wait_scl_high() {
            return false;
        }
        gpio::drive_low(self.sda);
        self.delay();
        gpio::drive_low(self.scl);
        true
    }

    pub fn stop(&self) {
        gpio::drive_low(self.sda);
        self.delay();
        let _ = self.wait_scl_high();
        self.delay();
        gpio::release(self.sda);
        self.delay();
    }

    pub fn write_byte(&self, byte: u8) -> bool {
        for bit in (0..8).rev() {
            gpio::drive_low(self.scl);
            if byte & (1 << bit) != 0 {
                gpio::release(self.sda);
            } else {
                gpio::drive_low(self.sda);
            }
            self.delay();
            if !self.wait_scl_high() {
                return false;
            }
            self.delay();
        }

        gpio::drive_low(self.scl);
        gpio::release(self.sda);
        self.delay();
        if !self.wait_scl_high() {
            return false;
        }
        let acknowledged = !gpio::level(self.sda);
        self.delay();
        gpio::drive_low(self.scl);
        acknowledged
    }

    pub fn read_byte(&self, acknowledge: bool) -> Option<u8> {
        let mut byte = 0u8;
        gpio::release(self.sda);
        for _ in 0..8 {
            gpio::drive_low(self.scl);
            self.delay();
            if !self.wait_scl_high() {
                return None;
            }
            byte = (byte << 1) | gpio::level(self.sda) as u8;
            self.delay();
        }

        gpio::drive_low(self.scl);
        if acknowledge {
            gpio::drive_low(self.sda);
        } else {
            gpio::release(self.sda); // NACK the final byte of a one-byte read.
        }
        self.delay();
        if !self.wait_scl_high() {
            return None;
        }
        self.delay();
        gpio::drive_low(self.scl);
        gpio::release(self.sda);
        Some(byte)
    }

    /// Releases SCL and polls it high. Exposed for callers that need to run
    /// their own bus-recovery pulses (see `lcd::reset_lcd_panel` and
    /// `cardkb::CardKb::init`) using this bus's own retry count.
    pub fn wait_scl_high(&self) -> bool {
        gpio::release(self.scl);
        for _ in 0..self.scl_wait_iterations {
            if gpio::level(self.scl) {
                return true;
            }
        }
        false
    }

    /// Waits this bus's configured bit-time. Exposed for the same
    /// bus-recovery callers as `wait_scl_high`.
    pub fn delay(&self) {
        crate::delay::delay_us(self.delay_us);
    }
}
