//! Command dispatcher for the CardKB console.
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

use crate::console::Console;
use crate::framebuffer::{BLUE, CYAN, GREEN, MAGENTA, RED, WHITE, YELLOW};
use crate::{interrupts, lcd, psram, sdmmc, startup, uart, usb};

/// Roughly the panel's vsync rate; used only for the coarse `uptime` command.
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
    HelpEntry { name: "help", usage: "help [command]", lines: &["list commands, or describe one"] },
    HelpEntry { name: "clear", usage: "clear", lines: &["clear the screen"] },
    HelpEntry { name: "echo", usage: "echo <text>", lines: &["print text back"] },
    HelpEntry { name: "about", usage: "about", lines: &["firmware banner"] },
    HelpEntry { name: "mem", usage: "mem", lines: &["PSRAM/RAM usage"] },
    HelpEntry {
        name: "alloctest",
        usage: "alloctest <MiB>",
        lines: &["allocate N MiB on the PSRAM heap, verify read/write"],
    },
    HelpEntry { name: "uptime", usage: "uptime", lines: &["time since boot"] },
    HelpEntry { name: "backlight", usage: "backlight on|off", lines: &["LCD backlight"] },
    HelpEntry {
        name: "color",
        usage: "color <name>",
        lines: &[
            "change header color",
            "green/red/blue/cyan/magenta/yellow/white",
        ],
    },
    HelpEntry { name: "paint", usage: "paint", lines: &["touch drawing screen"] },
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
            "bring up USB-A, enumerate the attached device (VID/PID,",
            "HID Boot keyboard interface if found); plug in a device first",
        ],
    },
    HelpEntry {
        name: "usbvbus",
        usage: "usbvbus <0-7> on|off",
        lines: &[
            "raw PI4IOE2 (0x44) output-bit toggle; bit 3 = USB-A VBUS",
            "mainly useful for diagnostics; usbinfo drives bit 3 itself",
        ],
    },
    HelpEntry {
        name: "usbhub",
        usage: "usbhub",
        lines: &[
            "bring up USB-A, enumerate an attached USB hub, power its ports,",
            "then reset the first occupied one and enumerate what is on it",
        ],
    },
    HelpEntry { name: "reboot", usage: "reboot", lines: &["restart the board"] },
];

/// What the display loop in `lcd::run_console` should do once a command has
/// been dispatched.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Outcome {
    /// Keep running the console; write a fresh prompt.
    Continue,
    /// Reboot once this frame's output has reached the panel.
    Reboot,
    /// Hand the display over to the touch paint screen.
    Paint,
}

/// Parses and runs one command line, returning what the caller should do
/// next.
pub fn execute(console: &mut Console, line: &[u8]) -> Outcome {
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
        b"help" => cmd_help(console, argument),
        b"clear" => console.clear(),
        b"echo" => console.write_output_line(as_str(argument)),
        b"about" | b"version" => console.write_output_line("Tab5 CardKB Shell 0.1"),
        b"mem" => cmd_mem(console),
        b"alloctest" => cmd_alloctest(console, argument),
        b"uptime" => cmd_uptime(console),
        b"backlight" => cmd_backlight(console, argument),
        b"color" => cmd_color(console, argument),
        b"sdinfo" => cmd_sdinfo(console),
        b"sdread" => cmd_sdread(console, argument),
        b"sdreadn" => cmd_sdreadn(console, argument),
        b"sdwritetest" => cmd_sdwritetest(console, argument),
        b"sdzero" => cmd_sdzero(console, argument),
        b"sdmbr" => cmd_sdmbr(console),
        b"sdreadpsram" => cmd_sdreadpsram(console, argument),
        b"usbinfo" => cmd_usbinfo(console),
        b"usbvbus" => cmd_usbvbus(console, argument),
        b"usbhub" => cmd_usbhub(console),
        b"paint" => return Outcome::Paint,
        b"reboot" | b"reset" => {
            console.write_output_line("rebooting...");
            return Outcome::Reboot;
        }
        _ => console.write_output_line("unknown command (try 'help')"),
    }
    Outcome::Continue
}

