//! Full-screen multi-touch diagnostic entered through `touchtest`.

use crate::framebuffer::{BLACK, CYAN, Framebuffer, GREEN, HEIGHT, RED, WHITE, WIDTH, YELLOW};
use crate::input::InputManager;
use crate::touch::{Touch, TouchPoint};
use crate::{interrupts, uart};

const MAX_POINTS: usize = 10;
const COUNT_TEXT: [&str; MAX_POINTS + 1] = [
    "LIVE TOUCHES: 0",
    "LIVE TOUCHES: 1",
    "LIVE TOUCHES: 2",
    "LIVE TOUCHES: 3",
    "LIVE TOUCHES: 4",
    "LIVE TOUCHES: 5",
    "LIVE TOUCHES: 6",
    "LIVE TOUCHES: 7",
    "LIVE TOUCHES: 8",
    "LIVE TOUCHES: 9",
    "LIVE TOUCHES: 10",
];
const PEAK_TEXT: [&str; MAX_POINTS + 1] = [
    "PEAK: 0", "PEAK: 1", "PEAK: 2", "PEAK: 3", "PEAK: 4", "PEAK: 5", "PEAK: 6", "PEAK: 7",
    "PEAK: 8", "PEAK: 9", "PEAK: 10",
];

/// Shows the active contact count and succeeds once two contacts are read in
/// the same controller report. Press any managed keyboard key to exit.
pub fn run(framebuffer: &mut Framebuffer, input: &mut InputManager) {
    let touch_panel = Touch::init();
    framebuffer.fill(BLACK);
    framebuffer.draw_text(16, 8, "MULTITOUCH TEST", 3, CYAN, None);
    framebuffer.draw_text(
        16,
        48,
        "Place two or more fingers on the screen.",
        2,
        WHITE,
        None,
    );
    framebuffer.draw_text(
        16,
        72,
        "A simultaneous count of 2 or more is a PASS.",
        2,
        WHITE,
        None,
    );
    framebuffer.draw_text(16, 104, "Press any key to exit.", 2, YELLOW, None);
    if let Some(panel) = touch_panel.as_ref() {
        framebuffer.draw_text(16, 128, panel.controller_name(), 2, CYAN, None);
        framebuffer.draw_text(16, 152, configured_text(panel.max_touches()), 2, CYAN, None);
    } else {
        framebuffer.draw_text(16, 128, "NO TOUCH CONTROLLER FOUND", 2, RED, None);
    }
    draw_status(framebuffer, 0, 0, false);
    if !framebuffer.flush() {
        uart::log(b"Touch test: initial flush failed\r\n");
        return;
    }

    let Some(panel) = touch_panel.as_ref() else {
        wait_for_key(input);
        return;
    };
    uart::log(b"Touch test: place two fingers on the panel\r\n");

    let mut sequence = interrupts::frame_sequence();
    let mut points = [TouchPoint { x: 0, y: 0 }; MAX_POINTS];
    let mut current = 0;
    let mut peak = 0;
    let mut passed = false;
    loop {
        if interrupts::dma_error() != 0 {
            uart::log(b"Touch test: DMA interrupt error\r\n");
            return;
        }
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

        let count = panel.poll_points(&mut points).min(MAX_POINTS);
        let next_peak = peak.max(count);
        let next_passed = passed || count >= 2;
        let newly_passed = !passed && next_passed;
        if count == current && next_peak == peak && next_passed == passed {
            continue;
        }
        current = count;
        peak = next_peak;
        passed = next_passed;
        uart::log_hex(b"Touch test: simultaneous contacts=", count as u32);
        if newly_passed {
            uart::log(b"Touch test: PASS (multi-touch observed)\r\n");
        }
        draw_status(framebuffer, current, peak, passed);
        if !framebuffer.flush_rect(0, 176, WIDTH, HEIGHT - 176) {
            uart::log(b"Touch test: flush failed\r\n");
            return;
        }
    }
}

fn configured_text(max_touches: usize) -> &'static str {
    match max_touches.min(MAX_POINTS) {
        0 => "CONTROLLER REPORT SLOTS: 0",
        1 => "CONTROLLER REPORT SLOTS: 1",
        2 => "CONTROLLER REPORT SLOTS: 2",
        3 => "CONTROLLER REPORT SLOTS: 3",
        4 => "CONTROLLER REPORT SLOTS: 4",
        5 => "CONTROLLER REPORT SLOTS: 5",
        6 => "CONTROLLER REPORT SLOTS: 6",
        7 => "CONTROLLER REPORT SLOTS: 7",
        8 => "CONTROLLER REPORT SLOTS: 8",
        9 => "CONTROLLER REPORT SLOTS: 9",
        _ => "CONTROLLER REPORT SLOTS: 10",
    }
}

fn draw_status(framebuffer: &mut Framebuffer, current: usize, peak: usize, passed: bool) {
    framebuffer.fill_rect(0, 176, WIDTH, HEIGHT - 176, BLACK);
    framebuffer.draw_text(16, 192, COUNT_TEXT[current], 4, WHITE, None);
    framebuffer.draw_text(16, 240, PEAK_TEXT[peak], 3, WHITE, None);
    let (message, color) = if passed {
        ("PASS: MULTITOUCH DETECTED", GREEN)
    } else {
        ("WAITING FOR 2+ SIMULTANEOUS TOUCHES", YELLOW)
    };
    framebuffer.draw_text(16, 288, message, 3, color, None);
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
