//! Full-screen Windows 95 desktop mock-up entered through the shell's
//! `win` command, and the exercise for mouse and touch pointer input.
//!
//! Its reason for existing is the mouse: a pointer is the only input this
//! project has that needs a *position* rather than an event, so it is also
//! the first thing that needs a sprite drawn over arbitrary content and
//! taken back off again. `Cursor` below is that mechanism, built on
//! `Framebuffer::read_rect`/`blit_rgb565` rather than on redrawing the
//! desktop underneath, so it stays correct over the window and icons and
//! not just over flat background.
//!
//! The window can be dragged by its title bar with its contents showing,
//! the way Windows 95 did it with "show window contents while dragging"
//! turned on: the window itself follows the pointer rather than an outline
//! standing in for it. Each step repaints the window at its new origin and
//! the desktop strip it uncovered -- see `redraw_moved_window` for why that
//! is a redraw rather than a copy.
//!
//! Everything else is decoration with no behaviour behind it: the Start
//! button does not open, the close box does not close, and the desktop
//! icons do nothing. The other live element is the taskbar clock, read from
//! `rtc`. The rest is deliberately TBD.

use crate::framebuffer::{BLACK, Framebuffer, HEIGHT, WHITE, WIDTH};
use crate::input::{InputManager, PrimaryTouch};
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

/// The two desktop icons. Their captions are wider than the 48-pixel icons
/// themselves and so are what set the box a repaint has to test the window
/// against; both are 11 characters at text scale 2, which
/// `draw_icon_label` asserts.
const ICON_LEFT: usize = 44;
const COMPUTER_ICON_TOP: usize = 44;
const BIN_ICON_TOP: usize = 176;
const ICON_WIDTH: usize = 48;
const ICON_LABEL_WIDTH: usize = 11 * 6 * 2;
const ICON_LABEL_OFFSET_Y: usize = 54;
const ICON_BOUNDS_LEFT: usize = ICON_LEFT + ICON_WIDTH / 2 - ICON_LABEL_WIDTH / 2;
const ICON_BOUNDS_HEIGHT: usize = ICON_LABEL_OFFSET_Y + 7 * 2;

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
    if !input.has_mouse() && input.touch_controller_name().is_none() {
        uart::log(b"Win: no USB mouse attached; the pointer will not move\r\n");
    } else if !input.has_mouse() {
        uart::log(b"Win: no USB mouse attached; use the touch panel\r\n");
    }

    let mut clock = read_clock();
    let mut mouse_present = input.has_mouse();
    let mut window = Window::new();
    draw_desktop(framebuffer, window, clock, mouse_present);

    let mut cursor = Cursor::new(WIDTH / 2, HEIGHT / 2);
    let mut drag: Option<Drag> = None;
    // A previous screen may have been left while a finger was down. Its
    // contact must not become a synthetic move in this fresh desktop; the
    // next observed contact starts a new gesture instead.
    input.reset_primary_touch();
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

        let touch = input.poll_primary_touch();
        let motion = input.poll_mouse();
        let (target_x, target_y) = if matches!(drag, Some(active) if active.source == DragSource::Mouse)
        {
            match motion {
                Some(update) => cursor.moved_to(update.dx, update.dy),
                None => (cursor.x, cursor.y),
            }
        } else {
            match touch {
                PrimaryTouch::Pressed(point) | PrimaryTouch::Moved(point) => (point.x, point.y),
                PrimaryTouch::Idle | PrimaryTouch::Released => match motion {
                    Some(update) => cursor.moved_to(update.dx, update.dy),
                    None => (cursor.x, cursor.y),
                },
            }
        };
        let pointer_moved = (target_x, target_y) != (cursor.x, cursor.y);

        // Drag transitions are decided against where the pointer ends up
        // this frame, not where it started: press and motion can arrive in
        // the same report, and grabbing at the old position would offset
        // the window by one frame's travel.
        let mut release_drag = false;
        match touch {
            // The first finger is the only one `InputManager` reports here.
            // A title-bar touch is therefore a left-button press; its later
            // absolute positions move the same cursor used by a USB mouse.
            PrimaryTouch::Pressed(_)
                if drag.is_none() && window.title_bar_hit(target_x, target_y) =>
            {
                drag = Some(Drag::new(DragSource::Touch, window, target_x, target_y));
            }
            PrimaryTouch::Released if matches!(drag, Some(active) if active.source == DragSource::Touch) =>
            {
                release_drag = true;
            }
            _ => {}
        }
        if !matches!(touch, PrimaryTouch::Pressed(_) | PrimaryTouch::Moved(_))
            && let Some(update) = motion
        {
            // Do not let a physical mouse alter or release a touch-owned
            // drag. Touch itself owns that synthetic left button until its
            // selected finger leaves the panel.
            if !matches!(drag, Some(active) if active.source == DragSource::Touch) {
                if update.pressed & MOUSE_BUTTON_LEFT != 0
                    && drag.is_none()
                    && window.title_bar_hit(target_x, target_y)
                {
                    drag = Some(Drag::new(DragSource::Mouse, window, target_x, target_y));
                }
                if update.released & MOUSE_BUTTON_LEFT != 0
                    && matches!(drag, Some(active) if active.source == DragSource::Mouse)
                {
                    release_drag = true;
                }
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
            // that would end it, which would otherwise leave the window
            // following a pointer that cannot move. It stays where it was
            // last drawn.
            if !mouse_present && matches!(drag, Some(active) if active.source == DragSource::Mouse)
            {
                drag = None;
            }
        }

        // Where the drag puts the window this frame, if that is anywhere
        // other than where it already is. Read before the release below
        // clears the drag, so the last position a release carries with it
        // is still applied.
        let moved_to = drag.and_then(|active| {
            let position = active.window_origin(target_x, target_y);
            (position != (window.x, window.y)).then_some(position)
        });
        if release_drag {
            drag = None;
        }
        if !pointer_moved && !scene_dirty && moved_to.is_none() {
            continue;
        }

        // Every repaint happens with the pointer lifted off. Its saved
        // pixels are only valid until something else writes into that
        // region, so it has to come off before anything below repaints, or
        // putting it back would stamp a stale copy over the new drawing.
        // The pointer is topmost, so it comes off first and goes back on
        // last.
        let (previous_x, previous_y) = (cursor.x, cursor.y);
        cursor.hide(framebuffer);

        if let Some(position) = moved_to {
            let vacated = window;
            (window.x, window.y) = position;
            redraw_moved_window(framebuffer, vacated, window, mouse_present);
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
/// stack together (see docs/BOOT.md's "RAMの範囲"), which is far more than a
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
    source: DragSource,
    grab_x: usize,
    grab_y: usize,
}

/// A drag is owned by the input that started its left-button equivalent. The
/// source is retained so unplugging a mouse cannot terminate an active touch
/// drag, and mouse button reports cannot release a touch-owned drag.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DragSource {
    Mouse,
    Touch,
}

