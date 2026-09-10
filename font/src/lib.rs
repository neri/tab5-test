//! The 16 pixel bitmap font every screen on this device draws through.
//!
//! Two things live here and nowhere else: the glyph bitmaps, and the advance
//! width of a `char`. Keeping them together is the point. A renderer that
//! guessed widths from Unicode ranges and a layout engine that guessed them
//! separately would disagree eventually, and the disagreement would show up
//! as a link whose underline is the wrong length or a hit test that lands on
//! the character next door. Both sides call [`advance`] instead.
//!
//! The data is generated: `tools/font/generate.py` reads Unifont-JP and the
//! subset manifest and writes `data/tab5font16.bin`, which is checked in.
//! Nothing here parses BDF. The small printable-ASCII blob stays uncompressed
//! in DROM and is read directly. Normal GUI Latin and Japanese use the separate
//! A4 font crate.
//!
//! Glyphs are stored column by column, least significant bit at the top,
//! because the framebuffer maps increasing logical X onto decreasing native
//! address: text is painted one vertical run per column, so the data is laid
//! out the way it is consumed.

#![cfg_attr(not(test), no_std)]

pub mod console;

/// Every glyph is this tall. Half-width glyphs are 8 columns of it, full-width
/// glyphs all 16.
pub const HEIGHT: usize = 16;

/// The widest a glyph can be, and the number of columns each one is stored in.
pub const MAX_WIDTH: usize = 16;

const DATA: &[u8] = include_bytes!("../data/tab5font16.bin");

include!(concat!(env!("OUT_DIR"), "/font_meta.rs"));

pub const HEADER_BYTES: usize = 32;
const RANGE_BYTES: usize = 8;
const GLYPH_BYTES: usize = 32;

fn u16_from(bytes: &[u8], offset: usize) -> u16 {
    (bytes[offset] as u16) | ((bytes[offset + 1] as u16) << 8)
}

fn u32_from(bytes: &[u8], offset: usize) -> u32 {
    (bytes[offset] as u32)
        | ((bytes[offset + 1] as u32) << 8)
        | ((bytes[offset + 2] as u32) << 16)
        | ((bytes[offset + 3] as u32) << 24)
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
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn data() -> &'static [u8] {
    DATA
}

/// One glyph's pixels and the pen movement that follows it.
///
/// `columns[0]` is the leftmost column and bit 0 of each column is its top
/// pixel. Columns at or beyond `advance` are zero for half-width glyphs, so a
/// painter may either clip to the advance or write all 16 columns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Glyph {
    pub columns: [u16; MAX_WIDTH],
    /// 0 for combining marks, 8 for half-width, 16 for full-width.
    pub advance: u8,
}

impl Glyph {
    /// The box this glyph paints into, which is its advance except for
    /// combining marks: those have no advance of their own but still cover
    /// the glyph they are painted over.
    pub const fn width(&self) -> usize {
        if self.advance == 0 {
            MAX_WIDTH
        } else {
            self.advance as usize
        }
    }
}

/// Builds the hollow box used wherever a code point has no glyph.
///
/// Drawing something is the whole point: a space would say the text ended, and
/// a `?` would say the text contained one. A box says a character is here and
/// this font cannot show it, which is also what the width promises.
pub(crate) const fn hollow_box(width: usize) -> [u16; MAX_WIDTH] {
    // Rows 2..=13, leaving the ascender and descender rows clear so the box
    // does not touch the line above or below.
    const SIDES: u16 = 0x3FFC;
    const TOP_AND_BOTTOM: u16 = (1 << 2) | (1 << 13);
    let mut columns = [0u16; MAX_WIDTH];
    let mut column = 1;
    while column + 1 < width {
        columns[column] = if column == 1 || column + 2 == width {
            SIDES
        } else {
            TOP_AND_BOTTOM
        };
        column += 1;
    }
    columns
}

const HALFWIDTH_REPLACEMENT: Glyph = Glyph {
    columns: hollow_box(8),
    advance: 8,
};

