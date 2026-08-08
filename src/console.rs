//! A small, fixed-size text console rendered into an RGB565 framebuffer.

use core::cell::UnsafeCell;

use crate::framebuffer::{BLACK, DoubleBuffer, GREEN, WHITE, WIDTH};

const SCALE: usize = 3;
const CELL_WIDTH: usize = 6 * SCALE;
const CELL_HEIGHT: usize = 8 * SCALE;
const LEFT: usize = 16;
const TOP: usize = 40;
const COLUMNS: usize = (WIDTH - LEFT * 2) / CELL_WIDTH;
const ROWS: usize = 28;

// Only the 5x7 ASCII font is available (no Japanese glyphs), so the prompt
// uses a half-width '>' rather than the full-width '＞' a shell would show.
const PROMPT: &[u8] = b"> ";

/// Terminal-like text storage for the CardKB input echo display.
pub struct Console {
    cells: [[u8; COLUMNS]; ROWS],
    column: usize,
    row: usize,
    previous_was_carriage_return: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Update {
    None,
    Cell {
        column: usize,
        row: usize,
    },
    /// A new line's prompt was written; only its `PROMPT.len()` cells
    /// changed, so a full-screen redraw would waste PSRAM/GDMA bandwidth.
    Prompt {
        row: usize,
    },
    Full,
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
        let mut cells = [[0; COLUMNS]; ROWS];
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
        }
    }

    /// Adds one CardKB byte using familiar terminal control characters.
    pub fn push(&mut self, byte: u8) -> Update {
        let update = match byte {
            b'\r' => {
                let scrolled = self.new_line();
                self.previous_was_carriage_return = true;
                self.line_update(scrolled)
            }
            b'\n' if self.previous_was_carriage_return => {
                self.previous_was_carriage_return = false;
                Update::None
            }
            b'\n' => {
                let scrolled = self.new_line();
                self.line_update(scrolled)
            }
            0x08 | 0x7f => self.backspace(),
            b'\t' => {
                let spaces = 4 - self.column % 4;
                let mut wrap_update = Update::None;
                for _ in 0..spaces {
                    let result = self.put(b' ');
                    if matches!(result, Update::Full | Update::Prompt { .. }) {
                        wrap_update = result;
                    }
                }
                // Blank tab cells need no pixel update on a normal forward
                // cursor. A row wrap still needs its own redraw.
                wrap_update
            }
            b' '..=b'~' => self.put(byte),
            _ => Update::None,
        };
        if byte != b'\r' {
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
            Update::Prompt { row: self.row }
        }
    }

    /// Draws the complete console into one (currently inactive) framebuffer.
    #[inline(always)]
    pub fn render(&self, framebuffers: &mut DoubleBuffer, index: usize) {
        let cursor_row = self.row;
        let cursor_column = self.column;
        framebuffers.fill_console_background(index, 32, GREEN, BLACK);

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
                let byte = self.cells[row][column];
                if byte != 0 && byte != b' ' {
                    framebuffers.draw_ascii_char(
                        index,
                        LEFT + column * CELL_WIDTH,
                        TOP + row * CELL_HEIGHT,
                        byte,
                        SCALE,
                        WHITE,
                        false,
                    );
                }
            }
        }

        draw_ascii_text(
            framebuffers,
            index,
            LEFT,
            8,
            b"Tab5 Console",
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
        let byte = self.cells[row][column];
        if byte == 0 || byte == b' ' {
            // Backspace and explicit spaces erase the complete cell.
            framebuffers.fill_rect(index, x, y, CELL_WIDTH, CELL_HEIGHT, BLACK);
        } else {
            // Forward input always enters an already-clear cell. Avoid an
            // unnecessary sparse PSRAM clear before drawing the glyph.
            framebuffers.draw_ascii_char(index, x, y, byte, SCALE, WHITE, false);
        }
    }

    pub fn flush_cell(
        &self,
        framebuffers: &DoubleBuffer,
        index: usize,
        column: usize,
        row: usize,
    ) -> bool {
        if column >= COLUMNS || row >= ROWS {
            return false;
        }
        framebuffers.flush_rect(
            index,
            LEFT + column * CELL_WIDTH,
            TOP + row * CELL_HEIGHT,
            CELL_WIDTH,
            CELL_HEIGHT,
        )
    }

    /// Repaints a freshly written prompt (`PROMPT.len()` cells) on one row.
    pub fn render_prompt(&self, framebuffers: &mut DoubleBuffer, index: usize, row: usize) {
        for column in 0..PROMPT.len() {
            self.render_cell(framebuffers, index, column, row);
        }
    }

    /// Flushes a freshly written prompt's cells. Returns `false` if any cell
    /// failed to flush.
    pub fn flush_prompt(&self, framebuffers: &DoubleBuffer, index: usize, row: usize) -> bool {
        let mut ok = true;
        for column in 0..PROMPT.len() {
            ok &= self.flush_cell(framebuffers, index, column, row);
        }
        ok
    }

    fn put(&mut self, byte: u8) -> Update {
        let column = self.column;
        let row = self.row;
        self.cells[self.row][self.column] = byte;
        self.column += 1;
        if self.column == COLUMNS {
            let scrolled = self.new_line();
            return self.line_update(scrolled);
        }
        Update::Cell { column, row }
    }

    /// Erases the previous character. Never crosses into the prompt, so a
    /// line's leading "> " can't be backspaced away.
    fn backspace(&mut self) -> Update {
        if self.column <= PROMPT.len() {
            return Update::None;
        }
        self.column -= 1;
        self.cells[self.row][self.column] = 0;
        Update::Cell {
            column: self.column,
            row: self.row,
        }
    }

    /// Advances to the next row (scrolling if needed), writes a fresh prompt
    /// into it, and reports whether the bottom row scrolled.
    fn new_line(&mut self) -> bool {
        self.row += 1;
        let scrolled = self.row == ROWS;
        if scrolled {
            for row in 1..ROWS {
                self.cells[row - 1] = self.cells[row];
            }
            self.cells[ROWS - 1] = [0; COLUMNS];
            self.row = ROWS - 1;
        }
        for (column, &byte) in PROMPT.iter().enumerate() {
            self.cells[self.row][column] = byte;
        }
        self.column = PROMPT.len();
        scrolled
    }
}

fn draw_ascii_text(
    framebuffers: &mut DoubleBuffer,
    index: usize,
    x: usize,
    y: usize,
    text: &[u8],
    scale: usize,
    color: u16,
    trace_first: bool,
) {
    for (column, &byte) in text.iter().enumerate() {
        framebuffers.draw_ascii_char(
            index,
            x + column * 6 * scale,
            y,
            byte,
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
