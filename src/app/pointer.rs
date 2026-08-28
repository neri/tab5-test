//! The mouse pointer: a sprite drawn over whatever is already on screen,
//! and taken back off without the caller having to redraw underneath it.
//!
//! This started inside `app::win`, which is where it had to be proved: the
//! desktop mock-up is the screen with a window, icons and a taskbar under
//! the pointer, so a sprite that only worked over flat colour would have
//! been obviously wrong there. It is here now because the browser needs the
//! same thing over a page of text, and the way to share it is not to copy
//! two hundred lines and a bitmap into a second file.
//!
//! The mechanism is [`Framebuffer::read_rect`] and [`Framebuffer::blit_rgb565`]:
//! save the pixels the sprite is about to cover, draw it, and put the saved
//! pixels back before anything else writes there. That is what makes it
//! independent of the content -- it does not know what is underneath and
//! does not need to.
//!
//! **The order is not negotiable**, and getting it wrong is the bug this
//! module exists to make hard:
//!
//! ```text
//! cursor.hide(framebuffer);        // put back what was under the sprite
//! ...draw whatever changed...      // now safe: nothing stale is saved
//! cursor.move_to(x, y);
//! cursor.show(framebuffer);        // save again, draw on top
//! flush_union(...);                // one writeback over both positions
//! ```
//!
//! The saved pixels are only valid until something else writes into that
//! region. Drawing first and hiding afterwards stamps a stale copy of the
//! old content over the new drawing, and it looks like a rectangle of the
//! previous frame following the pointer around.

use crate::framebuffer::{BLACK, Framebuffer, HEIGHT, WHITE, WIDTH};

/// The pointer sprite: `X` outline, `O` white fill, `.` transparent, with
/// the hotspot at the top-left corner exactly as the classic arrow has it.
/// Drawn as a bitmap rather than from primitives because the outline is
/// what makes it legible over both a teal desktop and a white page, and an
/// outline is easier to be sure of by eye than by geometry.
const CURSOR_WIDTH: usize = 12;
const CURSOR_HEIGHT: usize = 18;

/// Stored flat, one row after another, rather than as an array of row
/// literals: an array of 18 row references is a shape the optimizer
/// unrolls, and 216 unrolled `draw_pixel` calls cost more instruction
/// memory than this whole sprite is worth on a part with 256 KiB of RAM
/// for everything. Flat, it stays a loop.
const CURSOR_PIXELS: &[u8; CURSOR_WIDTH * CURSOR_HEIGHT] = b"\
X...........\
XX..........\
XOX.........\
XOOX........\
XOOOX.......\
XOOOOX......\
XOOOOOX.....\
XOOOOOOX....\
XOOOOOOOX...\
XOOOOOOOOX..\
XOOOOOOOOOX.\
XOOOOOOXXXXX\
XOOXOOX.....\
XOX.XOOX....\
XX..XOOX....\
X....XOOX...\
.....XOOX...\
......XXX...";

/// Pointer gain, as a fraction applied to the mouse's raw counts: at 1/1 one
/// count moves the pointer one pixel, which is slow going across 1280
/// pixels. Kept as a fraction rather than a whole multiplier so it can be
/// tuned finely -- 3/2 and 5/2 are both reasonable, where 2 and 3 are a big
/// jump apart -- and the leftover is carried in [`Cursor`] rather than
/// truncated, so slow deliberate movement is not rounded away to nothing.
const POINTER_SPEED_NUMERATOR: i32 = 5;
const POINTER_SPEED_DENOMINATOR: i32 = 2;

/// The sprite is drawn at 1:1, so the drawn size is the bitmap's own size.
/// Kept as separate names because callers -- `flush_union`'s rectangles,
/// `win`'s hit tests -- ask for the size on screen, not the size of the
/// bitmap, and those were two different numbers when the sprite was scaled.
pub const CURSOR_DRAWN_WIDTH: usize = CURSOR_WIDTH;
pub const CURSOR_DRAWN_HEIGHT: usize = CURSOR_HEIGHT;
const CURSOR_SAVED_PIXELS: usize = CURSOR_DRAWN_WIDTH * CURSOR_DRAWN_HEIGHT;