const FULLWIDTH_REPLACEMENT: Glyph = Glyph {
    columns: hollow_box(16),
    advance: 16,
};

/// The glyph index of `code_point`, or `None` if the subset does not have it.
fn index_of(code_point: u32) -> Option<usize> {
    // The generated printable-ASCII repertoire is one contiguous run. Keep
    // the format's binary-search reader so the checked-in T5F1 format and its
    // validation remain usable without a special firmware-only representation.
    let mut low = 0;
    let mut high = RANGE_COUNT;
    while low < high {
        let middle = (low + high) / 2;
        let entry = RANGES_OFFSET + middle * RANGE_BYTES;
        let first = u32_at(entry);
        if code_point < first {
            high = middle;
            continue;
        }
        let length = u16_at(entry + 4) as u32;
        if code_point >= first + length {
            low = middle + 1;
            continue;
        }
        return Some(u16_at(entry + 6) as usize + (code_point - first) as usize);
    }
    None
}

fn glyph_at(index: usize) -> Glyph {
    let mut columns = [0u16; MAX_WIDTH];
    let base = BITMAPS_OFFSET + index * GLYPH_BYTES;
    let mut column = 0;
    while column < MAX_WIDTH {
        columns[column] = u16_at(base + column * 2);
        column += 1;
    }
    Glyph {
        columns,
        advance: data()[ADVANCES_OFFSET + index],
    }
}

/// The glyph for `character`, or `None` when the subset does not cover it.
///
/// Callers that draw text want [`glyph_or_replacement`] instead; this is for
/// coverage checks and diagnostics.
pub fn glyph(character: char) -> Option<Glyph> {
    index_of(character as u32).map(glyph_at)
}

/// The glyph for `character`, falling back to a hollow box of the width
/// [`advance`] promises.
pub fn glyph_or_replacement(character: char) -> Glyph {
    match glyph(character) {
        Some(glyph) => glyph,
        None if character.is_ascii() => HALFWIDTH_REPLACEMENT,
        None => FULLWIDTH_REPLACEMENT,
    }
}

/// How far the pen moves after `character`, in pixels: 0, 8 or 16.
///
/// Every caller that measures text -- the browser's line breaking, its hit
/// testing, the underline under a link -- goes through this, so that what is
/// measured is what gets drawn. Characters the subset does not cover still
/// have a width: ASCII takes 8 pixels and everything else 16, matching the
/// replacement glyph.
pub fn advance(character: char) -> u8 {
    match index_of(character as u32) {
        Some(index) => data()[ADVANCES_OFFSET + index],
        None if character.is_ascii() => 8,
        None => 16,
    }
}

/// Whether `character` is painted over the glyph before it instead of after
/// it, which is the same thing as having no advance of its own.
pub fn is_combining(character: char) -> bool {
    advance(character) == 0
}

