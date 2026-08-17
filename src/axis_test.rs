//! Full-screen accelerometer diagnostic entered through `axistest`.
//!
//! Device communication is delegated to `crate::bmi270`; this module owns
//! only the test's rendering, input, and motion simulation.

use crate::bmi270::{Bmi270, InitError, MotionSample};
use crate::framebuffer::{BLACK, CYAN, Framebuffer, GREEN, HEIGHT, RED, WHITE, WIDTH, YELLOW};
use crate::input::InputManager;
use crate::{interrupts, uart};

// Bosch BMI270 maximum-FIFO configuration image, from BMI270 SensorAPI
// v2.86.1. It is the compact 328-byte image intended for raw data and FIFO
// access, rather than the 8 KiB feature-engine image.
//
// Copyright (c) 2023 Bosch Sensortec GmbH. SPDX-License-Identifier: BSD-3-Clause
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the copyright notice, these
// conditions, and the BSD-3-Clause warranty disclaimer are retained. Neither
// the copyright holder's name nor contributors' names may endorse derived
// products without specific prior written permission.
const BMI270_MINIMUM_FIRMWARE: [u8; 328] = [
    0xc8, 0x2e, 0x00, 0x2e, 0x80, 0x2e, 0x1a, 0x00, 0xc8, 0x2e, 0x00, 0x2e, 0xc8, 0x2e, 0x00, 0x2e,
    0xc8, 0x2e, 0x00, 0x2e, 0xc8, 0x2e, 0x00, 0x2e, 0xc8, 0x2e, 0x00, 0x2e, 0xc8, 0x2e, 0x00, 0x2e,
    0x90, 0x32, 0x21, 0x2e, 0x59, 0xf5, 0x10, 0x30, 0x21, 0x2e, 0x6a, 0xf5, 0x1a, 0x24, 0x22, 0x00,
    0x80, 0x2e, 0x3b, 0x00, 0xc8, 0x2e, 0x44, 0x47, 0x22, 0x00, 0x37, 0x00, 0xa4, 0x00, 0xff, 0x0f,
    0xd1, 0x00, 0x07, 0xad, 0x80, 0x2e, 0x00, 0xc1, 0x80, 0x2e, 0x00, 0xc1, 0x80, 0x2e, 0x00, 0xc1,
    0x80, 0x2e, 0x00, 0xc1, 0x80, 0x2e, 0x00, 0xc1, 0x80, 0x2e, 0x00, 0xc1, 0x80, 0x2e, 0x00, 0xc1,
    0x80, 0x2e, 0x00, 0xc1, 0x80, 0x2e, 0x00, 0xc1, 0x80, 0x2e, 0x00, 0xc1, 0x80, 0x2e, 0x00, 0xc1,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x24, 0xfc, 0xf5, 0x80, 0x30, 0x40, 0x42, 0x50, 0x50,
    0x00, 0x30, 0x12, 0x24, 0xeb, 0x00, 0x03, 0x30, 0x00, 0x2e, 0xc1, 0x86, 0x5a, 0x0e, 0xfb, 0x2f,
    0x21, 0x2e, 0xfc, 0xf5, 0x13, 0x24, 0x63, 0xf5, 0xe0, 0x3c, 0x48, 0x00, 0x22, 0x30, 0xf7, 0x80,
    0xc2, 0x42, 0xe1, 0x7f, 0x3a, 0x25, 0xfc, 0x86, 0xf0, 0x7f, 0x41, 0x33, 0x98, 0x2e, 0xc2, 0xc4,
    0xd6, 0x6f, 0xf1, 0x30, 0xf1, 0x08, 0xc4, 0x6f, 0x11, 0x24, 0xff, 0x03, 0x12, 0x24, 0x00, 0xfc,
    0x61, 0x09, 0xa2, 0x08, 0x36, 0xbe, 0x2a, 0xb9, 0x13, 0x24, 0x38, 0x00, 0x64, 0xbb, 0xd1, 0xbe,
    0x94, 0x0a, 0x71, 0x08, 0xd5, 0x42, 0x21, 0xbd, 0x91, 0xbc, 0xd2, 0x42, 0xc1, 0x42, 0x00, 0xb2,
    0xfe, 0x82, 0x05, 0x2f, 0x50, 0x30, 0x21, 0x2e, 0x21, 0xf2, 0x00, 0x2e, 0x00, 0x2e, 0xd0, 0x2e,
    0xf0, 0x6f, 0x02, 0x30, 0x02, 0x42, 0x20, 0x26, 0xe0, 0x6f, 0x02, 0x31, 0x03, 0x40, 0x9a, 0x0a,
    0x02, 0x42, 0xf0, 0x37, 0x05, 0x2e, 0x5e, 0xf7, 0x10, 0x08, 0x12, 0x24, 0x1e, 0xf2, 0x80, 0x42,
    0x83, 0x84, 0xf1, 0x7f, 0x0a, 0x25, 0x13, 0x30, 0x83, 0x42, 0x3b, 0x82, 0xf0, 0x6f, 0x00, 0x2e,
    0x00, 0x2e, 0xd0, 0x2e, 0x12, 0x40, 0x52, 0x42, 0x00, 0x2e, 0x12, 0x40, 0x52, 0x42, 0x3e, 0x84,
    0x00, 0x40, 0x40, 0x42, 0x7e, 0x82, 0xe1, 0x7f, 0xf2, 0x7f, 0x98, 0x2e, 0x6a, 0xd6, 0x21, 0x30,
    0x23, 0x2e, 0x61, 0xf5, 0xeb, 0x2c, 0xe1, 0x6f,
];