impl Drag {
    fn new(source: DragSource, window: Window, x: usize, y: usize) -> Self {
        Self {
            source,
            grab_x: x - window.x,
            grab_y: y - window.y,
        }
    }

    /// Where the window origin would land for a pointer at `(x, y)`,
    /// clamped to the desktop.
    fn window_origin(&self, x: usize, y: usize) -> (usize, usize) {
        (
            x.saturating_sub(self.grab_x).min(WINDOW_MAX_LEFT),
            y.saturating_sub(self.grab_y).min(WINDOW_MAX_TOP),
        )
    }
}

/// A logical rectangle, as `(x, y, width, height)`.
type Rect = (usize, usize, usize, usize);

/// Redraws the window at its new origin and repaints the desktop its old
/// one uncovered.
///
/// Nothing is copied. `dma2d::copy_rgb565` could move the window's 300 KiB
/// of pixels in a single transfer, but it only orders its reads safely when
/// the destination lies at a lower address than the source, and a window is
/// dragged in every direction; half of them would overwrite source pixels
/// the engine had not reached yet. So the window is drawn again from its
/// primitives, all of them at a fixed origin -- which is what keeps every
/// element on this screen free of having to clip itself. The face and the
/// title bar are large enough to take `fill_rect`'s PPA path; only the
/// border and the text are CPU work.
///
/// What the uncovered desktop needs in return is the one thing the outline
/// this replaced was avoiding: something that can repaint bare background
/// under an arbitrary rectangle. `draw_desktop_patch` is that, and it is
/// only tractable because the desktop underneath is teal plus two icons.
///
/// The writeback is one rectangle covering both positions rather than two.
/// Its cost is set by the logical-X extent, because CW rotation makes
/// logical X stride across native rows: a 600-wide rectangle spans 600
/// native rows whatever its height, about 860 KiB. The two positions
/// overlap for anything but a large jump, so writing them back separately
/// would pay most of that extent twice.
///
/// A drag frame can still be caught half-drawn. The PPA writes to PSRAM
/// directly while the CPU's pixels wait in the cache for the writeback
/// below, so the face reaches the panel before the text over it does, and
/// with one framebuffer there is no way to make a repaint atomic.
#[inline(never)]
fn redraw_moved_window(
    framebuffer: &mut Framebuffer,
    vacated: Window,
    window: Window,
    mouse_present: bool,
) {
    for strip in uncovered_strips(vacated, window).into_iter().flatten() {
        draw_desktop_patch(framebuffer, strip);
    }
    draw_window(framebuffer, window, mouse_present);
    flush_union(
        framebuffer,
        (vacated.x, vacated.y),
        (window.x, window.y),
        WINDOW_WIDTH,
        WINDOW_HEIGHT,
    );
}

