//! SDIO I/O-card activation for the ESP32-C6 on SDMMC card 1 (stage 1 of
//! `docs/WIFI_C6_PLAN.md`).
//!
//! The Tab5's Wi-Fi radio lives in an ESP32-C6 that is wired to the P4 as an
//! SDIO card, not as a memory card: GPIO11/10/9/8 = D0..D3, GPIO13 = CMD,
//! GPIO12 = CLK on SDMMC slot 1, GPIO15 = the C6's reset line, and E2.P0
//! (the second PI4IOE5V6408, I2C `0x44`) gates its power. Slot 1 has no
//! IOMUX path on the ESP32-P4, so the pads go through the GPIO matrix
//! (`gpio::configure_c6_sdio_pins`).
//!
//! Activation follows ESP-IDF v5.5.3's `components/sdmmc/sdmmc_io.c` and
//! `components/sdmmc/sdmmc_init.c` SDIO branch: CMD52 I/O reset, CMD0,
//! CMD5 (IO_SEND_OP_COND) instead of the memory card's ACMD41, CMD3, CMD7,
//! then CCCR writes for bus width, block size and function enable. All
//! register access here is CMD52 (one byte per command, no data phase);
//! the block transfers that the ESP-Hosted protocol needs come in a later
//! stage.
//!
//! `sdmmc.rs` owns the controller itself. Both cards share one CIU, so
//! activating the C6 drops the microSD's activation and vice versa.

use crate::delay::{delay_ms, delay_us};
use crate::gpio::{self, Pin};
use crate::sdmmc::{self, CARD_C6};
use crate::uart;
use crate::usb;

/// E2 (PI4IOE5V6408 at I2C `0x44`) output P0 gates the C6's power.
const C6_POWER_BIT: u8 = 0;

/// How long the C6 gets to boot after its reset line is released, matching
/// ESP-Hosted's `H_HOST_SDIO_RESET_DELAY_MS` default.
const RESET_BOOT_DELAY_MS: u32 = 1500;

/// CMD5 attempts, 100 ms apart, before declaring the C6 absent. ESP-Hosted's
/// host retries card init over the same kind of window.
const CARD_PROBE_RETRIES: u32 = 15;

// CMD52 (IO_RW_DIRECT) argument fields, from ESP-IDF's `sd_protocol_defs.h`.
const CMD52_WRITE: u32 = 1 << 31;
const CMD52_FUNCTION_SHIFT: u32 = 28;
const CMD52_READ_AFTER_WRITE: u32 = 1 << 27;
const CMD52_ADDRESS_SHIFT: u32 = 9;

// CMD53 (IO_RW_EXTENDED) argument fields, same source.
const CMD53_WRITE: u32 = 1 << 31;
const CMD53_FUNCTION_SHIFT: u32 = 28;
const CMD53_BLOCK_MODE: u32 = 1 << 27;
const CMD53_INCREMENT: u32 = 1 << 26;
const CMD53_ADDRESS_SHIFT: u32 = 9;

/// SDIO block size, set on both functions during activation.
pub const BLOCK_BYTES: usize = 512;

// Card Common Control Registers (function 0).
const CCCR_FUNCTION_ENABLE: u32 = 0x02;
const CCCR_FUNCTION_READY: u32 = 0x03;
const CCCR_INTERRUPT_ENABLE: u32 = 0x04;
const CCCR_CONTROL: u32 = 0x06;
const CCCR_CONTROL_RESET: u8 = 1 << 3;
const CCCR_BUS_WIDTH: u32 = 0x07;
const CCCR_BUS_WIDTH_4BIT: u8 = 2;
const CCCR_CARD_CAPABILITY: u32 = 0x08;
const CCCR_CAPABILITY_LOW_SPEED: u8 = 1 << 6;
const CCCR_CAPABILITY_4BIT_LOW_SPEED: u8 = 1 << 7;
const CCCR_CIS_POINTER: u32 = 0x09;
const CCCR_BLOCK_SIZE_LOW: u32 = 0x10;
const CCCR_HIGH_SPEED: u32 = 0x13;
const CCCR_HIGH_SPEED_SUPPORTED: u8 = 1 << 0;

