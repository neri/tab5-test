//! CW-rotated RGB565 drawing primitives backed by external PSRAM.

use crate::dma2d;
use crate::font;
use crate::ppa;
use crate::psram::{HEIGHT as NATIVE_HEIGHT, Psram, WIDTH as NATIVE_WIDTH};

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
///   12x16                         19 ppa     36 cpu
///   24x32                         22 ppa     44 cpu
///   48x64                         52 ppa    273 cpu
///   96x128                       334 ppa   1351 cpu
///   1280x720 (full screen)     13267 ppa  93548 cpu
/// ```
///
/// The smallest rectangle measured was the console cell of the time, 12x16;
/// a cell is 8x16 now, which is smaller still and on the same side of the
/// threshold. PPA is faster even at that size in this run, but the absolute
/// saving there is only 17 us and every DMA write competes with scanout.
/// 24x32 is therefore kept as the conservative threshold: the console's
/// per-cell repaints -- by far the most frequent fills, and the ones on the
/// cursor-blink path -- stay on the CPU, while larger repaints take the
/// increasingly decisive DMA gain.
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
#[allow(dead_code)]
pub const MAGENTA: u16 = 0xF81F;
pub const YELLOW: u16 = 0xFFE0;

pub struct Framebuffer {
    memory: Psram,
}

struct UiSurface(*mut u16);

impl tab5_ui_font::PixelSurface for UiSurface {
    fn width(&self) -> usize {
        WIDTH
    }

    fn height(&self) -> usize {
        HEIGHT
    }

    fn read(&self, x: usize, y: usize) -> u16 {
        unsafe { self.0.add(native_offset(x, y)).read_volatile() }
    }

