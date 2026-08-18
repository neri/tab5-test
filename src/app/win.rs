//! Full-screen Windows 95 desktop mock-up entered through the shell's
//! `win` command, and the exercise for `usb::hid_mouse`.
//!
//! Its reason for existing is the mouse: a pointer is the only input this
//! project has that needs a *position* rather than an event, so it is also
//! the first thing that needs a sprite drawn over arbitrary content and
//! taken back off again. `Cursor` below is that mechanism, built on
//! `Framebuffer::read_rect`/`blit_rgb565` rather than on redrawing the
//! desktop underneath, so it stays correct over the window and icons and
//! not just over flat background.
//!
//! The window can be dragged by its title bar, the way Windows 95 itself
//! did it with "show window contents while dragging" off: a dithered
//! outline follows the pointer and the window jumps to it on release. That
//! is not nostalgia for its own sake -- see `Outline` for why drawing the
//! window itself every frame is the expensive option here.
//!
//! Everything else is decoration with no behaviour behind it: the Start
//! button does not open, the close box does not close, and the desktop
//! icons do nothing. The other live element is the taskbar clock, read from
//! `rtc`. The rest is deliberately TBD.

use crate::framebuffer::{BLACK, Framebuffer, HEIGHT, WHITE, WIDTH};
use crate::input::InputManager;
use crate::usb::MOUSE_BUTTON_LEFT;
use crate::{interrupts, rtc, uart};

/// The Windows 95 system palette, as close as RGB565 gets.
///
/// The 3D look is entirely these four greys in a fixed order (see
/// `draw_raised`/`draw_sunken`), which is why they are named for their role
/// in that border rather than for their brightness.
const DESKTOP: u16 = 0x0410; // teal, (0,128,128)
const FACE: u16 = 0xC618; // control face, (192,192,192)
const SHADOW: u16 = 0x8410; // (128,128,128)
const HILIGHT: u16 = WHITE;
const DARK_SHADOW: u16 = BLACK;
const TITLE_BAR: u16 = 0x0010; // active caption navy, (0,0,128)
const ICON_SCREEN: u16 = 0x03F3; // the little CRT's phosphor
const FLAG_RED: u16 = 0xF800;
const FLAG_GREEN: u16 = 0x07E0;
const FLAG_BLUE: u16 = 0x001F;
const FLAG_YELLOW: u16 = 0xFFE0;

const TASKBAR_HEIGHT: usize = 44;
const TASKBAR_TOP: usize = HEIGHT - TASKBAR_HEIGHT;
const BUTTON_TOP: usize = TASKBAR_TOP + 6;
const BUTTON_HEIGHT: usize = 32;
const START_LEFT: usize = 6;
const START_WIDTH: usize = 150;
const TRAY_WIDTH: usize = 140;
const TRAY_LEFT: usize = WIDTH - 6 - TRAY_WIDTH;

/// "HH:MM" at text scale 3, centred in the tray.
const CLOCK_TEXT_WIDTH: usize = 5 * 6 * 3;
const CLOCK_LEFT: usize = TRAY_LEFT + (TRAY_WIDTH - CLOCK_TEXT_WIDTH) / 2;
const CLOCK_TOP: usize = BUTTON_TOP + (BUTTON_HEIGHT - 7 * 3) / 2;

const WINDOW_INITIAL_LEFT: usize = 340;
const WINDOW_INITIAL_TOP: usize = 210;
const WINDOW_WIDTH: usize = 600;
const WINDOW_HEIGHT: usize = 250;
const TITLE_HEIGHT: usize = 30;
/// Dragging keeps the whole window on the desktop, above the taskbar.
/// Windows 95 let a window hang off the edges; allowing that here would
/// mean every part of the window drawing having to clip correctly for a
/// case that adds nothing to a mock-up.
const WINDOW_MAX_LEFT: usize = WIDTH - WINDOW_WIDTH;
const WINDOW_MAX_TOP: usize = TASKBAR_TOP - WINDOW_HEIGHT;
/// The "mouse attached / not detected" line's offset inside the window,
/// kept separate from the static body text because it is repainted on its
/// own whenever a mouse is plugged in or pulled out mid-screen.
const STATUS_OFFSET_X: usize = 16;
const STATUS_OFFSET_Y: usize = TITLE_HEIGHT + 118;
/// Height of one text-scale-2 line, which `draw_text` paints as 7 rows per
/// character cell.
const STATUS_LINE_HEIGHT: usize = 7 * 2;