/// Reboots the board. The caller must have already flushed the "rebooting..."
/// output to the panel; this never returns.
pub fn reboot() -> ! {
    startup::reboot()
}

/// With no argument, lists command names only; with a command name, shows
/// its usage and description. `write_output_line` wraps at the console's
/// column width on its own, so the name list can just be one long line.
fn cmd_help(console: &mut Console, argument: &[u8]) {
    if argument.is_empty() {
        console.write_output_line("commands (help <name> for details):");
        let mut names = String::new();
        for (index, entry) in HELP_ENTRIES.iter().enumerate() {
            if index > 0 {
                names.push(' ');
            }
            names.push_str(entry.name);
        }
        console.write_output_line(&names);
        return;
    }

    let name = as_str(argument);
    match HELP_ENTRIES.iter().find(|entry| entry.name == name) {
        Some(entry) => {
            console.write_output_line(entry.usage);
            for line in entry.lines {
                console.write_output_line(line);
            }
        }
        None => console.write_output_line("unknown command (try 'help')"),
    }
}

fn cmd_mem(console: &mut Console) {
    let mut line = Line::new();
    line.push_str("PSRAM window: ");
    line.push_u32(psram::MAPPED_BYTES as u32);
    line.push_str(" bytes");
    console.write_output_line(line.as_str());

    let mut line = Line::new();
    line.push_str("framebuffer: ");
    line.push_u32(psram::FRAMEBUFFER_BYTES as u32);
    line.push_str(" bytes x");
    line.push_u32(psram::FRAMEBUFFER_COUNT as u32);
    console.write_output_line(line.as_str());

    let mut line = Line::new();
    line.push_str("heap: ");
    line.push_u32((heap_bytes() / (1024 * 1024)) as u32);
    line.push_str(" MiB");
    console.write_output_line(line.as_str());
}

/// Bytes of PSRAM past the two framebuffers, matching `Psram::heap`'s split
/// and backing the global allocator installed in `main`.
fn heap_bytes() -> usize {
    psram::MAPPED_BYTES - psram::FRAMEBUFFER_COUNT * psram::FRAMEBUFFER_BYTES
}

/// Allocates `mib` MiB from the PSRAM-backed global allocator, fills it with
/// a per-byte pattern derived from its index, reads it back and reports any
/// mismatch. Uses `try_reserve_exact` so a too-large request reports failure
/// instead of aborting the firmware.
fn cmd_alloctest(console: &mut Console, argument: &[u8]) {
    let Some(mib) = parse_u32(argument) else {
        console.write_output_line("usage: alloctest <MiB>");
        return;
    };
    if mib == 0 {
        console.write_output_line("MiB must be at least 1");
        return;
    }
    let bytes = mib as usize * 1024 * 1024;

    let mut line = Line::new();
    line.push_str("allocating ");
    line.push_u32(mib);
    line.push_str(" MiB (heap has ");
    line.push_u32((heap_bytes() / (1024 * 1024)) as u32);
    line.push_str(" MiB)...");
    console.write_output_line(line.as_str());

    let mut buffer: Vec<u8> = Vec::new();
    if buffer.try_reserve_exact(bytes).is_err() {
        console.write_output_line("allocation failed (not enough contiguous heap)");
        return;
    }
    buffer.resize(bytes, 0);

    console.write_output_line("writing pattern...");
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = pattern_byte(index);
    }

    console.write_output_line("verifying...");
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
    console.write_output_line(line.as_str());
}

/// A well-mixed byte per index so nearby or aliased addresses are unlikely
/// to share a value; a plain `index as u8` would just repeat every 256 bytes.
fn pattern_byte(index: usize) -> u8 {
    ((index as u32).wrapping_mul(2_654_435_761) >> 24) as u8
}

