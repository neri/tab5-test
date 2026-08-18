//! CW-rotated RGB565 drawing primitives backed by external PSRAM.

use crate::dma2d;
use crate::ppa;
use crate::psram::{HEIGHT as NATIVE_HEIGHT, Psram, WIDTH as NATIVE_WIDTH};

mod font;

/// Landscape logical dimensions. DSI and PSRAM retain the native 720x1280
/// scan layout.
pub const WIDTH: usize = NATIVE_HEIGHT;
pub const HEIGHT: usize = NATIVE_WIDTH;

/// Writeback granularity. Small enough that GDMA's scanout reads get the bus
/// back between chunks; see `Framebuffer::flush`.
const WRITEBACK_CHUNK_BYTES: usize = 64 * 1024;

/// Rectangle area from which `fill_rect` hands the work to the PPA.
///
/// A DMA transfer has to be described, started and waited for, and that fixed
/// cost has to be earned back. `ppafill sweep` measured where it is, in
/// microseconds per fill on this hardware:
///
/// ```text
///   12x16 (one console cell)      14 ppa     15 cpu
///   24x32                         21 ppa     41 cpu
///   48x64                         51 ppa    133 cpu
///   96x128                       314 ppa   1503 cpu
///   1280x720 (full screen)     12879 ppa  94408 cpu
/// ```
///
/// At a console cell the two are the same to within noise, and the gap opens
/// from there. 24x32 is the smallest size measured to be clearly ahead, so
/// that is the threshold: the console's per-cell repaints -- by far the most
/// frequent fills, and the ones on the cursor-blink path -- stay on the CPU
/// where they gain nothing, and everything the full-screen modes repaint goes
/// to the DMA.
///
/// Area is the test rather than width or height, which is an approximation: a
/// wide, short rectangle covers a large native span for few pixels, because
/// rotation makes logical width the thing that strides across memory. It costs
/// nothing to be wrong about, since the cache pass over that span is the same
/// either way and the caller pays it regardless of which path filled.
const PPA_FILL_MIN_PIXELS: usize = 24 * 32;

pub const BLACK: u16 = 0x0000;
pub const WHITE: u16 = 0xFFFF;
pub const RED: u16 = 0xF800;
pub const GREEN: u16 = 0x07E0;
pub const BLUE: u16 = 0x001F;
pub const CYAN: u16 = 0x07FF;
pub const MAGENTA: u16 = 0xF81F;
pub const YELLOW: u16 = 0xFFE0;

/// Maps a `char` onto the 5x7 ASCII font's byte-indexed glyph table. Only
/// ASCII is defined there today, so anything outside that range falls back
/// to a blank space; real non-ASCII glyph rendering is future work.
fn ascii_or_space(ch: char) -> u8 {
    if ch.is_ascii() { ch as u8 } else { b' ' }
}

pub struct Framebuffer {
    memory: Psram,
}

impl Framebuffer {
    pub fn new(memory: Psram) -> Option<Self> {
        memory.framebuffer()?;
        Some(Self { memory })
    }

    pub fn address(&self) -> Option<u32> {
        self.memory.framebuffer().map(|pointer| pointer as u32)
    }

    /// Clears the whole framebuffer, by DMA where that is available.
    ///
    /// This is the single most expensive thing the console does and the reason
    /// the PPA path exists. Written by the CPU, the clear costs about 86 ms:
    /// each 2-byte store misses a 64-byte line, write-allocate reads that line
    /// back from PSRAM before overwriting it, and the core cannot overlap the
    /// misses, so 1.8 MiB of writes drag 1.8 MiB of pointless reads through
    /// the same PSRAM read path the display is being starved on. The PPA path
    /// issues none of those reads and is not serialised on cache-miss latency.
    ///
    /// Unlike the CPU path this one leaves the result in PSRAM rather than in
    /// dirty cache lines. Callers flush afterwards either way, and that flush
    /// stays correct -- it just has less to write back.
    pub fn fill(&mut self, color: u16) {
        if self.ppa_fill_rect(0, 0, WIDTH, HEIGHT, color) {
            return;
        }
        self.fill_with_cpu(color);
    }

