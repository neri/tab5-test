//! Bit-banged software I2C master.
//!
//! Normal callers use [`SoftI2c::write`], [`SoftI2c::read`], or
//! [`SoftI2c::write_read`]. Those methods own the complete transaction,
//! including the address byte, repeated START, final read NACK, and STOP.
//! [`SoftI2c::transaction`] is for protocols whose read length is learned
//! while the transaction is in progress.

use crate::gpio::{self, Pin};
use core::sync::atomic::{AtomicBool, Ordering};

/// The shared I2C bus on Tab5's board (SDA31/SCL32).
///
/// This is initialized once during boot before any LCD, touch, BMI270, or
/// USB-VBUS operation uses it.
static BOARD_BUS: SoftI2c = SoftI2c::new(Pin::BoardI2cSda, Pin::BoardI2cScl, 3, 10_000);
static BOARD_BUS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// The independent I2C bus exposed by the CardKB connector (GPIO53/54).
static CARDKB_BUS: SoftI2c = SoftI2c::new(Pin::CardKbSda, Pin::CardKbScl, 5, 40_000);
static CARDKB_BUS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// The I2C bus exposed by Tab5 Ext.Port1 (GPIO0/1) for the dedicated keyboard.
static TAB5_KEYBOARD_BUS: SoftI2c =
    SoftI2c::new(Pin::Tab5KeyboardSda, Pin::Tab5KeyboardScl, 2, 40_000);
static TAB5_KEYBOARD_BUS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Returns the shared board I2C bus after [`initialize_board_bus`] has run.
pub fn board_bus() -> &'static SoftI2c {
    &BOARD_BUS
}

/// Configures and recovers the board I2C bus once during boot.
pub fn initialize_board_bus() -> Result<(), I2cError> {
    initialize_once(&BOARD_BUS, &BOARD_BUS_INITIALIZED)
}

/// Returns the CardKB connector's I2C bus after [`initialize_cardkb_bus`] has run.
pub fn cardkb_bus() -> &'static SoftI2c {
    &CARDKB_BUS
}

/// Configures and recovers the CardKB I2C bus once during input initialization.
pub fn initialize_cardkb_bus() -> Result<(), I2cError> {
    initialize_once(&CARDKB_BUS, &CARDKB_BUS_INITIALIZED)
}

/// Returns the Tab5 Keyboard I2C bus after [`initialize_tab5_keyboard_bus`] has run.
pub fn tab5_keyboard_bus() -> &'static SoftI2c {
    &TAB5_KEYBOARD_BUS
}

/// Configures and recovers the dedicated Tab5 Keyboard bus once during input initialization.
pub fn initialize_tab5_keyboard_bus() -> Result<(), I2cError> {
    initialize_once(&TAB5_KEYBOARD_BUS, &TAB5_KEYBOARD_BUS_INITIALIZED)
}

/// Failure while driving an I2C transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum I2cError {
    /// SDA was held low when a START condition was requested.
    BusBusy,
    /// SCL did not rise before this bus's clock-stretch timeout elapsed.
    ClockStretchTimeout,
    /// The addressed device did not acknowledge its address byte.
    AddressNack,
    /// The addressed device did not acknowledge a written data byte.
    DataNack,
    /// The address, buffer length, or transaction operation order was invalid.
    InvalidTransfer,
}

/// One software I2C bus and its bit timing.
///
/// `delay_us` and `scl_wait_iterations` exist to pass through the values
/// each caller has measured and tuned on its own; there is no shared default.
pub struct SoftI2c {
    sda: Pin,
    scl: Pin,
    delay_us: u32,
    scl_wait_iterations: u32,
}

