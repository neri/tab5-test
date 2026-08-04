//! CW-rotated RGB565 drawing primitives backed by external PSRAM.

use crate::psram::{HEIGHT as NATIVE_HEIGHT, Psram, WIDTH as NATIVE_WIDTH};

mod font;

/// Landscape logical dimensions. DSI and PSRAM retain the native 720x1280
/// scan layout.
pub const WIDTH: usize = NATIVE_HEIGHT;
pub const HEIGHT: usize = NATIVE_WIDTH;

pub const BLACK: u16 = 0x0000;
pub const WHITE: u16 = 0xFFFF;
pub const RED: u16 = 0xF800;
pub const GREEN: u16 = 0x07E0;
pub const BLUE: u16 = 0x001F;
pub const CYAN: u16 = 0x07FF;
pub const MAGENTA: u16 = 0xF81F;
pub const YELLOW: u16 = 0xFFE0;

pub struct DoubleBuffer {
    memory: Psram,
}

impl DoubleBuffer {
    pub fn new(memory: Psram) -> Option<Self> {
        memory.framebuffer(0)?;
        memory.framebuffer(1)?;
        Some(Self { memory })
    }

    pub fn address(&self, index: usize) -> Option<u32> {
        self.memory.framebuffer(index).map(|pointer| pointer as u32)
    }

    pub fn fill(&mut self, index: usize, color: u16) {
        let Some(pointer) = self.memory.framebuffer(index) else {
            return;
        };
        for offset in 0..NATIVE_WIDTH * NATIVE_HEIGHT {
            unsafe { pointer.add(offset).write_volatile(color) };
        }
    }