fn cmd_uptime(console: &mut Console) {
    let seconds = interrupts::frame_sequence() / FRAMES_PER_SECOND;
    let mut line = Line::new();
    line.push_str("uptime: ~");
    line.push_u32(seconds);
    line.push_str(" s (frame-counted)");
    console.write_output_line(line.as_str());
}

fn cmd_sdinfo(console: &mut Console) {
    console.write_output_line("activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line("SD card activation failed, see UART log");
        return;
    };

    let mut line = Line::new();
    line.push_str("RCA: 0x");
    line.push_hex(card.rca as u32, 4);
    line.push_str("  manufacturer ID: 0x");
    line.push_hex(card.cid[3] >> 24, 2);
    console.write_output_line(line.as_str());

    let mut line = Line::new();
    line.push_str("type: ");
    line.push_str(if card.high_capacity {
        "SDHC/SDXC"
    } else {
        "SDSC"
    });
    console.write_output_line(line.as_str());

    let mut line = Line::new();
    match card.capacity_bytes {
        Some(bytes) => {
            line.push_str("capacity: ~");
            line.push_u32((bytes / (1024 * 1024)) as u32);
            line.push_str(" MiB");
        }
        None => line.push_str("capacity: unknown (CSD v1, not decoded)"),
    }
    console.write_output_line(line.as_str());

    // CSD PERM_WRITE_PROTECT (bit 13) / TMP_WRITE_PROTECT (bit 12), common to
    // both CSD structure versions.
    let write_protected = card.csd[0] & (0b11 << 12) != 0;
    console.write_output_line(if write_protected {
        "write-protected: yes"
    } else {
        "write-protected: no"
    });
    console.write_output_line(if card.bus_width_4bit {
        "bus width: 4-bit"
    } else {
        "bus width: 1-bit (ACMD6 failed or skipped)"
    });
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
    console.write_output_line(line.as_str());
    console.write_output_line("full CID/CSD dump: see UART log");
}

