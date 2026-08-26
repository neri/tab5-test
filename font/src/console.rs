//! The console's fixed half-width repertoire.
//!
//! The console is a 8x16 character cell terminal and stays one, so it does not
//! use [`advance`](crate::advance) for layout the way the browser does. Every
//! Unicode scalar it is asked to print becomes exactly one cell: the ones this
//! repertoire covers become themselves, and everything else -- kanji, kana,
//! combining marks, characters the font does not have -- becomes a visible
//! placeholder. Nothing becomes a blank, because a blank would claim there was
//! no character there.
//!
//! That is a display decision and only a display decision. The UART still
//! receives the original UTF-8, which is where the text is meant to be read
//! back when the LCD can only say "something was here".
//!
//! A cell holds a [`Id`], one byte, rather than a `char`. At 156x44 cells the
//! difference is 20 KiB of static RAM, and the byte is what the console
//! actually needs: the glyph to paint.

use crate::{Glyph, MAX_WIDTH, glyph, hollow_box};

/// A console cell's glyph, as stored in the grid.
///
/// The value space is dense and fixed, so the whole grid is a byte array:
///
/// | id | character |
/// | --- | --- |
/// | 0 | blank |
/// | 1 | placeholder |
/// | 2..=95 | U+0021..U+007E |
/// | 96..=191 | U+00A0..U+00FF |
/// | 192..=254 | U+FF61..U+FF9F |
/// | 255 | reserved, drawn as the placeholder |
pub type Id = u8;

/// An empty cell. This is what `clear` and `scroll` fill with.
pub const BLANK: Id = 0;
/// A character that exists but cannot be shown in one half-width cell.
pub const PLACEHOLDER: Id = 1;

const ASCII_FIRST: Id = 2;
const ASCII_START: u32 = 0x21;
const ASCII_END: u32 = 0x7E;
const LATIN1_FIRST: Id = 96;
const LATIN1_START: u32 = 0xA0;
const LATIN1_END: u32 = 0xFF;
const KANA_FIRST: Id = 192;
const KANA_START: u32 = 0xFF61;
const KANA_END: u32 = 0xFF9F;

/// U+00AD SOFT HYPHEN keeps its id so that Latin-1 stays one contiguous span,
/// but Unifont draws it as a 16 pixel code point box -- it is a line breaking
/// hint with no visible form of its own -- so the console shows the
/// placeholder instead. `tools/font/generate.py` skips the same code point
/// when it checks that the repertoire is half-width.
const SOFT_HYPHEN: u32 = 0xAD;

/// The cell for `character`. Always exactly one cell, for every scalar.
///
/// Control characters are the caller's business: the console acts on `\n`,
/// `\r` and the rest before it gets here, and anything left over is a
/// character that failed to be printable, which is what the placeholder says.
pub const fn id(character: char) -> Id {
    let code_point = character as u32;
    match code_point {
        0x20 => BLANK,
        SOFT_HYPHEN => PLACEHOLDER,
        ASCII_START..=ASCII_END => ASCII_FIRST + (code_point - ASCII_START) as Id,
        LATIN1_START..=LATIN1_END => LATIN1_FIRST + (code_point - LATIN1_START) as Id,
        KANA_START..=KANA_END => KANA_FIRST + (code_point - KANA_START) as Id,
        _ => PLACEHOLDER,
    }
}

/// The character an id stands for, or `None` for the blank and the
/// placeholder, which stand for no particular character.
///
/// This is the inverse of [`id`] over the repertoire, and exists so that a
/// caller holding a grid can recover printable text from it.
pub const fn character(id: Id) -> Option<char> {
    let (first, start) = match id {
        BLANK | PLACEHOLDER | 255 => return None,
        _ if id < LATIN1_FIRST => (ASCII_FIRST, ASCII_START),
        _ if id < KANA_FIRST => (LATIN1_FIRST, LATIN1_START),
        _ => (KANA_FIRST, KANA_START),
    };
    match char::from_u32(start + (id - first) as u32) {
        // Its id exists to keep Latin-1 contiguous, but no cell ever holds it:
        // `id` maps it to the placeholder, and so this direction has to agree.
        Some(character) if character as u32 == SOFT_HYPHEN => None,
        character => character,
    }
}