    /// The store-loop clear, used when the PPA is unavailable or refused the
    /// transfer. Keeping it means a failed PPA bring-up costs speed and
    /// nothing else.
    fn fill_with_cpu(&mut self, color: u16) {
        let Some(pointer) = self.memory.framebuffer() else {
            return;
        };
        // A framebuffer starts on a 64 KiB MMU page and holds an even number
        // of pixels, so it can be cleared as 32-bit words. Halving the store
        // count matters here: this is the first thing a full redraw does, and
        // every cycle it spends is a cycle the DSI bridge competes with it for
        // PSRAM.
        let pair = (color as u32) << 16 | color as u32;
        let words = pointer as *mut u32;
        for offset in 0..NATIVE_WIDTH * NATIVE_HEIGHT / 2 {
            unsafe { words.add(offset).write_volatile(pair) };
        }
    }

    pub fn draw_pixel(&mut self, x: usize, y: usize, color: u16) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let Some(pointer) = self.memory.framebuffer() else {
            return;
        };
        unsafe { pointer.add(native_offset(x, y)).write_volatile(color) };
    }

    pub fn draw_line(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, color: u16) {
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
                self.draw_pixel(x0 as usize, y0 as usize, color);
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

    /// Fills a logical rectangle, by DMA once the rectangle is big enough to
    /// be worth setting one up.
    pub fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: u16) {
        if width.saturating_mul(height) >= PPA_FILL_MIN_PIXELS
            && self.ppa_fill_rect(x, y, width, height, color)
        {
            return;
        }
        let Some(pointer) = self.memory.framebuffer() else {
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

    pub fn stroke_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: u16) {
        if width == 0 || height == 0 {
            return;
        }
        let right = x.saturating_add(width - 1);
        let bottom = y.saturating_add(height - 1);
        self.draw_line(x, y, right, y, color);
        self.draw_line(x, y, x, bottom, color);
        self.draw_line(right, y, right, bottom, color);
        self.draw_line(x, bottom, right, bottom, color);
    }

    pub fn draw_circle(&mut self, center_x: usize, center_y: usize, radius: usize, color: u16) {
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
                    self.draw_pixel(px as usize, py as usize, color);
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

    pub fn fill_circle(&mut self, center_x: usize, center_y: usize, radius: usize, color: u16) {
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
                    self.draw_pixel(x as usize, (center_y as isize + y) as usize, color);
                }
            }
        }
    }

    /// Copies a compact row-major RGB565 image, clipped to the display.
    pub fn blit_rgb565(
        &mut self,
        x: usize,
        y: usize,
        image_width: usize,
        image_height: usize,
        pixels: &[u16],
    ) -> bool {
        if pixels.len() < image_width.saturating_mul(image_height) {
            return false;
        }
        let Some(pointer) = self.memory.framebuffer() else {
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

    /// Copies a logical rectangle back out of the framebuffer, the exact
    /// counterpart of `blit_rgb565`: same clipping against the right and
    /// bottom edges, same `image_width` row stride in `pixels`. Reading a
    /// rectangle out with one and writing it back with the other therefore
    /// restores it unchanged even where the rectangle hangs off an edge,
    /// because both touch the same clipped subrectangle and address
    /// `pixels` the same way.
    ///
    /// This is what lets a moving sprite -- `app::win`'s mouse cursor --
    /// put back what it covered without the caller having to be able to
    /// redraw the scene underneath it procedurally.
    ///
    /// Reads are ordinary cached loads and so are coherent with both
    /// drawing paths: the CPU ones leave their pixels in the same cache
    /// this reads through, and `ppa_fill_rect` invalidates the region after
    /// its DMA writes so a later read misses and fetches what the DMA
    /// actually wrote.
    pub fn read_rect(
        &self,
        x: usize,
        y: usize,
        image_width: usize,
        image_height: usize,
        pixels: &mut [u16],
    ) -> bool {
        if pixels.len() < image_width.saturating_mul(image_height) {
            return false;
        }
        let Some(pointer) = self.memory.framebuffer() else {
            return false;
        };
        let copy_width = image_width.min(WIDTH.saturating_sub(x));
        let copy_height = image_height.min(HEIGHT.saturating_sub(y));
        for row in 0..copy_height {
            for column in 0..copy_width {
                pixels[row * image_width + column] = unsafe {
                    pointer
                        .add(native_offset(x + column, y + row))
                        .read_volatile()
                };
            }
        }
        true
    }

    /// Draws scaled 5x7 ASCII, upper and lower case each with their own
    /// glyphs. Non-ASCII is drawn as a space; ASCII the table has no glyph
    /// for (control codes, a few punctuation marks) is drawn as '?'.
    pub fn draw_text(
        &mut self,
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
        for ch in text.chars() {
            if ch == '\n' {
                cursor_x = origin_x;
                cursor_y = cursor_y.saturating_add(8 * scale);
                continue;
            }
            let glyph = font::glyph(ascii_or_space(ch));
            for column in 0..6 {
                let bits = if column < 5 { glyph[column] } else { 0 };
                for row in 0..7 {
                    let color = if bits & (1 << row) != 0 {
                        Some(foreground)
                    } else {
                        background
                    };
                    if let Some(color) = color {
                        // Text consists of many tiny cells. Write those pixels
                        // directly rather than repeatedly entering fill_rect;
                        // this keeps PSRAM accesses predictable on ECO2.
                        for offset_x in 0..scale {
                            for offset_y in 0..scale {
                                self.draw_pixel(
                                    cursor_x + column * scale + offset_x,
                                    cursor_y + row * scale + offset_y,
                                    color,
                                );
                            }
                        }
                    }
                }
            }
            cursor_x = cursor_x.saturating_add(6 * scale);
        }
    }

    /// Draws one scaled 5x7 ASCII glyph without the general text iterator.
    ///
    /// `background` covers the glyph's whole 6x8 advance box -- one column of
    /// letter spacing and one row of line spacing wider than the glyph itself,
    /// i.e. exactly one console cell. Passing it lets the console repaint a
    /// cell in this one call: without it a caller has to `fill_rect` the cell
    /// first, because only foreground pixels are written and whatever stood
    /// there (a glyph, or the cursor block's white) would show through.
    /// `None` writes foreground pixels only, for callers drawing onto a
    /// background they have already established.
    ///
    /// The two cases share nothing but this entry: opaque writes every pixel
    /// of a fixed box, transparent writes a sparse subset of it, and the work
    /// worth avoiding is the opposite in each. They are separate loops below.
    #[inline(never)]
    pub fn draw_ascii_char(
        &mut self,
        x: usize,
        y: usize,
        ch: char,
        scale: usize,
        foreground: u16,
        background: Option<u16>,
    ) {
        let Some(pointer) = self.memory.framebuffer() else {
            return;
        };
        let scale = scale.max(1);
        let bits = font::glyph(ascii_or_space(ch));
        // Clip the box's pixel rows once. Increasing logical Y is increasing
        // native address, so this is also the length of each column's run.
        let run = y.saturating_add(8 * scale).min(HEIGHT).saturating_sub(y);
        if run == 0 {
            return;
        }
        let glyph = Glyph {
            pointer,
            x,
            y,
            run,
            bits: &bits,
            scale,
            foreground,
        };
        match background {
            Some(background) => unsafe { glyph.paint_opaque(background) },
            None => unsafe { glyph.paint_sparse() },
        }
    }

    /// Writes back the complete framebuffer in chunks rather than one
    /// single-shot writeback.
    ///
    /// A full 1.8 MiB writeback contends with GDMA's concurrent PSRAM reads
    /// for the buffer being scanned out. If those reads fall behind, the DSI
    /// bridge's FIFO runs dry and the panel shows a solid light blue frame.
    /// Chunking keeps each writeback burst closer to the size of a per-cell
    /// update, which has never provoked it, so GDMA's reads can interleave
    /// between chunks instead of losing the bus for the whole framebuffer at
    /// once. Interconnect arbitration (`icm::prioritize_display_reads`) is
    /// what actually guarantees those reads win; this only smooths the peak.
    pub fn flush(&self) -> bool {
        self.flush_rect(0, 0, WIDTH, HEIGHT)
    }

    /// Fills a logical rectangle through the PPA instead of the CPU, keeping
    /// the caches consistent on both sides of the transfer.
    ///
    /// CW rotation costs nothing here. A logical rectangle is still a
    /// rectangle in the native 720x1280 picture -- just a transposed one, with
    /// logical Y running along a native row and logical X running up the rows
    /// from the bottom -- and describing a block inside a larger picture is
    /// exactly what a 2D-DMA descriptor does.
    ///
    /// Both cache passes are needed, and for opposite reasons. Beforehand,
    /// because a dirty line still held over this region would be evicted after
    /// the DMA had written and would put the old pixels back on top of it.
    /// Afterwards, because the DMA does not go through the cache, so a clean
    /// line left resident holds pre-fill content that a later partial write by
    /// the CPU would hit and write on top of. `flush_rect` writes back and
    /// invalidates, which serves both.
    ///
    /// Returns false if the rectangle is empty, does not fit, or the transfer
    /// did not complete; the caller can then fall back to `fill_rect`.
    pub fn ppa_fill_rect(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        color: u16,
    ) -> bool {
        let Some(pointer) = self.memory.framebuffer() else {
            return false;
        };
        let x_start = x.min(WIDTH);
        let y_start = y.min(HEIGHT);
        let x_end = x.saturating_add(width).min(WIDTH);
        let y_end = y.saturating_add(height).min(HEIGHT);
        if x_start >= x_end || y_start >= y_end {
            return false;
        }
        let width = x_end - x_start;
        let height = y_end - y_start;

        if !self.flush_rect(x_start, y_start, width, height) {
            return false;
        }
        // Native row `NATIVE_HEIGHT - 1 - x` holds logical column x, so the
        // block's top row is the one belonging to the rectangle's right edge.
        let picture = native_picture(pointer);
        let destination = dma2d::Block {
            picture: &picture,
            x: y_start,
            y: NATIVE_HEIGHT - x_end,
        };
        let filled = ppa::fill_rgb565(&destination, height, width, color);
        // Invalidate even on failure: a partial fill still leaves the cache
        // disagreeing with PSRAM.
        self.flush_rect(x_start, y_start, width, height);
        filled
    }

    /// Moves a band of the screen up by `distance` logical Y pixels, by DMA.
    ///
    /// This is what a text console does on every scroll, and by far the
    /// cheapest way to do it: the pixels already say what they should say one
    /// row higher, so nothing needs re-rendering except the row exposed at the
    /// bottom. The CPU touches no pixel at all.
    ///
    /// The band runs the full logical width and covers logical Y in
    /// `[top, top + height)`; afterwards its bottom `distance` rows still hold
    /// their old contents and are the caller's to repaint.
    ///
    /// **Only upwards.** Rotation puts logical Y along a native row, so moving
    /// up means moving to a *lower* address within each row, which is the
    /// direction the engine's own read-before-write ordering makes safe --
    /// see `dma2d::copy_rgb565`. Moving down would need the opposite order and
    /// is not offered.
    ///
    /// Returns false if the geometry is empty or the transfer did not
    /// complete, leaving the caller to repaint the band itself.
    pub fn scroll_up(&mut self, top: usize, height: usize, distance: usize) -> bool {
        let Some(pointer) = self.memory.framebuffer() else {
            return false;
        };
        let bottom = top.saturating_add(height).min(HEIGHT);
        if distance == 0 || top >= bottom || distance >= bottom - top {
            return false;
        }
        // Native columns: logical Y indexes along a native row directly.
        let moved = bottom - top - distance;

        // One writeback-invalidate over the whole band, covering source and
        // destination together. Afterwards nothing of the band is resident, so
        // the DMA's writes cannot be overwritten by an eviction and cannot be
        // shadowed by a stale clean line.
        if !self.flush_rect(0, top, WIDTH, height) {
            return false;
        }
        let picture = native_picture(pointer);
        let source = dma2d::Block {
            picture: &picture,
            x: top + distance,
            y: 0,
        };
        let destination = dma2d::Block {
            picture: &picture,
            x: top,
            y: 0,
        };
        let copied = dma2d::copy_rgb565(&source, &destination, moved, NATIVE_HEIGHT);
        self.flush_rect(0, top, WIDTH, height);
        copied
    }

    /// Synchronises the native-memory span covering a logical rectangle.
    /// Rotation makes the rows sparse, so this includes the short gaps
    /// between them while remaining far smaller than a complete framebuffer.
    ///
    /// The span is written back in chunks, for the reason described on
    /// `flush`: one uninterrupted multi-megabyte writeback takes the bus away
    /// from GDMA's scanout reads for long enough to empty the DSI bridge's
    /// FIFO. Small rectangles are one chunk and pay nothing for the loop.
    pub fn flush_rect(&self, x: usize, y: usize, width: usize, height: usize) -> bool {
        let x_start = x.min(WIDTH);
        let y_start = y.min(HEIGHT);
        let x_end = x.saturating_add(width).min(WIDTH);
        let y_end = y.saturating_add(height).min(HEIGHT);
        if x_start == x_end || y_start == y_end {
            return false;
        }

        let first_pixel = (NATIVE_HEIGHT - x_end) * NATIVE_WIDTH + y_start;
        let end_pixel = (NATIVE_HEIGHT - 1 - x_start) * NATIVE_WIDTH + y_end;
        let mut offset = first_pixel * core::mem::size_of::<u16>();
        let end = end_pixel * core::mem::size_of::<u16>();
        while offset < end {
            let bytes = WRITEBACK_CHUNK_BYTES.min(end - offset);
            if !self.memory.writeback_range(offset, bytes) {
                return false;
            }
            offset += bytes;
        }
        true
    }

    /// Paints the bring-up quadrant pattern and confirms it reached PSRAM.
    ///
    /// `draw_coordinate_chart` is the other bring-up screen; this one is kept
    /// alongside it so either can be dropped in while checking rotation or
    /// clipping.
    pub fn draw_test_images(&mut self) -> bool {
        self.draw_quadrants();
        if !self.flush() {
            return false;
        }

        // Cache invalidation above makes this read come back from external
        // PSRAM. The same first word is consumed by DW-GDMA, so this detects
        // a failed cache-to-memory synchronization before scanout starts.
        let Some(framebuffer) = self.memory.framebuffer() else {
            return false;
        };
        unsafe { framebuffer.read_volatile() == GREEN }
    }

    fn draw_quadrants(&mut self) {
        let Some(pointer) = self.memory.framebuffer() else {
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
            self.draw_pixel(WIDTH / 2, y, BLACK);
            self.draw_pixel(WIDTH / 2 + 1, y, BLACK);
        }
        for x in 0..WIDTH {
            self.draw_pixel(x, HEIGHT / 2, BLACK);
            self.draw_pixel(x, HEIGHT / 2 + 1, BLACK);
        }

        self.fill_rect(120, 48, 1040, 112, BLACK);
        self.stroke_rect(112, 40, 1056, 128, YELLOW);
        self.draw_text(448, 80, "RUST NO_STD  FB0", 4, WHITE, None);

        self.fill_circle(220, 560, 72, YELLOW);
        self.draw_circle(1060, 560, 72, CYAN);

        self.draw_line(80, 660, 1200, 400, MAGENTA);
    }

    /// Paints the coordinate calibration chart: a 100-pixel grid, the exact
    /// logical centre axes, labelled corners, and four one-pixel inset borders.
    ///
    /// Every number on it is a logical coordinate, so comparing the chart
    /// against a ruler on the panel is what verifies the CW rotation, the
    /// clipping at each edge, and the logical-to-native mapping. The caller
    /// flushes; nothing here writes back the cache.
    pub fn draw_coordinate_chart(&mut self) {
        const DARK_A: u16 = 0x0841;
        const GRID: u16 = 0x7BEF;

        // A native-order fill avoids almost one million rotated address
        // calculations before the calibration grid becomes useful.
        self.fill(DARK_A);

        for x in (0..WIDTH).step_by(100) {
            self.draw_line(x, 0, x, HEIGHT - 1, GRID);
        }
        for y in (0..HEIGHT).step_by(100) {
            self.draw_line(0, y, WIDTH - 1, y, GRID);
        }

        // Exact logical centre axes use two pixels so they remain distinct
        // from the 100-pixel grid when viewed at arm's length.
        self.draw_line(WIDTH / 2, 0, WIDTH / 2, HEIGHT - 1, YELLOW);
        self.draw_line(WIDTH / 2 + 1, 0, WIDTH / 2 + 1, HEIGHT - 1, YELLOW);
        self.draw_line(0, HEIGHT / 2, WIDTH - 1, HEIGHT / 2, YELLOW);
        self.draw_line(0, HEIGHT / 2 + 1, WIDTH - 1, HEIGHT / 2 + 1, YELLOW);

        for x in (0..WIDTH).step_by(100) {
            let bytes = coordinate_label(b'X', x);
            // coordinate_label emits ASCII only.
            let label = unsafe { core::str::from_utf8_unchecked(&bytes) };
            let label_width = bytes.len() * 12;
            let label_x = x
                .saturating_sub(label_width / 2)
                .min(WIDTH.saturating_sub(label_width + 4))
                .max(4);
            self.draw_text(label_x, 8, label, 2, WHITE, Some(BLACK));
        }

        for y in (100..HEIGHT - 100).step_by(100) {
            let bytes = coordinate_label(b'Y', y);
            // coordinate_label emits ASCII only.
            let label = unsafe { core::str::from_utf8_unchecked(&bytes) };
            self.draw_text(8, y.saturating_sub(7), label, 2, WHITE, Some(BLACK));
        }

        self.draw_text(452, 52, "LOGICAL 1280X720 CW", 3, CYAN, Some(BLACK));
        self.draw_text(674, 378, "CENTER (640,360)", 3, YELLOW, Some(BLACK));
        self.draw_text(20, 42, "(0,0)", 2, WHITE, Some(BLACK));
        self.draw_text(1160, 42, "(1279,0)", 2, WHITE, Some(BLACK));
        self.draw_text(20, 686, "(0,719)", 2, WHITE, Some(BLACK));
        self.draw_text(1140, 686, "(1279,719)", 2, WHITE, Some(BLACK));

        let center_marker = [RED, GREEN, BLUE, WHITE];
        let _ = self.blit_rgb565(WIDTH / 2 - 1, HEIGHT / 2 - 1, 2, 2, &center_marker);

        // Four one-pixel inset borders reveal clipping independently on every
        // edge: red is the exact edge, followed by green, blue and white.
        self.stroke_rect(0, 0, WIDTH, HEIGHT, RED);
        self.stroke_rect(1, 1, WIDTH - 2, HEIGHT - 2, GREEN);
        self.stroke_rect(2, 2, WIDTH - 4, HEIGHT - 4, BLUE);
        self.stroke_rect(3, 3, WIDTH - 6, HEIGHT - 6, WHITE);
    }
}

/// One glyph placed on the screen, as the two painters below need it. They
/// take identical placement and differ only in what they do with the pixels
/// the glyph does not cover, so it is worth naming once.
struct Glyph<'a> {
    /// Framebuffer base, held raw so the rotation can be resolved once per
    /// column rather than once per pixel.
    pointer: *mut u16,
    x: usize,
    y: usize,
    /// Height of the 6x8 advance box in pixels, already clipped to the
    /// screen. Increasing logical Y is increasing native address, so this is
    /// also the length of each column's contiguous native run.
    run: usize,
    bits: &'a [u8; 5],
    scale: usize,
    foreground: u16,
}