const BALL_RADIUS: i32 = 32;
const ARENA_LEFT: i32 = 32;
const ARENA_TOP: i32 = 144;
const ARENA_RIGHT: i32 = WIDTH as i32 - 32;
const ARENA_BOTTOM: i32 = HEIGHT as i32 - 32;
const FIXED: i32 = 256;
const HUD_TOP: usize = 40;
const HUD_HEIGHT: usize = 88;
const HUD_INTERVAL_FRAMES: u8 = 4;

/// Opens the tilt-controlled ball test. Any managed keyboard key exits.
pub fn run(framebuffer: &mut Framebuffer, input: &mut InputManager) {
    let sensor = match Bmi270::init(&BMI270_MINIMUM_FIRMWARE) {
        Ok(sensor) => sensor,
        Err(error) => {
            show_unavailable(framebuffer, error);
            wait_for_key(input);
            return;
        }
    };

    uart::log(b"Axis test: BMI270 ready; tilt the Tab5 to roll the ball\r\n");
    let mut sample = sensor.read_motion().unwrap_or(MotionSample::ZERO);
    draw_scene(framebuffer);
    draw_hud(framebuffer, sample);
    draw_ball(framebuffer, WIDTH as i32 / 2, HEIGHT as i32 / 2);
    if !framebuffer.flush() {
        uart::log(b"Axis test: initial flush failed\r\n");
        return;
    }

    let mut x = WIDTH as i32 * FIXED / 2;
    let mut y = HEIGHT as i32 * FIXED / 2;
    let mut velocity_x = 0i32;
    let mut velocity_y = 0i32;
    let mut last_x = x;
    let mut last_y = y;
    let mut sequence = interrupts::frame_sequence();
    let mut read_failure_reported = false;
    let mut hud_frames = 0;
    let mut shown_sample = sample;

    loop {
        if interrupts::dma_error() != 0 {
            uart::log(b"Axis test: DMA interrupt error\r\n");
            return;
        }
        interrupts::wait_for_interrupt();
        let next_sequence = interrupts::frame_sequence();
        if next_sequence == sequence {
            continue;
        }
        sequence = next_sequence;
        // `InputManager::service` periodically probes an empty USB-A root
        // port, which performs a blocking reset/debounce.  A motion test
        // must not pause for that background maintenance; existing keyboards
        // remain readable through `poll_key`, and normal maintenance resumes
        // on return to the console.
        if input.poll_key().is_some() {
            return;
        }

        match sensor.read_motion() {
            Some(next_sample) => {
                read_failure_reported = false;
                sample = next_sample;
            }
            None => {
                if !read_failure_reported {
                    uart::log(b"Axis test: BMI270 read failed; coasting\r\n");
                    read_failure_reported = true;
                }
                // A momentary I2C error should not drop out of the test.
                // Let the ball coast until a fresh sample arrives.
            }
        }
        let [raw_x, raw_y, _raw_z] = sample.acceleration;

        // The board is mounted portrait internally while the framebuffer is
        // landscape CW. Accelerometers report the support force, which is
        // opposite to downhill gravity, so negate the rotated axes here.
        velocity_x -= dead_zone(raw_y) / 100;
        velocity_y += dead_zone(raw_x) / 100;
        // Light damping makes the result feel like a ball rolling over a mat.
        velocity_x = velocity_x * 248 / 256;
        velocity_y = velocity_y * 248 / 256;
        x += velocity_x;
        y += velocity_y;
        bounce(
            &mut x,
            &mut velocity_x,
            ARENA_LEFT + BALL_RADIUS,
            ARENA_RIGHT - BALL_RADIUS,
        );
        bounce(
            &mut y,
            &mut velocity_y,
            ARENA_TOP + BALL_RADIUS,
            ARENA_BOTTOM - BALL_RADIUS,
        );

        erase_ball_path(
            framebuffer,
            last_x / FIXED,
            last_y / FIXED,
            x / FIXED,
            y / FIXED,
        );
        draw_ball(framebuffer, x / FIXED, y / FIXED);
        if !framebuffer.flush_rect(
            ball_dirty_left(last_x, x),
            ball_dirty_top(last_y, y),
            ball_dirty_width(last_x, x),
            ball_dirty_height(last_y, y),
        ) {
            uart::log(b"Axis test: flush failed\r\n");
            return;
        }
        if hud_frames == 0 {
            if hud_changed(sample, shown_sample) {
                draw_hud(framebuffer, sample);
                if !framebuffer.flush_rect(0, HUD_TOP, WIDTH, HUD_HEIGHT) {
                    uart::log(b"Axis test: HUD flush failed\r\n");
                    return;
                }
                shown_sample = sample;
            }
            hud_frames = HUD_INTERVAL_FRAMES;
        } else {
            hud_frames -= 1;
        }
        last_x = x;
        last_y = y;
    }
}

