//! Pin-level API for the ESP32-P4's GPIO/IO_MUX registers.
//!
//! GPIO0..31 use each register's first word; GPIO32 and above use the second
//! word (the register name's `1`-suffixed variant). `lcd.rs`'s PI4IOE1
//! control and `cardkb.rs`'s CardKB control both implement software I2C on
//! top of the open-drain-style bit-level GPIO operations provided here.
//!
//! `read`/`write`/`modify` take an arbitrary address, so the compiler cannot
//! verify it is a valid, mapped register; they are `unsafe fn` for that
//! reason. Every public function in this file only ever passes fixed,
//! known-valid GPIO/IO_MUX offsets, so each one upholds that invariant by
//! construction and stays a safe `fn`, wrapping its own register access in a
//! single `unsafe` block.

const GPIO: usize = 0x500E_0000;
const IO_MUX: usize = 0x500E_1000;

/// GPIOs wired to peripherals that this firmware controls directly.
///
/// Keeping the numeric GPIO assignment here prevents callers from passing an
/// arbitrary number to the pin-level register API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Pin {
    Backlight = 22,
    BoardI2cSda = 31,
    BoardI2cScl = 32,
    SdmmcData0 = 39,
    SdmmcData1 = 40,
    SdmmcData2 = 41,
    SdmmcData3 = 42,
    SdmmcClock = 43,
    SdmmcCommand = 44,
    CardKbSda = 53,
    CardKbScl = 54,
}

impl Pin {
    const fn number(self) -> u32 {
        self as u32
    }
}

fn matrix_out_register(pin: Pin) -> usize {
    GPIO + 0x558 + pin.number() as usize * 4
}

fn iomux_register(pin: Pin) -> usize {
    IO_MUX + 0x04 + pin.number() as usize * 4
}

/// Fixes the GPIO matrix output to this pin's GPIO function and configures
/// IOMUX for input-enabled, pull-up-enabled, medium drive strength. This is
/// the setup used for open-drain-style bit-level GPIO control such as
/// software I2C; the pin ends up released (floating, left to the external
/// pull-up) once configured.
pub fn configure_open_drain(pin: Pin) {
    unsafe {
        // GPIO matrix output, output-enable controlled by GPIO_ENABLE.
        write(matrix_out_register(pin), 256 | (1 << 10));
        // GPIO function, input enabled, pull-up enabled, medium drive strength.
        modify(
            iomux_register(pin),
            (1 << 7) | (1 << 8) | (1 << 9) | (0x3 << 10) | (0x7 << 12),
            (1 << 8) | (1 << 9) | (2 << 10) | (1 << 12),
        );
    }
    drive_low(pin);
    release(pin);
}

/// Routes the Tab5 microSD socket's GPIO39..44 pads to the SDMMC peripheral.
///
/// CMD and D0..D3 need their input buffers and pull-ups enabled because they
/// are bidirectional; CLK is controller-driven only and needs neither.
pub fn configure_sdmmc_4bit_pins() {
    const FUNCTION_MASK: u32 = 0x7 << 12;
    const SDMMC_FUNCTION: u32 = 0 << 12;
    const INPUT_ENABLE_AND_PULLUP: u32 = (1 << 8) | (1 << 9);

    unsafe {
        for pin in [
            Pin::SdmmcClock,
            Pin::SdmmcCommand,
            Pin::SdmmcData0,
            Pin::SdmmcData1,
            Pin::SdmmcData2,
            Pin::SdmmcData3,
        ] {
            modify(iomux_register(pin), FUNCTION_MASK, SDMMC_FUNCTION);
        }
        for pin in [
            Pin::SdmmcCommand,
            Pin::SdmmcData0,
            Pin::SdmmcData1,
            Pin::SdmmcData2,
            Pin::SdmmcData3,
        ] {
            modify(
                iomux_register(pin),
                INPUT_ENABLE_AND_PULLUP,
                INPUT_ENABLE_AND_PULLUP,
            );
        }
    }
}

/// Drives the pin low (sets the output value low and enables the output).
pub fn drive_low(pin: Pin) {
    set_low(pin);
    enable_output(pin);
}

/// Disables the pin's output (the open-drain "release"; it will not go high
/// unless an external pull-up pulls it there).
pub fn release(pin: Pin) {
    disable_output(pin);
}

pub fn set_low(pin: Pin) {
    let pin = pin.number();
    unsafe {
        if pin < 32 {
            write(GPIO + 0x0C, 1u32 << pin); // GPIO_OUT_W1TC
        } else {
            write(GPIO + 0x18, 1u32 << (pin - 32)); // GPIO_OUT1_W1TC
        }
    }
}

/// Drives a pin<32 push-pull output high. Software-I2C open-drain pins
/// (including pin>=32) always go high through a release instead, so this is
/// for plain outputs like the backlight only; the pin>=32 register is not
/// implemented.
pub fn set_high(pin: Pin) {
    let pin = pin.number();
    unsafe {
        write(GPIO + 0x08, 1u32 << pin); // GPIO_OUT_W1TS
    }
}

pub fn enable_output(pin: Pin) {
    let pin = pin.number();
    unsafe {
        if pin < 32 {
            write(GPIO + 0x24, 1u32 << pin); // GPIO_ENABLE_W1TS
        } else {
            write(GPIO + 0x30, 1u32 << (pin - 32)); // GPIO_ENABLE1_W1TS
        }
    }
}

pub fn disable_output(pin: Pin) {
    let pin = pin.number();
    unsafe {
        if pin < 32 {
            write(GPIO + 0x28, 1u32 << pin); // GPIO_ENABLE_W1TC
        } else {
            write(GPIO + 0x34, 1u32 << (pin - 32)); // GPIO_ENABLE1_W1TC
        }
    }
}

pub fn level(pin: Pin) -> bool {
    let pin = pin.number();
    unsafe {
        if pin < 32 {
            read(GPIO + 0x3C) & (1u32 << pin) != 0 // GPIO_IN
        } else {
            read(GPIO + 0x40) & (1u32 << (pin - 32)) != 0 // GPIO_IN1
        }
    }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn write(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn modify(address: usize, mask: u32, value: u32) {
    unsafe {
        write(address, (read(address) & !mask) | (value & mask));
    }
}
