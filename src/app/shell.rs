//! Command dispatcher for the keyboard-input console.
//!
//! Every command reads or writes only through `Console`'s output-line API,
//! so results share the same wrapping/scrolling/rendering as the rest of
//! the console -- and, since that API mirrors each line to the UART log,
//! every command's output is also readable over serial without having to
//! transcribe it off the panel. `execute` runs once per `Enter` keypress
//! and returns whether the caller should reboot the board once the current
//! frame has actually reached the panel.

use alloc::string::String;
use alloc::vec::Vec;

use super::{mbr, membench};
use crate::console::Console;
use crate::framebuffer::Framebuffer;
use crate::{icm, interrupts, lcd, power, psram, rtc, sdmmc, startup, uart, usb};

/// Roughly the panel's vsync rate; used only for the coarse `uptime` command.
/// Fixed, because the panel only tolerates the one set of vertical timings
/// (see `lcd`'s `VFP_LINES`).
const FRAMES_PER_SECOND: u32 = 57;

/// One `help` entry: the bare command name (used both to look commands up
/// and to list them), its usage line, and one or more description lines
/// shown by `help <name>`.
struct HelpEntry {
    name: &'static str,
    usage: &'static str,
    lines: &'static [&'static str],
}

const HELP_ENTRIES: &[HelpEntry] = &[
    HelpEntry {
        name: "help",
        usage: "help [command]",
        lines: &["list commands, or describe one"],
    },
    HelpEntry {
        name: "clear",
        usage: "clear",
        lines: &["clear the screen"],
    },
    HelpEntry {
        name: "echo",
        usage: "echo <text>",
        lines: &["print text back"],
    },
    HelpEntry {
        name: "about",
        usage: "about",
        lines: &["firmware banner"],
    },
    HelpEntry {
        name: "cpuinfo",
        usage: "cpuinfo",
        lines: &["show RISC-V machine identification CSRs"],
    },
    HelpEntry {
        name: "mem",
        usage: "mem",
        lines: &["PSRAM/RAM usage"],
    },
    HelpEntry {
        name: "alloctest",
        usage: "alloctest <MiB>",
        lines: &["allocate N MiB on the PSRAM heap, verify read/write"],
    },
    HelpEntry {
        name: "stress",
        usage: "stress [count]",
        lines: &[
            "repeat a full-screen fill and report how long it took and how",
            "many DPI FIFO underruns it caused. a fixed, countable workload,",
            "so settings can be compared: run it, change one thing (say the",
            "'icm' priority), run it again with the same count.",
        ],
    },
    HelpEntry {
        name: "membench",
        usage: "membench",
        lines: &[
            "measure CPU access to SRAM, cached PSRAM, and the direct alias.",
            "the 'line' rows are the ones that matter: one access per 64-byte",
            "cache line, which is what a per-pixel drawing loop produces and",
            "so the real price of write-allocate. scanout keeps running, so",
            "the PSRAM figures include the bandwidth the display is taking.",
        ],
    },
    HelpEntry {
        name: "uptime",
        usage: "uptime",
        lines: &["time since boot"],
    },
    HelpEntry {
        name: "backlight",
        usage: "backlight on|off",
        lines: &["LCD backlight"],
    },
    HelpEntry {
        name: "icm",
        usage: "icm [priority arqos]",
        lines: &[
            "display DMA arbitration and DPI FIFO underruns. with no",
            "argument, report the interconnect registers and the underrun",
            "count. with two values (0-15), set the DW-GDMA read priority",
            "and AXI QoS; 15 15 measurably lowers the underrun rate, 0 0",
            "is the power-on state. compare with 'stress'.",
        ],
    },
    HelpEntry {
        name: "ppafill",
        usage: "ppafill <x> <y> <w> <h> <color> [cpu] | ppafill sweep",
        lines: &[
            "fill a rectangle through the PPA and report how long it took.",
            "color is RGB565, decimal or 0x-prefixed. add 'cpu' to fill the",
            "same rectangle with the CPU store loop instead; the two must be",
            "indistinguishable on the panel. 'sweep' times both paths from",
            "one console cell up to the full screen, which is what decides",
            "the size below which the DMA setup costs more than it saves.",
        ],
    },
    HelpEntry {
        name: "paint",
        usage: "paint",
        lines: &["touch drawing screen"],
    },
    HelpEntry {
        name: "touchtest",
        usage: "touchtest",
        lines: &["live multi-touch test; use two fingers, any key exits"],
    },
    HelpEntry {
        name: "coordtest",
        usage: "coordtest",
        lines: &[
            "full-screen coordinate chart: a 100-pixel grid, the logical centre",
            "axes, labelled corners, and four one-pixel inset borders (red is",
            "the exact edge, then green, blue, white). hold a ruler against it",
            "to check the CW rotation and that nothing is clipped or offset.",
            "any key exits.",
        ],
    },
    HelpEntry {
        name: "axistest",
        usage: "axistest",
        lines: &["tilt-controlled BMI270 ball test; any key exits"],
    },
    HelpEntry {
        name: "battery",
        usage: "battery",
        lines: &[
            "live INA226 battery monitor: pack voltage, current, power, and",
            "a voltage-based estimate; any key exits",
        ],
    },
    HelpEntry {
        name: "rtc",
        usage: "rtc | rtc set <YYYY-MM-DD> <HH:MM:SS> | rtc regs | rtc test",
        lines: &[
            "RX8130CE real-time clock (board I2C 0x32). with no argument,",
            "show the calendar and the flag/control registers. 'set' writes",
            "the calendar (day of the week is computed from the date) and",
            "clears the voltage-low flag. 'regs' dumps registers 0x10-0x1F.",
            "'test' checks that the device answers, that the calendar is a",
            "valid date, and that the counters actually advance -- it waits",
            "for two second-carries, so it takes about 3 seconds and the",
            "whole report appears at once when it finishes.",
        ],
    },
    HelpEntry {
        name: "sdinfo",
        usage: "sdinfo",
        lines: &["activate SD card, show CID/CSD summary"],
    },
    HelpEntry {
        name: "sdread",
        usage: "sdread <lba>",
        lines: &["read one 512-byte block, dump to UART log"],
    },
    HelpEntry {
        name: "sdreadn",
        usage: "sdreadn <lba> <n>",
        lines: &["read n blocks (DMA, n<=8), dump to UART log"],
    },
    HelpEntry {
        name: "sdwritetest",
        usage: "sdwritetest <lba>",
        lines: &["write+verify+restore 1 block at lba (DMA)"],
    },
    HelpEntry {
        name: "sdzero",
        usage: "sdzero <lba>",
        lines: &["write a zeroed block at lba (DMA, no round-trip)"],
    },
    HelpEntry {
        name: "sdmbr",
        usage: "sdmbr",
        lines: &["show MBR partition table (LBA 0)"],
    },
    HelpEntry {
        name: "sdreadpsram",
        usage: "sdreadpsram <lba> <n>",
        lines: &["DMA n blocks (n<=8) into PSRAM, verify vs SRAM"],
    },
    HelpEntry {
        name: "usbinfo",
        usage: "usbinfo",
        lines: &[
            "show every device currently attached to USB-A (direct or behind",
            "a hub) from the last scan; run usbrescan first if just plugged in",
        ],
    },
    HelpEntry {
        name: "usbrescan",
        usage: "usbrescan",
        lines: &[
            "force a fresh USB-A probe: reset the port, re-enumerate whatever",
            "is plugged in (and every occupied port if it is a hub)",
        ],
    },
    HelpEntry {
        name: "usbvbus",
        usage: "usbvbus <0-7> on|off",
        lines: &[
            "raw PI4IOE2 (0x44) output-bit toggle; bit 3 = USB-A VBUS",
            "mainly useful for diagnostics; usbrescan drives bit 3 itself",
        ],
    },
    HelpEntry {
        name: "shutdown",
        usage: "shutdown",
        lines: &[
            "turn off the whole Tab5 through the board power controller",
            "(save data first; press the physical power key to start again)",
        ],
    },
    HelpEntry {
        name: "usbhub",
        usage: "usbhub",
        lines: &[
            "show the attached USB hub's descriptor and every port's live",
            "status, plus which class driver (if any) is attached to it",
        ],
    },
    HelpEntry {
        name: "usbhw",
        usage: "usbhw",
        lines: &[
            "dump the DWC core's GHWCFG registers and probe HCSPLT to show",
            "whether split transactions exist in hardware at all",
        ],
    },
    HelpEntry {
        name: "usbmsc",
        usage: "usbmsc",
        lines: &[
            "SCSI INQUIRY/TEST UNIT READY/READ CAPACITY(10) against the",
            "attached Mass Storage device (direct or behind a hub port)",
        ],
    },
    HelpEntry {
        name: "usbread",
        usage: "usbread <lba>",
        lines: &["USB MSC: read one 512-byte block (SCSI READ(10)), dump to UART log"],
    },
    HelpEntry {
        name: "usbmbr",
        usage: "usbmbr",
        lines: &["USB MSC: show MBR partition table (LBA 0), same format as sdmbr"],
    },
    HelpEntry {
        name: "reboot",
        usage: "reboot",
        lines: &["restart the board"],
    },
];

/// What the foreground application loop should do once a command has
/// been dispatched.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Outcome {
    /// Keep running the console; write a fresh prompt.
    Continue,
    /// Reboot once this frame's output has reached the panel.
    Reboot,
    /// Shut down once this frame's output has reached the panel.
    Shutdown,
    /// Hand the display over to the touch paint screen.
    Paint,
    /// Hand the display over to the multi-touch diagnostic screen.
    TouchTest,
    /// Hand the display over to the coordinate calibration chart.
    CoordTest,
    /// Hand the display over to the BMI270 tilt diagnostic screen.
    AxisTest,
    /// Hand the display over to the INA226 battery monitor.
    Battery,
}

