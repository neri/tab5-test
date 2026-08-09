//! Command dispatcher for the CardKB console.
//!
//! Every command reads or writes only through `Console`'s output-line API,
//! so results share the same wrapping/scrolling/rendering as the rest of
//! the console. `execute` runs once per `Enter` keypress and returns whether
//! the caller should reboot the board once the current frame has actually
//! reached the panel.

use crate::console::Console;
use crate::framebuffer::{BLUE, CYAN, GREEN, MAGENTA, RED, WHITE, YELLOW};
use crate::{interrupts, lcd, psram, sdmmc, startup};

/// Roughly the panel's vsync rate; used only for the coarse `uptime` command.
const FRAMES_PER_SECOND: u32 = 57;

const HELP_LINES: &[&[u8]] = &[
    b"help              this text",
    b"clear             clear the screen",
    b"echo <text>       print text back",
    b"about             firmware banner",
    b"mem               PSRAM/RAM usage",
    b"uptime            time since boot",
    b"backlight on|off  LCD backlight",
    b"color <name>      change header color",
    b"                  green/red/blue/cyan/magenta/yellow/white",
    b"paint             touch drawing screen",
    b"sdinfo            activate SD card, show CID/CSD summary",
    b"sdread <lba>      read one 512-byte block, dump to UART log",
    b"sdreadn <lba> <n> read n blocks (DMA, n<=8), dump to UART log",
    b"sdwritetest <lba> write+verify+restore 1 block at lba (DMA)",
    b"sdzero <lba>      write a zeroed block at lba (DMA, no round-trip)",
    b"reboot            restart the board",
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
    let (command, rest) = split_first_word(line);
    let argument = trim(rest);
    match command {
        b"help" => cmd_help(console),
        b"clear" => console.clear(),
        b"echo" => console.write_output_line(argument),
        b"about" | b"version" => console.write_output_line(b"Tab5 CardKB Shell 0.1"),
        b"mem" => cmd_mem(console),
        b"uptime" => cmd_uptime(console),
        b"backlight" => cmd_backlight(console, argument),
        b"color" => cmd_color(console, argument),
        b"sdinfo" => cmd_sdinfo(console),
        b"sdread" => cmd_sdread(console, argument),
        b"sdreadn" => cmd_sdreadn(console, argument),
        b"sdwritetest" => cmd_sdwritetest(console, argument),
        b"sdzero" => cmd_sdzero(console, argument),
        b"paint" => return Outcome::Paint,
        b"reboot" | b"reset" => {
            console.write_output_line(b"rebooting...");
            return Outcome::Reboot;
        }
        _ => console.write_output_line(b"unknown command (try 'help')"),
    }
    Outcome::Continue
}

/// Reboots the board. The caller must have already flushed the "rebooting..."
/// output to the panel; this never returns.
pub fn reboot() -> ! {
    startup::reboot()
}

fn cmd_help(console: &mut Console) {
    for line in HELP_LINES {
        console.write_output_line(line);
    }
}

fn cmd_mem(console: &mut Console) {
    let mut line = Line::new();
    line.push_str(b"PSRAM window: ");
    line.push_u32(psram::MAPPED_BYTES as u32);
    line.push_str(b" bytes");
    console.write_output_line(line.as_bytes());

    let mut line = Line::new();
    line.push_str(b"framebuffer: ");
    line.push_u32(psram::FRAMEBUFFER_BYTES as u32);
    line.push_str(b" bytes x");
    line.push_u32(psram::FRAMEBUFFER_COUNT as u32);
    console.write_output_line(line.as_bytes());
}

fn cmd_uptime(console: &mut Console) {
    let seconds = interrupts::frame_sequence() / FRAMES_PER_SECOND;
    let mut line = Line::new();
    line.push_str(b"uptime: ~");
    line.push_u32(seconds);
    line.push_str(b" s (frame-counted)");
    console.write_output_line(line.as_bytes());
}

fn cmd_sdinfo(console: &mut Console) {
    console.write_output_line(b"activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(b"SD card activation failed, see UART log");
        return;
    };

    let mut line = Line::new();
    line.push_str(b"RCA: 0x");
    line.push_hex(card.rca as u32, 4);
    line.push_str(b"  manufacturer ID: 0x");
    line.push_hex(card.cid[3] >> 24, 2);
    console.write_output_line(line.as_bytes());

    let mut line = Line::new();
    line.push_str(b"type: ");
    line.push_str(if card.high_capacity {
        b"SDHC/SDXC"
    } else {
        b"SDSC"
    });
    console.write_output_line(line.as_bytes());

    let mut line = Line::new();
    match card.capacity_bytes {
        Some(bytes) => {
            line.push_str(b"capacity: ~");
            line.push_u32((bytes / (1024 * 1024)) as u32);
            line.push_str(b" MiB");
        }
        None => line.push_str(b"capacity: unknown (CSD v1, not decoded)"),
    }
    console.write_output_line(line.as_bytes());

    // CSD PERM_WRITE_PROTECT (bit 13) / TMP_WRITE_PROTECT (bit 12), common to
    // both CSD structure versions.
    let write_protected = card.csd[0] & (0b11 << 12) != 0;
    console.write_output_line(if write_protected {
        b"write-protected: yes"
    } else {
        b"write-protected: no"
    });
    console.write_output_line(b"full CID/CSD dump: see UART log");
}