/// The close box, which does not close: the pointer's only meaning so far
/// is dragging, and deciding what "closing" means for a screen whose only
/// exit is a keypress is a separate question. It is still excluded from the
/// title bar's drag area, because dragging a window by its close box is not
/// something the original did either.
const CLOSE_SIZE: usize = TITLE_HEIGHT - 10;

/// How often the clock and the mouse-present line are re-read, in frames
/// at the panel's fixed 57.3 Hz -- about once a second. The clock only
/// shows minutes, so this is far more often than it needs to change; what
/// it buys is that a mouse plugged in while this screen is up is reflected
/// within a second rather than not at all.
const POLL_INTERVAL_FRAMES: u32 = 57;

/// The pointer sprite: `X` outline, `O` white fill, `.` transparent, with
/// the hotspot at the top-left corner exactly as the classic arrow has it.
/// Drawn as a bitmap rather than from primitives because the outline is
/// what makes it legible over both the teal desktop and the grey window,
/// and an outline is easier to be sure of by eye than by geometry.
const CURSOR_WIDTH: usize = 12;
const CURSOR_HEIGHT: usize = 18;
/// Stored flat, one row after another, rather than as an array of row
/// literals: an array of 18 row references is a shape the optimizer
/// unrolls, and 216 unrolled `fill_rect` calls cost more instruction
/// memory than this whole screen is worth on a part with 256 KiB of RAM
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
/// Thickness of the drag outline's edges, and the size of the four backing
/// stores that hold what each one covers.
const OUTLINE_THICKNESS: usize = 3;
const OUTLINE_HORIZONTAL_PIXELS: usize = WINDOW_WIDTH * OUTLINE_THICKNESS;
const OUTLINE_VERTICAL_PIXELS: usize = OUTLINE_THICKNESS * (WINDOW_HEIGHT - 2 * OUTLINE_THICKNESS);

/// Pointer gain, as a fraction applied to the mouse's raw counts: at 1/1 one
/// count moves the pointer one pixel, which is slow going across 1280
/// pixels. Kept as a fraction rather than a whole multiplier so it can be
/// tuned finely -- 3/2 and 5/2 are both reasonable, where 2 and 3 are a big
/// jump apart -- and the leftover is carried in `Cursor` rather than
/// truncated, so slow deliberate movement is not rounded away to nothing.
const POINTER_SPEED_NUMERATOR: i32 = 5;
const POINTER_SPEED_DENOMINATOR: i32 = 2;

/// The panel is 1280x720 on a 5-inch module, so a 12x18 sprite at 1:1 is
/// about 1.5 mm tall. Doubling it puts the pointer at roughly the apparent
/// size it has on a desktop monitor.
const CURSOR_SCALE: usize = 2;
const CURSOR_DRAWN_WIDTH: usize = CURSOR_WIDTH * CURSOR_SCALE;
const CURSOR_DRAWN_HEIGHT: usize = CURSOR_HEIGHT * CURSOR_SCALE;
const CURSOR_SAVED_PIXELS: usize = CURSOR_DRAWN_WIDTH * CURSOR_DRAWN_HEIGHT;