/// The total advance of `text`, in pixels.
pub fn text_width(text: &str) -> usize {
    text.chars()
        .map(|character| advance(character) as usize)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_blob_is_plain_ascii() {
        assert_eq!(&DATA[..4], b"T5F1");
        assert_eq!(DATA.len(), STORAGE_BYTES);
        assert_eq!(GLYPH_COUNT, 95);
    }

    #[test]
    fn payload_matches_its_recorded_crc() {
        assert_eq!(crc32(&data()[HEADER_BYTES..]), CRC32);
    }

    #[test]
    fn ranges_are_sorted_disjoint_and_cover_every_glyph_once() {
        let mut previous_end = 0u32;
        let mut expected_index = 0usize;
        for range in 0..RANGE_COUNT {
            let entry = RANGES_OFFSET + range * RANGE_BYTES;
            let first = u32_at(entry);
            let length = u16_at(entry + 4) as u32;
            let first_glyph = u16_at(entry + 6) as usize;
            assert!(length > 0, "range {range} is empty");
            assert!(
                range == 0 || first > previous_end,
                "range {range} starts at U+{first:04X}, inside or touching the one before"
            );
            assert_eq!(
                first_glyph, expected_index,
                "range {range} glyph index jumped"
            );
            assert!(first + length - 1 <= 0xFFFF, "range {range} leaves the BMP");
            previous_end = first + length - 1;
            expected_index += length as usize;
        }
        assert_eq!(expected_index, GLYPH_COUNT);
    }

    #[test]
    fn every_advance_is_zero_eight_or_sixteen() {
        for index in 0..GLYPH_COUNT {
            let advance = data()[ADVANCES_OFFSET + index];
            assert!(
                matches!(advance, 0 | 8 | 16),
                "glyph {index} has advance {advance}"
            );
        }
    }

    #[test]
    fn halfwidth_glyphs_leave_their_right_half_blank() {
        // A painter is allowed to clip to the advance, so ink beyond it would
        // appear or not depending on which path drew the character.
        for index in 0..GLYPH_COUNT {
            if data()[ADVANCES_OFFSET + index] != 8 {
                continue;
            }
            let glyph = glyph_at(index);
            assert_eq!(
                &glyph.columns[8..],
                &[0u16; 8],
                "glyph {index} is 8 wide but has ink past column 8"
            );
        }
    }

    #[test]
    fn every_lookup_round_trips_to_its_own_glyph() {
        for range in 0..RANGE_COUNT {
            let entry = RANGES_OFFSET + range * RANGE_BYTES;
            let first = u32_at(entry);
            let length = u16_at(entry + 4) as u32;
            let first_glyph = u16_at(entry + 6) as usize;
            for step in 0..length {
                let character = char::from_u32(first + step).expect("subset holds scalars only");
                assert_eq!(
                    index_of(character as u32),
                    Some(first_glyph + step as usize)
                );
            }
        }
    }

    #[test]
    fn only_ascii_is_stored_and_other_characters_keep_a_fallback_width() {
        for character in ' '..='~' {
            assert_eq!(advance(character), 8, "{character:?}");
        }
        for character in ['あ', 'ア', '漢', '。', '　', '１', '￥', 'ｱ', 'é', 'Ω', '→']
        {
            assert_eq!(advance(character), 16, "{character:?}");
            assert!(glyph(character).is_none(), "{character:?}");
        }
    }

    #[test]
    fn combining_marks_are_not_part_of_the_ascii_font() {
        for character in ['\u{3099}', '\u{309A}', '\u{0300}', '\u{0301}'] {
            assert_eq!(advance(character), 16, "{character:?}");
            assert!(!is_combining(character), "{character:?}");
            assert!(glyph(character).is_none(), "{character:?} has a bitmap");
        }
        assert!(!is_combining('あ'));
        assert!(!is_combining('a'));
    }

    #[test]
    fn missing_characters_still_draw_something() {
        // An emoji: deliberately out of scope, and exactly the case where a
        // blank cell would read as "the text ended here".
        let glyph = glyph_or_replacement('\u{1F600}');
        assert_eq!(glyph.advance, 16);
        assert_ne!(glyph.columns, [0u16; MAX_WIDTH]);

        let control = glyph_or_replacement('\u{7}');
        assert_eq!(control.advance, 8);
        assert_ne!(control.columns, [0u16; MAX_WIDTH]);
    }

    #[test]
    fn capital_a_has_the_expected_pixels() {
        // Pinning one bitmap end to end catches a transpose or bit order that
        // reversed itself somewhere between the BDF and here. Rendered with
        // bit 0 at the top, these columns are Unifont's 'A'.
        let glyph = glyph('A').expect("ASCII");
        assert_eq!(
            glyph.columns,
            [
                0x0000, 0x3F80, 0x0260, 0x0210, 0x0210, 0x0260, 0x3F80, 0x0000, 0x0000, 0x0000,
                0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            ]
        );
    }

    #[test]
    fn text_width_adds_up_the_way_a_renderer_walks() {
        assert_eq!(text_width("abc"), 24);
        assert_eq!(text_width("あいう"), 48);
        assert_eq!(text_width("aあ"), 24);
        assert_eq!(text_width("か\u{3099}"), 32);
        assert_eq!(text_width(""), 0);
    }
}
