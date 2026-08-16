//! Read-only measurements from Tab5's built-in INA226 power monitor.
//!
//! The monitor measures the removable two-cell battery pack through the
//! board's 5 mOhm shunt resistor.  Its address differs between board
//! revisions, so initialization identifies it safely in the INA226 address
//! range before configuring it.  Configuration is limited to the INA226's own
//! measurement and calibration registers; no battery charger or power-switch
//! setting is changed here.

use crate::i2c::{self, SoftI2c};

const ADDRESS_FIRST: u8 = 0x40;
const ADDRESS_LAST: u8 = 0x4F;

const REGISTER_CONFIG: u8 = 0x00;
const REGISTER_SHUNT_VOLTAGE: u8 = 0x01;
const REGISTER_CALIBRATION: u8 = 0x05;
const REGISTER_MANUFACTURER_ID: u8 = 0xFE;

const MANUFACTURER_ID: u16 = 0x5449; // "TI"
const DIE_ID: u16 = 0x2260;

// 16 samples; 1.1 ms bus and shunt conversion; continuous shunt + bus.
const CONFIG_CONTINUOUS_16_SAMPLES: u16 = 0x0527;
// A 62.5 uA current LSB makes 0x4000 the exact calibration for Tab5's
// 5 mOhm shunt: 0.00512 / (62.5e-6 * 0.005) = 16384.
const CALIBRATION_5_MILLIOHM: u16 = 0x4000;
const CURRENT_LSB_UA_NUMERATOR: i32 = 125;
const CURRENT_LSB_UA_DENOMINATOR: i32 = 2;
const MAX_BUS_VOLTAGE_MV: u32 = 36_000;

/// An INA226 measurement, represented in integer micro-units to keep the
/// driver free of floating point support.
#[derive(Clone, Copy)]
pub struct BatterySample {
    /// Battery-pack bus voltage in millivolts.
    pub bus_voltage_mv: u32,
    /// Shunt voltage in microvolts.  Its sign follows INA226 IN+ to IN-.
    pub shunt_voltage_uv: i32,
    /// Shunt current in microamps.  Its sign follows INA226 IN+ to IN-.
    pub current_ua: i32,
    /// Instantaneous bus-voltage × current power in microwatts.
    pub power_uw: i32,
}

/// Stage that prevented the monitor from being used.
#[derive(Clone, Copy)]
pub enum InitError {
    NotFound,
    Configure,
}

impl InitError {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotFound => "INA226 NOT FOUND ON I2C 0X40-0X4F",
            Self::Configure => "INA226 CONFIGURATION FAILED",
        }
    }

    pub fn log_message(self) -> &'static [u8] {
        match self {
            Self::NotFound => b"Battery: INA226 identity read failed\r\n",
            Self::Configure => b"Battery: INA226 configuration write failed\r\n",
        }
    }
}

/// The Tab5 battery power monitor on the board I2C bus.
pub struct Ina226 {
    bus: &'static SoftI2c,
    address: u8,
}

impl Ina226 {
    /// Verifies the monitor identity and starts stable continuous conversion.
    pub fn init() -> Result<Self, InitError> {
        let bus = i2c::board_bus();
        let Some(address) = find_address(bus) else {
            return Err(InitError::NotFound);
        };
        if !write_register(bus, address, REGISTER_CONFIG, CONFIG_CONTINUOUS_16_SAMPLES)
            || !write_register(bus, address, REGISTER_CALIBRATION, CALIBRATION_5_MILLIOHM)
        {
            return Err(InitError::Configure);
        }
        crate::uart::log_hex(b"Battery: INA226 found at I2C address=0x", address as u32);
        Ok(Self { bus, address })
    }

    /// The verified 7-bit I2C address of this monitor.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Reads shunt voltage, pack voltage, and calibrated current.
    ///
    /// Each 16-bit register is read in its own transaction. This avoids
    /// relying on register-pointer auto-increment while handling board
    /// revisions and mixed I2C traffic.
    pub fn read_sample(&self) -> Option<BatterySample> {
        let shunt_raw = read_word(self.bus, self.address, REGISTER_SHUNT_VOLTAGE)? as i16 as i32;
        let bus_raw = read_word(self.bus, self.address, REGISTER_SHUNT_VOLTAGE + 1)? as u32;
        let current_raw =
            read_word(self.bus, self.address, REGISTER_SHUNT_VOLTAGE + 3)? as i16 as i32;
        let bus_voltage_mv = bus_raw * 5 / 4; // 1.25 mV/LSB
        if bus_voltage_mv > MAX_BUS_VOLTAGE_MV {
            crate::uart::log_hex(b"Battery: invalid INA226 bus register=", bus_raw);
            return None;
        }
        let current_ua = current_raw * CURRENT_LSB_UA_NUMERATOR / CURRENT_LSB_UA_DENOMINATOR;
        Some(BatterySample {
            bus_voltage_mv,
            shunt_voltage_uv: shunt_raw * 5 / 2, // 2.5 uV/LSB
            current_ua,
            power_uw: (bus_voltage_mv as i64 * current_ua as i64 / 1_000) as i32,
        })
    }
}

/// Finds a genuine INA226 rather than merely accepting an ACK from another
/// board device.  The manufacturer and die IDs are separate reads so this
/// does not depend on register-pointer wrapping at the end of the map.
fn find_address(bus: &SoftI2c) -> Option<u8> {
    for address in ADDRESS_FIRST..=ADDRESS_LAST {
        let mut manufacturer = [0u8; 2];
        let mut die = [0u8; 2];
        if read_registers(bus, address, REGISTER_MANUFACTURER_ID, &mut manufacturer)
            && read_registers(bus, address, REGISTER_MANUFACTURER_ID + 1, &mut die)
            && u16::from_be_bytes(manufacturer) == MANUFACTURER_ID
            && u16::from_be_bytes(die) == DIE_ID
        {
            return Some(address);
        }
    }
    None
}

fn read_registers(bus: &SoftI2c, address: u8, register: u8, values: &mut [u8]) -> bool {
    bus.write_read(address, &[register], values).is_ok()
}

fn read_word(bus: &SoftI2c, address: u8, register: u8) -> Option<u16> {
    let mut bytes = [0u8; 2];
    read_registers(bus, address, register, &mut bytes).then(|| u16::from_be_bytes(bytes))
}

fn write_register(bus: &SoftI2c, address: u8, register: u8, value: u16) -> bool {
    bus.write(address, &[register, (value >> 8) as u8, value as u8])
        .is_ok()
}
