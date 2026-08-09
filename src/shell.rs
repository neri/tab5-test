//! Command dispatcher for the CardKB console.
//!
//! Every command reads or writes only through `Console`'s output-line API,
//! so results share the same wrapping/scrolling/rendering as the rest of
//! the console. `execute` runs once per `Enter` keypress and returns whether
//! the caller should reboot the board once the current frame has actually
//! reached the panel.

use crate::console::Console;
use crate::framebuffer::{BLUE, CYAN, GREEN, MAGENTA, RED, WHITE, YELLOW};
use crate::{interrupts, lcd, psram, startup};

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
    b"reboot            restart the board",
];

/// Parses and runs one command line. Returns `true` if the board should
/// reboot once this frame's output has reached the panel.
pub fn execute(console: &mut Console, line: &[u8]) -> bool {
    let line = trim(line);
    if line.is_empty() {
        return false;
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
        b"reboot" | b"reset" => {
            console.write_output_line(b"rebooting...");
            return true;
        }
        _ => console.write_output_line(b"unknown command (try 'help')"),
    }
    false
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

    fn as_bytes(&self) -> &[u8] {
        &self.buffer[..self.len]
    }
}