fn cmd_sdread(console: &mut Console, argument: &[u8]) {
    let Some(lba) = parse_u32(argument) else {
        console.write_output_line(b"usage: sdread <lba>");
        return;
    };

    console.write_output_line(b"activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(b"SD card activation failed, see UART log");
        return;
    };

    let mut buffer = [0u8; 512];
    if !sdmmc::read_block(&card, lba, &mut buffer) {
        console.write_output_line(b"block read failed, see UART log");
        return;
    }
    sdmmc::dump_block(&buffer);

    let mut line = Line::new();
    line.push_str(b"read LBA ");
    line.push_u32(lba);
    line.push_str(b": ");
    for &byte in &buffer[..8] {
        line.push_hex(byte as u32, 2);
        line.push_str(b" ");
    }
    line.push_str(b"...");
    console.write_output_line(line.as_bytes());

    let boot_signature = buffer[510] == 0x55 && buffer[511] == 0xAA;
    console.write_output_line(if boot_signature {
        b"bytes 510-511 = 55 AA (MBR/boot-sector signature)"
    } else {
        b"no 55 AA signature at bytes 510-511"
    });
    console.write_output_line(b"full 512-byte hex dump: see UART log");
}

const MAX_MULTI_BLOCKS: u32 = 8;

fn cmd_sdreadn(console: &mut Console, argument: &[u8]) {
    let (lba_text, count_text) = split_first_word(argument);
    let (Some(lba), Some(count)) = (parse_u32(lba_text), parse_u32(trim(count_text))) else {
        console.write_output_line(b"usage: sdreadn <lba> <count>");
        return;
    };
    if count == 0 || count > MAX_MULTI_BLOCKS {
        console.write_output_line(b"count must be 1..=8");
        return;
    }

    console.write_output_line(b"activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(b"SD card activation failed, see UART log");
        return;
    };

    let mut buffer = [0u8; 512 * MAX_MULTI_BLOCKS as usize];
    let region = &mut buffer[..512 * count as usize];
    if !sdmmc::read_blocks(&card, lba, region) {
        console.write_output_line(b"multi-block read failed, see UART log");
        return;
    }
    for block in region.chunks_exact(512) {
        sdmmc::dump_block(block.try_into().unwrap());
    }

    let mut line = Line::new();
    line.push_str(b"read ");
    line.push_u32(count);
    line.push_str(b" block(s) from LBA ");
    line.push_u32(lba);
    line.push_str(b" via DMA, OK");
    console.write_output_line(line.as_bytes());
    console.write_output_line(b"full hex dump: see UART log");
}

fn cmd_sdwritetest(console: &mut Console, argument: &[u8]) {
    let Some(lba) = parse_u32(argument) else {
        console.write_output_line(b"usage: sdwritetest <lba>");
        return;
    };

    console.write_output_line(b"WARNING: temporarily overwrites 1 block, then restores it");
    console.write_output_line(b"activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(b"SD card activation failed, see UART log");
        return;
    };

    let mut original = [0u8; 512];
    if !sdmmc::read_blocks(&card, lba, &mut original) {
        console.write_output_line(b"could not read original block, aborting (nothing written)");
        return;
    }

    let mut pattern = [0u8; 512];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = (index as u8) ^ 0xA5;
    }

    if !sdmmc::write_blocks(&card, lba, &mut pattern) {
        console.write_output_line(b"pattern write failed, see UART log");
        return;
    }
    let mut verify = [0u8; 512];
    let pattern_ok =
        sdmmc::read_blocks(&card, lba, &mut verify) && verify == pattern;
    console.write_output_line(if pattern_ok {
        b"pattern write+read-back: match"
    } else {
        b"pattern write+read-back: MISMATCH, see UART log"
    });

    let mut restore = original;
    let restored = sdmmc::write_blocks(&card, lba, &mut restore);
    let mut check = [0u8; 512];
    let restore_ok = restored && sdmmc::read_blocks(&card, lba, &mut check) && check == original;
    console.write_output_line(if restore_ok {
        b"original data restored: yes"
    } else {
        b"original data restored: NO -- see UART log, LBA may be corrupted"
    });
}

fn cmd_sdzero(console: &mut Console, argument: &[u8]) {
    let Some(lba) = parse_u32(argument) else {
        console.write_output_line(b"usage: sdzero <lba>");
        return;
    };

    console.write_output_line(b"activating SD card...");
    let Some(card) = sdmmc::init() else {
        console.write_output_line(b"SD card activation failed, see UART log");
        return;
    };

    let mut zero = [0u8; 512];
    if !sdmmc::write_blocks(&card, lba, &mut zero) {
        console.write_output_line(b"zero write failed, see UART log");
        return;
    }
    console.write_output_line(b"block zeroed");
}

fn cmd_backlight(console: &mut Console, argument: &[u8]) {
    match argument {
        b"on" => {
            lcd::set_backlight(true);
            console.write_output_line(b"backlight on");
        }
        b"off" => {
            lcd::set_backlight(false);
            console.write_output_line(b"backlight off");
        }
        _ => console.write_output_line(b"usage: backlight on|off"),
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
            console.write_output_line(b"header color changed");
        }
        None => console.write_output_line(b"usage: color <name>, see 'help'"),
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

    fn push_str(&mut self, text: &[u8]) {
        for &byte in text {
            if self.len < self.buffer.len() {
                self.buffer[self.len] = byte;
                self.len += 1;
            }
        }
    }

    fn push_u32(&mut self, value: u32) {
        if value == 0 {
            self.push_str(b"0");
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

    fn as_bytes(&self) -> &[u8] {
        &self.buffer[..self.len]
    }
}