/// Parses and runs one command line, returning what the caller should do
/// next.
///
/// `usb_host` is the single registry the application's frame loop owns
/// (`docs/USB_REFACTOR_PLAN.md` Stage A) -- every USB command reads or drives
/// devices already in it instead of probing the bus independently, which
/// is what used to let a diagnostic command reset a live keyboard/Mass
/// Storage session out from under itself.
pub fn execute(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    line: &[u8],
    usb_host: &mut usb::UsbHost,
) -> Outcome {
    let line = trim(line);
    if line.is_empty() {
        return Outcome::Continue;
    }
    // `Console::write_output_line` mirrors every result line to the UART
    // log; echoing the command itself first makes that log read as a
    // transcript instead of anonymous output.
    uart::log(b"> ");
    uart::log(line);
    uart::log(b"\r\n");

    let (command, rest) = split_first_word(line);
    let argument = trim(rest);
    match command {
        b"help" => cmd_help(console, framebuffer, argument),
        b"clear" => console.clear(framebuffer),
        b"echo" => console.write_output_line(framebuffer, as_str(argument)),
        b"about" | b"version" => console.write_output_line(framebuffer, "Tab5 Shell 0.1"),
        b"cpuinfo" => cmd_cpuinfo(console, framebuffer),
        b"mem" => cmd_mem(console, framebuffer),
        b"alloctest" => cmd_alloctest(console, framebuffer, argument),
        b"uptime" => cmd_uptime(console, framebuffer),
        b"membench" => cmd_membench(console, framebuffer),
        b"stress" => cmd_stress(console, framebuffer, argument),
        b"backlight" => cmd_backlight(console, framebuffer, argument),
        b"icm" => cmd_icm(console, framebuffer, argument),
        b"ppafill" => cmd_ppafill(console, framebuffer, argument),
        b"rtc" => cmd_rtc(console, framebuffer, argument),
        b"sdinfo" => cmd_sdinfo(console, framebuffer),
        b"sdread" => cmd_sdread(console, framebuffer, argument),
        b"sdreadn" => cmd_sdreadn(console, framebuffer, argument),
        b"sdwritetest" => cmd_sdwritetest(console, framebuffer, argument),
        b"sdzero" => cmd_sdzero(console, framebuffer, argument),
        b"sdmbr" => cmd_sdmbr(console, framebuffer),
        b"sdreadpsram" => cmd_sdreadpsram(console, framebuffer, argument),
        b"usbinfo" => cmd_usbinfo(console, framebuffer, usb_host),
        b"usbrescan" => cmd_usbrescan(console, framebuffer, usb_host),
        b"usbvbus" => cmd_usbvbus(console, framebuffer, argument),
        b"usbhub" => cmd_usbhub(console, framebuffer, usb_host),
        b"usbhw" => cmd_usbhw(console, framebuffer, usb_host),
        b"usbmsc" => cmd_usbmsc(console, framebuffer, usb_host),
        b"usbread" => cmd_usbread(console, framebuffer, argument, usb_host),
        b"usbmbr" => cmd_usbmbr(console, framebuffer, usb_host),
        b"paint" => return Outcome::Paint,
        b"touchtest" => return Outcome::TouchTest,
        b"coordtest" => return Outcome::CoordTest,
        b"axistest" => return Outcome::AxisTest,
        b"battery" | b"batinfo" => return Outcome::Battery,
        b"reboot" | b"reset" => {
            console.write_output_line(framebuffer, "rebooting...");
            return Outcome::Reboot;
        }
        b"shutdown" | b"poweroff" => {
            if argument.is_empty() {
                console.write_output_line(framebuffer, "shutting down...");
                return Outcome::Shutdown;
            }
            console.write_output_line(framebuffer, "usage: shutdown");
        }
        _ => console.write_output_line(framebuffer, "unknown command (try 'help')"),
    }
    Outcome::Continue
}

/// Reboots the board. The caller must have already flushed the "rebooting..."
/// output to the panel; this never returns.
pub fn reboot() -> ! {
    // Only the HP CPU core is reset, so scanout would otherwise keep reading
    // PSRAM -- at the raised interconnect priority this firmware gave it --
    // right through the bootloader's flash reads and the next boot's PSRAM
    // bring-up. The boot path quiesces it too, for resets that never reach
    // here, but that is too late to help the bootloader.
    lcd::quiesce_dma();
    startup::reboot()
}

/// Sends the board power controller's hardware shutdown request.
///
/// The caller has already flushed the status line, so an otherwise immediate
/// power cut still gives the user feedback on the display.
pub fn shutdown() -> bool {
    power::shutdown()
}

/// With no argument, lists command names only; with a command name, shows
/// its usage and description. `write_output_line` wraps at the console's
/// column width on its own, so the name list can just be one long line.
fn cmd_help(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    if argument.is_empty() {
        console.write_output_line(framebuffer, "commands (help <name> for details):");
        let mut names = String::new();
        for (index, entry) in HELP_ENTRIES.iter().enumerate() {
            if index > 0 {
                names.push(' ');
            }
            names.push_str(entry.name);
        }
        console.write_output_line(framebuffer, &names);
        return;
    }

    let name = as_str(argument);
    match HELP_ENTRIES.iter().find(|entry| entry.name == name) {
        Some(entry) => {
            console.write_output_line(framebuffer, entry.usage);
            for line in entry.lines {
                console.write_output_line(framebuffer, line);
            }
        }
        None => console.write_output_line(framebuffer, "unknown command (try 'help')"),
    }
}

fn cmd_mem(console: &mut Console, framebuffer: &mut Framebuffer) {
    let mut line = Line::new();
    line.push_str("PSRAM window: ");
    line.push_u32(psram::MAPPED_BYTES as u32);
    line.push_str(" bytes");
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("framebuffer: ");
    line.push_u32(psram::FRAMEBUFFER_BYTES as u32);
    line.push_str(" bytes");
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("heap: ");
    line.push_u32((heap_bytes() / (1024 * 1024)) as u32);
    line.push_str(" MiB");
    console.write_output_line(framebuffer, line.as_str());
}

/// Shows the standard RISC-V machine identification registers verbatim.
///
/// The architecture permits implementation-defined ID values (including
/// zero), so this intentionally does not attempt to map them to a vendor or
/// core name. `mhartid` identifies the hart executing this shell command.
/// The `misa` line additionally renders its known single-letter ISA
/// extensions in the RISC-V canonical order.
fn cmd_cpuinfo(console: &mut Console, framebuffer: &mut Framebuffer) {
    let mvendorid: u32;
    let marchid: u32;
    let mimpid: u32;
    let mhartid: u32;
    let misa: u32;
    unsafe {
        core::arch::asm!("csrr {value}, mvendorid", value = out(reg) mvendorid, options(nomem, nostack));
        core::arch::asm!("csrr {value}, marchid", value = out(reg) marchid, options(nomem, nostack));
        core::arch::asm!("csrr {value}, mimpid", value = out(reg) mimpid, options(nomem, nostack));
        core::arch::asm!("csrr {value}, mhartid", value = out(reg) mhartid, options(nomem, nostack));
        core::arch::asm!("csrr {value}, misa", value = out(reg) misa, options(nomem, nostack));
    }

    console.write_output_line(framebuffer, "RISC-V machine CSRs:");
    for (name, value) in [
        ("mvendorid", mvendorid),
        ("marchid", marchid),
        ("mimpid", mimpid),
        ("mhartid", mhartid),
    ] {
        let mut line = Line::new();
        line.push_str(name);
        line.push_str(": 0x");
        line.push_hex(value, 8);
        console.write_output_line(framebuffer, line.as_str());
    }

    let mut line = Line::new();
    line.push_str("misa: 0x");
    line.push_hex(misa, 8);
    line.push_str(" (");
    push_misa_isa(&mut line, misa);
    line.push_str(")");
    console.write_output_line(framebuffer, line.as_str());
}

/// Appends the ISA name derivable from `misa`.
///
/// `misa` reports only single-letter extensions. In particular, it cannot
/// identify individual `Z*` extensions or name non-standard extensions, even
/// when its `X` bit is set. The single-letter extensions follow the canonical
/// order specified by the RISC-V ISA naming convention.
fn push_misa_isa(line: &mut Line, misa: u32) {
    if misa == 0 {
        line.push_str("unavailable");
        return;
    }

    line.push_str(match misa >> 30 {
        1 => "RV32",
        2 => "RV64",
        3 => "RV128",
        _ => "RV?",
    });

    // I and E are alternate base ISAs. The specification requires I to be
    // selected when both are supported at reset, so prefer it defensively.
    if misa_has_extension(misa, b'I') {
        line.push_str("I");
    } else if misa_has_extension(misa, b'E') {
        line.push_str("E");
    } else {
        line.push_str("?");
    }

    // Canonical order for standard single-letter extensions after I/E:
    // M, A, F, D, Q, C, B, P, V, H. `G` is an abbreviation, not a bit to
    // render; privilege-mode and custom-extension bits are not ISA names.
    for extension in b"MAFDQCBPVH" {
        if misa_has_extension(misa, *extension) {
            line.push_ascii(&[*extension]);
        }
    }
}

fn misa_has_extension(misa: u32, extension: u8) -> bool {
    misa & (1 << (extension - b'A')) != 0
}

/// Bytes of PSRAM past the two framebuffers, matching `Psram::heap`'s split
/// and backing the global allocator installed in `main`.
fn heap_bytes() -> usize {
    psram::MAPPED_BYTES - psram::FRAMEBUFFER_BYTES
}

/// Allocates `mib` MiB from the PSRAM-backed global allocator, fills it with
/// a per-byte pattern derived from its index, reads it back and reports any
/// mismatch. Uses `try_reserve_exact` so a too-large request reports failure
/// instead of aborting the firmware.
fn cmd_alloctest(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let Some(mib) = parse_u32(argument) else {
        console.write_output_line(framebuffer, "usage: alloctest <MiB>");
        return;
    };
    if mib == 0 {
        console.write_output_line(framebuffer, "MiB must be at least 1");
        return;
    }
    let bytes = mib as usize * 1024 * 1024;

    let mut line = Line::new();
    line.push_str("allocating ");
    line.push_u32(mib);
    line.push_str(" MiB (heap has ");
    line.push_u32((heap_bytes() / (1024 * 1024)) as u32);
    line.push_str(" MiB)...");
    console.write_output_line(framebuffer, line.as_str());

    let mut buffer: Vec<u8> = Vec::new();
    if buffer.try_reserve_exact(bytes).is_err() {
        console.write_output_line(
            framebuffer,
            "allocation failed (not enough contiguous heap)",
        );
        return;
    }
    buffer.resize(bytes, 0);

    console.write_output_line(framebuffer, "writing pattern...");
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = pattern_byte(index);
    }

    console.write_output_line(framebuffer, "verifying...");
    let mut mismatches: u32 = 0;
    let mut first_mismatch = None;
    for (index, &byte) in buffer.iter().enumerate() {
        if byte != pattern_byte(index) {
            mismatches += 1;
            if first_mismatch.is_none() {
                first_mismatch = Some(index);
            }
        }
    }
    drop(buffer);

    let mut line = Line::new();
    if mismatches == 0 {
        line.push_str("OK: ");
        line.push_u32(mib);
        line.push_str(" MiB allocated, written and read back correctly");
    } else {
        line.push_str("FAILED: ");
        line.push_u32(mismatches);
        line.push_str(" mismatch(es), first at offset 0x");
        line.push_hex(first_mismatch.unwrap_or(0) as u32, 8);
    }
    console.write_output_line(framebuffer, line.as_str());
}

/// A well-mixed byte per index so nearby or aliased addresses are unlikely
/// to share a value; a plain `index as u8` would just repeat every 256 bytes.
fn pattern_byte(index: usize) -> u8 {
    ((index as u32).wrapping_mul(2_654_435_761) >> 24) as u8
}