fn dead_zone(value: i16) -> i32 {
    const DEAD_ZONE: i32 = 250;
    let value = value as i32;
    if value.abs() <= DEAD_ZONE {
        0
    } else {
        value - DEAD_ZONE * value.signum()
    }
}

fn bounce(position: &mut i32, velocity: &mut i32, low: i32, high: i32) {
    let low = low * FIXED;
    let high = high * FIXED;
    if *position < low {
        *position = low;
        *velocity = -*velocity * 3 / 5;
    } else if *position > high {
        *position = high;
        *velocity = -*velocity * 3 / 5;
    }
}

fn draw_scene(framebuffer: &mut Framebuffer) {
    framebuffer.fill(BLACK);
    framebuffer.draw_text(16, 12, "AXIS SENSOR TEST", 3, CYAN, None);
    framebuffer.draw_text(
        16,
        40,
        "TILT THE TAB5 - THE BALL ROLLS DOWNHILL",
        2,
        WHITE,
        None,
    );
    framebuffer.stroke_rect(
        ARENA_LEFT as usize,
        ARENA_TOP as usize,
        (ARENA_RIGHT - ARENA_LEFT) as usize,
        (ARENA_BOTTOM - ARENA_TOP) as usize,
        CYAN,
    );
}

fn draw_hud(framebuffer: &mut Framebuffer, sample: MotionSample) {
    framebuffer.fill_rect(0, HUD_TOP, WIDTH, HUD_HEIGHT, BLACK);
    for (row, axis) in [b'X', b'Y', b'Z'].into_iter().enumerate() {
        let mut acceleration = AxisLine::new();
        acceleration.push_str("ACC ");
        acceleration.push_byte(axis);
        acceleration.push_str(": ");
        acceleration.push_g(sample.acceleration[row]);
        framebuffer.draw_text(16, 64 + row * 20, acceleration.as_str(), 2, CYAN, None);

        let mut gyroscope = AxisLine::new();
        gyroscope.push_str("GYR ");
        gyroscope.push_byte(axis);
        gyroscope.push_str(": ");
        gyroscope.push_dps(sample.gyroscope[row]);
        framebuffer.draw_text(300, 64 + row * 20, gyroscope.as_str(), 2, CYAN, None);
    }

    let level = is_level(sample.acceleration[0], sample.acceleration[1]);
    framebuffer.draw_text(
        600,
        84,
        if level { "HORIZONTAL" } else { "TILTED" },
        3,
        if level { GREEN } else { YELLOW },
        None,
    );
    draw_level(
        framebuffer,
        sample.acceleration[0],
        sample.acceleration[1],
        level,
    );
}