/// Function Basic Register block stride: function `n`'s registers start at
/// `0x100 * n`, so function 1's block size lives at `0x110`/`0x111`.
const FBR_STRIDE: u32 = 0x100;

/// The Wi-Fi firmware runs as SDIO function 1.
const FUNCTION_WIFI: u32 = 1;

/// CMD5's R4 response fields.
const OCR_READY: u32 = 1 << 31;
const OCR_MEMORY_PRESENT: u32 = 1 << 27;
const OCR_VOLTAGE_WINDOW: u32 = 0x00FF_8000; // ~2.7-3.6V, as in `sdmmc.rs`

/// CIS tuple codes (PC Card 16 derived, `CISTPL_CODE_*` in ESP-IDF).
const CIS_TUPLE_MANUFACTURER: u8 = 0x20;
const CIS_TUPLE_END: u8 = 0xFF;

/// `CISTPL_MANFID` (manufacturer, product) pairs that mean "this is an ESP
/// SDIO slave".
///
/// The Tab5's C6 reports `0x0092`/`0x6666`, read straight out of its CIS at
/// `0x01005` (`20 04 92 00 66 66`), so that is the pair that matters here.
/// ESP-Hosted's `host/drivers/transport/sdio/sdio_reg.h` instead names
/// `ESP_VENDOR_ID = 0x6666` with `ESP_DEVICE_ID_1/2 = 0x2222/0x3333`; those
/// constants are never referenced by its own MCU host code and appear to
/// describe the Linux driver's view, so both spellings are accepted rather
/// than assuming one of them is wrong.
const ESP_SDIO_IDENTITIES: [(u16, u16); 3] = [(0x0092, 0x6666), (0x6666, 0x2222), (0x6666, 0x3333)];

fn is_known_esp_identity(manufacturer: u16, product: u16) -> bool {
    ESP_SDIO_IDENTITIES
        .iter()
        .any(|&(known_manufacturer, known_product)| {
            manufacturer == known_manufacturer && product == known_product
        })
}

pub struct SdioCard {
    pub rca: u16,
    /// Number of I/O functions the card reports in its CMD5 response.
    pub io_functions: u8,
    /// Whether the card also has an SD memory part (an ESP slave does not).
    pub memory_present: bool,
    pub bus_width_4bit: bool,
    /// Whether the card advertises High Speed timing. This stage stays at
    /// Default Speed regardless.
    pub high_speed_supported: bool,
    pub clock_khz: u32,
    /// `CISTPL_MANFID` from the card's CIS; see [`ESP_SDIO_IDENTITIES`].
    pub manufacturer: u16,
    pub product: u16,
}

impl SdioCard {
    /// Whether the CIS identifies this card as an ESP SDIO slave. This says
    /// nothing about *which* firmware the C6 runs -- the CIS comes from the
    /// chip, not from ESP-Hosted -- only that the right chip answered.
    pub fn is_esp_slave(&self) -> bool {
        is_known_esp_identity(self.manufacturer, self.product)
    }
}