/// Repeats a full-screen fill and reports its cost.
///
/// A full-screen fill is the redraw that starves the DSI bridge, so this is
/// both the workload that provokes underruns and the baseline any replacement
/// for it has to beat. Fixed and countable, so two settings can be compared by
/// running the same count under each.
fn cmd_stress(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let count = if argument.is_empty() {
        10
    } else {
        match parse_u32(argument) {
            Some(value) if value > 0 && value <= 1000 => value,
            _ => {
                console.write_output_line(framebuffer, "usage: stress [count] (1-1000)");
                return;
            }
        }
    };

    // The underrun indication is one sticky bit and the frame loop is what
    // normally consumes it. This loop never yields to the frame loop, so it
    // has to consume the bit itself -- and between fills rather than once at
    // the end, or every underrun in the whole run collapses into one.
    lcd::take_underrun();
    let start = membench::cycles();
    let mut underruns = 0u32;
    for _ in 0..count {
        framebuffer.fill(crate::framebuffer::BLACK);
        framebuffer.flush();
        if lcd::take_underrun() {
            underruns += 1;
        }
    }
    let elapsed = membench::cycles().wrapping_sub(start);

    // The screen is now blank; put the console back over it.
    console.clear(framebuffer);
    console.write_prompt(framebuffer);

    let cpu_hz = startup::cpu_hz();
    let microseconds = ((elapsed as u64) * 1_000_000 / cpu_hz as u64) as u32;

    let mut line = Line::new();
    line.push_str("stress: ");
    line.push_u32(count);
    line.push_str(" full-screen fills in ");
    line.push_u32(microseconds / 1000);
    line.push_str(" ms");
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("per fill: ");
    line.push_u32(microseconds / count.max(1) / 1000);
    line.push_str(" ms   fills that underran: ");
    line.push_u32(underruns);
    line.push_str("/");
    line.push_u32(count);
    console.write_output_line(framebuffer, line.as_str());
}

/// Measures CPU-side memory cost and prints it.
///
/// The estimates this replaces were built from the MSPI timing registers with
/// a guessed command-phase length; these are the cycle counter's answer.
fn cmd_membench(console: &mut Console, framebuffer: &mut Framebuffer) {
    console.write_output_line(framebuffer, "measuring (scanout keeps running)...");

    // A 64-byte aligned span of the PSRAM heap, big enough that a pass over it
    // cannot sit in L2.
    let words = membench::psram_bytes() / 4 + 16;
    let mut buffer: Vec<u32> = Vec::new();
    let psram = if buffer.try_reserve_exact(words).is_ok() {
        buffer.resize(words, 0);
        let raw = buffer.as_mut_ptr() as usize;
        let aligned = (raw + 63) & !63;
        Some((aligned as *mut u32, membench::psram_bytes()))
    } else {
        console.write_output_line(framebuffer, "PSRAM buffer allocation failed; SRAM only");
        None
    };

    let report = membench::run(psram);

    let mut line = Line::new();
    line.push_str("CPU ");
    line.push_u32(report.cpu_hz / 1_000_000);
    line.push_str(" MHz, SRAM ");
    line.push_u32((membench::sram_bytes() / 1024) as u32);
    line.push_str(" KiB, PSRAM ");
    line.push_u32((membench::psram_bytes() / 1024) as u32);
    line.push_str(" KiB, L1D ");
    line.push_u32(report.l1_data_cache_bytes / 1024);
    line.push_str(" KiB");
    console.write_output_line(framebuffer, line.as_str());

    throughput_line(
        console,
        framebuffer,
        "seq write u32",
        |m| m.sequential_write_u32,
        &report,
    );
    throughput_line(
        console,
        framebuffer,
        "seq write u16",
        |m| m.sequential_write_u16,
        &report,
    );
    throughput_line(
        console,
        framebuffer,
        "seq read  u32",
        |m| m.sequential_read_u32,
        &report,
    );
    latency_line(
        console,
        framebuffer,
        "line write   ",
        |m| m.line_write_ns,
        &report,
    );
    latency_line(
        console,
        framebuffer,
        "line read    ",
        |m| m.line_read_ns,
        &report,
    );
    latency_line(
        console,
        framebuffer,
        "scatter read ",
        |m| m.scatter_read_ns,
        &report,
    );

    // A buffer that fits in L1 measures L1, not the memory behind it. That is
    // not a footnote: it changes what the SRAM column means entirely.
    if (membench::sram_bytes() as u32) <= report.l1_data_cache_bytes {
        console.write_output_line(
            framebuffer,
            "WARNING: SRAM buffer fits in L1D; SRAM column is L1, not SRAM",
        );
    }
    console.write_output_line(framebuffer, "(line = 1 access per 64B line, in order)");
    console.write_output_line(
        framebuffer,
        "(scatter = same, but 4 KiB apart: no prefetch)",
    );
}

fn throughput_line(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    label: &str,
    field: fn(&membench::Measurements) -> u32,
    report: &membench::Report,
) {
    let mut line = Line::new();
    line.push_str(label);
    line.push_str(": SRAM ");
    line.push_u32(field(&report.sram));
    line.push_str(" MB/s");
    if let Some(psram) = &report.psram {
        line.push_str("  CACHED ");
        line.push_u32(field(psram));
        line.push_str(" MB/s");
    }
    if let Some(psram) = &report.psram_direct {
        line.push_str("  DIRECT ");
        line.push_u32(field(psram));
        line.push_str(" MB/s");
    }
    console.write_output_line(framebuffer, line.as_str());
}

fn latency_line(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    label: &str,
    field: fn(&membench::Measurements) -> u32,
    report: &membench::Report,
) {
    let mut line = Line::new();
    line.push_str(label);
    line.push_str(": SRAM ");
    line.push_u32(field(&report.sram));
    line.push_str(" ns");
    if let Some(psram) = &report.psram {
        line.push_str("  CACHED ");
        line.push_u32(field(psram));
        line.push_str(" ns");
    }
    if let Some(psram) = &report.psram_direct {
        line.push_str("  DIRECT ");
        line.push_u32(field(psram));
        line.push_str(" ns");
    }
    console.write_output_line(framebuffer, line.as_str());
}

fn cmd_uptime(console: &mut Console, framebuffer: &mut Framebuffer) {
    let seconds = interrupts::frame_sequence() / FRAMES_PER_SECOND;
    let mut line = Line::new();
    line.push_str("uptime: ~");
    line.push_u32(seconds);
    line.push_str(" s (frame-counted)");
    console.write_output_line(framebuffer, line.as_str());
}

/// Reads, writes and tests the RX8130CE real-time clock.
///
/// The clock is the one board device whose whole purpose is to keep counting
/// while the firmware is not running, so "does it answer on I2C" says very
/// little about it: `rtc test` also measures a carry of the second counter,
/// which is the only check here that observes the 32.768 kHz oscillator
/// rather than the register file.
fn cmd_rtc(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let (subcommand, rest) = split_first_word(argument);
    match subcommand {
        b"" => cmd_rtc_show(console, framebuffer),
        b"regs" => cmd_rtc_regs(console, framebuffer),
        b"set" => cmd_rtc_set(console, framebuffer, trim(rest)),
        b"test" => cmd_rtc_test(console, framebuffer),
        _ => console.write_output_line(
            framebuffer,
            "usage: rtc [set <YYYY-MM-DD> <HH:MM:SS> | regs | test]",
        ),
    }
}