fn is_level(raw_x: i16, raw_y: i16) -> bool {
    // At +/-4 g, 700 LSB is about 0.085 g (roughly a five-degree tilt).
    (raw_x as i32).abs() + (raw_y as i32).abs() <= 700
}

fn hud_changed(current: MotionSample, shown: MotionSample) -> bool {
    if is_level(current.acceleration[0], current.acceleration[1])
        != is_level(shown.acceleration[0], shown.acceleration[1])
    {
        return true;
    }
    if current
        .acceleration
        .into_iter()
        .zip(shown.acceleration)
        .any(|(current, shown)| display_centi_g(current) != display_centi_g(shown))
    {
        return true;
    }
    if current
        .gyroscope
        .into_iter()
        .zip(shown.gyroscope)
        .any(|(current, shown)| display_dps(current) != display_dps(shown))
    {
        return true;
    }
    let (current_x, current_y) = level_position(current.acceleration[0], current.acceleration[1]);
    let (shown_x, shown_y) = level_position(shown.acceleration[0], shown.acceleration[1]);
    current_x.abs_diff(shown_x) >= 2 || current_y.abs_diff(shown_y) >= 2
}

fn draw_level(framebuffer: &mut Framebuffer, raw_x: i16, raw_y: i16, level: bool) {
    const LEFT: usize = 1040;
    const TOP: usize = 44;
    const WIDTH: usize = 200;
    const HEIGHT: usize = 80;
    const CENTER_X: i32 = LEFT as i32 + WIDTH as i32 / 2;
    const CENTER_Y: i32 = TOP as i32 + HEIGHT as i32 / 2 + 8;

    framebuffer.stroke_rect(LEFT, TOP, WIDTH, HEIGHT, CYAN);
    framebuffer.draw_text(LEFT + 8, TOP + 6, "LEVEL", 2, WHITE, None);
    framebuffer.draw_line(
        CENTER_X as usize - 48,
        CENTER_Y as usize,
        CENTER_X as usize + 48,
        CENTER_Y as usize,
        0x7BEF,
    );
    framebuffer.draw_line(
        CENTER_X as usize,
        CENTER_Y as usize - 20,
        CENTER_X as usize,
        CENTER_Y as usize + 20,
        0x7BEF,
    );
    let (x, y) = level_position(raw_x, raw_y);
    framebuffer.fill_circle(x, y, 10, if level { GREEN } else { YELLOW });
    framebuffer.draw_circle(x, y, 10, WHITE);
}

struct AxisLine {
    bytes: [u8; 32],
    len: usize,
}