/// Cuts the ESP32-C6's power, leaving it stopped rather than merely idle.
///
/// `startup::reboot` resets only the HP CPU core, so the C6 keeps its power
/// rail (the expander that gates it is a separate chip on I2C) and carries
/// its firmware state -- including an association with an access point --
/// straight across a reboot. It then disappears mid-association when the
/// next [`init`] pulses its reset line, which leaves the access point
/// holding an entry for a station that has silently gone away.
///
/// Called early in boot, this makes a warm start begin from the same
/// stopped co-processor a cold one does. It does not remove that stale
/// entry -- only a proper disconnect before the reset does -- but it starts
/// the access point's inactivity timer running at boot rather than at the
/// moment the operator asks to connect.
///
/// The reset line goes low before the rail does. A pin left driving high
/// into an unpowered chip feeds it through its protection diodes; low is
/// the same potential as the rail being removed, so nothing flows. It also
/// means the C6 is held in reset while its rail comes back up, which is the
/// order a power-on wants anyway.
pub fn power_down_c6() {
    gpio::configure_push_pull_output(Pin::C6Reset);
    gpio::set_low(Pin::C6Reset);
    if !usb::set_pi4ioe2_output_bit(C6_POWER_BIT, false) {
        uart::log(b"SDIO: could not remove ESP32-C6 power at boot\r\n");
    }
}

/// Powers, resets and activates the ESP32-C6 as an SDIO card, logging
/// progress and any failure reason over USB serial.
pub fn init() -> Option<SdioCard> {
    uart::log(b"SDIO: enabling ESP32-C6 power (E2.P0)\r\n");
    if !usb::set_pi4ioe2_output_bit(C6_POWER_BIT, true) {
        uart::log(b"SDIO: E2.P0 write was not acknowledged\r\n");
        return None;
    }
    log_power_expander_state();
    delay_ms(50);
    log_bus_idle_levels();

    gpio::configure_c6_sdio_pins();
    gpio::configure_push_pull_output(Pin::C6Reset);
    delay_ms(10);

    activate()
}

/// Logs E2's direction/high-impedance/output bytes. Bit 0 of each is the C6's
/// power line, so this shows whether the expander really took the enable.
fn log_power_expander_state() {
    for (name, register) in [
        (b"SDIO: E2 direction=".as_slice(), 0x03u8),
        (b"SDIO: E2 hi-z=".as_slice(), 0x07),
        (b"SDIO: E2 output=".as_slice(), 0x05),
        (b"SDIO: E2 input=".as_slice(), 0x0F),
    ] {
        match usb::pi4ioe2_register(register) {
            Some(value) => uart::log_hex(name, value as u32),
            None => uart::log(b"SDIO: E2 register read failed\r\n"),
        }
    }
}

/// Logs the idle level of each SDIO pad, first without and then with the
/// pad's internal pull-up. A powered C6 board holds CMD and D0..D3 high
/// through its own external pull-ups, so `no-pullup` bits that read low mean
/// the other end is unpowered (or the pads are not routed where we think).
fn log_bus_idle_levels() {
    let pins = [
        Pin::C6SdioCommand,
        Pin::C6SdioData0,
        Pin::C6SdioData1,
        Pin::C6SdioData2,
        Pin::C6SdioData3,
        Pin::C6SdioClock,
    ];

    let mut without = 0u32;
    let mut with = 0u32;
    for (index, pin) in pins.into_iter().enumerate() {
        if gpio::probe_input_level(pin, false) {
            without |= 1 << index;
        }
        if gpio::probe_input_level(pin, true) {
            with |= 1 << index;
        }
    }
    // Bit order: CMD, D0, D1, D2, D3, CLK.
    uart::log_hex(b"SDIO: pad levels without pull-up=", without);
    uart::log_hex(b"SDIO: pad levels with pull-up=", with);
}

