//! Tab5 whole-device power control.
//!
//! The physical power key is handled by the board power controller, but the
//! same controller accepts a shutdown request on PI4IOE2 (I2C address 0x44)
//! output P4. The factory firmware drives that input as three 100 ms pulses.

use crate::delay::delay_ms;
use crate::usb;
use crate::uart;

/// PI4IOE2 P4 is wired to the board power controller's `PWROFF_PULSE` input.
const POWEROFF_PULSE_BIT: u8 = 4;
const PULSE_COUNT: u8 = 3;
const PULSE_WIDTH_MS: u32 = 100;

/// Requests a complete Tab5 power-off.
///
/// Returns `false` if the I2C expander did not acknowledge any step. A
/// successful call normally removes power before it returns; if an external
/// supply or board fault keeps the system alive, returning `true` only means
/// that all requested pulses were delivered to the expander.
pub fn shutdown() -> bool {
    uart::log(b"POWER: requesting shutdown via PI4IOE2 P4\r\n");
    for _ in 0..PULSE_COUNT {
        if !usb::set_pi4ioe2_output_bit(POWEROFF_PULSE_BIT, true) {
            uart::log(b"POWER: PI4IOE2 high pulse was not acknowledged\r\n");
            return false;
        }
        delay_ms(PULSE_WIDTH_MS);
        if !usb::set_pi4ioe2_output_bit(POWEROFF_PULSE_BIT, false) {
            uart::log(b"POWER: PI4IOE2 low pulse was not acknowledged\r\n");
            return false;
        }
        delay_ms(PULSE_WIDTH_MS);
    }
    true
}