fn cmd_rtc_show(console: &mut Console, framebuffer: &mut Framebuffer) {
    match rtc::read_datetime() {
        Ok(datetime) => {
            let mut line = Line::new();
            push_datetime(&mut line, &datetime);
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => console.write_output_line(framebuffer, error.message()),
    }
    match rtc::read_status() {
        Ok(status) => report_rtc_status(console, framebuffer, status),
        Err(error) => console.write_output_line(framebuffer, error.message()),
    }
}

/// Dumps registers 0x10-0x1F as they were read, eight per line.
fn cmd_rtc_regs(console: &mut Console, framebuffer: &mut Framebuffer) {
    let mut registers = [0u8; rtc::REGISTER_COUNT];
    if let Err(error) = rtc::read_all_registers(&mut registers) {
        console.write_output_line(framebuffer, error.message());
        return;
    }
    for (index, chunk) in registers.chunks(8).enumerate() {
        let mut line = Line::new();
        line.push_str("0x");
        line.push_hex(rtc::FIRST_REGISTER as u32 + (index * 8) as u32, 2);
        line.push_str(":");
        for &byte in chunk {
            line.push_str(" ");
            line.push_hex(byte as u32, 2);
        }
        console.write_output_line(framebuffer, line.as_str());
    }
}

fn cmd_rtc_set(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let (date_text, rest) = split_first_word(argument);
    let time_text = trim(rest);
    let mut date = [0u32; 3];
    let mut time = [0u32; 3];
    if !parse_fields(date_text, b'-', &mut date) || !parse_fields(time_text, b':', &mut time) {
        console.write_output_line(framebuffer, "usage: rtc set <YYYY-MM-DD> <HH:MM:SS>");
        return;
    }
    // Narrowing through `try_from` so that a field too large for the calendar
    // is rejected rather than truncated into a plausible-looking one; the
    // calendar ranges themselves are `DateTime::is_valid`'s business.
    let (Ok(year), Ok(month), Ok(day), Ok(hour), Ok(minute), Ok(second)) = (
        u16::try_from(date[0]),
        u8::try_from(date[1]),
        u8::try_from(date[2]),
        u8::try_from(time[0]),
        u8::try_from(time[1]),
        u8::try_from(time[2]),
    ) else {
        console.write_output_line(
            framebuffer,
            "out of range: year 2000-2099, a real calendar day, 24-hour time",
        );
        return;
    };

    // The device's week register is written from the date, so `weekday` here
    // only has to be a value `is_valid` accepts.
    let datetime = rtc::DateTime {
        year,
        month,
        day,
        weekday: None,
        hour,
        minute,
        second,
    };
    if !datetime.is_valid() {
        console.write_output_line(
            framebuffer,
            "out of range: year 2000-2099, a real calendar day, 24-hour time",
        );
        return;
    }

    if let Err(error) = rtc::write_datetime(&datetime) {
        console.write_output_line(framebuffer, error.message());
        return;
    }
    // Reading the calendar back is what turns "the writes were acknowledged"
    // into "the device kept them".
    match rtc::read_datetime() {
        Ok(readback) => {
            let mut line = Line::new();
            line.push_str("set; reads back as ");
            push_datetime(&mut line, &readback);
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => console.write_output_line(framebuffer, error.message()),
    }
}

/// Runs the checks in increasing order of what they prove: that something
/// answers, that its control registers are in a usable state, that the
/// calendar holds a real date, that the second counter carries once per
/// second, and that the per-second update logic still raises its flag.
///
/// Only the flag register is written (to clear the update flag), so a clock
/// already keeping correct time still is afterwards.
fn cmd_rtc_test(console: &mut Console, framebuffer: &mut Framebuffer) {
    /// A carry must arrive within this long, or the counters are not running.
    const CARRY_TIMEOUT_MS: u32 = 1_500;
    /// How far a measured carry interval may sit from one second. The clock
    /// itself is a crystal part; this only has to absorb the I2C read that
    /// observes the carry and the cycle-counter conversion.
    const CARRY_TOLERANCE_MS: u32 = 50;

    if !rtc::probe() {
        console.write_output_line(framebuffer, "FAIL probe: nothing acknowledged I2C 0x32");
        return;
    }
    console.write_output_line(framebuffer, "PASS probe: 0x32 acknowledged");

    let status = match rtc::read_status() {
        Ok(status) => status,
        Err(error) => {
            console.write_output_line(framebuffer, error.message());
            return;
        }
    };
    report_rtc_status(console, framebuffer, status);

    let mut failures = 0u32;
    if status.stopped() {
        failures += 1;
    }

    match rtc::read_datetime() {
        Ok(datetime) => {
            let mut line = Line::new();
            line.push_str("PASS calendar: ");
            push_datetime(&mut line, &datetime);
            console.write_output_line(framebuffer, line.as_str());

            // The week register is an independent counter, not derived from
            // the date, so the two can legitimately be read and still
            // disagree -- which is worth naming rather than hiding.
            let expected = rtc::weekday_from_date(datetime.year, datetime.month, datetime.day);
            if datetime.weekday.is_some_and(|weekday| weekday != expected) {
                let mut line = Line::new();
                line.push_str("WARN week register disagrees with the date (expected ");
                line.push_str(rtc::weekday_name(expected));
                line.push_str(")");
                console.write_output_line(framebuffer, line.as_str());
            }
        }
        Err(error) => {
            failures += 1;
            let mut line = Line::new();
            line.push_str("FAIL calendar: ");
            line.push_str(error.message());
            console.write_output_line(framebuffer, line.as_str());
        }
    }

    // The first carry only synchronises to a second boundary; the interval
    // between it and the next one is the measurement.
    let interval = match wait_for_second_change(CARRY_TIMEOUT_MS) {
        Ok(Some(_)) => wait_for_second_change(CARRY_TIMEOUT_MS),
        timeout_or_error => timeout_or_error,
    };
    match interval {
        Ok(Some(interval_ms)) => {
            let within_tolerance = interval_ms.abs_diff(1_000) <= CARRY_TOLERANCE_MS;
            if !within_tolerance {
                failures += 1;
            }
            let mut line = Line::new();
            line.push_str(if within_tolerance { "PASS" } else { "FAIL" });
            line.push_str(" tick: second counter carried after ");
            line.push_u32(interval_ms);
            line.push_str(" ms (expected 1000)");
            console.write_output_line(framebuffer, line.as_str());
        }
        Ok(None) => {
            failures += 1;
            console.write_output_line(
                framebuffer,
                "FAIL tick: no carry within 1500 ms; the counters are not running",
            );
        }
        Err(error) => {
            failures += 1;
            console.write_output_line(framebuffer, error.message());
        }
    }

    if status.extension & rtc::EXTENSION_UPDATE_MINUTE == 0 {
        match test_update_flag(CARRY_TIMEOUT_MS) {
            Ok(Some(elapsed_ms)) => {
                let mut line = Line::new();
                line.push_str("PASS update flag: cleared, then set again after ");
                line.push_u32(elapsed_ms);
                line.push_str(" ms");
                console.write_output_line(framebuffer, line.as_str());
            }
            Ok(None) => {
                failures += 1;
                console.write_output_line(
                    framebuffer,
                    "FAIL update flag: did not clear, or was not set again within 1500 ms",
                );
            }
            Err(error) => {
                failures += 1;
                console.write_output_line(framebuffer, error.message());
            }
        }
    } else {
        console.write_output_line(
            framebuffer,
            "SKIP update flag: the extension register selects per-minute updates",
        );
    }

    let mut line = Line::new();
    if failures == 0 {
        line.push_str("rtc test: all checks passed");
    } else {
        line.push_str("rtc test: ");
        line.push_u32(failures);
        line.push_str(" check(s) failed");
    }
    console.write_output_line(framebuffer, line.as_str());
}

/// Clears the update flag and waits for the device to set it again, which the
/// RX8130CE does once per second while its extension register selects
/// per-second updates. Returns how long that took, or `None` if the flag did
/// not clear or never came back.
fn test_update_flag(timeout_ms: u32) -> Result<Option<u32>, rtc::Error> {
    rtc::clear_flag(rtc::FLAG_UPDATE)?;
    if rtc::read_status()?.flags & rtc::FLAG_UPDATE != 0 {
        return Ok(None);
    }
    let start = membench::cycles();
    loop {
        let flags = rtc::read_status()?.flags;
        let elapsed_ms = elapsed_ms_since(start);
        if flags & rtc::FLAG_UPDATE != 0 {
            return Ok(Some(elapsed_ms));
        }
        if elapsed_ms > timeout_ms {
            return Ok(None);
        }
    }
}

/// Polls the second counter until it changes, returning how long that took.
/// `None` means it had not changed within `timeout_ms`.
fn wait_for_second_change(timeout_ms: u32) -> Result<Option<u32>, rtc::Error> {
    let start = membench::cycles();
    let first = rtc::read_second()?;
    loop {
        let second = rtc::read_second()?;
        // Taken after the read that observed the change, so the reported
        // interval includes one I2C read rather than excluding it.
        let elapsed_ms = elapsed_ms_since(start);
        if second != first {
            return Ok(Some(elapsed_ms));
        }
        if elapsed_ms > timeout_ms {
            return Ok(None);
        }
    }
}

/// Milliseconds since a `membench::cycles()` reading. The counter is 32-bit
/// and wraps every 11.9 seconds at 360 MHz, which is far longer than any wait
/// this command performs.
fn elapsed_ms_since(start: u32) -> u32 {
    let cycles = membench::cycles().wrapping_sub(start) as u64;
    (cycles * 1_000 / startup::cpu_hz() as u64) as u32
}

fn report_rtc_status(console: &mut Console, framebuffer: &mut Framebuffer, status: rtc::Status) {
    let mut line = Line::new();
    line.push_str("ext=0x");
    line.push_hex(status.extension as u32, 2);
    line.push_str(" flags=0x");
    line.push_hex(status.flags as u32, 2);
    line.push_str(" ctrl0=0x");
    line.push_hex(status.control0 as u32, 2);
    line.push_str(" ctrl1=0x");
    line.push_hex(status.control1 as u32, 2);
    console.write_output_line(framebuffer, line.as_str());

    // The hex above is the device's answer verbatim; this names the bits in
    // it that a test cares about, so neither has to be taken on trust.
    let mut line = Line::new();
    line.push_str("flags set:");
    if !push_bit_names(
        &mut line,
        status.flags,
        &[
            (rtc::FLAG_VOLTAGE_LOW, "VLF"),
            (rtc::FLAG_ALARM, "AF"),
            (rtc::FLAG_TIMER, "TF"),
            (rtc::FLAG_UPDATE, "UF"),
        ],
    ) {
        line.push_str(" none");
    }
    line.push_str("   ctrl0 set:");
    if !push_bit_names(
        &mut line,
        status.control0,
        &[
            (rtc::CONTROL0_ALARM_INTERRUPT, "AIE"),
            (rtc::CONTROL0_TIMER_INTERRUPT, "TIE"),
            (rtc::CONTROL0_UPDATE_INTERRUPT, "UIE"),
            (rtc::CONTROL0_STOP, "STOP"),
            (rtc::CONTROL0_TEST, "TEST"),
        ],
    ) {
        line.push_str(" none");
    }
    console.write_output_line(framebuffer, line.as_str());

    if status.voltage_low() {
        console.write_output_line(
            framebuffer,
            "WARN voltage-low flag: the oscillator stopped, so the calendar is",
        );
        console.write_output_line(
            framebuffer,
            "     stale; 'rtc set' writes a new time and clears the flag",
        );
    }
    if status.stopped() {
        console.write_output_line(
            framebuffer,
            "FAIL STOP is set in ctrl0: the calendar counters are halted",
        );
    }
}

/// Formats a calendar as `YYYY-MM-DD (Day) HH:MM:SS`, naming an
/// uninterpretable week register instead of inventing a day for it.
fn push_datetime(line: &mut Line, datetime: &rtc::DateTime) {
    line.push_u32(datetime.year as u32);
    line.push_str("-");
    push_two_digits(line, datetime.month);
    line.push_str("-");
    push_two_digits(line, datetime.day);
    match datetime.weekday {
        Some(weekday) => {
            line.push_str(" (");
            line.push_str(rtc::weekday_name(weekday));
            line.push_str(") ");
        }
        None => line.push_str(" (week reg invalid) "),
    }
    push_two_digits(line, datetime.hour);
    line.push_str(":");
    push_two_digits(line, datetime.minute);
    line.push_str(":");
    push_two_digits(line, datetime.second);
}

/// Appends the names of whichever `names` bits are set in `value`, each
/// preceded by a space. Returns whether any was appended, so the caller can
/// say "none" rather than leaving a bare label.
fn push_bit_names(line: &mut Line, value: u8, names: &[(u8, &str)]) -> bool {
    let mut any = false;
    for &(bit, name) in names {
        if value & bit != 0 {
            line.push_str(" ");
            line.push_str(name);
            any = true;
        }
    }
    any
}

fn push_two_digits(line: &mut Line, value: u8) {
    if value < 10 {
        line.push_str("0");
    }
    line.push_u32(value as u32);
}

/// Splits `text` into exactly `values.len()` decimal fields separated by
/// `separator`, as `2026-08-17` and `12:34:56` are. Returns false unless
/// every field is present and is a plain decimal number.
fn parse_fields(text: &[u8], separator: u8, values: &mut [u32]) -> bool {
    let mut remaining = text;
    let last = values.len() - 1;
    for (index, value) in values.iter_mut().enumerate() {
        let field = if index == last {
            core::mem::take(&mut remaining)
        } else {
            match remaining.iter().position(|&byte| byte == separator) {
                Some(at) => {
                    let (head, tail) = remaining.split_at(at);
                    remaining = &tail[1..];
                    head
                }
                None => return false,
            }
        };
        match parse_u32(field) {
            Some(parsed) => *value = parsed,
            None => return false,
        }
    }
    true
}

fn cmd_sdinfo(console: &mut Console, framebuffer: &mut Framebuffer) {
    console.write_output_line(framebuffer, "activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(framebuffer, "SD card activation failed, see UART log");
        return;
    };

    let mut line = Line::new();
    line.push_str("RCA: 0x");
    line.push_hex(card.rca as u32, 4);
    line.push_str("  manufacturer ID: 0x");
    line.push_hex(card.cid[3] >> 24, 2);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("type: ");
    line.push_str(if card.high_capacity {
        "SDHC/SDXC"
    } else {
        "SDSC"
    });
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    match card.capacity_bytes {
        Some(bytes) => {
            line.push_str("capacity: ~");
            line.push_u32((bytes / (1024 * 1024)) as u32);
            line.push_str(" MiB");
        }
        None => line.push_str("capacity: unknown (CSD v1, not decoded)"),
    }
    console.write_output_line(framebuffer, line.as_str());

    // CSD PERM_WRITE_PROTECT (bit 13) / TMP_WRITE_PROTECT (bit 12), common to
    // both CSD structure versions.
    let write_protected = card.csd[0] & (0b11 << 12) != 0;
    console.write_output_line(
        framebuffer,
        if write_protected {
            "write-protected: yes"
        } else {
            "write-protected: no"
        },
    );
    console.write_output_line(
        framebuffer,
        if card.bus_width_4bit {
            "bus width: 4-bit"
        } else {
            "bus width: 1-bit (ACMD6 failed or skipped)"
        },
    );
    let mut line = Line::new();
    line.push_str("clock: ");
    line.push_u32(card.clock_khz);
    line.push_str(" kHz (");
    line.push_str(if card.high_speed {
        "High Speed"
    } else {
        "Default Speed"
    });
    line.push_str(")");
    console.write_output_line(framebuffer, line.as_str());
    console.write_output_line(framebuffer, "full CID/CSD dump: see UART log");
}

fn cmd_sdread(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let Some(lba) = parse_u32(argument) else {
        console.write_output_line(framebuffer, "usage: sdread <lba>");
        return;
    };

    console.write_output_line(framebuffer, "activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(framebuffer, "SD card activation failed, see UART log");
        return;
    };

    let mut buffer = [0u8; 512];
    if !sdmmc::read_block(&card, lba, &mut buffer) {
        console.write_output_line(framebuffer, "block read failed, see UART log");
        return;
    }
    sdmmc::dump_block(&buffer);

    let mut line = Line::new();
    line.push_str("read LBA ");
    line.push_u32(lba);
    line.push_str(": ");
    for &byte in &buffer[..8] {
        line.push_hex(byte as u32, 2);
        line.push_str(" ");
    }
    line.push_str("...");
    console.write_output_line(framebuffer, line.as_str());

    let boot_signature = buffer[510] == 0x55 && buffer[511] == 0xAA;
    console.write_output_line(
        framebuffer,
        if boot_signature {
            "bytes 510-511 = 55 AA (MBR/boot-sector signature)"
        } else {
            "no 55 AA signature at bytes 510-511"
        },
    );
    console.write_output_line(framebuffer, "full 512-byte hex dump: see UART log");
}

const MAX_MULTI_BLOCKS: u32 = 8;

fn cmd_sdreadn(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let (lba_text, count_text) = split_first_word(argument);
    let (Some(lba), Some(count)) = (parse_u32(lba_text), parse_u32(trim(count_text))) else {
        console.write_output_line(framebuffer, "usage: sdreadn <lba> <count>");
        return;
    };
    if count == 0 || count > MAX_MULTI_BLOCKS {
        console.write_output_line(framebuffer, "count must be 1..=8");
        return;
    }

    console.write_output_line(framebuffer, "activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(framebuffer, "SD card activation failed, see UART log");
        return;
    };

    let mut buffer = [0u8; 512 * MAX_MULTI_BLOCKS as usize];
    let region = &mut buffer[..512 * count as usize];
    if !sdmmc::read_blocks(&card, lba, region) {
        console.write_output_line(framebuffer, "multi-block read failed, see UART log");
        return;
    }
    for (index, block) in region.chunks_exact(512).enumerate() {
        sdmmc::dump_block_at(block.try_into().unwrap(), (index * 512) as u16);
    }

    let mut line = Line::new();
    line.push_str("read ");
    line.push_u32(count);
    line.push_str(" block(s) from LBA ");
    line.push_u32(lba);
    line.push_str(" via DMA, OK");
    console.write_output_line(framebuffer, line.as_str());
    console.write_output_line(framebuffer, "full hex dump: see UART log");
}

fn cmd_sdwritetest(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let Some(lba) = parse_u32(argument) else {
        console.write_output_line(framebuffer, "usage: sdwritetest <lba>");
        return;
    };

    console.write_output_line(
        framebuffer,
        "WARNING: temporarily overwrites 1 block, then restores it",
    );
    console.write_output_line(framebuffer, "activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(framebuffer, "SD card activation failed, see UART log");
        return;
    };

    let mut original = [0u8; 512];
    if !sdmmc::read_blocks(&card, lba, &mut original) {
        console.write_output_line(
            framebuffer,
            "could not read original block, aborting (nothing written)",
        );
        return;
    }

    let mut pattern = [0u8; 512];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = (index as u8) ^ 0xA5;
    }

    if !sdmmc::write_blocks(&card, lba, &mut pattern) {
        console.write_output_line(framebuffer, "pattern write failed, see UART log");
        return;
    }
    let mut verify = [0u8; 512];
    let pattern_ok = sdmmc::read_blocks(&card, lba, &mut verify) && verify == pattern;
    console.write_output_line(
        framebuffer,
        if pattern_ok {
            "pattern write+read-back: match"
        } else {
            "pattern write+read-back: MISMATCH, see UART log"
        },
    );

    let mut restore = original;
    let restored = sdmmc::write_blocks(&card, lba, &mut restore);
    let mut check = [0u8; 512];
    let restore_ok = restored && sdmmc::read_blocks(&card, lba, &mut check) && check == original;
    console.write_output_line(
        framebuffer,
        if restore_ok {
            "original data restored: yes"
        } else {
            "original data restored: NO -- see UART log, LBA may be corrupted"
        },
    );
}

fn cmd_sdzero(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let Some(lba) = parse_u32(argument) else {
        console.write_output_line(framebuffer, "usage: sdzero <lba>");
        return;
    };

    console.write_output_line(framebuffer, "activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(framebuffer, "SD card activation failed, see UART log");
        return;
    };

    let mut zero = [0u8; 512];
    if !sdmmc::write_blocks(&card, lba, &mut zero) {
        console.write_output_line(framebuffer, "zero write failed, see UART log");
        return;
    }
    console.write_output_line(framebuffer, "block zeroed");
}

/// Reads LBA 0 from the SD card and hands it to `mbr::show` -- the
/// device-specific half of the SD/USB split described in
/// `docs/USB_MSC_PLAN.md` Stage 6; the actual MBR parsing lives in `mbr.rs` and
/// knows nothing about SD.
fn cmd_sdmbr(console: &mut Console, framebuffer: &mut Framebuffer) {
    console.write_output_line(framebuffer, "activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(framebuffer, "SD card activation failed, see UART log");
        return;
    };

    let mut sector = [0u8; 512];
    if !sdmmc::read_block(&card, 0, &mut sector) {
        console.write_output_line(framebuffer, "MBR read failed, see UART log");
        return;
    }
    mbr::show(console, framebuffer, &sector);
}

/// Reads the same blocks twice -- once into a stack (internal SRAM) buffer,
/// once into a `Vec` on the PSRAM-backed heap -- through the identical
/// `sdmmc::read_blocks` DMA path, then compares them byte-for-byte. This is
/// the test for whether the SDHOST's IDMAC can address PSRAM's cache-mapped
/// window directly: `read_blocks` never branches on the destination
/// address, so if IDMAC's bus reach doesn't extend there, or the existing
/// cache writeback/invalidate isn't sufficient for PSRAM, this either times
/// out/fails outright or comes back with silently wrong bytes -- which the
/// comparison catches without having to eyeball a hex dump.
fn cmd_sdreadpsram(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let (lba_text, count_text) = split_first_word(argument);
    let (Some(lba), Some(count)) = (parse_u32(lba_text), parse_u32(trim(count_text))) else {
        console.write_output_line(framebuffer, "usage: sdreadpsram <lba> <count>");
        return;
    };
    if count == 0 || count > MAX_MULTI_BLOCKS {
        console.write_output_line(framebuffer, "count must be 1..=8");
        return;
    }
    let bytes = 512 * count as usize;

    console.write_output_line(framebuffer, "activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(framebuffer, "SD card activation failed, see UART log");
        return;
    };

    let mut sram_reference = [0u8; 512 * MAX_MULTI_BLOCKS as usize];
    let sram_region = &mut sram_reference[..bytes];
    if !sdmmc::read_blocks(&card, lba, sram_region) {
        console.write_output_line(framebuffer, "SRAM reference read failed, see UART log");
        return;
    }

    let mut psram_buffer: Vec<u8> = Vec::new();
    if psram_buffer.try_reserve_exact(bytes).is_err() {
        console.write_output_line(
            framebuffer,
            "PSRAM allocation failed (not enough contiguous heap)",
        );
        return;
    }
    psram_buffer.resize(bytes, 0);

    console.write_output_line(
        framebuffer,
        "DMA-ing the same blocks directly into PSRAM...",
    );
    if !sdmmc::read_blocks(&card, lba, &mut psram_buffer) {
        console.write_output_line(framebuffer, "PSRAM DMA read failed, see UART log");
        return;
    }

    if psram_buffer.as_slice() == sram_region {
        console.write_output_line(
            framebuffer,
            "match: SD -> PSRAM DMA works, same bytes as SD -> SRAM",
        );
    } else {
        let mismatches = psram_buffer
            .iter()
            .zip(sram_region.iter())
            .filter(|(a, b)| a != b)
            .count();
        let mut line = Line::new();
        line.push_str("MISMATCH: ");
        line.push_u32(mismatches as u32);
        line.push_str(" of ");
        line.push_u32(bytes as u32);
        line.push_str(" bytes differ");
        console.write_output_line(framebuffer, line.as_str());
        console.write_output_line(framebuffer, "SD -> PSRAM DMA does not work as-is");
    }
    console.write_output_line(
        framebuffer,
        "first block of the SRAM reference, for reference:",
    );
    sdmmc::dump_block_at((&sram_region[..512]).try_into().unwrap(), 0);
}

/// Shared by every read-only USB command: prints the most recent root-port
/// probe (`UsbHost::rescan`, which runs automatically at startup and
/// periodically thereafter -- see `lcd.rs`) instead of running a fresh one
/// itself. Returns true only if that probe ended up enabled and ready for
/// control transfers.
///
/// This -- together with every other USB command reading or driving
/// devices already in `usb_host` instead of calling `usb::probe_port`/
/// `usb::enumerate_device` on its own -- is `docs/USB_REFACTOR_PLAN.md` Stage
/// A: only `UsbHost::rescan` (via `usbrescan`, or `lcd.rs`'s frame loop)
/// ever resets the bus, so no USB command can invalidate another device's
/// live session anymore.
fn report_last_probe(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &usb::UsbHost,
) -> bool {
    let Some(port) = usb_host.last_probe() else {
        console.write_output_line(framebuffer, "USB-A not probed yet; try 'usbrescan'");
        return false;
    };

    console.write_output_line(
        framebuffer,
        if port.vbus_enable_acked {
            "VBUS enable: I2C ok"
        } else {
            "VBUS enable: I2C not acked (PI4IOE2 @ 0x44 not responding)"
        },
    );

    if !port.core_alive {
        let mut line = Line::new();
        line.push_str("DWC core not responding, GSNPSID=0x");
        line.push_hex(port.core_id, 8);
        console.write_output_line(framebuffer, line.as_str());
        return false;
    }

    let mut line = Line::new();
    line.push_str("core id: 0x");
    line.push_hex(port.core_id, 8);
    line.push_str("  channels: ");
    line.push_u32(port.channel_count);
    line.push_str("  fifo: ");
    line.push_u32(port.fifo_depth_words);
    line.push_str("w");
    console.write_output_line(framebuffer, line.as_str());

    // The speed line below only means what it says once it is clear
    // whether the host was allowed to negotiate High-Speed at all.
    console.write_output_line(
        framebuffer,
        if usb::FORCE_FS_LS_ONLY_HOST {
            "host: FS/LS-only forced (HCFG.FSLSSupp)"
        } else {
            "host: High-Speed capable, split transactions on (see 'usbhw')"
        },
    );

    if !port.connected {
        console.write_output_line(
            framebuffer,
            "no device detected (plug in USB-A and try 'usbrescan')",
        );
        return false;
    }
    console.write_output_line(
        framebuffer,
        if port.enabled {
            "device connected, port reset and enabled"
        } else {
            "device connected, but port did not enable after reset"
        },
    );
    if !port.enabled {
        return false;
    }
    let mut line = Line::new();
    line.push_str("speed: ");
    line.push_str(speed_text(port.speed));
    console.write_output_line(framebuffer, line.as_str());
    true
}

fn speed_text(speed: usb::Speed) -> &'static str {
    match speed {
        usb::Speed::High => "High-Speed",
        usb::Speed::Full => "Full-Speed",
        usb::Speed::Low => "Low-Speed",
        usb::Speed::Unknown => "unknown",
    }
}

