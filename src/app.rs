//! Foreground application loop for the framebuffer console.
//!
//! The display supplies frame boundaries; this module owns input sources,
//! command dispatch, and application-mode transitions.
//!
//! Everything below it exists only to serve a shell command: `shell` itself
//! dispatches them, `membench`, `mbr` and `lsusb` are the ones whose output
//! is long enough to deserve their own file, and the rest are the full-screen modes
//! `run` hands the framebuffer to. None of them is reachable from the
//! hardware-facing modules at the crate root, which is what keeps that
//! dependency pointing one way.

mod automount;
mod axis_test;
mod battery;
mod blockdev;
mod browser;
mod browsertest;
mod coord_test;
mod fetch;
mod files;
mod font_test;
mod localfile;
mod lsusb;
mod mbr;
mod membench;
mod paint;
mod pointer;
mod shell;
mod startup_screen;
mod touch_test;
mod wifi_manager;
mod wifi_menu;
mod wifi_retry;
mod win;

use alloc::vec::Vec;

use crate::delay::delay_ms;
use crate::fs::partition::PartitionRange;
use crate::fs::registry::{DeviceId, Devices};
use crate::fs::vfs::{MountMode, Vfs};
use crate::fs::{self, RamBlockDevice, SdSlot};
use crate::input::InputManager;
use crate::lcd::Display;
use crate::psram::Psram;
use crate::startup::RebootTestBoot;
use crate::{startup, tick, uart};

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
    let psram_frequency_mhz = psram.frequency_mhz();
    let Some(mut display) = Display::new(psram) else {
        return;
    };
    // One foreground hart owns the singleton for the lifetime of the app.
    let console = unsafe { crate::console::singleton() };
    if !startup_screen::draw_initial(display.framebuffer_mut()) {
        uart::log(b"STARTUP: initial screen flush failed\r\n");
        return;
    }
    if !display.start() {
        return;
    }
    // The trap entry and machine interrupts are in place now, which is what
    // the tick's CLIC line needs; `uptime` and the IP stack both read it.
    tick::init();
    let startup_screen_shown_ms = tick::now_ms();
    let mut boot_lines: Vec<shell::Line> = Vec::new();

    match startup::complete_reboot_test_boot(psram_frequency_mhz == 200) {
        RebootTestBoot::Inactive => {}
        RebootTestBoot::Reboot {
            completed,
            remaining,
        } => {
            uart::log_hex(b"REBOOT TEST: completed=", completed);
            uart::log_hex(b"REBOOT TEST: remaining=", remaining);
            // Scanout has started, which is the last boot milestone covered by
            // this diagnostic. Give the UART line time to leave before the
            // next HP-core reset, then quiesce display DMA through the normal
            // reboot path.
            delay_ms(100);
            // No session exists this early, and the reboot test never gets
            // as far as bringing the C6 up.
            shell::reboot(None);
        }
        RebootTestBoot::Complete { total } => {
            uart::log_hex(b"REBOOT TEST: PASS total=", total);
            let mut line = shell::Line::new();
            line.push_str("REBOOT TEST PASS: ");
            line.push_u32(total);
            line.push_str("/");
            line.push_u32(total);
            boot_lines.push(line);
        }
        RebootTestBoot::Failed { completed, total } => {
            uart::log_hex(b"REBOOT TEST: FAIL completed=", completed);
            uart::log_hex(b"REBOOT TEST: expected total=", total);
            uart::log_hex(b"REBOOT TEST: PSRAM MHz=", psram_frequency_mhz);
            let mut line = shell::Line::new();
            line.push_str("REBOOT TEST FAIL after ");
            line.push_u32(completed);
            line.push_str("/");
            line.push_u32(total);
            line.push_str(" (PSRAM fallback)");
            boot_lines.push(line);
        }
    }
    // The RAM disk is formatted here, once, before anything can reach it.
    // Its PSRAM span holds whatever the last boot left behind, and a stale
    // FAT header there would be read as a live filesystem, so the volume is
    // rebuilt every time rather than probed. A failure leaves the device
    // absent: there is no fallback that hands the span to the heap instead,
    // because the fixed layout is what keeps the reservation useful.
    let mut ram_disk = init_ram_disk(&psram, &mut boot_lines);
    // The mount table outlives every command: `/tmp` is attached once here,
    // and whatever the user mounts later stays until they unmount it.
    let mut vfs = Vfs::new();

    // The current directory outlives every command too, for the same reason
    // the mount table does: `cd` is only useful if the next command is still
    // there.
    let mut shell_state = shell::State::new();

    // Volumes appear and disappear with the media they are on unless this is
    // turned off. Its first pass runs after the startup screen's short USB
    // discovery campaign, which mounts a stick that was already plugged in
    // without blocking the first white frame.
    let mut auto_mount = automount::AutoMount::new();

    let mut input = InputManager::new();
    if ram_disk.is_some() {
        mount_ram_disk(
            &mut vfs,
            ram_disk.as_mut(),
            input.usb_host_mut(),
            &mut boot_lines,
        );
    }

    // One owner keeps the C6 link, IP stack and the policy connecting them
    // coherent across shell commands and full-screen modes.
    let mut wifi_manager = wifi_manager::Manager::new();
    let initial_route = startup_screen::run(
        &mut display,
        &mut input,
        &mut auto_mount,
        &mut vfs,
        ram_disk.as_mut(),
        &mut wifi_manager,
        startup_screen_shown_ms,
    );
    match initial_route {
        startup_screen::InitialRoute::Browser => {
            browser::run(
                display.framebuffer_mut(),
                &mut input,
                &mut wifi_manager,
                &mut vfs,
                ram_disk.as_mut(),
                None,
            );
            shell::drop_dead_session(console, display.framebuffer_mut(), &mut wifi_manager);
        }
        startup_screen::InitialRoute::WifiMenu => {
            let outcome = wifi_menu::run(
                display.framebuffer_mut(),
                &mut input,
                &mut wifi_manager,
                wifi_menu::Entry::Startup,
            );
            if outcome == wifi_menu::Outcome::Online {
                browser::run(
                    display.framebuffer_mut(),
                    &mut input,
                    &mut wifi_manager,
                    &mut vfs,
                    ram_disk.as_mut(),
                    None,
                );
            }
            shell::drop_dead_session(console, display.framebuffer_mut(), &mut wifi_manager);
        }
        startup_screen::InitialRoute::Console => {}
    }

    console.clear(display.framebuffer_mut());
    console.write_output_line(display.framebuffer_mut(), BOOT_VERSION);
    for line in &boot_lines {
        console.write_output_line(display.framebuffer_mut(), line.as_str());
    }
    console.write_prompt(display.framebuffer_mut());
    let mut blink_frames = 0u32;
    loop {
        if display
            .wait_for_frame_with(|| input.service_fast())
            .is_none()
        {
            return;
        }

        let framebuffer = display.framebuffer_mut();

        input.service();
        // Reconciling the mount table sits here, beside command dispatch,
        // rather than inside `input.service`: opening a volume is bus I/O and
        // FAT parsing, and the input servicing above is what the console's
        // redraw is waiting on. Nothing happens at all unless the bus moved.
        auto_mount.service(
            console,
            framebuffer,
            &mut vfs,
            ram_disk.as_mut(),
            input.usb_host_mut(),
        );
        // Frames the C6 has received are held there until the host reads
        // them, and a backlog larger than the transport's staging buffer
        // cannot be resynchronized -- so the link is serviced every frame,
        // not only while a network command is running.
        wifi_manager.service();

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
            ram_disk.as_mut(),
            &mut vfs,
            &mut shell_state,
            &mut auto_mount,
            &mut wifi_manager,
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
            shell::Outcome::FontTest => {
                font_test::run(framebuffer, &mut input);
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
            shell::Outcome::WifiMenu => {
                let _ = wifi_menu::run(
                    framebuffer,
                    &mut input,
                    &mut wifi_manager,
                    wifi_menu::Entry::Shell,
                );
                console.clear(framebuffer);
                shell::drop_dead_session(console, framebuffer, &mut wifi_manager);
            }
            shell::Outcome::Browser(start) => {
                // The viewer keeps borrowing the manager for one fetch step
                // at a time and services it every frame, because the loop
                // above is paused while the full-screen mode is running.
                browser::run(
                    framebuffer,
                    &mut input,
                    &mut wifi_manager,
                    &mut vfs,
                    ram_disk.as_mut(),
                    start,
                );
                console.clear(framebuffer);
                // A disconnection while the viewer was up is otherwise
                // invisible: the same check every network command makes.
                shell::drop_dead_session(console, framebuffer, &mut wifi_manager);
            }
            shell::Outcome::VisualQa => {
                run_visual_qa(console, framebuffer, &mut input);
            }
            shell::Outcome::Continue => {}
            shell::Outcome::Reboot => {
                // The "rebooting..." line is already in PSRAM; give the panel
                // one scan-out interval to actually show it before the reset.
                delay_ms(300);
                // An HP-core reset does not physically clear all L2 RAM.
                // Scrub the retained menu credential before the reboot path
                // borrows the still-live session for its best-effort deauth.
                wifi_manager.erase_credentials();
                shell::reboot(wifi_manager.session_mut());
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

/// Claims the PSRAM RAM disk span and puts a fresh FAT16 volume on it.
///
/// Failures are reported on the console as well as UART because whether
/// `/tmp` exists changes what the shell can do. Success stays silent; RAM
/// disk capacity remains available through the explicit memory diagnostics.
fn init_ram_disk(psram: &Psram, boot_lines: &mut Vec<shell::Line>) -> Option<RamBlockDevice> {
    let mut device = RamBlockDevice::claim(psram)?;
    match fs::format::fat16(&mut device) {
        Ok(layout) => {
            fs::format::log_layout(&layout);
            // Without content the volume would mount and list nothing, which
            // looks identical to a reader that cannot see entries.
            if let Err(error) = fs::seed::test_files(&mut device, &layout) {
                let mut line = shell::Line::new();
                line.push_str("ram disk seed failed: ");
                line.push_str(fs::seed::error_name(error));
                uart::log(line.as_str().as_bytes());
                uart::log(b"\r\n");
                boot_lines.push(line);
                return None;
            }
            Some(device)
        }
        Err(error) => {
            let mut line = shell::Line::new();
            line.push_str("ram disk format failed: ");
            line.push_str(fs::format::error_name(error));
            uart::log(line.as_str().as_bytes());
            uart::log(b"\r\n");
            boot_lines.push(line);
            None
        }
    }
}

/// Attaches the RAM disk at `/tmp`.
///
/// The mount is explicit even though nothing else could be at `/tmp`: the
/// VFS has no auto-mount path at all, so that a volume appearing in the tree
/// always corresponds to something that asked for it.
///
/// The RAM disk is the one read-write mount. Its `MountMode` says so, but
/// what actually keeps writes off the other media is their block adapters
/// refusing them, so this is a statement of policy rather than the guard.
fn mount_ram_disk(
    vfs: &mut Vfs,
    ram_disk: Option<&mut RamBlockDevice>,
    usb_host: &mut crate::usb::UsbHost,
    boot_lines: &mut Vec<shell::Line>,
) {
    let mut sd = SdSlot::new();
    let mut devices = Devices {
        ram: ram_disk,
        sd: &mut sd,
        usb: usb_host,
    };
    // The whole device, with no partition table in front of it: the RAM disk
    // is reached by its own mount point rather than by an entry number.
    let range = PartitionRange {
        start_lba: 0,
        block_count: devices
            .with_device(DeviceId::Ram, |device| device.geometry().block_count)
            .unwrap_or(0),
    };
    // No partition: the RAM disk is reached by its own mount point rather
    // than through an MBR entry.
    let outcome = vfs.mount(
        &mut devices,
        "/tmp",
        DeviceId::Ram,
        None,
        range,
        MountMode::ReadWrite,
    );
    if let Err(error) = outcome {
        let mut line = shell::Line::new();
        line.push_str("mounting /tmp failed: ");
        line.push_str(fs::vfs::error_name(error));
        uart::log(line.as_str().as_bytes());
        uart::log(b"\r\n");
        boot_lines.push(line);
    }
}

/// Visits every display-sensitive full-screen mode from one short command.
/// `dp 100` already supplies the 100 full-frame transition equivalent; this
/// sequence checks the actual pixels, sensors and pointer interactions once,
/// while counting each mode's initial draw and return-to-console transition.
fn run_visual_qa(
    console: &mut crate::console::Console,
    framebuffer: &mut crate::framebuffer::Framebuffer,
    input: &mut InputManager,
) {
    let _ = crate::lcd::take_underrun();
    let initial_underruns = crate::lcd::underrun_count();
    let mut previous_underruns = initial_underruns;

    uart::log(b"UI visual: coordinate chart; any key advances\r\n");
    coord_test::run(framebuffer, input);
    console.clear(framebuffer);
    previous_underruns = finish_visual_stage(b"coordinate", previous_underruns);

    uart::log(b"UI visual: font sheet; any key advances\r\n");
    font_test::run(framebuffer, input);
    console.clear(framebuffer);
    previous_underruns = finish_visual_stage(b"font", previous_underruns);

    uart::log(b"UI visual: paint; draw, then any key advances\r\n");
    paint::run(framebuffer, input);
    console.clear(framebuffer);
    previous_underruns = finish_visual_stage(b"paint", previous_underruns);

    uart::log(b"UI visual: touch; use two fingers, then any key advances\r\n");
    touch_test::run(framebuffer, input);
    console.clear(framebuffer);
    previous_underruns = finish_visual_stage(b"touch", previous_underruns);

    uart::log(b"UI visual: axis; tilt, then any key advances\r\n");
    axis_test::run(framebuffer, input);
    console.clear(framebuffer);
    previous_underruns = finish_visual_stage(b"axis", previous_underruns);

    uart::log(b"UI visual: desktop; move/drag, then any key finishes\r\n");
    win::run(framebuffer, input);
    console.clear(framebuffer);
    let final_underruns = finish_visual_stage(b"desktop", previous_underruns);

    let mut line = shell::Line::new();
    line.push_str("ui visual: underruns=");
    line.push_u32(final_underruns.wrapping_sub(initial_underruns));
    line.push_str(" dma_error=0x");
    line.push_hex(crate::interrupts::dma_error(), 8);
    console.write_output_line(framebuffer, line.as_str());
}

fn finish_visual_stage(name: &[u8], before: u32) -> u32 {
    // The return-to-console clear may finish late in the current scan; wait
    // beyond one 57.3 Hz frame before consuming its sticky indication.
    delay_ms(20);
    let _ = crate::lcd::take_underrun();
    let after = crate::lcd::underrun_count();
    uart::log(b"UI visual: ");
    uart::log(name);
    uart::log_hex(b" underruns=", after.wrapping_sub(before));
    after
}
