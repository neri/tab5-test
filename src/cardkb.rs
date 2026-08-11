//! CardKB v1.1 reader for the Tab5 PORT.A connector.
//!
//! The connector exposes GPIO53 as SDA and GPIO54 as SCL.  CardKB returns one
//! key byte directly when addressed for reading at I2C address 0x5F.

use crate::delay::delay_us;
use crate::gpio;
use crate::i2c::SoftI2c;

const SDA: u32 = 53;
const SCL: u32 = 54;
const CARDKB_ADDRESS: u8 = 0x5F;

pub struct CardKb {
    bus: SoftI2c,
}

impl CardKb {
    /// Configures PORT.A for open-drain, software-I2C operation.
    ///
    /// Initialization succeeds when both bus lines can be released high.  A
    /// disconnected keyboard is therefore harmless: `poll` simply returns no
    /// byte until a device acknowledges its address.
    pub fn init() -> Option<Self> {
        let bus = SoftI2c::new(SDA, SCL, 5, 40_000);
        gpio::configure_open_drain(SDA);
        gpio::configure_open_drain(SCL);
        gpio::release(SDA);
        gpio::release(SCL);
        delay_us(20);

        // Recover a bus left in the middle of a transaction before starting
        // normal polling.  This also makes hot-plugging the unit predictable.
        for _ in 0..9 {
            gpio::drive_low(SCL);
            bus.delay();
            gpio::release(SCL);
            if !bus.wait_scl_high() {
                return None;
            }
            bus.delay();
        }
        bus.stop();
        if gpio::level(SDA) && gpio::level(SCL) {
            Some(Self { bus })
        } else {
            None
        }
    }

    /// Reads the current key value.  `None` means no key, no attached unit,
    /// or a transient bus failure; CardKB itself returns zero when idle.
    pub fn poll(&mut self) -> Option<u8> {
        if !self.bus.start() || !self.bus.write_byte((CARDKB_ADDRESS << 1) | 1) {
            self.bus.stop();
            return None;
        }
        let Some(byte) = self.bus.read_byte(false) else {
            self.bus.stop();
            return None;
        };
        self.bus.stop();
        (byte != 0).then_some(byte)
    }
}