impl Glyph<'_> {
    /// Native address of the top pixel of one box column, or `None` if that
    /// column falls off the right edge.
    ///
    /// Columns are walked backwards by both painters for the same reason
    /// `fill_rect` does it: CW rotation maps increasing logical X onto
    /// decreasing native rows, so this order leaves the box as a single
    /// forward write stream.
    ///
    /// # Safety
    /// `self.pointer` must be the base of a mapped framebuffer.
    unsafe fn column_base(&self, column: usize, offset_x: usize) -> Option<*mut u16> {
        let pixel_x = self.x + column * self.scale + offset_x;
        if pixel_x >= WIDTH {
            return None;
        }
        Some(unsafe { self.pointer.add(native_offset(pixel_x, self.y)) })
    }

    /// Paints the complete 6x8 advance box: the glyph in `foreground`, the
    /// rest -- including the letter-spacing column and the line-spacing row --
    /// in `background`.
    ///
    /// Every pixel is written, so each column of the box is one contiguous
    /// native run and the rotation costs one multiply per column instead of
    /// one per pixel: 12 for a scale-2 cell rather than 192.
    ///
    /// # Safety
    /// `self.pointer` must be the base of a mapped framebuffer.
    unsafe fn paint_opaque(&self, background: u16) {
        for column in (0..6).rev() {
            // Column 5 is the letter spacing: background for its full height.
            let bits = if column < 5 { self.bits[column] } else { 0 };
            for offset_x in (0..self.scale).rev() {
                let Some(base) = (unsafe { self.column_base(column, offset_x) }) else {
                    continue;
                };
                for row in 0..8 {
                    let start = row * self.scale;
                    if start >= self.run {
                        break;
                    }
                    // Row 7 is the line spacing, matching column 5 above.
                    let color = if row < 7 && bits & (1 << row) != 0 {
                        self.foreground
                    } else {
                        background
                    };
                    for index in start..(start + self.scale).min(self.run) {
                        unsafe { base.add(index).write_volatile(color) };
                    }
                }
            }
        }
    }

    /// Paints only the glyph's own pixels, leaving whatever is under the rest
    /// of the box. For callers that have established the background
    /// themselves, typically by clearing the screen in one pass.
    ///
    /// What is written here is a sparse subset -- a third of the box at most,
    /// and nothing at all for a space -- so this skips rather than writes:
    /// an empty glyph column drops out before any address is computed, and
    /// the spacing column and row are never visited.
    ///
    /// # Safety
    /// `self.pointer` must be the base of a mapped framebuffer.
    unsafe fn paint_sparse(&self) {
        for column in (0..5).rev() {
            let bits = self.bits[column];
            if bits == 0 {
                continue;
            }
            for offset_x in (0..self.scale).rev() {
                let Some(base) = (unsafe { self.column_base(column, offset_x) }) else {
                    continue;
                };
                for row in 0..7 {
                    if bits & (1 << row) == 0 {
                        continue;
                    }
                    let start = row * self.scale;
                    if start >= self.run {
                        break;
                    }
                    for index in start..(start + self.scale).min(self.run) {
                        unsafe { base.add(index).write_volatile(self.foreground) };
                    }
                }
            }
        }
    }
}

/// Converts 1280x720 landscape coordinates to the panel-native 720x1280
/// row-major framebuffer: (x, y) -> (y, 1279 - x).
#[inline(always)]
fn native_offset(x: usize, y: usize) -> usize {
    (NATIVE_HEIGHT - 1 - x) * NATIVE_WIDTH + y
}

/// Describes the framebuffer to the 2D-DMA in its own scan orientation: 720
/// pixels per row, 1280 rows, whatever the logical geometry above says.
fn native_picture(pointer: *mut u16) -> dma2d::Picture {
    dma2d::Picture {
        buffer: pointer as usize,
        width: NATIVE_WIDTH,
        height: NATIVE_HEIGHT,
    }
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
