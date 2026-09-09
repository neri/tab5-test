//! Pre-rasterised anti-aliased Latin fonts for normal GUI surfaces.
//!
//! The checked-in blob contains DejaVu Sans and Sans Mono at 16, 24 and 32
//! pixels. The build-time DROM image is an LZ4 raw-block container expanded
//! into PSRAM at startup; runtime code performs only bounds-checked table
//! lookup and A4 blending from that copy. FreeType, Pillow and the TTF sources
//! are generator-side tools.

#![cfg_attr(not(test), no_std)]

#[cfg(not(feature = "drom-direct"))]
use core::ptr;
#[cfg(not(feature = "drom-direct"))]
use core::sync::atomic::{AtomicPtr, Ordering};

#[cfg(not(feature = "drom-direct"))]
const COMPRESSED_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tab5-ui-fonts.lz4"));
#[cfg(any(test, not(target_arch = "riscv32"), feature = "drom-direct"))]
const TEST_DATA: &[u8] = include_bytes!("../data/tab5-ui-fonts.bin");
#[cfg(not(feature = "drom-direct"))]
static ACTIVE_DATA: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());

pub const HEADER_BYTES: usize = 32;
const STRIKE_BYTES: usize = 16;
const GLYPH_BYTES: usize = 16;

include!(concat!(env!("OUT_DIR"), "/font_meta.rs"));

fn u16_from(bytes: &[u8], offset: usize) -> u16 {
    bytes[offset] as u16 | (bytes[offset + 1] as u16) << 8
}

fn u32_from(bytes: &[u8], offset: usize) -> u32 {
    bytes[offset] as u32
        | (bytes[offset + 1] as u32) << 8
        | (bytes[offset + 2] as u32) << 16
        | (bytes[offset + 3] as u32) << 24
}

fn u16_at(offset: usize) -> u16 {
    u16_from(data(), offset)
}

fn u32_at(offset: usize) -> u32 {
    u32_from(data(), offset)
}

#[cfg_attr(not(test), allow(dead_code))]
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(not(feature = "drom-direct"))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstallError {
    Decode(tab5_font_codec::DecodeError),
    BadHeader,
    BadLayout,
    BadCrc,
    AlreadyInstalled,
}

#[cfg(not(feature = "drom-direct"))]
fn decode_and_validate(destination: &mut [u8]) -> Result<(), InstallError> {
    tab5_font_codec::decode(COMPRESSED_DATA, destination).map_err(InstallError::Decode)?;
    if destination[..4] != *b"T5A4"
        || u16_from(destination, 4) != 1
        || u16_from(destination, 6) as usize != HEADER_BYTES
    {
        return Err(InstallError::BadHeader);
    }
    if u16_from(destination, 8) as usize != STRIKE_COUNT
        || u16_from(destination, 10) as usize != GLYPH_COUNT
        || u32_from(destination, 12) as usize != STRIKES_OFFSET
        || u32_from(destination, 16) as usize != GLYPHS_OFFSET
        || u32_from(destination, 20) as usize != BITMAPS_OFFSET
        || u32_from(destination, 24) as usize != TOTAL_BYTES
        || STRIKE_COUNT != 6
        || GLYPH_COUNT != 1260
        || STRIKES_OFFSET != HEADER_BYTES
        || GLYPHS_OFFSET != STRIKES_OFFSET + STRIKE_COUNT * STRIKE_BYTES
        || BITMAPS_OFFSET != GLYPHS_OFFSET + GLYPH_COUNT * GLYPH_BYTES
        || TOTAL_BYTES != STORAGE_BYTES
    {
        return Err(InstallError::BadLayout);
    }
    if u32_from(destination, 28) != CRC32 || crc32(&destination[HEADER_BYTES..]) != CRC32 {
        return Err(InstallError::BadCrc);
    }
    Ok(())
}

