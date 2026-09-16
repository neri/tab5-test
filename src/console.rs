//! A small, fixed-size text console rendered into an RGB565 framebuffer.
//!
//! The cell array is the console's state; the framebuffer is its view. Every
//! call that changes a cell also paints the cells it changed and writes that
//! span back to PSRAM before returning, so the two never drift apart and no
//! caller has to remember to flush anything afterwards.
//!
//! Writes that move cells *under* the pixels already on screen cannot be
//! expressed as a span, so they adjust the cell array first and then bring the
//! pixels back into line with it. `clear` and a return from a full-screen mode
//! repaint everything. A scroll does not: the rows that merely moved already
//! hold the right pixels one row higher, so `scroll_screen` slides them with a
//! block copy and repaints only what the copy could not have carried.

use core::cell::UnsafeCell;

use crate::font;
use crate::font::console::{BLANK, Id};
use crate::framebuffer::{BLACK, Framebuffer, HEIGHT, WHITE, WIDTH};
use crate::input::Key;
use crate::uart;

/// One cell is one half-width glyph at its natural size: no scaling, and the
/// same 8x16 box `font::console::cell_glyph` always returns.
const CELL_WIDTH: usize = 8;
const CELL_HEIGHT: usize = font::HEIGHT;
const LEFT: usize = 16;
const TOP: usize = 8;
/// Cells across one row. Public because `ls` lays its names out in columns
/// and has to know how many characters reach the edge; every other caller
/// writes a line and lets the console wrap it.
pub const COLUMNS: usize = (WIDTH - LEFT * 2) / CELL_WIDTH;
const ROWS: usize = (HEIGHT - TOP) / CELL_HEIGHT;

// The grid `docs/plans/archive/FONT_MIGRATION_PLAN.md` sizes everything else against. The
// arithmetic above is what defines it; this is here so that a change to the
// margins or the cell size has to be a deliberate one rather than a quiet
// reshaping of every screen that counts columns.
const _: () = assert!(COLUMNS == 156 && ROWS == 44);

// The console is a half-width fixed terminal by design -- every cell is one
// 8x16 glyph -- so the prompt is a half-width '>' rather than the full-width
// '＞' a shell would show. See `docs/plans/archive/FONT_MIGRATION_PLAN.md`.
const PROMPT: [Id; 2] = [font::console::id('>'), font::console::id(' ')];

/// Longest command line `submit` can capture: one row minus the prompt. This
/// is the hard limit on input, not a buffer that happens to be this size --
/// `put` stops accepting characters when the row is full.
const MAX_LINE: usize = COLUMNS - PROMPT.len();

/// Terminal-like text storage for the keyboard input echo display.
pub struct Console {
    /// One byte per cell: the glyph to paint, not the character that produced
    /// it. Every Unicode scalar written to the console maps to exactly one of
    /// these (`font::console::id`), so a full-width character or one the font
    /// does not have occupies a single cell holding a visible placeholder
    /// rather than splitting the row or vanishing into a blank.
    cells: [[Id; COLUMNS]; ROWS],
    column: usize,
    row: usize,
    /// First unused cell on the current editable line.  It is distinct from
    /// `column` while the cursor has moved left, so Enter still submits the
    /// full line and Insert/Delete can shift its suffix correctly.
    input_end: usize,
    previous_was_carriage_return: bool,
    /// Current blink phase of the pseudo cursor block drawn at `(column,
    /// row)`. The cursor cell itself never holds a printable character (it
    /// is always the next write position, or a position `backspace` has
    /// just cleared), so `render_cell` can use this flag to decide between
    /// the cursor block and that cell's normal (blank) contents without
    /// tracking the glyph underneath.
    cursor_visible: bool,
    /// Set by `submit` when Enter completes a command line; drained by
    /// `take_submission`. Kept separate from drawing since dispatching a
    /// command is an application-level reaction, not a rendering step.
    pending_submission: Option<Submission>,
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

/// A command line that was still being typed when something else needed to
/// write to the console. See [`Console::take_input_line`].
pub struct InputLine {
    text: [u8; MAX_LINE],
    len: usize,
    /// Where the cursor was, counted from the first cell after the prompt.
    cursor: usize,
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
        let mut cells = [[BLANK; COLUMNS]; ROWS];
        let mut i = 0;
        while i < PROMPT.len() {
            cells[0][i] = PROMPT[i];
            i += 1;
        }
        Self {
            cells,
            column: PROMPT.len(),
            row: 0,
            input_end: PROMPT.len(),
            previous_was_carriage_return: false,
            cursor_visible: true,
            pending_submission: None,
        }
    }