/// Resets the C6 and walks the SDIO card activation sequence.
fn activate() -> Option<SdioCard> {
    reset_pulse();
    if !sdmmc::init_host(CARD_C6) {
        return None;
    }

    // CCCR I/O reset. The card resets itself in response, so a missing
    // answer is normal here (ESP-IDF's `sdmmc_io_reset` tolerates it too).
    let _ = rw_direct(true, 0, CCCR_CONTROL, CCCR_CONTROL_RESET);
    delay_ms(10);

    if sdmmc::send_command_on(CARD_C6, 0, 0, sdmmc::RESPONSE_NONE_WITH_INIT).is_err() {
        uart::log(b"SDIO: CMD0 (GO_IDLE_STATE) failed\r\n");
        return None;
    }
    delay_ms(1);

    // CMD5 with a zero argument only asks what the card supports; a card
    // that is not an I/O card does not answer at all. The C6 boots its own
    // firmware first, so retry for a while before giving up -- ESP-Hosted's
    // host allows 1.5 s of card-init retries on top of its post-reset delay.
    let mut probe = 0u32;
    let mut answered = false;
    for _ in 0..CARD_PROBE_RETRIES {
        if let Ok(response) = sdmmc::send_command_on(CARD_C6, 5, 0, sdmmc::RESPONSE_SHORT_NO_CRC) {
            probe = response[0];
            answered = true;
            break;
        }
        delay_ms(100);
    }
    if !answered {
        uart::log(b"SDIO: CMD5 (IO_SEND_OP_COND) got no answer; C6 not on the bus\r\n");
        sdmmc::log_diagnostics();
        return None;
    }
    uart::log_hex(b"SDIO: CMD5 probe OCR=", probe);

    let io_functions = ((probe >> 28) & 0x7) as u8;
    let memory_present = probe & OCR_MEMORY_PRESENT != 0;
    if io_functions == 0 {
        uart::log(b"SDIO: card reports no I/O functions\r\n");
        return None;
    }

    // Repeat CMD5 with the voltage window until the card leaves busy.
    let voltage_arg = probe & OCR_VOLTAGE_WINDOW;
    let mut ready = false;
    for _ in 0..100 {
        match sdmmc::send_command_on(CARD_C6, 5, voltage_arg, sdmmc::RESPONSE_SHORT_NO_CRC) {
            Ok(response) if response[0] & OCR_READY != 0 => {
                ready = true;
                break;
            }
            Ok(_) => {}
            Err(_) => {
                uart::log(b"SDIO: CMD5 (IO_SEND_OP_COND) failed\r\n");
                return None;
            }
        }
        delay_ms(10);
    }
    if !ready {
        uart::log(b"SDIO: C6 stayed busy after CMD5\r\n");
        return None;
    }

    let rca = match sdmmc::send_command_on(CARD_C6, 3, 0, sdmmc::RESPONSE_SHORT) {
        Ok(response) => (response[0] >> 16) as u16,
        Err(_) => {
            uart::log(b"SDIO: CMD3 (SEND_RELATIVE_ADDR) failed\r\n");
            return None;
        }
    };
    let rca_arg = (rca as u32) << 16;

    if sdmmc::send_command_on(CARD_C6, 7, rca_arg, sdmmc::RESPONSE_SHORT).is_err() {
        uart::log(b"SDIO: CMD7 (SELECT_CARD) failed\r\n");
        return None;
    }
    delay_us(100);

    let bus_width_4bit = set_bus_width_4bit();
    if !set_block_size_512() {
        return None;
    }
    if !enable_wifi_function() {
        return None;
    }
    enable_card_interrupts();
    let high_speed_supported = read_byte(0, CCCR_HIGH_SPEED)
        .map(|value| value & CCCR_HIGH_SPEED_SUPPORTED != 0)
        .unwrap_or(false);

    // 160 MHz / 8 = 20 MHz, the same Default Speed divider the microSD path
    // uses. High Speed is left for a later stage.
    if !sdmmc::set_clock(CARD_C6, 8, 0) {
        uart::log(b"SDIO: switch to 20 MHz failed\r\n");
        sdmmc::log_diagnostics();
        return None;
    }

    let (manufacturer, product) = read_manufacturer_id();

    // From here the microSD has to share the controller's input clock with
    // this card, which caps what the SD path may switch to.
    sdmmc::note_second_card_active();

    uart::log(b"SDIO: C6 activated\r\n");
    uart::log_hex(b"SDIO: RCA=", rca as u32);
    uart::log_hex(b"SDIO: manufacturer=", manufacturer as u32);
    uart::log_hex(b"SDIO: product=", product as u32);

    Some(SdioCard {
        rca,
        io_functions,
        memory_present,
        bus_width_4bit,
        high_speed_supported,
        clock_khz: 20_000,
        manufacturer,
        product,
    })
}