/// Runs the desktop until any managed keyboard key is pressed. As with the
/// other full-screen modes, the framebuffer is left holding the finished
/// screen on return for the caller to draw straight over.
#[inline(never)]
pub fn run(framebuffer: &mut Framebuffer, input: &mut InputManager) {
    if !input.has_mouse() {
        uart::log(b"Win: no USB mouse attached; the pointer will not move\r\n");
    }

    let mut clock = read_clock();
    let mut mouse_present = input.has_mouse();
    let mut window = Window::new();
    draw_desktop(framebuffer, window, clock, mouse_present);

    let mut cursor = Cursor::new(WIDTH / 2, HEIGHT / 2);
    let mut outline = Outline::new();
    let mut drag: Option<Drag> = None;
    cursor.show(framebuffer);
    if !framebuffer.flush() {
        uart::log(b"Win: initial flush failed\r\n");
        return;
    }

    let mut sequence = interrupts::frame_sequence();
    let mut frames_since_poll = 0u32;
    loop {
        if interrupts::dma_error() != 0 {
            uart::log(b"Win: DMA interrupt error\r\n");
            return;
        }
        interrupts::wait_for_interrupt();
        let next_sequence = interrupts::frame_sequence();
        if next_sequence == sequence {
            continue;
        }
        sequence = next_sequence;

        input.service();
        if input.poll_key().is_some() {
            return;
        }

        let motion = input.poll_mouse();
        let (target_x, target_y) = match motion {
            Some(update) => cursor.moved_to(update.dx, update.dy),
            None => (cursor.x, cursor.y),
        };
        let pointer_moved = (target_x, target_y) != (cursor.x, cursor.y);

        // Drag transitions are decided against where the pointer ends up
        // this frame, not where it started: press and motion can arrive in
        // the same report, and grabbing at the old position would offset
        // the window by one frame's travel.
        let mut commit_drag = false;
        if let Some(update) = motion {
            if update.pressed & MOUSE_BUTTON_LEFT != 0
                && drag.is_none()
                && window.title_bar_hit(target_x, target_y)
            {
                drag = Some(Drag {
                    grab_x: target_x - window.x,
                    grab_y: target_y - window.y,
                });
            }
            if update.released & MOUSE_BUTTON_LEFT != 0 && drag.is_some() {
                commit_drag = true;
            }
        }

        frames_since_poll += 1;
        let mut scene_dirty = false;
        if frames_since_poll >= POLL_INTERVAL_FRAMES {
            frames_since_poll = 0;
            let next_clock = read_clock();
            let next_mouse_present = input.has_mouse();
            scene_dirty = next_clock != clock || next_mouse_present != mouse_present;
            clock = next_clock;
            mouse_present = next_mouse_present;
            // A mouse unplugged mid-drag never sends the button release
            // that would end it, which would otherwise leave the outline
            // stuck on screen following a pointer that cannot move.
            if !mouse_present && drag.is_some() {
                drag = None;
                commit_drag = true;
            }
        }

        let outline_target = drag.map(|active| active.window_origin(target_x, target_y));
        let outline_dirty = match outline_target {
            Some(position) => !outline.visible || (outline.x, outline.y) != position,
            None => outline.visible && !commit_drag,
        };
        if !pointer_moved && !scene_dirty && !outline_dirty && !commit_drag {
            continue;
        }

        // Every repaint happens with the pointer lifted off, and the
        // outline lifted off under that. The saved pixels of each are only
        // valid until something else writes into that region, so both have
        // to come off before anything below repaints, or putting them back
        // would stamp a stale copy over the new drawing. The pointer is
        // topmost, so it comes off first and goes back on last.
        let (previous_x, previous_y) = (cursor.x, cursor.y);
        cursor.hide(framebuffer);

        if commit_drag {
            // The window jumps to where the outline was left. Repainting
            // the whole desktop is the simple way to get the vacated
            // background back without every element needing to redraw
            // itself clipped, and it happens once per drag rather than
            // once per frame.
            if let Some(position) = outline_target {
                (window.x, window.y) = position;
            }
            drag = None;
            outline.forget();
            draw_desktop(framebuffer, window, clock, mouse_present);
            cursor.move_to(target_x, target_y);
            cursor.show(framebuffer);
            if !framebuffer.flush() {
                uart::log(b"Win: flush after window move failed\r\n");
                return;
            }
            continue;
        }

        if scene_dirty {
            draw_clock(framebuffer, clock);
            draw_mouse_status(framebuffer, window, mouse_present);
            flush(
                framebuffer,
                TRAY_LEFT,
                BUTTON_TOP,
                TRAY_WIDTH,
                BUTTON_HEIGHT,
            );
            let (status_x, status_y) = window.status_origin();
            flush(
                framebuffer,
                status_x,
                status_y,
                WINDOW_WIDTH - 2 * STATUS_OFFSET_X,
                STATUS_LINE_HEIGHT,
            );
        }

        if outline_dirty {
            let vacated = (outline.x, outline.y);
            let was_visible = outline.visible;
            outline.hide(framebuffer);
            match outline_target {
                Some(position) => {
                    outline.show(framebuffer, position.0, position.1);
                    // One rectangle covering both positions, for the reason
                    // given on `Outline`: the writeback's cost is set by its
                    // logical-X extent, so flushing the four edges
                    // separately would pay that extent four times over for
                    // no benefit.
                    if was_visible {
                        flush_union(framebuffer, vacated, position, WINDOW_WIDTH, WINDOW_HEIGHT);
                    } else {
                        flush(
                            framebuffer,
                            position.0,
                            position.1,
                            WINDOW_WIDTH,
                            WINDOW_HEIGHT,
                        );
                    }
                }
                // The drag ended without a commit. Nothing puts the
                // registry in this state today, but leaving the outline on
                // screen would also leave `outline_dirty` true forever and
                // repaint every frame, so it is cleared rather than
                // ignored.
                None => flush(
                    framebuffer,
                    vacated.0,
                    vacated.1,
                    WINDOW_WIDTH,
                    WINDOW_HEIGHT,
                ),
            }
        }

        cursor.move_to(target_x, target_y);
        cursor.show(framebuffer);
        // One writeback covering both the vacated and the newly covered
        // rectangle. They overlap for anything but a large jump, and a
        // single frame's motion bounds how far apart they can be, so the
        // union stays a small fraction of the screen either way.
        flush_union(
            framebuffer,
            (previous_x, previous_y),
            (cursor.x, cursor.y),
            CURSOR_DRAWN_WIDTH,
            CURSOR_DRAWN_HEIGHT,
        );
    }
}

