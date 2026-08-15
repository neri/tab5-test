//! CardKB v1.1 reader for the Tab5 PORT.A connector.
//!
//! The connector exposes GPIO53 as SDA and GPIO54 as SCL.  CardKB returns one
//! key byte directly when addressed for reading at I2C address 0x5F.  The
//! four cursor keys return CardKB-specific non-ASCII bytes; `input.rs`
//! normalizes them into its common key representation.

use crate::i2c::{self, SoftI2c};

const CARDKB_ADDRESS: u8 = 0x5F;

pub struct CardKb {
    bus: &'static SoftI2c,
}

impl CardKb {
    /// Opens the already-initialized PORT.A software-I2C bus.
    /// Initialization succeeds when both bus lines are idle high. A
    /// disconnected keyboard is therefore harmless: `poll` simply returns no
    /// byte until a device acknowledges its address.
    pub fn init() -> Option<Self> {
        let bus = i2c::cardkb_bus();
        if bus.is_idle() {
            Some(Self { bus })
        } else {
            None
        }
    }

    /// Reads the current raw key value.  `None` means no key, no attached
    /// unit, or a transient bus failure; CardKB itself returns zero when
    /// idle.  Non-ASCII cursor values are deliberately left raw here and
    /// normalized by `input::InputManager`.
    pub fn poll(&mut self) -> Option<u8> {
        let mut byte = [0u8; 1];
        self.bus.read(CARDKB_ADDRESS, &mut byte).ok()?;
        (byte[0] != 0).then_some(byte[0])
    }
}