/// Read-only: shows every device the last scan attached, root or hub port
/// alike. Run `usbrescan` first if something was just plugged in.
fn cmd_usbinfo(console: &mut Console, framebuffer: &mut Framebuffer, usb_host: &usb::UsbHost) {
    report_usb_state(console, framebuffer, usb_host);
}

/// Forces a fresh probe (`UsbHost::rescan`: port reset, every address
/// reassigned) and then shows the same report as `usbinfo`. This is the
/// only USB shell command that resets the bus -- run it after plugging
/// something in, not routinely, since it briefly drops whatever else was
/// already attached and working.
fn cmd_usbrescan(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &mut usb::UsbHost,
) {
    console.write_output_line(framebuffer, "probing USB-A host port (USB-DWC HS)...");
    usb_host.rescan();
    report_usb_state(console, framebuffer, usb_host);
}

/// Shared by `usbinfo` and `usbrescan`.
fn report_usb_state(console: &mut Console, framebuffer: &mut Framebuffer, usb_host: &usb::UsbHost) {
    if !report_last_probe(console, framebuffer, usb_host) {
        return;
    }
    if usb_host.hub().is_some() {
        console.write_output_line(
            framebuffer,
            "hub attached; see 'usbhub' for per-port detail",
        );
    }

    let mut any = false;
    for device in usb_host.attached_devices() {
        any = true;
        console.write_output_line(framebuffer, location_text(device.location).as_str());
        console.write_output_line(framebuffer, device_summary_text(device.summary).as_str());
        console.write_output_line(framebuffer, device_kind_text(device.kind).as_str());
    }
    if !any {
        console.write_output_line(
            framebuffer,
            "no supported device attached (unsupported device, or nothing plugged in)",
        );
    }
}