/// Every rectangle, string and writeback on this screen goes through one
/// of these three non-inlined wrappers.
///
/// `Framebuffer::fill_rect` and friends are meant to be inlined -- that is
/// right for the console's per-cell repaints, which are on the cursor-blink
/// path -- but `fill_rect` alone carries both a PPA and a CPU path, and
/// this module fills around thirty rectangles. Thirty inlined copies cost
/// several kilobytes of a part that has 256 KiB of RAM for code, data and
/// stack together (see DESIGN.md's "RAMの範囲"), which is far more than a
/// mock-up desktop is worth. None of these calls is hot: the screen is
/// painted once and then only the pointer and the clock change.
#[inline(never)]
fn fill(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    color: u16,
) {
    framebuffer.fill_rect(x, y, width, height, color);
}

#[inline(never)]
fn draw_string(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    body: &str,
    scale: usize,
    foreground: u16,
    background: Option<u16>,
) {
    framebuffer.draw_text(x, y, body, scale, foreground, background);
}

#[inline(never)]
fn flush(framebuffer: &Framebuffer, x: usize, y: usize, width: usize, height: usize) {
    framebuffer.flush_rect(x, y, width, height);
}

/// Where the `Welcome` window currently is. Only its origin moves; its size
/// and everything inside it are fixed, so every part is an offset from
/// here.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Window {
    x: usize,
    y: usize,
}

impl Window {
    const fn new() -> Self {
        Self {
            x: WINDOW_INITIAL_LEFT,
            y: WINDOW_INITIAL_TOP,
        }
    }

    /// True if `(x, y)` is on the title bar and not on the close box --
    /// that is, on the part a drag can start from.
    fn title_bar_hit(&self, x: usize, y: usize) -> bool {
        let on_title = x >= self.x + 4
            && x < self.x + WINDOW_WIDTH - 4
            && y >= self.y + 4
            && y < self.y + TITLE_HEIGHT;
        let close_left = self.x + WINDOW_WIDTH - 8 - CLOSE_SIZE;
        let on_close = x >= close_left
            && x < close_left + CLOSE_SIZE
            && y >= self.y + 7
            && y < self.y + 7 + CLOSE_SIZE;
        on_title && !on_close
    }

    fn status_origin(&self) -> (usize, usize) {
        (self.x + STATUS_OFFSET_X, self.y + STATUS_OFFSET_Y)
    }
}

/// A title-bar drag in progress: where inside the title bar it was grabbed,
/// so the window keeps the same relationship to the pointer however far it
/// travels.
#[derive(Clone, Copy)]
struct Drag {
    grab_x: usize,
    grab_y: usize,
}

impl Drag {
    /// Where the window origin would land for a pointer at `(x, y)`,
    /// clamped to the desktop.
    fn window_origin(&self, x: usize, y: usize) -> (usize, usize) {
        (
            x.saturating_sub(self.grab_x).min(WINDOW_MAX_LEFT),
            y.saturating_sub(self.grab_y).min(WINDOW_MAX_TOP),
        )
    }
}

