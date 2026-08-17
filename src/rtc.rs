//! Direct I2C driver for Tab5's RX8130CE real-time clock.
//!
//! The RX8130CE has no identity register, so nothing here can prove which
//! chip answered: every function reports the register bytes it actually read
//! and lets the caller judge them. What makes a bad address detectable in
//! practice is the register content itself -- BCD time fields with valid
//! digits in valid ranges, and a week register holding exactly one set bit --
//! which is why [`read_datetime`] returns [`Error::InvalidTime`] rather than
//! silently accepting whatever bytes arrived.
//!
//! Only the calendar counters and the flag register are ever written, and
//! only by [`write_datetime`] and [`clear_flag`]. The control registers hold
//! the backup-power settings (`CTRL1`: backup voltage select, charge enable)
//! that decide whether the clock survives with the main supply removed, so
//! this driver reads them for display and never touches them -- with the one
//! exception of `CTRL0`'s `STOP` bit, which has to be set for the duration of
//! a calendar write and is restored immediately afterwards.

use crate::i2c::{self, I2cError, SoftI2c};

/// RX8130CE's fixed 7-bit address on Tab5's board I2C bus (SDA31/SCL32),
/// shared with the I/O expanders, touch controller, BMI270 and INA226.
pub const ADDRESS: u8 = 0x32;

/// The calendar counters. `SECOND` through `YEAR` are consecutive, which is
/// what lets one transaction read or write the whole date and time.
const REGISTER_SECOND: u8 = 0x10;
const REGISTER_YEAR: u8 = 0x16;
/// Second through year, the span one calendar read or write covers.
const CALENDAR_BYTES: usize = (REGISTER_YEAR - REGISTER_SECOND + 1) as usize;
const REGISTER_EXTENSION: u8 = 0x1C;
const REGISTER_FLAG: u8 = 0x1D;
const REGISTER_CONTROL0: u8 = 0x1E;

/// First and last register this driver reads as a block: the calendar
/// counters, the alarm and wakeup-timer registers, and the extension, flag
/// and control registers. Registers above `CTRL1` are left out because
/// nothing here can name them.
pub const FIRST_REGISTER: u8 = REGISTER_SECOND;
pub const REGISTER_COUNT: usize = 16; // 0x10..=0x1F

/// Extension register bit selecting what the update flag follows: clear for
/// once per second, set for once per minute.
pub const EXTENSION_UPDATE_MINUTE: u8 = 1 << 5;

/// Flag register bits. Each flag is cleared by writing 0 to its bit and
/// preserved by writing 1, so [`clear_flag`] writes the complement of the
/// single flag it means to clear.
pub const FLAG_VOLTAGE_LOW: u8 = 1 << 1;
pub const FLAG_ALARM: u8 = 1 << 3;
pub const FLAG_TIMER: u8 = 1 << 4;
pub const FLAG_UPDATE: u8 = 1 << 5;

/// Control register 0 bits.
pub const CONTROL0_ALARM_INTERRUPT: u8 = 1 << 3;
pub const CONTROL0_TIMER_INTERRUPT: u8 = 1 << 4;
pub const CONTROL0_UPDATE_INTERRUPT: u8 = 1 << 5;
pub const CONTROL0_STOP: u8 = 1 << 6;
pub const CONTROL0_TEST: u8 = 1 << 7;

/// The RX8130CE keeps a two-digit year and has no century bit, so the century
/// is this firmware's choice rather than the device's.
const YEAR_BASE: u16 = 2000;

/// How many times [`read_datetime`] re-reads the calendar after seeing the
/// second counter move during a read.
const TORN_READ_ATTEMPTS: u32 = 3;

/// One calendar reading or setting.
///
/// `weekday` is `None` when the device's week register does not hold exactly
/// one set bit: the register is a one-of-seven bit field, not a number, so any
/// other pattern means the byte cannot be interpreted as a day of the week
/// (an unset clock, or the wrong device answering).
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub weekday: Option<u8>,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    /// Whether every field is in range for the calendar it describes,
    /// including the length of the given month in the given year.
    pub fn is_valid(&self) -> bool {
        self.year >= YEAR_BASE
            && self.year < YEAR_BASE + 100
            && (1..=12).contains(&self.month)
            && self.day >= 1
            && self.day <= days_in_month(self.year, self.month)
            && self.hour <= 23
            && self.minute <= 59
            && self.second <= 59
            && self.weekday.is_none_or(|weekday| weekday <= 6)
    }
}

/// The extension, flag and control registers, read together so that a
/// caller's report cannot mix values from different reads.
#[derive(Clone, Copy)]
pub struct Status {
    pub extension: u8,
    pub flags: u8,
    pub control0: u8,
    pub control1: u8,
}