    /// Blanks every cell, returns to a fresh, empty first row (no prompt: the
    /// caller writes it, same convention as `submit`), and repaints.
    ///
    /// The repaint is what makes this the way back from `paint`, `touchtest`
    /// and the other full-screen modes: they leave their own drawing in the
    /// framebuffer, and this puts the console back over it.
    pub fn clear(&mut self, framebuffer: &mut Framebuffer) {
        self.cells = [[BLANK; COLUMNS]; ROWS];
        self.column = 0;
        self.row = 0;
        self.input_end = 0;
        self.previous_was_carriage_return = false;
        self.cursor_visible = true;
        self.redraw(framebuffer);
    }

    /// Takes the command line captured by the most recent `submit`, if any.
    pub fn take_submission(&mut self) -> Option<Submission> {
        self.pending_submission.take()
    }

    /// Flips the blink phase and repaints the cursor's own cell. Called from
    /// the frame loop's idle timer, so it must stay this cheap.
    pub fn blink_cursor(&mut self, framebuffer: &mut Framebuffer) {
        self.cursor_visible = !self.cursor_visible;
        self.draw_cursor(framebuffer);
    }

    /// Handles a normalized key event.  Printable ASCII retains the original
    /// console behavior; navigation and editing keys operate on the current
    /// command line. Esc clears that line. Up/Down, paging, Insert, and
    /// function keys are accepted here but intentionally have no behavior
    /// until command history or another consumer is added.
    pub fn push_key(&mut self, framebuffer: &mut Framebuffer, key: Key) {
        // Typing always shows a solid cursor; only idle time blinks it. This
        // runs before the edit so the edit's own repaint already carries the
        // solid block, and so keys with no effect still bring it back.
        self.show_cursor(framebuffer);
        match key {
            Key::Ascii(byte) => self.push(framebuffer, char::from(byte)),
            Key::Escape => self.clear_input_line(framebuffer),
            Key::ArrowLeft => {
                self.move_cursor(framebuffer, self.column.saturating_sub(1).max(PROMPT.len()));
            }
            Key::ArrowRight => self.move_cursor(framebuffer, (self.column + 1).min(self.input_end)),
            Key::Home => self.move_cursor(framebuffer, PROMPT.len()),
            Key::End => self.move_cursor(framebuffer, self.input_end),
            Key::Delete => self.delete(framebuffer),
            // Ctrl with a letter is listed rather than left to a wildcard:
            // the shell has no command bound to one, and a line editor that
            // silently inserted something for it would be worse than a key
            // that does nothing.
            Key::Control(_)
            | Key::ArrowUp
            | Key::ArrowDown
            | Key::PageUp
            | Key::PageDown
            | Key::Insert
            | Key::Function(_) => {}
        }
    }

    /// Lifts the line being edited off the current row and blanks it, so
    /// that output can be written where it was.
    ///
    /// Every other writer of output lines runs immediately after Enter
    /// consumed the input, on a row that is already blank. Automount does
    /// not: it reports while the user may be halfway through typing
    /// something, and `write_output_line` starts at column 0 and would take
    /// the prompt and the half-typed command with it. Pair this with
    /// [`Self::restore_input_line`] around the output.
    pub fn take_input_line(&mut self, framebuffer: &mut Framebuffer) -> InputLine {
        let mut text = [0u8; MAX_LINE];
        let mut len = 0;
        for column in PROMPT.len()..self.input_end {
            // Only printable ASCII reaches a cell through `put`, the same
            // narrowing `submit` relies on.
            text[len] = ascii_of(self.cells[self.row][column]);
            len += 1;
        }
        let cursor = self.column.saturating_sub(PROMPT.len());
        // The cursor block may be sitting past the last character, so the
        // repaint has to cover it as well as the cells being blanked.
        let painted = self.input_end.max(self.column + 1);
        for column in 0..self.input_end {
            self.cells[self.row][column] = BLANK;
        }
        self.column = 0;
        self.input_end = 0;
        self.draw_span(framebuffer, self.row, 0, painted);
        InputLine { text, len, cursor }
    }