/// An in-progress I2C transaction, scoped to [`SoftI2c::transaction`].
///
/// This exposes byte-wise operations only for protocols with dynamic transfer
/// lengths. Its lifetime prevents callers from leaving a transaction open.
pub struct I2cTransaction<'a> {
    bus: &'a SoftI2c,
    started: bool,
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

    /// Configures this physical bus's pins and performs one recovery sequence.
    ///
    /// Call this once after reset, before making transactions. Reconfiguration
    /// drives the pins briefly, so it must not run while another transaction
    /// on the same physical bus is in progress.
    pub fn initialize(&self) -> Result<(), I2cError> {
        gpio::configure_open_drain(self.sda);
        gpio::configure_open_drain(self.scl);
        self.recover()
    }

    /// Attempts to return this bus to its idle state after a bus fault.
    ///
    /// Recovery clocks SCL nine times and then sends STOP. It is deliberately
    /// separate from normal transactions; callers must not run it routinely.
    pub fn recover(&self) -> Result<(), I2cError> {
        gpio::release(self.sda);
        gpio::release(self.scl);
        crate::delay::delay_us(20);

        for _ in 0..9 {
            gpio::drive_low(self.scl);
            self.delay();
            gpio::release(self.scl);
            if !self.wait_scl_high() {
                return Err(I2cError::ClockStretchTimeout);
            }
            self.delay();
        }
        self.stop_condition()
    }

    /// Returns whether both lines are released high.
    pub fn is_idle(&self) -> bool {
        gpio::level(self.sda) && gpio::level(self.scl)
    }

    /// Writes one or more bytes after a 7-bit device address.
    pub fn write(&self, address: u8, bytes: &[u8]) -> Result<(), I2cError> {
        if !valid_address(address) || bytes.is_empty() {
            return Err(I2cError::InvalidTransfer);
        }
        self.transaction(|transaction| {
            transaction.start_write(address)?;
            transaction.write_all(bytes)
        })
    }

    /// Reads a fixed number of bytes after a 7-bit device address.
    pub fn read(&self, address: u8, buffer: &mut [u8]) -> Result<(), I2cError> {
        if !valid_address(address) || buffer.is_empty() {
            return Err(I2cError::InvalidTransfer);
        }
        self.transaction(|transaction| {
            transaction.start_read(address)?;
            transaction.read_all(buffer)
        })
    }

    /// Writes a prefix, then uses a repeated START to read a fixed response.
    pub fn write_read(&self, address: u8, write: &[u8], read: &mut [u8]) -> Result<(), I2cError> {
        if !valid_address(address) || write.is_empty() || read.is_empty() {
            return Err(I2cError::InvalidTransfer);
        }
        self.transaction(|transaction| {
            transaction.start_write(address)?;
            transaction.write_all(write)?;
            transaction.restart_read(address)?;
            transaction.read_all(read)
        })
    }

    /// Runs a variable-length transaction and finishes every opened bus
    /// transaction with STOP before returning.
    pub fn transaction<T>(
        &self,
        operation: impl FnOnce(&mut I2cTransaction<'_>) -> Result<T, I2cError>,
    ) -> Result<T, I2cError> {
        let mut transaction = I2cTransaction {
            bus: self,
            started: false,
        };
        let result = operation(&mut transaction);
        let stop_result = if transaction.started {
            transaction.bus.stop_condition()
        } else {
            Ok(())
        };

        match result {
            Err(error) => Err(error),
            Ok(value) => stop_result.map(|()| value),
        }
    }

    fn wait_scl_high(&self) -> bool {
        gpio::release(self.scl);
        for _ in 0..self.scl_wait_iterations {
            if gpio::level(self.scl) {
                return true;
            }
        }
        false
    }

    fn delay(&self) {
        crate::delay::delay_us(self.delay_us);
    }

    fn start_condition(&self) -> Result<(), I2cError> {
        gpio::release(self.sda);
        gpio::release(self.scl);
        self.delay();
        if !gpio::level(self.sda) {
            return Err(I2cError::BusBusy);
        }
        if !self.wait_scl_high() {
            return Err(I2cError::ClockStretchTimeout);
        }
        gpio::drive_low(self.sda);
        self.delay();
        gpio::drive_low(self.scl);
        Ok(())
    }

    fn stop_condition(&self) -> Result<(), I2cError> {
        gpio::drive_low(self.sda);
        self.delay();
        let scl_high = self.wait_scl_high();
        self.delay();
        gpio::release(self.sda);
        self.delay();
        if scl_high {
            Ok(())
        } else {
            Err(I2cError::ClockStretchTimeout)
        }
    }

    fn write_raw_byte(&self, byte: u8) -> Result<bool, I2cError> {
        for bit in (0..8).rev() {
            gpio::drive_low(self.scl);
            if byte & (1 << bit) != 0 {
                gpio::release(self.sda);
            } else {
                gpio::drive_low(self.sda);
            }
            self.delay();
            if !self.wait_scl_high() {
                return Err(I2cError::ClockStretchTimeout);
            }
            self.delay();
        }

        gpio::drive_low(self.scl);
        gpio::release(self.sda);
        self.delay();
        if !self.wait_scl_high() {
            return Err(I2cError::ClockStretchTimeout);
        }
        let acknowledged = !gpio::level(self.sda);
        self.delay();
        gpio::drive_low(self.scl);
        Ok(acknowledged)
    }

    fn read_raw_byte(&self, acknowledge: bool) -> Result<u8, I2cError> {
        let mut byte = 0u8;
        gpio::release(self.sda);
        for _ in 0..8 {
            gpio::drive_low(self.scl);
            self.delay();
            if !self.wait_scl_high() {
                return Err(I2cError::ClockStretchTimeout);
            }
            byte = (byte << 1) | gpio::level(self.sda) as u8;
            self.delay();
        }

        gpio::drive_low(self.scl);
        if acknowledge {
            gpio::drive_low(self.sda);
        } else {
            gpio::release(self.sda);
        }
        self.delay();
        if !self.wait_scl_high() {
            return Err(I2cError::ClockStretchTimeout);
        }
        self.delay();
        gpio::drive_low(self.scl);
        gpio::release(self.sda);
        Ok(byte)
    }
}

