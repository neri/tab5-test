//! Battery details rendering.

use super::theme;
use crate::framebuffer::{Framebuffer, WIDTH};
use crate::ina226::BatterySample;
fn draw_reading(framebuffer: &mut Framebuffer, sample: BatterySample) {
    let percent = voltage_percent(sample.bus_voltage_mv);
    let level_color = level_color(percent);
    draw_battery(framebuffer, percent, level_color);

    let mut text = Text::new();
    text.push_str("Voltage estimate  ");
    text.push_u32(percent);
    text.push_str("%");
    framebuffer.draw_gui_text(68, 530, text.as_str(), 1, level_color, None);
    framebuffer.draw_gui_text(68, 562, "6.00 V empty  /  8.23 V full", 1, theme::TEXT, None);

    draw_value(
        framebuffer,
        510,
        148,
        "Pack voltage",
        voltage_text(sample.bus_voltage_mv).as_str(),
        theme::ACCENT,
    );
    draw_value(
        framebuffer,
        510,
        270,
        "Current (IN+ to IN-)",
        current_text(sample.current_ua).as_str(),
        level_color,
    );
    draw_value(
        framebuffer,
        510,
        392,
        "Power (V x I)",
        power_text(sample.power_uw).as_str(),
        level_color,
    );
    draw_value(
        framebuffer,
        510,
        514,
        "Shunt voltage",
        shunt_text(sample.shunt_voltage_uv).as_str(),
        theme::TEXT,
    );
}

fn draw_battery(framebuffer: &mut Framebuffer, percent: u32, color: u16) {
    const LEFT: usize = 74;
    const TOP: usize = 148;
    const BODY_WIDTH: usize = 310;
    const BODY_HEIGHT: usize = 330;
    const CAP_WIDTH: usize = 36;
    const CAP_HEIGHT: usize = 100;
    const INNER: usize = 16;

    framebuffer.stroke_rect(LEFT, TOP, BODY_WIDTH, BODY_HEIGHT, theme::TEXT);
    framebuffer.stroke_rect(
        LEFT + BODY_WIDTH,
        TOP + (BODY_HEIGHT - CAP_HEIGHT) / 2,
        CAP_WIDTH,
        CAP_HEIGHT,
        theme::TEXT,
    );
    let available_height = BODY_HEIGHT - INNER * 2;
    let filled = available_height * percent as usize / 100;
    if filled > 0 {
        framebuffer.fill_rect(
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
    // Centred in the battery body rather than offset by a fixed amount: the
    // reading is one to four characters wide, and the old offset was only
    // right for the widest of them.
    let width = crate::font::ui_text_width(text.as_str(), crate::font::UiTextStyle::HEADING);
    framebuffer.draw_gui_text(
        LEFT + (BODY_WIDTH - width) / 2,
        TOP + (BODY_HEIGHT - crate::font::HEIGHT * 2) / 2,
        text.as_str(),
        2,
        theme::TEXT,
        Some(theme::BACKGROUND),
    );
}

fn draw_value(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    label: &str,
    value: &str,
    value_color: u16,
) {
    framebuffer.draw_gui_text(x, y, label, 1, theme::TEXT, None);
    framebuffer.draw_gui_text(x, y + 34, value, 2, value_color, None);
    framebuffer.draw_line(x, y + 102, WIDTH - 54, y + 102, theme::BORDER);
}

fn voltage_percent(mv: u32) -> u32 {
    tab5_system_ui::voltage_percent(mv)
}

fn level_color(percent: u32) -> u16 {
    if percent <= 15 {
        theme::ERROR
    } else if percent <= 45 {
        theme::WARNING
    } else {
        theme::SUCCESS
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
    text.push_str(" mA");
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
    text.push_str(" uV");
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

/// Draws only content. The host owns the bar and the writeback boundary.
pub fn draw_details(fb: &mut Framebuffer, monitor: &super::battery_monitor::BatteryMonitor) {
    fb.fill_rect(0, 48, WIDTH, 672, theme::BACKGROUND);
    if let Some(sample) = monitor.sample {
        draw_reading(fb, sample);
    } else {
        fb.draw_gui_text(
            32,
            300,
            monitor.error.unwrap_or("Waiting for INA226 data"),
            2,
            theme::TEXT,
            None,
        );
    }
    fb.draw_gui_text(
        32,
        674,
        "Voltage estimate only / Esc Back / F3 Launcher",
        1,
        theme::TEXT,
        None,
    );
}
