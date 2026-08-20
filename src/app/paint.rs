//! Full-screen touch drawing mode entered via the shell's `paint` command.

use crate::framebuffer::{BLACK, CYAN, Framebuffer, HEIGHT, WHITE, WIDTH};
use crate::input::{InputManager, TouchPoint};
use crate::{interrupts, uart};

const BRUSH_RADIUS: usize = 5;
const HINT: &str = "PAINT - TOUCH TO DRAW, ANY KEY TO EXIT";

/// Runs the paint screen until any managed keyboard key is pressed. The
/// framebuffer holds the finished canvas on return, so the caller can render
/// straight over it (e.g. a cleared console) without needing to know paint's
/// internals.
pub fn run(framebuffer: &mut Framebuffer, input: &mut InputManager) {
    if input.touch_controller_name().is_none() {
        uart::log(b"Paint: no touch controller found, no drawing input available\r\n");
    }

    framebuffer.fill(BLACK);
    framebuffer.draw_text(16, 8, HINT, 2, CYAN, None);
    if !framebuffer.flush() {
        uart::log(b"Paint: initial flush failed\r\n");
        return;
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

        let mut points = [TouchPoint { x: 0, y: 0 }; 1];
        match input.poll_touch_points(&mut points) {
            0 => last_point = None,
            _ => {
                let point = (points[0].x, points[0].y);
                if !logged_first_touch {
                    logged_first_touch = true;
                    uart::log_hex(b"Paint: first touch x=", point.0 as u32);
                    uart::log_hex(b"Paint: first touch y=", point.1 as u32);
                }
                let from = last_point.unwrap_or(point);
                draw_stroke(framebuffer, from, point, BRUSH_RADIUS, WHITE);
                let _ = flush_stroke(framebuffer, from, point, BRUSH_RADIUS);
                last_point = Some(point);
            }
        }
    }
}

/// Stamps filled circles along the segment from `from` to `to` so a fast
/// touch drag still draws a solid stroke rather than isolated dots.
fn draw_stroke(
    framebuffer: &mut Framebuffer,
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
            framebuffer.fill_circle(x as usize, y as usize, radius, color);
        }
    }
}

/// Writes back the bounding box of a stroke segment, padded by its radius.
fn flush_stroke(
    framebuffer: &Framebuffer,
    from: (usize, usize),
    to: (usize, usize),
    radius: usize,
) -> bool {
    let min_x = from.0.min(to.0).saturating_sub(radius);
    let min_y = from.1.min(to.1).saturating_sub(radius);
    let max_x = from.0.max(to.0).saturating_add(radius).min(WIDTH - 1);
    let max_y = from.1.max(to.1).saturating_add(radius).min(HEIGHT - 1);
    framebuffer.flush_rect(min_x, min_y, max_x - min_x + 1, max_y - min_y + 1)
}