    pub fn draw_pixel(&mut self, index: usize, x: usize, y: usize, color: u16) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let Some(pointer) = self.memory.framebuffer(index) else {
            return;
        };
        unsafe { pointer.add(native_offset(x, y)).write_volatile(color) };
    }

    pub fn draw_line(
        &mut self,
        index: usize,
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        color: u16,
    ) {
        let (mut x0, mut y0, mut x1, mut y1) = (x0 as isize, y0 as isize, x1 as isize, y1 as isize);
        // CW rotation maps decreasing logical X to increasing native rows.
        // Bresenham is endpoint-symmetric, so choose the direction that makes
        // PSRAM accesses advance (and Y advance for a vertical logical line).
        if x0 < x1 || (x0 == x1 && y0 > y1) {
            core::mem::swap(&mut x0, &mut x1);
            core::mem::swap(&mut y0, &mut y1);
        }
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        loop {
            if x0 >= 0 && y0 >= 0 {
                self.draw_pixel(index, x0 as usize, y0 as usize, color);
            }
            if x0 == x1 && y0 == y1 {
                break;
            }
            let doubled = error * 2;
            if doubled >= dy {
                error += dy;
                x0 += sx;
            }
            if doubled <= dx {
                error += dx;
                y0 += sy;
            }
        }
    }

    pub fn fill_rect(
        &mut self,
        index: usize,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        color: u16,
    ) {
        let Some(pointer) = self.memory.framebuffer(index) else {
            return;
        };
        let x_end = x.saturating_add(width).min(WIDTH);
        let y_end = y.saturating_add(height).min(HEIGHT);
        // Keep logical X outermost so each inner Y run is contiguous. Iterate
        // X backwards because CW rotation maps increasing logical X to
        // decreasing native rows; this keeps the PSRAM write stream forward.
        for column in (x.min(WIDTH)..x_end).rev() {
            for row in y.min(HEIGHT)..y_end {
                unsafe {
                    pointer
                        .add(native_offset(column, row))
                        .write_volatile(color)
                };
            }
        }
    }

    pub fn stroke_rect(
        &mut self,
        index: usize,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        color: u16,
    ) {
        if width == 0 || height == 0 {
            return;
        }
        let right = x.saturating_add(width - 1);
        let bottom = y.saturating_add(height - 1);
        self.draw_line(index, x, y, right, y, color);
        self.draw_line(index, x, y, x, bottom, color);
        self.draw_line(index, right, y, right, bottom, color);
        self.draw_line(index, x, bottom, right, bottom, color);
    }

    pub fn draw_circle(
        &mut self,
        index: usize,
        center_x: usize,
        center_y: usize,
        radius: usize,
        color: u16,
    ) {
        let (cx, cy) = (center_x as isize, center_y as isize);
        let (mut x, mut y, mut error) = (radius as isize, 0isize, 1 - radius as isize);
        while x >= y {
            for (px, py) in [
                (cx + x, cy + y),
                (cx + y, cy + x),
                (cx - y, cy + x),
                (cx - x, cy + y),
                (cx - x, cy - y),
                (cx - y, cy - x),
                (cx + y, cy - x),
                (cx + x, cy - y),
            ] {
                if px >= 0 && py >= 0 {
                    self.draw_pixel(index, px as usize, py as usize, color);
                }
            }
            y += 1;
            if error < 0 {
                error += 2 * y + 1;
            } else {
                x -= 1;
                error += 2 * (y - x) + 1;
            }
        }
    }

    pub fn fill_circle(
        &mut self,
        index: usize,
        center_x: usize,
        center_y: usize,
        radius: usize,
        color: u16,
    ) {
        let radius = radius as isize;
        for y in -radius..=radius {
            let span = integer_sqrt((radius * radius - y * y) as usize) as isize;
            let start = center_x as isize - span;
            let end = center_x as isize + span;
            if center_y as isize + y < 0 {
                continue;
            }
            for x in start..=end {
                if x >= 0 {
                    self.draw_pixel(index, x as usize, (center_y as isize + y) as usize, color);
                }
            }
        }
    }

    /// Copies a compact row-major RGB565 image, clipped to the display.
    pub fn blit_rgb565(
        &mut self,
        index: usize,
        x: usize,
        y: usize,
        image_width: usize,
        image_height: usize,
        pixels: &[u16],
    ) -> bool {
        if pixels.len() < image_width.saturating_mul(image_height) {
            return false;
        }
        let Some(pointer) = self.memory.framebuffer(index) else {
            return false;
        };
        let copy_width = image_width.min(WIDTH.saturating_sub(x));
        let copy_height = image_height.min(HEIGHT.saturating_sub(y));
        for row in 0..copy_height {
            for column in 0..copy_width {
                unsafe {
                    pointer
                        .add(native_offset(x + column, y + row))
                        .write_volatile(pixels[row * image_width + column]);
                }
            }
        }
        true
    }

    /// Draws scaled 5x7 ASCII. Lowercase is rendered as uppercase.
    pub fn draw_text(
        &mut self,
        index: usize,
        x: usize,
        y: usize,
        text: &str,
        scale: usize,
        foreground: u16,
        background: Option<u16>,
    ) {
        let scale = scale.max(1);
        let origin_x = x;
        let (mut cursor_x, mut cursor_y) = (x, y);
        for byte in text.bytes() {
            if byte == b'\n' {
                cursor_x = origin_x;
                cursor_y = cursor_y.saturating_add(8 * scale);
                continue;
            }
            let glyph = font::glyph(byte);
            for column in 0..6 {
                let bits = if column < 5 { glyph[column] } else { 0 };
                for row in 0..7 {
                    let color = if bits & (1 << row) != 0 {
                        Some(foreground)
                    } else {
                        background
                    };
                    if let Some(color) = color {
                        self.fill_rect(
                            index,
                            cursor_x + column * scale,
                            cursor_y + row * scale,
                            scale,
                            scale,
                            color,
                        );
                    }
                }
            }
            cursor_x = cursor_x.saturating_add(6 * scale);
        }
    }

    pub fn flush(&self, index: usize) -> bool {
        self.memory.writeback(index)
    }

    pub fn draw_test_images(&mut self) -> bool {
        self.draw_quadrants(0);
        self.draw_coordinate_chart(1);
        if !self.flush(0) || !self.flush(1) {
            return false;
        }

        // Cache invalidation above makes these reads come back from external
        // PSRAM. The same first words are consumed by DW-GDMA, so this detects
        // a failed cache-to-memory synchronization before VPG is disabled.
        let Some(fb0) = self.memory.framebuffer(0) else {
            return false;
        };
        let Some(fb1) = self.memory.framebuffer(1) else {
            return false;
        };
        unsafe { fb0.read_volatile() == GREEN && fb1.read_volatile() == RED }
    }

    fn draw_quadrants(&mut self, index: usize) {
        let Some(pointer) = self.memory.framebuffer(index) else {
            return;
        };

        // Generate the rotated quadrants directly in panel-native row order.
        // The former logical-coordinate loop walked native rows backwards;
        // forward linear writes are substantially friendlier to the cache and
        // also make it possible to pinpoint a large-write stall by row.
        let mut offset = 0;
        for native_y in 0..NATIVE_HEIGHT {
            let logical_right = native_y < NATIVE_HEIGHT / 2;
            for native_x in 0..NATIVE_WIDTH {
                let logical_bottom = native_x >= NATIVE_WIDTH / 2;
                let color = match (logical_right, logical_bottom) {
                    (false, false) => RED,
                    (true, false) => GREEN,
                    (false, true) => BLUE,
                    (true, true) => WHITE,
                };
                unsafe { pointer.add(offset).write_volatile(color) };
                offset += 1;
            }
        }

        for y in 0..HEIGHT {
            self.draw_pixel(index, WIDTH / 2, y, BLACK);
            self.draw_pixel(index, WIDTH / 2 + 1, y, BLACK);
        }
        for x in 0..WIDTH {
            self.draw_pixel(index, x, HEIGHT / 2, BLACK);
            self.draw_pixel(index, x, HEIGHT / 2 + 1, BLACK);
        }

        self.fill_rect(index, 120, 48, 1040, 112, BLACK);
        self.stroke_rect(index, 112, 40, 1056, 128, YELLOW);
        self.draw_text(index, 448, 80, "RUST NO_STD  FB0", 4, WHITE, None);

        self.fill_circle(index, 220, 560, 72, YELLOW);
        self.draw_circle(index, 1060, 560, 72, CYAN);

        self.draw_line(index, 80, 660, 1200, 400, MAGENTA);
    }

    fn draw_coordinate_chart(&mut self, index: usize) {
        const DARK_A: u16 = 0x0841;
        const GRID: u16 = 0x7BEF;

        // A native-order fill avoids almost one million rotated address
        // calculations before the calibration grid becomes useful.
        self.fill(index, DARK_A);

        for x in (0..WIDTH).step_by(100) {
            self.draw_line(index, x, 0, x, HEIGHT - 1, GRID);
        }
        for y in (0..HEIGHT).step_by(100) {
            self.draw_line(index, 0, y, WIDTH - 1, y, GRID);
        }

        // Exact logical centre axes use two pixels so they remain distinct
        // from the 100-pixel grid when viewed at arm's length.
        self.draw_line(index, WIDTH / 2, 0, WIDTH / 2, HEIGHT - 1, YELLOW);
        self.draw_line(index, WIDTH / 2 + 1, 0, WIDTH / 2 + 1, HEIGHT - 1, YELLOW);
        self.draw_line(index, 0, HEIGHT / 2, WIDTH - 1, HEIGHT / 2, YELLOW);
        self.draw_line(index, 0, HEIGHT / 2 + 1, WIDTH - 1, HEIGHT / 2 + 1, YELLOW);

        for x in (0..WIDTH).step_by(100) {
            let bytes = coordinate_label(b'X', x);
            // coordinate_label emits ASCII only.
            let label = unsafe { core::str::from_utf8_unchecked(&bytes) };
            let label_width = bytes.len() * 12;
            let label_x = x
                .saturating_sub(label_width / 2)
                .min(WIDTH.saturating_sub(label_width + 4))
                .max(4);
            self.draw_text(index, label_x, 8, label, 2, WHITE, Some(BLACK));
        }

        for y in (100..HEIGHT - 100).step_by(100) {
            let bytes = coordinate_label(b'Y', y);
            // coordinate_label emits ASCII only.
            let label = unsafe { core::str::from_utf8_unchecked(&bytes) };
            self.draw_text(index, 8, y.saturating_sub(7), label, 2, WHITE, Some(BLACK));
        }

        self.draw_text(index, 452, 52, "LOGICAL 1280X720 CW", 3, CYAN, Some(BLACK));
        self.draw_text(index, 674, 378, "CENTER (640,360)", 3, YELLOW, Some(BLACK));
        self.draw_text(index, 20, 42, "(0,0)", 2, WHITE, Some(BLACK));
        self.draw_text(index, 1160, 42, "(1279,0)", 2, WHITE, Some(BLACK));
        self.draw_text(index, 20, 686, "(0,719)", 2, WHITE, Some(BLACK));
        self.draw_text(index, 1140, 686, "(1279,719)", 2, WHITE, Some(BLACK));

        let center_marker = [RED, GREEN, BLUE, WHITE];
        let _ = self.blit_rgb565(index, WIDTH / 2 - 1, HEIGHT / 2 - 1, 2, 2, &center_marker);

        // Four one-pixel inset borders reveal clipping independently on every
        // edge: red is the exact edge, followed by green, blue and white.
        self.stroke_rect(index, 0, 0, WIDTH, HEIGHT, RED);
        self.stroke_rect(index, 1, 1, WIDTH - 2, HEIGHT - 2, GREEN);
        self.stroke_rect(index, 2, 2, WIDTH - 4, HEIGHT - 4, BLUE);
        self.stroke_rect(index, 3, 3, WIDTH - 6, HEIGHT - 6, WHITE);
    }
}

/// Converts 1280x720 landscape coordinates to the panel-native 720x1280
/// row-major framebuffer: (x, y) -> (y, 1279 - x).
#[inline(always)]
fn native_offset(x: usize, y: usize) -> usize {
    (NATIVE_HEIGHT - 1 - x) * NATIVE_WIDTH + y
}

fn coordinate_label(axis: u8, value: usize) -> [u8; 5] {
    [
        axis,
        b'0' + ((value / 1000) % 10) as u8,
        b'0' + ((value / 100) % 10) as u8,
        b'0' + ((value / 10) % 10) as u8,
        b'0' + (value % 10) as u8,
    ]
}

fn integer_sqrt(value: usize) -> usize {
    if value < 2 {
        return value;
    }
    let mut x = value;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + value / x) / 2;
    }
    x
}
