//! A small, fixed-size text console rendered into an RGB565 framebuffer.

use core::cell::UnsafeCell;

use crate::{
    framebuffer::{BLACK, DoubleBuffer, GREEN, WHITE, WIDTH},
    uart,
};

const SCALE: usize = 3;
const CELL_WIDTH: usize = 6 * SCALE;
const CELL_HEIGHT: usize = 8 * SCALE;
const LEFT: usize = 16;
const TOP: usize = 40;
const COLUMNS: usize = (WIDTH - LEFT * 2) / CELL_WIDTH;
const ROWS: usize = 28;

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
    Cell { column: usize, row: usize },
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
        Self {
            cells: [[0; COLUMNS]; ROWS],
            column: 0,
            row: 0,
            previous_was_carriage_return: false,
        }
    }

    /// Adds one CardKB byte using familiar terminal control characters.
    pub fn push(&mut self, byte: u8) -> Update {
        let update = match byte {
            b'\r' => {
                let update = if self.new_line() {
                    Update::Full
                } else {
                    Update::None
                };
                self.previous_was_carriage_return = true;
                update
            }
            b'\n' if self.previous_was_carriage_return => {
                self.previous_was_carriage_return = false;
                Update::None
            }
            b'\n' => {
                if self.new_line() {
                    Update::Full
                } else {
                    Update::None
                }
            }
            0x08 | 0x7f => self.backspace(),
            b'\t' => {
                let spaces = 4 - self.column % 4;
                let mut scrolled = false;
                for _ in 0..spaces {
                    scrolled |= matches!(self.put(b' '), Update::Full);
                }
                // Blank tab cells need no pixel update on a normal forward
                // cursor. A bottom-row scroll still needs a complete redraw.
                if scrolled { Update::Full } else { Update::None }
            }
            b' '..=b'~' => self.put(byte),
            _ => Update::None,
        };
        if byte != b'\r' {
            self.previous_was_carriage_return = false;
        }
        update
    }

    /// Draws the complete console into one (currently inactive) framebuffer.
    #[inline(always)]
    pub fn render(&self, framebuffers: &mut DoubleBuffer, index: usize) {
        uart::log(b"Console render: begin\r\n");
        let cursor_row = self.row;
        let cursor_column = self.column;
        uart::log(b"Console render: state loaded\r\n");
        framebuffers.fill_console_background(index, 32, GREEN, BLACK);
        uart::log(b"Console render: background done\r\n");

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
        uart::log(b"Console render: body done\r\n");

        draw_ascii_text(
            framebuffers,
            index,
            LEFT,
            8,
            b"CARDKB V1.1 CONSOLE",
            2,
            BLACK,
            true,
        );
        uart::log(b"Console render: header text done\r\n");
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

    fn put(&mut self, byte: u8) -> Update {
        let column = self.column;
        let row = self.row;
        self.cells[self.row][self.column] = byte;
        self.column += 1;
        if self.column == COLUMNS {
            if self.new_line() {
                return Update::Full;
            }
        }
        Update::Cell { column, row }
    }

    fn backspace(&mut self) -> Update {
        if self.column > 0 {
            self.column -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.column = COLUMNS - 1;
        } else {
            return Update::None;
        }
        self.cells[self.row][self.column] = 0;
        Update::Cell {
            column: self.column,
            row: self.row,
        }
    }

    fn new_line(&mut self) -> bool {
        self.column = 0;
        self.row += 1;
        if self.row == ROWS {
            for row in 1..ROWS {
                self.cells[row - 1] = self.cells[row];
            }
            self.cells[ROWS - 1] = [0; COLUMNS];
            self.row = ROWS - 1;
            true
        } else {
            false
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echoes_printable_characters_and_wraps() {
        let mut console = Console::new();
        assert_eq!(console.push(b'a'), Update::Cell { column: 0, row: 0 });
        assert_eq!(console.cells[0][0], b'a');

        for _ in 1..COLUMNS {
            console.push(b'x');
        }
        assert_eq!(console.row, 1);
        assert_eq!(console.column, 0);
    }

    #[test]
    fn crlf_is_one_new_line_and_backspace_erases() {
        let mut console = Console::new();
        console.push(b'A');
        console.push(b'\r');
        console.push(b'\n');
        assert_eq!(console.row, 1);
        console.push(b'B');
        assert_eq!(console.push(0x08), Update::Cell { column: 0, row: 1 });
        assert_eq!(console.cells[1][0], 0);
    }
}
