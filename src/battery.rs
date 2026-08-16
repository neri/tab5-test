//! Full-screen live battery monitor entered through the `battery` command.

use crate::framebuffer::{DoubleBuffer, BLACK, CYAN, GREEN, RED, WHITE, WIDTH, YELLOW};
use crate::ina226::{BatterySample, Ina226, InitError};
use crate::input::InputManager;
use crate::{interrupts, uart};

const UPDATE_INTERVAL_FRAMES: u32 = 57;
const PACK_EMPTY_MV: u32 = 6_000;
const PACK_FULL_MV: u32 = 8_230;

/// Opens the battery monitor.  Any managed keyboard key returns to the shell.
pub fn run(framebuffers: &mut DoubleBuffer, input: &mut InputManager) {
    let monitor = match Ina226::init() {
        Ok(monitor) => monitor,
        Err(error) => {
            show_unavailable(framebuffers, error);
            wait_for_key(input);
            return;
        }
    };
    uart::log(b"Battery: INA226 monitor ready\r\n");

    let mut sample = monitor.read_sample();
    draw_all(framebuffers, sample, monitor.address());
    let mut sequence = interrupts::frame_sequence();
    let mut last_update = sequence;
    let mut read_failure_reported = false;

    loop {
        if interrupts::dma_error() != 0 {
            uart::log(b"Battery: DMA interrupt error\r\n");
            return;
        }
        interrupts::wait_for_interrupt();
        let next_sequence = interrupts::frame_sequence();
        if next_sequence == sequence {
            continue;
        }
        sequence = next_sequence;
        // As in the axis diagnostic, do not service an empty USB root port
        // here: its debounce/reset work would make a live reading stutter.
        if input.poll_key().is_some() {
            return;
        }
        if sequence.wrapping_sub(last_update) < UPDATE_INTERVAL_FRAMES {
            continue;
        }
        last_update = sequence;
        match monitor.read_sample() {
            Some(next) => {
                sample = Some(next);
                read_failure_reported = false;
            }
            None if !read_failure_reported => {
                uart::log(b"Battery: INA226 read failed; retaining last reading\r\n");
                read_failure_reported = true;
            }
            None => {}
        }
        draw_all(framebuffers, sample, monitor.address());
    }
}

fn draw_all(framebuffers: &mut DoubleBuffer, sample: Option<BatterySample>, address: u8) {
    let displayed = interrupts::active_framebuffer();
    for index in [displayed ^ 1, displayed] {
        draw_scene(framebuffers, index, sample, address);
        if !framebuffers.flush(index) {
            uart::log(b"Battery: framebuffer flush failed\r\n");
            return;
        }
    }
}

fn draw_scene(
    framebuffers: &mut DoubleBuffer,
    index: usize,
    sample: Option<BatterySample>,
    address: u8,
) {
    framebuffers.fill(index, BLACK);
    framebuffers.draw_text(index, 32, 26, "BATTERY MONITOR", 4, CYAN, None);
    let mut device_text = Text::new();
    device_text.push_str("TAB5 / INA226 / I2C 0X");
    device_text.push_hex_byte(address);
    device_text.push_str(" / 5 MOHM");
    framebuffers.draw_text(index, 34, 70, device_text.as_str(), 2, WHITE, None);
    framebuffers.draw_line(index, 32, 100, WIDTH - 32, 100, CYAN);

    match sample {
        Some(sample) => draw_reading(framebuffers, index, sample),
        None => {
            framebuffers.draw_text(index, 380, 300, "WAITING FOR INA226 DATA", 3, YELLOW, None);
            draw_battery(framebuffers, index, 0, RED);
        }
    }
    framebuffers.draw_text(
        index,
        32,
        674,
        "LIVE UPDATE: 1 S       PRESS ANY KEY TO EXIT",
        2,
        WHITE,
        None,
    );
}

fn draw_reading(framebuffers: &mut DoubleBuffer, index: usize, sample: BatterySample) {
    let percent = voltage_percent(sample.bus_voltage_mv);
    let level_color = level_color(percent);
    draw_battery(framebuffers, index, percent, level_color);

    let mut text = Text::new();
    text.push_str("VOLTAGE ESTIMATE  ");
    text.push_u32(percent);
    text.push_str("%");
    framebuffers.draw_text(index, 68, 530, text.as_str(), 2, level_color, None);
    framebuffers.draw_text(index, 68, 562, "6.00V EMPTY  /  8.23V FULL", 2, WHITE, None);

    draw_value(
        framebuffers,
        index,
        510,
        148,
        "PACK VOLTAGE",
        voltage_text(sample.bus_voltage_mv).as_str(),
        CYAN,
    );
    draw_value(
        framebuffers,
        index,
        510,
        270,
        "CURRENT (IN+ TO IN-)",
        current_text(sample.current_ua).as_str(),
        level_color,
    );
    draw_value(
        framebuffers,
        index,
        510,
        392,
        "POWER (V X I)",
        power_text(sample.power_uw).as_str(),
        level_color,
    );
    draw_value(
        framebuffers,
        index,
        510,
        514,
        "SHUNT VOLTAGE",
        shunt_text(sample.shunt_voltage_uv).as_str(),
        WHITE,
    );
}