fn location_text(location: usb::Location) -> Line {
    let mut line = Line::new();
    match location {
        usb::Location::Direct => line.push_str("USB-A direct:"),
        usb::Location::HubPort(port) => {
            line.push_str("hub port ");
            line.push_u32(port as u32);
            line.push_str(":");
        }
    }
    line
}

fn device_summary_text(summary: &usb::DeviceSummary) -> Line {
    let mut line = Line::new();
    line.push_str("  VID:PID = 0x");
    line.push_hex(summary.vendor_id as u32, 4);
    line.push_str(":0x");
    line.push_hex(summary.product_id as u32, 4);
    line.push_str("  class ");
    line.push_hex(summary.device_class as u32, 2);
    line.push_str("/");
    line.push_hex(summary.device_subclass as u32, 2);
    line.push_str("/");
    line.push_hex(summary.device_protocol as u32, 2);
    line.push_str("  interfaces: ");
    line.push_u32(summary.num_interfaces as u32);
    line.push_str("  config bytes: ");
    line.push_u32(summary.config_total_length as u32);
    line
}

fn device_kind_text(kind: &usb::DeviceKind) -> Line {
    let mut line = Line::new();
    line.push_str("  driver: ");
    line.push_str(match kind {
        usb::DeviceKind::Keyboard(_) => "HID Boot keyboard",
        usb::DeviceKind::MassStorage(_) => "Mass Storage (Bulk-Only Transport)",
    });
    line
}

/// `docs/USB_MSC_PLAN.md` Stage 1-4, extended by `docs/USB_REFACTOR_PLAN.md` Stage F:
/// runs SCSI INQUIRY/TEST UNIT READY/READ CAPACITY(10) against the Mass
/// Storage device `UsbHost::rescan` already attached, wherever it is --
/// USB-A directly or a hub port -- instead of enumerating one fresh. See
/// `usbinfo` for VID/PID and interface identity; if nothing shows up here,
/// plug a device in and run `usbrescan` first.
fn cmd_usbmsc(console: &mut Console, framebuffer: &mut Framebuffer, usb_host: &mut usb::UsbHost) {
    let Some(mass_storage) = usb_host.mass_storage_mut() else {
        console.write_output_line(
            framebuffer,
            "no Mass Storage device attached; plug one in and run 'usbrescan'",
        );
        return;
    };

    console.write_output_line(framebuffer, "sending SCSI INQUIRY (bulk transfers)...");
    let Some(inquiry) = mass_storage.inquiry() else {
        console.write_output_line(framebuffer, "INQUIRY failed, see UART log");
        return;
    };

    // Standard INQUIRY data (SPC): Vendor Identification is bytes 8-15,
    // Product Identification is bytes 16-31, Product Revision Level is
    // bytes 32-35 -- fixed offsets, same spirit as `cmd_sdmbr`'s fixed MBR
    // offsets.
    let mut line = Line::new();
    line.push_str("Vendor: ");
    line.push_ascii(&inquiry[8..16]);
    line.push_str("  Product: ");
    line.push_ascii(&inquiry[16..32]);
    line.push_str("  Rev: ");
    line.push_ascii(&inquiry[32..36]);
    console.write_output_line(framebuffer, line.as_str());

    console.write_output_line(framebuffer, "checking media (TEST UNIT READY)...");
    match mass_storage.test_unit_ready() {
        Some(true) => console.write_output_line(framebuffer, "media ready"),
        Some(false) => {
            console.write_output_line(framebuffer, "media not ready");
            if let Some(sense) = mass_storage.request_sense() {
                let mut line = Line::new();
                line.push_str("sense key: 0x");
                line.push_hex((sense[2] & 0x0F) as u32, 1);
                console.write_output_line(framebuffer, line.as_str());
            } else {
                console.write_output_line(framebuffer, "REQUEST SENSE failed, see UART log");
            }
        }
        None => {
            console.write_output_line(framebuffer, "TEST UNIT READY command failed, see UART log");
            return;
        }
    }

    console.write_output_line(framebuffer, "reading capacity (READ CAPACITY(10))...");
    let Some(capacity) = mass_storage.read_capacity() else {
        console.write_output_line(framebuffer, "READ CAPACITY(10) failed, see UART log");
        return;
    };
    let block_count = capacity.last_lba as u64 + 1;
    let total_mib = block_count * capacity.block_length as u64 / (1024 * 1024);

    let mut line = Line::new();
    line.push_str("capacity: ");
    line.push_u32(block_count as u32);
    line.push_str(" blocks x ");
    line.push_u32(capacity.block_length);
    line.push_str(" bytes = ");
    line.push_u32(total_mib as u32);
    line.push_str(" MiB");
    console.write_output_line(framebuffer, line.as_str());
}

/// `docs/USB_MSC_PLAN.md` Stage 5, extended by `docs/USB_REFACTOR_PLAN.md` Stage F:
/// read one 512-byte block via SCSI READ(10) from whichever Mass Storage
/// device `UsbHost::rescan` already attached, and dump it, mirroring
/// `cmd_sdread`'s shape (and reusing `sdmmc::dump_block` for the UART hex
/// dump -- the dump format itself has nothing SD-specific about it).
fn cmd_usbread(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
) {
    let Some(lba) = parse_u32(argument) else {
        console.write_output_line(framebuffer, "usage: usbread <lba>");
        return;
    };
    let Some(mass_storage) = usb_host.mass_storage_mut() else {
        console.write_output_line(
            framebuffer,
            "no Mass Storage device attached; plug one in and run 'usbrescan'",
        );
        return;
    };

    // Some drives are not immediately ready to service a data-phase Bulk
    // command right after SET_CONFIGURATION; skipping this made an
    // immediate `usbread` unreliable on real hardware (see
    // `UsbMassStorage::wait_until_ready`'s doc comment).
    console.write_output_line(framebuffer, "waiting for media ready (TEST UNIT READY)...");
    if !mass_storage.wait_until_ready(10) {
        console.write_output_line(
            framebuffer,
            "media not ready after retries, attempting read anyway",
        );
    }

    let mut buffer = [0u8; 512];
    if !mass_storage.read_blocks(lba, &mut buffer) {
        console.write_output_line(framebuffer, "block read failed, see UART log");
        return;
    }
    sdmmc::dump_block(&buffer);

    let mut line = Line::new();
    line.push_str("read LBA ");
    line.push_u32(lba);
    line.push_str(": ");
    for &byte in &buffer[..8] {
        line.push_hex(byte as u32, 2);
        line.push_str(" ");
    }
    line.push_str("...");
    console.write_output_line(framebuffer, line.as_str());

    let boot_signature = buffer[510] == 0x55 && buffer[511] == 0xAA;
    console.write_output_line(
        framebuffer,
        if boot_signature {
            "bytes 510-511 = 55 AA (MBR/boot-sector signature)"
        } else {
            "no 55 AA signature at bytes 510-511"
        },
    );
    console.write_output_line(framebuffer, "full 512-byte hex dump: see UART log");
}