fn cmd_sdread(console: &mut Console, argument: &[u8]) {
    let Some(lba) = parse_u32(argument) else {
        console.write_output_line("usage: sdread <lba>");
        return;
    };

    console.write_output_line("activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line("SD card activation failed, see UART log");
        return;
    };

    let mut buffer = [0u8; 512];
    if !sdmmc::read_block(&card, lba, &mut buffer) {
        console.write_output_line("block read failed, see UART log");
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
    console.write_output_line(line.as_str());

    let boot_signature = buffer[510] == 0x55 && buffer[511] == 0xAA;
    console.write_output_line(if boot_signature {
        "bytes 510-511 = 55 AA (MBR/boot-sector signature)"
    } else {
        "no 55 AA signature at bytes 510-511"
    });
    console.write_output_line("full 512-byte hex dump: see UART log");
}

const MAX_MULTI_BLOCKS: u32 = 8;

fn cmd_sdreadn(console: &mut Console, argument: &[u8]) {
    let (lba_text, count_text) = split_first_word(argument);
    let (Some(lba), Some(count)) = (parse_u32(lba_text), parse_u32(trim(count_text))) else {
        console.write_output_line("usage: sdreadn <lba> <count>");
        return;
    };
    if count == 0 || count > MAX_MULTI_BLOCKS {
        console.write_output_line("count must be 1..=8");
        return;
    }

    console.write_output_line("activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line("SD card activation failed, see UART log");
        return;
    };

    let mut buffer = [0u8; 512 * MAX_MULTI_BLOCKS as usize];
    let region = &mut buffer[..512 * count as usize];
    if !sdmmc::read_blocks(&card, lba, region) {
        console.write_output_line("multi-block read failed, see UART log");
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
    console.write_output_line(line.as_str());
    console.write_output_line("full hex dump: see UART log");
}

fn cmd_sdwritetest(console: &mut Console, argument: &[u8]) {
    let Some(lba) = parse_u32(argument) else {
        console.write_output_line("usage: sdwritetest <lba>");
        return;
    };

    console.write_output_line("WARNING: temporarily overwrites 1 block, then restores it");
    console.write_output_line("activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line("SD card activation failed, see UART log");
        return;
    };

    let mut original = [0u8; 512];
    if !sdmmc::read_blocks(&card, lba, &mut original) {
        console.write_output_line("could not read original block, aborting (nothing written)");
        return;
    }

    let mut pattern = [0u8; 512];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = (index as u8) ^ 0xA5;
    }

    if !sdmmc::write_blocks(&card, lba, &mut pattern) {
        console.write_output_line("pattern write failed, see UART log");
        return;
    }
    let mut verify = [0u8; 512];
    let pattern_ok =
        sdmmc::read_blocks(&card, lba, &mut verify) && verify == pattern;
    console.write_output_line(if pattern_ok {
        "pattern write+read-back: match"
    } else {
        "pattern write+read-back: MISMATCH, see UART log"
    });

    let mut restore = original;
    let restored = sdmmc::write_blocks(&card, lba, &mut restore);
    let mut check = [0u8; 512];
    let restore_ok = restored && sdmmc::read_blocks(&card, lba, &mut check) && check == original;
    console.write_output_line(if restore_ok {
        "original data restored: yes"
    } else {
        "original data restored: NO -- see UART log, LBA may be corrupted"
    });
}

fn cmd_sdzero(console: &mut Console, argument: &[u8]) {
    let Some(lba) = parse_u32(argument) else {
        console.write_output_line("usage: sdzero <lba>");
        return;
    };

    console.write_output_line("activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line("SD card activation failed, see UART log");
        return;
    };

    let mut zero = [0u8; 512];
    if !sdmmc::write_blocks(&card, lba, &mut zero) {
        console.write_output_line("zero write failed, see UART log");
        return;
    }
    console.write_output_line("block zeroed");
}

/// Classic MBR layout: 4 fixed 16-byte partition entries at offset 446,
/// each `[boot flag, 3 CHS bytes, type, 3 CHS bytes, start LBA (u32 LE),
/// sector count (u32 LE)]`, followed by the `55 AA` signature at 510-511.
/// Does not look past the MBR itself -- no GPT, no filesystem parsing.
fn cmd_sdmbr(console: &mut Console) {
    console.write_output_line("activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line("SD card activation failed, see UART log");
        return;
    };

    let mut mbr = [0u8; 512];
    if !sdmmc::read_block(&card, 0, &mut mbr) {
        console.write_output_line("MBR read failed, see UART log");
        return;
    }

    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        console.write_output_line("no 55 AA boot signature at LBA 0; not a valid MBR");
        return;
    }

    let mut any_entry = false;
    for entry in 0..4usize {
        let offset = 446 + entry * 16;
        let partition_type = mbr[offset + 4];
        if partition_type == 0 {
            continue;
        }
        any_entry = true;
        let boot = mbr[offset];
        let start_lba = u32::from_le_bytes(mbr[offset + 8..offset + 12].try_into().unwrap());
        let sectors = u32::from_le_bytes(mbr[offset + 12..offset + 16].try_into().unwrap());
        let size_mib = (sectors as u64) * 512 / (1024 * 1024);

        let mut line = Line::new();
        line.push_str("#");
        line.push_u32((entry + 1) as u32);
        line.push_str(if boot == 0x80 { " * type 0x" } else { "   type 0x" });
        line.push_hex(partition_type as u32, 2);
        line.push_str(" ");
        line.push_str(partition_type_name(partition_type));
        console.write_output_line(line.as_str());

        let mut line = Line::new();
        line.push_str("   start LBA ");
        line.push_u32(start_lba);
        line.push_str(", ");
        line.push_u32(size_mib as u32);
        line.push_str(" MiB");
        console.write_output_line(line.as_str());

        if partition_type == 0xEE {
            console.write_output_line("   (GPT protective MBR; GPT itself not parsed)");
        }
    }

    if !any_entry {
        console.write_output_line("no partition entries (all empty)");
    }
}

