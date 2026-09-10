//! The console's fixed half-width repertoire.
//!
//! The console is a 8x16 character cell terminal and stays one, so it does not
//! use [`advance`](crate::advance) for layout the way the browser does. Every
//! Unicode scalar it is asked to print becomes exactly one cell: the ones this
//! printable ASCII repertoire covers become themselves, and everything else
//! -- kanji, kana, Latin-1, combining marks -- becomes a visible
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
/// | 96..=255 | reserved, drawn as the placeholder |
pub type Id = u8;

/// An empty cell. This is what `clear` and `scroll` fill with.
pub const BLANK: Id = 0;
/// A character that exists but cannot be shown in one half-width cell.
pub const PLACEHOLDER: Id = 1;

const ASCII_FIRST: Id = 2;
const ASCII_START: u32 = 0x21;
const ASCII_END: u32 = 0x7E;

/// The cell for `character`. Always exactly one cell, for every scalar.
///
/// Control characters are the caller's business: the console acts on `\n`,
/// `\r` and the rest before it gets here, and anything left over is a
/// character that failed to be printable, which is what the placeholder says.
pub const fn id(character: char) -> Id {
    let code_point = character as u32;
    match code_point {
        0x20 => BLANK,
        ASCII_START..=ASCII_END => ASCII_FIRST + (code_point - ASCII_START) as Id,
        _ => PLACEHOLDER,
    }
}

/// The character an id stands for, or `None` for the blank and the
/// placeholder, which stand for no particular character.
///
/// This is the inverse of [`id`] over the repertoire, and exists so that a
/// caller holding a grid can recover printable text from it.
pub const fn character(id: Id) -> Option<char> {
    if id < ASCII_FIRST || id > 95 {
        return None;
    }
    char::from_u32(ASCII_START + (id - ASCII_FIRST) as u32)
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
        for character in (ASCII_START..=ASCII_END).filter_map(char::from_u32) {
            let id = id(character);
            assert_ne!(id, PLACEHOLDER, "{character:?} fell out of the repertoire");
            assert_eq!(super::character(id), Some(character), "id {id}");
        }
        assert_eq!(id(' '), BLANK);
        assert_eq!(super::character(BLANK), None);
        assert_eq!(super::character(PLACEHOLDER), None);
        assert_eq!(super::character(255), None);
    }

    #[test]
    fn ids_are_unique_across_the_repertoire() {
        let mut seen = [false; 256];
        for character in (ASCII_START..=ASCII_END).filter_map(char::from_u32) {
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
        for character in ['é', 'ｶ', 'あ', '漢', '\u{3099}', '\u{1F600}', '\u{7}'] {
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