/// The parts of `vacated`'s rectangle that the window at `window` no longer
/// covers.
///
/// Both rectangles are the same size, so what is left behind is at most an
/// L: one strip the width of the horizontal travel, one the height of the
/// vertical travel. The second is cut back to the columns the first did not
/// already take, so no pixel is filled twice.
fn uncovered_strips(vacated: Window, window: Window) -> [Option<Rect>; 2] {
    let (old_x, old_y) = (vacated.x, vacated.y);
    let (new_x, new_y) = (window.x, window.y);
    let travel_x = old_x.abs_diff(new_x);
    let travel_y = old_y.abs_diff(new_y);
    if travel_x >= WINDOW_WIDTH || travel_y >= WINDOW_HEIGHT {
        // A jump clear of the old position, which a fast mouse or a touch
        // landing far from the last one can produce: none of it survives.
        return [Some((old_x, old_y, WINDOW_WIDTH, WINDOW_HEIGHT)), None];
    }
    let vertical = if new_x > old_x {
        Some((old_x, old_y, travel_x, WINDOW_HEIGHT))
    } else if new_x < old_x {
        Some((new_x + WINDOW_WIDTH, old_y, travel_x, WINDOW_HEIGHT))
    } else {
        None
    };
    let shared_left = old_x.max(new_x);
    let shared_width = WINDOW_WIDTH - travel_x;
    let horizontal = if new_y > old_y {
        Some((shared_left, old_y, shared_width, travel_y))
    } else if new_y < old_y {
        Some((shared_left, new_y + WINDOW_HEIGHT, shared_width, travel_y))
    } else {
        None
    };
    [vertical, horizontal]
}

/// Repaints one rectangle of bare desktop: the teal background, and any
/// icon that reaches into it.
///
/// The icon is redrawn whole rather than clipped to `rect`. The parts of it
/// that fall outside only get painted with what they already held, except
/// where the window is about to cover them -- so this has to run before
/// `draw_window`, never after it.
///
/// Those outside parts are also why the icon is written back here instead
/// of being left to the caller's rectangle: a caption reaches further left
/// than any strip the window vacates, and pixels the CPU has drawn but not
/// written back would otherwise sit in the cache until an eviction put them
/// on screen at a time of the cache's choosing.
#[inline(never)]
fn draw_desktop_patch(framebuffer: &mut Framebuffer, rect: Rect) {
    let (x, y, width, height) = rect;
    fill(framebuffer, x, y, width, height, DESKTOP);
    if overlaps(rect, icon_bounds(COMPUTER_ICON_TOP)) {
        draw_computer_icon(framebuffer, ICON_LEFT, COMPUTER_ICON_TOP);
        flush_icon(framebuffer, COMPUTER_ICON_TOP);
    }
    if overlaps(rect, icon_bounds(BIN_ICON_TOP)) {
        draw_bin_icon(framebuffer, ICON_LEFT, BIN_ICON_TOP);
        flush_icon(framebuffer, BIN_ICON_TOP);
    }
}

/// The box an icon and its caption together occupy, for an icon drawn at
/// `top`.
fn icon_bounds(top: usize) -> Rect {
    (ICON_BOUNDS_LEFT, top, ICON_LABEL_WIDTH, ICON_BOUNDS_HEIGHT)
}

#[inline(never)]
fn flush_icon(framebuffer: &Framebuffer, top: usize) {
    let (x, y, width, height) = icon_bounds(top);
    flush(framebuffer, x, y, width, height);
}

fn overlaps(first: Rect, second: Rect) -> bool {
    first.0 < second.0 + second.2
        && second.0 < first.0 + first.2
        && first.1 < second.1 + second.3
        && second.1 < first.1 + first.3
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

/// Writes back one rectangle covering both positions of something that just
/// moved -- the pointer sprite, or the window.
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
    draw_computer_icon(framebuffer, ICON_LEFT, COMPUTER_ICON_TOP);
    draw_bin_icon(framebuffer, ICON_LEFT, BIN_ICON_TOP);
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
    draw_icon_label(framebuffer, x, y + ICON_LABEL_OFFSET_Y, "My Computer");
}

/// "Recycle Bin": a tapered bin with a lid and a couple of ribs.
#[inline(never)]
fn draw_bin_icon(framebuffer: &mut Framebuffer, x: usize, y: usize) {
    draw_raised(framebuffer, x + 10, y, 28, 8);
    draw_raised(framebuffer, x + 12, y + 8, 24, 40);
    for offset in [8usize, 16] {
        fill(framebuffer, x + 12 + offset, y + 12, 2, 32, SHADOW);
    }
    draw_icon_label(framebuffer, x, y + ICON_LABEL_OFFSET_Y, "Recycle Bin");
}

/// Desktop icon captions: white on the teal desktop, centred under a
/// 48-pixel icon at text scale 2 (6 pixels per character cell).
#[inline(never)]
fn draw_icon_label(framebuffer: &mut Framebuffer, icon_x: usize, y: usize, text: &str) {
    let width = text.len() * 6 * 2;
    // `ICON_LABEL_WIDTH` is the box `draw_desktop_patch` tests the window
    // against, and a caption wider than it would be left unrepainted.
    debug_assert!(width <= ICON_LABEL_WIDTH);
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