/// Short names for common partition type bytes; not exhaustive.
fn partition_type_name(partition_type: u8) -> &'static str {
    match partition_type {
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0E => "FAT16",
        0x0B | 0x0C => "FAT32",
        0x05 | 0x0F => "Extended",
        0x07 => "NTFS/exFAT",
        0x82 => "Linux swap",
        0x83 => "Linux",
        0xEE => "GPT protective",
        0xEF => "EFI System",
        _ => "unknown",
    }
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
fn cmd_sdreadpsram(console: &mut Console, argument: &[u8]) {
    let (lba_text, count_text) = split_first_word(argument);
    let (Some(lba), Some(count)) = (parse_u32(lba_text), parse_u32(trim(count_text))) else {
        console.write_output_line("usage: sdreadpsram <lba> <count>");
        return;
    };
    if count == 0 || count > MAX_MULTI_BLOCKS {
        console.write_output_line("count must be 1..=8");
        return;
    }
    let bytes = 512 * count as usize;

    console.write_output_line("activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line("SD card activation failed, see UART log");
        return;
    };

    let mut sram_reference = [0u8; 512 * MAX_MULTI_BLOCKS as usize];
    let sram_region = &mut sram_reference[..bytes];
    if !sdmmc::read_blocks(&card, lba, sram_region) {
        console.write_output_line("SRAM reference read failed, see UART log");
        return;
    }

    let mut psram_buffer: Vec<u8> = Vec::new();
    if psram_buffer.try_reserve_exact(bytes).is_err() {
        console.write_output_line("PSRAM allocation failed (not enough contiguous heap)");
        return;
    }
    psram_buffer.resize(bytes, 0);

    console.write_output_line("DMA-ing the same blocks directly into PSRAM...");
    if !sdmmc::read_blocks(&card, lba, &mut psram_buffer) {
        console.write_output_line("PSRAM DMA read failed, see UART log");
        return;
    }

    if psram_buffer.as_slice() == sram_region {
        console.write_output_line("match: SD -> PSRAM DMA works, same bytes as SD -> SRAM");
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
        console.write_output_line(line.as_str());
        console.write_output_line("SD -> PSRAM DMA does not work as-is");
    }
    console.write_output_line("first block of the SRAM reference, for reference:");
    sdmmc::dump_block_at((&sram_region[..512]).try_into().unwrap(), 0);
}