fn draw_battery(framebuffers: &mut DoubleBuffer, index: usize, percent: u32, color: u16) {
    const LEFT: usize = 74;
    const TOP: usize = 148;
    const BODY_WIDTH: usize = 310;
    const BODY_HEIGHT: usize = 330;
    const CAP_WIDTH: usize = 36;
    const CAP_HEIGHT: usize = 100;
    const INNER: usize = 16;

    framebuffers.stroke_rect(index, LEFT, TOP, BODY_WIDTH, BODY_HEIGHT, WHITE);
    framebuffers.stroke_rect(
        index,
        LEFT + BODY_WIDTH,
        TOP + (BODY_HEIGHT - CAP_HEIGHT) / 2,
        CAP_WIDTH,
        CAP_HEIGHT,
        WHITE,
    );
    let available_height = BODY_HEIGHT - INNER * 2;
    let filled = available_height * percent as usize / 100;
    if filled > 0 {
        framebuffers.fill_rect(
            index,
            LEFT + INNER,
            TOP + INNER + available_height - filled,
            BODY_WIDTH - INNER * 2,
            filled,
            color,
        );
    }
    let mut text = Text::new();
    text.push_u32(percent);
    text.push_str("%");
    framebuffers.draw_text(
        index,
        LEFT + 92,
        TOP + 136,
        text.as_str(),
        5,
        WHITE,
        Some(BLACK),
    );
}

fn draw_value(
    framebuffers: &mut DoubleBuffer,
    index: usize,
    x: usize,
    y: usize,
    label: &str,
    value: &str,
    value_color: u16,
) {
    framebuffers.draw_text(index, x, y, label, 2, WHITE, None);
    framebuffers.draw_text(index, x, y + 34, value, 5, value_color, None);
    framebuffers.draw_line(index, x, y + 102, WIDTH - 54, y + 102, 0x39E7);
}

fn voltage_percent(voltage_mv: u32) -> u32 {
    voltage_mv
        .saturating_sub(PACK_EMPTY_MV)
        .saturating_mul(100)
        .checked_div(PACK_FULL_MV - PACK_EMPTY_MV)
        .unwrap_or(0)
        .min(100)
}

fn level_color(percent: u32) -> u16 {
    if percent <= 15 {
        RED
    } else if percent <= 45 {
        YELLOW
    } else {
        GREEN
    }
}

fn show_unavailable(framebuffers: &mut DoubleBuffer, error: InitError) {
    let displayed = interrupts::active_framebuffer();
    for index in [displayed ^ 1, displayed] {
        framebuffers.fill(index, BLACK);
        framebuffers.draw_text(index, 32, 26, "BATTERY MONITOR", 4, CYAN, None);
        framebuffers.draw_text(index, 32, 160, error.message(), 3, RED, None);
        framebuffers.draw_text(index, 32, 220, "PRESS ANY KEY TO EXIT", 2, YELLOW, None);
        if !framebuffers.flush(index) {
            uart::log(b"Battery: unavailable-screen flush failed\r\n");
            return;
        }
    }
    uart::log(error.log_message());
}

fn wait_for_key(input: &mut InputManager) {
    let mut sequence = interrupts::frame_sequence();
    loop {
        interrupts::wait_for_interrupt();
        let next_sequence = interrupts::frame_sequence();
        if next_sequence == sequence {
            continue;
        }
        sequence = next_sequence;
        input.service();
        if input.poll_key().is_some() {
            return;
        }
    }
}

struct Text {
    bytes: [u8; 40],
    len: usize,
}

impl Text {
    const fn new() -> Self {
        Self {
            bytes: [0; 40],
            len: 0,
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.len < self.bytes.len() {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
    }

    fn push_str(&mut self, value: &str) {
        for &byte in value.as_bytes() {
            self.push_byte(byte);
        }
    }

    fn push_u32(&mut self, value: u32) {
        let mut digits = [0u8; 10];
        let mut count = 0;
        let mut remaining = value;
        if remaining == 0 {
            self.push_byte(b'0');
            return;
        }
        while remaining > 0 {
            digits[count] = b'0' + (remaining % 10) as u8;
            remaining /= 10;
            count += 1;
        }
        for &digit in digits[..count].iter().rev() {
            self.push_byte(digit);
        }
    }

    fn push_signed(&mut self, value: i32) {
        self.push_byte(if value < 0 { b'-' } else { b'+' });
        self.push_u32(value.unsigned_abs());
    }

    fn push_hex_byte(&mut self, value: u8) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        self.push_byte(HEX[(value >> 4) as usize]);
        self.push_byte(HEX[(value & 0x0F) as usize]);
    }

    fn as_str(&self) -> &str {
        // Every entry is an ASCII literal or decimal digit.
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }
}

fn voltage_text(mv: u32) -> Text {
    let mut text = Text::new();
    text.push_u32(mv / 1_000);
    text.push_byte(b'.');
    push_fraction(&mut text, mv % 1_000, 3);
    text.push_str(" V");
    text
}

fn current_text(ua: i32) -> Text {
    let mut text = Text::new();
    text.push_byte(if ua < 0 { b'-' } else { b'+' });
    let magnitude = ua.unsigned_abs();
    text.push_u32(magnitude / 1_000);
    text.push_byte(b'.');
    push_fraction(&mut text, magnitude % 1_000, 1);
    text.push_str(" MA");
    text
}

fn power_text(uw: i32) -> Text {
    let mut text = Text::new();
    text.push_byte(if uw < 0 { b'-' } else { b'+' });
    let magnitude = uw.unsigned_abs();
    text.push_u32(magnitude / 1_000_000);
    text.push_byte(b'.');
    push_fraction(&mut text, magnitude % 1_000_000, 2);
    text.push_str(" W");
    text
}

fn shunt_text(uv: i32) -> Text {
    let mut text = Text::new();
    text.push_signed(uv);
    text.push_str(" UV");
    text
}

fn push_fraction(text: &mut Text, remainder: u32, digits: u32) {
    let divisor = match digits {
        1 => 100,
        2 => 10_000,
        3 => 1,
        _ => 1,
    };
    let scaled = remainder / divisor;
    for position in (0..digits).rev() {
        let digit = (scaled / 10u32.pow(position)) % 10;
        text.push_byte(b'0' + digit as u8);
    }
}
