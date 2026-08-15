//! Direct I2C driver for Tab5's built-in BMI270 six-axis IMU.
//!
//! The caller supplies the BMI270 configuration firmware because different
//! feature sets use different images. This driver owns the bus protocol,
//! reset, register configuration, and raw sample decoding.

use crate::delay::delay_ms;
use crate::i2c::{self, SoftI2c};

const ADDRESS: u8 = 0x68;
const CHIP_ID: u8 = 0x24;

/// One raw BMI270 sample. Acceleration is configured to +/-4 g and the
/// gyroscope to +/-1000 degrees/second.
#[derive(Clone, Copy)]
pub struct MotionSample {
    pub acceleration: [i16; 3],
    pub gyroscope: [i16; 3],
}

impl MotionSample {
    pub const ZERO: Self = Self {
        acceleration: [0; 3],
        gyroscope: [0; 3],
    };
}

/// Initialization stage that failed, suitable for a caller's UI and log.
#[derive(Clone, Copy)]
pub enum InitError {
    NotFound,
    Reset,
    Firmware,
    Settings,
}

impl InitError {
    pub fn message(self) -> &'static str {
        match self {
            InitError::NotFound => "BMI270 NOT FOUND ON I2C",
            InitError::Reset => "BMI270 RESET WRITE FAILED",
            InitError::Firmware => "BMI270 FIRMWARE LOAD FAILED",
            InitError::Settings => "BMI270 CONFIGURATION FAILED",
        }
    }

    pub fn log_message(self) -> &'static [u8] {
        match self {
            InitError::NotFound => b"Axis test: BMI270 chip ID read failed\r\n",
            InitError::Reset => b"Axis test: BMI270 reset write failed\r\n",
            InitError::Firmware => b"Axis test: BMI270 firmware load failed\r\n",
            InitError::Settings => b"Axis test: BMI270 configuration write failed\r\n",
        }
    }
}

/// A configured BMI270 on Tab5's board I2C bus.
pub struct Bmi270 {
    bus: &'static SoftI2c,
}

impl Bmi270 {
    /// Resets the device, loads an even-length configuration image, and
    /// enables 200 Hz acceleration and gyroscope output.
    pub fn init(firmware: &[u8]) -> Result<Self, InitError> {
        if firmware.is_empty() || firmware.len() % 2 != 0 {
            return Err(InitError::Firmware);
        }
        let bus = i2c::board_bus();

        let mut id = [0u8; 1];
        if !read_register(&bus, 0x00, &mut id) || id[0] != CHIP_ID {
            return Err(InitError::NotFound);
        }
        if !write_register(&bus, 0x7E, 0xB6) {
            return Err(InitError::Reset);
        }
        delay_ms(10);
        if !write_register(&bus, 0x7C, 0x00) || !upload_firmware(&bus, firmware) {
            return Err(InitError::Firmware);
        }
        // 200 Hz / +/-4 g acceleration, 200 Hz / +/-1000 dps gyro.
        if !write_register(&bus, 0x40, 0xA9)
            || !write_register(&bus, 0x41, 0x01)
            || !write_register(&bus, 0x42, 0xE9)
            || !write_register(&bus, 0x43, 0x01)
            || !write_register(&bus, 0x7D, 0x06)
        {
            return Err(InitError::Settings);
        }
        delay_ms(50);
        Ok(Self { bus })
    }

    /// Reads acceleration XYZ and gyro XYZ from the contiguous data block.
    pub fn read_motion(&self) -> Option<MotionSample> {
        let mut bytes = [0u8; 12];
        read_register(&self.bus, 0x0C, &mut bytes).then(|| MotionSample {
            acceleration: [
                i16::from_le_bytes([bytes[0], bytes[1]]),
                i16::from_le_bytes([bytes[2], bytes[3]]),
                i16::from_le_bytes([bytes[4], bytes[5]]),
            ],
            gyroscope: [
                i16::from_le_bytes([bytes[6], bytes[7]]),
                i16::from_le_bytes([bytes[8], bytes[9]]),
                i16::from_le_bytes([bytes[10], bytes[11]]),
            ],
        })
    }
}

fn upload_firmware(bus: &SoftI2c, firmware: &[u8]) -> bool {
    delay_ms(1);
    if !write_register(bus, 0x59, 0x00) {
        return false;
    }
    for (offset, chunk) in firmware.chunks(16).enumerate() {
        let address = (offset * 8) as u16;
        if !write_register(bus, 0x5B, (address & 0x000F) as u8)
            || !write_register(bus, 0x5C, (address >> 4) as u8)
            || !write_registers(bus, 0x5E, chunk)
        {
            return false;
        }
    }
    if !write_register(bus, 0x59, 0x01) {
        return false;
    }
    delay_ms(20);
    let mut status = [0u8; 1];
    read_register(bus, 0x21, &mut status)
        && status[0] & 0x0F == 0x01
        && write_register(bus, 0x7C, 0x01)
}

fn read_register(bus: &SoftI2c, register: u8, buffer: &mut [u8]) -> bool {
    bus.write_read(ADDRESS, &[register], buffer).is_ok()
}

fn write_register(bus: &SoftI2c, register: u8, value: u8) -> bool {
    write_registers(bus, register, &[value])
}

fn write_registers(bus: &SoftI2c, register: u8, values: &[u8]) -> bool {
    bus.transaction(|transaction| {
        transaction.start_write(ADDRESS)?;
        transaction.write_byte(register)?;
        transaction.write_all(values)
    })
    .is_ok()
}