/// The pixels for a cell. Always 8 columns wide.
pub fn cell_glyph(id: Id) -> Glyph {
    const BLANK_GLYPH: Glyph = Glyph {
        columns: [0; MAX_WIDTH],
        advance: 8,
    };
    const PLACEHOLDER_GLYPH: Glyph = Glyph {
        columns: hollow_box(8),
        advance: 8,
    };
    match character(id) {
        None if id == BLANK => BLANK_GLYPH,
        None => PLACEHOLDER_GLYPH,
        // The repertoire is checked at generation time, so the fallback here
        // is unreachable rather than a policy.
        Some(character) => glyph(character).unwrap_or(PLACEHOLDER_GLYPH),
    }
}

/// How many cells `text` occupies once converted.
///
/// Callers that align columns -- `ls`, right-justified fields, counts -- use
/// this rather than `len()` or `chars().count()`, so that the alignment is
/// computed in the same units the screen is.
pub fn cell_count(text: &str) -> usize {
    text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repertoire_round_trips() {
        for range in [
            ASCII_START..=ASCII_END,
            LATIN1_START..=LATIN1_END,
            KANA_START..=KANA_END,
        ] {
            for character in range.filter_map(char::from_u32) {
                if character as u32 == SOFT_HYPHEN {
                    continue;
                }
                let id = id(character);
                assert_ne!(id, PLACEHOLDER, "{character:?} fell out of the repertoire");
                assert_eq!(super::character(id), Some(character), "id {id}");
            }
        }
        assert_eq!(id(' '), BLANK);
        assert_eq!(super::character(BLANK), None);
        assert_eq!(super::character(PLACEHOLDER), None);
        assert_eq!(super::character(255), None);
    }

    #[test]
    fn ids_are_unique_across_the_repertoire() {
        let mut seen = [false; 256];
        for character in (ASCII_START..=ASCII_END)
            .chain(LATIN1_START..=LATIN1_END)
            .chain(KANA_START..=KANA_END)
            .filter_map(char::from_u32)
        {
            let id = id(character);
            if id == PLACEHOLDER {
                continue;
            }
            assert!(!seen[id as usize], "id {id} used twice, at {character:?}");
            seen[id as usize] = true;
        }
    }

    #[test]
    fn every_cell_is_eight_pixels_wide() {
        for id in 0..=255u8 {
            assert_eq!(cell_glyph(id).advance, 8, "id {id}");
            assert_eq!(
                &cell_glyph(id).columns[8..],
                &[0u16; 8],
                "id {id} has ink outside its cell"
            );
        }
    }

    #[test]
    fn unrepresentable_characters_are_visible_not_blank() {
        for character in ['あ', '漢', '\u{3099}', '\u{1F600}', '\u{7}', '\u{AD}'] {
            assert_eq!(id(character), PLACEHOLDER, "{character:?}");
        }
        assert_ne!(
            cell_glyph(PLACEHOLDER).columns,
            [0u16; MAX_WIDTH],
            "the placeholder has to draw something"
        );
    }

    #[test]
    fn every_scalar_takes_exactly_one_cell() {
        // The property the console depends on: no scalar splits into two
        // cells, and none disappears. Walked over the whole BMP plus a sample
        // above it rather than argued about.
        for code_point in 0..=0x10FFFFu32 {
            let Some(character) = char::from_u32(code_point) else {
                continue;
            };
            let _ = id(character);
            assert_eq!(cell_count(character.encode_utf8(&mut [0; 4])), 1);
        }
    }

    #[test]
    fn cell_count_measures_cells_not_bytes() {
        assert_eq!(cell_count("abc"), 3);
        // Three cells: three scalars, each a placeholder.
        assert_eq!(cell_count("日本語"), 3);
        // Not the four bytes of its UTF-8, and not two cells for its width.
        assert_eq!(cell_count("か\u{3099}"), 2);
    }
}