/// Pulses the C6's reset line. The line rests high and a low pulse resets,
/// confirmed on hardware and matching ESP-Hosted's default for SDIO slaves
/// (`Kconfig`'s "RESET: Active High": High->Low->High triggers reset).
fn reset_pulse() {
    gpio::set_high(Pin::C6Reset);
    delay_ms(10);
    gpio::set_low(Pin::C6Reset);
    delay_ms(10);
    gpio::set_high(Pin::C6Reset);
    // The C6 has to boot its own firmware before it answers on the bus.
    // ESP-Hosted's host waits `H_HOST_SDIO_RESET_DELAY_MS` (1500 ms by
    // default) here, and its card init retries on top of that.
    delay_ms(RESET_BOOT_DELAY_MS);
}

/// Switches both sides to a 4-bit bus if the card allows it. As on the
/// microSD path a failure is soft: the bus keeps working at 1 bit.
fn set_bus_width_4bit() -> bool {
    let Some(capability) = read_byte(0, CCCR_CARD_CAPABILITY) else {
        uart::log(b"SDIO: could not read CCCR card capability; staying 1-bit\r\n");
        return false;
    };
    // A low-speed card only supports 4-bit if it says so explicitly.
    if capability & CCCR_CAPABILITY_LOW_SPEED != 0
        && capability & CCCR_CAPABILITY_4BIT_LOW_SPEED == 0
    {
        uart::log(b"SDIO: card is low-speed without 4-bit support; staying 1-bit\r\n");
        return false;
    }
    if write_byte(0, CCCR_BUS_WIDTH, CCCR_BUS_WIDTH_4BIT).is_none() {
        uart::log(b"SDIO: CCCR bus width write failed; staying 1-bit\r\n");
        return false;
    }
    sdmmc::set_host_bus_width_4bit(CARD_C6);
    // Only now may D3 stop being the SD-mode strap and become a data line.
    gpio::connect_c6_sdio_data3();
    uart::log(b"SDIO: switched to 4-bit bus width\r\n");
    true
}

/// Sets function 0's and function 1's block size to 512 bytes, which is what
/// the ESP-Hosted transport's block transfers assume.
fn set_block_size_512() -> bool {
    for function in [0, FUNCTION_WIFI] {
        let base = CCCR_BLOCK_SIZE_LOW + FBR_STRIDE * function;
        // Block size registers live in function 0's address space even for
        // function 1 (they are part of that function's FBR block).
        if write_byte(0, base, 0x00).is_none() || write_byte(0, base + 1, 0x02).is_none() {
            uart::log_hex(b"SDIO: block size write failed for function ", function);
            return false;
        }
        match (read_byte(0, base), read_byte(0, base + 1)) {
            (Some(0x00), Some(0x02)) => {}
            _ => {
                uart::log_hex(
                    b"SDIO: block size readback mismatch for function ",
                    function,
                );
                return false;
            }
        }
    }
    true
}

/// Enables SDIO function 1 and waits for the card to report it ready.
fn enable_wifi_function() -> bool {
    let Some(enabled) = read_byte(0, CCCR_FUNCTION_ENABLE) else {
        uart::log(b"SDIO: could not read CCCR function enable\r\n");
        return false;
    };
    if write_byte(
        0,
        CCCR_FUNCTION_ENABLE,
        enabled | (1 << FUNCTION_WIFI) as u8,
    )
    .is_none()
    {
        uart::log(b"SDIO: CCCR function enable write failed\r\n");
        return false;
    }

    for _ in 0..100 {
        if let Some(ready) = read_byte(0, CCCR_FUNCTION_READY)
            && ready & (1 << FUNCTION_WIFI) as u8 != 0
        {
            return true;
        }
        delay_ms(10);
    }
    uart::log(b"SDIO: function 1 never became ready\r\n");
    false
}