    /// Puts the prompt and a line taken by [`Self::take_input_line`] back on
    /// the current (assumed blank) row, cursor where it was.
    ///
    /// The cells are filled in directly and painted in one span rather than
    /// replayed through `put`, which would repaint a widening span per
    /// character and write it back each time.
    pub fn restore_input_line(&mut self, framebuffer: &mut Framebuffer, saved: InputLine) {
        self.set_prompt_cells();
        for index in 0..saved.len {
            self.cells[self.row][PROMPT.len() + index] =
                font::console::id(char::from(saved.text[index]));
        }
        self.input_end = PROMPT.len() + saved.len;
        self.column = (PROMPT.len() + saved.cursor).min(self.input_end);
        self.cursor_visible = true;
        self.draw_span(
            framebuffer,
            self.row,
            0,
            self.input_end.max(self.column + 1),
        );
    }

    /// Writes the prompt into the current (assumed blank) row, positions
    /// `column` after it, and paints it.
    pub fn write_prompt(&mut self, framebuffer: &mut Framebuffer) {
        self.set_prompt_cells();
        self.draw_span(framebuffer, self.row, 0, self.column + 1);
    }

    /// Writes one line of command output starting at column 0 of the
    /// current (assumed blank) row, wrapping at the row width, and always
    /// leaves the console on a fresh blank row afterward -- ready for
    /// either the next output line or `write_prompt`.
    ///
    /// Every line is also mirrored to the UART log. This is the single
    /// funnel for all shell command output, so mirroring here (rather than
    /// at each of `shell.rs`'s call sites) means hardware bring-up results
    /// can be read off the serial log instead of squinting at the panel
    /// and retyping them -- which is how every other module in this
    /// project already reports itself. Unlike the panel, the UART gets the
    /// line unwrapped and in full.
    pub fn write_output_line(&mut self, framebuffer: &mut Framebuffer, text: &str) {
        uart::log(text.as_bytes());
        uart::log(b"\r\n");
        self.write_output_line_pixels(framebuffer, text);
    }

    /// Writes a diagnostic line without mirroring it to UART. Long automated
    /// scroll tests use this to exercise the exact console pixel path without
    /// making serial-output backpressure part of the display workload.
    pub(crate) fn diagnostic_write_output_line(
        &mut self,
        framebuffer: &mut Framebuffer,
        text: &str,
    ) {
        self.write_output_line_pixels(framebuffer, text);
    }

    fn write_output_line_pixels(&mut self, framebuffer: &mut Framebuffer, text: &str) {
        let first_row = self.row;
        self.column = 0;
        let mut scrolled_rows = 0;
        // Widest column any of this line's rows reached. Only these columns
        // are painted and written back; a writeback's cost is set by its
        // column span (CW rotation makes that a run of native rows), so a
        // short line must not pay for the full row width.
        let mut widest = 0;
        for character in text.chars() {
            // One cell per scalar, whatever the scalar is. Full-width
            // characters, combining marks and characters outside the font all
            // become a visible placeholder here rather than a blank; the UART
            // mirror above still carries the original UTF-8.
            self.cells[self.row][self.column] = font::console::id(character);
            self.column += 1;
            widest = widest.max(self.column);
            if self.column == COLUMNS {
                scrolled_rows += usize::from(self.advance_row());
            }
        }
        scrolled_rows += usize::from(self.advance_row());

        if scrolled_rows > 0 {
            // The rows moved under the pixels already on screen, so
            // `first_row` no longer names the row this line started on: it is
            // now that many rows higher. From there down is exactly this
            // line's own cells plus the row the scroll cleared, none of which
            // any pixel on screen reflects yet.
            self.scroll_screen(
                framebuffer,
                scrolled_rows,
                first_row.saturating_sub(scrolled_rows),
            );
            return;
        }
        for row in first_row..self.row {
            self.draw_span(framebuffer, row, 0, widest);
        }
        // `advance_row` left the cursor at the start of the fresh row.
        self.draw_cursor(framebuffer);
    }