impl AxisLine {
    const fn new() -> Self {
        Self {
            bytes: [0; 32],
            len: 0,
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.len < self.bytes.len() {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
    }

    fn push_str(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            self.push_byte(byte);
        }
    }

    fn push_g(&mut self, raw: i16) {
        let centi_g = display_centi_g(raw);
        self.push_byte(if centi_g < 0 { b'-' } else { b'+' });
        let magnitude = centi_g.unsigned_abs();
        self.push_byte(b'0' + (magnitude / 100) as u8);
        self.push_byte(b'.');
        self.push_byte(b'0' + ((magnitude / 10) % 10) as u8);
        self.push_byte(b'0' + (magnitude % 10) as u8);
        self.push_byte(b'G');
    }

    fn push_dps(&mut self, raw: i16) {
        let dps = display_dps(raw);
        self.push_byte(if dps < 0 { b'-' } else { b'+' });
        let magnitude = dps.unsigned_abs();
        for divisor in [1_000, 100, 10, 1] {
            self.push_byte(b'0' + ((magnitude / divisor) % 10) as u8);
        }
        self.push_str("DPS");
    }

    fn as_str(&self) -> &str {
        // `push_*` above only emits ASCII.
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }
}

fn display_centi_g(raw: i16) -> i32 {
    // 0.02 g steps prevent harmless sensor noise from triggering a redraw.
    raw as i32 * 100 / 8192 / 2 * 2
}

fn display_dps(raw: i16) -> i32 {
    // Five-degree-per-second steps are precise enough for this diagnostic.
    raw as i32 * 1_000 / 32_768 / 5 * 5
}

fn level_position(raw_x: i16, raw_y: i16) -> (usize, usize) {
    const LEFT: usize = 1040;
    const TOP: usize = 44;
    const WIDTH: usize = 200;
    const HEIGHT: usize = 80;
    const CENTER_X: i32 = LEFT as i32 + WIDTH as i32 / 2;
    const CENTER_Y: i32 = TOP as i32 + HEIGHT as i32 / 2 + 8;
    let x = (CENTER_X - raw_y as i32 / 90).clamp(CENTER_X - 48, CENTER_X + 48) as usize;
    let y = (CENTER_Y + raw_x as i32 / 90).clamp(CENTER_Y - 20, CENTER_Y + 20) as usize;
    (x, y)
}

fn draw_ball(framebuffer: &mut Framebuffer, x: i32, y: i32) {
    let x = x as usize;
    let y = y as usize;
    framebuffer.fill_circle(
        x.saturating_add(5),
        y.saturating_add(6),
        BALL_RADIUS as usize,
        0x2104,
    );
    framebuffer.fill_circle(x, y, BALL_RADIUS as usize, RED);
    framebuffer.draw_circle(x, y, BALL_RADIUS as usize, YELLOW);
    framebuffer.fill_circle(x.saturating_sub(10), y.saturating_sub(10), 7, WHITE);
}

fn erase_ball_path(framebuffer: &mut Framebuffer, old_x: i32, old_y: i32, new_x: i32, new_y: i32) {
    let left = old_x.min(new_x) - BALL_RADIUS - 8;
    let top = old_y.min(new_y) - BALL_RADIUS - 8;
    let right = old_x.max(new_x) + BALL_RADIUS + 9;
    let bottom = old_y.max(new_y) + BALL_RADIUS + 9;
    framebuffer.fill_rect(
        left.max(0) as usize,
        top.max(0) as usize,
        (right - left).max(0) as usize,
        (bottom - top).max(0) as usize,
        BLACK,
    );
}

fn ball_dirty_left(old_x: i32, new_x: i32) -> usize {
    (old_x.min(new_x) / FIXED - BALL_RADIUS - 8).max(0) as usize
}

fn ball_dirty_top(old_y: i32, new_y: i32) -> usize {
    (old_y.min(new_y) / FIXED - BALL_RADIUS - 8).max(0) as usize
}

fn ball_dirty_width(old_x: i32, new_x: i32) -> usize {
    (old_x.max(new_x) / FIXED - old_x.min(new_x) / FIXED + BALL_RADIUS * 2 + 18) as usize
}

fn ball_dirty_height(old_y: i32, new_y: i32) -> usize {
    (old_y.max(new_y) / FIXED - old_y.min(new_y) / FIXED + BALL_RADIUS * 2 + 18) as usize
}

fn show_unavailable(framebuffer: &mut Framebuffer, error: InitError) {
    framebuffer.fill(BLACK);
    framebuffer.draw_text(16, 16, "AXIS SENSOR TEST", 3, CYAN, None);
    framebuffer.draw_text(16, 72, error.message(), 3, RED, None);
    framebuffer.draw_text(16, 120, "PRESS ANY KEY TO EXIT", 2, YELLOW, None);
    if !framebuffer.flush() {
        uart::log(b"Axis test: unavailable-screen flush failed\r\n");
        return;
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
