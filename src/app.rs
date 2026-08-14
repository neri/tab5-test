//! Foreground application loop for the framebuffer console.
//!
//! The display supplies frame boundaries; this module owns input sources,
//! command dispatch, and application-mode transitions.

use crate::cardkb::CardKb;
use crate::console::{Console, Update};
use crate::delay::delay_ms;
use crate::framebuffer::DoubleBuffer;
use crate::lcd::Display;
use crate::paint;
use crate::psram::Psram;
use crate::shell;
use crate::touch_test;
use crate::uart;
use crate::usb;

const BLINK_INTERVAL_FRAMES: u32 = 30;
const BOOT_VERSION: &str = "Tab5 Shell 0.1";
const HUB_PORT_SCAN_FRAMES: u32 = 60;
const ROOT_RESCAN_FRAMES: u32 = 300;

/// Runs the CardKB/USB input console over the DMA display.
///
/// Each keystroke is rendered into the inactive buffer, cache-synchronised,
/// then selected at the next full-frame DMA completion. This never modifies a
/// buffer while the display engine can read from it.
pub fn run(psram: Psram) {
    let Some(mut display) = Display::new(psram) else {
        return;
    };
    // One foreground hart owns the singleton for the lifetime of the app.
    let console = unsafe { crate::console::singleton() };
    console.clear();
    console.write_output_line(BOOT_VERSION);
    console.write_prompt();
    {
        let framebuffers = display.framebuffers_mut();
        console.render(framebuffers, 0);
        console.render(framebuffers, 1);
        if !framebuffers.flush(0) || !framebuffers.flush(1) {
            uart::log(b"FB: cache sync failed\r\n");
            return;
        }
    }
    if !display.start() {
        return;
    }
    let mut keyboard = CardKb::init();
    if keyboard.is_some() {
        uart::log(b"CardKB: ready\r\n");
    } else {
        uart::log(b"CardKB: absent\r\n");
    }
    let mut usb_host = usb::UsbHost::new();
    usb_host.rescan();
    uart::log(b"USB: initial scan complete\r\n");
    let mut reconnect_frames = 0u32;
    let mut usb_reconnect_frames = 0u32;
    let mut blink_frames = 0u32;
    loop {
        let Some(displayed) = display.wait_for_frame() else {
            return;
        };
        let mut framebuffers = display.framebuffers_mut();

        if keyboard.is_none() {
            reconnect_frames += 1;
            if reconnect_frames == 60 {
                reconnect_frames = 0;
                keyboard = CardKb::init();
                if keyboard.is_some() {
                    uart::log(b"CardKB: connected\r\n");
                }
            }
        }

        // The registry can go stale two different ways, and they call for
        // different urgency:
        // - The cable comes out (`root_disconnected`, a single cheap
        //   register read). Nothing is plugged in at all, so there is no
        //   rush; fall through to the throttled rescan below, same as
        //   never having found anything. `is_empty` guards the log line so
        //   it fires once on the transition, not every frame the cable
        //   stays out.
        // - A keyboard slot's session goes stale (`needs_reinit`, from
        //   `UsbKeyboard::poll`'s error tracking). Now that `UsbHost` is
        //   the only thing that ever resets the bus (`usbinfo`/`usbhub`/
        //   `usbrescan` all read or drive the same registry instead of
        //   probing independently), this should only fire on a genuine
        //   hardware error -- but whatever is plugged in is almost
        //   certainly still there, so this rescans immediately rather than
        //   leaving everything dead for the throttle's full ~5s interval.
        if usb_host.root_disconnected() {
            if !usb_host.is_empty() {
                usb_host.clear();
                uart::log(b"USB: nothing connected to USB-A\r\n");
            }
        } else if usb_host.needs_reinit() {
            uart::log(b"USB: a device session went stale, rescanning...\r\n");
            usb_host.rescan();
            usb_reconnect_frames = 0;
        }
        if usb_host.has_room() {
            usb_reconnect_frames += 1;
            if usb_host.hub().is_some() {
                // With a hub attached, a newly plugged-in device can be
                // found by asking the hub about its own ports -- one
                // `GET_STATUS` control transfer per empty port, no bus
                // reset, and nothing already attached is disturbed. That is
                // cheap enough to run about once a second.
                //
                // This used to be the `rescan` below instead, which was
                // wrong rather than merely slow: resetting the bus
                // invalidates every device address on it, so a timer-driven
                // rescan tore down and re-enumerated *working* devices
                // every few seconds, long enough to drop a keystroke.
                if usb_reconnect_frames >= HUB_PORT_SCAN_FRAMES {
                    usb_reconnect_frames = 0;
                    usb_host.scan_empty_hub_ports();
                }
            } else if usb_reconnect_frames >= ROOT_RESCAN_FRAMES {
                // Nothing but the root port to ask, and the only way to
                // enumerate what is on it is the full sequence. Much
                // coarser than CardKB's 60-frame retry: this is a few
                // hundred ms of blocking work (VBUS settle, connect wait,
                // debounce, reset), not a cheap I2C probe. Nothing is
                // attached in this state, so there is nothing to disturb.
                // (The immediate `needs_reinit` retry above bypasses this
                // for the "still plugged in, just needs re-enumerating"
                // case.)
                usb_reconnect_frames = 0;
                usb_host.rescan();
            }
        }

        let key = keyboard
            .as_mut()
            .and_then(CardKb::poll)
            .or_else(|| usb_host.poll_keyboards());

        if let Some(byte) = key {
            blink_frames = 0;
            // Capture the cursor's cell before this byte can move it, so it
            // can be re-rendered below as plain (non-cursor) content: that
            // is the only way its block is ever erased, since backspace,
            // newline and scroll updates above only ever touch the *new*
            // cursor cell, not the one just vacated.
            let previous_cursor = console.cursor();
            let update = console.push(char::from(byte));
            // Typing always shows a solid cursor; only idle time blinks it.
            console.show_cursor();

            // Enter completing a command line is an application-level
            // reaction, not a rendering hint, so it is handled independently
            // of `update` (which is always `None` for the byte that
            // triggered it -- `Console::push` leaves any actual redraw to
            // this dispatch instead).
            if let Some(submission) = console.take_submission() {
                // A command's output can span an arbitrary number of rows,
                // so (like a scroll) this redraws both sides from scratch
                // rather than tracking an incremental region.
                let outcome = shell::execute(console, submission.as_bytes(), &mut usb_host);
                if outcome == shell::Outcome::Paint {
                    // Blocks until a key is pressed, leaving both
                    // framebuffers holding the finished drawing; clear them
                    // back to a fresh console before the redraw below.
                    paint::run(&mut framebuffers, &mut keyboard);
                    console.clear();
                }
                if outcome == shell::Outcome::TouchTest {
                    touch_test::run(&mut framebuffers, &mut keyboard);
                    console.clear();
                }
                if outcome != shell::Outcome::Reboot {
                    console.write_prompt();
                }
                for index in [displayed ^ 1, displayed] {
                    console.render(&mut framebuffers, index);
                    if !framebuffers.flush(index) {
                        uart::log(b"Console: flush failed\r\n");
                        break;
                    }
                }
                if outcome == shell::Outcome::Reboot {
                    // Let the panel actually scan out the frame just
                    // flushed (the "rebooting..." line) before the
                    // watchdog fires.
                    delay_ms(300);
                    shell::reboot();
                }
                continue;
            }

            match update {
                Update::None => {}
                Update::Cells { row, start, end } => {
                    // Both sides start identical. Update the inactive side
                    // first, then the displayed side. Only the native span
                    // covering this range is written back, so GDMA retains
                    // almost all PSRAM bandwidth and visible tearing is at
                    // most one small run of glyphs.
                    let back_buffer = displayed ^ 1;
                    console.render_cells(&mut framebuffers, back_buffer, row, start, end);
                    if !console.flush_cells(&framebuffers, back_buffer, row, start, end) {
                        uart::log(b"Console: cell flush failed\r\n");
                        continue;
                    }
                    console.render_cells(&mut framebuffers, displayed, row, start, end);
                    if !console.flush_cells(&framebuffers, displayed, row, start, end) {
                        uart::log(b"Console: cell flush failed\r\n");
                    }
                }
                Update::Full => {
                    // Only a bottom-row scroll reaches here; every visible
                    // row moved, so both sides need a complete redraw to
                    // stay coherent for subsequent incremental updates.
                    //
                    // A brief flash of a wrong solid color has been
                    // observed here during scrolling. Buffer selection,
                    // writeback chunking, and pausing/aborting GDMA for the
                    // duration of the write have all been tried and none
                    // change it, so it is not fixed by anything in this
                    // function; see DESIGN.md's known-issues note.
                    for index in [displayed ^ 1, displayed] {
                        console.render(&mut framebuffers, index);
                        if !framebuffers.flush(index) {
                            uart::log(b"Console: flush failed\r\n");
                            break;
                        }
                    }
                }
            }
            // `Update::Full` already redrew every cell, cursor included, so
            // only the incremental path needs this separate cursor pass.
            if !matches!(update, Update::Full) {
                let current_cursor = console.cursor();
                let back_buffer = displayed ^ 1;
                // `continue` must reach the outer frame loop on a back-buffer
                // failure, as it does in the arms above, so a plain
                // `continue` inside this `for` (which would only skip to the
                // next cell) is tracked with a flag instead.
                let mut back_buffer_failed = false;
                for (column, row) in [previous_cursor, current_cursor] {
                    if !redraw_cell(console, &mut framebuffers, back_buffer, column, row) {
                        uart::log(b"Console: cursor flush failed\r\n");
                        back_buffer_failed = true;
                        break;
                    }
                    if !redraw_cell(console, &mut framebuffers, displayed, column, row) {
                        uart::log(b"Console: cursor flush failed\r\n");
                    }
                }
                if back_buffer_failed {
                    continue;
                }
            }
        } else {
            // No key this frame: advance the idle blink timer and, on
            // phase change, repaint only the cursor's own cell.
            blink_frames += 1;
            if blink_frames >= BLINK_INTERVAL_FRAMES {
                blink_frames = 0;
                console.toggle_cursor();
                let (column, row) = console.cursor();
                let back_buffer = displayed ^ 1;
                if !redraw_cell(console, &mut framebuffers, back_buffer, column, row) {
                    uart::log(b"Console: cursor flush failed\r\n");
                    continue;
                }
                if !redraw_cell(console, &mut framebuffers, displayed, column, row) {
                    uart::log(b"Console: cursor flush failed\r\n");
                }
            }
        }
    }
}

/// Repaints one console cell in one framebuffer and writes it back,
/// reporting whether the writeback succeeded.
fn redraw_cell(
    console: &Console,
    framebuffers: &mut DoubleBuffer,
    index: usize,
    column: usize,
    row: usize,
) -> bool {
    console.render_cell(framebuffers, index, column, row);
    console.flush_cell(framebuffers, index, column, row)
}
