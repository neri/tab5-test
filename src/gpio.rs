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
    Tab5KeyboardSda = 0,
    Tab5KeyboardScl = 1,
    C6SdioData3 = 8,
    C6SdioData2 = 9,
    C6SdioData1 = 10,
    C6SdioData0 = 11,
    C6SdioClock = 12,
    C6SdioCommand = 13,
    C6Reset = 15,
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

/// `GPIO_FUNCn_IN_SEL_CFG_REG`: which pad feeds peripheral input signal `n`.
fn matrix_in_register(signal: u32) -> usize {
    GPIO + 0x158 + signal as usize * 4
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

/// SDMMC slot 1 peripheral signal indices (`SD_CARD_*_2_*` in ESP-IDF's
/// `soc/esp32p4/gpio_sig_map.h`). Input and output share one index per
/// signal.
const SLOT1_SIGNAL_CLOCK: u32 = 0;
const SLOT1_SIGNAL_COMMAND: u32 = 1;
const SLOT1_SIGNAL_DATA0: u32 = 2;
const SLOT1_SIGNAL_DATA1: u32 = 3;
const SLOT1_SIGNAL_DATA2: u32 = 4;
const SLOT1_SIGNAL_DATA3: u32 = 5;
const SLOT1_SIGNAL_CARD_DETECT: u32 = 127;
const SLOT1_SIGNAL_CARD_INT: u32 = 129;
const SLOT1_SIGNAL_WRITE_PROTECT: u32 = 131;

/// `GPIO_FUNCn_IN_SEL` values that feed a peripheral input a constant level
/// instead of a pad (`GPIO_MATRIX_CONST_ZERO_INPUT`/`_ONE_INPUT`), with bit 7
/// set so the matrix is used at all.
const MATRIX_CONST_LOW: u32 = 0x3E | (1 << 7);
const MATRIX_CONST_HIGH: u32 = 0x3F | (1 << 7);

/// Routes the ESP32-C6's SDIO bus (SDMMC slot 1) onto GPIO8..13.
///
/// Unlike the microSD socket on slot 0, slot 1 has no IOMUX path on the
/// ESP32-P4 -- ESP-IDF's `soc/esp32p4/sdmmc_periph.c` leaves every
/// `sdmmc_slot_gpio_num` entry for slot 1 at `-1` and only fills in
/// `sdmmc_slot_gpio_sig` -- so each signal is routed through the GPIO
/// matrix instead: the peripheral's signal index goes into the pad's output
/// selector, and the pad number goes into the peripheral's input selector.
///
/// Output enable stays with the peripheral (`OEN_SEL` = 0) because CMD and
/// D0..D3 are bidirectional and the controller has to release them for the
/// card's response. As on slot 0, the bidirectional pads need their input
/// buffer (`fun_ie`) and pull-up (`fun_wpu`) enabled explicitly; CLK is
/// controller-driven only and needs neither.
pub fn configure_c6_sdio_pins() {
    const CONFIG_MASK: u32 = (1 << 7) | (1 << 8) | (1 << 9) | (0x3 << 10) | (0x7 << 12);
    const GPIO_FUNCTION_AND_DRIVE: u32 = (2 << 10) | (1 << 12);
    const INPUT_ENABLE_AND_PULLUP: u32 = (1 << 8) | (1 << 9);

    // D3 is deliberately missing here: see the end of this function.
    let bidirectional = [
        (Pin::C6SdioCommand, SLOT1_SIGNAL_COMMAND),
        (Pin::C6SdioData0, SLOT1_SIGNAL_DATA0),
        (Pin::C6SdioData1, SLOT1_SIGNAL_DATA1),
        (Pin::C6SdioData2, SLOT1_SIGNAL_DATA2),
    ];

    unsafe {
        modify(
            iomux_register(Pin::C6SdioClock),
            CONFIG_MASK,
            GPIO_FUNCTION_AND_DRIVE,
        );
        write(matrix_out_register(Pin::C6SdioClock), SLOT1_SIGNAL_CLOCK);

        for (pin, signal) in bidirectional {
            modify(
                iomux_register(pin),
                CONFIG_MASK,
                GPIO_FUNCTION_AND_DRIVE | INPUT_ENABLE_AND_PULLUP,
            );
            write(matrix_out_register(pin), signal);
            // Bit 7 keeps the signal on the GPIO matrix instead of bypassing
            // it to a fixed pad.
            write(matrix_in_register(signal), pin.number() | (1 << 7));
        }
    }

    // ESP-IDF routes peripheral outputs through the ROM's
    // `esp_rom_gpio_connect_out_signal`, which sets `GPIO_ENABLE` for the pad
    // as well as clearing `OEN_SEL`. Clearing `OEN_SEL` alone should be
    // enough by the register description (output enable then comes from the
    // peripheral), but the first bring-up attempt without this saw nothing on
    // the bus at all, so this follows what ESP-IDF actually does.
    enable_output(Pin::C6SdioClock);
    for (pin, _) in bidirectional {
        enable_output(pin);
    }

    // The controller's card-detect/card-interrupt/write-protect inputs are
    // not wired to pads here, so they are tied off the way ESP-IDF ties them
    // when a slot has no such pins: detect low (card present), interrupt
    // high, write protect low (not protected).
    unsafe {
        write(
            matrix_in_register(SLOT1_SIGNAL_CARD_DETECT),
            MATRIX_CONST_LOW,
        );
        write(matrix_in_register(SLOT1_SIGNAL_CARD_INT), MATRIX_CONST_HIGH);
        write(
            matrix_in_register(SLOT1_SIGNAL_WRITE_PROTECT),
            MATRIX_CONST_LOW,
        );
    }

    // D3 doubles as the SD/SPI mode select an SD or SDIO card samples while
    // it is being identified: it has to be held high until the bus is
    // switched to 4-bit, or the card falls into SPI mode and answers
    // nothing. ESP-IDF drives it as a plain GPIO output for exactly this
    // reason ("Force D3 high to make slave enter SD mode") and only hands it
    // to the peripheral from `sdmmc_host_set_bus_width`, which is what
    // `connect_c6_sdio_data3` below does.
    configure_push_pull_output(Pin::C6SdioData3);
    set_high(Pin::C6SdioData3);
}

/// Hands D3 over from the plain "hold it high" output that
/// [`configure_c6_sdio_pins`] leaves it in to the SDMMC peripheral. Call this
/// only once the card has been told to use a 4-bit bus.
pub fn connect_c6_sdio_data3() {
    unsafe {
        modify(
            iomux_register(Pin::C6SdioData3),
            (1 << 7) | (1 << 8) | (1 << 9) | (0x3 << 10) | (0x7 << 12),
            (1 << 8) | (1 << 9) | (2 << 10) | (1 << 12),
        );
        write(matrix_out_register(Pin::C6SdioData3), SLOT1_SIGNAL_DATA3);
        write(
            matrix_in_register(SLOT1_SIGNAL_DATA3),
            Pin::C6SdioData3.number() | (1 << 7),
        );
    }
    enable_output(Pin::C6SdioData3);
}

/// Configures a pin as a plain GPIO input for diagnostics, with the internal
/// pull-up either on or off, and returns its level. Used by `sdio.rs` to tell
/// "the C6 board is powered and its bus is pulled up" apart from "nothing is
/// driving these pads".
pub fn probe_input_level(pin: Pin, pull_up: bool) -> bool {
    unsafe {
        write(matrix_out_register(pin), 256 | (1 << 10));
        modify(
            iomux_register(pin),
            (1 << 7) | (1 << 8) | (1 << 9) | (0x3 << 10) | (0x7 << 12),
            (if pull_up { 1 << 8 } else { 0 }) | (1 << 9) | (2 << 10) | (1 << 12),
        );
    }
    disable_output(pin);
    // The pad's input buffer needs a moment to settle after the pull-up
    // change, especially with only a weak pull against the trace's
    // capacitance.
    crate::delay::delay_us(50);
    level(pin)
}

/// Configures the pin as a plain push-pull output driven from `GPIO_OUT_REG`
/// (matrix output source 256 = "GPIO", output enable taken from
/// `GPIO_ENABLE_REG`). Unlike [`configure_open_drain`] the pin keeps driving
/// both levels, which is what the C6's reset line needs.
pub fn configure_push_pull_output(pin: Pin) {
    unsafe {
        write(matrix_out_register(pin), 256 | (1 << 10));
        modify(
            iomux_register(pin),
            (1 << 7) | (1 << 8) | (1 << 9) | (0x3 << 10) | (0x7 << 12),
            (2 << 10) | (1 << 12),
        );
    }
    enable_output(pin);
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