/// The dragging outline: a dithered rectangle the size of the window, drawn
/// over whatever is underneath and taken back off the same way `Cursor` is.
///
/// Windows 95 dragged an outline rather than the window because redrawing
/// the window was too expensive; on this hardware the reason is different
/// but points the same way. Repainting the window itself every frame means
/// re-filling 600x250 pixels and redrawing its text, and also repainting
/// whatever background the window uncovered -- which would need every
/// element on the desktop to be redrawable clipped to an arbitrary
/// rectangle, exactly the problem `Cursor`'s save-and-restore exists to
/// avoid. The outline is about 3,400 pixels and needs none of that.
///
/// What it does *not* save is the writeback. `flush_rect`'s cost is set by
/// the logical-X extent, because CW rotation makes logical X stride across
/// native rows: a 600-wide rectangle spans 600 native rows whatever its
/// height, so one drag frame writes back about 860 KiB either way -- just
/// under half of a full-screen flush. That is the real cost of dragging
/// something this wide, and it is why the outline is flushed as one
/// rectangle covering both positions rather than as four separate strips,
/// which would pay that X extent twice.
struct Outline {
    x: usize,
    y: usize,
    /// The four edges' backing stores. Split by edge rather than kept as
    /// one window-sized buffer because the interior is never touched --
    /// a whole 600x250 copy would be 300 KiB.
    top: [u16; OUTLINE_HORIZONTAL_PIXELS],
    bottom: [u16; OUTLINE_HORIZONTAL_PIXELS],
    left: [u16; OUTLINE_VERTICAL_PIXELS],
    right: [u16; OUTLINE_VERTICAL_PIXELS],
    visible: bool,
}

impl Outline {
    fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            top: [0; OUTLINE_HORIZONTAL_PIXELS],
            bottom: [0; OUTLINE_HORIZONTAL_PIXELS],
            left: [0; OUTLINE_VERTICAL_PIXELS],
            right: [0; OUTLINE_VERTICAL_PIXELS],
            visible: false,
        }
    }

    /// The four edge rectangles at a given origin, in the order the backing
    /// stores are declared.
    fn edges(x: usize, y: usize) -> [(usize, usize, usize, usize); 4] {
        let inner_height = WINDOW_HEIGHT - 2 * OUTLINE_THICKNESS;
        [
            (x, y, WINDOW_WIDTH, OUTLINE_THICKNESS),
            (
                x,
                y + WINDOW_HEIGHT - OUTLINE_THICKNESS,
                WINDOW_WIDTH,
                OUTLINE_THICKNESS,
            ),
            (x, y + OUTLINE_THICKNESS, OUTLINE_THICKNESS, inner_height),
            (
                x + WINDOW_WIDTH - OUTLINE_THICKNESS,
                y + OUTLINE_THICKNESS,
                OUTLINE_THICKNESS,
                inner_height,
            ),
        ]
    }

    #[inline(never)]
    fn hide(&mut self, framebuffer: &mut Framebuffer) {
        if !self.visible {
            return;
        }
        let edges = Self::edges(self.x, self.y);
        let saved: [&[u16]; 4] = [&self.top, &self.bottom, &self.left, &self.right];
        for (&(x, y, width, height), pixels) in edges.iter().zip(saved) {
            framebuffer.blit_rgb565(x, y, width, height, pixels);
        }
        self.visible = false;
    }

    #[inline(never)]
    fn show(&mut self, framebuffer: &mut Framebuffer, x: usize, y: usize) {
        if self.visible {
            return;
        }
        self.x = x;
        self.y = y;
        let edges = Self::edges(x, y);
        let mut saved: [&mut [u16]; 4] = [
            &mut self.top,
            &mut self.bottom,
            &mut self.left,
            &mut self.right,
        ];
        for (&(x, y, width, height), pixels) in edges.iter().zip(saved.iter_mut()) {
            framebuffer.read_rect(x, y, width, height, pixels);
            draw_dither_rect(framebuffer, x, y, width, height);
        }
        self.visible = true;
    }

    /// Drops the backing store without restoring it, for the one case where
    /// the caller is about to repaint the whole screen anyway. Restoring
    /// first would be correct but pointless work.
    fn forget(&mut self) {
        self.visible = false;
    }
}

