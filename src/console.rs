//! A small, fixed-size text console rendered into an RGB565 framebuffer.

use core::cell::UnsafeCell;

use crate::framebuffer::{BLACK, DoubleBuffer, GREEN, WHITE, WIDTH};

const DEFAULT_HEADER_COLOR: u16 = GREEN;

const SCALE: usize = 3;
const CELL_WIDTH: usize = 6 * SCALE;
const CELL_HEIGHT: usize = 8 * SCALE;
const LEFT: usize = 16;
const TOP: usize = 40;
const COLUMNS: usize = (WIDTH - LEFT * 2) / CELL_WIDTH;
const ROWS: usize = 28;

// Only the 5x7 ASCII font is available (no Japanese glyphs), so the prompt
// uses a half-width '>' rather than the full-width '＞' a shell would show.
const PROMPT: [char; 2] = ['>', ' '];

/// Longest command line `submit` can capture: one row minus the prompt.
const MAX_LINE: usize = COLUMNS - PROMPT.len();

/// Terminal-like text storage for the CardKB input echo display.
pub struct Console {
    cells: [[char; COLUMNS]; ROWS],
    column: usize,
    row: usize,
    previous_was_carriage_return: bool,
    /// Current blink phase of the pseudo cursor block drawn at `(column,
    /// row)`. The cursor cell itself never holds a printable character (it
    /// is always the next write position, or a position `backspace` has
    /// just cleared), so `render_cell` can use this flag to decide between
    /// the cursor block and that cell's normal (blank) contents without
    /// tracking the glyph underneath.
    cursor_visible: bool,
    /// Header band color, changeable by the shell's `color` command.
    header_color: u16,
    /// Set by `submit` when Enter completes a command line; drained by
    /// `take_submission`. Kept separate from `Update` since dispatching a
    /// command is an application-level reaction, not a rendering hint.
    pending_submission: Option<Submission>,
}

/// Describes which part of the screen changed, independent of *why* --
/// callers decide how to react (dispatch a command, move the cursor, etc.)
/// separately, e.g. via `take_submission`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Update {
    /// Nothing on screen needs to change.
    None,
    /// Columns `start..end` of `row` changed; every other cell is
    /// unaffected. Covers everything from a single typed character to a
    /// freshly written prompt -- any contiguous run within one row.
    Cells {
        row: usize,
        start: usize,
        end: usize,
    },
    /// Every row may have changed (a scroll, `clear`, or multi-line command
    /// output); a full redraw is required.
    Full,
}

/// A command line captured by `submit`, ready for shell dispatch.
pub struct Submission {
    text: [u8; MAX_LINE],
    len: usize,
}

impl Submission {
    pub fn as_bytes(&self) -> &[u8] {
        &self.text[..self.len]
    }
}

struct ConsoleStorage(UnsafeCell<Console>);

// The firmware owns this state from one hart and accesses it only from the
// foreground console loop.
unsafe impl Sync for ConsoleStorage {}

static CONSOLE: ConsoleStorage = ConsoleStorage(UnsafeCell::new(Console::new()));

/// Returns the single firmware console instance kept in low HP SRAM.
///
/// # Safety
/// The caller must not create another live reference to this singleton.
pub unsafe fn singleton() -> &'static mut Console {
    unsafe { &mut *CONSOLE.0.get() }
}

impl Console {
    pub const fn new() -> Self {
        let mut cells = [['\0'; COLUMNS]; ROWS];
        let mut i = 0;
        while i < PROMPT.len() {
            cells[0][i] = PROMPT[i];
            i += 1;
        }
        Self {
            cells,
            column: PROMPT.len(),
            row: 0,
            previous_was_carriage_return: false,
            cursor_visible: true,
            header_color: DEFAULT_HEADER_COLOR,
            pending_submission: None,
        }
    }

    /// Takes the command line captured by the most recent `submit`, if any.
    pub fn take_submission(&mut self) -> Option<Submission> {
        self.pending_submission.take()
    }

    /// Changes the header band color, e.g. from the shell's `color` command.
    pub fn set_header_color(&mut self, color: u16) {
        self.header_color = color;
    }

    /// Clears every cell and returns to a fresh, empty first row (no
    /// prompt: the caller writes it, same convention as `submit`).
    pub fn clear(&mut self) {
        self.cells = [['\0'; COLUMNS]; ROWS];
        self.column = 0;
        self.row = 0;
        self.previous_was_carriage_return = false;
        self.cursor_visible = true;
    }

    /// Current cursor cell, i.e. the next write position.
    pub fn cursor(&self) -> (usize, usize) {
        (self.column, self.row)
    }

    /// Forces the cursor block on, e.g. so activity doesn't leave the
    /// cursor mid-blink at its new position.
    pub fn show_cursor(&mut self) {
        self.cursor_visible = true;
    }