pub struct Cursor {
    pub x: usize,
    pub y: usize,
    /// Pixels underneath, laid out exactly as [`Framebuffer::read_rect`]
    /// wrote them so [`Framebuffer::blit_rgb565`] puts them back unchanged
    /// -- including when the sprite hangs off the right or bottom edge,
    /// which both clip identically.
    ///
    /// 216 pixels, 432 bytes, on the caller's stack rather than the heap:
    /// full-screen modes are entered from a shell command with the whole
    /// 128 KiB stack free, and a pointer that could fail to allocate would
    /// be a pointer with a failure path nobody would ever exercise.
    saved: [u16; CURSOR_SAVED_PIXELS],
    visible: bool,
    /// Sub-pixel motion left over from [`POINTER_SPEED_DENOMINATOR`],
    /// carried into the next frame. Without this, any frame whose scaled
    /// motion lands below one pixel would be discarded, and a slow drag
    /// across the screen would lose ground on every one of them.
    remainder_x: i32,
    remainder_y: i32,
}

impl Cursor {
    pub fn new(x: usize, y: usize) -> Self {
        Self {
            x,
            y,
            saved: [0; CURSOR_SAVED_PIXELS],
            visible: false,
            remainder_x: 0,
            remainder_y: 0,
        }
    }

    /// Where this frame's relative motion puts the hotspot, after pointer
    /// gain and clamped to the panel.
    ///
    /// The sprite itself is allowed to hang off the right and bottom edges
    /// from there; clamping its whole box instead would stop the hotspot
    /// short of the edge and make the last row of pixels unreachable.
    ///
    /// Clamping happens after scaling, and the remainder is still carried
    /// even when the clamp discards the movement -- so pushing the pointer
    /// into an edge and coming back does not first have to work off a debt.
    pub fn moved_to(&mut self, dx: i32, dy: i32) -> (usize, usize) {
        let scaled_x = scale_motion(dx, &mut self.remainder_x);
        let scaled_y = scale_motion(dy, &mut self.remainder_y);
        let x = (self.x as i32 + scaled_x).clamp(0, WIDTH as i32 - 1) as usize;
        let y = (self.y as i32 + scaled_y).clamp(0, HEIGHT as i32 - 1) as usize;
        (x, y)
    }

    /// Moves the hotspot. Only legal while the sprite is lifted: moving it
    /// while visible would leave the saved pixels describing one place and
    /// the drawn sprite another.
    pub fn move_to(&mut self, x: usize, y: usize) {
        debug_assert!(!self.visible);
        self.x = x;
        self.y = y;
    }

    #[inline(never)]
    pub fn hide(&mut self, framebuffer: &mut Framebuffer) {
        if !self.visible {
            return;
        }
        framebuffer.blit_rgb565(
            self.x,
            self.y,
            CURSOR_DRAWN_WIDTH,
            CURSOR_DRAWN_HEIGHT,
            &self.saved,
        );
        self.visible = false;
    }

    #[inline(never)]
    pub fn show(&mut self, framebuffer: &mut Framebuffer) {
        if self.visible {
            return;
        }
        framebuffer.read_rect(
            self.x,
            self.y,
            CURSOR_DRAWN_WIDTH,
            CURSOR_DRAWN_HEIGHT,
            &mut self.saved,
        );
        for (index, &cell) in CURSOR_PIXELS.iter().enumerate() {
            let color = match cell {
                b'X' => BLACK,
                b'O' => WHITE,
                _ => continue,
            };
            let (column, row) = (index % CURSOR_WIDTH, index / CURSOR_WIDTH);
            framebuffer.draw_pixel(self.x + column, self.y + row, color);
        }
        self.visible = true;
    }
}

/// Applies pointer gain to one axis, keeping the sub-pixel leftover in
/// `remainder` for the next frame.
///
/// Truncation is toward zero on both signs and `remainder` keeps the sign of
/// the motion, so moving left and moving right accumulate their leftovers
/// the same way instead of one direction drifting against the other.
fn scale_motion(delta: i32, remainder: &mut i32) -> i32 {
    let total = delta * POINTER_SPEED_NUMERATOR + *remainder;
    let moved = total / POINTER_SPEED_DENOMINATOR;
    *remainder = total - moved * POINTER_SPEED_DENOMINATOR;
    moved
}

/// Writes back one rectangle covering both positions of something that just
/// moved -- the pointer sprite, or a window being dragged.
///
/// One writeback rather than two: the rectangles overlap for anything but a
/// large jump, and a single frame's motion bounds how far apart they can be,
/// so the union stays a small fraction of the screen either way.
#[inline(never)]
pub fn flush_union(
    framebuffer: &Framebuffer,
    from: (usize, usize),
    to: (usize, usize),
    width: usize,
    height: usize,
) {
    let left = from.0.min(to.0);
    let top = from.1.min(to.1);
    let right = (from.0.max(to.0) + width).min(WIDTH);
    let bottom = (from.1.max(to.1) + height).min(HEIGHT);
    framebuffer.flush_rect(left, top, right - left, bottom - top);
}