    /// Adds one keyboard character using familiar terminal control characters.
    fn push(&mut self, framebuffer: &mut Framebuffer, ch: char) {
        match ch {
            '\r' => {
                self.submit(framebuffer);
                self.previous_was_carriage_return = true;
            }
            '\n' if self.previous_was_carriage_return => {
                self.previous_was_carriage_return = false;
            }
            '\n' => self.submit(framebuffer),
            '\u{8}' | '\u{7f}' => self.backspace(framebuffer),
            '\t' => {
                let spaces = 4 - self.column % 4;
                for _ in 0..spaces {
                    self.put(framebuffer, ' ');
                }
            }
            ' '..='~' => self.put(framebuffer, ch),
            _ => {}
        }
        if ch != '\r' {
            self.previous_was_carriage_return = false;
        }
    }

    /// Inserts one character at the cursor.
    ///
    /// A command line is one screen row and no more: `submit` reads the
    /// current row's cells and nothing else, and `Submission` is sized to
    /// match. So a full row stops accepting characters rather than continuing
    /// somewhere `submit` will not look. It used to wrap onto a fresh prompt
    /// row here, which put the typed text out of `submit`'s reach and silently
    /// discarded the command.
    fn put(&mut self, framebuffer: &mut Framebuffer, ch: char) {
        if self.input_end == COLUMNS {
            return;
        }
        let row = self.row;
        let previous_column = self.column;
        for index in (self.column..self.input_end).rev() {
            self.cells[row][index + 1] = self.cells[row][index];
        }
        self.cells[row][self.column] = font::console::id(ch);
        self.column += 1;
        self.input_end += 1;
        self.draw_edit(framebuffer, previous_column, self.input_end);
    }

    /// Erases the previous character. Never crosses into the prompt, so a
    /// line's leading "> " can't be backspaced away.
    fn backspace(&mut self, framebuffer: &mut Framebuffer) {
        if self.column <= PROMPT.len() {
            return;
        }
        let previous_column = self.column;
        let old_end = self.input_end;
        self.column -= 1;
        self.remove_at_cursor();
        self.draw_edit(framebuffer, previous_column, old_end);
    }

    fn delete(&mut self, framebuffer: &mut Framebuffer) {
        if self.column >= self.input_end {
            return;
        }
        let old_end = self.input_end;
        self.remove_at_cursor();
        self.draw_edit(framebuffer, self.column, old_end);
    }

    /// Drops the character under the cursor and closes the gap, leaving the
    /// vacated last cell blank.
    fn remove_at_cursor(&mut self) {
        for index in self.column..self.input_end - 1 {
            self.cells[self.row][index] = self.cells[self.row][index + 1];
        }
        self.input_end -= 1;
        self.cells[self.row][self.input_end] = BLANK;
    }

    fn clear_input_line(&mut self, framebuffer: &mut Framebuffer) {
        if self.input_end == PROMPT.len() && self.column == PROMPT.len() {
            return;
        }
        let previous_column = self.column;
        let old_end = self.input_end;
        for column in PROMPT.len()..old_end {
            self.cells[self.row][column] = BLANK;
        }
        self.column = PROMPT.len();
        self.input_end = PROMPT.len();
        self.draw_edit(framebuffer, previous_column, old_end);
    }

    fn move_cursor(&mut self, framebuffer: &mut Framebuffer, column: usize) {
        let column = column.clamp(PROMPT.len(), self.input_end);
        if column == self.column {
            return;
        }
        let previous_column = self.column;
        self.column = column;
        // No cell changed; only the block moved, so `draw_edit`'s own cursor
        // coverage is the whole repaint.
        self.draw_edit(framebuffer, previous_column, 0);
    }

    /// Handles Enter: captures the just-typed line (the cells between the
    /// prompt and the cursor) into `pending_submission` for `take_submission`,
    /// then advances to a fresh blank row. The row is deliberately left
    /// without a prompt -- unlike `new_line` -- since command output, not
    /// more input, comes next.
    fn submit(&mut self, framebuffer: &mut Framebuffer) {
        let mut text = [0u8; MAX_LINE];
        let mut len = 0;
        for column in PROMPT.len()..self.input_end {
            // Only ASCII printable chars (or blanks) ever land in a cell via
            // `put`, so this narrowing never loses information.
            text[len] = ascii_of(self.cells[self.row][column]);
            len += 1;
        }
        let submitted_row = self.row;
        let submitted_column = self.column;
        if self.advance_row() {
            // The submitted row's own pixels move up correctly, but it still
            // carries the cursor block where the cursor no longer is, so it
            // has to be repainted along with the row the scroll cleared.
            self.scroll_screen(framebuffer, 1, submitted_row.saturating_sub(1));
        } else {
            // Nothing on the submitted row changed, but the cursor left it,
            // so its block has to be erased where it stood.
            self.draw_span(
                framebuffer,
                submitted_row,
                submitted_column,
                submitted_column + 1,
            );
            self.draw_cursor(framebuffer);
        }
        self.pending_submission = Some(Submission { text, len });
    }