/// Enables the card's interrupt master and function 1's interrupt. This
/// firmware polls rather than taking the SDIO interrupt, but the slave
/// firmware expects the same CCCR state ESP-Hosted's host sets up.
fn enable_card_interrupts() {
    if let Some(enabled) = read_byte(0, CCCR_INTERRUPT_ENABLE) {
        let _ = write_byte(
            0,
            CCCR_INTERRUPT_ENABLE,
            enabled | 0x01 | (1 << FUNCTION_WIFI) as u8,
        );
    }
}

/// Follows the CIS pointer in the CCCR and walks the tuple chain looking for
/// `CISTPL_MANFID`, returning `(manufacturer, product)`. Zeroes mean the
/// tuple was not found, which is itself a useful signal: an ESP slave always
/// has one.
fn read_manufacturer_id() -> (u16, u16) {
    let mut address = 0u32;
    for offset in 0..3 {
        let Some(byte) = read_byte(0, CCCR_CIS_POINTER + offset) else {
            uart::log(b"SDIO: could not read the CIS pointer\r\n");
            return (0, 0);
        };
        address |= (byte as u32) << (8 * offset);
    }
    let start = address;

    // A malformed chain must not spin forever; a real CIS has only a
    // handful of tuples before the manufacturer one.
    for _ in 0..32 {
        let Some(code) = read_byte(0, address) else {
            return (0, 0);
        };
        if code == CIS_TUPLE_END {
            return (0, 0);
        }
        let Some(length) = read_byte(0, address + 1) else {
            return (0, 0);
        };
        if code == CIS_TUPLE_MANUFACTURER && length >= 4 {
            let mut bytes = [0u8; 4];
            for (index, slot) in bytes.iter_mut().enumerate() {
                match read_byte(0, address + 2 + index as u32) {
                    Some(byte) => *slot = byte,
                    None => return (0, 0),
                }
            }
            let manufacturer = u16::from_le_bytes([bytes[0], bytes[1]]);
            let product = u16::from_le_bytes([bytes[2], bytes[3]]);
            if !is_known_esp_identity(manufacturer, product) {
                uart::log(b"SDIO: unrecognized CIS identifiers, dumping the CIS\r\n");
                uart::log_hex(b"SDIO: CIS pointer=", start);
                dump_cis(start);
            }
            return (manufacturer, product);
        }
        address += 2 + length as u32;
    }

    uart::log(b"SDIO: no CISTPL_MANFID tuple found, dumping the CIS\r\n");
    uart::log_hex(b"SDIO: CIS pointer=", start);
    dump_cis(start);
    (0, 0)
}

/// Dumps the start of the CIS to the UART log in the same `hexdump -C` style
/// `sdmmc::dump_block` uses, so the tuple chain can be checked by hand. The
/// identifiers a card reports here are the only way to tell which firmware
/// the C6 is running, and the field order in `CISTPL_MANFID` is worth being
/// able to verify rather than infer.
fn dump_cis(start: u32) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    const DUMP_BYTES: u32 = 64;

    let mut bytes = [0u8; DUMP_BYTES as usize];
    for (index, slot) in bytes.iter_mut().enumerate() {
        match read_byte(0, start + index as u32) {
            Some(byte) => *slot = byte,
            None => {
                uart::log(b"SDIO: CIS read failed mid-dump\r\n");
                return;
            }
        }
    }

    for (row_index, row) in bytes.chunks(16).enumerate() {
        let offset = start + (row_index * 16) as u32;
        let mut line = [0u8; 5 + 2 + 16 * 3 + 2];
        let mut pos = 0;
        for shift in (0..5).rev() {
            line[pos] = HEX[((offset >> (shift * 4)) & 0xF) as usize];
            pos += 1;
        }
        line[pos] = b':';
        line[pos + 1] = b' ';
        pos += 2;
        for &byte in row {
            line[pos] = HEX[(byte >> 4) as usize];
            line[pos + 1] = HEX[(byte & 0xF) as usize];
            line[pos + 2] = b' ';
            pos += 3;
        }
        line[pos] = b'\r';
        line[pos + 1] = b'\n';
        uart::log(&line[..pos + 2]);
    }
}