    /// Flips the blink phase for the idle blink timer.
    pub fn toggle_cursor(&mut self) {
        self.cursor_visible = !self.cursor_visible;
    }

    /// Adds one CardKB character using familiar terminal control characters.
    pub fn push(&mut self, ch: char) -> Update {
        let update = match ch {
            '\r' => {
                self.submit();
                self.previous_was_carriage_return = true;
                Update::None
            }
            '\n' if self.previous_was_carriage_return => {
                self.previous_was_carriage_return = false;
                Update::None
            }
            '\n' => {
                self.submit();
                Update::None
            }
            '\u{8}' | '\u{7f}' => self.backspace(),
            '\t' => {
                let spaces = 4 - self.column % 4;
                let mut wrap_update = Update::None;
                for _ in 0..spaces {
                    let row_before = self.row;
                    let result = self.put(' ');
                    // Blank tab cells need no pixel update on a normal
                    // forward cursor; only a row change (wrap or scroll)
                    // still needs its own redraw.
                    if self.row != row_before || matches!(result, Update::Full) {
                        wrap_update = result;
                    }
                }
                wrap_update
            }
            ' '..='~' => self.put(ch),
            _ => Update::None,
        };
        if ch != '\r' {
            self.previous_was_carriage_return = false;
        }
        update
    }

    /// Only a bottom-row scroll moves every visible row, so it is the only
    /// case that needs a full redraw; a plain new line just needs its own
    /// prompt cells drawn.
    fn line_update(&self, scrolled: bool) -> Update {
        if scrolled {
            Update::Full
        } else {
            Update::Cells {
                row: self.row,
                start: 0,
                end: PROMPT.len(),
            }
        }
    }

    /// Draws the complete console into one (currently inactive) framebuffer.
    #[inline(always)]
    pub fn render(&self, framebuffers: &mut DoubleBuffer, index: usize) {
        let cursor_row = self.row;
        let cursor_column = self.column;
        framebuffers.fill_console_background(index, 32, self.header_color, BLACK);

        // Only rows up to the cursor can contain input. Drawing each occupied
        // cell directly also avoids the former empty-run scanner, which could
        // stall before the first CardKB byte was received on ECO2.
        for row in 0..=cursor_row {
            let end = if row == cursor_row {
                cursor_column
            } else {
                COLUMNS
            };
            for column in 0..end {
                let ch = self.cells[row][column];
                if ch != '\0' && ch != ' ' {
                    framebuffers.draw_ascii_char(
                        index,
                        LEFT + column * CELL_WIDTH,
                        TOP + row * CELL_HEIGHT,
                        ch,
                        SCALE,
                        WHITE,
                        false,
                    );
                }
            }
        }

        // The occupied-cell loop above stops before `cursor_column`, so the
        // cursor cell (always blank) still needs its own pass here.
        self.render_cell(framebuffers, index, cursor_column, cursor_row);

        draw_ascii_text(
            framebuffers,
            index,
            LEFT,
            8,
            "Tab5 Console",
            2,
            BLACK,
            false,
        );
    }

    /// Repaints one text cell without touching the rest of the framebuffer.
    pub fn render_cell(
        &self,
        framebuffers: &mut DoubleBuffer,
        index: usize,
        column: usize,
        row: usize,
    ) {
        if column >= COLUMNS || row >= ROWS {
            return;
        }
        let x = LEFT + column * CELL_WIDTH;
        let y = TOP + row * CELL_HEIGHT;
        if self.cursor_visible && column == self.column && row == self.row {
            // Always this cell's only content: it is either the untouched
            // next write position or one `backspace` just cleared.
            framebuffers.fill_rect(index, x, y, CELL_WIDTH, CELL_HEIGHT, WHITE);
            return;
        }
        let ch = self.cells[row][column];
        if ch == '\0' || ch == ' ' {
            // Backspace and explicit spaces erase the complete cell. This
            // also covers hiding the cursor block: the cell it vacates is
            // always blank underneath.
            framebuffers.fill_rect(index, x, y, CELL_WIDTH, CELL_HEIGHT, BLACK);
        } else {
            // A freshly typed character's cell was the cursor block moments
            // ago. `draw_ascii_char` only paints the glyph's foreground
            // pixels, so without this clear the cursor's white background
            // would stay visible around the letter.
            framebuffers.fill_rect(index, x, y, CELL_WIDTH, CELL_HEIGHT, BLACK);
            framebuffers.draw_ascii_char(index, x, y, ch, SCALE, WHITE, false);
        }
    }

    pub fn flush_cell(
        &self,
        framebuffers: &DoubleBuffer,
        index: usize,
        column: usize,
        row: usize,
    ) -> bool {
        self.flush_cells(framebuffers, index, row, column, column + 1)
    }

