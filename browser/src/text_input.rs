//! UTF-8 text editing state shared by address and form fields.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use crate::memory::{self, OutOfMemory};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    SingleLine,
    MultiLine,
}

pub struct TextInput {
    text: String,
    caret: usize,
    anchor: Option<usize>,
    max_bytes: usize,
    mode: Mode,
    composition: Option<Composition>,
}

pub struct Composition {
    pub text: String,
    pub selection: Range<usize>,
}

impl TextInput {
    pub fn new(mut text: String, max_bytes: usize, mode: Mode) -> Self {
        if mode == Mode::SingleLine {
            if let Some(end) = text.find(['\r', '\n']) {
                text.truncate(end);
            }
        }
        if text.len() > max_bytes {
            let mut end = max_bytes.min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        let caret = text.len();
        Self {
            text,
            caret,
            anchor: None,
            max_bytes,
            mode,
            composition: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn composition(&self) -> Option<&Composition> {
        self.composition.as_ref()
    }

    pub fn take_text(&mut self) -> String {
        self.caret = 0;
        self.anchor = None;
        self.composition = None;
        core::mem::take(&mut self.text)
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
        self.anchor = None;
        self.composition = None;
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.caret).then(|| anchor.min(self.caret)..anchor.max(self.caret))
    }

    pub fn set_composition(&mut self, composition: Option<Composition>) {
        self.composition = composition;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.len();
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    pub fn replace_selection(&mut self, inserted: &str) -> bool {
        if self.mode == Mode::SingleLine && (inserted.contains('\r') || inserted.contains('\n')) {
            return false;
        }
        let range = self.selection().unwrap_or(self.caret..self.caret);
        if self.text.len() - range.len() + inserted.len() > self.max_bytes {
            return false;
        }
        self.text.replace_range(range.clone(), inserted);
        self.caret = range.start + inserted.len();
        self.anchor = None;
        self.composition = None;
        true
    }

    pub fn insert_char(&mut self, character: char) -> bool {
        let mut encoded = [0; 4];
        self.replace_selection(character.encode_utf8(&mut encoded))
    }

    pub fn backspace(&mut self) -> bool {
        if self.selection().is_some() {
            return self.replace_selection("");
        }
        let Some(previous) = self.text[..self.caret]
            .char_indices()
            .next_back()
            .map(|(at, _)| at)
        else {
            return false;
        };
        self.anchor = Some(previous);
        self.replace_selection("")
    }

    pub fn delete(&mut self) -> bool {
        if self.selection().is_some() {
            return self.replace_selection("");
        }
        let Some(character) = self.text[self.caret..].chars().next() else {
            return false;
        };
        self.anchor = Some(self.caret + character.len_utf8());
        self.replace_selection("")
    }

    pub fn move_left(&mut self, selecting: bool) {
        if !selecting {
            if let Some(selection) = self.selection() {
                self.caret = selection.start;
                self.anchor = None;
                self.composition = None;
                return;
            }
        }
        self.prepare_move(selecting);
        self.caret = self.text[..self.caret]
            .char_indices()
            .next_back()
            .map_or(0, |(at, _)| at);
    }

    pub fn move_right(&mut self, selecting: bool) {
        if !selecting {
            if let Some(selection) = self.selection() {
                self.caret = selection.end;
                self.anchor = None;
                self.composition = None;
                return;
            }
        }
        self.prepare_move(selecting);
        if let Some(character) = self.text[self.caret..].chars().next() {
            self.caret += character.len_utf8();
        }
    }

    pub fn move_home(&mut self, selecting: bool) {
        self.prepare_move(selecting);
        self.caret = if self.mode == Mode::MultiLine {
            self.text[..self.caret].rfind('\n').map_or(0, |at| at + 1)
        } else {
            0
        };
    }

    pub fn move_end(&mut self, selecting: bool) {
        self.prepare_move(selecting);
        self.caret = if self.mode == Mode::MultiLine {
            self.text[self.caret..]
                .find('\n')
                .map_or(self.text.len(), |at| self.caret + at)
        } else {
            self.text.len()
        };
    }

    fn prepare_move(&mut self, selecting: bool) {
        if selecting {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.composition = None;
    }

    /// Moves the caret to the displayed row above or below, keeping its
    /// horizontal position as nearly as the row allows. Past the first or
    /// last row it goes to the start or end of the text.
    pub fn move_row(
        &mut self,
        rows: &[Range<usize>],
        down: bool,
        selecting: bool,
        mut measure: impl FnMut(&str) -> usize,
    ) {
        let current = row_of(rows, self.caret);
        let x = rows.get(current).map_or(0, |row| {
            measure(&self.text[row.start..self.caret.max(row.start)])
        });
        self.prepare_move(selecting);
        let target = if down {
            current.checked_add(1)
        } else {
            current.checked_sub(1)
        };
        let Some(row) = target.and_then(|target| rows.get(target)) else {
            self.caret = if down { self.text.len() } else { 0 };
            return;
        };
        let mut caret = row.start;
        let mut used = 0;
        for (at, character) in self.text[row.clone()].char_indices() {
            let start = row.start + at;
            let end = start + character.len_utf8();
            let width = measure(&self.text[start..end]);
            if used + width / 2 >= x {
                break;
            }
            used += width;
            caret = end;
        }
        // The end of a soft-wrapped row is the start of the next one, and a
        // caret there would be drawn on the row below the one it moved to.
        let wrapped = target
            .and_then(|target| rows.get(target + 1))
            .is_some_and(|next| next.start == row.end);
        if wrapped && caret == row.end && caret > row.start {
            caret = self.text[..caret]
                .char_indices()
                .next_back()
                .map_or(row.start, |(at, _)| at);
        }
        self.caret = caret;
    }

    pub fn visible_start(&self, width: usize, mut measure: impl FnMut(&str) -> usize) -> usize {
        let mut start = self.caret;
        while start > 0 {
            let candidate = self.text[..start]
                .char_indices()
                .next_back()
                .map_or(0, |(at, _)| at);
            if measure(&self.text[candidate..self.caret]) > width {
                break;
            }
            start = candidate;
        }
        start
    }
}

/// Splits text into displayed rows no wider than `width`: at every line
/// break, and before any character that would overflow the row. Wrapping is
/// by character, not by word. A range excludes its line break. There is
/// always at least one row, and text ending in a line break ends with an
/// empty one, which is where the caret goes after it.
pub fn wrap_rows(
    text: &str,
    width: usize,
    mut measure: impl FnMut(&str) -> usize,
) -> Result<Vec<Range<usize>>, OutOfMemory> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (at, character) in text.char_indices() {
        if character == '\n' {
            memory::push(&mut rows, start..at)?;
            start = at + 1;
            used = 0;
            continue;
        }
        let advance = measure(&text[at..at + character.len_utf8()]);
        if used > 0 && used + advance > width {
            memory::push(&mut rows, start..at)?;
            start = at;
            used = 0;
        }
        used += advance;
    }
    memory::push(&mut rows, start..text.len())?;
    Ok(rows)
}

/// The displayed row a caret is on: the last row starting at or before it,
/// so the boundary of a soft wrap belongs to the row that follows.
pub fn row_of(rows: &[Range<usize>], caret: usize) -> usize {
    rows.iter().rposition(|row| row.start <= caret).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_break_at_newlines_and_before_overflow() {
        let count = |text: &str| text.chars().count();
        assert_eq!(
            wrap_rows("abcde\n\n界", 3, count).unwrap(),
            [0..3, 3..5, 6..6, 7..10]
        );
        assert_eq!(wrap_rows("ab\n", 3, count).unwrap(), [0..2, 3..3]);
        let rows = wrap_rows("abcde", 3, count).unwrap();
        assert_eq!(row_of(&rows, 3), 1);
        assert_eq!(row_of(&rows, 2), 0);
    }

    #[test]
    fn vertical_moves_keep_the_column_and_stop_at_the_ends() {
        let count = |text: &str| text.chars().count();
        let mut input = TextInput::new("abcd\nxy\nlonger".into(), 64, Mode::MultiLine);
        let rows = wrap_rows(input.text(), 16, count).unwrap();
        input.move_home(false);
        assert_eq!(input.caret(), 8);
        input.move_right(false);
        input.move_right(false);
        input.move_right(false);
        input.move_row(&rows, false, false, count);
        assert_eq!(input.caret(), 7);
        input.move_row(&rows, false, false, count);
        assert_eq!(input.caret(), 2);
        input.move_row(&rows, false, false, count);
        assert_eq!(input.caret(), 0);
        input.move_row(&rows, true, false, count);
        input.move_row(&rows, true, false, count);
        assert_eq!(input.caret(), 8);
        input.move_row(&rows, true, false, count);
        assert_eq!(input.caret(), input.text().len());

        let mut wrapped = TextInput::new("abcdef".into(), 16, Mode::MultiLine);
        let rows = wrap_rows(wrapped.text(), 3, count).unwrap();
        wrapped.move_row(&rows, false, false, count);
        assert_eq!(wrapped.caret(), 2);
        assert_eq!(row_of(&rows, wrapped.caret()), 0);
    }

    #[test]
    fn unicode_operations_keep_boundaries() {
        let mut input = TextInput::new("a界b".into(), 32, Mode::SingleLine);
        input.move_left(false);
        input.move_left(false);
        assert_eq!(input.caret(), 1);
        assert!(input.replace_selection("日本"));
        assert_eq!(input.text(), "a日本界b");
        assert!(input.backspace());
        assert!(input.delete());
        assert_eq!(input.text(), "a日b");
    }

    #[test]
    fn selection_replacement_is_atomic_at_the_limit() {
        let mut input = TextInput::new("abcdef".into(), 7, Mode::SingleLine);
        input.move_home(false);
        input.move_right(true);
        input.move_right(true);
        assert!(input.replace_selection("界"));
        assert_eq!(input.text(), "界cdef");
        input.select_all();
        assert!(!input.replace_selection("日本語"));
        assert_eq!(input.text(), "界cdef");
    }

    #[test]
    fn modes_control_newlines() {
        let mut single = TextInput::new("before\nafter".into(), 8, Mode::SingleLine);
        assert_eq!(single.text(), "before");
        assert!(!single.replace_selection("a\nb"));
        let mut multi = TextInput::new("aa\n界界\nzz".into(), 32, Mode::MultiLine);
        multi.move_left(false);
        multi.move_left(false);
        multi.move_left(false);
        multi.move_home(false);
        assert_eq!(multi.caret(), 3);
        multi.move_end(false);
        assert_eq!(multi.caret(), 9);
    }

    #[test]
    fn an_arrow_collapses_a_selection_to_its_near_edge() {
        let mut input = TextInput::new("abcdef".into(), 16, Mode::SingleLine);
        input.select_all();
        input.move_left(false);
        assert_eq!(input.caret(), 0);
        assert_eq!(input.selection(), None);
        input.select_all();
        input.move_right(false);
        assert_eq!(input.caret(), 6);
        assert_eq!(input.selection(), None);
    }

    #[test]
    fn visible_start_walks_utf8_boundaries_by_measured_width() {
        let input = TextInput::new("a界bc".into(), 16, Mode::SingleLine);
        assert_eq!(input.visible_start(3, |text| text.chars().count()), 1);
        assert_eq!(input.visible_start(2, |text| text.chars().count()), 4);
    }
}