/// Reads one byte from an SDIO function's address space (CMD52).
pub fn read_byte(function: u32, address: u32) -> Option<u8> {
    rw_direct(false, function, address, 0)
}

/// Writes one byte into an SDIO function's address space (CMD52) and returns
/// the value the card read back.
pub fn write_byte(function: u32, address: u32, value: u8) -> Option<u8> {
    rw_direct(true, function, address, value)
}

/// Asks whether an SDIO card is on the bus at all (CMD5 with a zero
/// argument, which any I/O card answers even before it is selected).
///
/// Used to tell "the C6 stopped answering because it rebooted" from "the C6
/// is gone": a card that reset is back in the idle state, where it still
/// answers CMD5 but no longer answers the CMD52s that need it selected.
pub fn probe_present() -> bool {
    match sdmmc::send_command_on(CARD_C6, 5, 0, sdmmc::RESPONSE_SHORT_NO_CRC) {
        Ok(response) => {
            uart::log_hex(b"SDIO: C6 still answers CMD5, OCR=", response[0]);
            true
        }
        Err(_) => {
            uart::log(b"SDIO: C6 does not answer CMD5 either\r\n");
            false
        }
    }
}

/// CMD53 (IO_RW_EXTENDED) block-mode transfer into or out of `function`'s
/// address space, starting at `address` and incrementing.
///
/// `buffer` must be a nonzero multiple of [`BLOCK_BYTES`] long and should be
/// 64-byte aligned: it is handed straight to the IDMAC, and the cache
/// maintenance around the transfer works in cache lines.
pub fn transfer_blocks(function: u32, address: u32, buffer: &mut [u8], is_write: bool) -> bool {
    if buffer.is_empty() || buffer.len() % BLOCK_BYTES != 0 {
        uart::log(b"SDIO: CMD53 length must be a nonzero multiple of 512 bytes\r\n");
        return false;
    }
    let blocks = (buffer.len() / BLOCK_BYTES) as u32;

    let mut arg = ((function & 0x7) << CMD53_FUNCTION_SHIFT)
        | CMD53_BLOCK_MODE
        | CMD53_INCREMENT
        | ((address & 0x1_FFFF) << CMD53_ADDRESS_SHIFT)
        | (blocks & 0x1FF);
    if is_write {
        arg |= CMD53_WRITE;
    }

    // CMD53 has no stop command of its own: the block count in the argument
    // ends the transfer, so no auto-stop here (unlike SD's CMD18/CMD25).
    sdmmc::data_transfer_on(
        CARD_C6,
        53,
        arg,
        BLOCK_BYTES as u32,
        buffer,
        is_write,
        false,
        b"SDIO: CMD53",
    )
}

/// CMD52 (IO_RW_DIRECT): a single-byte register access with no data phase.
/// Writes always use the "read after write" flag, so the R5 response carries
/// the value the card ended up with.
fn rw_direct(write: bool, function: u32, address: u32, value: u8) -> Option<u8> {
    let mut arg = ((function & 0x7) << CMD52_FUNCTION_SHIFT)
        | ((address & 0x1_FFFF) << CMD52_ADDRESS_SHIFT)
        | value as u32;
    if write {
        arg |= CMD52_WRITE | CMD52_READ_AFTER_WRITE;
    }

    match sdmmc::send_command_on(CARD_C6, 52, arg, sdmmc::RESPONSE_SHORT) {
        Ok(response) => Some((response[0] & 0xFF) as u8),
        Err(_) => None,
    }
}