/// The 50% black-and-white dither Windows 95 drew its drag outline in,
/// which reads as grey and stays visible over both the teal desktop and the
/// grey window. The pattern is keyed to absolute coordinates so it does not
/// crawl as the outline moves.
#[inline(never)]
fn draw_dither_rect(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) {
    for row in 0..height {
        for column in 0..width {
            let color = if (x + column + y + row) % 2 == 0 {
                WHITE
            } else {
                BLACK
            };
            framebuffer.draw_pixel(x + column, y + row, color);
        }
    }
}

/// The pointer sprite plus the pixels it is currently covering.
///
/// Save-and-restore is what lets the pointer cross the window, the icons
/// and the taskbar without any of them knowing about it. The alternative --
/// repainting whatever the pointer just left -- would mean every element on
/// screen needing to be redrawable clipped to an arbitrary rectangle, for
/// no gain.
struct Cursor {
    x: usize,
    y: usize,
    /// Pixels underneath, laid out exactly as `Framebuffer::read_rect`
    /// wrote them so `blit_rgb565` puts them back unchanged -- including
    /// when the sprite hangs off the right or bottom edge, which both
    /// clip identically.
    saved: [u16; CURSOR_SAVED_PIXELS],
    visible: bool,
    /// Sub-pixel motion left over from `POINTER_SPEED_DENOMINATOR`, carried
    /// into the next frame. Without this, any frame whose scaled motion
    /// lands below one pixel would be discarded, and a slow drag across the
    /// screen would lose ground on every one of them.
    remainder_x: i32,
    remainder_y: i32,
}