/// `docs/USB_MSC_PLAN.md` Stage 6, extended by `docs/USB_REFACTOR_PLAN.md` Stage F:
/// reads LBA 0 from whichever Mass Storage device `UsbHost::rescan` already
/// attached and hands it to the same `mbr::show` that `cmd_sdmbr` uses, so
/// the two commands print partition tables in an identical format despite
/// reading them through entirely different block-I/O stacks.
fn cmd_usbmbr(console: &mut Console, framebuffer: &mut Framebuffer, usb_host: &mut usb::UsbHost) {
    let Some(mass_storage) = usb_host.mass_storage_mut() else {
        console.write_output_line(
            framebuffer,
            "no Mass Storage device attached; plug one in and run 'usbrescan'",
        );
        return;
    };

    console.write_output_line(framebuffer, "waiting for media ready (TEST UNIT READY)...");
    if !mass_storage.wait_until_ready(10) {
        console.write_output_line(
            framebuffer,
            "media not ready after retries, attempting read anyway",
        );
    }

    let mut sector = [0u8; 512];
    if !mass_storage.read_blocks(0, &mut sector) {
        console.write_output_line(framebuffer, "MBR read failed, see UART log");
        return;
    }
    mbr::show(console, framebuffer, &sector);
}

/// `docs/USB_HOST_PLAN.md` Stage 4-2/4-3, generalized by `docs/USB_REFACTOR_PLAN.md`
/// Stage C: reports the hub `UsbHost::rescan` already opened, and every
/// port's live status alongside which class driver (if any) is attached to
/// it. `Hub::status`/`Hub::port_status` are plain `GET_STATUS` reads, safe
/// to run here without disturbing any attached device's address -- unlike
/// `rescan`, nothing below this command resets the bus.
/// Asks the DWC core itself whether it can do split transactions, so the
/// "FS/LS behind a High-Speed hub is impossible on this chip" claim behind
/// `usb::FORCE_FS_LS_ONLY_HOST` rests on measured silicon rather than only
/// on Espressif's synthesis parameters and docs.
fn cmd_usbhw(console: &mut Console, framebuffer: &mut Framebuffer, usb_host: &usb::UsbHost) {
    if !report_last_probe(console, framebuffer, usb_host) {
        return;
    }
    let hw = usb::probe_split_support();

    let mut line = Line::new();
    line.push_str("GHWCFG1=0x");
    line.push_hex(hw.hwcfg1, 8);
    line.push_str(" 2=0x");
    line.push_hex(hw.hwcfg2, 8);
    line.push_str(" 3=0x");
    line.push_hex(hw.hwcfg3, 8);
    line.push_str(" 4=0x");
    line.push_hex(hw.hwcfg4, 8);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("GHWCFG2.SingPnt (bit5): ");
    line.push_u32(if hw.single_point { 1 } else { 0 });
    line.push_str(if hw.single_point {
        "  = single point: no hub/split in hardware"
    } else {
        "  = multi point: split transactions ARE supported"
    });
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("HCSPLT ch0: wrote 0xFFFFFFFF, read 0x");
    line.push_hex(hw.hcsplt_readback, 8);
    line.push_str("; wrote 0x12345678, read 0x");
    line.push_hex(hw.hcsplt_pattern_readback, 8);
    console.write_output_line(framebuffer, line.as_str());

    console.write_output_line(
        framebuffer,
        if hw.hcsplt_readback == 0 {
            "  -> register not implemented (SSPLIT/CSPLIT impossible)"
        } else {
            "  -> bits stuck; real HCSPLT would read 0x8001FFFF"
        },
    );
}

fn cmd_usbhub(console: &mut Console, framebuffer: &mut Framebuffer, usb_host: &usb::UsbHost) {
    if !report_last_probe(console, framebuffer, usb_host) {
        return;
    }
    let Some(hub) = usb_host.hub() else {
        console.write_output_line(
            framebuffer,
            "no hub attached; plug one into USB-A and run 'usbrescan'",
        );
        return;
    };
    if let Some(summary) = usb_host.hub_summary() {
        console.write_output_line(framebuffer, device_summary_text(summary).as_str());
    }
    let descriptor = &hub.descriptor;

    let mut line = Line::new();
    line.push_str("ports: ");
    line.push_u32(descriptor.port_count as u32);
    line.push_str("  power-good delay: ");
    line.push_u32(descriptor.power_on_to_power_good_ms as u32);
    line.push_str("ms  hub current: ");
    line.push_u32(descriptor.control_current_ma as u32);
    line.push_str("mA");
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("power switching: ");
    line.push_str(match descriptor.power_switching() {
        usb::PowerSwitching::Ganged => "ganged",
        usb::PowerSwitching::PerPort => "per-port",
        usb::PowerSwitching::AlwaysOn => "always on",
    });
    line.push_str("  over-current: ");
    line.push_str(match descriptor.over_current_protection() {
        usb::OverCurrentProtection::Global => "global",
        usb::OverCurrentProtection::PerPort => "per-port",
        usb::OverCurrentProtection::Unsupported => "none",
    });
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("compound: ");
    line.push_str(if descriptor.is_compound_device() {
        "yes"
    } else {
        "no"
    });
    line.push_str("  indicators: ");
    line.push_str(if descriptor.has_port_indicators() {
        "yes"
    } else {
        "no"
    });
    line.push_str("  TT think time: ");
    line.push_u32(descriptor.tt_think_time_bits() as u32);
    line.push_str(" FS bits  hubdesc: ");
    line.push_u32(descriptor.descriptor_len as u32);
    line.push_str("b");
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("removable ports:");
    let mut any_removable = false;
    for port in 1..=descriptor.port_count {
        if descriptor.port_is_removable(port) {
            any_removable = true;
            line.push_str(" ");
            line.push_u32(port as u32);
        }
    }
    if !any_removable {
        line.push_str(" none (all permanently attached)");
    }
    console.write_output_line(framebuffer, line.as_str());

    let Some(status) = hub.status() else {
        console.write_output_line(framebuffer, "hub GET_STATUS failed, see UART log");
        return;
    };
    let mut line = Line::new();
    line.push_str("hub status: local power ");
    line.push_str(if status.local_power_lost() {
        "lost"
    } else {
        "good"
    });
    line.push_str(", over-current ");
    line.push_str(if status.over_current() { "YES" } else { "no" });
    if status.local_power_changed() || status.over_current_changed() {
        line.push_str("  (change bits set: 0x");
        line.push_hex(status.change as u32, 4);
        line.push_str(")");
    }
    console.write_output_line(framebuffer, line.as_str());

    // Live per-port status (safe: a plain GET_STATUS, not a reset) next to
    // whichever slot `rescan` attached there, if any.
    for port in 1..=descriptor.port_count.min(usb::MAX_HUB_PORTS) {
        let Some(status) = hub.port_status(port) else {
            let mut line = Line::new();
            line.push_str("port ");
            line.push_u32(port as u32);
            line.push_str(": GET_PORT_STATUS failed, see UART log");
            console.write_output_line(framebuffer, line.as_str());
            break;
        };
        let mut line = port_status_text(port, &status);
        if let Some(device) = usb_host
            .attached_devices()
            .find(|device| device.location == usb::Location::HubPort(port))
        {
            line.push_str("  [");
            line.push_str(match device.kind {
                usb::DeviceKind::Keyboard(_) => "keyboard",
                usb::DeviceKind::MassStorage(_) => "mass storage",
            });
            line.push_str("]");
        }
        console.write_output_line(framebuffer, line.as_str());
    }
    if descriptor.port_count > usb::MAX_HUB_PORTS {
        console.write_output_line(
            framebuffer,
            "(ports beyond the tracked limit are not shown; see UART log)",
        );
    }
}

fn port_status_text(port: u8, status: &usb::PortStatus) -> Line {
    let mut line = Line::new();
    line.push_str("port ");
    line.push_u32(port as u32);
    line.push_str(": ");
    line.push_str(if status.connected() { "conn " } else { "---- " });
    line.push_str(if status.powered() { "pwr " } else { "--- " });
    line.push_str(if status.enabled() { "ena " } else { "--- " });
    line.push_str(if status.suspended() { "susp " } else { "" });
    line.push_str(if status.in_reset() { "rst " } else { "" });
    line.push_str(if status.over_current() {
        "OVERCURRENT "
    } else {
        ""
    });
    if status.connected() {
        line.push_str(speed_text(status.speed()));
        line.push_str(" ");
    }
    line.push_str("st=0x");
    line.push_hex(status.status as u32, 4);
    line.push_str(" chg=0x");
    line.push_hex(status.change as u32, 4);
    line
}

fn cmd_usbvbus(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let (bit_text, rest) = split_first_word(argument);
    let state = trim(rest);
    let Some(bit) = parse_u32(bit_text) else {
        console.write_output_line(framebuffer, "usage: usbvbus <0-7> on|off");
        return;
    };
    if bit > 7 {
        console.write_output_line(framebuffer, "bit must be 0-7");
        return;
    }
    let on = match state {
        b"on" => true,
        b"off" => false,
        _ => {
            console.write_output_line(framebuffer, "usage: usbvbus <0-7> on|off");
            return;
        }
    };
    if usb::set_vbus_bit(bit as u8, on) {
        console.write_output_line(
            framebuffer,
            "ok; check USB-A 5V with a meter/current tester",
        );
    } else {
        console.write_output_line(framebuffer, "I2C write failed (PI4IOE2 @ 0x44 not acked)");
    }
}

fn cmd_backlight(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    match argument {
        b"on" => {
            lcd::set_backlight(true);
            console.write_output_line(framebuffer, "backlight on");
        }
        b"off" => {
            lcd::set_backlight(false);
            console.write_output_line(framebuffer, "backlight off");
        }
        _ => console.write_output_line(framebuffer, "usage: backlight on|off"),
    }
}

/// Reports, and optionally retunes, the interconnect arbitration that decides
/// whether DSI scanout reads beat CPU and cache traffic to PSRAM.
///
/// Losing that race empties the DSI bridge FIFO and paints the rest of the
/// frame light blue, so the underrun count reported here is the direct
/// pass/fail measure for any value tried. Setting the fields at runtime avoids
/// a reflash per experiment, which matters because the arbitration priority
/// field's polarity is not documented in the register description.
fn cmd_icm(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    if !argument.is_empty() {
        let (head, rest) = split_first_word(argument);
        let (Some(priority), Some(arqos)) = (parse_u32(head), parse_u32(trim(rest))) else {
            console.write_output_line(framebuffer, "usage: icm [priority arqos]");
            return;
        };
        if priority > 15 || arqos > 15 {
            console.write_output_line(framebuffer, "priority and arqos must be 0-15");
            return;
        }
        icm::set_display_priority(priority, arqos);
    }

    let status = icm::status();
    let mut line = Line::new();
    line.push_str("clk_en: 0x");
    line.push_hex(status.clock_enable, 8);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("mst_arb_priority: 0x");
    line.push_hex(status.master_priority, 8);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("mst_arqos: 0x");
    line.push_hex(status.master_arqos, 8);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("mst_awqos: 0x");
    line.push_hex(status.master_awqos, 8);
    console.write_output_line(framebuffer, line.as_str());

    console.write_output_line(
        framebuffer,
        "(DW-GDMA bits 12-19, 2D-DMA bits 8-11, in all three)",
    );

    let mut line = Line::new();
    line.push_str("DPI FIFO underruns: ");
    line.push_u32(lcd::underrun_count());
    console.write_output_line(framebuffer, line.as_str());
}