/// Shared front half of `usbinfo` and `usbhub`: runs the full host port
/// bring-up and reports what came back, returning true only if the port
/// ended up enabled and ready for control transfers.
fn report_host_port(console: &mut Console) -> bool {
    console.write_output_line("probing USB-A host port (USB-DWC HS)...");
    let port = usb::probe_port();

    console.write_output_line(if port.vbus_enable_acked {
        "VBUS enable: I2C ok"
    } else {
        "VBUS enable: I2C not acked (PI4IOE2 @ 0x44 not responding)"
    });

    if !port.core_alive {
        let mut line = Line::new();
        line.push_str("DWC core not responding, GSNPSID=0x");
        line.push_hex(port.core_id, 8);
        console.write_output_line(line.as_str());
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
    console.write_output_line(line.as_str());

    // The speed line below only means what it says once it is clear
    // whether the host was allowed to negotiate High-Speed at all.
    console.write_output_line(if usb::FORCE_FS_LS_ONLY_HOST {
        "host: FS/LS-only forced (HCFG.FSLSSupp, for hub support)"
    } else {
        "host: High-Speed capable"
    });

    if !port.connected {
        console.write_output_line("no device detected (plug in USB-A and retry)");
        return false;
    }
    console.write_output_line(if port.enabled {
        "device connected, port reset and enabled"
    } else {
        "device connected, but port did not enable after reset"
    });
    if !port.enabled {
        return false;
    }
    let mut line = Line::new();
    line.push_str("speed: ");
    line.push_str(speed_text(port.speed));
    console.write_output_line(line.as_str());
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

fn cmd_usbinfo(console: &mut Console) {
    if !report_host_port(console) {
        return;
    }

    console.write_output_line("enumerating device (control transfers)...");
    // Nothing plugged into USB-A directly ever needs preambles; see
    // `usb::connect_keyboard`.
    let Some(device) = usb::enumerate_device(usb::ROOT_DEVICE_ADDRESS, false) else {
        console.write_output_line("enumeration failed, see UART log");
        return;
    };

    let mut line = Line::new();
    line.push_str("VID:PID = 0x");
    line.push_hex(device.vendor_id as u32, 4);
    line.push_str(":0x");
    line.push_hex(device.product_id as u32, 4);
    line.push_str("  EP0 MPS: ");
    line.push_u32(device.max_packet_size0 as u32);
    console.write_output_line(line.as_str());

    let mut line = Line::new();
    line.push_str("class ");
    line.push_hex(device.device_class as u32, 2);
    line.push_str("/");
    line.push_hex(device.device_subclass as u32, 2);
    line.push_str("/");
    line.push_hex(device.device_protocol as u32, 2);
    line.push_str("  configs: ");
    line.push_u32(device.num_configurations as u32);
    line.push_str("  interfaces: ");
    line.push_u32(device.num_interfaces as u32);
    line.push_str("  config bytes: ");
    line.push_u32(device.config_total_length as u32);
    console.write_output_line(line.as_str());

    match usb::find_hid_keyboard(device.config_bytes()) {
        Some(hid) => {
            let mut line = Line::new();
            line.push_str("HID Boot keyboard: interface ");
            line.push_u32(hid.interface_number as u32);
            line.push_str(", EP 0x");
            line.push_hex(hid.endpoint_address as u32, 2);
            line.push_str(", MPS ");
            line.push_u32(hid.max_packet_size as u32);
            line.push_str(", interval ");
            line.push_u32(hid.interval as u32);
            console.write_output_line(line.as_str());
        }
        None => console.write_output_line("no HID Boot keyboard interface found"),
    }
}

/// `USB_HOST_PLAN.md` Stage 4-2: enumerate an attached hub and dump its
/// class descriptor and status. Port power-up and downstream devices are
/// Stage 4-3 onwards.
fn cmd_usbhub(console: &mut Console) {
    if !report_host_port(console) {
        return;
    }

    console.write_output_line("enumerating hub (control transfers)...");
    let Some(device) = usb::enumerate_device(usb::ROOT_DEVICE_ADDRESS, false) else {
        console.write_output_line("enumeration failed, see UART log");
        return;
    };

    let mut line = Line::new();
    line.push_str("VID:PID = 0x");
    line.push_hex(device.vendor_id as u32, 4);
    line.push_str(":0x");
    line.push_hex(device.product_id as u32, 4);
    line.push_str("  class ");
    line.push_hex(device.device_class as u32, 2);
    line.push_str("/");
    line.push_hex(device.device_subclass as u32, 2);
    line.push_str("/");
    line.push_hex(device.device_protocol as u32, 2);
    console.write_output_line(line.as_str());

    let Some(hub) = usb::Hub::open(&device) else {
        console.write_output_line("not a hub, or the hub descriptor read failed (see UART log)");
        return;
    };
    let descriptor = &hub.descriptor;

    let mut line = Line::new();
    line.push_str("ports: ");
    line.push_u32(descriptor.port_count as u32);
    line.push_str("  power-good delay: ");
    line.push_u32(descriptor.power_on_to_power_good_ms as u32);
    line.push_str("ms  hub current: ");
    line.push_u32(descriptor.control_current_ma as u32);
    line.push_str("mA");
    console.write_output_line(line.as_str());

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
    console.write_output_line(line.as_str());

    let mut line = Line::new();
    line.push_str("compound: ");
    line.push_str(if descriptor.is_compound_device() { "yes" } else { "no" });
    line.push_str("  indicators: ");
    line.push_str(if descriptor.has_port_indicators() { "yes" } else { "no" });
    line.push_str("  TT think time: ");
    line.push_u32(descriptor.tt_think_time_bits() as u32);
    line.push_str(" FS bits  hubdesc: ");
    line.push_u32(descriptor.descriptor_len as u32);
    line.push_str("b");
    console.write_output_line(line.as_str());

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
    console.write_output_line(line.as_str());

    let Some(status) = hub.status() else {
        console.write_output_line("hub GET_STATUS failed, see UART log");
        return;
    };
    let mut line = Line::new();
    line.push_str("hub status: local power ");
    line.push_str(if status.local_power_lost() { "lost" } else { "good" });
    line.push_str(", over-current ");
    line.push_str(if status.over_current() { "YES" } else { "no" });
    if status.local_power_changed() || status.over_current_changed() {
        line.push_str("  (change bits set: 0x");
        line.push_hex(status.change as u32, 4);
        line.push_str(")");
    }
    console.write_output_line(line.as_str());

    report_hub_ports(console, &hub);
}

/// `USB_HOST_PLAN.md` Stage 4-3: power the hub's ports, list what each one
/// reports, then reset the one port that is going to be used and show the
/// speed it came up at.
fn report_hub_ports(console: &mut Console, hub: &usb::Hub) {
    let mut line = Line::new();
    line.push_str("powering ports (power-good wait ");
    line.push_u32(hub.descriptor.power_on_to_power_good_ms as u32);
    line.push_str("ms)...");
    console.write_output_line(line.as_str());
    if !hub.power_on_all_ports() {
        console.write_output_line("PORT_POWER failed, see UART log");
        // One more request tells apart "that request hung" from "the hub
        // is gone", which point at completely different causes.
        console.write_output_line(if hub.status().is_some() {
            "hub still answers GET_STATUS: it is alive, PORT_POWER itself failed"
        } else {
            "hub no longer answers GET_STATUS: it dropped off the bus entirely"
        });
        return;
    }

    for port in 1..=hub.port_count() {
        // One failure means the hub itself went away, so stop instead of
        // spending a control-transfer timeout on each remaining port.
        let Some(status) = hub.port_status(port) else {
            let mut line = Line::new();
            line.push_str("port ");
            line.push_u32(port as u32);
            line.push_str(": GET_PORT_STATUS failed, see UART log");
            console.write_output_line(line.as_str());
            return;
        };
        console.write_output_line(port_status_text(port, &status).as_str());
    }

    let Some(port) = hub.find_connected_port() else {
        console.write_output_line("no usable device on any hub port; see UART log");
        return;
    };

    let mut line = Line::new();
    line.push_str("resetting port ");
    line.push_u32(port as u32);
    line.push_str("...");
    console.write_output_line(line.as_str());

    let Some(status) = hub.reset_port(port) else {
        console.write_output_line("port reset failed or the port is unusable, see UART log");
        if let Some(status) = hub.port_status(port) {
            console.write_output_line(port_status_text(port, &status).as_str());
        }
        return;
    };

    let mut line = Line::new();
    line.push_str("port ");
    line.push_u32(port as u32);
    line.push_str(" enabled after reset, device speed: ");
    line.push_str(speed_text(status.speed()));
    console.write_output_line(line.as_str());
    console.write_output_line(port_status_text(port, &status).as_str());

    report_downstream_device(console, hub, port, status.speed());
}

/// `USB_HOST_PLAN.md` Stage 4-4: enumerate the device sitting on the hub
/// port that was just reset. It gets its own address, and the speed the
/// hub reported for it rather than the bus's.
fn report_downstream_device(console: &mut Console, hub: &usb::Hub, port: u8, speed: usb::Speed) {
    console.write_output_line("enumerating device behind the hub (address 2)...");
    let Some(device) = usb::enumerate_device(usb::DOWNSTREAM_DEVICE_ADDRESS, speed == usb::Speed::Low)
    else {
        console.write_output_line("enumeration failed, see UART log");
        // The hub's own view of the port says whether the failure was on
        // our side of it or the device's: a port the hub has since
        // disabled (or flagged over-current) means it saw something wrong
        // on the wire, while a port still connected and enabled means the
        // transactions simply never got a usable answer.
        if let Some(status) = hub.port_status(port) {
            console.write_output_line(port_status_text(port, &status).as_str());
        }
        return;
    };

    let mut line = Line::new();
    line.push_str("VID:PID = 0x");
    line.push_hex(device.vendor_id as u32, 4);
    line.push_str(":0x");
    line.push_hex(device.product_id as u32, 4);
    line.push_str("  EP0 MPS: ");
    line.push_u32(device.max_packet_size0 as u32);
    line.push_str("  class ");
    line.push_hex(device.device_class as u32, 2);
    line.push_str("/");
    line.push_hex(device.device_subclass as u32, 2);
    line.push_str("/");
    line.push_hex(device.device_protocol as u32, 2);
    console.write_output_line(line.as_str());

    let mut line = Line::new();
    line.push_str("interfaces: ");
    line.push_u32(device.num_interfaces as u32);
    line.push_str("  config bytes: ");
    line.push_u32(device.config_total_length as u32);
    console.write_output_line(line.as_str());

    match usb::find_hid_keyboard(device.config_bytes()) {
        Some(hid) => {
            let mut line = Line::new();
            line.push_str("HID Boot keyboard: interface ");
            line.push_u32(hid.interface_number as u32);
            line.push_str(", EP 0x");
            line.push_hex(hid.endpoint_address as u32, 2);
            line.push_str(", MPS ");
            line.push_u32(hid.max_packet_size as u32);
            console.write_output_line(line.as_str());
        }
        None => console.write_output_line("no HID Boot keyboard interface found"),
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
    line.push_str(if status.over_current() { "OVERCURRENT " } else { "" });
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

fn cmd_usbvbus(console: &mut Console, argument: &[u8]) {
    let (bit_text, rest) = split_first_word(argument);
    let state = trim(rest);
    let Some(bit) = parse_u32(bit_text) else {
        console.write_output_line("usage: usbvbus <0-7> on|off");
        return;
    };
    if bit > 7 {
        console.write_output_line("bit must be 0-7");
        return;
    }
    let on = match state {
        b"on" => true,
        b"off" => false,
        _ => {
            console.write_output_line("usage: usbvbus <0-7> on|off");
            return;
        }
    };
    if usb::set_vbus_bit(bit as u8, on) {
        console.write_output_line("ok; check USB-A 5V with a meter/current tester");
    } else {
        console.write_output_line("I2C write failed (PI4IOE2 @ 0x44 not acked)");
    }
}

fn cmd_backlight(console: &mut Console, argument: &[u8]) {
    match argument {
        b"on" => {
            lcd::set_backlight(true);
            console.write_output_line("backlight on");
        }
        b"off" => {
            lcd::set_backlight(false);
            console.write_output_line("backlight off");
        }
        _ => console.write_output_line("usage: backlight on|off"),
    }
}

fn cmd_color(console: &mut Console, argument: &[u8]) {
    let color = match argument {
        b"green" => Some(GREEN),
        b"red" => Some(RED),
        b"blue" => Some(BLUE),
        b"cyan" => Some(CYAN),
        b"magenta" => Some(MAGENTA),
        b"yellow" => Some(YELLOW),
        b"white" => Some(WHITE),
        _ => None,
    };
    match color {
        Some(color) => {
            console.set_header_color(color);
            console.write_output_line("header color changed");
        }
        None => console.write_output_line("usage: color <name>, see 'help'"),
    }
}

/// Trims leading and trailing spaces (the only whitespace CardKB input or
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
struct Line {
    buffer: [u8; 80],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Self {
            buffer: [0; 80],
            len: 0,
        }
    }

    fn push_str(&mut self, text: &str) {
        for byte in text.bytes() {
            if self.len < self.buffer.len() {
                self.buffer[self.len] = byte;
                self.len += 1;
            }
        }
    }

    fn push_u32(&mut self, value: u32) {
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

    fn push_hex(&mut self, value: u32, digits: u32) {
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
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buffer[..self.len]).unwrap_or("")
    }
}