impl Cursor {
    fn new(x: usize, y: usize) -> Self {
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
    /// short of the edge and make the taskbar's right end unreachable.
    ///
    /// Clamping happens after scaling, and the remainder is still carried
    /// even when the clamp discards the movement -- so pushing the pointer
    /// into an edge and coming back does not first have to work off a debt.
    fn moved_to(&mut self, dx: i32, dy: i32) -> (usize, usize) {
        let scaled_x = scale_motion(dx, &mut self.remainder_x);
        let scaled_y = scale_motion(dy, &mut self.remainder_y);
        let x = (self.x as i32 + scaled_x).clamp(0, WIDTH as i32 - 1) as usize;
        let y = (self.y as i32 + scaled_y).clamp(0, HEIGHT as i32 - 1) as usize;
        (x, y)
    }

    fn move_to(&mut self, x: usize, y: usize) {
        debug_assert!(!self.visible);
        self.x = x;
        self.y = y;
    }

    #[inline(never)]
    fn hide(&mut self, framebuffer: &mut Framebuffer) {
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
    fn show(&mut self, framebuffer: &mut Framebuffer) {
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
                b'X' => DARK_SHADOW,
                b'O' => WHITE,
                _ => continue,
            };
            let (column, row) = (index % CURSOR_WIDTH, index / CURSOR_WIDTH);
            fill(
                framebuffer,
                self.x + column * CURSOR_SCALE,
                self.y + row * CURSOR_SCALE,
                CURSOR_SCALE,
                CURSOR_SCALE,
                color,
            );
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

/// Writes back one rectangle covering both positions of a sprite that just
/// moved.
#[inline(never)]
fn flush_union(
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
    flush(framebuffer, left, top, right - left, bottom - top);
}

/// Paints the whole screen: desktop, icons, window, taskbar. Called once on
/// entry; everything after that is incremental.
#[inline(never)]
fn draw_desktop(
    framebuffer: &mut Framebuffer,
    window: Window,
    clock: Option<(u8, u8)>,
    mouse_present: bool,
) {
    framebuffer.fill(DESKTOP);
    draw_computer_icon(framebuffer, 44, 44);
    draw_bin_icon(framebuffer, 44, 176);
    draw_window(framebuffer, window, mouse_present);
    draw_taskbar(framebuffer, clock);
}

/// The Windows 95 raised 3D border: white and black on the outside, face
/// and grey shadow on the inside, over a face-coloured fill. Buttons, the
/// taskbar and window frames are all this same shape at different sizes.
#[inline(never)]
fn draw_raised(framebuffer: &mut Framebuffer, x: usize, y: usize, width: usize, height: usize) {
    fill(framebuffer, x, y, width, height, FACE);
    draw_border(framebuffer, x, y, width, height, HILIGHT, DARK_SHADOW);
    draw_border(
        framebuffer,
        x + 1,
        y + 1,
        width - 2,
        height - 2,
        FACE,
        SHADOW,
    );
}

/// The same border with the light source on the other side, which is what
/// makes a region read as a recess rather than a button: text fields, the
/// tray clock, and status panels.
#[inline(never)]
fn draw_sunken(framebuffer: &mut Framebuffer, x: usize, y: usize, width: usize, height: usize) {
    fill(framebuffer, x, y, width, height, FACE);
    draw_border(framebuffer, x, y, width, height, SHADOW, HILIGHT);
    draw_border(
        framebuffer,
        x + 1,
        y + 1,
        width - 2,
        height - 2,
        DARK_SHADOW,
        FACE,
    );
}

/// One 1-pixel bevel ring: `top_left` along the top and left edges,
/// `bottom_right` along the bottom and right.
#[inline(never)]
fn draw_border(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    top_left: u16,
    bottom_right: u16,
) {
    if width < 2 || height < 2 {
        return;
    }
    fill(framebuffer, x, y, width, 1, top_left);
    fill(framebuffer, x, y, 1, height, top_left);
    fill(framebuffer, x, y + height - 1, width, 1, bottom_right);
    fill(framebuffer, x + width - 1, y, 1, height, bottom_right);
}

#[inline(never)]
fn draw_taskbar(framebuffer: &mut Framebuffer, clock: Option<(u8, u8)>) {
    draw_raised(framebuffer, 0, TASKBAR_TOP, WIDTH, TASKBAR_HEIGHT);
    draw_raised(
        framebuffer,
        START_LEFT,
        BUTTON_TOP,
        START_WIDTH,
        BUTTON_HEIGHT,
    );
    draw_start_flag(framebuffer, START_LEFT + 10, BUTTON_TOP + 6);
    draw_string(
        framebuffer,
        START_LEFT + 46,
        BUTTON_TOP + 6,
        "Start",
        3,
        BLACK,
        None,
    );
    draw_sunken(
        framebuffer,
        TRAY_LEFT,
        BUTTON_TOP,
        TRAY_WIDTH,
        BUTTON_HEIGHT,
    );
    draw_clock(framebuffer, clock);
}

/// The four-pane flag on the Start button, as four solid quarters. The real
/// one is skewed into a wave; at this size a plain 2x2 reads the same and
/// stays crisp.
#[inline(never)]
fn draw_start_flag(framebuffer: &mut Framebuffer, x: usize, y: usize) {
    const PANE: usize = 9;
    const GAP: usize = 2;
    fill(framebuffer, x, y, PANE, PANE, FLAG_RED);
    fill(framebuffer, x + PANE + GAP, y, PANE, PANE, FLAG_GREEN);
    fill(framebuffer, x, y + PANE + GAP, PANE, PANE, FLAG_BLUE);
    fill(
        framebuffer,
        x + PANE + GAP,
        y + PANE + GAP,
        PANE,
        PANE,
        FLAG_YELLOW,
    );
}

/// Repaints just the tray's clock text. Separate from `draw_taskbar`
/// because it is the one part of the taskbar that changes, and repainting
/// the whole bar every minute would also mean re-flushing it.
#[inline(never)]
fn draw_clock(framebuffer: &mut Framebuffer, clock: Option<(u8, u8)>) {
    let mut digits = *b"--:--";
    if let Some((hour, minute)) = clock {
        digits[0] = b'0' + hour / 10;
        digits[1] = b'0' + hour % 10;
        digits[3] = b'0' + minute / 10;
        digits[4] = b'0' + minute % 10;
    }
    let text = core::str::from_utf8(&digits).unwrap_or("--:--");
    draw_string(
        framebuffer,
        CLOCK_LEFT,
        CLOCK_TOP,
        text,
        3,
        BLACK,
        Some(FACE),
    );
}

#[inline(never)]
fn draw_window(framebuffer: &mut Framebuffer, window: Window, mouse_present: bool) {
    let (left, top) = (window.x, window.y);
    draw_raised(framebuffer, left, top, WINDOW_WIDTH, WINDOW_HEIGHT);
    fill(
        framebuffer,
        left + 4,
        top + 4,
        WINDOW_WIDTH - 8,
        TITLE_HEIGHT - 4,
        TITLE_BAR,
    );
    draw_string(framebuffer, left + 12, top + 8, "Welcome", 3, WHITE, None);

    let close_left = left + WINDOW_WIDTH - 8 - CLOSE_SIZE;
    let close_top = top + 7;
    draw_raised(framebuffer, close_left, close_top, CLOSE_SIZE, CLOSE_SIZE);
    draw_string(
        framebuffer,
        close_left + 5,
        close_top + 4,
        "x",
        2,
        BLACK,
        None,
    );

    let body_left = left + 16;
    let mut line_top = top + TITLE_HEIGHT + 20;
    for line in [
        "Windows 95 desktop, drawn for the USB HID",
        "Boot Mouse driver to have something to",
        "move a pointer across.",
    ] {
        draw_string(framebuffer, body_left, line_top, line, 2, BLACK, None);
        line_top += 24;
    }
    draw_mouse_status(framebuffer, window, mouse_present);
    let status_top = top + STATUS_OFFSET_Y;
    draw_string(
        framebuffer,
        body_left,
        status_top + 44,
        "Drag the title bar to move this window.",
        2,
        BLACK,
        None,
    );
    draw_string(
        framebuffer,
        body_left,
        status_top + 68,
        "Press any key to return to the shell.",
        2,
        BLACK,
        None,
    );
}

/// Repaints the one window line that can change while the screen is up: a
/// mouse plugged in after entry is picked up by `InputManager::service` and
/// should be visible without having to leave and come back.
///
/// Drawn with an opaque background so the longer of the two messages is
/// fully covered when it is replaced by the shorter one.
#[inline(never)]
fn draw_mouse_status(framebuffer: &mut Framebuffer, window: Window, mouse_present: bool) {
    let text = if mouse_present {
        "USB mouse: connected     "
    } else {
        "USB mouse: not detected  "
    };
    let (x, y) = window.status_origin();
    draw_string(framebuffer, x, y, text, 2, BLACK, Some(FACE));
}

/// "My Computer": a CRT on a stand, near enough at 48 pixels.
#[inline(never)]
fn draw_computer_icon(framebuffer: &mut Framebuffer, x: usize, y: usize) {
    draw_raised(framebuffer, x + 4, y, 40, 32);
    fill(framebuffer, x + 9, y + 5, 30, 22, ICON_SCREEN);
    draw_border(framebuffer, x + 8, y + 4, 32, 24, SHADOW, HILIGHT);
    draw_raised(framebuffer, x + 12, y + 32, 24, 6);
    draw_raised(framebuffer, x, y + 38, 48, 10);
    draw_icon_label(framebuffer, x, y + 54, "My Computer");
}

/// "Recycle Bin": a tapered bin with a lid and a couple of ribs.
#[inline(never)]
fn draw_bin_icon(framebuffer: &mut Framebuffer, x: usize, y: usize) {
    draw_raised(framebuffer, x + 10, y, 28, 8);
    draw_raised(framebuffer, x + 12, y + 8, 24, 40);
    for offset in [8usize, 16] {
        fill(framebuffer, x + 12 + offset, y + 12, 2, 32, SHADOW);
    }
    draw_icon_label(framebuffer, x, y + 54, "Recycle Bin");
}

/// Desktop icon captions: white on the teal desktop, centred under a
/// 48-pixel icon at text scale 2 (6 pixels per character cell).
#[inline(never)]
fn draw_icon_label(framebuffer: &mut Framebuffer, icon_x: usize, y: usize, text: &str) {
    let width = text.len() * 6 * 2;
    let left = (icon_x + 24).saturating_sub(width / 2);
    draw_string(framebuffer, left, y, text, 2, WHITE, None);
}

/// Reads the wall clock, or `None` if the RTC did not answer with a valid
/// time -- in which case the taskbar shows `--:--` rather than a made-up
/// time.
#[inline(never)]
fn read_clock() -> Option<(u8, u8)> {
    rtc::read_datetime().ok().map(|now| (now.hour, now.minute))
}
