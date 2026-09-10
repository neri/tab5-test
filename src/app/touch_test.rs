//! Full-screen multi-touch diagnostic entered through `touchtest`.

use crate::framebuffer::{BLACK, CYAN, Framebuffer, GREEN, HEIGHT, RED, WHITE, WIDTH, YELLOW};
use crate::input::{InputManager, TouchPoint};
use crate::{interrupts, uart};

const MAX_POINTS: usize = 10;
const COUNT_TEXT: [&str; MAX_POINTS + 1] = [
    "Live touches: 0",
    "Live touches: 1",
    "Live touches: 2",
    "Live touches: 3",
    "Live touches: 4",
    "Live touches: 5",
    "Live touches: 6",
    "Live touches: 7",
    "Live touches: 8",
    "Live touches: 9",
    "Live touches: 10",
];
const PEAK_TEXT: [&str; MAX_POINTS + 1] = [
    "Peak: 0", "Peak: 1", "Peak: 2", "Peak: 3", "Peak: 4", "Peak: 5", "Peak: 6", "Peak: 7",
    "Peak: 8", "Peak: 9", "Peak: 10",
];

/// Shows the active contact count and succeeds once two contacts are read in
/// the same controller report. Press any managed keyboard key to exit.
pub fn run(framebuffer: &mut Framebuffer, input: &mut InputManager) {
    let touch_controller = input.touch_controller_name();
    let touch_max_points = input.touch_max_points();
    framebuffer.fill(BLACK);
    framebuffer.draw_text(16, 8, "Multitouch test", 2, CYAN, None);
    framebuffer.draw_text(
        16,
        48,
        "Place two or more fingers on the screen.",
        1,
        WHITE,
        None,
    );
    framebuffer.draw_text(
        16,
        72,
        "A simultaneous count of 2 or more is a PASS.",
        1,
        WHITE,
        None,
    );
    framebuffer.draw_text(16, 104, "Press any key to exit.", 1, YELLOW, None);
    if let Some(controller) = touch_controller {
        framebuffer.draw_text(16, 128, controller, 1, CYAN, None);
        framebuffer.draw_text(
            16,
            152,
            configured_text(touch_max_points.unwrap_or(0)),
            1,
            CYAN,
            None,
        );
    } else {
        framebuffer.draw_text(16, 128, "No touch controller found", 1, RED, None);
    }
    draw_status(framebuffer, 0, 0, false);
    if !framebuffer.flush() {
        uart::log(b"Touch test: initial flush failed\r\n");
        return;
    }

    if touch_controller.is_none() {
        input.wait_for_key();
        return;
    }
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
            input.service_fast();
            continue;
        }
        sequence = next_sequence;
        input.service();
        if input.poll_key().is_some() {
            return;
        }

        let count = input.poll_touch_points(&mut points).min(MAX_POINTS);
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
        0 => "Controller report slots: 0",
        1 => "Controller report slots: 1",
        2 => "Controller report slots: 2",
        3 => "Controller report slots: 3",
        4 => "Controller report slots: 4",
        5 => "Controller report slots: 5",
        6 => "Controller report slots: 6",
        7 => "Controller report slots: 7",
        8 => "Controller report slots: 8",
        9 => "Controller report slots: 9",
        _ => "Controller report slots: 10",
    }
}

fn draw_status(framebuffer: &mut Framebuffer, current: usize, peak: usize, passed: bool) {
    framebuffer.fill_rect(0, 176, WIDTH, HEIGHT - 176, BLACK);
    framebuffer.draw_text(16, 192, COUNT_TEXT[current], 2, WHITE, None);
    framebuffer.draw_text(16, 240, PEAK_TEXT[peak], 2, WHITE, None);
    let (message, color) = if passed {
        ("Pass: multitouch detected", GREEN)
    } else {
        ("Waiting for 2+ simultaneous touches", YELLOW)
    };
    framebuffer.draw_text(16, 288, message, 2, color, None);
}