/// Expands the build-time LZ4 DROM blob into permanent PSRAM storage.
/// This must run exactly once before any firmware UI-font lookup.
#[cfg(not(feature = "drom-direct"))]
pub fn install_psram(destination: &'static mut [u8]) -> Result<(), InstallError> {
    decode_and_validate(destination)?;
    ACTIVE_DATA
        .compare_exchange(
            ptr::null_mut(),
            destination.as_mut_ptr(),
            Ordering::Release,
            Ordering::Relaxed,
        )
        .map(|_| ())
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// Address of the active decoded blob, for boot diagnostics.
#[cfg(not(feature = "drom-direct"))]
pub fn psram_address() -> Option<usize> {
    let pointer = ACTIVE_DATA.load(Ordering::Acquire);
    (!pointer.is_null()).then_some(pointer as usize)
}

#[cfg(all(not(test), target_arch = "riscv32", not(feature = "drom-direct")))]
fn data() -> &'static [u8] {
    let pointer = ACTIVE_DATA.load(Ordering::Acquire);
    assert!(!pointer.is_null(), "UI font PSRAM is not installed");
    unsafe { core::slice::from_raw_parts(pointer, STORAGE_BYTES) }
}

#[cfg(any(test, not(target_arch = "riscv32"), feature = "drom-direct"))]
fn data() -> &'static [u8] {
    TEST_DATA
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Face {
    Sans = 0,
    Mono = 1,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextStyle {
    pub face: Face,
    pub size: u8,
}

impl TextStyle {
    pub const BODY: Self = Self {
        face: Face::Sans,
        size: 16,
    };
    pub const HEADING: Self = Self {
        face: Face::Sans,
        size: 32,
    };
    pub const MONO: Self = Self {
        face: Face::Mono,
        size: 16,
    };

    pub const fn new(face: Face, size: u8) -> Self {
        Self { face, size }
    }

    pub const fn legacy_scale(self) -> usize {
        if self.size >= 32 { 2 } else { 1 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineMetrics {
    pub size: u8,
    pub ascent: u8,
    pub descent: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub code_point: u32,
    pub x_bearing: i8,
    /// Top of the bitmap relative to the line box.
    pub y: i8,
    pub width: u8,
    pub height: u8,
    pub advance: u8,
    bitmap_offset: u32,
    bitmap_length: u16,
}

impl Glyph {
    pub fn alpha(&self, x: usize, y: usize) -> u8 {
        if x >= self.width as usize || y >= self.height as usize {
            return 0;
        }
        let index = x * self.height as usize + y;
        let byte = data()[BITMAPS_OFFSET + self.bitmap_offset as usize + index / 2];
        if index & 1 == 0 {
            byte >> 4
        } else {
            byte & 0x0f
        }
    }

    pub fn bitmap_len(&self) -> usize {
        self.bitmap_length as usize
    }
}

fn strike(style: TextStyle) -> Option<(usize, LineMetrics)> {
    for index in 0..STRIKE_COUNT {
        let at = STRIKES_OFFSET + index * STRIKE_BYTES;
        if data()[at] == style.face as u8 && data()[at + 1] == style.size {
            return Some((
                index,
                LineMetrics {
                    size: data()[at + 1],
                    ascent: data()[at + 2],
                    descent: data()[at + 3],
                },
            ));
        }
    }
    None
}

pub fn line_metrics(style: TextStyle) -> LineMetrics {
    strike(style)
        .map(|(_, metrics)| metrics)
        .unwrap_or(LineMetrics {
            size: 16,
            ascent: 12,
            descent: 4,
        })
}

fn glyph_at(index: usize) -> Glyph {
    let at = GLYPHS_OFFSET + index * GLYPH_BYTES;
    Glyph {
        code_point: u32_at(at),
        x_bearing: data()[at + 4] as i8,
        y: data()[at + 5] as i8,
        width: data()[at + 6],
        height: data()[at + 7],
        advance: data()[at + 8],
        bitmap_offset: u32_at(at + 10),
        bitmap_length: u16_at(at + 14),
    }
}

pub fn glyph(style: TextStyle, character: char) -> Option<Glyph> {
    let (strike_index, _) = strike(style)?;
    let strike_at = STRIKES_OFFSET + strike_index * STRIKE_BYTES;
    let first = u32_at(strike_at + 4) as usize;
    let count = u16_at(strike_at + 8) as usize;
    let target = character as u32;
    let (mut low, mut high) = (first, first + count);
    while low < high {
        let middle = (low + high) / 2;
        let candidate = glyph_at(middle);
        if target < candidate.code_point {
            high = middle;
        } else if target > candidate.code_point {
            low = middle + 1;
        } else {
            return Some(candidate);
        }
    }
    None
}

pub const fn is_english_latin(character: char) -> bool {
    matches!(character as u32,
        0x20..=0x7e | 0xa0..=0xac | 0xae..=0xff | 0x2010..=0x2015 |
        0x2018..=0x201f | 0x2022 | 0x2026 | 0x2032 | 0x2033 | 0x20ac | 0x2122)
}

/// Width used by layout and drawing. A Latin base followed by a combining
/// mark routes as one legacy cluster, avoiding a mark positioned against a
/// proportional advance.
pub fn text_width(text: &str, style: TextStyle) -> usize {
    let mut chars = text.chars().peekable();
    let mut width = 0usize;
    while let Some(character) = chars.next() {
        let combining_cluster = chars
            .peek()
            .is_some_and(|next| tab5_font::is_combining(*next));
        if is_english_latin(character) && !combining_cluster {
            width += glyph(style, character)
                .expect("validated Latin glyph missing")
                .advance as usize;
        } else {
            width += tab5_font::advance(character) as usize * style.legacy_scale();
        }
    }
    width
}

/// Blend one A4 coverage value in RGB565 channel space.
pub fn blend_rgb565(foreground: u16, background: u16, alpha: u8) -> u16 {
    let alpha = alpha.min(15) as u32;
    if alpha == 0 {
        return background;
    }
    if alpha == 15 {
        return foreground;
    }
    let inverse = 15 - alpha;
    let blend = |shift: u32, mask: u32| -> u16 {
        let f = (foreground as u32 >> shift) & mask;
        let b = (background as u32 >> shift) & mask;
        (((f * alpha + b * inverse + 7) / 15) << shift) as u16
    };
    blend(11, 0x1f) | blend(5, 0x3f) | blend(0, 0x1f)
}

/// Minimal target-independent surface used by the A4 painter. The firmware
/// maps this to its rotated volatile framebuffer; host tests use a guarded
/// row-major buffer.
pub trait PixelSurface {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    fn read(&self, x: usize, y: usize) -> u16;
    fn write(&mut self, x: usize, y: usize, color: u16);
}

/// A4 value used by the diagnostic 1-bit renderer.  Keeping this conversion
/// here makes the A4 and 1-bit comparison differ only in coverage handling;
/// glyph metrics, bearings and source outlines remain identical.
pub const BINARY_ALPHA_THRESHOLD: u8 = 8;

pub fn paint_glyph<S: PixelSurface>(
    surface: &mut S,
    origin_x: isize,
    origin_y: isize,
    glyph: &Glyph,
    metrics: LineMetrics,
    foreground: u16,
    background: Option<u16>,
) {
    paint_glyph_inner::<S, false>(
        surface, origin_x, origin_y, glyph, metrics, foreground, background,
    );
}

/// Paints the same stored A4 glyph after thresholding its coverage to 1 bit.
///
/// This is a visual/performance baseline for `fonttest`, not a normal GUI
/// rendering policy.  Product surfaces must continue to use [`paint_glyph`].
pub fn paint_glyph_1bpp<S: PixelSurface>(
    surface: &mut S,
    origin_x: isize,
    origin_y: isize,
    glyph: &Glyph,
    metrics: LineMetrics,
    foreground: u16,
    background: Option<u16>,
) {
    paint_glyph_inner::<S, true>(
        surface, origin_x, origin_y, glyph, metrics, foreground, background,
    );
}

fn paint_glyph_inner<S: PixelSurface, const BINARY: bool>(
    surface: &mut S,
    origin_x: isize,
    origin_y: isize,
    glyph: &Glyph,
    metrics: LineMetrics,
    foreground: u16,
    background: Option<u16>,
) {
    if let Some(color) = background {
        for x in 0..glyph.advance as isize {
            for y in 0..metrics.size as isize {
                write_clipped(surface, origin_x + x, origin_y + y, color);
            }
        }
    }
    for x in 0..glyph.width as usize {
        for y in 0..glyph.height as usize {
            let alpha = glyph.alpha(x, y);
            if alpha == 0 {
                continue;
            }
            let pixel_x = origin_x + glyph.x_bearing as isize + x as isize;
            let pixel_y = origin_y + glyph.y as isize + y as isize;
            if pixel_x < 0
                || pixel_y < 0
                || pixel_x >= surface.width() as isize
                || pixel_y >= surface.height() as isize
            {
                continue;
            }
            let base =
                background.unwrap_or_else(|| surface.read(pixel_x as usize, pixel_y as usize));
            let color = if BINARY {
                if alpha < BINARY_ALPHA_THRESHOLD {
                    continue;
                }
                foreground
            } else {
                blend_rgb565(foreground, base, alpha)
            };
            surface.write(pixel_x as usize, pixel_y as usize, color);
        }
    }
}

fn write_clipped<S: PixelSurface>(surface: &mut S, x: isize, y: isize, color: u16) {
    if x >= 0 && y >= 0 && x < surface.width() as isize && y < surface.height() as isize {
        surface.write(x as usize, y as usize, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(feature = "drom-direct"))]
    fn firmware_blob_is_compressed_and_restores_exact_source() {
        assert_eq!(&COMPRESSED_DATA[..4], b"T5L4");
        assert_ne!(&COMPRESSED_DATA[..4], b"T5A4");
        assert_eq!(COMPRESSED_DATA.len(), COMPRESSED_BYTES);
        let mut restored = std::vec![0u8; STORAGE_BYTES];
        decode_and_validate(&mut restored).unwrap();
        assert_eq!(restored, TEST_DATA);
    }

    #[test]
    fn blob_crc_and_bounds_match() {
        assert_eq!(crc32(&data()[HEADER_BYTES..]), CRC32);
        for i in 0..GLYPH_COUNT {
            let glyph = glyph_at(i);
            assert_eq!(
                glyph.bitmap_len(),
                (glyph.width as usize * glyph.height as usize + 1) / 2
            );
            assert!(
                glyph.bitmap_offset as usize + glyph.bitmap_len() <= data().len() - BITMAPS_OFFSET
            );
            if glyph.code_point != 0x20 && glyph.code_point != 0xa0 {
                assert!(
                    (0..glyph.width as usize)
                        .any(|x| (0..glyph.height as usize).any(|y| glyph.alpha(x, y) != 0)),
                    "U+{:04X} has an empty bitmap",
                    glyph.code_point
                );
            }
        }
    }

    #[test]
    fn every_required_strike_has_the_complete_set() {
        for face in [Face::Sans, Face::Mono] {
            for size in [16, 24, 32] {
                let style = TextStyle::new(face, size);
                let mut count = 0;
                for cp in 0..=0x2122 {
                    if let Some(character) = char::from_u32(cp) {
                        if is_english_latin(character) {
                            assert!(glyph(style, character).is_some(), "missing U+{cp:04X}");
                            count += 1;
                        }
                    }
                }
                assert_eq!(count, 210);
            }
        }
    }

    #[test]
    fn proportional_and_monospace_contracts_hold() {
        assert!(text_width("III", TextStyle::BODY) < text_width("MMM", TextStyle::BODY));
        for size in [16, 24, 32] {
            let style = TextStyle::new(Face::Mono, size);
            assert_eq!(
                glyph(style, ' ').unwrap().advance,
                glyph(style, 'W').unwrap().advance
            );
            assert_eq!(
                glyph(style, 'i').unwrap().advance,
                glyph(style, 'W').unwrap().advance
            );
        }
    }

    #[test]
    fn combining_cluster_falls_back_as_a_unit() {
        assert_ne!(text_width("i", TextStyle::BODY), tab5_font::text_width("i"));
        assert_eq!(
            text_width("i\u{301}", TextStyle::BODY),
            tab5_font::text_width("i\u{301}")
        );
        assert_eq!(
            text_width("日本語", TextStyle::HEADING),
            tab5_font::text_width("日本語") * 2
        );
    }

    #[test]
    fn a4_blending_keeps_endpoints_and_intermediate_color() {
        assert_eq!(blend_rgb565(0xffff, 0x0000, 0), 0x0000);
        assert_eq!(blend_rgb565(0xffff, 0x0000, 15), 0xffff);
        let middle = blend_rgb565(0xffff, 0x0000, 8);
        assert_ne!(middle, 0x0000);
        assert_ne!(middle, 0xffff);
        assert_eq!(middle, 0x8c51);
    }

    struct Guarded {
        width: usize,
        height: usize,
        pixels: [u16; 66],
    }

    impl PixelSurface for Guarded {
        fn width(&self) -> usize {
            self.width
        }
        fn height(&self) -> usize {
            self.height
        }
        fn read(&self, x: usize, y: usize) -> u16 {
            self.pixels[1 + y * self.width + x]
        }
        fn write(&mut self, x: usize, y: usize, color: u16) {
            self.pixels[1 + y * self.width + x] = color;
        }
    }

    #[test]
    fn painter_clips_and_preserves_guards_and_transparent_pixels() {
        let mut surface = Guarded {
            width: 8,
            height: 8,
            pixels: [0x1234; 66],
        };
        surface.pixels[0] = 0xaaaa;
        surface.pixels[65] = 0xbbbb;
        let glyph = glyph(TextStyle::BODY, 'W').unwrap();
        paint_glyph(
            &mut surface,
            -3,
            -2,
            &glyph,
            line_metrics(TextStyle::BODY),
            0xffff,
            None,
        );
        assert_eq!(surface.pixels[0], 0xaaaa);
        assert_eq!(surface.pixels[65], 0xbbbb);
        assert!(surface.pixels[1..65].iter().any(|pixel| *pixel != 0x1234));
        assert!(surface.pixels[1..65].iter().any(|pixel| *pixel == 0x1234));
    }

    #[test]
    fn opaque_painter_clears_the_advance_box_before_ink() {
        let mut surface = Guarded {
            width: 8,
            height: 8,
            pixels: [0xf800; 66],
        };
        let glyph = glyph(TextStyle::BODY, ' ').unwrap();
        paint_glyph(
            &mut surface,
            0,
            0,
            &glyph,
            line_metrics(TextStyle::BODY),
            0xffff,
            Some(0x001f),
        );
        assert!(
            surface.pixels[1..1 + glyph.advance as usize * 8]
                .iter()
                .any(|pixel| *pixel == 0x001f)
        );
        assert!(!surface.pixels[1..65].iter().any(|pixel| *pixel == 0xffff));
    }

    #[test]
    fn binary_painter_keeps_metrics_but_writes_only_foreground_or_background() {
        let mut surface = Guarded {
            width: 8,
            height: 8,
            pixels: [0x1234; 66],
        };
        let glyph = glyph(TextStyle::BODY, 'S').unwrap();
        paint_glyph_1bpp(
            &mut surface,
            -1,
            0,
            &glyph,
            line_metrics(TextStyle::BODY),
            0xffff,
            Some(0x001f),
        );
        assert!(surface.pixels[1..65].iter().any(|pixel| *pixel == 0xffff));
        assert!(surface.pixels[1..65].iter().any(|pixel| *pixel == 0x001f));
        assert!(
            surface.pixels[1..65]
                .iter()
                .all(|pixel| matches!(*pixel, 0x1234 | 0xffff | 0x001f))
        );
    }
}
