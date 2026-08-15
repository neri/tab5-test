//! Full-screen touch drawing mode entered via the shell's `paint` command.

use crate::framebuffer::{BLACK, CYAN, DoubleBuffer, HEIGHT, WHITE, WIDTH};
use crate::input::InputManager;
use crate::touch::Touch;
use crate::{interrupts, uart};

const BRUSH_RADIUS: usize = 5;
const HINT: &str = "PAINT - TOUCH TO DRAW, ANY KEY TO EXIT";

/// Runs the paint screen until any managed keyboard key is pressed. Both framebuffers
/// hold the same canvas on return, so the caller can render straight over
/// it (e.g. a cleared console) without needing to know paint's internals.
pub fn run(framebuffers: &mut DoubleBuffer, input: &mut InputManager) {
    let touch_panel = Touch::init();
    if touch_panel.is_none() {
        uart::log(b"Paint: no touch controller found, no drawing input available\r\n");
    }

    let displayed = interrupts::active_framebuffer();
    for index in [displayed ^ 1, displayed] {
        framebuffers.fill(index, BLACK);
        framebuffers.draw_text(index, 16, 8, HINT, 2, CYAN, None);
        if !framebuffers.flush(index) {
            uart::log(b"Paint: initial flush failed\r\n");
            return;
        }
    }

    let mut sequence = interrupts::frame_sequence();
    let mut last_point: Option<(usize, usize)> = None;
    let mut logged_first_touch = false;
    loop {
        if interrupts::dma_error() != 0 {
            uart::log(b"Paint: DMA interrupt error\r\n");
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

        let Some(panel) = touch_panel.as_ref() else {
            continue;
        };
        match panel.poll() {
            Some(point) => {
                if !logged_first_touch {
                    logged_first_touch = true;
                    uart::log_hex(b"Paint: first touch x=", point.0 as u32);
                    uart::log_hex(b"Paint: first touch y=", point.1 as u32);
                }
                let from = last_point.unwrap_or(point);
                let displayed = interrupts::active_framebuffer();
                for index in [displayed ^ 1, displayed] {
                    draw_stroke(framebuffers, index, from, point, BRUSH_RADIUS, WHITE);
                    let _ = flush_stroke(framebuffers, index, from, point, BRUSH_RADIUS);
                }
                last_point = Some(point);
            }
            None => last_point = None,
        }
    }
}

/// Stamps filled circles along the segment from `from` to `to` so a fast
/// touch drag still draws a solid stroke rather than isolated dots.
fn draw_stroke(
    framebuffers: &mut DoubleBuffer,
    index: usize,
    from: (usize, usize),
    to: (usize, usize),
    radius: usize,
    color: u16,
) {
    let (x0, y0) = (from.0 as isize, from.1 as isize);
    let (x1, y1) = (to.0 as isize, to.1 as isize);
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    for step in 0..=steps {
        let x = x0 + (x1 - x0) * step / steps;
        let y = y0 + (y1 - y0) * step / steps;
        if x >= 0 && y >= 0 {
            framebuffers.fill_circle(index, x as usize, y as usize, radius, color);
        }
    }
}

/// Writes back the bounding box of a stroke segment, padded by its radius.
fn flush_stroke(
    framebuffers: &DoubleBuffer,
    index: usize,
    from: (usize, usize),
    to: (usize, usize),
    radius: usize,
) -> bool {
    let min_x = from.0.min(to.0).saturating_sub(radius);
    let min_y = from.1.min(to.1).saturating_sub(radius);
    let max_x = from.0.max(to.0).saturating_add(radius).min(WIDTH - 1);
    let max_y = from.1.max(to.1).saturating_add(radius).min(HEIGHT - 1);
    framebuffers.flush_rect(index, min_x, min_y, max_x - min_x + 1, max_y - min_y + 1)
}