    /// Advances to the next row (scrolling if needed) and reports whether
    /// the bottom row scrolled. Leaves the row blank and `column` at 0; it
    /// carries no prompt until `set_prompt_cells` adds one. Touches cells
    /// only -- the caller repaints, since a scroll and a plain advance need
    /// very different amounts of it.
    fn advance_row(&mut self) -> bool {
        self.row += 1;
        let scrolled = self.row == ROWS;
        if scrolled {
            for row in 1..ROWS {
                self.cells[row - 1] = self.cells[row];
            }
            self.cells[ROWS - 1] = [BLANK; COLUMNS];
            self.row = ROWS - 1;
        }
        self.column = 0;
        self.input_end = 0;
        scrolled
    }

    /// Moves the grid up by `rows` cell rows on screen, then repaints rows
    /// `repaint_from..ROWS` from the cell array.
    ///
    /// The cell array has already been shifted by `advance_row`; this brings
    /// the pixels into line with it. Every row that only moved already says
    /// what it should say one row higher, so a block copy expresses that part
    /// of the operation entirely and the CPU touches none of those pixels --
    /// where a full repaint would re-render every glyph on screen, at roughly
    /// the price of a screen clear.
    ///
    /// `repaint_from` is the caller's business because "what the copy got
    /// right" is: the copy carries the pixels that were *on screen*, and cells
    /// written during this update never were. A wrapped output line has
    /// already filled several rows of the array that no pixel anywhere
    /// reflects, so it is not enough to repaint only what the scroll exposed
    /// at the bottom -- the caller knows how far up its own writes reached and
    /// says so.
    ///
    /// Falls back to a full repaint when there is nothing left worth
    /// preserving, or when the copy did not happen. `redraw` is correct in
    /// every case; it is only slower.
    fn scroll_screen(&self, framebuffer: &mut Framebuffer, rows: usize, repaint_from: usize) {
        let grid_height = ROWS * CELL_HEIGHT;
        if rows >= ROWS
            || repaint_from == 0
            || !framebuffer.scroll_up(TOP, grid_height, rows * CELL_HEIGHT)
        {
            self.redraw(framebuffer);
            return;
        }
        for row in repaint_from..ROWS {
            self.draw_span(framebuffer, row, 0, COLUMNS);
        }
    }

    /// Fills the current row's prompt cells and positions `column` after
    /// them, without drawing.
    fn set_prompt_cells(&mut self) {
        for (column, &id) in PROMPT.iter().enumerate() {
            self.cells[self.row][column] = id;
        }
        self.column = PROMPT.len();
        self.input_end = PROMPT.len();
    }

    /// Forces the cursor block on, e.g. so activity doesn't leave the cursor
    /// mid-blink at its new position.
    fn show_cursor(&mut self, framebuffer: &mut Framebuffer) {
        if !self.cursor_visible {
            self.cursor_visible = true;
            self.draw_cursor(framebuffer);
        }
    }

    /// Repaints the cursor's own cell.
    fn draw_cursor(&self, framebuffer: &mut Framebuffer) {
        self.draw_span(framebuffer, self.row, self.column, self.column + 1);
    }

    /// Repaints what an edit to the current row touched: the cells between
    /// the edit's start and `dirty_end`, plus both the cell the cursor left
    /// and the one it moved to.
    ///
    /// The vacated cursor cell is the reason this takes `previous_column` at
    /// all: it is the only chance to erase the block, since every later
    /// repaint draws it at its new position instead.
    fn draw_edit(&self, framebuffer: &mut Framebuffer, previous_column: usize, dirty_end: usize) {
        let start = previous_column.min(self.column);
        let end = dirty_end.max(previous_column.max(self.column) + 1);
        self.draw_span(framebuffer, self.row, start, end);
    }

