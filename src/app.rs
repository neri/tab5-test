//! Foreground application loop for the framebuffer console.
//!
//! The display supplies frame boundaries; this module owns input sources,
//! command dispatch, and application-mode transitions.
//!
//! Everything below it exists only to serve a shell command: `shell` itself
//! dispatches them, `membench` and `mbr` are the two whose output is long
//! enough to deserve their own file, and the rest are the full-screen modes
//! `run` hands the framebuffer to. None of them is reachable from the
//! hardware-facing modules at the crate root, which is what keeps that
//! dependency pointing one way.

mod axis_test;
mod battery;
mod coord_test;
mod mbr;
mod membench;
mod paint;
mod shell;
mod touch_test;
mod win;

use crate::delay::delay_ms;
use crate::input::InputManager;
use crate::lcd::Display;
use crate::psram::Psram;

/// Roughly half a second of cursor blink at the panel's fixed 57.3 Hz.
const BLINK_INTERVAL_FRAMES: u32 = 30;

const BOOT_VERSION: &str = "Tab5 Shell 0.1";

/// Runs the unified keyboard-input console over the DMA display.
///
/// Scanout is single-buffered, so every write lands in the frame being read
/// out. That is deliberate: the second buffer doubled the PSRAM traffic of
/// every update, which is precisely what starves the DSI bridge's read stream
/// and flashes the panel light blue, and it bought nothing here because the
/// incremental paths always had to write both sides to stay coherent.
///
/// Drawing is the console's own business: every call that changes its cells
/// paints them and writes them back before returning. This loop therefore owns
/// only input, command dispatch, and the transitions into and out of the
/// full-screen modes -- each of which hands the framebuffer to another module
/// and then has `Console::clear` put the console back over its drawing.
pub fn run(psram: Psram) {
    let Some(mut display) = Display::new(psram) else {
        return;
    };
    // One foreground hart owns the singleton for the lifetime of the app.
    let console = unsafe { crate::console::singleton() };
    {
        let framebuffer = display.framebuffer_mut();
        console.clear(framebuffer);
        console.write_output_line(framebuffer, BOOT_VERSION);
        console.write_prompt(framebuffer);
    }
    if !display.start() {
        return;
    }
    let mut input = InputManager::new();
    let mut blink_frames = 0u32;
    loop {
        if display.wait_for_frame().is_none() {
            return;
        }

        let framebuffer = display.framebuffer_mut();

        input.service();

        let Some(event) = input.poll_key() else {
            // No key this frame: advance the idle blink timer and, on phase
            // change, repaint only the cursor's own cell.
            blink_frames += 1;
            if blink_frames >= BLINK_INTERVAL_FRAMES {
                blink_frames = 0;
                console.blink_cursor(framebuffer);
            }
            continue;
        };

        blink_frames = 0;
        console.push_key(framebuffer, event.key);

        // Enter completing a command line is an application-level reaction
        // rather than part of the echo, so it is handled here instead of
        // inside the console.
        let Some(submission) = console.take_submission() else {
            continue;
        };
        let outcome = shell::execute(
            console,
            framebuffer,
            submission.as_bytes(),
            input.usb_host_mut(),
        );
        match outcome {
            // Each of these blocks until a key is pressed and leaves its own
            // drawing in the framebuffer; `clear` repaints the console over it.
            shell::Outcome::Paint => {
                paint::run(framebuffer, &mut input);
                console.clear(framebuffer);
            }
            shell::Outcome::TouchTest => {
                touch_test::run(framebuffer, &mut input);
                console.clear(framebuffer);
            }
            shell::Outcome::CoordTest => {
                coord_test::run(framebuffer, &mut input);
                console.clear(framebuffer);
            }
            shell::Outcome::AxisTest => {
                axis_test::run(framebuffer, &mut input);
                console.clear(framebuffer);
            }
            shell::Outcome::Battery => {
                battery::run(framebuffer, &mut input);
                console.clear(framebuffer);
            }
            shell::Outcome::Win => {
                win::run(framebuffer, &mut input);
                console.clear(framebuffer);
            }
            shell::Outcome::Continue => {}
            shell::Outcome::Reboot => {
                // The "rebooting..." line is already in PSRAM; give the panel
                // one scan-out interval to actually show it before the reset.
                delay_ms(300);
                shell::reboot();
            }
            shell::Outcome::Shutdown => {
                // As with reboot, let the acknowledgement reach the panel
                // before the power controller removes the device rail.
                delay_ms(300);
                if !shell::shutdown() {
                    console.write_output_line(
                        framebuffer,
                        "shutdown request failed; device is still running",
                    );
                    console.write_prompt(framebuffer);
                }
                continue;
            }
        }
        console.write_prompt(framebuffer);
    }
}