impl Status {
    /// Whether the device reports that its oscillator stopped -- a supply
    /// interruption on both the main and backup rails. The calendar
    /// registers are meaningless until a new time is written.
    pub fn voltage_low(self) -> bool {
        self.flags & FLAG_VOLTAGE_LOW != 0
    }

    /// Whether the calendar counters are currently held stopped. This
    /// firmware only ever sets `STOP` for the duration of a calendar write,
    /// so finding it set outside one means something left the clock halted.
    pub fn stopped(self) -> bool {
        self.control0 & CONTROL0_STOP != 0
    }
}

/// Which stage of an RTC operation failed.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Error {
    /// Nothing acknowledged address 0x32.
    NotFound,
    /// The device answered but a transfer failed part-way.
    Bus,
    /// The calendar registers do not hold a valid BCD date and time.
    InvalidTime,
}

impl Error {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotFound => "RX8130CE did not answer on I2C 0x32",
            Self::Bus => "RX8130CE I2C transfer failed",
            Self::InvalidTime => "RX8130CE calendar registers are not a valid date/time",
        }
    }
}

/// Whether something acknowledges the RX8130CE's address at all.
///
/// An ACK alone does not identify the device; it only separates "no device,
/// or a bus fault" from "answered, now judge the registers".
pub fn probe() -> bool {
    let mut byte = [0u8; 1];
    read_registers_at(i2c::board_bus(), REGISTER_SECOND, &mut byte).is_ok()
}

/// Reads [`REGISTER_COUNT`] registers starting at [`FIRST_REGISTER`], for
/// callers that report the device's raw state rather than interpreting it.
pub fn read_all_registers(buffer: &mut [u8; REGISTER_COUNT]) -> Result<(), Error> {
    read_registers_at(i2c::board_bus(), FIRST_REGISTER, buffer)
}

/// Reads the extension, flag and control registers.
pub fn read_status() -> Result<Status, Error> {
    let mut bytes = [0u8; 4]; // 0x1C..=0x1F
    read_registers_at(i2c::board_bus(), REGISTER_EXTENSION, &mut bytes)?;
    Ok(Status {
        extension: bytes[0],
        flags: bytes[1],
        control0: bytes[2],
        control1: bytes[3],
    })
}

/// Reads the second counter alone, which is all a tick test needs.
pub fn read_second() -> Result<u8, Error> {
    let mut byte = [0u8; 1];
    read_registers_at(i2c::board_bus(), REGISTER_SECOND, &mut byte)?;
    from_bcd(byte[0] & 0x7F)
        .filter(|&second| second <= 59)
        .ok_or(Error::InvalidTime)
}

/// Reads the whole calendar.
///
/// The RX8130CE does not freeze its counters for the duration of a read, and
/// this bus is bit-banged at 10 kHz, so a seven-byte read takes long enough
/// for a carry to land in the middle of it (23:59:59 read as 23:59:00 of the
/// next minute). Re-reading the second counter afterwards detects exactly
/// that case, and the read is repeated when it happens.
pub fn read_datetime() -> Result<DateTime, Error> {
    let bus = i2c::board_bus();
    for _ in 0..TORN_READ_ATTEMPTS {
        let mut bytes = [0u8; CALENDAR_BYTES];
        read_registers_at(bus, REGISTER_SECOND, &mut bytes)?;
        let mut after = [0u8; 1];
        read_registers_at(bus, REGISTER_SECOND, &mut after)?;
        if after[0] != bytes[0] {
            continue;
        }
        return decode_datetime(&bytes);
    }
    Err(Error::Bus)
}

/// Writes the calendar counters.
///
/// The counters are held with `STOP` for the write so that the device cannot
/// carry between two of the seven bytes, then released. `weekday` is ignored
/// in favour of the day computed from the date, because a week register that
/// disagrees with the date is not a state any caller wants to install.
///
/// A successful write also clears the voltage-low flag: that flag means "the
/// calendar content is not trustworthy", which has just stopped being true.
/// Nothing else is cleared -- a pending alarm or timer flag belongs to
/// whoever armed it.
pub fn write_datetime(datetime: &DateTime) -> Result<(), Error> {
    if !datetime.is_valid() {
        return Err(Error::InvalidTime);
    }
    let bus = i2c::board_bus();
    let control0 = read_register(bus, REGISTER_CONTROL0)?;

    write_register(bus, REGISTER_CONTROL0, control0 | CONTROL0_STOP)?;
    let weekday = weekday_from_date(datetime.year, datetime.month, datetime.day);
    let counters: [u8; CALENDAR_BYTES] = [
        to_bcd(datetime.second),
        to_bcd(datetime.minute),
        to_bcd(datetime.hour),
        1 << weekday,
        to_bcd(datetime.day),
        to_bcd(datetime.month),
        to_bcd((datetime.year - YEAR_BASE) as u8),
    ];
    let written = write_registers(bus, REGISTER_SECOND, &counters);
    // Release the counters whether or not the write itself succeeded; leaving
    // the clock stopped is worse than a half-written calendar, which the
    // caller can retry.
    let released = write_register(bus, REGISTER_CONTROL0, control0 & !CONTROL0_STOP);
    written?;
    released?;

    clear_flag(FLAG_VOLTAGE_LOW)
}