    /// Repaints columns `start..end` of one row and writes them back.
    ///
    /// One writeback covers the whole span: CW rotation maps a run of logical
    /// X onto a contiguous run of native rows, so these cells share a single
    /// PSRAM range. That range grows with the *column* count, not the cell
    /// count, which is why callers narrow `start..end` to what they actually
    /// changed rather than repainting whole rows.
    fn draw_span(&self, framebuffer: &mut Framebuffer, row: usize, start: usize, end: usize) {
        let end = end.min(COLUMNS);
        if row >= ROWS || start >= end {
            return;
        }
        for column in start..end {
            self.render_cell(framebuffer, column, row);
        }
        if !framebuffer.flush_rect(
            LEFT + start * CELL_WIDTH,
            TOP + row * CELL_HEIGHT,
            (end - start) * CELL_WIDTH,
            CELL_HEIGHT,
        ) {
            uart::log(b"Console: cell writeback failed\r\n");
        }
    }

    /// Paints one text cell. Pixels only: `draw_span` owns the writeback so
    /// a run of cells costs one.
    ///
    /// A cell is exactly one half-width glyph box, so passing the background
    /// to `draw_glyph` paints the whole cell in one call. That matters because
    /// every repaint has to overwrite the cell's previous contents -- another
    /// glyph, or the cursor block's white -- and doing it with a separate
    /// `fill_rect` first wrote a third of the cell's pixels twice.
    fn render_cell(&self, framebuffer: &mut Framebuffer, column: usize, row: usize) {
        let x = LEFT + column * CELL_WIDTH;
        let y = TOP + row * CELL_HEIGHT;
        if self.cursor_visible && column == self.column && row == self.row {
            // Always this cell's only content: it is either the untouched
            // next write position or one `backspace` just cleared. A solid
            // block is `fill_rect`'s own job -- there is no glyph to draw and
            // nothing written twice.
            framebuffer.fill_rect(x, y, CELL_WIDTH, CELL_HEIGHT, WHITE);
            return;
        }
        let glyph = font::console::cell_glyph(self.cells[row][column]);
        framebuffer.draw_glyph(x, y, &glyph, 1, WHITE, Some(BLACK));
    }

    /// Repaints the whole screen from the cell array and writes all of it
    /// back.
    ///
    /// This is the fallback for changes no span describes: a scroll moves
    /// every row's contents one row up, and `clear` (or a return from a
    /// full-screen mode) invalidates the entire picture. It is also this
    /// firmware's single heaviest PSRAM burst, so the callers above take
    /// some trouble to reach it only when they must.
    ///
    /// Blanking with `fill` first and then painting glyphs alone beats
    /// painting each cell with its own background, on both counts. The cell
    /// grid covers 95% of the screen, so the rest -- the margins `LEFT` and
    /// `TOP` leave -- would keep the previous full-screen mode's drawing,
    /// which is exactly what this path exists to remove. And it is cheaper
    /// anyway: `fill` writes pixel pairs, clearing all 921,600 of them in
    /// 460,800 stores, where per-cell backgrounds cost 878,592 stores for the
    /// 95%. Glyphs then add ~48 pixels each and blanks cost nothing at all.
    fn redraw(&self, framebuffer: &mut Framebuffer) {
        framebuffer.fill(BLACK);
        for row in 0..ROWS {
            for column in 0..COLUMNS {
                let id = self.cells[row][column];
                if id != BLANK {
                    framebuffer.draw_glyph(
                        LEFT + column * CELL_WIDTH,
                        TOP + row * CELL_HEIGHT,
                        &font::console::cell_glyph(id),
                        1,
                        WHITE,
                        // `fill` has already blanked the screen, so only the
                        // glyph's own pixels are left to write.
                        None,
                    );
                }
            }
        }
        if self.cursor_visible {
            framebuffer.fill_rect(
                LEFT + self.column * CELL_WIDTH,
                TOP + self.row * CELL_HEIGHT,
                CELL_WIDTH,
                CELL_HEIGHT,
                WHITE,
            );
        }
        if !framebuffer.flush() {
            uart::log(b"Console: full writeback failed\r\n");
        }
    }
}

/// The ASCII byte a cell stands for, for recovering a typed line from the
/// grid. Input reaches a cell only through `put`, which takes printable ASCII,
/// so anything else here is a blank.
fn ascii_of(id: Id) -> u8 {
    match font::console::character(id) {
        Some(character) if character.is_ascii() => character as u8,
        _ => b' ',
    }
}

impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}