/// Fills one rectangle, by DMA or by CPU, and reports how long it took.
///
/// The two paths exist side by side because that is the only way to answer the
/// two questions this stage has. Whether the PPA writes the right pixels is
/// decided by putting the same rectangle on the panel both ways and seeing no
/// difference; where the crossover between them lies is decided by the times,
/// and it has to be measured rather than assumed -- a DMA that has to be set
/// up, started and waited for loses to a store loop below some size.
fn cmd_ppafill(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    const USAGE: &str = "usage: ppafill <x> <y> <w> <h> <color> | ppafill sweep";
    if trim(argument) == b"sweep" {
        cmd_ppafill_sweep(console, framebuffer);
        return;
    }
    let (x, rest) = split_first_word(argument);
    let (y, rest) = split_first_word(trim(rest));
    let (width, rest) = split_first_word(trim(rest));
    let (height, rest) = split_first_word(trim(rest));
    let (color, rest) = split_first_word(trim(rest));
    let (Some(x), Some(y), Some(width), Some(height), Some(color)) = (
        parse_u32(x),
        parse_u32(y),
        parse_u32(width),
        parse_u32(height),
        parse_number(color),
    ) else {
        console.write_output_line(framebuffer, USAGE);
        return;
    };
    let use_cpu = match trim(rest) {
        b"" => false,
        b"cpu" => true,
        _ => {
            console.write_output_line(framebuffer, USAGE);
            return;
        }
    };
    if color > u16::MAX as u32 {
        console.write_output_line(framebuffer, "color must be a 16-bit RGB565 value");
        return;
    }
    let (x, y) = (x as usize, y as usize);
    let (width, height) = (width as usize, height as usize);
    let color = color as u16;

    let start = membench::cycles();
    let filled = if use_cpu {
        framebuffer.fill_rect(x, y, width, height, color);
        framebuffer.flush_rect(x, y, width, height)
    } else {
        framebuffer.ppa_fill_rect(x, y, width, height, color)
    };
    let elapsed = membench::cycles().wrapping_sub(start);

    if !filled {
        console.write_output_line(framebuffer, "ppafill: fill failed or rectangle is empty");
        return;
    }

    let microseconds = ((elapsed as u64) * 1_000_000 / startup::cpu_hz() as u64) as u32;
    let mut line = Line::new();
    line.push_str("ppafill: ");
    line.push_u32(width as u32);
    line.push_str("x");
    line.push_u32(height as u32);
    line.push_str(if use_cpu {
        " by CPU in "
    } else {
        " by PPA in "
    });
    line.push_u32(microseconds);
    line.push_str(" us");
    console.write_output_line(framebuffer, line.as_str());
}

/// Trims leading and trailing spaces (the only whitespace current keyboard input or
/// command output ever contains).
fn trim(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start] == b' ' {
        start += 1;
    }
    while end > start && bytes[end - 1] == b' ' {
        end -= 1;
    }
    &bytes[start..end]
}

/// Parses a plain decimal (no sign, no whitespace) argument.
fn parse_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    let mut value: u32 = 0;
    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add((byte - b'0') as u32)?;
    }
    Some(value)
}

/// Times both fill paths across a range of rectangle sizes, so the size at
/// which the DMA starts winning is measured rather than guessed.
///
/// That number is what `Framebuffer::fill_rect` needs in order to route: below
/// the crossover a store loop beats setting up, starting and waiting for a
/// transfer, and the console's own repaints -- one 12x16 cell at a time -- sit
/// firmly in that region. Sizes here run from exactly that cell up to the full
/// screen. Each is repeated, because scanout is still running and a single
/// short fill lands wherever it happens to land against that traffic.
fn cmd_ppafill_sweep(console: &mut Console, framebuffer: &mut Framebuffer) {
    /// Logical width x height. The first is one console cell.
    const SIZES: &[(usize, usize)] = &[
        (12, 16),
        (24, 32),
        (48, 64),
        (96, 128),
        (192, 256),
        (384, 512),
        (768, 640),
        (crate::framebuffer::WIDTH, crate::framebuffer::HEIGHT),
    ];
    const REPEATS: u32 = 8;

    // Measure everything before reporting anything. The larger sizes paint
    // over the console, so restoring it has to happen once at the end --
    // repainting between sizes would both erase the results already printed
    // and charge each size for the repaint.
    let mut results = Vec::new();
    for &(width, height) in SIZES {
        let ppa = time_fills(framebuffer, width, height, REPEATS, false);
        let cpu = time_fills(framebuffer, width, height, REPEATS, true);
        results.push((width, height, ppa, cpu));
    }
    console.clear(framebuffer);

    const NAME_COLUMNS: usize = 12;
    const VALUE_COLUMNS: usize = 10;

    let mut header = Line::new();
    header.push_str("size");
    pad_to(&mut header, NAME_COLUMNS);
    push_right_str(&mut header, "ppa", VALUE_COLUMNS);
    push_right_str(&mut header, "cpu", VALUE_COLUMNS);
    header.push_str("   (us per fill)");
    console.write_output_line(framebuffer, header.as_str());

    for (width, height, ppa, cpu) in results {
        let mut line = Line::new();
        line.push_u32(width as u32);
        line.push_str("x");
        line.push_u32(height as u32);
        pad_to(&mut line, NAME_COLUMNS);
        for value in [ppa, cpu] {
            match value {
                Some(microseconds) => {
                    let mut digits = Line::new();
                    digits.push_u32(microseconds);
                    push_right_str(&mut line, digits.as_str(), VALUE_COLUMNS);
                }
                None => push_right_str(&mut line, "n/a", VALUE_COLUMNS),
            }
        }
        console.write_output_line(framebuffer, line.as_str());
    }
}

/// Runs one size `repeats` times and returns the mean in microseconds, or
/// `None` if a fill was refused.
fn time_fills(
    framebuffer: &mut Framebuffer,
    width: usize,
    height: usize,
    repeats: u32,
    use_cpu: bool,
) -> Option<u32> {
    // Alternate the colour so a repeat cannot be optimised away anywhere in
    // the path and so a stuck fill is visible on the panel.
    let start = membench::cycles();
    for index in 0..repeats {
        let color = if index % 2 == 0 {
            crate::framebuffer::BLACK
        } else {
            crate::framebuffer::BLUE
        };
        if use_cpu {
            framebuffer.fill_rect(0, 0, width, height, color);
            if !framebuffer.flush_rect(0, 0, width, height) {
                return None;
            }
        } else if !framebuffer.ppa_fill_rect(0, 0, width, height, color) {
            return None;
        }
    }
    let elapsed = membench::cycles().wrapping_sub(start);
    let total = (elapsed as u64) * 1_000_000 / startup::cpu_hz() as u64;
    Some((total / repeats as u64) as u32)
}

/// Pads a line with spaces out to `columns`, for table layout in the
/// console's fixed-width cells.
fn pad_to(line: &mut Line, columns: usize) {
    while line.as_str().len() < columns {
        line.push_str(" ");
    }
}

/// Appends `text` right-aligned in a field `columns` wide.
fn push_right_str(line: &mut Line, text: &str, columns: usize) {
    for _ in text.len()..columns {
        line.push_str(" ");
    }
    line.push_str(text);
}

/// Parses a decimal value, or a hexadecimal one written with a `0x` prefix.
/// Colours are the reason: RGB565 constants are only recognisable in hex.
fn parse_number(bytes: &[u8]) -> Option<u32> {
    let Some(digits) = bytes
        .strip_prefix(b"0x")
        .or_else(|| bytes.strip_prefix(b"0X"))
    else {
        return parse_u32(bytes);
    };
    if digits.is_empty() {
        return None;
    }
    let mut value: u32 = 0;
    for &byte in digits {
        let digit = (byte as char).to_digit(16)?;
        value = value.checked_mul(16)?.checked_add(digit)?;
    }
    Some(value)
}

/// Splits off the command name from its (untrimmed) argument text.
fn split_first_word(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&byte| byte == b' ') {
        Some(index) => (&bytes[..index], &bytes[index + 1..]),
        None => (bytes, b""),
    }
}

/// Command-line bytes only ever hold what `Console::push` accepted (printable
/// ASCII or space), so this is always valid UTF-8; the empty fallback is
/// unreachable in practice but keeps this infallible.
fn as_str(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("")
}

/// Small stack-allocated line builder: this crate has no allocator and
/// deliberately avoids `core::fmt`, so command output is assembled a few
/// bytes at a time instead.
/// Fixed-buffer ASCII line builder shared by every command's output
/// formatting -- `pub(crate)` (rather than local to `shell.rs`) so
/// `mbr.rs` can format its own output lines the same way, without either
/// module owning the other's display concerns.
pub(crate) struct Line {
    buffer: [u8; 80],
    len: usize,
}

impl Line {
    pub(crate) fn new() -> Self {
        Self {
            buffer: [0; 80],
            len: 0,
        }
    }

    pub(crate) fn push_str(&mut self, text: &str) {
        for byte in text.bytes() {
            if self.len < self.buffer.len() {
                self.buffer[self.len] = byte;
                self.len += 1;
            }
        }
    }

    pub(crate) fn push_u32(&mut self, value: u32) {
        if value == 0 {
            self.push_str("0");
            return;
        }
        let mut digits = [0u8; 10];
        let mut count = 0;
        let mut remaining = value;
        while remaining > 0 {
            digits[count] = b'0' + (remaining % 10) as u8;
            remaining /= 10;
            count += 1;
        }
        for &digit in digits[..count].iter().rev() {
            if self.len < self.buffer.len() {
                self.buffer[self.len] = digit;
                self.len += 1;
            }
        }
    }

    /// Pushes raw bytes as ASCII, substituting `.` for anything outside
    /// printable-graphic-or-space -- mirrors `sdmmc::dump_block_at`'s ASCII
    /// column. Used for SCSI INQUIRY vendor/product/revision fields, which
    /// are device-supplied and not guaranteed clean.
    pub(crate) fn push_ascii(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let ch = if byte.is_ascii_graphic() || byte == b' ' {
                byte
            } else {
                b'.'
            };
            if self.len < self.buffer.len() {
                self.buffer[self.len] = ch;
                self.len += 1;
            }
        }
    }

    pub(crate) fn push_hex(&mut self, value: u32, digits: u32) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for index in 0..digits {
            let nibble = (value >> (4 * (digits - 1 - index))) & 0xF;
            if self.len < self.buffer.len() {
                self.buffer[self.len] = HEX[nibble as usize];
                self.len += 1;
            }
        }
    }

    /// All bytes ever written come from ASCII literals or the digit/hex
    /// tables above, so this is always valid UTF-8.
    pub(crate) fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buffer[..self.len]).unwrap_or("")
    }
}