/// Clears one flag-register bit, leaving the others as they are.
///
/// Flags are cleared by writing 0 and preserved by writing 1, so this writes
/// the complement of the requested bit rather than a read-modify-write --
/// which would reintroduce the flag if it were set again between the read and
/// the write.
pub fn clear_flag(flag: u8) -> Result<(), Error> {
    write_register(i2c::board_bus(), REGISTER_FLAG, !flag)
}

/// Day of the week for a date, 0 = Sunday, matching the bit position the
/// RX8130CE's week register uses (bit 0 = Sunday .. bit 6 = Saturday).
pub fn weekday_from_date(year: u16, month: u8, day: u8) -> u8 {
    // Zeller's congruence, whose January and February belong to the previous
    // year.
    let (year, month) = if month <= 2 {
        (year - 1, month + 12)
    } else {
        (year, month)
    };
    let (year, month, day) = (year as u32, month as u32, day as u32);
    let century = year / 100;
    let within_century = year % 100;
    let saturday_based = (day
        + 13 * (month + 1) / 5
        + within_century
        + within_century / 4
        + century / 4
        + 5 * century)
        % 7;
    // Zeller counts from Saturday; shift to Sunday.
    ((saturday_based + 6) % 7) as u8
}

pub fn weekday_name(weekday: u8) -> &'static str {
    match weekday {
        0 => "Sun",
        1 => "Mon",
        2 => "Tue",
        3 => "Wed",
        4 => "Thu",
        5 => "Fri",
        6 => "Sat",
        _ => "???",
    }
}

pub fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

/// Interprets seven calendar bytes, rejecting anything that is not a valid
/// BCD date and time. The masks drop the bits each register leaves unused.
fn decode_datetime(bytes: &[u8; CALENDAR_BYTES]) -> Result<DateTime, Error> {
    let second = from_bcd(bytes[0] & 0x7F).ok_or(Error::InvalidTime)?;
    let minute = from_bcd(bytes[1] & 0x7F).ok_or(Error::InvalidTime)?;
    let hour = from_bcd(bytes[2] & 0x3F).ok_or(Error::InvalidTime)?;
    let week = bytes[3] & 0x7F;
    let day = from_bcd(bytes[4] & 0x3F).ok_or(Error::InvalidTime)?;
    let month = from_bcd(bytes[5] & 0x1F).ok_or(Error::InvalidTime)?;
    let year = from_bcd(bytes[6]).ok_or(Error::InvalidTime)?;

    let datetime = DateTime {
        year: YEAR_BASE + year as u16,
        month,
        day,
        weekday: (week.count_ones() == 1).then(|| week.trailing_zeros() as u8),
        hour,
        minute,
        second,
    };
    if datetime.is_valid() {
        Ok(datetime)
    } else {
        Err(Error::InvalidTime)
    }
}

fn from_bcd(value: u8) -> Option<u8> {
    let tens = value >> 4;
    let ones = value & 0x0F;
    (tens <= 9 && ones <= 9).then_some(tens * 10 + ones)
}

fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

fn read_register(bus: &SoftI2c, register: u8) -> Result<u8, Error> {
    let mut byte = [0u8; 1];
    read_registers_at(bus, register, &mut byte)?;
    Ok(byte[0])
}

/// Reads consecutive registers in one transaction, relying on the
/// RX8130CE's register-pointer auto-increment.
fn read_registers_at(bus: &SoftI2c, register: u8, buffer: &mut [u8]) -> Result<(), Error> {
    bus.write_read(ADDRESS, &[register], buffer)
        .map_err(classify)
}

fn write_register(bus: &SoftI2c, register: u8, value: u8) -> Result<(), Error> {
    write_registers(bus, register, &[value])
}

fn write_registers(bus: &SoftI2c, register: u8, values: &[u8]) -> Result<(), Error> {
    bus.transaction(|transaction| {
        transaction.start_write(ADDRESS)?;
        transaction.write_byte(register)?;
        transaction.write_all(values)
    })
    .map_err(classify)
}

/// A NACK on the address byte is the one bus error that means "no such
/// device"; everything else happened after the device answered.
fn classify(error: I2cError) -> Error {
    match error {
        I2cError::AddressNack => Error::NotFound,
        _ => Error::Bus,
    }
}