    /// Repaints columns `start..end` of one row without touching the rest
    /// of the framebuffer.
    pub fn render_cells(
        &self,
        framebuffers: &mut DoubleBuffer,
        index: usize,
        row: usize,
        start: usize,
        end: usize,
    ) {
        for column in start..end {
            self.render_cell(framebuffers, index, column, row);
        }
    }

    /// Flushes columns `start..end` of one row in a single writeback, since
    /// they cover a contiguous native-memory span. Returns `false` if the
    /// range is empty or out of bounds.
    pub fn flush_cells(
        &self,
        framebuffers: &DoubleBuffer,
        index: usize,
        row: usize,
        start: usize,
        end: usize,
    ) -> bool {
        if start >= end || start >= COLUMNS || row >= ROWS {
            return false;
        }
        let end = end.min(COLUMNS);
        framebuffers.flush_rect(
            index,
            LEFT + start * CELL_WIDTH,
            TOP + row * CELL_HEIGHT,
            (end - start) * CELL_WIDTH,
            CELL_HEIGHT,
        )
    }

    fn put(&mut self, ch: char) -> Update {
        let column = self.column;
        let row = self.row;
        self.cells[self.row][self.column] = ch;
        self.column += 1;
        if self.column == COLUMNS {
            let scrolled = self.new_line();
            return self.line_update(scrolled);
        }
        Update::Cells {
            row,
            start: column,
            end: column + 1,
        }
    }

    /// Erases the previous character. Never crosses into the prompt, so a
    /// line's leading "> " can't be backspaced away.
    fn backspace(&mut self) -> Update {
        if self.column <= PROMPT.len() {
            return Update::None;
        }
        self.column -= 1;
        self.cells[self.row][self.column] = '\0';
        Update::Cells {
            row: self.row,
            start: self.column,
            end: self.column + 1,
        }
    }

    /// Advances to the next row (scrolling if needed) and reports whether
    /// the bottom row scrolled. Leaves the row blank and `column` at 0; it
    /// carries no prompt until `write_prompt` adds one.
    fn advance_row(&mut self) -> bool {
        self.row += 1;
        let scrolled = self.row == ROWS;
        if scrolled {
            for row in 1..ROWS {
                self.cells[row - 1] = self.cells[row];
            }
            self.cells[ROWS - 1] = ['\0'; COLUMNS];
            self.row = ROWS - 1;
        }
        self.column = 0;
        scrolled
    }

    /// Advances to the next row (scrolling if needed) and writes a fresh
    /// prompt into it. Used for plain input newlines, where the next row is
    /// always ready for more typing immediately.
    fn new_line(&mut self) -> bool {
        let scrolled = self.advance_row();
        self.write_prompt();
        scrolled
    }

    /// Writes the prompt into the current (assumed blank) row and positions
    /// `column` after it, ready for input.
    pub fn write_prompt(&mut self) {
        for (column, &ch) in PROMPT.iter().enumerate() {
            self.cells[self.row][column] = ch;
        }
        self.column = PROMPT.len();
    }

    /// Handles Enter: captures the just-typed line (the cells between the
    /// prompt and the cursor) into `pending_submission` for `take_submission`,
    /// then advances to a fresh blank row. The row is deliberately left
    /// without a prompt -- unlike `new_line` -- since command output, not
    /// more input, comes next.
    fn submit(&mut self) {
        let mut text = [0u8; MAX_LINE];
        let mut len = 0;
        for column in PROMPT.len()..self.column {
            // Only ASCII printable chars (or blanks) ever land in a cell via
            // `put`, so this narrowing cast never loses information.
            text[len] = self.cells[self.row][column] as u8;
            len += 1;
        }
        self.advance_row();
        self.pending_submission = Some(Submission { text, len });
    }

    /// Writes one line of command output starting at column 0 of the
    /// current (assumed blank) row, wrapping at the row width, and always
    /// leaves the console on a fresh blank row afterward -- ready for
    /// either the next output line or `write_prompt`.
    pub fn write_output_line(&mut self, text: &str) {
        self.column = 0;
        for ch in text.chars() {
            let ch = if (' '..='~').contains(&ch) { ch } else { ' ' };
            self.cells[self.row][self.column] = ch;
            self.column += 1;
            if self.column == COLUMNS {
                self.advance_row();
            }
        }
        self.advance_row();
    }
}

fn draw_ascii_text(
    framebuffers: &mut DoubleBuffer,
    index: usize,
    x: usize,
    y: usize,
    text: &str,
    scale: usize,
    color: u16,
    trace_first: bool,
) {
    for (column, ch) in text.chars().enumerate() {
        framebuffers.draw_ascii_char(
            index,
            x + column * 6 * scale,
            y,
            ch,
            scale,
            color,
            trace_first && column == 0,
        );
    }
}

impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}