impl I2cTransaction<'_> {
    /// Begins a transaction by addressing a device for writing.
    pub fn start_write(&mut self, address: u8) -> Result<(), I2cError> {
        if self.started {
            return Err(I2cError::InvalidTransfer);
        }
        self.start_address(address, false)
    }

    /// Begins a transaction by addressing a device for reading.
    pub fn start_read(&mut self, address: u8) -> Result<(), I2cError> {
        if self.started {
            return Err(I2cError::InvalidTransfer);
        }
        self.start_address(address, true)
    }

    /// Issues a repeated START and addresses a device for writing.
    #[allow(dead_code)] // Kept for device protocols that switch from reading back to writing.
    pub fn restart_write(&mut self, address: u8) -> Result<(), I2cError> {
        if !self.started {
            return Err(I2cError::InvalidTransfer);
        }
        self.start_address(address, false)
    }

    /// Issues a repeated START and addresses a device for reading.
    pub fn restart_read(&mut self, address: u8) -> Result<(), I2cError> {
        if !self.started {
            return Err(I2cError::InvalidTransfer);
        }
        self.start_address(address, true)
    }

    /// Writes one data byte and requires an ACK.
    pub fn write_byte(&mut self, byte: u8) -> Result<(), I2cError> {
        if !self.started {
            return Err(I2cError::InvalidTransfer);
        }
        if self.bus.write_raw_byte(byte)? {
            Ok(())
        } else {
            Err(I2cError::DataNack)
        }
    }

    /// Writes data bytes and requires an ACK for every byte.
    pub fn write_all(&mut self, bytes: &[u8]) -> Result<(), I2cError> {
        if bytes.is_empty() {
            return Err(I2cError::InvalidTransfer);
        }
        for &byte in bytes {
            self.write_byte(byte)?;
        }
        Ok(())
    }

    /// Reads one byte, ACKing it when another byte will follow and NACKing it
    /// when it is the final byte of the read phase.
    pub fn read_byte(&mut self, acknowledge: bool) -> Result<u8, I2cError> {
        if !self.started {
            return Err(I2cError::InvalidTransfer);
        }
        self.bus.read_raw_byte(acknowledge)
    }

    fn read_all(&mut self, buffer: &mut [u8]) -> Result<(), I2cError> {
        if buffer.is_empty() {
            return Err(I2cError::InvalidTransfer);
        }
        let last = buffer.len() - 1;
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = self.read_byte(index != last)?;
        }
        Ok(())
    }

    fn start_address(&mut self, address: u8, read: bool) -> Result<(), I2cError> {
        if !valid_address(address) {
            return Err(I2cError::InvalidTransfer);
        }
        self.bus.start_condition()?;
        self.started = true;
        let address_byte = (address << 1) | read as u8;
        if self.bus.write_raw_byte(address_byte)? {
            Ok(())
        } else {
            Err(I2cError::AddressNack)
        }
    }
}

const fn valid_address(address: u8) -> bool {
    address < 0x80
}

fn initialize_once(bus: &SoftI2c, initialized: &AtomicBool) -> Result<(), I2cError> {
    if initialized.load(Ordering::Acquire) {
        return Ok(());
    }
    bus.initialize()?;
    initialized.store(true, Ordering::Release);
    Ok(())
}