    fn write(&mut self, x: usize, y: usize, color: u16) {
        unsafe { self.0.add(native_offset(x, y)).write_volatile(color) }
    }
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
        let _ = self.fill_with_cpu(color);
    }

    /// The store-loop clear, used when the PPA is unavailable or refused the
    /// transfer. Keeping it means a failed PPA bring-up costs speed and
    /// nothing else.
    fn fill_with_cpu(&mut self, color: u16) -> bool {
        let Some(pointer) = self.memory.framebuffer() else {
            return false;
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
        true
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
        let _ = self.fill_rect_with_cpu(x, y, width, height, color);
    }

    /// Forces the CPU store path for display diagnostics.
    ///
    /// Production code must use `fill_rect`, whose job is to select the best
    /// available path. The benchmark needs the opposite contract: a named
    /// path that cannot silently route a large rectangle back through the
    /// PPA. This writes pixels only; the caller times cache synchronisation
    /// separately or follows it with `flush_rect`.
    pub(crate) fn diagnostic_fill_rect_with_cpu(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        color: u16,
    ) -> bool {
        if x == 0 && y == 0 && width >= WIDTH && height >= HEIGHT {
            return self.fill_with_cpu(color);
        }
        self.fill_rect_with_cpu(x, y, width, height, color)
    }

    /// CPU-only rectangle implementation shared by production fallback and
    /// the diagnostic path above.
    fn fill_rect_with_cpu(
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
        let x_end = x.saturating_add(width).min(WIDTH);
        let y_end = y.saturating_add(height).min(HEIGHT);
        if x.min(WIDTH) >= x_end || y.min(HEIGHT) >= y_end {
            return false;
        }
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
        true
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

    /// Draws one 16 pixel glyph, scaled by a whole number.
    ///
    /// This does no lookup and knows nothing about text: it paints the pixels
    /// of the glyph it is handed, in the box that glyph's own width describes
    /// -- 8 columns wide for half-width, 16 for full-width, and 16 for a
    /// combining mark, which has no advance but still covers the character it
    /// is painted over.
    ///
    /// `background` fills the rest of that box, which is what lets a caller
    /// repaint a cell in one call rather than clearing it first. `None` writes
    /// only the glyph's own pixels, onto whatever is already there. Combining
    /// marks have to use `None`: their box overlaps the character before them
    /// on one side and the character after them on the other, so an opaque
    /// paint would erase a neighbour.
    ///
    /// The two cases share nothing but this entry. Opaque writes every pixel
    /// of a fixed box, so each column is one contiguous native run; sparse
    /// writes a sparse subset and skips empty columns and rows outright.
    #[inline(never)]
    pub fn draw_glyph(
        &mut self,
        x: usize,
        y: usize,
        glyph: &font::Glyph,
        scale: usize,
        foreground: u16,
        background: Option<u16>,
    ) {
        let Some(pointer) = self.memory.framebuffer() else {
            return;
        };
        let scale = scale.max(1);
        // Clip the box's pixel rows once. Increasing logical Y is increasing
        // native address, so this is also the length of each column's run.
        let run = y
            .saturating_add(font::HEIGHT * scale)
            .min(HEIGHT)
            .saturating_sub(y);
        if run == 0 {
            return;
        }
        let painter = WideGlyph {
            pointer,
            x,
            y,
            run,
            columns: &glyph.columns,
            width: glyph.width(),
            scale,
            foreground,
        };
        match background {
            Some(background) => unsafe { painter.paint_opaque(background) },
            None => unsafe { painter.paint_sparse() },
        }
    }

    /// Draws UTF-8 text and returns the width it occupied, in pixels.
    ///
    /// Every character advances by [`font::advance`], the same function the
    /// browser's line breaking and hit testing measure with, so what is drawn
    /// and what was measured cannot disagree. Characters the font does not
    /// cover are drawn as a box of that same width rather than skipped: a
    /// blank would claim the text ended there.
    ///
    /// Combining marks are painted over the character before them and take no
    /// width of their own. One that arrives with no character before it --
    /// leading a string, or after a newline -- has nothing to combine with, so
    /// it is drawn as a full-width box instead of vanishing.
    ///
    /// `\n` returns to the starting x and moves down one 16 pixel line. The
    /// returned width is the widest line, which for the single-line case every
    /// caller here uses is just the pen movement.
    pub fn draw_text(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        scale: usize,
        foreground: u16,
        background: Option<u16>,
    ) -> usize {
        let scale = scale.max(1);
        let origin_x = x;
        let (mut cursor_x, mut cursor_y) = (x, y);
        let mut widest = 0;
        // In pixels, already scaled: how far back a combining mark has to go
        // to land on the character it belongs to. Zero means there is none.
        let mut previous_advance = 0;
        for character in text.chars() {
            if character == '\n' {
                widest = widest.max(cursor_x - origin_x);
                cursor_x = origin_x;
                cursor_y = cursor_y.saturating_add(font::HEIGHT * scale);
                previous_advance = 0;
                continue;
            }
            let glyph = font::glyph_or_replacement(character);
            if glyph.advance == 0 {
                if previous_advance == 0 {
                    let orphan = font::glyph_or_replacement(char::REPLACEMENT_CHARACTER);
                    self.draw_glyph(cursor_x, cursor_y, &orphan, scale, foreground, background);
                    previous_advance = orphan.advance as usize * scale;
                    cursor_x = cursor_x.saturating_add(previous_advance);
                    continue;
                }
                let over = cursor_x.saturating_sub(previous_advance);
                self.draw_glyph(over, cursor_y, &glyph, scale, foreground, None);
                continue;
            }
            self.draw_glyph(cursor_x, cursor_y, &glyph, scale, foreground, background);
            previous_advance = glyph.advance as usize * scale;
            cursor_x = cursor_x.saturating_add(previous_advance);
        }
        widest.max(cursor_x - origin_x)
    }

    /// Draws normal-UI text with pre-rasterised A4 Latin and Japanese glyphs.
    /// Console and direct-ROM diagnostics deliberately keep using `draw_text`,
    /// preserving their fixed 8x16 ASCII cell contract.
    pub fn draw_ui_text(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        style: font::UiTextStyle,
        foreground: u16,
        background: Option<u16>,
    ) -> usize {
        self.draw_ui_text_inner::<false>(x, y, text, style, foreground, background)
    }

    /// Diagnostic-only counterpart to `draw_ui_text`: it uses the exact same
    /// A4 glyphs and metrics but thresholds Latin coverage to one bit.
    pub fn draw_ui_text_1bpp(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        style: font::UiTextStyle,
        foreground: u16,
        background: Option<u16>,
    ) -> usize {
        self.draw_ui_text_inner::<true>(x, y, text, style, foreground, background)
    }

    fn draw_ui_text_inner<const BINARY: bool>(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        style: font::UiTextStyle,
        foreground: u16,
        background: Option<u16>,
    ) -> usize {
        let origin_x = x;
        let (mut cursor_x, mut cursor_y) = (x, y);
        let mut widest = 0usize;
        let mut previous_advance = 0usize;
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            if character == '\n' {
                widest = widest.max(cursor_x.saturating_sub(origin_x));
                cursor_x = origin_x;
                cursor_y = cursor_y.saturating_add(style.size as usize);
                previous_advance = 0;
                continue;
            }
            let combining_cluster = characters
                .peek()
                .is_some_and(|next| tab5_ui_font::is_combining(*next));
            if tab5_ui_font::is_english_latin(character) && !combining_cluster {
                let glyph = tab5_ui_font::glyph(style, character)
                    .expect("validated UI Latin glyph is missing");
                self.draw_ui_glyph::<BINARY>(
                    cursor_x, cursor_y, &glyph, style, foreground, background,
                );
                cursor_x = cursor_x.saturating_add(glyph.advance as usize);
                previous_advance = 0;
                continue;
            }

            if let Some(glyph) = tab5_ui_font::japanese_glyph(character) {
                let scale = tab5_ui_font::japanese_scale(style);
                if glyph.advance == 0 {
                    if previous_advance == 0 {
                        let replacement = font::glyph_or_replacement(char::REPLACEMENT_CHARACTER);
                        self.draw_glyph(
                            cursor_x,
                            cursor_y,
                            &replacement,
                            style.fallback_scale(),
                            foreground,
                            background,
                        );
                        previous_advance = replacement.advance as usize * style.fallback_scale();
                        cursor_x = cursor_x.saturating_add(previous_advance);
                    } else {
                        self.draw_ui_glyph_scaled::<BINARY>(
                            cursor_x.saturating_sub(previous_advance),
                            cursor_y,
                            &glyph,
                            tab5_ui_font::line_metrics(font::UiTextStyle::new(
                                font::UiFace::Japanese,
                                16,
                            )),
                            scale,
                            foreground,
                            None,
                        );
                    }
                } else {
                    self.draw_ui_glyph_scaled::<BINARY>(
                        cursor_x,
                        cursor_y,
                        &glyph,
                        tab5_ui_font::line_metrics(font::UiTextStyle::new(
                            font::UiFace::Japanese,
                            16,
                        )),
                        scale,
                        foreground,
                        background,
                    );
                    previous_advance = glyph.advance as usize * scale;
                    cursor_x = cursor_x.saturating_add(previous_advance);
                }
                continue;
            }

            let scale = style.fallback_scale();
            let glyph = font::glyph_or_replacement(character);
            if glyph.advance == 0 {
                if previous_advance == 0 {
                    let replacement = font::glyph_or_replacement(char::REPLACEMENT_CHARACTER);
                    self.draw_glyph(
                        cursor_x,
                        cursor_y,
                        &replacement,
                        scale,
                        foreground,
                        background,
                    );
                    previous_advance = replacement.advance as usize * scale;
                    cursor_x = cursor_x.saturating_add(previous_advance);
                } else {
                    self.draw_glyph(
                        cursor_x.saturating_sub(previous_advance),
                        cursor_y,
                        &glyph,
                        scale,
                        foreground,
                        None,
                    );
                }
            } else {
                self.draw_glyph(cursor_x, cursor_y, &glyph, scale, foreground, background);
                previous_advance = glyph.advance as usize * scale;
                cursor_x = cursor_x.saturating_add(previous_advance);
            }
        }
        widest.max(cursor_x.saturating_sub(origin_x))
    }

    /// Normal GUI policy: proportional DejaVu Sans for Latin and the 16 pixel
    /// Noto CJK A4 strike for Japanese (integer-scaled at 32 pixels).
    pub fn draw_gui_text(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        scale: usize,
        foreground: u16,
        background: Option<u16>,
    ) -> usize {
        self.draw_ui_text(
            x,
            y,
            text,
            font::UiTextStyle::new(font::UiFace::Sans, if scale >= 2 { 32 } else { 16 }),
            foreground,
            background,
        )
    }

    /// Draws the longest whole-character prefix fitting `budget` pixels.
    /// Measurement and drawing share the same style, so the following fixed
    /// column cannot be crossed by a proportional label.
    pub fn draw_gui_text_clipped(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        budget: usize,
        scale: usize,
        foreground: u16,
        background: Option<u16>,
    ) -> usize {
        let style = font::UiTextStyle::new(font::UiFace::Sans, if scale >= 2 { 32 } else { 16 });
        let mut end = 0usize;
        for (offset, character) in text.char_indices() {
            let candidate = offset + character.len_utf8();
            if font::ui_text_width(&text[..candidate], style) > budget {
                break;
            }
            end = candidate;
        }
        self.draw_ui_text(x, y, &text[..end], style, foreground, background)
    }

    fn draw_ui_glyph<const BINARY: bool>(
        &mut self,
        x: usize,
        y: usize,
        glyph: &font::UiGlyph,
        style: font::UiTextStyle,
        foreground: u16,
        background: Option<u16>,
    ) {
        self.draw_ui_glyph_scaled::<BINARY>(
            x,
            y,
            glyph,
            tab5_ui_font::line_metrics(style),
            1,
            foreground,
            background,
        );
    }

    fn draw_ui_glyph_scaled<const BINARY: bool>(
        &mut self,
        x: usize,
        y: usize,
        glyph: &font::UiGlyph,
        metrics: tab5_ui_font::LineMetrics,
        scale: usize,
        foreground: u16,
        background: Option<u16>,
    ) {
        let Some(pointer) = self.memory.framebuffer() else {
            return;
        };
        let mut surface = UiSurface(pointer);
        if BINARY {
            tab5_ui_font::paint_glyph_1bpp_scaled(
                &mut surface,
                x as isize,
                y as isize,
                glyph,
                metrics,
                scale,
                foreground,
                background,
            );
        } else {
            tab5_ui_font::paint_glyph_scaled(
                &mut surface,
                x as isize,
                y as isize,
                glyph,
                metrics,
                scale,
                foreground,
                background,
            );
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
        let filled = self.ppa_fill_rect_raw(x_start, y_start, width, height, color);
        // Invalidate even on failure: a partial fill still leaves the cache
        // disagreeing with PSRAM.
        self.flush_rect(x_start, y_start, width, height);
        filled
    }

    /// Forces a PPA transfer without performing cache maintenance.
    ///
    /// This is intentionally diagnostic-only. The caller must first ensure
    /// that no dirty or clean framebuffer cache line can outlive the DMA, and
    /// must not let the CPU touch the destination until coherency has been
    /// restored. `displaybench ppa-raw` invalidates the whole framebuffer once
    /// before its loop and then keeps the CPU away from it until the loop is
    /// complete. Production callers must use `ppa_fill_rect` instead.
    pub(crate) fn diagnostic_ppa_fill_rect_raw(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        color: u16,
    ) -> bool {
        let x_start = x.min(WIDTH);
        let y_start = y.min(HEIGHT);
        let x_end = x.saturating_add(width).min(WIDTH);
        let y_end = y.saturating_add(height).min(HEIGHT);
        if x_start >= x_end || y_start >= y_end {
            return false;
        }
        self.ppa_fill_rect_raw(x_start, y_start, x_end - x_start, y_end - y_start, color)
    }

    /// Starts the PPA for a rectangle already clipped to the logical screen.
    /// Cache ownership is deliberately absent from this primitive.
    fn ppa_fill_rect_raw(
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
        // Native row `NATIVE_HEIGHT - 1 - x` holds logical column x, so the
        // block's top row is the one belonging to the rectangle's right edge.
        let picture = native_picture(pointer);
        let destination = dma2d::Block {
            picture: &picture,
            x: y,
            y: NATIVE_HEIGHT - x - width,
        };
        ppa::fill_rgb565(&destination, height, width, color)
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

    /// Paints the coordinate calibration chart: a 100-pixel grid, the exact
    /// logical centre axes, labelled corners, and four one-pixel inset borders.
    ///
    /// Every number on it is a logical coordinate, so comparing the chart
    /// against a ruler on the panel is what verifies the CW rotation, the
    /// clipping at each edge, and the logical-to-native mapping. The caller
    /// flushes; nothing here writes back the cache.
    /// One of the two right-hand corner labels, pushed in from the edge by
    /// the same margin its left-hand partner sits at.
    fn draw_corner_label(&mut self, y: usize, text: &str) {
        let x = WIDTH.saturating_sub(font::text_width(text) + 20);
        self.draw_text(x, y, text, 1, WHITE, Some(BLACK));
    }

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
            let label_width = font::text_width(label);
            let label_x = x
                .saturating_sub(label_width / 2)
                .min(WIDTH.saturating_sub(label_width + 4))
                .max(4);
            self.draw_text(label_x, 8, label, 1, WHITE, Some(BLACK));
        }

        for y in (100..HEIGHT - 100).step_by(100) {
            let bytes = coordinate_label(b'Y', y);
            // coordinate_label emits ASCII only.
            let label = unsafe { core::str::from_utf8_unchecked(&bytes) };
            self.draw_text(
                8,
                y.saturating_sub(font::HEIGHT / 2),
                label,
                1,
                WHITE,
                Some(BLACK),
            );
        }

        // Centred and corner-aligned from the text's own width rather than
        // from a hand-counted cell count, which is what let the old fixed
        // coordinates go stale every time a string changed.
        let title = "Logical 1280x720 CW";
        // At scale 2 the drawn width is twice the measured one, so half of it
        // is the measured width itself.
        let title_x = WIDTH / 2 - font::text_width(title);
        self.draw_text(title_x, 52, title, 2, CYAN, Some(BLACK));
        self.draw_text(674, 378, "Center (640,360)", 2, YELLOW, Some(BLACK));
        self.draw_text(20, 42, "(0,0)", 1, WHITE, Some(BLACK));
        self.draw_corner_label(42, "(1279,0)");
        self.draw_text(20, 686, "(0,719)", 1, WHITE, Some(BLACK));
        self.draw_corner_label(686, "(1279,719)");

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

/// One 16 pixel glyph placed on the screen, as the two painters below need
/// it.
///
/// They take identical placement and differ only in what they do with the
/// pixels the glyph does not cover, so it is worth naming once. The box's
/// width comes from the caller rather than being fixed, because a glyph is
/// 8 pixels wide, 16, or -- for a combining mark -- 16 with no advance.
struct WideGlyph<'a> {
    /// Framebuffer base, held raw so the rotation can be resolved once per
    /// column rather than once per pixel.
    pointer: *mut u16,
    x: usize,
    y: usize,
    /// Height of the box in pixels, already clipped to the screen. Increasing
    /// logical Y is increasing native address, so this is also the length of
    /// each column's contiguous native run.
    run: usize,
    columns: &'a [u16; font::MAX_WIDTH],
    /// Columns to paint: the glyph's advance, or the full 16 for a combining
    /// mark. Columns past this are zero in the data, but an opaque paint would
    /// still fill them with background, so the box has to stop here.
    width: usize,
    scale: usize,
    foreground: u16,
}

impl WideGlyph<'_> {
    /// Native address of the top pixel of one box column, or `None` if that
    /// column falls off the right edge.
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

    /// Paints the complete box: the glyph in `foreground`, the rest in
    /// `background`.
    ///
    /// Columns are walked backwards for the same reason `fill_rect` does it:
    /// CW rotation maps increasing logical X onto decreasing native rows, so
    /// this order leaves the box as a single forward write stream.
    ///
    /// # Safety
    /// `self.pointer` must be the base of a mapped framebuffer.
    unsafe fn paint_opaque(&self, background: u16) {
        for column in (0..self.width).rev() {
            let bits = self.columns[column];
            for offset_x in (0..self.scale).rev() {
                let Some(base) = (unsafe { self.column_base(column, offset_x) }) else {
                    continue;
                };
                for row in 0..font::HEIGHT {
                    let start = row * self.scale;
                    if start >= self.run {
                        break;
                    }
                    let color = if bits & (1 << row) != 0 {
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
    /// of the box.
    ///
    /// # Safety
    /// `self.pointer` must be the base of a mapped framebuffer.
    unsafe fn paint_sparse(&self) {
        for column in (0..self.width).rev() {
            let bits = self.columns[column];
            if bits == 0 {
                continue;
            }
            for offset_x in (0..self.scale).rev() {
                let Some(base) = (unsafe { self.column_base(column, offset_x) }) else {
                    continue;
                };
                for row in 0..font::HEIGHT {
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
