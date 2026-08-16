//! Foreground application loop for the framebuffer console.
//!
//! The display supplies frame boundaries; this module owns input sources,
//! command dispatch, and application-mode transitions.

use crate::console::{Console, Update};
use crate::axis_test;
use crate::delay::delay_ms;
use crate::framebuffer::DoubleBuffer;
use crate::input::InputManager;
use crate::lcd::Display;
use crate::paint;
use crate::psram::Psram;
use crate::shell;
use crate::touch_test;
use crate::uart;

const BLINK_INTERVAL_FRAMES: u32 = 30;
const BOOT_VERSION: &str = "Tab5 Shell 0.1";

/// Runs the unified keyboard-input console over the DMA display.
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
    let mut input = InputManager::new();
    let mut blink_frames = 0u32;
    loop {
        let Some(displayed) = display.wait_for_frame() else {
            return;
        };
        let mut framebuffers = display.framebuffers_mut();

        input.service();

        if let Some(event) = input.poll_key() {
            blink_frames = 0;
            // Capture the cursor's cell before this byte can move it, so it
            // can be re-rendered below as plain (non-cursor) content: that
            // is the only way its block is ever erased, since backspace,
            // newline and scroll updates above only ever touch the *new*
            // cursor cell, not the one just vacated.
            let previous_cursor = console.cursor();
            let update = console.push_key(event.key);
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
                let outcome = shell::execute(console, submission.as_bytes(), input.usb_host_mut());
                if outcome == shell::Outcome::Paint {
                    // Blocks until a key is pressed, leaving both
                    // framebuffers holding the finished drawing; clear them
                    // back to a fresh console before the redraw below.
                    paint::run(&mut framebuffers, &mut input);
                    console.clear();
                }
                if outcome == shell::Outcome::TouchTest {
                    touch_test::run(&mut framebuffers, &mut input);
                    console.clear();
                }
                if outcome == shell::Outcome::AxisTest {
                    axis_test::run(&mut framebuffers, &mut input);
                    console.clear();
                }
                if outcome != shell::Outcome::Reboot && outcome != shell::Outcome::Shutdown {
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
                if outcome == shell::Outcome::Shutdown {
                    // As with reboot, give the panel one scan-out interval to
                    // show the acknowledgement before the power controller
                    // is asked to remove the device rail.
                    delay_ms(300);
                    if !shell::shutdown() {
                        console.write_output_line("shutdown request failed; device is still running");
                        console.write_prompt();
                        for index in [displayed ^ 1, displayed] {
                            console.render(&mut framebuffers, index);
                            if !framebuffers.flush(index) {
                                uart::log(b"Console: flush failed\r\n");
                                break;
                            }
                        }
                    }
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
