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

use super::automount::AutoMount;
use super::wifi_manager::{self, IpPolicy, Manager as WifiManager};
use super::{blockdev, browsertest, files, fswritetest, lsusb, mbr, membench};
use crate::console::Console;
use crate::framebuffer::Framebuffer;
use smoltcp::wire::{Ipv4Address, Ipv4Cidr};

use crate::browser::url::Url;
use crate::fs;
use crate::fs::path::{self, Path};
use crate::fs::vfs::{FileHandle, MAX_OPEN_FILES, MountRequest, Vfs};
use crate::fs::{Devices, RamBlockDevice, SdSlot};
use crate::{
    browser, delay, dma2d, entropy, icm, interrupts, lcd, net, pma, pmp, power, psram, rtc, sdio,
    sdmmc, startup, tick, uart, usb, wall_clock, wifi,
};

/// Roughly the panel's vsync rate; used only for the coarse `uptime` command.
/// Fixed, because the panel only tolerates the one set of vertical timings
/// (see `lcd`'s `VFP_LINES`).
const FRAMES_PER_SECOND: u32 = 57;

/// One `help` entry: the bare command name (used both to look commands up
/// and to list them), its usage line, and one or more description lines
/// shown by `help <name>`.
struct HelpEntry {
    name: &'static str,
    /// Other names that reach the same command. Data rather than extra
    /// match arms, so a name that works is always a name `help` can show.
    aliases: &'static [&'static str],
    group: Group,
    /// What `execute` dispatches on. The table is the only place a name is
    /// turned into one of these, so a command that is not listed here
    /// cannot be reached from the console.
    id: Cmd,
    usage: &'static str,
    lines: &'static [&'static str],
}

/// Whether a command is part of what this firmware is for, or scaffolding
/// that exists only while something is still being built.
///
/// The split is deliberately not "who is it for". From outside the project
/// a command that decodes the DW-GDMA arbitration registers is no more
/// usable than one that soaks the display for two hours -- neither was
/// written for somebody using the board. What separates them is whether
/// they outlive the work that caused them to be written, and most of these
/// do not.
///
/// `docs/CONSOLE_COMMAND_REVIEW.md` records the assignment, and for each
/// scaffold command the work that is still keeping it alive.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    /// Operates the board. Stays.
    Product,
    /// Written to get something working, or to show that it works. Goes
    /// when that question is settled.
    Scaffold,
}

/// One command's identity. Fieldless on purpose: it names a command without
/// carrying anything, so [`HELP_ENTRIES`] stays a table of data and the
/// `match` in [`execute`] stays exhaustive over it. Adding a variant without
/// a body, or listing one that no longer exists, is a compile error rather
/// than something the console discovers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmd {
    Win,
    Help,
    Clear,
    Echo,
    About,
    Cpuinfo,
    Pma,
    Pmp,
    Mem,
    Alloctest,
    Stress,
    Displaybench,
    Db,
    Dp,
    Di,
    Ui,
    Mix,
    Ut,
    Usbmargin,
    Pf,
    Rt,
    Membench,
    Uptime,
    Backlight,
    Icm,
    Ppafill,
    Paint,
    Touchtest,
    Touchcheck,
    Coordtest,
    Fonttest,
    Axistest,
    Tls,
    Entropy,
    Rtc,
    Sdinfo,
    Sdread,
    Sdreadn,
    Sdwritetest,
    Sdzero,
    Sdmbr,
    Devices,
    Blkread,
    Mount,
    Umount,
    Automount,
    Mounts,
    Df,
    Fsverify,
    Cd,
    Pwd,
    Ls,
    Cat,
    Rm,
    Rmdir,
    Mv,
    Fill,
    Fswritetest,
    Fsopen,
    Fsread,
    Fsclose,
    Write,
    Append,
    Mkdir,
    Sdreadpsram,
    Lsusb,
    Usbinfo,
    Usbrescan,
    Usbfs,
    Usbvbus,
    Shutdown,
    Usbhub,
    Usbcachefail,
    Usbcheck,
    Usbrawcheck,
    Usbmultiwrite,
    Usbhw,
    Usbperiodic,
    Usbmsc,
    Usbread,
    Usbzero,
    Usbwritetest,
    Usbmbr,
    Wifi,
    Wifion,
    Wifioff,
    Wifiinfo,
    Wifiup,
    Wifimac,
    Wifiscan,
    Wificonnect,
    Wifistatus,
    Wifisaved,
    Wififorget,
    Wifilog,
    Wifidisconnect,
    Netdump,
    Ipconfig,
    Nslookup,
    Ping,
    Tftpget,
    Httpget,
    Browser,
    Bt,
    Hs,
    Reboot,
}

const HELP_ENTRIES: &[HelpEntry] = &[
    HelpEntry {
        name: "help",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Help,
        usage: "help [command]",
        lines: &["list commands, or describe one"],
    },
    HelpEntry {
        name: "clear",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Clear,
        usage: "clear",
        lines: &["clear the screen"],
    },
    HelpEntry {
        name: "echo",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Echo,
        usage: "echo <text>",
        lines: &["print text back"],
    },
    HelpEntry {
        name: "about",
        aliases: &["version"],
        group: Group::Product,
        id: Cmd::About,
        usage: "about",
        lines: &["firmware banner"],
    },
    HelpEntry {
        name: "cpuinfo",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Cpuinfo,
        usage: "cpuinfo",
        lines: &["show RISC-V machine identification CSRs"],
    },
    HelpEntry {
        name: "pma",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Pma,
        usage: "pma",
        lines: &[
            "decode all RISC-V Physical Memory Attribute entries as a memory",
            "map. ranges are [start,end); show mode, R/W/X, enable/lock, and",
            "cache attributes (WB, WT, NC, WNA, RNA). OFF entries supply a",
            "following TOR entry's lower bound and therefore have no range.",
        ],
    },
    HelpEntry {
        name: "pmp",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Pmp,
        usage: "pmp",
        lines: &[
            "decode all RISC-V Physical Memory Protection entries as a memory",
            "map. ranges are [start,end); show mode, R/W/X and the lock bit.",
            "entries are priority-ordered, so the lowest matching one decides",
            "an access; machine mode obeys only the locked (L) entries. OFF",
            "entries supply a following TOR entry's lower bound.",
        ],
    },
    HelpEntry {
        name: "mem",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Mem,
        usage: "mem",
        lines: &["PSRAM/RAM usage"],
    },
    HelpEntry {
        name: "alloctest",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Alloctest,
        usage: "alloctest <MiB>",
        lines: &["allocate N MiB on the PSRAM heap, verify read/write"],
    },
    HelpEntry {
        name: "stress",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Stress,
        usage: "stress [count]",
        lines: &[
            "repeat a full-screen fill and report how long it took and how",
            "many DPI FIFO underruns it caused. a fixed, countable workload,",
            "so settings can be compared: run it, change one thing (say the",
            "'icm' priority), run it again with the same count.",
        ],
    },
    HelpEntry {
        name: "displaybench",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Displaybench,
        usage: "displaybench <mode> [count] [phase_ms] [burst]",
        lines: &[
            "full-screen display-path diagnostic. modes: idle, sync, cpu,",
            "ppa-raw, ppa-safe, production. phase_ms is 0, 3, 8, or 12;",
            "burst is 8, 16, 32, 64, or 128 bytes. each draw starts at a",
            "frame boundary plus phase_ms and consumes the underrun bit.",
        ],
    },
    HelpEntry {
        name: "db",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Db,
        usage: "db [count]",
        lines: &[
            "run the standard displaybench suite with one short command.",
            "covers idle, cache sync, CPU, raw/safe PPA, production, all",
            "four frame phases and all five DMA2D bursts; default count 100.",
        ],
    },
    HelpEntry {
        name: "dp",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Dp,
        usage: "dp [count]",
        lines: &[
            "run only the production full-screen display path at the normal",
            "128-byte DMA2D burst; default count 100. unlike db, this skips",
            "the deliberately hostile short-burst diagnostic cases.",
        ],
    },
    HelpEntry {
        name: "di",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Di,
        usage: "di [minutes]",
        lines: &[
            "idle-display soak with per-frame underrun accounting; default",
            "30 minutes, maximum 120. sets display ICM priority to 15/15.",
        ],
    },
    HelpEntry {
        name: "ui",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Ui,
        usage: "ui",
        lines: &[
            "run 100 real console scrolls, then visit the coordinate, paint,",
            "touch and axis screens. interact with each screen and",
            "press any key to advance; the final screen reports underruns.",
        ],
    },
    HelpEntry {
        name: "mix",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Mix,
        usage: "mix [minutes]",
        lines: &[
            "read-only combined soak: display fills, PSRAM heap verify, SD",
            "and USB Mass Storage reads. default 120 minutes, maximum 240;",
            "insert both media before starting. no storage block is written.",
        ],
    },
    HelpEntry {
        name: "ut",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Ut,
        usage: "ut [count]",
        lines: &[
            "read and compare the same 4 KiB from USB Mass Storage repeatedly;",
            "default 100, maximum 1000. read-only; reports packet and BOT retries.",
        ],
    },
    HelpEntry {
        name: "usbmargin",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbmargin,
        usage: "usbmargin [rounds]",
        lines: &[
            "measure how long USB Mass Storage takes to become readable after",
            "its 5V comes on: cut VBUS, rescan, time connect/enumeration/ready/",
            "LBA 0. read-only; default 5 rounds, maximum 20.",
        ],
    },
    HelpEntry {
        name: "pf",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Pf,
        usage: "pf",
        lines: &[
            "reboot once, reject the valid 200 MHz DQS result diagnostically,",
            "and verify that the same boot recovers with the 80 MHz profile.",
            "the marker is consumed once; the following reboot tries 200 MHz again.",
        ],
    },
    HelpEntry {
        name: "rt",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Rt,
        usage: "rt [count]",
        lines: &[
            "reboot count times automatically; default 20, maximum 100.",
            "each pass reaches 200 MHz PSRAM, the post-XIP probe, heap, and",
            "display scanout. the final boot prints one PASS line and stops.",
        ],
    },
    HelpEntry {
        name: "membench",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Membench,
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
        aliases: &[],
        group: Group::Product,
        id: Cmd::Uptime,
        usage: "uptime",
        lines: &["time since boot"],
    },
    HelpEntry {
        name: "backlight",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Backlight,
        usage: "backlight on|off",
        lines: &["LCD backlight"],
    },
    HelpEntry {
        name: "icm",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Icm,
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
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Ppafill,
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
        aliases: &[],
        group: Group::Product,
        id: Cmd::Paint,
        usage: "paint",
        lines: &["touch drawing screen"],
    },
    HelpEntry {
        name: "touchtest",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Touchtest,
        usage: "touchtest",
        lines: &["live multi-touch test; use two fingers, any key exits"],
    },
    HelpEntry {
        name: "touchcheck",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Touchcheck,
        usage: "touchcheck",
        lines: &[
            "open the built-in long Browser page for GUI touch acceptance.",
            "a press alone must do nothing; a short press and release within",
            "500 ms and 10 px follows the bottom link; dragging more than 10",
            "px scrolls and must not follow a link; holding over 500 ms and",
            "releasing must do nothing. Ctrl+Q (or q) leaves.",
        ],
    },
    HelpEntry {
        name: "coordtest",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Coordtest,
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
        name: "fonttest",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Fonttest,
        usage: "fonttest",
        lines: &[
            "three full-screen sheets: the uncompressed ROM-only ASCII 1bpp",
            "font, the Latin/Japanese A4 UI font at 16/24/32 pixels, then A4",
            "versus thresholded 1-bit edges side by side. Japanese is stored",
            "only at 16 pixels and enlarged 2x for the 32-pixel sample. press",
            "a key to advance; the third key exits.",
        ],
    },
    HelpEntry {
        name: "axistest",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Axistest,
        usage: "axistest",
        lines: &["tilt-controlled BMI270 ball test; any key exits"],
    },
    HelpEntry {
        name: "tls",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Tls,
        usage: "tls <host|a.b.c.d>[:port] [path]",
        lines: &[
            "open a TLS 1.3 connection, fetch one page over it and report",
            "what the handshake proved. the default port is 443 and the",
            "default path is /. the connection is UNAUTHENTICATED: the",
            "server's CertificateVerify and Finished are checked against the",
            "key it sent, but nothing checks that the key is this host's --",
            "no chain, no root, no name, no expiry. it stops passive",
            "eavesdropping and not an active attacker. reports the handshake",
            "time and the longest single poll, which is what the browser's",
            "frame loop would feel.",
        ],
    },
    HelpEntry {
        name: "entropy",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Entropy,
        usage: "entropy | entropy test [count] | entropy fail on|off",
        lines: &[
            "the SAR ADC noise source behind the hardware RNG, which TLS",
            "seeds its CSPRNG from. with no argument, take one seed and",
            "print it. 'test' repeats the seeding, checks that every",
            "enable was paired with a disable and that no two seeds came",
            "out the same. 'fail' forces seeding to fail without touching",
            "the hardware, so a caller can be checked for starting nothing",
            "when there is no randomness to start it with.",
        ],
    },
    HelpEntry {
        name: "rtc",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Rtc,
        usage: "rtc | rtc set <YYYY-MM-DD> <HH:MM:SS> (UTC) | rtc regs | rtc test",
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
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Sdinfo,
        usage: "sdinfo",
        lines: &["activate SD card, show CID/CSD summary"],
    },
    HelpEntry {
        name: "sdread",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Sdread,
        usage: "sdread <lba>",
        lines: &["read one 512-byte block, dump to UART log"],
    },
    HelpEntry {
        name: "sdreadn",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Sdreadn,
        usage: "sdreadn <lba> <n>",
        lines: &["read n blocks (DMA, n<=8), dump to UART log"],
    },
    HelpEntry {
        name: "sdwritetest",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Sdwritetest,
        usage: "sdwritetest <lba>",
        lines: &["write+verify+restore 1 block at lba (DMA)"],
    },
    HelpEntry {
        name: "sdzero",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Sdzero,
        usage: "sdzero <lba>",
        lines: &["write a zeroed block at lba (DMA, no round-trip)"],
    },
    HelpEntry {
        name: "sdmbr",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Sdmbr,
        usage: "sdmbr",
        lines: &["show MBR partition table (LBA 0)"],
    },
    HelpEntry {
        name: "devices",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Devices,
        usage: "devices",
        lines: &[
            "list block devices (ram, sd0, usb0) with their geometry and",
            "what LBA 0 turned out to be: an MBR with its usable and",
            "rejected primary entries, an unpartitioned FAT/exFAT volume,",
            "a sector readable as both (refused), or neither",
        ],
    },
    HelpEntry {
        name: "blkread",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Blkread,
        usage: "blkread <device> [pN] <lba>",
        lines: &[
            "read one block through the block layer and dump it to UART.",
            "with pN the lba is relative to that MBR primary partition,",
            "and one past its end is refused before reaching the medium",
        ],
    },
    HelpEntry {
        name: "mount",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Mount,
        usage: "mount [-r] [<ram|sd0pN|usbMpN>]",
        lines: &[
            "with no argument, list mounts. with one, attach that volume:",
            "ram is the permanent writable root /; everything else lands on",
            "/vol/<name>. usbM is the",
            "number the host gave that drive when it attached, which 'devices'",
            "prints; it stays with the drive until it is unplugged, so pulling",
            "one stick never renumbers another. FAT volumes mount read-write",
            "and exFAT ones read-only, whatever you ask for; -r forces a FAT",
            "volume read-only too. /vol is reserved for their mount points",
        ],
    },
    HelpEntry {
        name: "umount",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Umount,
        usage: "umount <mount point>",
        lines: &["detach a volume; refused while it still has open files"],
    },
    HelpEntry {
        name: "automount",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Automount,
        usage: "automount [on|off]",
        lines: &[
            "with no argument, report whether USB volumes mount and unmount",
            "on their own. they do by default: a stick appears under /vol a",
            "moment after it is plugged in and its mounts are dropped when it",
            "is pulled, each with a line saying so. off leaves the tree to",
            "'mount' and 'umount' alone. a volume you unmount by hand stays",
            "unmounted until its drive is physically removed and brought back",
        ],
    },
    HelpEntry {
        name: "mounts",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Mounts,
        usage: "mounts",
        lines: &[
            "list what is mounted where, with each mount's media generation",
            "and what its identity rests on. 'size only' means the medium",
            "offered nothing but its capacity, so a match proves little",
        ],
    },
    HelpEntry {
        name: "df",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Df,
        usage: "df [-c]",
        lines: &[
            "size, used and free space of every mounted volume, in KiB, and",
            "where the free count came from. FAT32 uses its FSInfo count when",
            "it is valid; FAT12/16, or FAT32 with -c, read the whole FAT, which",
            "takes a moment on a large card. exFAT counts its bitmap",
        ],
    },
    HelpEntry {
        name: "fsverify",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Fsverify,
        usage: "fsverify",
        lines: &[
            "re-check every mount against the medium it was mounted from.",
            "a changed medium drops the mount and fails its open files; a",
            "device that is absent or will not identify itself is left",
            "alone, since neither shows the medium is different",
        ],
    },
    HelpEntry {
        name: "cd",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Cd,
        usage: "cd [<path>]",
        lines: &[
            "change the current directory, which every path argument is",
            "resolved against. with no path, go to the root. the target",
            "has to exist and be a directory. the current directory is a",
            "path, not a hold on a volume: unmount what it is on and the",
            "commands using it start failing until it is mounted again",
        ],
    },
    HelpEntry {
        name: "pwd",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Pwd,
        usage: "pwd",
        lines: &["print the current directory"],
    },
    HelpEntry {
        name: "ls",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Ls,
        usage: "ls [-l] [-a] [<path>]",
        lines: &[
            "list a directory, names only, in name order. -l adds the kind,",
            "size and timestamp one per line; -a adds the dotted entries",
            "(FAT gives every subdirectory a . and a ..); -la does both.",
            "with no path, list the current one. a path not starting with /",
            "is taken from there, and one containing spaces goes in double",
            "quotes. / is the writable RAM disk; /tmp is the conventional",
            "temporary directory and external volumes are under /vol",
        ],
    },
    HelpEntry {
        name: "cat",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Cat,
        usage: "cat <path> [offset]",
        lines: &[
            "print a file, e.g. cat /tmp/README.TXT. a path with spaces in",
            "it goes in double quotes. reports the bytes read against the",
            "size in the directory entry, so a chain that ends early shows",
            "as SHORT READ instead of a plausible prefix",
        ],
    },
    HelpEntry {
        name: "rm",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Rm,
        usage: "rm <path>",
        lines: &[
            "remove a file. refused while something has it open, and refused",
            "on a directory -- rmdir takes those, so a mistyped path cannot",
            "quietly take a directory away instead",
        ],
    },
    HelpEntry {
        name: "rmdir",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Rmdir,
        usage: "rmdir <path>",
        lines: &["remove an empty directory; a non-empty one is refused"],
    },
    HelpEntry {
        name: "mv",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Mv,
        usage: "mv <from> <to>",
        lines: &[
            "rename, or move within one volume. only the directory entry",
            "moves, so crossing volumes is refused rather than copied. the",
            "destination must not already exist",
        ],
    },
    HelpEntry {
        name: "fill",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Fill,
        usage: "fill <path> <KiB> [chunk] [repeat]",
        lines: &[
            "write a known pattern and report how long it took. the default",
            "path holds one writer open for the whole file; 'repeat' takes",
            "the old path of one open, one chain walk and one sync per",
            "chunk. both are linear in size at these sizes -- what differs",
            "is the chunk: halving it roughly doubles 'repeat' and leaves",
            "the streaming path about where it was",
        ],
    },
    HelpEntry {
        name: "fswritetest",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Fswritetest,
        usage: "fswritetest <dir> [rounds] [KiB]",
        lines: &[
            "WRITES TO THE MEDIUM. run the whole write-path acceptance list",
            "against a mounted volume and answer PASS or FAIL. everything it",
            "makes goes in <dir>/FSWTEST, which it creates and removes; it",
            "refuses to start if that name is already taken. it checks the",
            "bytes rather than showing them, so replacing a file with a",
            "shorter one and appending to it is actually verified.",
            "on a read-only mount it checks that every change is refused",
            "instead, so 'mount -r' is tested by the same command.",
            "rounds (default 8) repeats create/append/replace/delete: raise",
            "it to soak the WRITE path, and 'fswritetest /tmp 70' is what",
            "shows the 8 MiB RAM disk really gives clusters back",
        ],
    },
    HelpEntry {
        name: "fsopen",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Fsopen,
        usage: "fsopen [<path>]",
        lines: &[
            "hold a file open across commands, read-only, or with no path",
            "list what is being held. every other command closes what it",
            "opens, so this is the only way to still have a handle open when",
            "a drive is pulled. the listing's live/stale column is the point:",
            "a handle goes stale the moment its volume leaves the mount table",
            "and never recovers, since re-mounting gives a new generation",
        ],
    },
    HelpEntry {
        name: "fsread",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Fsread,
        usage: "fsread <slot> [bytes]",
        lines: &[
            "read through a handle held by fsopen and report the outcome, not",
            "the bytes -- 'cat' already prints contents. this is how a stale",
            "handle is seen failing rather than inferred from the listing",
        ],
    },
    HelpEntry {
        name: "fsclose",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Fsclose,
        usage: "fsclose <slot>",
        lines: &[
            "release a handle held by fsopen. a stale one still occupies a",
            "slot in the open-file table, so this is how the slot comes back",
        ],
    },
    HelpEntry {
        name: "write",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Write,
        usage: "write <path> <text>",
        lines: &[
            "create or replace a file with one line of text, e.g.",
            "write /tmp/NOTE.TXT hello. the RAM root is writable; SD and USB",
            "refuse before any command reaches the medium",
        ],
    },
    HelpEntry {
        name: "append",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Append,
        usage: "append <path> <text>",
        lines: &["add one line to a file, creating it if it is not there"],
    },
    HelpEntry {
        name: "mkdir",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Mkdir,
        usage: "mkdir <path>",
        lines: &[
            "create a directory. only the last component is created, so a",
            "parent that is not there is an error rather than something to",
            "build silently. /tmp and /vol themselves are reserved",
        ],
    },
    HelpEntry {
        name: "sdreadpsram",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Sdreadpsram,
        usage: "sdreadpsram <lba> <n>",
        lines: &["DMA n blocks (n<=8) into PSRAM, verify vs SRAM"],
    },
    HelpEntry {
        name: "lsusb",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Lsusb,
        usage: "lsusb [address]",
        lines: &[
            "show what is attached to USB-A as a tree through the hub, with",
            "every interface of a composite device listed under it. with an",
            "address (the number in brackets), show that device's device,",
            "configuration, interface and endpoint descriptors instead, plus",
            "its manufacturer/product/serial strings read on the spot.",
            "reads the last scan, so run usbrescan after plugging something in",
        ],
    },
    HelpEntry {
        name: "usbinfo",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Usbinfo,
        usage: "usbinfo",
        lines: &[
            "show every device currently attached to USB-A (direct or behind",
            "a hub) from the last scan; run usbrescan first if just plugged in",
        ],
    },
    HelpEntry {
        name: "usbrescan",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Usbrescan,
        usage: "usbrescan",
        lines: &[
            "force a fresh USB-A probe: reset the port, re-enumerate whatever",
            "is plugged in (and every occupied port if it is a hub)",
        ],
    },
    HelpEntry {
        name: "usbfs",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbfs,
        usage: "usbfs on|off",
        lines: &[
            "force the root host to FS/LS-only or restore High-Speed mode,",
            "then reset and re-enumerate the bus (diagnostic)",
        ],
    },
    HelpEntry {
        name: "usbvbus",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbvbus,
        usage: "usbvbus <0-7> on|off",
        lines: &[
            "raw PI4IOE2 (0x44) output-bit toggle; bit 3 = USB-A VBUS",
            "mainly useful for diagnostics; usbrescan drives bit 3 itself",
        ],
    },
    HelpEntry {
        name: "shutdown",
        aliases: &["poweroff"],
        group: Group::Product,
        id: Cmd::Shutdown,
        usage: "shutdown",
        lines: &[
            "turn off the whole Tab5 through its power controller",
            "(save data first; press the physical power key to start again)",
        ],
    },
    HelpEntry {
        name: "usbhub",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbhub,
        usage: "usbhub",
        lines: &[
            "show the attached USB hub's descriptor and every port's live",
            "status, plus which class driver (if any) is attached to it",
        ],
    },
    HelpEntry {
        name: "usbhw",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbhw,
        usage: "usbhw",
        lines: &[
            "dump the DWC core's GHWCFG registers and probe HCSPLT to show",
            "whether split transactions exist in hardware at all",
        ],
    },
    HelpEntry {
        name: "usbperiodic",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbperiodic,
        usage: "usbperiodic",
        lines: &[
            "run one channel-1 HID Interrupt IN transaction through the DWC",
            "32-entry periodic frame list; press/release a key within 5 seconds",
        ],
    },
    HelpEntry {
        name: "usbmsc",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbmsc,
        usage: "usbmsc",
        lines: &[
            "SCSI INQUIRY/TEST UNIT READY/READ CAPACITY(10) against the",
            "attached Mass Storage device (direct or behind a hub port)",
        ],
    },
    HelpEntry {
        name: "usbread",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbread,
        usage: "usbread <lba>",
        lines: &["USB MSC: read one 512-byte block (SCSI READ(10)), dump to UART log"],
    },
    HelpEntry {
        name: "usbzero",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbzero,
        usage: "usbzero <lba> [count]",
        lines: &[
            "USB MSC: overwrite 1-8 blocks with zeros and verify each one from",
            "the medium. clears the pattern a failed usbwritetest leaves behind.",
            "destructive: the previous contents are gone for good.",
        ],
    },
    HelpEntry {
        name: "usbcheck",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbcheck,
        usage: "usbcheck [reads] [lba]",
        lines: &[
            "USB BOT/HCD acceptance run for one configuration: read soak, then",
            "ten write rounds when an LBA is given, with counter deltas and a",
            "PASS/FAIL per gate. no LBA means read-only. use 'usbrawcheck'",
            "for write bursts without touching filesystem structures.",
        ],
    },
    HelpEntry {
        name: "usbrawcheck",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbrawcheck,
        usage: "usbrawcheck <lba> [writes] [span] [gap_ms]",
        lines: &[
            "USB MSC raw write burst over a sacrificial LBA range, followed",
            "by read-back and best-effort restore. defaults: 32 writes over",
            "one block; span 1..8, gap_ms 0..2000 (default 0). no file or",
            "directory is created. gap waits only between successful writes.",
            "THE RANGE MUST BE OUTSIDE EVERY FILESYSTEM YOU CARE ABOUT: a",
            "transport failure can prevent restore, but needs only usbrescan",
            "to retry when the named range is deliberately disposable.",
        ],
    },
    HelpEntry {
        name: "usbmultiwrite",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbmultiwrite,
        usage: "usbmultiwrite <lba> <2|4|8>",
        lines: &[
            "Stage 7 diagnostic: issue one multi-block WRITE(10) ten times,",
            "logging every data-OUT packet and final CSW to UART. each round",
            "is flushed and read back; the test range plus one guard block on",
            "each side is snapshotted and restored with single-block writes.",
            "DESTRUCTIVE IF THE USB SESSION DIES: all named and guard blocks",
            "must be outside every filesystem and safe to lose. run 2 blocks,",
            "then 4, then 8; stop at the first FAIL and run usbrescan.",
        ],
    },
    HelpEntry {
        name: "usbcachefail",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbcachefail,
        usage: "usbcachefail",
        lines: &[
            "USB: inject a refused DMA cache sync, a stale completion, a short",
            "OUT and a FIFO flush timeout, and check each one fails the transfer",
            "instead of being absorbed. tests driver logic, so one run covers",
            "every topology. reads only, never writes. ends the MSC session by",
            "design; run usbrescan afterwards.",
        ],
    },
    HelpEntry {
        name: "usbwritetest",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbwritetest,
        usage: "usbwritetest <lba>",
        lines: &[
            "USB MSC: write a pattern to one 512-byte block (SCSI WRITE(10)),",
            "read it back, then restore the original contents and verify that",
            "too. pick an LBA outside any filesystem you care about.",
        ],
    },
    HelpEntry {
        name: "usbmbr",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Usbmbr,
        usage: "usbmbr",
        lines: &["USB MSC: show MBR partition table (LBA 0), same format as sdmbr"],
    },
    HelpEntry {
        name: "wifi",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Wifi,
        usage: "wifi <subcommand> [arguments]",
        lines: &[
            "on / off: persist the radio setting; status: manager, IPv4 and AP",
            "scan / connect <ssid> [password] / disconnect / forget",
            "info / up / mac / saved / log: diagnostics",
            "help wifi <subcommand> shows details. GUI settings are in the system bar.",
        ],
    },
    HelpEntry {
        name: "wifi on",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Wifion,
        usage: "wifi on",
        lines: &["enable Wi-Fi and save the setting for next boot"],
    },
    HelpEntry {
        name: "wifi off",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Wifioff,
        usage: "wifi off",
        lines: &["disable Wi-Fi and save the setting for next boot"],
    },
    HelpEntry {
        name: "wifi info",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Wifiinfo,
        usage: "wifi info",
        lines: &[
            "power and activate the ESP32-C6 as an SDIO card, show its",
            "CIS identifiers and bus setup",
        ],
    },
    HelpEntry {
        name: "wifi up",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Wifiup,
        usage: "wifi up",
        lines: &[
            "bring up the ESP-Hosted link to the ESP32-C6 and show what the",
            "slave firmware reports about itself",
        ],
    },
    HelpEntry {
        name: "wifi mac",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Wifimac,
        usage: "wifi mac",
        lines: &[
            "bring up the link and ask the C6 for its station MAC address",
            "over RPC (one request/response round trip)",
        ],
    },
    HelpEntry {
        name: "wifi scan",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Wifiscan,
        usage: "wifi scan",
        lines: &[
            "bring up the C6, start Wi-Fi in station mode and list the",
            "access points it can see",
        ],
    },
    HelpEntry {
        name: "wifi connect",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Wificonnect,
        usage: "wifi connect <ssid> [password]",
        lines: &[
            "join an access point and report the result. this associates",
            "only; run 'ipconfig dhcp' afterwards to get an address",
        ],
    },
    HelpEntry {
        name: "wifi status",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Wifistatus,
        usage: "wifi status",
        lines: &["show manager state, saved-profile presence, IPv4 and the associated AP"],
    },
    HelpEntry {
        name: "wifi saved",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Wifisaved,
        usage: "wifi saved",
        lines: &[
            "show whether the C6 currently has a station configuration;",
            "never prints the password or its length",
        ],
    },
    HelpEntry {
        name: "wifi forget",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Wififorget,
        usage: "wifi forget",
        lines: &["delete the station profile saved in C6 flash"],
    },
    HelpEntry {
        name: "wifi log",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Wifilog,
        usage: "wifi log",
        lines: &["show the last 16 Wi-Fi manager transitions and retry decisions"],
    },
    HelpEntry {
        name: "wifi disconnect",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Wifidisconnect,
        usage: "wifi disconnect",
        lines: &[
            "leave the current access point for this boot; unlike 'wifi off',",
            "Wi-Fi remains enabled and a saved profile remains in C6 flash",
        ],
    },
    HelpEntry {
        name: "netdump",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Netdump,
        usage: "netdump [tx|count]",
        lines: &[
            "show the ethernet header of each frame the C6 pushes at the",
            "host: destination MAC, source MAC and ethertype, each marked",
            "as addressed to us, broadcast or multicast. the check that the",
            "station interface really carries 802.3 frames. before an",
            "address is configured, nothing is addressed to us: with no IP,",
            "nothing on the network has a reason to talk to this station.",
            "'netdump tx' instead shows the last frames handed to the C6,",
            "which is the only way to see what this device actually sent",
        ],
    },
    HelpEntry {
        name: "ipconfig",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Ipconfig,
        usage: "ipconfig [dhcp|release|dns <a.b.c.d>...|<a.b.c.d[/len]> [gw]]",
        lines: &[
            "show the IPv4 settings, or change where they come from. 'dhcp'",
            "starts the client and waits for a lease; an address sets one by",
            "hand (a bare address means /24). 'dns' replaces the resolvers",
            "without touching the address, and with no address after it",
            "removes them. with no argument, only reports",
        ],
    },
    HelpEntry {
        name: "nslookup",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Nslookup,
        usage: "nslookup <name>",
        lines: &[
            "resolve a name to its A records. unlike the commands below,",
            "this always asks the resolver, even for something that would",
            "read as an address -- it is the way to test the resolver alone",
        ],
    },
    HelpEntry {
        name: "ping",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Ping,
        usage: "ping <host|a.b.c.d> [count]",
        lines: &[
            "send ICMP echo requests and time the replies; default 4. echo",
            "requests aimed at this device are answered whenever an address",
            "is configured, whether or not this command is running",
        ],
    },
    HelpEntry {
        name: "tftpget",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Tftpget,
        usage: "tftpget <host|a.b.c.d> <file>",
        lines: &[
            "read a file over TFTP (RFC 1350, 512-byte blocks, no options)",
            "into the current directory, under the last component of the",
            "remote name, and report its size and CRC-32. the bytes go",
            "straight to the volume as they arrive, so the size is not",
            "limited by memory. it is written under a .part name and renamed",
            "when it completes, so a file under the real name is a whole",
            "one; a failed transfer takes its .part file with it. an",
            "existing file of the same name is replaced. its .part file stays",
            "beside the destination so completion is a same-volume rename",
        ],
    },
    HelpEntry {
        name: "httpget",
        aliases: &[],
        group: Group::Scaffold,
        id: Cmd::Httpget,
        usage: "httpget <host|a.b.c.d>[:port] [path] | httpget <url>",
        lines: &[
            "issue a minimal HTTP/1.0 GET, print the status and headers, and",
            "save the body into the current directory under the last part of",
            "the path. a path naming no file -- / or one ending in / -- is",
            "not saved, just reported. a status outside 2xx keeps its error",
            "page out of the tree. still a TCP smoke test rather than an HTTP",
            "client: no redirects and no chunked decoding. a name given here",
            "is also what goes in the Host: header.",
            "  a bare host is plaintext, as it has always been. TLS needs an",
            "  explicit 'https://' url -- the command never decides on its",
            "  own to encrypt or not to. an https fetch is UNAUTHENTICATED:",
            "  it stops passive eavesdropping and nothing else (see 'tls')",
        ],
    },
    HelpEntry {
        name: "win",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Win,
        usage: "win",
        lines: &["open the desktop with the shared system bar"],
    },
    HelpEntry {
        name: "browser",
        aliases: &[],
        group: Group::Product,
        id: Cmd::Browser,
        usage: "browser [<url>]",
        lines: &[
            "open the hypertext viewer. with no argument it starts on its",
            "built-in home page, which needs no network; with one it fetches",
            "that address, which gets 'http://' if no scheme was typed. Tab",
            "selects a link and the status line shows where it goes, Enter",
            "follows it or -- with nothing selected -- opens the address",
            "field, Ctrl+L opens that field outright, Backspace goes back,",
            "the arrow keys and Page Up/Down scroll, Escape stops a load or",
            "closes the field, and Ctrl+Q (or 'q') leaves. touch and a USB",
            "mouse both select links. http only: an https address is",
            "reported and never downgraded",
        ],
    },
    HelpEntry {
        name: "bt",
        aliases: &["browsertest"],
        group: Group::Scaffold,
        id: Cmd::Bt,
        usage: "bt <url> [rounds]",
        lines: &[
            "read /manifest.txt from the fixture server named here, then",
            "fetch every endpoint in it through the same code the browser",
            "screen uses and check each one against what the manifest says",
            "it should do. the url gets 'http://' when no scheme is typed,",
            "so 'bt 192.168.0.2:8080' is the whole of it. prints one line",
            "per endpoint on the uart -- outcome, text crc32, blocks, links,",
            "bytes, redirect hops, peak owned memory, elapsed -- and only",
            "the failures plus a summary on the console. 'rounds' repeats",
            "the whole walk, which is how the socket and heap leak checks",
            "are run",
        ],
    },
    HelpEntry {
        name: "hs",
        aliases: &["httpstream"],
        group: Group::Scaffold,
        id: Cmd::Hs,
        usage: "hs <url> [r <n>|p [n]|c <n>]",
        lines: &[
            "fetch a url through the interruptible transaction the browser",
            "uses, and print what it came to: status, how the body was",
            "framed, decoded body bytes, a crc32 of them, elapsed time and",
            "the number of polls it took. the url is parsed by the browser's",
            "own parser, so the host connected to, the Host: header and the",
            "request target all come from one value. 'http://' is supplied",
            "when no scheme was typed.",
            "  p [n]  also build the document -- title, blocks, runs, links,",
            "         text bytes and what it all costs -- without drawing",
            "  r <n>  fetch n times",
            "  c <n>  start and abandon n times",
            "each reports whether the socket set and the heap came back to",
            "where they started. an https url is fetched over unauthenticated",
            "TLS -- encrypted, with nobody identified (see 'tls')",
        ],
    },
    HelpEntry {
        name: "reboot",
        aliases: &["reset"],
        group: Group::Product,
        id: Cmd::Reboot,
        usage: "reboot",
        lines: &["restart the device"],
    },
];

/// What the shell keeps between one command and the next.
///
/// Only the current directory so far. It lives here rather than in the
/// `Vfs` because it is an interface convenience, not a property of any
/// filesystem: the VFS takes absolute paths from every caller, and putting
/// a current directory down there would raise the question of whose it is
/// as soon as something other than this shell opened a file.
///
/// It is a path and nothing more. Unmounting the volume it points at, or
/// swapping the medium under it, leaves it pointing where it pointed;
/// commands then fail with "no filesystem mounted on that path" until
/// something is mounted there again, at which point it works once more.
/// The alternative -- resetting it to the root -- would mean `umount`, the
/// media check and every future automatic unmount all reaching into the
/// shell's state to fix up a string.
pub struct State {
    cwd: Path,
    /// Files held open across commands by `fsopen`.
    ///
    /// Every other command that opens a file closes it before it returns, so
    /// until this existed there was no way from the console to have a handle
    /// open at the moment a drive was pulled -- and therefore no way to see
    /// that pulling it makes the handle fail, which is the behaviour the
    /// automatic unmount rests on. The slot index here is the number the
    /// user types; it is not the VFS's own, which stays private.
    ///
    /// Sized to `MAX_OPEN_FILES` because the VFS will not give out more than
    /// that anyway, so a longer array here could only ever hold `None`.
    held: [Option<FileHandle>; MAX_OPEN_FILES],
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        const NONE_HANDLE: Option<FileHandle> = None;
        Self {
            cwd: path::root(),
            held: [NONE_HANDLE; MAX_OPEN_FILES],
        }
    }
}

/// What the foreground application loop should do once a command has
/// been dispatched.
///
/// Not `Copy`: `Browser` carries the address it was given, and a parsed
/// `Url` owns its strings. Moving the outcome once, into the `match` in
/// `app::run`, is all anything does with it.
#[derive(Clone, Eq, PartialEq)]
pub enum Outcome {
    /// Open the normal GUI desktop.
    Desktop,
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
    /// Hand the display over to the 16 pixel font sheet.
    FontTest,
    /// Hand the display over to the BMI270 tilt diagnostic screen.
    AxisTest,
    /// Hand the display over to the hypertext viewer, on the address
    /// given or on its built-in home page.
    Browser(Option<Url>),
    /// Run all interactive full-screen visual checks in one sequence.
    VisualQa,
}

/// Parses and runs one command line, returning what the caller should do
/// next.
///
/// `usb_host` is the single registry the application's frame loop owns
/// (`docs/plans/archive/USB_REFACTOR_PLAN.md` Stage A) -- every USB command reads or drives
/// devices already in it instead of probing the bus independently, which
/// is what used to let a diagnostic command reset a live keyboard/Mass
/// Storage session out from under itself.
pub fn execute(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    line: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &mut State,
    auto_mount: &mut AutoMount,
    wifi_manager: &mut WifiManager,
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
    let mut argument = trim(rest);
    let mut qualified = String::new();
    let command = if command == b"wifi" && !argument.is_empty() {
        let (subcommand, rest) = split_first_word(argument);
        qualified.push_str("wifi ");
        qualified.push_str(as_str(subcommand));
        argument = trim(rest);
        qualified.as_bytes()
    } else {
        command
    };
    // A name becomes a command in exactly one place, and it is the same
    // table `help` reads. Nothing below can dispatch a command the listing
    // does not know about, and nothing listed can be missing a body: the
    // match is over `Cmd`, so the compiler requires an arm for every
    // variant the table can produce.
    let Some(entry) = lookup(command) else {
        console.write_output_line(framebuffer, "unknown command (try 'help')");
        return Outcome::Continue;
    };
    if entry.name.starts_with("wifi ")
        && !matches!(entry.id, Cmd::Wificonnect)
        && !argument.is_empty()
    {
        console.write_output_line(framebuffer, entry.usage);
        return Outcome::Continue;
    }
    // GUI radio jobs retain their one RPC slot across a return to Console.
    // Commands that need that slot are rejected before borrowing any handle.
    if wifi_manager.gui_busy()
        && matches!(
            entry.id,
            Cmd::Wifion
                | Cmd::Wifioff
                | Cmd::Wifiinfo
                | Cmd::Wifistatus
                | Cmd::Wifiup
                | Cmd::Wifimac
                | Cmd::Wifiscan
                | Cmd::Wificonnect
                | Cmd::Wifisaved
                | Cmd::Wififorget
                | Cmd::Wifidisconnect
                | Cmd::Ipconfig
                | Cmd::Tls
                | Cmd::Nslookup
                | Cmd::Ping
                | Cmd::Tftpget
                | Cmd::Httpget
                | Cmd::Bt
        )
    {
        console.write_output_line(
            framebuffer,
            "Wi-Fi operation pending; retry after completion.",
        );
        return Outcome::Continue;
    }
    match entry.id {
        Cmd::Help => cmd_help(console, framebuffer, argument),
        Cmd::Clear => console.clear(framebuffer),
        Cmd::Echo => console.write_output_line(framebuffer, as_str(argument)),
        Cmd::About => console.write_output_line(framebuffer, "Tab5 Shell 0.1"),
        Cmd::Cpuinfo => cmd_cpuinfo(console, framebuffer),
        Cmd::Pma => cmd_pma(console, framebuffer),
        Cmd::Pmp => cmd_pmp(console, framebuffer),
        Cmd::Mem => cmd_mem(console, framebuffer),
        Cmd::Alloctest => cmd_alloctest(console, framebuffer, argument),
        Cmd::Uptime => cmd_uptime(console, framebuffer),
        Cmd::Membench => cmd_membench(console, framebuffer),
        Cmd::Stress => cmd_stress(console, framebuffer, argument),
        Cmd::Displaybench => cmd_displaybench(console, framebuffer, argument),
        Cmd::Db => cmd_displaybench_suite(console, framebuffer, argument),
        Cmd::Dp => cmd_displaybench_production(console, framebuffer, argument),
        Cmd::Di => cmd_display_idle_soak(console, framebuffer, argument),
        Cmd::Ui => {
            if argument.is_empty() {
                cmd_ui_scroll_bench(console, framebuffer);
                return Outcome::VisualQa;
            }
            console.write_output_line(framebuffer, "usage: ui");
        }
        Cmd::Mix => cmd_mixed_soak(console, framebuffer, argument, usb_host),
        Cmd::Ut => cmd_usb_read_test(console, framebuffer, argument, usb_host),
        Cmd::Usbmargin => cmd_usb_margin(console, framebuffer, argument, usb_host),
        Cmd::Pf => {
            if argument.is_empty() {
                psram::request_fallback_test();
                console.write_output_line(framebuffer, "forcing one 200-to-80 MHz fallback...");
                return Outcome::Reboot;
            }
            console.write_output_line(framebuffer, "usage: pf");
        }
        Cmd::Rt => {
            let count = if argument.is_empty() {
                Some(20)
            } else {
                parse_u32(argument)
            };
            if let Some(count) = count {
                if startup::request_reboot_test(count) {
                    let mut line = Line::new();
                    line.push_str("automatic reboot test: ");
                    line.push_u32(count);
                    line.push_str(" boots...");
                    console.write_output_line(framebuffer, line.as_str());
                    return Outcome::Reboot;
                }
            }
            console.write_output_line(framebuffer, "usage: rt [1-100]");
        }
        Cmd::Backlight => cmd_backlight(console, framebuffer, argument),
        Cmd::Icm => cmd_icm(console, framebuffer, argument),
        Cmd::Ppafill => cmd_ppafill(console, framebuffer, argument),
        Cmd::Entropy => cmd_entropy(console, framebuffer, argument),
        Cmd::Rtc => cmd_rtc(console, framebuffer, argument),
        Cmd::Sdinfo => cmd_sdinfo(console, framebuffer),
        Cmd::Sdread => cmd_sdread(console, framebuffer, argument),
        Cmd::Sdreadn => cmd_sdreadn(console, framebuffer, argument),
        Cmd::Sdwritetest => cmd_sdwritetest(console, framebuffer, argument),
        Cmd::Sdzero => cmd_sdzero(console, framebuffer, argument),
        Cmd::Sdmbr => cmd_sdmbr(console, framebuffer),
        Cmd::Devices => cmd_devices(console, framebuffer, usb_host, ram_disk),
        Cmd::Blkread => cmd_blkread(console, framebuffer, argument, usb_host, ram_disk),
        Cmd::Mounts => files::show_mounts(console, framebuffer, vfs),
        Cmd::Df => {
            let count = match trim(argument) {
                b"" => Some(false),
                b"-c" => Some(true),
                _ => None,
            };
            match count {
                Some(count) => with_devices(usb_host, ram_disk, |devices| {
                    files::show_usage(console, framebuffer, devices, vfs, count)
                }),
                None => console.write_output_line(framebuffer, "usage: df [-c]"),
            }
        }
        Cmd::Fsverify => {
            with_devices(usb_host, ram_disk, |devices| {
                files::verify(console, framebuffer, devices, vfs)
            });
        }
        Cmd::Mount => cmd_mount(console, framebuffer, argument, usb_host, ram_disk, vfs),
        Cmd::Automount => cmd_automount(console, framebuffer, argument, auto_mount),
        Cmd::Umount => {
            if let Some(path) = single_argument(
                console,
                framebuffer,
                argument,
                "usage: umount <mount point>",
            )
            .and_then(|path| absolute(console, framebuffer, state, path))
            {
                files::unmount(console, framebuffer, vfs, path.as_str());
            }
        }
        Cmd::Cd => cmd_cd(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
        ),
        Cmd::Pwd => console.write_output_line(framebuffer, state.cwd.as_str()),
        Cmd::Ls => cmd_ls(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
        ),
        Cmd::Cat => cmd_cat(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
        ),
        Cmd::Fsopen => cmd_fsopen(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
        ),
        Cmd::Fsread => cmd_fsread(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
        ),
        Cmd::Fsclose => cmd_fsclose(console, framebuffer, argument, vfs, state),
        Cmd::Rm => cmd_remove(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
            false,
        ),
        Cmd::Rmdir => cmd_remove(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
            true,
        ),
        Cmd::Mv => cmd_move(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
        ),
        Cmd::Fswritetest => cmd_fswritetest(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
        ),
        Cmd::Fill => cmd_fill(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
        ),
        Cmd::Write => cmd_write(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
            fs::vfs::OpenMode::Truncate,
        ),
        Cmd::Append => cmd_write(
            console,
            framebuffer,
            argument,
            usb_host,
            ram_disk,
            vfs,
            state,
            fs::vfs::OpenMode::Append,
        ),
        Cmd::Mkdir => {
            if let Some(path) =
                single_argument(console, framebuffer, argument, "usage: mkdir <path>")
                    .and_then(|path| absolute(console, framebuffer, state, path))
            {
                with_devices(usb_host, ram_disk, |devices| {
                    files::make_directory(console, framebuffer, devices, vfs, path.as_str())
                });
            }
        }
        Cmd::Sdreadpsram => cmd_sdreadpsram(console, framebuffer, argument),
        Cmd::Lsusb => cmd_lsusb(console, framebuffer, argument, usb_host),
        Cmd::Usbinfo => cmd_usbinfo(console, framebuffer, usb_host),
        Cmd::Usbrescan => cmd_usbrescan(console, framebuffer, usb_host),
        Cmd::Usbfs => cmd_usbfs(console, framebuffer, argument, usb_host),
        Cmd::Usbvbus => cmd_usbvbus(console, framebuffer, argument),
        Cmd::Usbhub => cmd_usbhub(console, framebuffer, usb_host),
        Cmd::Usbcheck => cmd_usbcheck(console, framebuffer, argument, usb_host),
        Cmd::Usbrawcheck => cmd_usbrawcheck(console, framebuffer, argument, usb_host),
        Cmd::Usbmultiwrite => cmd_usbmultiwrite(console, framebuffer, argument, usb_host),
        Cmd::Usbcachefail => cmd_usbcachefail(console, framebuffer, usb_host),
        Cmd::Usbhw => cmd_usbhw(console, framebuffer, usb_host),
        Cmd::Usbperiodic => cmd_usbperiodic(console, framebuffer, usb_host),
        Cmd::Usbmsc => cmd_usbmsc(console, framebuffer, usb_host),
        Cmd::Usbread => cmd_usbread(console, framebuffer, argument, usb_host),
        Cmd::Usbwritetest => cmd_usb_write_test(console, framebuffer, argument, usb_host),
        Cmd::Usbzero => cmd_usbzero(console, framebuffer, argument, usb_host),
        Cmd::Usbmbr => cmd_usbmbr(console, framebuffer, usb_host),
        Cmd::Wifi => cmd_help(console, framebuffer, b"wifi"),
        Cmd::Wifion | Cmd::Wifioff => {
            cmd_wifi_control(
                console,
                framebuffer,
                if matches!(entry.id, Cmd::Wifion) {
                    b"on"
                } else {
                    b"off"
                },
                wifi_manager,
            );
            drop_dead_session(console, framebuffer, wifi_manager);
        }
        Cmd::Wifiinfo => {
            let was_enabled = wifi_manager.is_enabled();
            wifi_manager.clear_link();
            cmd_wifiinfo(console, framebuffer);
            restore_after_wifi_diagnostic(console, framebuffer, wifi_manager, was_enabled);
        }
        Cmd::Wifiup => {
            let was_enabled = wifi_manager.is_enabled();
            wifi_manager.clear_link();
            cmd_wifiup(console, framebuffer);
            restore_after_wifi_diagnostic(console, framebuffer, wifi_manager, was_enabled);
        }
        Cmd::Wifimac => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let (wifi_session, _) = wifi_manager.options_mut();
                cmd_wifimac(console, framebuffer, wifi_session);
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Wifiscan => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let (wifi_session, _) = wifi_manager.options_mut();
                cmd_wifiscan(console, framebuffer, wifi_session);
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Wificonnect => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let result = cmd_wificonnect(console, framebuffer, argument, wifi_manager);
                match result {
                    ShellConnect::Associated(association) => {
                        wifi_manager.begin_shell_attempt();
                        wifi_manager.mark_shell_associated(association)
                    }
                    ShellConnect::Failed(failure) => {
                        wifi_manager.begin_shell_attempt();
                        wifi_manager.mark_failed(failure)
                    }
                    ShellConnect::NotStarted => {}
                }
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Wifistatus => {
            cmd_wifi_control(console, framebuffer, b"status", wifi_manager);
            if wifi_manager.is_enabled() && wifi_command_allowed(console, framebuffer, wifi_manager)
            {
                let (wifi_session, _) = wifi_manager.options_mut();
                cmd_wifistatus(console, framebuffer, wifi_session);
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Wifisaved => {
            if wifi_manager.is_enabled() {
                cmd_wifisaved(console, framebuffer, wifi_manager);
                drop_dead_session(console, framebuffer, wifi_manager);
            } else {
                console.write_output_line(
                    framebuffer,
                    if wifi_manager.has_saved_profile() {
                        "Wi-Fi is off; a saved profile is present"
                    } else {
                        "Wi-Fi is off; no saved profile is present"
                    },
                );
            }
        }
        Cmd::Wififorget => {
            cmd_wififorget(console, framebuffer, wifi_manager);
            drop_dead_session(console, framebuffer, wifi_manager);
        }
        Cmd::Wifilog => {
            wifi_manager.service();
            cmd_wifilog(console, framebuffer, wifi_manager);
            drop_dead_session(console, framebuffer, wifi_manager);
        }
        Cmd::Wifidisconnect => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let disconnected = {
                    let (wifi_session, _) = wifi_manager.options_mut();
                    cmd_wifidisconnect(console, framebuffer, wifi_session)
                };
                if disconnected {
                    wifi_manager.mark_disconnected();
                    console.write_output_line(
                        framebuffer,
                        "Wi-Fi remains on; saved auto-connect resumes after reboot",
                    );
                }
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Netdump => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let (wifi_session, _) = wifi_manager.options_mut();
                cmd_netdump(console, framebuffer, argument, wifi_session);
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Ipconfig => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let policy = {
                    let (wifi_session, net_stack) = wifi_manager.options_mut();
                    cmd_ipconfig(console, framebuffer, argument, wifi_session, net_stack)
                };
                if let Some(policy) = policy {
                    wifi_manager.set_ip_policy(policy);
                }
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Nslookup => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let (wifi_session, net_stack) = wifi_manager.options_mut();
                cmd_nslookup(console, framebuffer, argument, wifi_session, net_stack);
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Ping => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let (wifi_session, net_stack) = wifi_manager.options_mut();
                cmd_ping(console, framebuffer, argument, wifi_session, net_stack);
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Tftpget => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let (wifi_session, net_stack) = wifi_manager.options_mut();
                cmd_tftpget(
                    console,
                    framebuffer,
                    argument,
                    usb_host,
                    ram_disk,
                    vfs,
                    state,
                    wifi_session,
                    net_stack,
                );
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Bt => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let (wifi_session, net_stack) = wifi_manager.options_mut();
                cmd_browsertest(console, framebuffer, argument, wifi_session, net_stack);
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Hs => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let (wifi_session, net_stack) = wifi_manager.options_mut();
                cmd_httpstream(console, framebuffer, argument, wifi_session, net_stack);
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Httpget => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let (wifi_session, net_stack) = wifi_manager.options_mut();
                cmd_httpget(
                    console,
                    framebuffer,
                    argument,
                    usb_host,
                    ram_disk,
                    vfs,
                    state,
                    wifi_session,
                    net_stack,
                );
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Tls => {
            if wifi_command_allowed(console, framebuffer, wifi_manager) {
                let (wifi_session, net_stack) = wifi_manager.options_mut();
                cmd_tls(console, framebuffer, argument, wifi_session, net_stack);
                drop_dead_session(console, framebuffer, wifi_manager);
            }
        }
        Cmd::Paint => return Outcome::Paint,
        Cmd::Touchtest => return Outcome::TouchTest,
        Cmd::Touchcheck => {
            if argument.is_empty() {
                return Outcome::Browser(Some(
                    Url::parse("http://built-in/long").expect("built-in touchcheck URL"),
                ));
            }
            console.write_output_line(framebuffer, "usage: touchcheck");
        }
        Cmd::Coordtest => return Outcome::CoordTest,
        Cmd::Fonttest => return Outcome::FontTest,
        Cmd::Axistest => return Outcome::AxisTest,
        Cmd::Win => {
            if argument.is_empty() {
                return Outcome::Desktop;
            }
            console.write_output_line(framebuffer, "usage: win");
        }
        Cmd::Browser => {
            let argument = trim(argument);
            // Not refused without a network: the built-in pages are in
            // flash and are exactly what somebody wants to look at when the
            // link is down. What is missing is said here, in the shell,
            // where the commands that fix it are.
            browser_readiness(console, framebuffer, wifi_manager.stack());
            if argument.is_empty() {
                return Outcome::Browser(None);
            }
            match resolve_address(console, framebuffer, argument) {
                Some(url) => return Outcome::Browser(Some(url)),
                None => return Outcome::Continue,
            }
        }
        Cmd::Reboot => {
            console.write_output_line(framebuffer, "rebooting...");
            return Outcome::Reboot;
        }
        Cmd::Shutdown => {
            if argument.is_empty() {
                console.write_output_line(framebuffer, "shutting down...");
                return Outcome::Shutdown;
            }
            console.write_output_line(framebuffer, "usage: shutdown");
        }
    }
    Outcome::Continue
}

/// Reboots the board. The caller must have already flushed the "rebooting..."
/// output to the panel; this never returns.
/// Restarts the board, stopping first whatever a reset on its own would
/// leave running.
///
/// `startup::reboot` resets only the HP CPU core, so everything outside it
/// keeps the state this boot gave it. Each such thing is stopped here
/// rather than at the next boot, because the next boot is already too late:
/// the bootloader runs before any of this firmware does.
pub fn reboot(session: Option<&mut wifi::Rpc>) -> ! {
    // The C6 is not reset at all, so it stays associated to the access
    // point across the reboot and then vanishes mid-association when the
    // next `sdio::init` pulses its reset line. The access point is left
    // holding an entry for a station that stopped answering, and its
    // inactivity timeout for that entry lands on the *next* association --
    // which is why the first `wifi connect` after a reboot fails with
    // reason 4 while the second succeeds. Leaving properly costs one frame.
    if let Some(rpc) = session
        && let Some(0) = wifi::station::disconnect(rpc)
    {
        // Only long enough for the slave to report the frame went out; the
        // console is about to disappear, so the outcome is not worth
        // showing.
        let _ = wifi::station::wait_for_connection(rpc, REBOOT_DISCONNECT_TIMEOUT_MS);
    }

    // Scanout would otherwise keep reading PSRAM -- at the raised
    // interconnect priority this firmware gave it -- right through the
    // bootloader's flash reads and the next boot's PSRAM bring-up. The boot
    // path quiesces it too, for resets that never reach here, but that is
    // too late to help the bootloader.
    lcd::quiesce_dma();
    startup::reboot()
}

/// How long [`reboot`] waits for the C6 to confirm it has left the access
/// point. Shorter than `DISCONNECT_TIMEOUT_MS`: this is not a command whose
/// result anyone reads, and the deauthentication is a single frame.
const REBOOT_DISCONNECT_TIMEOUT_MS: u32 = 1_000;

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
/// Finds a command by the name that was typed, which may be an alias.
///
/// The one place a console name becomes a [`Cmd`]. `execute` and `help`
/// both go through it, so the set of names that run something and the set
/// of names `help` can describe are the same set by construction.
fn lookup(name: &[u8]) -> Option<&'static HelpEntry> {
    let name = as_str(name);
    HELP_ENTRIES
        .iter()
        .find(|entry| entry.name == name || entry.aliases.contains(&name))
}

/// Writes one group's command names as a single line. The console wraps at
/// its own width, so this stays one call however long the group gets.
fn list_group(console: &mut Console, framebuffer: &mut Framebuffer, group: Group) {
    let mut names = String::new();
    for entry in HELP_ENTRIES
        .iter()
        .filter(|entry| entry.group == group && !entry.name.contains(' '))
    {
        if !names.is_empty() {
            names.push(' ');
        }
        names.push_str(entry.name);
    }
    console.write_output_line(framebuffer, &names);
}

fn cmd_help(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    if argument.is_empty() {
        console.write_output_line(
            framebuffer,
            "commands (help <name> for details; help all also lists the diagnostic ones):",
        );
        list_group(console, framebuffer, Group::Product);
        return;
    }

    // Checked before command names. Nothing is called `all`, and nothing
    // may be: a command whose name is shadowed here could not be described
    // from the console.
    if argument == b"all" {
        console.write_output_line(framebuffer, "commands:");
        list_group(console, framebuffer, Group::Product);
        console.write_output_line(
            framebuffer,
            "diagnostics and benchmarks (development scaffolding):",
        );
        list_group(console, framebuffer, Group::Scaffold);
        return;
    }

    match lookup(argument) {
        Some(entry) => {
            console.write_output_line(framebuffer, entry.usage);
            if !entry.aliases.is_empty() {
                let mut line = String::from("also: ");
                for (index, alias) in entry.aliases.iter().enumerate() {
                    if index > 0 {
                        line.push(' ');
                    }
                    line.push_str(alias);
                }
                console.write_output_line(framebuffer, &line);
            }
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
    line.push_str("ram disk: ");
    line.push_u32(psram::RAM_DISK_BYTES as u32);
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

/// Decodes the bootloader-installed PMA CSRs into the ranges they match.
///
/// PMA entries are not a conventional linear table: an OFF entry has no
/// region of its own, but its address register supplies the lower bound for
/// the next Top Of Range (TOR) entry.  Keeping those rows in the output makes
/// that dependency explicit and avoids presenting a misleading gap map.
fn cmd_pma(console: &mut Console, framebuffer: &mut Framebuffer) {
    console.write_output_line(framebuffer, "PMA map (ranges are [start,end)):");
    console.write_output_line(
        framebuffer,
        "# range               mode  rwx E L cache          cfg",
    );

    for entry in pma::entries() {
        let mut line = Line::new();
        line.push_u32(entry.index as u32);
        line.push_str(" ");
        match entry.range {
            Some(range) => {
                push_address(&mut line, range.start);
                line.push_str("..");
                push_address(&mut line, range.end);
            }
            None => {
                line.push_str("off@ ");
                push_address(&mut line, entry.address_bytes());
            }
        }
        pad_to(&mut line, 22);
        line.push_str(entry.mode_name());
        pad_to(&mut line, 28);
        line.push_str(if entry.readable() { "R" } else { "-" });
        line.push_str(if entry.writable() { "W" } else { "-" });
        line.push_str(if entry.executable() { "X" } else { "-" });
        line.push_str(if entry.enabled() { " E" } else { " -" });
        line.push_str(if entry.locked() { " L " } else { " - " });
        push_cache_attributes(&mut line, entry);
        pad_to(&mut line, 51);
        line.push_hex(entry.config, 8);
        console.write_output_line(framebuffer, line.as_str());
    }
}

/// Appends an address without a `0x` prefix so PMA and PMP map rows stay
/// compact.
/// Eight digits cover the ESP32-P4 address space; a ninth digit appears only
/// for a half-open range ending one byte past it.
fn push_address(line: &mut Line, address: u64) {
    if address <= u32::MAX as u64 {
        line.push_hex(address as u32, 8);
    } else {
        line.push_u64_hex(address);
    }
}

/// Appends the cacheability portion of one PMA entry's attributes.
fn push_cache_attributes(line: &mut Line, entry: pma::Entry) {
    let mut has_explicit_policy = false;
    if entry.non_cacheable() {
        line.push_str("NC");
        has_explicit_policy = true;
    }
    if entry.write_through() {
        if has_explicit_policy {
            line.push_str(" ");
        }
        line.push_str("WT");
        has_explicit_policy = true;
    }
    if !has_explicit_policy {
        line.push_str("WB");
    }
    if entry.write_miss_no_alloc() {
        line.push_str(" WNA");
    }
    if entry.read_miss_no_alloc() {
        line.push_str(" RNA");
    }
}

/// Decodes the bootloader-installed PMP CSRs into the ranges they match.
///
/// PMP answers a different question from `pma`: what may be read, written or
/// executed where, rather than how the memory behaves.  Two properties make
/// the table easy to misread, so the output states both.  Entries are
/// priority-ordered -- the lowest-numbered matching entry decides an access
/// and any overlap by a later entry is dead -- and machine mode, which is the
/// only mode this firmware ever runs in, ignores entries whose lock bit is
/// clear as well as addresses that match no entry at all.
fn cmd_pmp(console: &mut Console, framebuffer: &mut Framebuffer) {
    console.write_output_line(framebuffer, "PMP map (ranges are [start,end)):");
    console.write_output_line(framebuffer, "# range               mode  rwx L cfg");

    for entry in pmp::entries() {
        let mut line = Line::new();
        line.push_u32(entry.index as u32);
        line.push_str(" ");
        match entry.range {
            Some(range) => {
                push_address(&mut line, range.start);
                line.push_str("..");
                push_address(&mut line, range.end);
            }
            None => {
                line.push_str("off@ ");
                push_address(&mut line, entry.address_bytes());
            }
        }
        // A space before each padded column keeps the row readable even when
        // a range runs one digit past the end of the 32-bit address space and
        // leaves no room for padding.
        line.push_str(" ");
        pad_to(&mut line, 22);
        line.push_str(entry.mode_name());
        line.push_str(" ");
        pad_to(&mut line, 28);
        line.push_str(if entry.readable() { "R" } else { "-" });
        line.push_str(if entry.writable() { "W" } else { "-" });
        line.push_str(if entry.executable() { "X" } else { "-" });
        line.push_str(if entry.locked() { " L " } else { " - " });
        line.push_hex(entry.config as u32, 2);
        // A TOR bound at or below the previous entry's bound matches nothing,
        // which the range column alone would not make obvious.
        if entry.range.is_some_and(pmp::Range::is_empty) {
            line.push_str(" empty");
        }
        console.write_output_line(framebuffer, line.as_str());
    }

    let mut line = Line::new();
    line.push_str("granularity ");
    line.push_u32(pmp::GRANULARITY);
    line.push_str(" B; machine mode obeys locked entries only");
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

/// Bytes of PSRAM past the framebuffer, matching `Psram::heap`'s split
/// and backing the global allocator installed in `main`.
fn heap_bytes() -> usize {
    // The three PSRAM spans in order: the framebuffer, the RAM disk, and the
    // heap with whatever is left. `Psram::heap` does the same arithmetic on
    // the mapping it actually got; this is the compile-time view, which is
    // the same number whenever the mapping came up at its full size.
    psram::MAPPED_BYTES - psram::FRAMEBUFFER_BYTES - psram::RAM_DISK_BYTES
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum DisplayBenchMode {
    Idle,
    Sync,
    Cpu,
    PpaRaw,
    PpaSafe,
    Production,
}

impl DisplayBenchMode {
    fn parse(value: &[u8]) -> Option<Self> {
        match value {
            b"idle" => Some(Self::Idle),
            b"sync" => Some(Self::Sync),
            b"cpu" => Some(Self::Cpu),
            b"ppa-raw" => Some(Self::PpaRaw),
            b"ppa-safe" => Some(Self::PpaSafe),
            b"production" => Some(Self::Production),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Sync => "sync",
            Self::Cpu => "cpu",
            Self::PpaRaw => "ppa-raw",
            Self::PpaSafe => "ppa-safe",
            Self::Production => "production",
        }
    }
}

/// Separates the framebuffer's competing traffic sources into reproducible,
/// fixed-count full-screen workloads.
///
/// The existing `stress` command deliberately keeps the production call
/// sequence. This command is the diagnostic counterpart: a mode names one
/// exact path, every operation starts at a known frame phase, and the sticky
/// bridge indication is consumed before another operation can hide behind it.
fn cmd_displaybench(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    const USAGE: &str = "usage: displaybench <mode> [count] [phase_ms] [burst] (see help)";
    let (mode, rest) = split_first_word(trim(argument));
    let Some(mode) = DisplayBenchMode::parse(mode) else {
        console.write_output_line(framebuffer, USAGE);
        return;
    };
    let (count_arg, rest) = split_first_word(trim(rest));
    let (phase_arg, rest) = split_first_word(trim(rest));
    let (burst_arg, rest) = split_first_word(trim(rest));
    if !trim(rest).is_empty() {
        console.write_output_line(framebuffer, USAGE);
        return;
    }

    let count = if count_arg.is_empty() {
        100
    } else {
        match parse_u32(count_arg) {
            Some(value) if value > 0 && value <= 200_000 => value,
            _ => {
                console.write_output_line(framebuffer, "count must be 1-200000");
                return;
            }
        }
    };
    let phase_ms = if phase_arg.is_empty() {
        0
    } else {
        match parse_u32(phase_arg) {
            Some(value @ (0 | 3 | 8 | 12)) => value,
            _ => {
                console.write_output_line(framebuffer, "phase_ms must be 0, 3, 8, or 12");
                return;
            }
        }
    };
    let burst = if burst_arg.is_empty() {
        dma2d::diagnostic_burst_bytes()
    } else {
        match parse_u32(burst_arg) {
            Some(value @ (8 | 16 | 32 | 64 | 128)) => value,
            _ => {
                console.write_output_line(framebuffer, "burst must be 8, 16, 32, 64, or 128");
                return;
            }
        }
    };

    let result = run_display_bench(framebuffer, mode, count, phase_ms, burst);
    console.clear(framebuffer);
    report_display_bench(console, framebuffer, &result);
}

#[derive(Clone, Copy)]
struct DisplayBenchResult {
    mode: DisplayBenchMode,
    count: u32,
    phase_ms: u32,
    burst: u32,
    completed: u32,
    frames: u32,
    mean_us: u32,
    underruns: u32,
}

fn run_display_bench(
    framebuffer: &mut Framebuffer,
    mode: DisplayBenchMode,
    count: u32,
    phase_ms: u32,
    burst: u32,
) -> DisplayBenchResult {
    // A raw PPA run is safe only if the CPU has no framebuffer cache line to
    // evict over its result. Flush/invalidate once before the loop, then keep
    // every CPU pixel access out until the last raw transfer has completed.
    if mode == DisplayBenchMode::PpaRaw && !framebuffer.flush() {
        return DisplayBenchResult {
            mode,
            count,
            phase_ms,
            burst,
            completed: 0,
            frames: 0,
            mean_us: 0,
            underruns: 0,
        };
    }

    let previous_burst = dma2d::diagnostic_set_burst_bytes(burst).unwrap_or(128);
    lcd::take_underrun();
    let first_frame = interrupts::frame_sequence();
    let mut elapsed_cycles = 0u64;
    let mut underruns = 0u32;
    let mut completed = 0u32;

    for index in 0..count {
        if mode == DisplayBenchMode::Idle {
            let start = membench::cycles();
            let succeeded = wait_for_next_display_frame();
            elapsed_cycles += membench::cycles().wrapping_sub(start) as u64;
            if !succeeded {
                break;
            }
            completed += 1;
            if lcd::take_underrun() {
                underruns += 1;
            }
            continue;
        }

        if !wait_for_next_display_frame() {
            break;
        }
        delay::delay_ms(phase_ms);
        // Exclude an idle-frame indication left before this operation. The
        // result is collected only after the following boundary, so an
        // underrun late in the frame still belongs to the operation which
        // actually provoked it rather than to the next loop iteration.
        lcd::take_underrun();
        // Alternate values so every operation really writes the full screen.
        // Avoid blue here: the Bridge's hardware underrun output is light
        // blue, and using a legitimate dark-blue test frame made the two
        // visually different events easy to report as the same failure.
        let color = if index % 2 == 0 {
            crate::framebuffer::BLACK
        } else {
            crate::framebuffer::RED
        };
        let start = membench::cycles();
        let succeeded = match mode {
            DisplayBenchMode::Idle => false,
            DisplayBenchMode::Sync => framebuffer.flush(),
            DisplayBenchMode::Cpu => {
                framebuffer.diagnostic_fill_rect_with_cpu(
                    0,
                    0,
                    crate::framebuffer::WIDTH,
                    crate::framebuffer::HEIGHT,
                    color,
                ) && framebuffer.flush()
            }
            DisplayBenchMode::PpaRaw => framebuffer.diagnostic_ppa_fill_rect_raw(
                0,
                0,
                crate::framebuffer::WIDTH,
                crate::framebuffer::HEIGHT,
                color,
            ),
            DisplayBenchMode::PpaSafe => framebuffer.ppa_fill_rect(
                0,
                0,
                crate::framebuffer::WIDTH,
                crate::framebuffer::HEIGHT,
                color,
            ),
            DisplayBenchMode::Production => {
                framebuffer.fill(color);
                framebuffer.flush()
            }
        };
        elapsed_cycles += membench::cycles().wrapping_sub(start) as u64;
        if !succeeded || !wait_for_next_display_frame() {
            break;
        }
        completed += 1;
        if lcd::take_underrun() {
            underruns += 1;
        }
    }
    let frames = interrupts::frame_sequence().wrapping_sub(first_frame);
    let _ = dma2d::diagnostic_set_burst_bytes(previous_burst);

    // Restore a conventional cache contract before the next mode or the
    // console repaints over a raw DMA result. This is outside the timed part.
    if mode == DisplayBenchMode::PpaRaw {
        let _ = framebuffer.flush();
    }

    let total_us = elapsed_cycles * 1_000_000 / startup::cpu_hz() as u64;
    DisplayBenchResult {
        mode,
        count,
        phase_ms,
        burst,
        completed,
        frames,
        mean_us: if completed == 0 {
            0
        } else {
            (total_us / completed as u64) as u32
        },
        underruns,
    }
}

fn report_display_bench(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    result: &DisplayBenchResult,
) {
    let mut line = Line::new();
    line.push_str("displaybench: ");
    line.push_str(result.mode.name());
    line.push_str(" count=");
    line.push_u32(result.count);
    line.push_str(" phase=");
    line.push_u32(result.phase_ms);
    line.push_str("ms");
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("burst=");
    line.push_u32(result.burst);
    line.push_str(" completed=");
    line.push_u32(result.completed);
    line.push_str(" frames=");
    line.push_u32(result.frames);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("mean=");
    line.push_u32(result.mean_us);
    line.push_str("us underrun operations=");
    line.push_u32(result.underruns);
    line.push_str("/");
    line.push_u32(result.completed);
    console.write_output_line(framebuffer, line.as_str());

    if result.completed != result.count {
        console.write_output_line(framebuffer, "displaybench: operation or display DMA failed");
    }
}

/// Runs the complete Stage 0 matrix without repainting the console or asking
/// the user to enter each long command. Detailed single cases remain available
/// through `displaybench` when a later stage needs one variable repeated.
fn cmd_displaybench_suite(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let Some(count) =
        parse_bench_count(argument, "usage: db [count] (1-1000)", console, framebuffer)
    else {
        return;
    };
    const CASES: &[(DisplayBenchMode, u32, u32)] = &[
        (DisplayBenchMode::Idle, 0, 128),
        (DisplayBenchMode::Sync, 0, 128),
        (DisplayBenchMode::Cpu, 0, 128),
        (DisplayBenchMode::PpaRaw, 0, 128),
        (DisplayBenchMode::PpaSafe, 0, 128),
        (DisplayBenchMode::Production, 0, 128),
        (DisplayBenchMode::PpaSafe, 3, 128),
        (DisplayBenchMode::PpaSafe, 8, 128),
        (DisplayBenchMode::PpaSafe, 12, 128),
        (DisplayBenchMode::PpaSafe, 0, 8),
        (DisplayBenchMode::PpaSafe, 0, 16),
        (DisplayBenchMode::PpaSafe, 0, 32),
        (DisplayBenchMode::PpaSafe, 0, 64),
    ];

    icm::set_display_priority(15, 15);
    let mut results = Vec::new();
    if results.try_reserve_exact(CASES.len()).is_err() {
        console.write_output_line(framebuffer, "db: result allocation failed");
        return;
    }
    for &(mode, phase_ms, burst) in CASES {
        results.push(run_display_bench(framebuffer, mode, count, phase_ms, burst));
    }
    console.clear(framebuffer);

    let mut line = Line::new();
    line.push_str("db: standard suite count=");
    line.push_u32(count);
    line.push_str(" ICM=15/15");
    console.write_output_line(framebuffer, line.as_str());
    console.write_output_line(
        framebuffer,
        "mode       phase burst  mean(us)  underruns frames",
    );
    for result in results {
        report_display_bench_compact(console, framebuffer, &result);
    }
}

/// Runs only the production configuration used by normal drawing. This is
/// the short acceptance command after `db` has established which diagnostic
/// burst/phase combinations are intentionally hostile.
fn cmd_displaybench_production(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
) {
    let Some(count) =
        parse_bench_count(argument, "usage: dp [count] (1-1000)", console, framebuffer)
    else {
        return;
    };
    icm::set_display_priority(15, 15);
    let result = run_display_bench(framebuffer, DisplayBenchMode::Production, count, 0, 128);
    console.clear(framebuffer);
    console.write_output_line(framebuffer, "dp: production phase=0ms burst=128 ICM=15/15");
    report_display_bench_compact(console, framebuffer, &result);
}

/// Runs the Stage 3 idle acceptance test without requiring the user to type a
/// six-digit frame count. The measured panel is 57.3 Hz; 3,440 frames per
/// minute rounds upward very slightly so the default covers at least 30 min.
fn cmd_display_idle_soak(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    const FRAMES_PER_MINUTE: u32 = 3_440;
    let minutes = if trim(argument).is_empty() {
        30
    } else {
        match parse_u32(trim(argument)) {
            Some(value) if value > 0 && value <= 120 => value,
            _ => {
                console.write_output_line(framebuffer, "usage: di [minutes] (1-120)");
                return;
            }
        }
    };
    let count = minutes * FRAMES_PER_MINUTE;

    let mut line = Line::new();
    line.push_str("di: idle soak running for ");
    line.push_u32(minutes);
    line.push_str(" minutes...");
    console.write_output_line(framebuffer, line.as_str());

    icm::set_display_priority(15, 15);
    let result = run_display_bench(framebuffer, DisplayBenchMode::Idle, count, 0, 128);
    console.clear(framebuffer);

    let mut line = Line::new();
    line.push_str("di: idle soak ");
    line.push_u32(minutes);
    line.push_str(" minutes ICM=15/15");
    console.write_output_line(framebuffer, line.as_str());
    report_display_bench_compact(console, framebuffer, &result);
}

/// Exercises the real console cell-array + DMA2D scroll path 100 times. This
/// is deliberately separate from the generic full-screen production fill:
/// scroll is a simultaneous PSRAM read/write block copy followed by a narrow
/// CPU repaint, so it has a different contention shape.
fn cmd_ui_scroll_bench(console: &mut Console, framebuffer: &mut Framebuffer) {
    const INITIAL_LINES: u32 = 43;
    const SCROLLS: u32 = 100;

    icm::set_display_priority(15, 15);
    console.clear(framebuffer);
    for _ in 0..INITIAL_LINES {
        console.diagnostic_write_output_line(framebuffer, "UI SCROLL ACCEPTANCE TEST");
    }

    lcd::take_underrun();
    let first_frame = interrupts::frame_sequence();
    let mut completed = 0u32;
    let mut underruns = 0u32;
    let mut elapsed_cycles = 0u64;
    for _ in 0..SCROLLS {
        if !wait_for_next_display_frame() {
            break;
        }
        lcd::take_underrun();
        let start = membench::cycles();
        console.diagnostic_write_output_line(framebuffer, "UI SCROLL ACCEPTANCE TEST");
        elapsed_cycles += membench::cycles().wrapping_sub(start) as u64;
        if !wait_for_next_display_frame() {
            break;
        }
        completed += 1;
        if lcd::take_underrun() {
            underruns += 1;
        }
    }
    let frames = interrupts::frame_sequence().wrapping_sub(first_frame);
    let mean_us = if completed == 0 {
        0
    } else {
        (elapsed_cycles * 1_000_000 / startup::cpu_hz() as u64 / completed as u64) as u32
    };

    console.clear(framebuffer);
    let mut line = Line::new();
    line.push_str("ui scroll: completed=");
    line.push_u32(completed);
    line.push_str(" underruns=");
    line.push_u32(underruns);
    line.push_str("/");
    line.push_u32(completed);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("mean=");
    line.push_u32(mean_us);
    line.push_str("us frames=");
    line.push_u32(frames);
    console.write_output_line(framebuffer, line.as_str());
    console.write_output_line(
        framebuffer,
        "ui visual: interact with each screen; any key advances",
    );
}

/// Final read-only acceptance soak. Scanout never stops; once per second the
/// command adds a production full-screen fill plus SD and USB MSC reads, while
/// every foreground iteration writes, flushes and verifies a rotating PSRAM
/// heap stripe. External media are read at LBA 0 and never modified.
fn cmd_mixed_soak(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
) {
    uart::log(b"MIX TEST: recovery v10\r\n");
    const FRAMES_PER_MINUTE: u32 = 3_440;
    const IO_INTERVAL_FRAMES: u32 = 57;
    const PROGRESS_INTERVAL_FRAMES: u32 = 34_400;
    const HEAP_BYTES: usize = 4 * 1024 * 1024;
    const HEAP_STRIPE_BYTES: usize = 4 * 1024;
    const STORAGE_BYTES: usize = 8 * 512;
    const USB_RESCAN_ATTEMPTS: u32 = 3;

    let minutes = if trim(argument).is_empty() {
        120
    } else {
        match parse_u32(trim(argument)) {
            Some(value) if value > 0 && value <= 240 => value,
            _ => {
                console.write_output_line(framebuffer, "usage: mix [minutes] (1-240)");
                return;
            }
        }
    };

    console.write_output_line(
        framebuffer,
        "mix: read-only soak setup (requires SD + USB Mass Storage)...",
    );
    let mut initial_usb_rescans = 0u32;
    let mut initial_usb_power_cycles = 0u32;
    if !ensure_usb_mass_storage_ready(
        usb_host,
        USB_RESCAN_ATTEMPTS,
        &mut initial_usb_rescans,
        &mut initial_usb_power_cycles,
    ) {
        console.write_output_line(
            framebuffer,
            "mix: no ready USB Mass Storage after automatic rescans",
        );
        return;
    }
    if let Some(mass_storage) = usb_host.mass_storage_mut() {
        write_usb_msc_mode(console, framebuffer, "mix", mass_storage);
    }
    let Some(card) = sdmmc::init() else {
        console.write_output_line(framebuffer, "mix: SD activation failed");
        return;
    };

    let mut heap = Vec::new();
    if heap.try_reserve_exact(HEAP_BYTES).is_err() {
        console.write_output_line(framebuffer, "mix: 4 MiB PSRAM heap allocation failed");
        return;
    }
    heap.resize(HEAP_BYTES, 0u8);

    let mut sd_reference = [0u8; STORAGE_BYTES];
    let mut sd_current = [0u8; STORAGE_BYTES];
    let mut usb_reference = [0u8; STORAGE_BYTES];
    let mut usb_current = [0u8; STORAGE_BYTES];
    if !sdmmc::read_blocks(&card, 0, &mut sd_reference) {
        console.write_output_line(framebuffer, "mix: initial SD read failed");
        return;
    }
    if !read_initial_usb_block(
        usb_host,
        &mut usb_reference,
        &mut initial_usb_rescans,
        &mut initial_usb_power_cycles,
        USB_RESCAN_ATTEMPTS,
    ) {
        console.write_output_line(
            framebuffer,
            "mix: initial USB read failed after automatic rescans",
        );
        return;
    }

    let mut line = Line::new();
    line.push_str("mix: running ");
    line.push_u32(minutes);
    line.push_str(" minutes; nothing is written to external media...");
    console.write_output_line(framebuffer, line.as_str());

    icm::set_display_priority(15, 15);
    let _ = lcd::take_underrun();
    let initial_underruns = lcd::underrun_count();
    let start_frame = interrupts::frame_sequence();
    let target_frames = minutes * FRAMES_PER_MINUTE;
    let mut last_io_frame = start_frame.wrapping_sub(IO_INTERVAL_FRAMES);
    let mut next_progress = PROGRESS_INTERVAL_FRAMES;
    let mut iterations = 0u32;
    let mut io_operations = 0u32;
    let mut usb_packet_retries = 0u32;
    let mut usb_command_retries = 0u32;
    let mut usb_rescans = initial_usb_rescans;
    let mut usb_power_cycles = initial_usb_power_cycles;
    let mut failure: Option<&'static str> = None;

    while interrupts::frame_sequence().wrapping_sub(start_frame) < target_frames {
        if !wait_for_next_display_frame() {
            failure = Some("display DMA stopped");
            break;
        }
        if !exercise_heap_stripe(&mut heap, iterations, HEAP_STRIPE_BYTES) {
            failure = Some("PSRAM heap mismatch");
            break;
        }
        iterations = iterations.wrapping_add(1);

        let frame = interrupts::frame_sequence();
        if frame.wrapping_sub(last_io_frame) >= IO_INTERVAL_FRAMES {
            let color = if io_operations & 1 == 0 {
                crate::framebuffer::BLACK
            } else {
                crate::framebuffer::RED
            };
            framebuffer.fill(color);
            if !framebuffer.flush() {
                failure = Some("framebuffer writeback failed");
                break;
            }
            if !sdmmc::read_blocks(&card, 0, &mut sd_current) || sd_current != sd_reference {
                failure = Some("SD read mismatch");
                break;
            }
            match read_usb_soak_block(
                usb_host,
                &usb_reference,
                &mut usb_current,
                &mut usb_packet_retries,
                &mut usb_command_retries,
                &mut usb_rescans,
                &mut usb_power_cycles,
                USB_RESCAN_ATTEMPTS,
            ) {
                UsbSoakRead::Match => {}
                UsbSoakRead::Mismatch => {
                    failure = Some("USB data mismatch");
                    break;
                }
                UsbSoakRead::TransportFailed => {
                    failure = Some("USB transport failed after rescan");
                    break;
                }
            }
            io_operations += 1;
            last_io_frame = frame;
        }

        let elapsed_frames = frame.wrapping_sub(start_frame);
        if elapsed_frames >= next_progress {
            uart::log_hex(b"MIX: elapsed frames=", elapsed_frames);
            next_progress = next_progress.wrapping_add(PROGRESS_INTERVAL_FRAMES);
        }
        let _ = lcd::take_underrun();
        if interrupts::dma_error() != 0 {
            failure = Some("display DMA error");
            break;
        }
    }

    delay::delay_ms(20);
    let _ = lcd::take_underrun();
    let elapsed_frames = interrupts::frame_sequence().wrapping_sub(start_frame);
    let underruns = lcd::underrun_count().wrapping_sub(initial_underruns);
    let dma_error = interrupts::dma_error();

    console.clear(framebuffer);
    let mut line = Line::new();
    line.push_str("mix: frames=");
    line.push_u32(elapsed_frames);
    line.push_str(" io=");
    line.push_u32(io_operations);
    line.push_str(" heap=");
    line.push_u32(iterations);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("usb retries: packet=");
    line.push_u32(usb_packet_retries);
    line.push_str(" command=");
    line.push_u32(usb_command_retries);
    line.push_str(" rescans=");
    line.push_u32(usb_rescans);
    line.push_str(" power_cycles=");
    line.push_u32(usb_power_cycles);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("underruns=");
    line.push_u32(underruns);
    line.push_str(" dma_error=0x");
    line.push_hex(dma_error, 8);
    console.write_output_line(framebuffer, line.as_str());

    match failure {
        Some(reason) => {
            let mut line = Line::new();
            line.push_str("mix: FAIL: ");
            line.push_str(reason);
            console.write_output_line(framebuffer, line.as_str());
        }
        None if underruns == 0 && dma_error == 0 && elapsed_frames >= target_frames => {
            console.write_output_line(framebuffer, "mix: PASS (nothing written to SD/USB)");
        }
        None => console.write_output_line(framebuffer, "mix: FAIL: incomplete or underrun"),
    }
}

/// Milliseconds USB-A stays unpowered between `usbmargin` rounds.
///
/// A cold boot is what this command is standing in for, so each round has to
/// start from a device that is genuinely unpowered rather than one that only
/// saw a bus reset. `hcd::power_cycle_vbus` uses the same second for the same
/// reason.
const MARGIN_VBUS_OFF_MS: u32 = 1_000;
/// Connect wait used while measuring. Deliberately far above both the
/// steady-state limit and the boot limit: a truncated wait would report a
/// device as absent instead of showing how long it actually took, which is
/// the one number this command exists to produce.
const MARGIN_CONNECT_WAIT_MS: u32 = 5_000;
/// TEST UNIT READY budget per round, for the same reason.
const MARGIN_READY_BUDGET_MS: u32 = 15_000;

/// Measures the time from USB-A's 5V switching on until a mass-storage device
/// behind it can actually be read, over several cold power cycles.
///
/// The firmware wants to prefer USB mass storage over the SD card when one is
/// plugged in at boot, which means boot has to wait for a device that is
/// powering up at that moment -- and the wait has to come from measurement,
/// not a guess: too short silently boots off the wrong medium, too long
/// delays every boot that has no USB device at all. Each round cuts VBUS,
/// discards every session, and times a full scan plus the SCSI sequence a
/// filesystem probe would run. Nothing is written to the device.
///
/// See `docs/plans/archive/USB_MSC_BOOT_MARGIN_PLAN.md`.
fn cmd_usb_margin(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
) {
    let rounds = if trim(argument).is_empty() {
        5
    } else {
        match parse_u32(trim(argument)) {
            Some(value) if value > 0 && value <= 20 => value,
            _ => {
                console.write_output_line(framebuffer, "usage: usbmargin [rounds] (1-20)");
                return;
            }
        }
    };

    if let Some(boot) = usb_host.boot_scan_timing().copied() {
        let mut line = Line::new();
        line.push_str("boot scan: connect=");
        line.push_u32(boot.connect_ms);
        line.push_str(" enum=");
        line.push_u32(boot.enumerated_ms);
        line.push_str(" msc=");
        line.push_u32(boot.mass_storage_ms);
        line.push_str(" total=");
        line.push_u32(boot.total_ms);
        line.push_str("ms");
        console.write_output_line(framebuffer, line.as_str());
    }

    let mut usable_rounds = 0u32;
    let mut minimum_total = u32::MAX;
    let mut maximum_total = 0u32;
    let mut maximum_connect = 0u32;
    let mut maximum_ready = 0u32;

    for round in 1..=rounds {
        // Every address and endpoint toggle on the bus dies with the rail, so
        // the registry has to be emptied before the power goes rather than
        // after it comes back.
        usb_host.clear();
        if !usb::set_vbus_power(false) {
            console.write_output_line(framebuffer, "usbmargin: VBUS off failed (PI4IOE2 @ 0x44)");
            return;
        }
        delay::delay_ms(MARGIN_VBUS_OFF_MS);

        // `rescan` switches the rail back on itself, inside the probe whose
        // entry the timings are measured from.
        let steady_state_connect_wait = usb::set_connect_wait_ms(MARGIN_CONNECT_WAIT_MS);
        usb_host.rescan(usb::RescanReason::PowerRecovery);
        usb::set_connect_wait_ms(steady_state_connect_wait);

        let Some(scan) = usb_host.last_scan_timing().copied() else {
            console.write_output_line(framebuffer, "usbmargin: scan produced no timing");
            return;
        };
        let ready = usb_host
            .mass_storage_mut()
            .map(|storage| storage.measure_ready_and_first_read(MARGIN_READY_BUDGET_MS));

        let mut line = Line::new();
        line.push_u32(round);
        line.push_str(": con=");
        line.push_u32(scan.connect_ms);
        line.push_str(" ena=");
        line.push_u32(scan.port_enabled_ms);
        line.push_str(" enum=");
        line.push_u32(scan.enumerated_ms);
        line.push_str(" msc=");
        line.push_u32(scan.mass_storage_ms);
        match ready {
            Some(ready) => {
                line.push_str(" rdy=");
                line.push_u32(ready.ready_ms);
                line.push_str("/");
                line.push_u32(ready.attempts);
                line.push_str(" lba0=");
                line.push_u32(ready.first_read_ms);
                if ready.usable() {
                    let total = scan.total_ms.saturating_add(ready.first_read_ms);
                    line.push_str(" total=");
                    line.push_u32(total);
                    usable_rounds += 1;
                    minimum_total = minimum_total.min(total);
                    maximum_total = maximum_total.max(total);
                    maximum_connect = maximum_connect.max(scan.connect_ms);
                    maximum_ready = maximum_ready.max(ready.first_read_ms);
                } else if ready.outcome == usb::ReadyOutcome::NoMedium {
                    line.push_str(" NO MEDIUM");
                } else {
                    line.push_str(" UNREADABLE");
                }
            }
            None if scan.connected => line.push_str(" no MSC attached"),
            None => line.push_str(" no device"),
        }
        console.write_output_line(framebuffer, line.as_str());
        uart::log(line.as_str().as_bytes());
        uart::log(b"\r\n");
    }

    let mut line = Line::new();
    line.push_str("usbmargin: usable ");
    line.push_u32(usable_rounds);
    line.push_str("/");
    line.push_u32(rounds);
    if usable_rounds == 0 {
        line.push_str(" -- no measurement");
        console.write_output_line(framebuffer, line.as_str());
        return;
    }
    line.push_str(" total min=");
    line.push_u32(minimum_total);
    line.push_str(" max=");
    line.push_u32(maximum_total);
    line.push_str("ms");
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("worst connect=");
    line.push_u32(maximum_connect);
    line.push_str("ms scsi=");
    line.push_u32(maximum_ready);
    line.push_str("ms; 1.5x boot budget=");
    // Half again on top of the worst round, rounded up to a tenth of a
    // second: a suggestion to compare against the other devices' numbers,
    // not a value to adopt from a single run.
    line.push_u32(
        maximum_total
            .saturating_add(maximum_total / 2)
            .div_ceil(100)
            * 100,
    );
    line.push_str("ms");
    console.write_output_line(framebuffer, line.as_str());
}

/// Short, read-only USB MSC stability test used before the full `mix` soak.
/// It keeps the same persistent BOT session and repeats the same 4 KiB
/// READ(10), so a timeout, recovery retry, or silent data mismatch is visible
/// without requiring the user to type a long command matrix.
/// What one read soak established.
#[derive(Clone, Copy, Default)]
struct ReadSoakOutcome {
    requested: u32,
    completed: u32,
    transport_failures: u32,
    mismatches: u32,
}

impl ReadSoakOutcome {
    fn passed(&self) -> bool {
        self.requested > 0
            && self.completed == self.requested
            && self.transport_failures == 0
            && self.mismatches == 0
    }
}

fn cmd_usb_read_test(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
) {
    let count = if trim(argument).is_empty() {
        100
    } else {
        match parse_u32(trim(argument)) {
            Some(value) if value > 0 && value <= 1_000 => value,
            _ => {
                console.write_output_line(framebuffer, "usage: ut [count] (1-1000)");
                return;
            }
        }
    };
    let _ = run_usb_read_soak(console, framebuffer, count, "ut", usb_host);
}

/// Reads and compares the same 4 KiB `count` times.
///
/// Split from the command so `usbcheck` can run it as one step of a longer
/// acceptance run. `label` prefixes the output lines so a soak run inside
/// another command is not mistaken for a bare `ut`.
fn run_usb_read_soak(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    count: u32,
    label: &str,
    usb_host: &mut usb::UsbHost,
) -> ReadSoakOutcome {
    const STORAGE_BYTES: usize = 8 * 512;
    uart::log(b"USB TEST: fault-rescan retry v42\r\n");
    let aborted = ReadSoakOutcome::default();

    let Some(mass_storage) = usb_host.mass_storage_mut() else {
        let mut line = Line::new();
        line.push_str(label);
        line.push_str(": no USB Mass Storage; attach one and run usbrescan");
        console.write_output_line(framebuffer, line.as_str());
        return aborted;
    };
    write_usb_msc_mode(console, framebuffer, label, mass_storage);
    if !mass_storage.wait_until_ready(10) {
        let mut line = Line::new();
        line.push_str(label);
        line.push_str(": USB Mass Storage is not ready");
        console.write_output_line(framebuffer, line.as_str());
        return aborted;
    }

    let mut reference = [0u8; STORAGE_BYTES];
    let mut current = [0u8; STORAGE_BYTES];
    let retries_before = mass_storage.read_retry_count();
    let packet_retries_before = mass_storage.packet_retry_count();
    if !mass_storage.read_blocks(0, &mut reference) {
        let mut line = Line::new();
        line.push_str(label);
        line.push_str(": initial USB read failed");
        console.write_output_line(framebuffer, line.as_str());
        return aborted;
    }

    let mut completed = 0u32;
    let mut transport_failures = 0u32;
    let mut mismatches = 0u32;
    for _ in 0..count {
        if !mass_storage.read_blocks(0, &mut current) {
            transport_failures += 1;
            break;
        }
        if current != reference {
            mismatches += 1;
            break;
        }
        completed += 1;
    }

    let retries = mass_storage.read_retry_count().wrapping_sub(retries_before);
    let packet_retries = mass_storage
        .packet_retry_count()
        .wrapping_sub(packet_retries_before);
    let mut line = Line::new();
    line.push_str(label);
    line.push_str(": completed=");
    line.push_u32(completed);
    line.push_str("/");
    line.push_u32(count);
    line.push_str(" failures=");
    line.push_u32(transport_failures);
    line.push_str(" mismatch=");
    line.push_u32(mismatches);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str(label);
    line.push_str(": packet_retries=");
    line.push_u32(packet_retries);
    line.push_str(" command_retries=");
    line.push_u32(retries);
    console.write_output_line(framebuffer, line.as_str());

    let outcome = ReadSoakOutcome {
        requested: count,
        completed,
        transport_failures,
        mismatches,
    };
    let mut line = Line::new();
    line.push_str(label);
    line.push_str(if outcome.passed() {
        if retries == 0 {
            ": PASS"
        } else {
            ": PASS (BOT recovery was used)"
        }
    } else {
        ": FAIL"
    });
    console.write_output_line(framebuffer, line.as_str());
    outcome
}

fn write_usb_msc_mode(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    command: &str,
    mass_storage: &usb::UsbMassStorage,
) {
    let mut line = Line::new();
    line.push_str(command);
    line.push_str(": host=");
    line.push_str(if usb::fs_ls_only_host_forced() {
        "FS-only"
    } else {
        "High-Speed"
    });
    line.push_str(" bulk-in-mps=");
    line.push_u32(mass_storage.bulk_in_mps() as u32);
    let fifo = usb::fifo_configuration();
    line.push_str(" fifo=");
    line.push_u32(fifo.rx_lines);
    line.push_str("/");
    line.push_u32(fifo.non_periodic_tx_lines);
    line.push_str("/");
    line.push_u32(fifo.periodic_tx_lines);
    console.write_output_line(framebuffer, line.as_str());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UsbSoakRead {
    Match,
    Mismatch,
    TransportFailed,
}

/// Makes setup deterministic when boot-time hub enumeration missed the MSC.
/// `UsbHost::rescan` is synchronous, so success means the registry contains a
/// newly enumerated and TEST UNIT READY device before `mix` starts its timer.
fn ensure_usb_mass_storage_ready(
    usb_host: &mut usb::UsbHost,
    max_rescan_attempts: u32,
    rescans: &mut u32,
    power_cycles: &mut u32,
) -> bool {
    let initially_ready = match usb_host.mass_storage_mut() {
        Some(mass_storage) => mass_storage.wait_until_ready(10),
        None => false,
    };
    if initially_ready {
        return true;
    }

    uart::log(b"mix: USB MSC missing/not ready; resetting and rescanning root port\r\n");
    for _ in 0..max_rescan_attempts {
        *rescans = rescans.wrapping_add(1);
        usb_host.rescan(usb::RescanReason::Recovery);
        let ready = match usb_host.mass_storage_mut() {
            Some(mass_storage) => mass_storage.wait_until_ready(10),
            None => false,
        };
        if ready {
            uart::log_hex(b"mix: USB MSC ready after rescan count=", *rescans);
            return true;
        }
        delay::delay_ms(200);
    }

    if *power_cycles != 0 {
        return false;
    }
    uart::log(b"mix: root rescans exhausted; power-cycling USB-A VBUS\r\n");
    *power_cycles = power_cycles.wrapping_add(1);
    if !usb_host.power_cycle_and_rescan() {
        return false;
    }
    *rescans = rescans.wrapping_add(1);
    if let Some(mass_storage) = usb_host.mass_storage_mut()
        && mass_storage.wait_until_ready(10)
    {
        uart::log(b"mix: USB MSC ready after VBUS power cycle\r\n");
        true
    } else {
        false
    }
}

/// Captures the immutable comparison block used by the soak. A transient
/// transport failure here is handled exactly like one during the timed run:
/// rebuild the USB bus and retry before declaring setup failed.
fn read_initial_usb_block(
    usb_host: &mut usb::UsbHost,
    buffer: &mut [u8],
    rescans: &mut u32,
    power_cycles: &mut u32,
    max_rescan_attempts: u32,
) -> bool {
    if let Some(mass_storage) = usb_host.mass_storage_mut()
        && mass_storage.read_blocks(0, buffer)
    {
        return true;
    }

    uart::log(b"mix: initial USB read failed; resetting and rescanning root port\r\n");
    for _ in 0..max_rescan_attempts {
        *rescans = rescans.wrapping_add(1);
        usb_host.rescan(usb::RescanReason::Recovery);
        let read_ok = match usb_host.mass_storage_mut() {
            Some(mass_storage) => {
                mass_storage.wait_until_ready(10) && mass_storage.read_blocks(0, buffer)
            }
            None => false,
        };
        if read_ok {
            uart::log(b"mix: initial USB read recovered after rescan\r\n");
            return true;
        }
        delay::delay_ms(200);
    }

    if *power_cycles != 0 {
        return false;
    }
    uart::log(b"mix: initial USB read still failed; power-cycling USB-A VBUS\r\n");
    *power_cycles = power_cycles.wrapping_add(1);
    if !usb_host.power_cycle_and_rescan() {
        return false;
    }
    *rescans = rescans.wrapping_add(1);
    match usb_host.mass_storage_mut() {
        Some(mass_storage) => {
            mass_storage.wait_until_ready(10) && mass_storage.read_blocks(0, buffer)
        }
        None => false,
    }
}

/// Reads the fixed read-only acceptance block. A failed BOT Reset Recovery
/// means the device address, endpoint toggles, and EP0 session are no longer
/// trustworthy, so the only safe next step is a root-port reset and complete
/// registry rebuild. Continue only after the newly enumerated device returns
/// the exact bytes captured before the soak began.
fn read_usb_soak_block(
    usb_host: &mut usb::UsbHost,
    reference: &[u8],
    current: &mut [u8],
    packet_retries: &mut u32,
    command_retries: &mut u32,
    rescans: &mut u32,
    power_cycles: &mut u32,
    max_rescan_attempts: u32,
) -> UsbSoakRead {
    if read_usb_counted(usb_host, current, packet_retries, command_retries) {
        return if current == reference {
            UsbSoakRead::Match
        } else {
            UsbSoakRead::Mismatch
        };
    }

    uart::log(b"mix: USB BOT recovery exhausted; resetting and rescanning root port\r\n");
    for _ in 0..max_rescan_attempts {
        *rescans = rescans.wrapping_add(1);
        usb_host.rescan(usb::RescanReason::Recovery);
        let Some(mass_storage) = usb_host.mass_storage_mut() else {
            delay::delay_ms(200);
            continue;
        };
        let packet_before = mass_storage.packet_retry_count();
        let command_before = mass_storage.read_retry_count();
        let ready = mass_storage.wait_until_ready(10);
        let read_ok = ready && mass_storage.read_blocks(0, current);
        *packet_retries = packet_retries.wrapping_add(
            mass_storage
                .packet_retry_count()
                .wrapping_sub(packet_before),
        );
        *command_retries = command_retries
            .wrapping_add(mass_storage.read_retry_count().wrapping_sub(command_before));
        if !read_ok {
            delay::delay_ms(200);
            continue;
        }
        return if current == reference {
            uart::log(b"mix: USB rescan recovered matching read-only data\r\n");
            UsbSoakRead::Match
        } else {
            UsbSoakRead::Mismatch
        };
    }

    if *power_cycles != 0 {
        return UsbSoakRead::TransportFailed;
    }
    uart::log(b"mix: root rescans exhausted; power-cycling USB-A VBUS\r\n");
    *power_cycles = power_cycles.wrapping_add(1);
    if !usb_host.power_cycle_and_rescan() {
        return UsbSoakRead::TransportFailed;
    }
    *rescans = rescans.wrapping_add(1);
    let read_ok = match usb_host.mass_storage_mut() {
        Some(mass_storage) => {
            let packet_before = mass_storage.packet_retry_count();
            let command_before = mass_storage.read_retry_count();
            let ready = mass_storage.wait_until_ready(10);
            let result = ready && mass_storage.read_blocks(0, current);
            *packet_retries = packet_retries.wrapping_add(
                mass_storage
                    .packet_retry_count()
                    .wrapping_sub(packet_before),
            );
            *command_retries = command_retries
                .wrapping_add(mass_storage.read_retry_count().wrapping_sub(command_before));
            result
        }
        None => false,
    };
    if !read_ok {
        return UsbSoakRead::TransportFailed;
    }
    if current == reference {
        uart::log(b"mix: USB VBUS power cycle recovered matching read-only data\r\n");
        UsbSoakRead::Match
    } else {
        UsbSoakRead::Mismatch
    }
}

fn read_usb_counted(
    usb_host: &mut usb::UsbHost,
    buffer: &mut [u8],
    packet_retries: &mut u32,
    command_retries: &mut u32,
) -> bool {
    let Some(mass_storage) = usb_host.mass_storage_mut() else {
        return false;
    };
    let packet_before = mass_storage.packet_retry_count();
    let command_before = mass_storage.read_retry_count();
    let result = mass_storage.read_blocks(0, buffer);
    *packet_retries = packet_retries.wrapping_add(
        mass_storage
            .packet_retry_count()
            .wrapping_sub(packet_before),
    );
    *command_retries =
        command_retries.wrapping_add(mass_storage.read_retry_count().wrapping_sub(command_before));
    result
}

fn exercise_heap_stripe(heap: &mut [u8], iteration: u32, stripe_bytes: usize) -> bool {
    if heap.len() < stripe_bytes || stripe_bytes == 0 {
        return false;
    }
    let stripes = heap.len() / stripe_bytes;
    let offset = iteration as usize % stripes * stripe_bytes;
    let pointer = unsafe { heap.as_mut_ptr().add(offset) };
    for index in 0..stripe_bytes {
        let value = (index as u8).wrapping_mul(37).wrapping_add(iteration as u8);
        unsafe { pointer.add(index).write_volatile(value) };
    }
    // The stripe starts wherever the heap and the iteration put it, so this
    // is refused whenever that is not a cache line boundary, and the readback
    // below is then served from cache instead of from PSRAM. That weakens the
    // stripe as a PSRAM test rather than making it report a false result, so
    // it is left alone here; aligning the stripe is a change to what this
    // soak measures and belongs with that command, not with an SD card fix.
    let _ = psram::writeback_invalidate(pointer as usize, stripe_bytes);
    for index in 0..stripe_bytes {
        let expected = (index as u8).wrapping_mul(37).wrapping_add(iteration as u8);
        if unsafe { pointer.add(index).read_volatile() } != expected {
            return false;
        }
    }
    true
}

fn parse_bench_count(
    argument: &[u8],
    usage: &str,
    console: &mut Console,
    framebuffer: &mut Framebuffer,
) -> Option<u32> {
    if trim(argument).is_empty() {
        return Some(100);
    }
    match parse_u32(trim(argument)) {
        Some(value) if value > 0 && value <= 1000 => Some(value),
        _ => {
            console.write_output_line(framebuffer, usage);
            None
        }
    }
}

fn report_display_bench_compact(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    result: &DisplayBenchResult,
) {
    const MODE_COLUMNS: usize = 11;
    let mut line = Line::new();
    line.push_str(result.mode.name());
    pad_to(&mut line, MODE_COLUMNS);
    line.push_u32(result.phase_ms);
    line.push_str("ms  b");
    line.push_u32(result.burst);
    line.push_str("  ");
    line.push_u32(result.mean_us);
    line.push_str("us  ");
    line.push_u32(result.underruns);
    line.push_str("/");
    line.push_u32(result.completed);
    line.push_str("  f");
    line.push_u32(result.frames);
    if result.completed != result.count {
        line.push_str(" FAILED");
    }
    console.write_output_line(framebuffer, line.as_str());
}

/// Waits for the display DMA's next full-frame completion while a shell
/// command temporarily owns the foreground loop.
fn wait_for_next_display_frame() -> bool {
    let initial = interrupts::frame_sequence();
    loop {
        if interrupts::dma_error() != 0 {
            return false;
        }
        if interrupts::frame_sequence() != initial {
            return true;
        }
        interrupts::wait_for_interrupt();
    }
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

/// Reports uptime from `tick`'s millisecond counter, falling back to the
/// frame counter if the tick never started.
///
/// The frame count is only ever an estimate: it assumes every frame
/// completed and that the panel runs at exactly its nominal rate. The
/// SYSTIMER tick assumes neither, so it is also the number to compare
/// against a stopwatch when checking that no ticks are being lost.
fn cmd_uptime(console: &mut Console, framebuffer: &mut Framebuffer) {
    let mut line = Line::new();
    line.push_str("uptime: ");
    if !tick::is_running() {
        line.push_str("~");
        line.push_u32(interrupts::frame_sequence() / FRAMES_PER_SECOND);
        line.push_str(" s (frame-counted; the tick is not running)");
        console.write_output_line(framebuffer, line.as_str());
        return;
    }

    let milliseconds = tick::now_ms();
    line.push_u32((milliseconds / 1000) as u32);
    line.push_str(".");
    line.push_u32(((milliseconds % 1000) / 100) as u32);
    line.push_str(" s (systimer tick)");
    console.write_output_line(framebuffer, line.as_str());

    // Two independent counts of the same elapsed time. They drift apart
    // only if ticks are being lost or frames are being missed, so printing
    // both is what turns "the clock looks about right" into a measurement.
    let mut line = Line::new();
    line.push_str("frames say ~");
    line.push_u32(interrupts::frame_sequence() / FRAMES_PER_SECOND);
    line.push_str(" s; tick irq pending ");
    line.push_str(if tick::interrupt_pending() {
        "yes"
    } else {
        "no"
    });
    console.write_output_line(framebuffer, line.as_str());
}

/// Reads, writes and tests the RX8130CE real-time clock.
///
/// The clock is the one board device whose whole purpose is to keep counting
/// while the firmware is not running, so "does it answer on I2C" says very
/// The default number of seedings `entropy test` takes.
///
/// The plan's figure. Enough that a stuck source or an unpaired guard shows
/// up, few enough that the whole run is under a second.
const ENTROPY_TEST_ROUNDS: u32 = 100;

fn cmd_entropy(console: &mut Console, framebuffer: &mut Framebuffer, argument: &[u8]) {
    let (subcommand, rest) = split_first_word(argument);
    match subcommand {
        b"" => cmd_entropy_show(console, framebuffer),
        b"test" => {
            let rounds = match trim(rest) {
                b"" => ENTROPY_TEST_ROUNDS,
                text => match parse_u32(text) {
                    Some(rounds) if rounds > 0 => rounds,
                    _ => {
                        console.write_output_line(framebuffer, "usage: entropy test [count]");
                        return;
                    }
                },
            };
            cmd_entropy_test(console, framebuffer, rounds);
        }
        b"fail" => match trim(rest) {
            b"on" => {
                entropy::force_failure(true);
                console.write_output_line(framebuffer, "entropy: seeding will now fail");
            }
            b"off" => {
                entropy::force_failure(false);
                console.write_output_line(framebuffer, "entropy: seeding re-enabled");
            }
            _ => console.write_output_line(framebuffer, "usage: entropy fail on|off"),
        },
        _ => console.write_output_line(framebuffer, "usage: entropy [test [count] | fail on|off]"),
    }
}

fn cmd_entropy_show(console: &mut Console, framebuffer: &mut Framebuffer) {
    if entropy::failure_is_forced() {
        console.write_output_line(
            framebuffer,
            "entropy: failure is forced ('entropy fail off')",
        );
    }
    let mut bytes = [0u8; entropy::SEED_BYTES];
    let taken = {
        match entropy::Source::enable() {
            Ok(_source) => entropy::read_hardware_bytes(&mut bytes),
            Err(error) => Err(error),
        }
    };
    match taken {
        Ok(()) => {
            for chunk in bytes.chunks(16) {
                let mut line = Line::new();
                for &byte in chunk {
                    line.push_hex(byte as u32, 2);
                    line.push_str(" ");
                }
                console.write_output_line(framebuffer, line.as_str());
            }
        }
        Err(error) => {
            let mut line = Line::new();
            line.push_str(error.name());
            line.push_str(": ");
            line.push_str(error.message());
            console.write_output_line(framebuffer, line.as_str());
        }
    }
    push_entropy_counts(console, framebuffer);
}

/// Repeats the whole path a TLS connection takes -- bring the source up,
/// seed a CSPRNG, draw from it -- and reports the two things a single
/// reading cannot show: that the guard always powered the ADC back down,
/// and that no two connections would start from the same bytes.
///
/// Comparing whole draws rather than testing their statistics on purpose --
/// a statistical test cannot prove randomness, but a repeat proves its
/// absence.
fn cmd_entropy_test(console: &mut Console, framebuffer: &mut Framebuffer, rounds: u32) {
    use rand_core_06::RngCore;

    let (enables_before, disables_before) = entropy::transition_counts();
    let mut previous = [0u8; entropy::SEED_BYTES];
    let mut repeats = 0u32;
    let mut failures = 0u32;
    let mut first_error = None;

    for round in 0..rounds {
        let mut bytes = [0u8; entropy::SEED_BYTES];
        match entropy::Csprng::from_hardware() {
            Ok(mut csprng) => {
                // What a ClientHello would draw: the random and the key
                // share come out of this stream, so two rounds matching
                // here is two handshakes that would have matched.
                csprng.fill_bytes(&mut bytes);
                if round > 0 && bytes == previous {
                    repeats += 1;
                }
                previous = bytes;
            }
            Err(error) => {
                failures += 1;
                first_error.get_or_insert(error);
            }
        }
    }

    let (enables_after, disables_after) = entropy::transition_counts();
    let enables = enables_after.wrapping_sub(enables_before);
    let disables = disables_after.wrapping_sub(disables_before);

    let mut line = Line::new();
    line.push_str("rounds ");
    line.push_u32(rounds);
    line.push_str(", enables ");
    line.push_u32(enables);
    line.push_str(", disables ");
    line.push_u32(disables);
    console.write_output_line(framebuffer, line.as_str());

    let mut passed = true;
    if enables != disables {
        console.write_output_line(framebuffer, "FAIL an enable was not paired with a disable");
        passed = false;
    }
    if repeats > 0 {
        let mut line = Line::new();
        line.push_str("FAIL ");
        line.push_u32(repeats);
        line.push_str(" draws repeated the one before");
        console.write_output_line(framebuffer, line.as_str());
        passed = false;
    }
    if let Some(error) = first_error {
        let mut line = Line::new();
        line.push_str("FAIL ");
        line.push_u32(failures);
        line.push_str(" seedings failed: ");
        line.push_str(error.message());
        console.write_output_line(framebuffer, line.as_str());
        passed = false;
    }
    if passed {
        console.write_output_line(framebuffer, "entropy test: all checks passed");
    }
}

fn push_entropy_counts(console: &mut Console, framebuffer: &mut Framebuffer) {
    let (enables, disables) = entropy::transition_counts();
    let mut line = Line::new();
    line.push_str("source enabled ");
    line.push_u32(enables);
    line.push_str(" times, disabled ");
    line.push_u32(disables);
    console.write_output_line(framebuffer, line.as_str());
}

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
            "usage: rtc [set <YYYY-MM-DD> <HH:MM:SS> (UTC) | regs | test]",
        ),
    }
}

fn cmd_rtc_show(console: &mut Console, framebuffer: &mut Framebuffer) {
    match rtc::read_datetime() {
        Ok(datetime) => {
            let mut line = Line::new();
            line.push_str("UTC  ");
            push_datetime(&mut line, &datetime);
            console.write_output_line(framebuffer, line.as_str());
            push_local_line(console, framebuffer, datetime.calendar());
        }
        Err(error) => console.write_output_line(framebuffer, error.message()),
    }
    push_certificate_clock_line(console, framebuffer);
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
        console.write_output_line(
            framebuffer,
            "usage: rtc set <YYYY-MM-DD> <HH:MM:SS>; the time is UTC, not local",
        );
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
            line.push_str("set; reads back as UTC  ");
            push_datetime(&mut line, &readback);
            console.write_output_line(framebuffer, line.as_str());
            push_local_line(console, framebuffer, readback.calendar());
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
            let expected = datetime.calendar().weekday();
            if let Some(expected) = expected
                && datetime.weekday.is_some_and(|weekday| weekday != expected)
            {
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
/// Reports whether the stricter reading -- the one certificate validity
/// checking would use -- can be believed right now.
///
/// This is the only place the two clocks visibly disagree: a Tab5 whose
/// `VLF` is set still prints a UTC and a JST line above, because those are
/// what the counters hold, and still fails here, because what the counters
/// hold is not a time anyone set. Unauthenticated TLS and SPKI pinning do
/// not consult this line at all (`docs/plans/archive/TLS_PLAN.md`).
fn push_certificate_clock_line(console: &mut Console, framebuffer: &mut Framebuffer) {
    let mut line = Line::new();
    line.push_str("cert clock  ");
    match wall_clock::unix_time_utc() {
        Ok(seconds) => {
            line.push_str("unix ");
            line.push_u64(seconds as u64);
        }
        Err(error) => {
            line.push_str(error.name());
            line.push_str(": ");
            line.push_str(error.message());
        }
    }
    console.write_output_line(framebuffer, line.as_str());
}

/// Writes the local reading of a UTC one, on its own line and labelled with
/// the zone it was converted into.
///
/// Two labelled lines rather than one unlabelled time: the device holds UTC
/// and a person reads JST, and the only way a reader can tell which they are
/// looking at is for both to be named.
fn push_local_line(console: &mut Console, framebuffer: &mut Framebuffer, utc: tab5_time::Calendar) {
    let zone = wall_clock::timezone();
    let Some(local) = tab5_time::local_datetime(utc, zone) else {
        return;
    };
    let mut line = Line::new();
    line.push_str(zone.name);
    line.push_str("  ");
    line.push_u32(local.year as u32);
    line.push_str("-");
    push_two_digits(&mut line, local.month);
    line.push_str("-");
    push_two_digits(&mut line, local.day);
    line.push_str(" ");
    push_two_digits(&mut line, local.hour);
    line.push_str(":");
    push_two_digits(&mut line, local.minute);
    line.push_str(":");
    push_two_digits(&mut line, local.second);
    line.push_str(" ");
    // `offset_text` is five ASCII digits and a sign by construction.
    line.push_str(core::str::from_utf8(&zone.offset_text()).unwrap_or("?????"));
    console.write_output_line(framebuffer, line.as_str());
}

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

fn wifi_command_allowed(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    manager: &WifiManager,
) -> bool {
    if manager.is_enabled() {
        true
    } else {
        console.write_output_line(framebuffer, "Wi-Fi is off; run 'wifi on' first");
        false
    }
}

fn cmd_wifi_control(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    manager: &mut WifiManager,
) {
    match trim(argument) {
        b"on" => {
            if manager.is_enabled() {
                console.write_output_line(framebuffer, "Wi-Fi is already on");
                return;
            }
            match manager.set_enabled(true) {
                Ok(true) => console.write_output_line(
                    framebuffer,
                    "Wi-Fi enabled; connecting to the saved profile",
                ),
                Ok(false) => console.write_output_line(
                    framebuffer,
                    "Wi-Fi enabled; no saved profile to auto-connect",
                ),
                Err(failure) => {
                    write_wifi_control_failure(console, framebuffer, "enable failed", failure)
                }
            }
        }
        b"off" => {
            if !manager.is_enabled() {
                console.write_output_line(framebuffer, "Wi-Fi is already off");
                return;
            }
            match manager.set_enabled(false) {
                Ok(_) => console.write_output_line(
                    framebuffer,
                    "Wi-Fi disabled and C6 powered down; setting saved for next boot",
                ),
                Err(failure) => {
                    console.write_output_line(
                        framebuffer,
                        "Wi-Fi is off for this boot, but saving OFF failed",
                    );
                    write_wifi_control_failure(console, framebuffer, "OFF persistence", failure);
                }
            }
        }
        b"status" => {
            manager.service();
            let mut line = Line::new();
            line.push_str("Wi-Fi: ");
            line.push_str(if manager.is_enabled() { "ON" } else { "OFF" });
            line.push_str("  state: ");
            line.push_str(wifi_phase_name(manager.state().phase()));
            console.write_output_line(framebuffer, line.as_str());

            console.write_output_line(
                framebuffer,
                if manager.has_saved_profile() {
                    "saved profile: yes"
                } else {
                    "saved profile: no"
                },
            );
            if let Some(config) = manager.stack().and_then(|stack| stack.config()) {
                let mut line = Line::new();
                line.push_str("IPv4: ");
                push_ipv4(&mut line, config.address.address());
                console.write_output_line(framebuffer, line.as_str());
            }
        }
        _ => console.write_output_line(framebuffer, "usage: wifi <subcommand> [arguments]"),
    }
}

fn write_wifi_control_failure(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    operation: &str,
    failure: wifi_manager::Failure,
) {
    match failure {
        wifi_manager::Failure::StartStatus(status)
        | wifi_manager::Failure::ConfigStatus(status)
        | wifi_manager::Failure::StorageStatus(status)
        | wifi_manager::Failure::ModeStatus(status)
        | wifi_manager::Failure::StopStatus(status)
        | wifi_manager::Failure::DisconnectStatus(status) => {
            write_slave_status(console, framebuffer, operation, status)
        }
        _ => {
            let mut line = Line::new();
            line.push_str(operation);
            line.push_str(": RPC or link failure; see UART log");
            console.write_output_line(framebuffer, line.as_str());
        }
    }
}

fn restore_after_wifi_diagnostic(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    manager: &mut WifiManager,
    was_enabled: bool,
) {
    if !was_enabled {
        manager.finish_disabled_diagnostic();
        console.write_output_line(framebuffer, "diagnostic complete; Wi-Fi remains off");
        return;
    }
    if let Err(failure) = manager.begin_startup_auto_connect() {
        write_wifi_control_failure(
            console,
            framebuffer,
            "restore after diagnostic failed",
            failure,
        );
    }
}

fn cmd_wifiinfo(console: &mut Console, framebuffer: &mut Framebuffer) {
    console.write_output_line(framebuffer, "activating ESP32-C6 (SDIO card 1)...");
    let Some(card) = sdio::init() else {
        console.write_output_line(framebuffer, "C6 activation failed, see UART log");
        return;
    };
    console.write_output_line(framebuffer, "C6 activated as an SDIO card");

    let mut line = Line::new();
    line.push_str("RCA: 0x");
    line.push_hex(card.rca as u32, 4);
    line.push_str("  I/O functions: ");
    line.push_u32(card.io_functions as u32);
    line.push_str(if card.memory_present {
        "  memory: yes"
    } else {
        "  memory: no"
    });
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("CIS manufacturer: 0x");
    line.push_hex(card.manufacturer as u32, 4);
    line.push_str("  product: 0x");
    line.push_hex(card.product as u32, 4);
    line.push_str(if card.is_esp_slave() {
        "  (ESP)"
    } else {
        "  (unrecognized, raw CIS in UART log)"
    });
    console.write_output_line(framebuffer, line.as_str());

    console.write_output_line(
        framebuffer,
        if card.bus_width_4bit {
            "bus width: 4-bit"
        } else {
            "bus width: 1-bit (CCCR write failed or skipped)"
        },
    );

    let mut line = Line::new();
    line.push_str("clock: ");
    line.push_u32(card.clock_khz);
    line.push_str(" kHz  High Speed: ");
    line.push_str(if card.high_speed_supported {
        "supported (not enabled)"
    } else {
        "not supported"
    });
    console.write_output_line(framebuffer, line.as_str());
}

fn cmd_wifiup(console: &mut Console, framebuffer: &mut Framebuffer) {
    console.write_output_line(framebuffer, "bringing up the ESP-Hosted link...");
    let Some((transport, info)) = wifi::bring_up() else {
        console.write_output_line(framebuffer, "link bring-up failed, see UART log");
        return;
    };

    let mut line = Line::new();
    line.push_str("chip id: 0x");
    line.push_hex(info.chip_id as u32, 2);
    line.push_str(if info.chip_id == wifi::hosted::CHIP_ID_ESP32C6 {
        " (ESP32-C6)"
    } else {
        " (unexpected)"
    });
    line.push_str("  firmware: ");
    line.push_u32(info.firmware_major());
    line.push_str(".");
    line.push_u32(info.firmware_minor());
    line.push_str(".");
    line.push_u32(info.firmware_patch());
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("capabilities: 0x");
    line.push_hex(info.capabilities as u32, 2);
    line.push_str("  extended: 0x");
    line.push_hex(info.extended_capabilities, 8);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("slave queues: rx ");
    line.push_u32(info.rx_queue_size as u32);
    line.push_str(", tx ");
    line.push_u32(info.tx_queue_size as u32);
    line.push_str("  mode: ");
    line.push_str(if info.streaming_mode {
        "streaming"
    } else {
        "packet"
    });
    line.push_str(if transport.is_throttled() {
        "  throttled"
    } else {
        ""
    });
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("bus: ");
    line.push_u32(transport.card.clock_khz);
    line.push_str(" kHz, ");
    line.push_str(if transport.card.bus_width_4bit {
        "4-bit"
    } else {
        "1-bit"
    });
    console.write_output_line(framebuffer, line.as_str());
    console.write_output_line(framebuffer, "RPC (scan/connect) is the next stage");
}

fn cmd_wifimac(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    session: &mut Option<wifi::Rpc>,
) {
    let Some(rpc) = wifi_session(console, framebuffer, session) else {
        return;
    };
    console.write_output_line(framebuffer, "calling GetMacAddress...");
    let Some((status, mac)) = wifi::rpc::get_mac_address(rpc, wifi::rpc::WIFI_IF_STA) else {
        console.write_output_line(framebuffer, "RPC call failed, see UART log");
        return;
    };

    console.write_output_line(framebuffer, "RPC round trip completed");

    let mut line = Line::new();
    line.push_str("slave status: ");
    line.push_u32(status as u32);
    if status != 0 {
        line.push_str(" (Wi-Fi is not initialized yet)");
    }
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("station MAC: ");
    for (index, byte) in mac.iter().enumerate() {
        if index != 0 {
            line.push_str(":");
        }
        line.push_hex(*byte as u32, 2);
    }
    console.write_output_line(framebuffer, line.as_str());

    // The slave pushes its own events over the same channel; showing them
    // here is what proves the event path decodes as well as the response one.
    for event in rpc.take_events() {
        let mut line = Line::new();
        line.push_str("event ");
        line.push_u32(event.msg_id);
        line.push_str(" (");
        line.push_u32(event.payload.len() as u32);
        line.push_str(" bytes)");
        console.write_output_line(framebuffer, line.as_str());
    }

    let dropped = rpc.dropped_data_frames();
    if dropped != 0 {
        let mut line = Line::new();
        line.push_str("station frames dropped: ");
        line.push_u32(dropped);
        console.write_output_line(framebuffer, line.as_str());
    }
}

/// Forgets a session whose link died, so the next command starts over
/// instead of talking to a bus that no longer answers.
pub(super) fn drop_dead_session(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    manager: &mut WifiManager,
) {
    manager.service();
    for notice in manager.take_notices() {
        match notice {
            wifi_manager::Notice::Disconnected(reason) => {
                let mut line = Line::new();
                line.push_str("the station was disconnected, reason ");
                line.push_u32(reason);
                if let Some(name) = wifi::station::disconnect_reason_name(reason) {
                    line.push_str(" ");
                    line.push_str(name);
                }
                console.write_output_line(framebuffer, line.as_str());
                console.write_output_line(
                    framebuffer,
                    "nothing can be sent until it associates again",
                );
            }
            wifi_manager::Notice::Reassociated => {
                console.write_output_line(framebuffer, "the station (re)associated")
            }
            wifi_manager::Notice::LinkLost => console.write_output_line(
                framebuffer,
                "the C6 link was lost; it will be rebuilt next time",
            ),
        }
    }
}

/// Returns the open C6 session, establishing it first if there is none.
///
/// Bringing the link up resets the co-processor, so it is done once and then
/// reused: a connection made by `wifi connect` has to survive until
/// `wifi status` asks about it.
fn wifi_session<'a>(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    session: &'a mut Option<wifi::Rpc>,
) -> Option<&'a mut wifi::Rpc> {
    if session.is_none() {
        console.write_output_line(framebuffer, "bringing up the ESP-Hosted link...");
        let Some((transport, _)) = wifi::bring_up() else {
            console.write_output_line(framebuffer, "link bring-up failed, see UART log");
            return None;
        };
        let mut rpc = wifi::Rpc::new(transport);

        console.write_output_line(framebuffer, "starting Wi-Fi in station mode...");
        match wifi::station::start(&mut rpc) {
            Some(0) => {}
            Some(status) => {
                write_slave_status(console, framebuffer, "Wi-Fi start failed", status);
                return None;
            }
            None => {
                console.write_output_line(framebuffer, "Wi-Fi start: RPC failed, see UART log");
                return None;
            }
        }
        *session = Some(rpc);
    }
    session.as_mut()
}

fn cmd_wifiscan(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    session: &mut Option<wifi::Rpc>,
) {
    let Some(rpc) = wifi_session(console, framebuffer, session) else {
        return;
    };

    console.write_output_line(framebuffer, "scanning (this takes a few seconds)...");
    let Some((status, access_points)) = wifi::station::scan(rpc) else {
        console.write_output_line(framebuffer, "scan: RPC failed, see UART log");
        return;
    };
    if status != 0 {
        write_slave_status(console, framebuffer, "scan failed", status);
        return;
    }

    let mut line = Line::new();
    line.push_u32(access_points.len() as u32);
    line.push_str(" access points");
    console.write_output_line(framebuffer, line.as_str());

    for access_point in &access_points {
        let mut line = Line::new();
        // Right-align the RSSI so the list reads as columns.
        if access_point.rssi > -100 {
            line.push_str(" ");
        }
        if access_point.rssi < 0 {
            line.push_str("-");
        }
        line.push_u32(access_point.rssi.unsigned_abs());
        line.push_str("dBm ch");
        line.push_u32(access_point.channel);
        if access_point.channel < 10 {
            line.push_str(" ");
        }
        line.push_str(" ");
        match wifi::station::auth_mode_name(access_point.auth_mode) {
            Some(name) => line.push_str(name),
            None => {
                line.push_str("auth");
                line.push_u32(access_point.auth_mode as u32);
            }
        }
        line.push_str(" ");
        if access_point.ssid_length == 0 {
            line.push_str("(hidden)");
        } else {
            // Sanitize first: an SSID is arbitrary bytes, and the console
            // font only has ASCII.
            let ssid = access_point.ssid();
            let mut text = [0u8; wifi::station::SSID_MAX_BYTES];
            for (slot, &byte) in text.iter_mut().zip(ssid) {
                *slot = if byte.is_ascii_graphic() || byte == b' ' {
                    byte
                } else {
                    b'.'
                };
            }
            line.push_str(core::str::from_utf8(&text[..ssid.len()]).unwrap_or("?"));
        }
        console.write_output_line(framebuffer, line.as_str());
    }
}

fn cmd_wificonnect(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    manager: &mut WifiManager,
) -> ShellConnect {
    // The password keeps everything after the first gap, spaces included.
    let (ssid, password) = split_first_word(trim(argument));
    let password = trim(password);
    if ssid.is_empty() {
        console.write_output_line(framebuffer, "usage: wifi connect <ssid> [password]");
        return ShellConnect::NotStarted;
    }
    if ssid.len() > wifi::station::SSID_MAX_BYTES
        || password.len() > wifi::station::PASSWORD_MAX_BYTES
    {
        console.write_output_line(framebuffer, "SSID or password is too long");
        return ShellConnect::NotStarted;
    }

    if let Err(failure) = manager.prepare_connection_replacement() {
        match failure {
            wifi_manager::Failure::DisconnectStatus(status) => write_slave_status(
                console,
                framebuffer,
                "old connection disconnect refused",
                status,
            ),
            wifi_manager::Failure::DisconnectTimedOut => console.write_output_line(
                framebuffer,
                "old connection disconnect timed out; new connection not started",
            ),
            _ => console.write_output_line(
                framebuffer,
                "old connection disconnect failed; see UART log",
            ),
        }
        // The manager has already recorded the preparation failure. Do not
        // add a connect attempt to the history because no connect was sent.
        return ShellConnect::NotStarted;
    }

    let (session, _) = manager.options_mut();
    let Some(rpc) = wifi_session(console, framebuffer, session) else {
        return ShellConnect::NotStarted;
    };

    // The CLI contract is one transient association. Selecting RAM before
    // set_config prevents this command from replacing a saved menu profile.
    match wifi::station::set_storage(rpc, wifi::station::Storage::Ram) {
        Some(0) => {}
        Some(status) => {
            write_slave_status(console, framebuffer, "RAM storage refused", status);
            return ShellConnect::Failed(wifi_manager::Failure::StorageStatus(status));
        }
        None => {
            console.write_output_line(framebuffer, "set RAM storage: RPC failed, see UART log");
            return ShellConnect::Failed(wifi_manager::Failure::StorageRpc);
        }
    }

    console.write_output_line(framebuffer, "connecting...");
    match wifi::station::connect(rpc, ssid, password) {
        Some(0) => {}
        Some(status) => {
            write_slave_status(console, framebuffer, "connect refused", status);
            return ShellConnect::Failed(wifi_manager::Failure::ConnectStatus(status));
        }
        None => {
            console.write_output_line(framebuffer, "connect: RPC failed, see UART log");
            return ShellConnect::Failed(wifi_manager::Failure::ConnectRpc);
        }
    }

    match wifi::station::wait_for_connection(rpc, CONNECT_TIMEOUT_MS) {
        wifi::station::Outcome::Connected {
            ssid,
            ssid_length,
            bssid,
            channel,
            auth_mode,
        } => {
            let mut line = Line::new();
            line.push_str("connected to ");
            push_ssid(&mut line, &ssid[..ssid_length]);
            console.write_output_line(framebuffer, line.as_str());

            // Which AP of the network answered matters wherever several
            // share one SSID.
            let mut line = Line::new();
            line.push_str("bssid ");
            for (index, byte) in bssid.iter().enumerate() {
                if index != 0 {
                    line.push_str(":");
                }
                line.push_hex(*byte as u32, 2);
            }
            console.write_output_line(framebuffer, line.as_str());

            let mut line = Line::new();
            line.push_str("channel ");
            line.push_u32(channel);
            line.push_str(", ");
            match wifi::station::auth_mode_name(auth_mode) {
                Some(name) => line.push_str(name),
                None => {
                    line.push_str("auth");
                    line.push_u32(auth_mode as u32);
                }
            }
            console.write_output_line(framebuffer, line.as_str());
            console.write_output_line(framebuffer, "run 'ipconfig dhcp' to get an address");
            ShellConnect::Associated(wifi_manager::Association {
                ssid,
                ssid_length,
                channel,
            })
        }
        wifi::station::Outcome::Disconnected { reason } => {
            let mut line = Line::new();
            line.push_str("disconnected, reason ");
            line.push_u32(reason);
            if let Some(name) = wifi::station::disconnect_reason_name(reason) {
                line.push_str(" ");
                line.push_str(name);
            }
            console.write_output_line(framebuffer, line.as_str());
            ShellConnect::Failed(wifi_manager::Failure::Disconnected(reason))
        }
        wifi::station::Outcome::TimedOut => {
            console.write_output_line(framebuffer, "no answer from the slave in time");
            ShellConnect::Failed(wifi_manager::Failure::AssociationTimedOut)
        }
    }
}

enum ShellConnect {
    NotStarted,
    Associated(wifi_manager::Association),
    Failed(wifi_manager::Failure),
}

fn cmd_wifistatus(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    session: &mut Option<wifi::Rpc>,
) {
    let Some(rpc) = session.as_mut() else {
        console.write_output_line(
            framebuffer,
            "no C6 link (run wifi scan or wifi connect first)",
        );
        return;
    };

    let Some((status, access_point)) = wifi::station::connected_access_point(rpc) else {
        console.write_output_line(framebuffer, "status: RPC failed, see UART log");
        return;
    };

    match access_point {
        Some(access_point) if status == 0 => {
            let mut line = Line::new();
            line.push_str("connected to ");
            push_ssid(&mut line, access_point.ssid());
            console.write_output_line(framebuffer, line.as_str());

            let mut line = Line::new();
            line.push_str("bssid ");
            for (index, byte) in access_point.bssid.iter().enumerate() {
                if index != 0 {
                    line.push_str(":");
                }
                line.push_hex(*byte as u32, 2);
            }
            console.write_output_line(framebuffer, line.as_str());

            let mut line = Line::new();
            line.push_str("channel ");
            line.push_u32(access_point.channel);
            line.push_str(", ");
            if access_point.rssi < 0 {
                line.push_str("-");
            }
            line.push_u32(access_point.rssi.unsigned_abs());
            line.push_str(" dBm");
            console.write_output_line(framebuffer, line.as_str());
        }
        _ => write_slave_status(console, framebuffer, "not connected", status),
    }

    let dropped = rpc.dropped_data_frames();
    if dropped != 0 {
        let mut line = Line::new();
        line.push_str("station frames received and dropped: ");
        line.push_u32(dropped);
        console.write_output_line(framebuffer, line.as_str());
    }

    for event in rpc.take_events() {
        let mut line = Line::new();
        line.push_str("pending event ");
        line.push_u32(event.msg_id);
        console.write_output_line(framebuffer, line.as_str());
    }
}

fn cmd_wifilog(console: &mut Console, framebuffer: &mut Framebuffer, manager: &WifiManager) {
    if manager.history().is_empty() {
        console.write_output_line(framebuffer, "no Wi-Fi manager transitions recorded");
        return;
    }

    for transition in manager.history() {
        let mut line = Line::new();
        line.push_u64(transition.at_ms);
        line.push_str("ms g");
        line.push_u32(transition.generation);
        line.push_str(" a");
        line.push_u32(transition.attempt);
        line.push_str(" ");
        line.push_str(wifi_phase_name(transition.from));
        line.push_str("->");
        line.push_str(wifi_phase_name(transition.to));
        line.push_str(" ");
        push_wifi_cause(&mut line, transition.cause);
        console.write_output_line(framebuffer, line.as_str());
    }
}

fn cmd_wifisaved(console: &mut Console, framebuffer: &mut Framebuffer, manager: &mut WifiManager) {
    let rpc = match manager.ensure_station() {
        Ok(rpc) => rpc,
        Err(_) => {
            console.write_output_line(framebuffer, "cannot start the C6 station; see UART log");
            return;
        }
    };

    let Some((status, config)) = wifi::station::station_config(rpc) else {
        console.write_output_line(framebuffer, "station config RPC failed; see UART log");
        return;
    };
    if status != 0 {
        write_slave_status(console, framebuffer, "get station config refused", status);
        return;
    }
    if config.ssid().is_empty() {
        console.write_output_line(framebuffer, "C6 station configuration: empty");
    } else {
        let mut line = Line::new();
        line.push_str("C6 station SSID: ");
        push_ssid(&mut line, config.ssid());
        console.write_output_line(framebuffer, line.as_str());
        console.write_output_line(
            framebuffer,
            if config.has_password() {
                "security credential present: yes"
            } else {
                "security credential present: no (open or empty)"
            },
        );
    }
    let (writes, failures, forgets) = manager.profile_diagnostics();
    let mut line = Line::new();
    line.push_str("profile writes this boot: ");
    line.push_u32(writes);
    line.push_str(", failed ");
    line.push_u32(failures);
    line.push_str(", forgets ");
    line.push_u32(forgets);
    console.write_output_line(framebuffer, line.as_str());
}

fn cmd_wififorget(console: &mut Console, framebuffer: &mut Framebuffer, manager: &mut WifiManager) {
    let was_enabled = manager.is_enabled();
    match manager.forget_saved_profile() {
        Ok(()) => console.write_output_line(
            framebuffer,
            if was_enabled {
                "saved Wi-Fi profile deleted; current connection may remain active"
            } else {
                "saved Wi-Fi profile deleted; Wi-Fi remains off"
            },
        ),
        Err(wifi_manager::Failure::ConfigStatus(status)) => {
            write_slave_status(console, framebuffer, "forget refused", status)
        }
        Err(_) => console.write_output_line(framebuffer, "forget: RPC failed, see UART log"),
    }
}

fn wifi_phase_name(phase: wifi_manager::Phase) -> &'static str {
    match phase {
        wifi_manager::Phase::Off => "off",
        wifi_manager::Phase::LinkDown => "link-down",
        wifi_manager::Phase::Idle => "idle",
        wifi_manager::Phase::Associating => "associating",
        wifi_manager::Phase::RetryWaiting => "retry-wait",
        wifi_manager::Phase::NeedsPassword => "needs-password",
        wifi_manager::Phase::Associated => "associated",
        wifi_manager::Phase::RequestingDhcp => "dhcp",
        wifi_manager::Phase::AssociatedNoLease => "no-lease",
        wifi_manager::Phase::Online => "online",
        wifi_manager::Phase::Failed => "failed",
    }
}

fn push_wifi_cause(line: &mut Line, cause: wifi_manager::Cause) {
    match cause {
        wifi_manager::Cause::Enabled => line.push_str("enabled"),
        wifi_manager::Cause::Disabled => line.push_str("disabled"),
        wifi_manager::Cause::Manual => line.push_str("manual"),
        wifi_manager::Cause::LinkReady => line.push_str("link-ready"),
        wifi_manager::Cause::ConnectRequested => line.push_str("connect"),
        wifi_manager::Cause::Connected => line.push_str("connected"),
        wifi_manager::Cause::Disconnected(reason) => {
            line.push_str("reason=");
            line.push_u32(reason);
        }
        wifi_manager::Cause::AssociationTimeout => line.push_str("association-timeout"),
        wifi_manager::Cause::RetryScheduled(delay_ms) => {
            line.push_str("retry-ms=");
            line.push_u32(delay_ms);
        }
        wifi_manager::Cause::RetryTimer => line.push_str("retry-timer"),
        wifi_manager::Cause::StaleEvent(event) => {
            line.push_str("stale-event=");
            line.push_u32(event);
        }
        wifi_manager::Cause::RpcFailed => line.push_str("rpc-failed"),
        wifi_manager::Cause::RpcStatus(status) => {
            line.push_str("rpc-status=0x");
            line.push_hex(status as u32, 8);
        }
        wifi_manager::Cause::DhcpStarted => line.push_str("dhcp-start"),
        wifi_manager::Cause::DhcpConfigured => line.push_str("dhcp-configured"),
        wifi_manager::Cause::DhcpTimeout => line.push_str("dhcp-timeout"),
        wifi_manager::Cause::DhcpLost => line.push_str("dhcp-lost"),
        wifi_manager::Cause::StableConnection => line.push_str("stable-reset"),
        wifi_manager::Cause::LinkLost => line.push_str("link-lost"),
        wifi_manager::Cause::StartupProfile => line.push_str("startup-profile"),
        wifi_manager::Cause::ProfileSaved => line.push_str("profile-saved"),
        wifi_manager::Cause::ProfileSaveFailed => line.push_str("profile-save-failed"),
        wifi_manager::Cause::ProfileForgotten => line.push_str("profile-forgotten"),
        wifi_manager::Cause::ReplacementDisconnected => line.push_str("replace-disconnect"),
        wifi_manager::Cause::DisconnectTimeout => line.push_str("disconnect-timeout"),
    }
}

fn cmd_wifidisconnect(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    session: &mut Option<wifi::Rpc>,
) -> bool {
    let Some(rpc) = session.as_mut() else {
        console.write_output_line(framebuffer, "no C6 link (nothing to disconnect)");
        return false;
    };

    match wifi::station::disconnect(rpc) {
        Some(0) => {
            // The slave reports the actual teardown as an event.
            match wifi::station::wait_for_connection(rpc, DISCONNECT_TIMEOUT_MS) {
                wifi::station::Outcome::Disconnected { reason } => {
                    let mut line = Line::new();
                    line.push_str("disconnected, reason ");
                    line.push_u32(reason);
                    console.write_output_line(framebuffer, line.as_str());
                }
                _ => console.write_output_line(framebuffer, "disconnect requested"),
            }
            true
        }
        Some(status) => {
            write_slave_status(console, framebuffer, "disconnect refused", status);
            false
        }
        None => {
            console.write_output_line(framebuffer, "disconnect: RPC failed, see UART log");
            false
        }
    }
}

/// How long a connection attempt may take before the shell stops waiting.
/// A slow AP plus a WPA handshake can take several seconds; the slave gives
/// up on its own well before this.
const CONNECT_TIMEOUT_MS: u32 = 20_000;
/// Tearing an association down is local to the radio and quick.
const DISCONNECT_TIMEOUT_MS: u32 = 3_000;

/// Writes an SSID, replacing anything the console font cannot draw.
fn push_ssid(line: &mut Line, ssid: &[u8]) {
    if ssid.is_empty() {
        line.push_str("(hidden)");
        return;
    }
    let mut text = [0u8; wifi::station::SSID_MAX_BYTES];
    for (slot, &byte) in text.iter_mut().zip(ssid) {
        *slot = if byte.is_ascii_graphic() || byte == b' ' {
            byte
        } else {
            b'.'
        };
    }
    line.push_str(core::str::from_utf8(&text[..ssid.len()]).unwrap_or("?"));
}

/// Reports a request that reached the slave and came back refused. The code
/// is the slave's own `esp_err_t`, so it is shown as-is rather than mapped.
fn write_slave_status(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    what: &str,
    status: i32,
) {
    let mut line = Line::new();
    line.push_str(what);
    line.push_str(": slave status ");
    line.push_u32(status as u32);
    line.push_str(" (0x");
    line.push_hex(status as u32, 4);
    line.push_str(")");
    console.write_output_line(framebuffer, line.as_str());
}

/// Returns the open C6 session together with its IP stack, building the
/// stack the first time round.
///
/// The stack is kept beside the session rather than inside it because the
/// two have different lifetimes: `wifi info` and `wifi up` deliberately throw
/// the session away, and an interface holding an address obtained over a
/// link that no longer exists would be a lie.
fn net_session<'a>(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    session: &'a mut Option<wifi::Rpc>,
    stack: &'a mut Option<net::Stack>,
) -> Option<(&'a mut wifi::Rpc, &'a mut net::Stack)> {
    let rpc = wifi_session(console, framebuffer, session)?;

    if stack.is_none() {
        if !tick::is_running() {
            console.write_output_line(framebuffer, "no millisecond tick; the IP stack needs one");
            return None;
        }

        // The interface answers ARP for the radio's own address, so the
        // hardware address has to be the one the C6 transmits with.
        let Some((status, mac)) = wifi::rpc::get_mac_address(rpc, wifi::rpc::WIFI_IF_STA) else {
            console.write_output_line(framebuffer, "station MAC: RPC failed, see UART log");
            return None;
        };
        if status != 0 {
            write_slave_status(console, framebuffer, "station MAC", status);
            return None;
        }

        let mut line = Line::new();
        line.push_str("interface up, mac ");
        push_mac(&mut line, &mac);
        console.write_output_line(framebuffer, line.as_str());
        *stack = Some(net::Stack::new(rpc, mac));
    }

    Some((rpc, stack.as_mut()?))
}

/// `ipconfig` -- show the current IPv4 settings, or change where they come
/// from.
///
/// With no argument this only reports. `dhcp` starts the client and waits
/// for a lease; an address in CIDR form sets one by hand, which is what
/// Stage 3 of `docs/plans/archive/TCPIP_PLAN.md` used before DHCP existed and what still
/// works on a network without a server. `dns` replaces the resolvers
/// without touching the address, which works on a lease as well as a
/// static address -- the way to aim the stack at a resolver that is known
/// not to answer and watch it fall through to the next one.
fn cmd_ipconfig(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) -> Option<IpPolicy> {
    let Some((rpc, stack)) = net_session(console, framebuffer, session, stack) else {
        return None;
    };

    let mut policy = None;
    let (verb, rest) = split_first_word(trim(argument));
    match verb {
        b"" => {}
        b"release" => {
            stack.release();
            console.write_output_line(framebuffer, "address released, DHCP stopped");
            policy = Some(IpPolicy::Unconfigured);
        }
        b"dns" => {
            let mut servers = Vec::new();
            let mut remaining = trim(rest);
            while !remaining.is_empty() {
                let (word, next) = split_first_word(remaining);
                let Some(server) = parse_ipv4(word) else {
                    console.write_output_line(framebuffer, "each resolver must be an IPv4 address");
                    return None;
                };
                servers.push(server);
                remaining = trim(next);
            }
            // Not "no resolvers to remove": with no address there is no
            // configuration to hang them on, and one installed now would be
            // thrown away by the next lease anyway.
            if !stack.set_dns_servers(servers) {
                console.write_output_line(framebuffer, "no address; set one before the resolvers");
                return None;
            }
        }
        b"dhcp" => {
            stack.start_dhcp();
            policy = Some(IpPolicy::Dhcp);
            console.write_output_line(framebuffer, "requesting a lease...");
            let acquired = stack.pump_until(rpc, DHCP_TIMEOUT_MS, |stack| stack.has_address());
            if !acquired {
                let reason = if rpc.is_alive() {
                    "no DHCP answer; is the station associated?"
                } else {
                    "the C6 link was lost while waiting for DHCP"
                };
                console.write_output_line(framebuffer, reason);
                return policy;
            }
        }
        _ => {
            let Some((address, prefix)) = parse_ipv4_cidr(verb) else {
                console.write_output_line(
                    framebuffer,
                    "usage: ipconfig [dhcp|release|dns <a.b.c.d>...|<a.b.c.d/len> [gw]]",
                );
                return None;
            };
            let gateway = trim(rest);
            let gateway = if gateway.is_empty() {
                None
            } else {
                match parse_ipv4(gateway) {
                    Some(gateway) => Some(gateway),
                    None => {
                        console
                            .write_output_line(framebuffer, "the gateway is not an IPv4 address");
                        return None;
                    }
                }
            };
            // No resolvers: a hand-set address comes with no way to learn
            // them, so `ipconfig dns` is the next command if names are
            // wanted. Carrying the previous lease's resolvers over would
            // point at servers this configuration never promised.
            stack.set_static(Ipv4Cidr::new(address, prefix), gateway, Vec::new());
            policy = Some(IpPolicy::Static);
        }
    }

    write_ipconfig(console, framebuffer, rpc, stack);
    policy
}

/// Reports whether the radio is still associated.
///
/// Nothing else in this path can tell. smoltcp keeps building frames, the
/// transport keeps accepting them, and the co-processor silently discards
/// station traffic while it is not connected -- so a lost association and a
/// network that ignores us produce identical symptoms, right down to the
/// transmit counter going up.
fn write_association(console: &mut Console, framebuffer: &mut Framebuffer, rpc: &mut wifi::Rpc) {
    match wifi::station::connected_access_point(rpc) {
        Some((0, Some(access_point))) => {
            let mut line = Line::new();
            line.push_str("associated to ");
            push_ssid(&mut line, access_point.ssid());
            console.write_output_line(framebuffer, line.as_str());
        }
        Some((status, _)) => {
            write_slave_status(
                console,
                framebuffer,
                "NOT associated, so nothing can be sent",
                status,
            );
            console.write_output_line(framebuffer, "run wifi connect before expecting traffic");
        }
        None => console.write_output_line(framebuffer, "association: RPC failed, see UART log"),
    }
}

fn write_ipconfig(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    rpc: &mut wifi::Rpc,
    stack: &net::Stack,
) {
    write_association(console, framebuffer, rpc);

    let mut line = Line::new();
    line.push_str("mac ");
    push_mac(&mut line, &stack.mac());
    line.push_str(", ");
    line.push_str(match stack.source() {
        net::AddressSource::None => "unconfigured",
        net::AddressSource::Dhcp => "dhcp",
        net::AddressSource::Static => "static",
    });
    console.write_output_line(framebuffer, line.as_str());

    let Some(config) = stack.config() else {
        console.write_output_line(framebuffer, "no address");
        write_frame_stats(console, framebuffer, rpc, stack);
        return;
    };

    let mut line = Line::new();
    line.push_str("address ");
    push_ipv4(&mut line, config.address.address());
    line.push_str("/");
    line.push_u32(config.address.prefix_len() as u32);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("gateway ");
    match config.router {
        Some(router) => push_ipv4(&mut line, router),
        None => line.push_str("(none)"),
    }
    console.write_output_line(framebuffer, line.as_str());

    if config.dns_servers.is_empty() {
        console.write_output_line(framebuffer, "dns (none; only numeric addresses will work)");
    } else {
        for server in &config.dns_servers {
            let mut line = Line::new();
            line.push_str("dns ");
            push_ipv4(&mut line, *server);
            console.write_output_line(framebuffer, line.as_str());
        }
    }

    if let Some(server) = config.server {
        let mut line = Line::new();
        line.push_str("leased by ");
        push_ipv4(&mut line, server);
        line.push_str(", held ");
        line.push_u32(((tick::now_ms().saturating_sub(config.acquired_ms)) / 1000) as u32);
        line.push_str(" s");
        console.write_output_line(framebuffer, line.as_str());
    }

    write_frame_stats(console, framebuffer, rpc, stack);
}

/// Reports what has become of station traffic in both directions.
///
/// The transmit side is the half that cannot be seen from anywhere else: a
/// frame the co-processor never sent looks, from the other end of the
/// network, exactly like a frame that was never generated. `sent` moving
/// while a peer sees nothing puts the fault past this board; `sent` stuck
/// at zero puts it here.
fn write_frame_stats(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    rpc: &wifi::Rpc,
    stack: &net::Stack,
) {
    let stats = rpc.data_frame_stats();

    let mut line = Line::new();
    line.push_str("rx queued ");
    line.push_u32(stats.queued as u32);
    line.push_str(", delivered ");
    line.push_u32(stats.delivered);
    line.push_str(", dropped ");
    line.push_u32(stats.dropped);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("tx sent ");
    line.push_u32(stats.sent);
    line.push_str(", throttled ");
    line.push_u32(stats.throttled);
    line.push_str(", failed ");
    line.push_u32(stats.failed);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("slave throttling: ");
    line.push_str(if stats.throttling { "yes" } else { "no" });
    line.push_str(", leases lost ");
    line.push_u32(stack.deconfigured_count());
    console.write_output_line(framebuffer, line.as_str());
}

/// `nslookup` -- resolve a name and show what came back.
///
/// The one command that always sends a query, even for an argument that
/// would parse as an address: everything else short-circuits numeric
/// addresses, so without this there is no way to ask the resolver a
/// question and see only its answer.
fn cmd_nslookup(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) {
    let name = trim(argument);
    if name.is_empty() || !split_first_word(name).1.is_empty() {
        console.write_output_line(framebuffer, "usage: nslookup <name>");
        return;
    }

    let Some((rpc, stack)) = net_session(console, framebuffer, session, stack) else {
        return;
    };
    if !stack.has_address() {
        console.write_output_line(framebuffer, "no address; run ipconfig dhcp first");
        return;
    }

    if let Some(server) = stack.dns_servers().first() {
        let mut line = Line::new();
        line.push_str("server ");
        push_ipv4(&mut line, *server);
        console.write_output_line(framebuffer, line.as_str());
    }

    match net::dns::resolve(stack, rpc, name) {
        Ok(answer) => {
            for address in &answer.addresses {
                let mut line = Line::new();
                line.push_ascii(name);
                line.push_str(" has address ");
                push_ipv4(&mut line, *address);
                console.write_output_line(framebuffer, line.as_str());
            }
            let mut line = Line::new();
            line.push_str("in ");
            line.push_u32(answer.elapsed_ms as u32);
            line.push_str(" ms");
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => write_dns_error(console, framebuffer, name, error),
    }
}

/// Reports a failed lookup. Shared by `nslookup` and by every command that
/// takes a destination, so the same failure always reads the same way.
fn write_dns_error(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    name: &[u8],
    error: net::dns::Error,
) {
    // The name is worth repeating for this one: it is the answer to the
    // question, not a report about the machinery.
    if let net::dns::Error::NotFound = error {
        let mut line = Line::new();
        line.push_ascii(name);
        line.push_str(": no such name");
        console.write_output_line(framebuffer, line.as_str());
        return;
    }

    console.write_output_line(
        framebuffer,
        match error {
            net::dns::Error::NoServers => "no resolver configured; try 'ipconfig dns <a.b.c.d>'",
            net::dns::Error::InvalidName => "that is not a usable host name",
            net::dns::Error::TimedOut => "the resolver did not answer",
            net::dns::Error::LinkLost => "the C6 link was lost during the lookup",
            net::dns::Error::NotFound | net::dns::Error::Local => {
                "the lookup failed locally, see UART log"
            }
        },
    );
}

/// Turns a destination argument into an address.
///
/// A literal `a.b.c.d` is taken as it stands and **no query is sent**:
/// numeric destinations have to keep working on a network with no
/// resolver, because that is the state every bring-up starts in and the
/// one worth being able to ping from.
///
/// Reports its own failure, so a caller only has to stop.
pub(super) fn resolve_target(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    rpc: &mut wifi::Rpc,
    stack: &mut net::Stack,
    text: &[u8],
) -> Option<Ipv4Address> {
    if let Some(address) = parse_ipv4(text) {
        return Some(address);
    }

    match net::dns::resolve(stack, rpc, text) {
        Ok(answer) => {
            // `resolve` never returns an empty answer, but going quiet is
            // the one way a shell command must not fail: say the same thing
            // an empty answer means rather than returning with no output.
            let Some(&address) = answer.addresses.first() else {
                write_dns_error(console, framebuffer, text, net::dns::Error::NotFound);
                return None;
            };
            let mut line = Line::new();
            line.push_ascii(text);
            line.push_str(" is ");
            push_ipv4(&mut line, address);
            console.write_output_line(framebuffer, line.as_str());
            Some(address)
        }
        Err(error) => {
            write_dns_error(console, framebuffer, text, error);
            None
        }
    }
}

fn cmd_ping(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) {
    // Only the shape of the arguments is checked here. Resolving the
    // destination needs the stack, so it has to wait until the link is up
    // -- but a mistyped count is a usage error, and bringing up the C6 to
    // report one would be a slow way to say so.
    let (target, rest) = split_first_word(trim(argument));
    if target.is_empty() {
        console.write_output_line(framebuffer, "usage: ping <host|a.b.c.d> [count]");
        return;
    }
    let count = match trim(rest) {
        b"" => DEFAULT_PING_COUNT,
        text => match parse_u32(text) {
            Some(count) if (1..=64).contains(&count) => count,
            _ => {
                console.write_output_line(framebuffer, "count must be 1..64");
                return;
            }
        },
    };

    let Some((rpc, stack)) = net_session(console, framebuffer, session, stack) else {
        return;
    };
    if !stack.has_address() {
        console.write_output_line(framebuffer, "no address; run ipconfig dhcp first");
        return;
    }
    let Some(address) = resolve_target(console, framebuffer, rpc, stack, target) else {
        return;
    };

    let mut line = Line::new();
    line.push_str("pinging ");
    push_ipv4(&mut line, address);
    console.write_output_line(framebuffer, line.as_str());

    let mut report = |reply: net::ping::Reply| {
        let mut line = Line::new();
        match reply {
            net::ping::Reply::Received {
                sequence,
                elapsed_ms,
            } => {
                line.push_str("seq ");
                line.push_u32(sequence as u32);
                line.push_str(": ");
                line.push_u32(elapsed_ms as u32);
                line.push_str(" ms");
            }
            net::ping::Reply::TimedOut { sequence } => {
                line.push_str("seq ");
                line.push_u32(sequence as u32);
                line.push_str(": no reply");
            }
        }
        console.write_output_line(framebuffer, line.as_str());
    };

    let Some(summary) = net::ping::run(stack, rpc, address, count, &mut report) else {
        console.write_output_line(framebuffer, "ping: could not open an ICMP socket");
        return;
    };

    let mut line = Line::new();
    line.push_u32(summary.sent);
    line.push_str(" sent, ");
    line.push_u32(summary.received);
    line.push_str(" received");
    console.write_output_line(framebuffer, line.as_str());

    if summary.received != 0 {
        let mut line = Line::new();
        line.push_str("min/avg/max ");
        line.push_u32(summary.min_ms as u32);
        line.push_str("/");
        line.push_u32(summary.average_ms() as u32);
        line.push_str("/");
        line.push_u32(summary.max_ms as u32);
        line.push_str(" ms");
        console.write_output_line(framebuffer, line.as_str());
    }
}

fn cmd_tftpget(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    // Reborrowed per use below: the download opens the volume twice, once
    // for the transfer and once to put the result under its real name.
    mut ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &State,
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) {
    let (target, rest) = split_first_word(trim(argument));
    let filename = trim(rest);
    if target.is_empty() || filename.is_empty() {
        console.write_output_line(framebuffer, "usage: tftpget <host|a.b.c.d> <file>");
        return;
    }

    // Worked out before the network is touched: a destination the shell
    // cannot write to is worth finding out about now rather than after a
    // transfer that then has nowhere to go.
    let download = match files::download_to(&state.cwd, filename) {
        Ok(download) => download,
        Err(error) => {
            let mut line = Line::new();
            line.push_str("tftpget: ");
            line.push_str(fs::vfs::error_name(error));
            console.write_output_line(framebuffer, line.as_str());
            return;
        }
    };

    let Some((rpc, stack)) = net_session(console, framebuffer, session, stack) else {
        return;
    };
    if !stack.has_address() {
        console.write_output_line(framebuffer, "no address; run ipconfig dhcp first");
        return;
    }
    let Some(server) = resolve_target(console, framebuffer, rpc, stack, target) else {
        return;
    };

    console.write_output_line(framebuffer, "requesting the file...");
    let started = tick::now_ms();
    let mut crc = net::tftp::Crc32::new();

    // One `write_stream` around the whole transfer: the volume and the
    // writer are opened once and the blocks go straight through as they
    // arrive. TFTP is lock-step, so a write that takes a moment only makes
    // the transfer wait.
    let stream = with_devices(usb_host, ram_disk.as_deref_mut(), |devices| {
        vfs.write_stream(
            devices,
            download.part.as_str(),
            fs::vfs::OpenMode::Truncate,
            |write| {
                // Progress is reported by tenths of a MiB so a long transfer
                // shows movement without one console line per 512-byte block.
                let mut next_report = PROGRESS_STEP_BYTES;
                let mut received = 0usize;
                let mut sink = |block: &[u8]| {
                    crc.update(block);
                    received += block.len();
                    if received >= next_report {
                        next_report = received + PROGRESS_STEP_BYTES;
                        let mut line = Line::new();
                        line.push_u32(received as u32);
                        line.push_str(" bytes...");
                        console.write_output_line(framebuffer, line.as_str());
                    }
                    // A refused write stops the transfer here rather than
                    // letting it run to the end with nowhere to put the rest.
                    write(block)
                };
                net::tftp::get(stack, rpc, server, filename, &mut sink)
            },
        )
    });

    let stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            let mut line = Line::new();
            line.push_str("tftpget: cannot write to ");
            line.push_str(download.part.as_str());
            line.push_str(": ");
            line.push_str(fs::vfs::error_name(error));
            console.write_output_line(framebuffer, line.as_str());
            if error == fs::vfs::FsError::ReadOnly {
                console.write_output_line(
                    framebuffer,
                    "the file lands in the current directory; cd off a read-only mount",
                );
            }
            return;
        }
    };
    // The write failing is the transfer failing, even when every byte
    // arrived: a saved file that is missing its tail is worse than no file.
    let outcome = match stream.interrupted {
        Some(error) => Err(error),
        None => Ok(stream.value),
    };

    match outcome {
        Ok(Ok(total)) => {
            let mut line = Line::new();
            line.push_str("got ");
            line.push_u32(total as u32);
            line.push_str(" bytes, crc32 0x");
            line.push_hex(crc.finish(), 8);
            console.write_output_line(framebuffer, line.as_str());

            let mut line = Line::new();
            line.push_str("in ");
            line.push_u32((tick::now_ms().saturating_sub(started)) as u32);
            line.push_str(" ms, ");
            line.push_u32(net::tftp::throughput(total, started) / 1024);
            line.push_str(" KiB/s");
            console.write_output_line(framebuffer, line.as_str());
            with_devices(usb_host, ram_disk.as_deref_mut(), |devices| {
                files::finish_download(console, framebuffer, devices, vfs, &download, true)
            });
            return;
        }
        // The volume stopped taking bytes. What the transfer would have gone
        // on to do does not matter; there is nowhere to put the result.
        Err(error) => {
            let mut line = Line::new();
            line.push_str("tftpget: ");
            line.push_str(fs::vfs::error_name(error));
            line.push_str(" after ");
            line.push_u64(stream.written);
            line.push_str(" bytes");
            console.write_output_line(framebuffer, line.as_str());
        }
        Ok(Err(error)) => report_tftp_error(console, framebuffer, error),
    }
    with_devices(usb_host, ram_disk, |devices| {
        files::finish_download(console, framebuffer, devices, vfs, &download, false)
    });
}

fn report_tftp_error(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    error: net::tftp::Error,
) {
    match error {
        net::tftp::Error::Server { code, message } => {
            let mut line = Line::new();
            line.push_str("server error ");
            line.push_u32(code as u32);
            line.push_str(": ");
            line.push_ascii(&message);
            console.write_output_line(framebuffer, line.as_str());
        }
        net::tftp::Error::TimedOut => {
            console.write_output_line(framebuffer, "no answer from the TFTP server")
        }
        net::tftp::Error::LinkLost => {
            console.write_output_line(framebuffer, "the C6 link was lost during the transfer")
        }
        // The sink only refuses when the write failed, and that is reported
        // with the filesystem's own reason before this is reached.
        net::tftp::Error::SinkRefused => {
            console.write_output_line(framebuffer, "tftpget: the transfer was abandoned")
        }
        net::tftp::Error::Local => {
            console.write_output_line(framebuffer, "tftpget: a local socket operation failed")
        }
    }
}

/// `tls` -- one unauthenticated TLS 1.3 fetch, reported in detail.
///
/// A diagnostic rather than a way to browse. It runs the *same*
/// `net::http::Transaction` the plaintext path runs, over a TLS transport,
/// which is the point: if the status line, the header block and the body
/// framing needed their own code for HTTPS there would be two answers to
/// "where does this body end" and no way to know which was right.
///
/// What it prints beyond the head is the state of the handshake and the
/// shape of the polling, because those are the two things
/// `docs/plans/archive/TLS_PLAN.md` says have to be measured on real hardware before the
/// browser is allowed near HTTPS. The body is counted and dropped;
/// `httpget` is where saving one belongs.
fn cmd_tls(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) {
    let (target, rest) = split_first_word(trim(argument));
    let path = match trim(rest) {
        b"" => b"/".as_slice(),
        path => path,
    };
    let Some((host, port)) = split_host_port_with_default(target, 443) else {
        console.write_output_line(framebuffer, "usage: tls <host|a.b.c.d>[:port] [path]");
        return;
    };

    let Some((rpc, stack)) = net_session(console, framebuffer, session, stack) else {
        return;
    };
    if !stack.has_address() {
        console.write_output_line(framebuffer, "no address; run ipconfig dhcp first");
        return;
    }
    let Some(address) = resolve_target(console, framebuffer, rpc, stack, host) else {
        return;
    };
    let Ok(server_name) = core::str::from_utf8(host) else {
        console.write_output_line(framebuffer, "the host is not valid UTF-8");
        return;
    };

    // The `Host:` header carries the port when it is not 443, the same rule
    // the browser's `Url::host_header` applies -- and the same rule
    // `net::http::get` applies for the plaintext default.
    let mut host_header = alloc::vec::Vec::new();
    host_header.extend_from_slice(host);
    if port != 443 {
        host_header.push(b':');
        push_decimal_bytes(&mut host_header, port as u32);
    }

    console.write_output_line(framebuffer, "handshaking...");
    let mut transaction = match net::http::Transaction::start(
        stack,
        address,
        port,
        &host_header,
        path,
        u64::MAX,
        net::transport::Security::Tls {
            server_name,
            policy: net::pins::policy_for(server_name),
        },
    ) {
        Ok(transaction) => transaction,
        Err(error) => {
            write_http_failure(console, framebuffer, error);
            return;
        }
    };

    let mut announced = false;
    let mut body = 0u64;
    while !transaction.is_finished() {
        let mut sink = |bytes: &[u8]| {
            body += bytes.len() as u64;
            true
        };
        let progress = transaction.poll(stack, rpc, net::http::DEFAULT_POLL_BUDGET, &mut sink);
        if !announced && let Some(authentication) = transaction.authentication() {
            announced = true;
            let mut line = Line::new();
            line.push_str("handshake: ");
            line.push_str(authentication.label());
            console.write_output_line(framebuffer, line.as_str());
        }
        if progress == net::http::Progress::HeadReady
            && let Some(head) = transaction.head()
        {
            let mut line = Line::new();
            line.push_str("status ");
            match head.status {
                Some(status) => line.push_u32(status as u32),
                None => line.push_str("(not an HTTP status line)"),
            }
            if head.chunked {
                line.push_str(", chunked");
            }
            if let Some(length) = head.content_length {
                line.push_str(", length ");
                line.push_u64(length);
            }
            console.write_output_line(framebuffer, line.as_str());
        }
    }

    let failure = transaction.error();
    let without_notify = transaction.closed_without_notify();
    let tls_stats = transaction.tls_stats();
    let stats = transaction.close(stack, rpc);

    let mut line = Line::new();
    line.push_str("body ");
    line.push_u64(body);
    line.push_str(" B, plaintext received ");
    line.push_u32(stats.received as u32);
    console.write_output_line(framebuffer, line.as_str());

    if let Some(tls_stats) = tls_stats {
        let mut line = Line::new();
        line.push_str("ciphertext in ");
        line.push_u32(tls_stats.received as u32);
        line.push_str(" out ");
        line.push_u32(tls_stats.sent as u32);
        line.push_str(", handshake ");
        line.push_u64(tls_stats.handshake_ms);
        line.push_str(" ms");
        console.write_output_line(framebuffer, line.as_str());

        let mut line = Line::new();
        line.push_str("total ");
        line.push_u64(stats.elapsed_ms);
        line.push_str(" ms, polls ");
        line.push_u32(stats.polls);
        line.push_str(", longest TLS poll ");
        line.push_u32(tls_stats.longest_poll_us);
        line.push_str(" us");
        console.write_output_line(framebuffer, line.as_str());
    }

    if without_notify {
        // Worth saying rather than hiding: the stream ended with a TCP
        // close instead of a close_notify, so TLS cannot vouch that nothing
        // was cut off the end. Whether the body was whole is the HTTP
        // framing's answer, which is the line above this one.
        console.write_output_line(
            framebuffer,
            "note: the server closed without close_notify (normal for HTTP/1.0)",
        );
    }
    if let Some(error) = failure {
        write_http_failure(console, framebuffer, error);
    }
}

fn write_http_failure(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    error: net::http::Error,
) {
    let mut line = Line::new();
    line.push_str(net::http::error_name(error));
    line.push_str(": ");
    line.push_str(net::http::error_text(error));
    console.write_output_line(framebuffer, line.as_str());
}

fn push_decimal_bytes(out: &mut alloc::vec::Vec<u8>, value: u32) {
    if value >= 10 {
        push_decimal_bytes(out, value / 10);
    }
    out.push(b'0' + (value % 10) as u8);
}

/// The transport `httpget` was asked for.
///
/// `secure` came from the scheme the user typed and from nothing else. The
/// host is passed straight through as the SNI name, so what is asked for
/// and what is connected to are the same string.
fn httpget_security(host: &[u8], secure: bool) -> net::transport::Security<'_> {
    if !secure {
        return net::transport::Security::Plain;
    }
    match core::str::from_utf8(host) {
        Ok(server_name) => net::transport::Security::Tls {
            server_name,
            policy: net::pins::policy_for(server_name),
        },
        // Unreachable in practice: the URL parser has already accepted the
        // host as text. Falling back to plaintext would be a downgrade, so
        // this fails the connection instead by asking for a name no server
        // will match -- there is no "plaintext, but they asked for TLS".
        Err(_) => net::transport::Security::Tls {
            server_name: "",
            policy: net::tls::PinPolicy::UNAUTHENTICATED,
        },
    }
}

fn cmd_httpget(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    mut ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &State,
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) {
    let (target, rest) = split_first_word(trim(argument));

    // Two shapes, and which one was typed decides the transport. A bare
    // host is plaintext, exactly as this command has always been; TLS needs
    // the scheme spelled out. The command never chooses encryption on its
    // own in either direction -- a `httpget` that quietly used TLS would be
    // as wrong as an `https://` one that quietly did not.
    let parsed;
    let (host, port, path, secure) =
        if browser::url::has_scheme(core::str::from_utf8(target).unwrap_or("")) {
            if !trim(rest).is_empty() {
                console.write_output_line(
                    framebuffer,
                    "a url carries its own path; do not give a second one",
                );
                return;
            }
            let Ok(text) = core::str::from_utf8(target) else {
                console.write_output_line(framebuffer, "the address has to be ASCII");
                return;
            };
            parsed = match browser::url::Url::parse(text) {
                Ok(url) => url,
                Err(error) => {
                    let mut line = Line::new();
                    line.push_str("bad address: ");
                    line.push_str(browser::url::error_text(error));
                    console.write_output_line(framebuffer, line.as_str());
                    return;
                }
            };
            let Ok(request_target) = parsed.request_target() else {
                console.write_output_line(framebuffer, "out of memory");
                return;
            };
            // Leaked into a local so the borrows below outlive this block; the
            // `Url` owns its strings and is dropped with the command.
            let path = match request_target.as_str() {
                "" => "/",
                path => path,
            };
            let path: alloc::vec::Vec<u8> = path.as_bytes().to_vec();
            (
                parsed.host().as_bytes(),
                parsed.port(),
                path,
                parsed.scheme() == browser::url::Scheme::Https,
            )
        } else {
            let path = match trim(rest) {
                b"" => b"/".as_slice(),
                path => path,
            };
            // The port has to come off before anything else: whatever is left
            // is the host, and it is a name as often as an address.
            let Some((host, port)) = split_host_port(target) else {
                console.write_output_line(
                    framebuffer,
                    "usage: httpget <host|a.b.c.d>[:port] [path] | httpget <url>",
                );
                return;
            };
            (host, port, path.to_vec(), false)
        };
    let path = path.as_slice();

    // A path with no last component -- `/`, or one ending in `/` -- names no
    // file, so there is nothing to save it as. That is the shape of the
    // request this command started life as, and it still works: the headers
    // are printed and the body is counted and dropped.
    let download = if files::names_a_file(path) {
        match files::download_to(&state.cwd, path) {
            Ok(download) => Some(download),
            Err(error) => {
                let mut line = Line::new();
                line.push_str("httpget: ");
                line.push_str(fs::vfs::error_name(error));
                console.write_output_line(framebuffer, line.as_str());
                return;
            }
        }
    } else {
        None
    };

    let Some((rpc, stack)) = net_session(console, framebuffer, session, stack) else {
        return;
    };
    if !stack.has_address() {
        console.write_output_line(framebuffer, "no address; run ipconfig dhcp first");
        return;
    }
    let Some(address) = resolve_target(console, framebuffer, rpc, stack, host) else {
        return;
    };

    console.write_output_line(framebuffer, "connecting...");
    // `host` rather than `address`: a server sharing one address between
    // several sites picks the site from this header, so sending the
    // resolved address would ask for whichever one is the default.
    let Some(download) = download.as_ref() else {
        let mut discard = |_: &[u8]| true;
        let outcome = net::http::get(
            stack,
            rpc,
            address,
            port,
            host,
            path,
            httpget_security(host, secure),
            &mut discard,
        );
        report_http(console, framebuffer, outcome, None);
        return;
    };

    let stream = with_devices(usb_host, ram_disk.as_deref_mut(), |devices| {
        vfs.write_stream(
            devices,
            download.part.as_str(),
            fs::vfs::OpenMode::Truncate,
            |write| {
                net::http::get(
                    stack,
                    rpc,
                    address,
                    port,
                    host,
                    path,
                    httpget_security(host, secure),
                    &mut |body| write(body),
                )
            },
        )
    });

    let stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            let mut line = Line::new();
            line.push_str("httpget: cannot write to ");
            line.push_str(download.part.as_str());
            line.push_str(": ");
            line.push_str(fs::vfs::error_name(error));
            console.write_output_line(framebuffer, line.as_str());
            if error == fs::vfs::FsError::ReadOnly {
                console.write_output_line(
                    framebuffer,
                    "the body lands in the current directory; cd off a read-only mount",
                );
            }
            return;
        }
    };

    if let Some(error) = stream.interrupted {
        let mut line = Line::new();
        line.push_str("httpget: ");
        line.push_str(fs::vfs::error_name(error));
        line.push_str(" after ");
        line.push_u64(stream.written);
        line.push_str(" bytes");
        console.write_output_line(framebuffer, line.as_str());
        with_devices(usb_host, ram_disk.as_deref_mut(), |devices| {
            files::finish_download(console, framebuffer, devices, vfs, download, false)
        });
        return;
    }

    // A status other than success has a body -- an error page -- and keeping
    // it under the name the user asked for would put a file there that is
    // not the file they wanted. The headers still get printed, so what
    // happened is visible.
    let keep = matches!(stream.value.as_ref().ok().and_then(|response| response.status),
        Some(status) if (200..300).contains(&status));
    report_http(console, framebuffer, stream.value, Some(stream.written));
    with_devices(usb_host, ram_disk, |devices| {
        files::finish_download(console, framebuffer, devices, vfs, download, keep)
    });
}

/// `httpstream` -- the browser's HTTP path, driven from the console.
///
/// Only the session and the address are arranged here; everything the
/// command actually does is in `browsertest`, which is where the fixture
/// walk will join it.
fn cmd_httpstream(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) {
    // The address is resolved before the link is brought up: a mistyped
    // one is a usage error, and starting the C6 to report it is a slow way
    // to say so.
    let (target, rest) = split_first_word(trim(argument));
    let Some(url) = resolve_address(console, framebuffer, target) else {
        return;
    };
    let Some((rpc, stack)) = net_session(console, framebuffer, session, stack) else {
        return;
    };
    if !stack.has_address() {
        console.write_output_line(framebuffer, "no address; run ipconfig dhcp first");
        return;
    }
    browsertest::run(console, framebuffer, &url, rest, rpc, stack);
}

/// `bt` -- walk every endpoint the fixture server lists.
fn cmd_browsertest(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) {
    // The fixture server's address is the base every path in the manifest
    // resolves against, so the walk needs it before anything else. It is
    // typed rather than remembered: `bt 192.168.0.2:8080` is short enough
    // once a missing scheme is supplied.
    let (target, rest) = split_first_word(trim(argument));
    let Some(base) = resolve_address(console, framebuffer, target) else {
        return;
    };
    let rest = trim(rest);
    let rounds = if rest.is_empty() {
        1
    } else {
        match parse_u32(rest) {
            Some(rounds) if rounds > 0 && rounds <= 200 => rounds,
            _ => {
                console.write_output_line(framebuffer, "usage: bt <url> [rounds]   (1..200)");
                return;
            }
        }
    };
    let Some((rpc, stack)) = net_session(console, framebuffer, session, stack) else {
        return;
    };
    if !stack.has_address() {
        console.write_output_line(framebuffer, "no address; run ipconfig dhcp first");
        return;
    }
    browsertest::walk(console, framebuffer, &base, rounds, rpc, stack);
}

/// Says what the browser will not be able to do, before it opens.
///
/// The plan asks for the preparation steps to appear in the shell rather
/// than on the viewer's screen, and this is why: `wifi connect` and
/// `ipconfig dhcp` are shell commands, and a message about them belongs
/// where they can be typed.
fn browser_readiness(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    stack: Option<&net::Stack>,
) {
    let ready = match stack {
        Some(stack) => stack.has_address(),
        None => false,
    };
    if ready {
        return;
    }
    console.write_output_line(
        framebuffer,
        "no address: fetching will fail until 'wifi connect <ssid> <key>' and",
    );
    console.write_output_line(
        framebuffer,
        "'ipconfig dhcp' have run. the built-in pages work without them",
    );
}

/// Turns what was typed into an address, supplying `http://` if it has no
/// scheme.
///
/// The completion is `Url::parse_typed`, which the viewer's address field
/// uses as well, so the shell and the field accept exactly the same
/// spellings. What it does *not* do is complete against a remembered base.
/// `hbase` did, and removing it leaves one answer to "which server was
/// meant" instead of two -- the argument, and nothing else.
fn resolve_address(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    target: &[u8],
) -> Option<Url> {
    if target.is_empty() {
        console.write_output_line(framebuffer, "give an address, e.g. 192.168.0.2:8080/x");
        return None;
    }
    let Ok(text) = core::str::from_utf8(target) else {
        console.write_output_line(framebuffer, "the address has to be ASCII");
        return None;
    };
    match Url::parse_typed(text) {
        Ok(url) => Some(url),
        Err(error) => {
            let mut line = Line::new();
            line.push_str("bad address: ");
            line.push_str(browser::url::error_text(error));
            console.write_output_line(framebuffer, line.as_str());
            None
        }
    }
}

/// Prints what an HTTP exchange came to. `saved` is the number of body bytes
/// that reached a file, or `None` when the body was discarded.
fn report_http(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    outcome: Result<net::http::Response, net::http::Error>,
    saved: Option<u64>,
) {
    match outcome {
        Ok(response) => {
            let mut line = Line::new();
            line.push_u32(response.received as u32);
            line.push_str(" bytes in ");
            line.push_u32(response.elapsed_ms as u32);
            line.push_str(" ms, body ");
            line.push_u32(response.body_bytes as u32);
            if saved.is_none() {
                line.push_str(" (not saved)");
            }
            console.write_output_line(framebuffer, line.as_str());
            write_response_head(console, framebuffer, &response.headers);
        }
        // One arm for every failure, taken from `net::http` rather than
        // restated here: the errors grew when `Transaction` arrived, and a
        // match in the shell is a second place for that list to fall
        // behind. `httpget` shows the same sentence the browser's status
        // line does.
        Err(error) => {
            let mut line = Line::new();
            line.push_str("httpget: ");
            line.push_str(net::http::error_text(error));
            console.write_output_line(framebuffer, line.as_str());
        }
    }
}

/// Prints the response's first few lines, stopping at the blank line that
/// ends the headers. Anything past that is a body this shell has nothing
/// useful to do with.
fn write_response_head(console: &mut Console, framebuffer: &mut Framebuffer, body: &[u8]) {
    let mut start = 0usize;
    for _ in 0..HTTP_HEAD_LINES {
        let end = body[start..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map(|offset| start + offset)
            .unwrap_or(body.len());
        let text = &body[start..end];
        let text = match text.strip_suffix(b"\r") {
            Some(text) => text,
            None => text,
        };
        if text.is_empty() {
            return;
        }
        let mut line = Line::new();
        line.push_ascii(text);
        console.write_output_line(framebuffer, line.as_str());
        if end >= body.len() {
            return;
        }
        start = end + 1;
    }
}

/// `netdump` -- show the head of each 802.3 frame the C6 pushes at the host.
///
/// This is the check Stage 0 of `docs/plans/archive/TCPIP_PLAN.md` calls for, kept as a
/// command rather than thrown away: it is the one place that shows what is
/// actually on the wire when the stack above it does not answer. Destination
/// MAC, source MAC and ethertype are exactly the first fourteen bytes.
fn cmd_netdump(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    session: &mut Option<wifi::Rpc>,
) {
    let argument = trim(argument);
    let transmit_side = argument == b"tx";
    let count = match argument {
        b"" | b"tx" => DEFAULT_DUMP_FRAMES,
        text => match parse_u32(text) {
            Some(count) if (1..=32).contains(&count) => count,
            _ => {
                console.write_output_line(framebuffer, "usage: netdump [tx|1..32]");
                return;
            }
        },
    };

    let Some(rpc) = wifi_session(console, framebuffer, session) else {
        return;
    };

    // Which frames are this station's own is the question the command
    // exists to answer, and that cannot be read off a frame without
    // knowing the radio's address. A slave that refuses to say is not
    // fatal here -- the dump is still worth having, just without the
    // "(us)" mark -- so this reports and carries on.
    let station_mac = match wifi::rpc::get_mac_address(rpc, wifi::rpc::WIFI_IF_STA) {
        Some((0, mac)) => {
            let mut line = Line::new();
            line.push_str("station mac ");
            push_mac(&mut line, &mac);
            console.write_output_line(framebuffer, line.as_str());
            Some(mac)
        }
        Some((status, _)) => {
            write_slave_status(console, framebuffer, "station MAC", status);
            None
        }
        None => {
            console.write_output_line(framebuffer, "station MAC: RPC failed, see UART log");
            None
        }
    };

    if transmit_side {
        dump_transmitted(console, framebuffer, rpc, station_mac);
        return;
    }

    console.write_output_line(framebuffer, "waiting for station frames...");

    let deadline = tick::now_ms() + DUMP_TIMEOUT_MS;
    let mut shown = 0;
    let mut counts = DestinationCounts::default();
    while shown < count && tick::now_ms() < deadline {
        rpc.service();
        let Some(frame) = rpc.take_station_frame() else {
            continue;
        };
        shown += 1;

        let destination = frame.get(..6).unwrap_or(&[]);
        let kind = destination_kind(destination, station_mac);
        counts.add(kind);

        let mut line = Line::new();
        line.push_str("to ");
        push_mac(&mut line, destination);
        line.push_str(kind.label());
        line.push_str(" from ");
        push_mac(&mut line, frame.get(6..12).unwrap_or(&[]));
        console.write_output_line(framebuffer, line.as_str());

        write_ethertype(console, framebuffer, &frame, frame.len());
    }

    if shown == 0 {
        console.write_output_line(
            framebuffer,
            "no frames; the station has to be associated first",
        );
        return;
    }

    let mut line = Line::new();
    line.push_u32(shown);
    line.push_str(" frames: ");
    line.push_u32(counts.to_us);
    line.push_str(" to us, ");
    line.push_u32(counts.broadcast);
    line.push_str(" broadcast, ");
    line.push_u32(counts.multicast);
    line.push_str(" multicast, ");
    line.push_u32(counts.other);
    line.push_str(" other");
    console.write_output_line(framebuffer, line.as_str());

    if counts.to_us == 0 {
        // Expected until this station has an address: with no IP, nothing
        // on the network has a reason to address it directly. Say so, so
        // the absence does not read as a fault.
        console.write_output_line(
            framebuffer,
            "nothing addressed to us, which is normal without an address",
        );
    }
}

/// `netdump tx` -- the heads of the frames this host most recently handed
/// to the co-processor.
///
/// A frame the C6 never put on the air is indistinguishable, from the far
/// end, from one this firmware never built. This is the only place that
/// tells the two apart. The source address is checked against the radio's
/// own: a frame sent with somebody else's source MAC is accepted by the
/// SDIO link and then dropped by the Wi-Fi driver or the access point,
/// which looks exactly like silence.
fn dump_transmitted(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    rpc: &wifi::Rpc,
    station_mac: Option<[u8; 6]>,
) {
    let mut shown = 0u32;
    let mut wrong_source = 0u32;
    // Collected first: the iterator borrows `rpc`, and writing output does
    // not, but keeping both alive across the loop reads worse than this.
    let frames: Vec<wifi::rpc::TransmittedFrame> = rpc.transmitted_frames().copied().collect();

    for frame in &frames {
        shown += 1;
        let source = &frame.head[6..12];
        let source_is_ours = station_mac.is_some_and(|station| source == station);
        if !source_is_ours {
            wrong_source += 1;
        }

        let mut line = Line::new();
        line.push_str("to ");
        push_mac(&mut line, &frame.head[..6]);
        line.push_str(" from ");
        push_mac(&mut line, source);
        line.push_str(if source_is_ours { "" } else { " (NOT US)" });
        console.write_output_line(framebuffer, line.as_str());

        write_ethertype(console, framebuffer, &frame.head, frame.length);
    }

    if shown == 0 {
        console.write_output_line(
            framebuffer,
            "nothing sent yet; configure an address, then make the peer talk",
        );
        return;
    }

    let mut line = Line::new();
    line.push_str("last ");
    line.push_u32(shown);
    line.push_str(" sent, newest last");
    console.write_output_line(framebuffer, line.as_str());

    if wrong_source != 0 {
        console.write_output_line(
            framebuffer,
            "source MAC is not this station's; the AP will drop these",
        );
    }
}

/// Writes the ethertype and length line shared by both dump directions.
fn write_ethertype(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    frame: &[u8],
    length: usize,
) {
    let ethertype = match frame.get(12..14) {
        Some(bytes) => u16::from_be_bytes([bytes[0], bytes[1]]),
        None => 0,
    };
    let mut line = Line::new();
    line.push_str("  ethertype 0x");
    line.push_hex(ethertype as u32, 4);
    line.push_str(match ethertype {
        0x0800 => " (IPv4)",
        0x0806 => " (ARP)",
        0x86DD => " (IPv6)",
        _ => "",
    });
    line.push_str(", ");
    line.push_u32(length as u32);
    line.push_str(" bytes");
    console.write_output_line(framebuffer, line.as_str());
}

/// Where a received frame was addressed. Which of these turn up is the
/// point of the command: a link showing only broadcast and multicast is
/// working, it just has nobody talking to it yet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Destination {
    Us,
    Broadcast,
    Multicast,
    /// A unicast frame for somebody else, or a frame too short to tell.
    /// Neither should happen on a station interface.
    Other,
}

impl Destination {
    fn label(self) -> &'static str {
        match self {
            Destination::Us => " (us)",
            Destination::Broadcast => " (broadcast)",
            Destination::Multicast => " (multicast)",
            Destination::Other => " (not us)",
        }
    }
}

#[derive(Default)]
struct DestinationCounts {
    to_us: u32,
    broadcast: u32,
    multicast: u32,
    other: u32,
}

impl DestinationCounts {
    fn add(&mut self, destination: Destination) {
        let slot = match destination {
            Destination::Us => &mut self.to_us,
            Destination::Broadcast => &mut self.broadcast,
            Destination::Multicast => &mut self.multicast,
            Destination::Other => &mut self.other,
        };
        *slot += 1;
    }
}

fn destination_kind(destination: &[u8], station: Option<[u8; 6]>) -> Destination {
    if destination.len() != 6 {
        return Destination::Other;
    }
    if destination == [0xFF; 6] {
        return Destination::Broadcast;
    }
    if station.is_some_and(|station| destination == station) {
        return Destination::Us;
    }
    // Bit 0 of the first octet is the group bit.
    if destination[0] & 1 != 0 {
        return Destination::Multicast;
    }
    Destination::Other
}

/// How long `ipconfig dhcp` waits for a lease. Discover/offer/request/ack
/// is four packets, but a server that is asleep or a station that is not
/// associated yet costs the whole window.
const DHCP_TIMEOUT_MS: u64 = 15_000;
const DEFAULT_PING_COUNT: u32 = 4;
const DEFAULT_DUMP_FRAMES: u32 = 8;
const DUMP_TIMEOUT_MS: u64 = 10_000;
/// One progress line per this many bytes received over TFTP.
const PROGRESS_STEP_BYTES: usize = 64 * 1024;
/// How many response lines `httpget` prints before stopping.
const HTTP_HEAD_LINES: u32 = 8;

fn push_mac(line: &mut Line, mac: &[u8]) {
    for (index, byte) in mac.iter().enumerate() {
        if index != 0 {
            line.push_str(":");
        }
        line.push_hex(*byte as u32, 2);
    }
}

fn push_ipv4(line: &mut Line, address: Ipv4Address) {
    for (index, octet) in address.octets().iter().enumerate() {
        if index != 0 {
            line.push_str(".");
        }
        line.push_u32(*octet as u32);
    }
}

/// `a.b.c.d`, decimal, no leading-zero or shorthand forms.
fn parse_ipv4(bytes: &[u8]) -> Option<Ipv4Address> {
    let mut octets = [0u8; 4];
    let mut index = 0;
    for part in bytes.split(|&byte| byte == b'.') {
        if index == 4 {
            return None;
        }
        let value = parse_u32(part)?;
        if value > 255 {
            return None;
        }
        octets[index] = value as u8;
        index += 1;
    }
    if index != 4 {
        return None;
    }
    Some(Ipv4Address::from(octets))
}

/// `a.b.c.d/len`; a bare address is taken as /24, which is what a home
/// network almost always is and saves typing it every time.
fn parse_ipv4_cidr(bytes: &[u8]) -> Option<(Ipv4Address, u8)> {
    match bytes.iter().position(|&byte| byte == b'/') {
        None => Some((parse_ipv4(bytes)?, 24)),
        Some(slash) => {
            let prefix = parse_u32(&bytes[slash + 1..])?;
            if prefix > 32 {
                return None;
            }
            Some((parse_ipv4(&bytes[..slash])?, prefix as u8))
        }
    }
}

/// `<host>` or `<host>:port`, where the host may be a name.
///
/// Only the port is interpreted. The host is handed back as it was typed,
/// because it is not this function's business whether it is an address --
/// and because the text is what the `Host:` header wants either way.
fn split_host_port(bytes: &[u8]) -> Option<(&[u8], u16)> {
    split_host_port_with_default(bytes, 80)
}

/// The same split, for a scheme whose default port is not 80.
fn split_host_port_with_default(bytes: &[u8], default: u16) -> Option<(&[u8], u16)> {
    let (host, port) = match bytes.iter().position(|&byte| byte == b':') {
        None => (bytes, default),
        Some(colon) => {
            let port = parse_u32(&bytes[colon + 1..])?;
            if port == 0 || port > 65535 {
                return None;
            }
            (&bytes[..colon], port as u16)
        }
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port))
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

    if !sdmmc::write_blocks(&card, lba, &pattern) {
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

    let restore = original;
    let restored = sdmmc::write_blocks(&card, lba, &restore);
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

    let zero = [0u8; 512];
    if !sdmmc::write_blocks(&card, lba, &zero) {
        console.write_output_line(framebuffer, "zero write failed, see UART log");
        return;
    }
    console.write_output_line(framebuffer, "block zeroed");
}

/// `write <path> <text>` / `append <path> <text>`.
///
/// The text is the rest of the line, unsplit and unquoted: everything after
/// the path is content, so a quote in it is a quote rather than a delimiter.
/// Only the path has to be told apart from what follows it.
fn cmd_write(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &State,
    mode: fs::vfs::OpenMode,
) {
    let Some((path_text, rest)) = split_argument(argument) else {
        console.write_output_line(framebuffer, "unterminated quote");
        return;
    };
    if path_text.is_empty() {
        console.write_output_line(framebuffer, "usage: write <path> <text>");
        return;
    }
    let Some(path) = absolute(console, framebuffer, state, path_text) else {
        return;
    };
    let text = as_str(rest);
    with_devices(usb_host, ram_disk, |devices| {
        files::write(
            console,
            framebuffer,
            devices,
            vfs,
            path.as_str(),
            text,
            mode,
        )
    });
}

/// `ls [-l] [-a] [<path>]`.
///
/// The first command here to take options, so this is also where the shell's
/// rule for them is set: flags come before the path, each is a `-` and one
/// or more letters, and `-la` means the same as `-l -a`. Nothing after the
/// first non-flag word is read as a flag, which is what lets a file whose
/// name starts with `-` be listed by quoting it.
fn cmd_ls(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &State,
) {
    const USAGE: &str = "usage: ls [-l] [-a] [<path>]";
    let mut options = files::ListOptions::default();
    let mut rest = trim(argument);
    loop {
        let (word, tail) = split_first_word(rest);
        // A bare `-` is a path, not an empty set of flags.
        if word.len() < 2 || word[0] != b'-' {
            break;
        }
        for &flag in &word[1..] {
            match flag {
                b'l' => options.long = true,
                b'a' => options.all = true,
                _ => {
                    // Naming the flag rather than only the usage line: the
                    // likely cause is a flag another `ls` has and this one
                    // does not, and the usage line alone leaves the reader
                    // to spot which letter it refused.
                    let mut line = Line::new();
                    line.push_str("ls: unknown option -");
                    line.push_ascii(&[flag]);
                    console.write_output_line(framebuffer, line.as_str());
                    console.write_output_line(framebuffer, USAGE);
                    return;
                }
            }
        }
        rest = trim(tail);
    }

    // No path lists the current directory, which is the root until `cd`
    // moves it.
    let path = if rest.is_empty() {
        Some(state.cwd)
    } else {
        single_argument(console, framebuffer, rest, USAGE)
            .and_then(|path| absolute(console, framebuffer, state, path))
    };
    let Some(path) = path else {
        return;
    };
    with_devices(usb_host, ram_disk, |devices| {
        files::list(console, framebuffer, devices, vfs, path.as_str(), options)
    });
}

/// `cd [<path>]`.
///
/// No argument goes to the root rather than to a home directory: there is
/// no such thing here, and the root is the one place that is always there
/// whatever is mounted.
fn cmd_cd(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &mut State,
) {
    if argument.is_empty() {
        state.cwd = path::root();
        return;
    }
    let Some(target) = single_argument(console, framebuffer, argument, "usage: cd [<path>]")
        .and_then(|target| absolute(console, framebuffer, state, target))
    else {
        return;
    };
    with_devices(usb_host, ram_disk, |devices| {
        files::change_directory(console, framebuffer, devices, vfs, &mut state.cwd, target)
    });
}

/// `cat <path> [offset]`.
///
/// The only command here that takes a second argument, and the reason the
/// shell needed quoting at all: with the path settled by the quote rather
/// than by where the spaces fall, the offset can go back to being an
/// ordinary trailing word.
/// `rm <path>` and `rmdir <path>`.
fn cmd_remove(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &State,
    directory: bool,
) {
    let usage = if directory {
        "usage: rmdir <path>"
    } else {
        "usage: rm <path>"
    };
    let Some(path) = single_argument(console, framebuffer, argument, usage)
        .and_then(|path| absolute(console, framebuffer, state, path))
    else {
        return;
    };
    with_devices(usb_host, ram_disk, |devices| {
        files::remove(console, framebuffer, devices, vfs, path.as_str(), directory)
    });
}

/// `mv <from> <to>`.
fn cmd_move(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &State,
) {
    const USAGE: &str = "usage: mv <from> <to>";
    // Two quoted-or-bare arguments, so a name with a space in it can be
    // either half.
    let Some((from_text, rest)) = split_argument(argument) else {
        console.write_output_line(framebuffer, "unterminated quote");
        return;
    };
    let Some(to_text) = single_argument(console, framebuffer, rest, USAGE) else {
        return;
    };
    if from_text.is_empty() {
        console.write_output_line(framebuffer, USAGE);
        return;
    }
    let (Some(from), Some(to)) = (
        absolute(console, framebuffer, state, from_text),
        absolute(console, framebuffer, state, to_text),
    ) else {
        return;
    };
    with_devices(usb_host, ram_disk, |devices| {
        files::rename(
            console,
            framebuffer,
            devices,
            vfs,
            from.as_str(),
            to.as_str(),
        )
    });
}

/// `fill <path> <KiB> [chunk] [repeat]`.
///
/// The measurement `docs/plans/archive/FILESYSTEM_WORKFLOW_PLAN.md` Stage 3-2 asks for:
/// the streaming write path against the repeated-open one it replaces, on
/// the same volume with the same arguments.
/// `fswritetest <dir> [rounds] [KiB]`.
fn cmd_fswritetest(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &State,
) {
    const USAGE: &str = "usage: fswritetest <dir> [rounds] [KiB]";
    let Some((path_text, rest)) = split_argument(argument) else {
        console.write_output_line(framebuffer, "unterminated quote");
        return;
    };
    if path_text.is_empty() {
        console.write_output_line(framebuffer, USAGE);
        return;
    }
    let (rounds_text, rest) = split_first_word(trim(rest));
    let rounds = if rounds_text.is_empty() {
        fswritetest::DEFAULT_ROUNDS
    } else {
        match parse_u32(rounds_text) {
            Some(rounds) => rounds,
            None => {
                console.write_output_line(framebuffer, USAGE);
                return;
            }
        }
    };
    let (kib_text, rest) = split_first_word(trim(rest));
    let kib = if kib_text.is_empty() {
        fswritetest::DEFAULT_CHURN_KIB
    } else {
        match parse_u32(kib_text) {
            Some(kib) => kib,
            None => {
                console.write_output_line(framebuffer, USAGE);
                return;
            }
        }
    };
    if !trim(rest).is_empty() {
        console.write_output_line(framebuffer, USAGE);
        return;
    }
    let Some(path) = absolute(console, framebuffer, state, path_text) else {
        return;
    };
    with_devices(usb_host, ram_disk, |devices| {
        fswritetest::run(console, framebuffer, devices, vfs, &path, rounds, kib)
    });
}

fn cmd_fill(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &State,
) {
    const USAGE: &str = "usage: fill <path> <KiB> [chunk] [repeat]";
    let Some((path_text, rest)) = split_argument(argument) else {
        console.write_output_line(framebuffer, "unterminated quote");
        return;
    };
    if path_text.is_empty() {
        console.write_output_line(framebuffer, USAGE);
        return;
    }
    let (kib_text, rest) = split_first_word(trim(rest));
    let Some(kib) = parse_u32(kib_text) else {
        console.write_output_line(framebuffer, USAGE);
        return;
    };
    let (chunk_text, rest) = split_first_word(trim(rest));
    let chunk = if chunk_text.is_empty() {
        512
    } else {
        match parse_u32(chunk_text) {
            Some(chunk) => chunk,
            None => {
                console.write_output_line(framebuffer, USAGE);
                return;
            }
        }
    };
    let rest = trim(rest);
    let repeated = match rest {
        b"" => false,
        b"repeat" => true,
        _ => {
            console.write_output_line(framebuffer, USAGE);
            return;
        }
    };
    let Some(path) = absolute(console, framebuffer, state, path_text) else {
        return;
    };
    with_devices(usb_host, ram_disk, |devices| {
        files::fill(
            console,
            framebuffer,
            devices,
            vfs,
            path.as_str(),
            kib as usize,
            chunk as usize,
            repeated,
        )
    });
}

/// `fsopen [<path>]`: hold a file open across commands, or list what is held.
///
/// Every other command closes what it opens before it returns, which left no
/// way from the console to have a handle open at the moment a drive was
/// pulled. That is the state the automatic unmount is defined against -- the
/// mount goes and the handles on it start failing -- so without this the
/// behaviour could only be reasoned about, not watched.
fn cmd_fsopen(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &mut State,
) {
    if argument.is_empty() {
        files::show_open_files(console, framebuffer, vfs, &state.held);
        return;
    }
    let Some(argument) = single_argument(console, framebuffer, argument, "usage: fsopen [<path>]")
    else {
        return;
    };
    let Some(path) = absolute(console, framebuffer, state, argument) else {
        return;
    };
    with_devices(usb_host, ram_disk, |devices| {
        files::open_file(
            console,
            framebuffer,
            devices,
            vfs,
            &mut state.held,
            path.as_str(),
        )
    });
}

/// `fsread <slot> [bytes]`.
fn cmd_fsread(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &mut State,
) {
    const USAGE: &str = "usage: fsread <slot> [bytes]";
    let (slot_text, rest) = split_first_word(trim(argument));
    let Some(slot) = parse_u32(slot_text) else {
        console.write_output_line(framebuffer, USAGE);
        return;
    };
    let rest = trim(rest);
    let count = if rest.is_empty() {
        // One console line's worth: enough to move the offset visibly
        // without filling the screen when the read succeeds.
        64
    } else {
        match parse_u32(rest) {
            Some(count) => count,
            None => {
                console.write_output_line(framebuffer, USAGE);
                return;
            }
        }
    };
    with_devices(usb_host, ram_disk, |devices| {
        files::read_open_file(
            console,
            framebuffer,
            devices,
            vfs,
            &mut state.held,
            slot as usize,
            count as usize,
        )
    });
}

/// `fsclose <slot>`.
fn cmd_fsclose(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    vfs: &mut Vfs,
    state: &mut State,
) {
    let Some(slot) = single_argument(console, framebuffer, argument, "usage: fsclose <slot>")
        .and_then(parse_u32)
    else {
        return;
    };
    files::close_open_file(console, framebuffer, vfs, &mut state.held, slot as usize);
}

fn cmd_cat(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
    state: &State,
) {
    const USAGE: &str = "usage: cat <path> [offset]";
    let Some((path_text, rest)) = split_argument(argument) else {
        console.write_output_line(framebuffer, "unterminated quote");
        return;
    };
    if path_text.is_empty() {
        console.write_output_line(framebuffer, USAGE);
        return;
    }
    let offset = if rest.is_empty() {
        0
    } else {
        // A trailing word that is not a number is almost always the tail of
        // a path whose quotes were left off, so the message names that
        // rather than reprinting the usage line. `single_argument`'s wording
        // does not fit here: a second argument is expected, it just has to
        // be a number.
        match parse_u32(rest) {
            Some(offset) => offset,
            None => {
                console.write_output_line(
                    framebuffer,
                    "offset must be a number; quote paths containing spaces",
                );
                return;
            }
        }
    };

    let Some(path) = absolute(console, framebuffer, state, path_text) else {
        return;
    };
    with_devices(usb_host, ram_disk, |devices| {
        files::concatenate(
            console,
            framebuffer,
            devices,
            vfs,
            path.as_str(),
            offset as u64,
        )
    });
}

/// `mount [-r] [<volume>]`.
///
/// Three shapes, and the one flag there is. `-r` forces a FAT volume
/// read-only; there is no flag the other way, because forcing exFAT
/// read-write is not something the VFS can honour and offering it would be
/// offering something that is refused at the point of use.
fn cmd_mount(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    vfs: &mut Vfs,
) {
    const USAGE: &str = "usage: mount [-r] [<ram|sd0pN|usbMpN>]";
    if argument.is_empty() {
        files::show_mounts(console, framebuffer, vfs);
        return;
    }
    let Some((first, rest)) = split_argument(argument) else {
        console.write_output_line(framebuffer, "unterminated quote");
        return;
    };
    let (request, name) = if first == b"-r" {
        (MountRequest::ReadOnly, rest)
    } else {
        (MountRequest::Default, argument)
    };
    // Re-parsed through `single_argument` so that the volume name is subject
    // to the same "nothing left over" rule as every other command's, whether
    // or not the flag was there.
    let Some(name) = single_argument(console, framebuffer, name, USAGE) else {
        return;
    };
    let name = as_str(name);
    with_devices(usb_host, ram_disk, |devices| {
        files::mount(console, framebuffer, devices, vfs, name, request)
    });
}

/// `automount [on|off]`.
///
/// Reporting the state with no argument matters more here than for most
/// settings: automount changes the tree without being asked, so "is this on"
/// has to be answerable without plugging something in to find out.
fn cmd_automount(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    auto_mount: &mut AutoMount,
) {
    match argument {
        b"" => {}
        b"on" => auto_mount.set_enabled(true),
        b"off" => auto_mount.set_enabled(false),
        _ => {
            console.write_output_line(framebuffer, "usage: automount [on|off]");
            return;
        }
    }
    console.write_output_line(
        framebuffer,
        if auto_mount.enabled() {
            "automount on: USB volumes mount and unmount with their media"
        } else {
            "automount off: mount and umount only"
        },
    );
}

/// Builds the `fs::Devices` view for one command and runs `body` on it.
///
/// The SD slot starts unopened and activates only if the command actually
/// resolves `sd0`, so `blkread ram 0` does not put an SD identification
/// sequence in the log. Activation does not outlive the command: the slot has
/// no detect line, so whether a card is there -- and whether it is the same
/// one -- is only answerable by asking again.
///
/// The USB session comes from the host registry, and the RAM disk from the
/// frame loop that owns it. Neither is created here.
fn with_devices<T>(
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
    body: impl FnOnce(&mut Devices) -> T,
) -> T {
    let mut sd = SdSlot::new();
    let mut devices = Devices {
        ram: ram_disk,
        sd: &mut sd,
        usb: usb_host,
    };
    body(&mut devices)
}

/// `docs/plans/archive/FILESYSTEM_PLAN.md` Stage 1: what the block layer sees, before
/// there is a VFS to mount any of it.
fn cmd_devices(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
) {
    with_devices(usb_host, ram_disk, |devices| {
        blockdev::show_devices(console, framebuffer, devices)
    });
}

fn cmd_blkread(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
    ram_disk: Option<&mut RamBlockDevice>,
) {
    let (device_text, rest) = split_first_word(argument);
    let (second, rest) = split_first_word(trim(rest));
    // The partition is optional and sits between the two required words, so
    // the second word is whichever of the two it turned out to be.
    let (partition_number, lba_text) = match second.first() {
        Some(b'p') => (parse_u32(&second[1..]), trim(rest)),
        _ => (None, second),
    };
    if device_text.is_empty() || lba_text.is_empty() {
        console.write_output_line(framebuffer, "usage: blkread <device> [pN] <lba>");
        return;
    }
    if matches!(second.first(), Some(b'p')) && partition_number.is_none() {
        console.write_output_line(framebuffer, "partition must be p1..p4");
        return;
    }
    let Some(lba) = parse_u32(lba_text) else {
        console.write_output_line(framebuffer, "usage: blkread <device> [pN] <lba>");
        return;
    };
    let device_name = as_str(device_text);

    with_devices(usb_host, ram_disk, |devices| {
        blockdev::read_block(
            console,
            framebuffer,
            devices,
            device_name,
            partition_number.map(|number| number as u8),
            lba as u64,
        )
    });
}

/// Reads LBA 0 from the SD card and hands it to `mbr::show` -- the
/// device-specific half of the SD/USB split described in
/// `docs/plans/archive/USB_MSC_PLAN.md` Stage 6; the actual MBR parsing lives in `mbr.rs` and
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
/// `usb::enumerate_device` on its own -- is `docs/plans/archive/USB_REFACTOR_PLAN.md` Stage
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
        if usb::fs_ls_only_host_forced() {
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
/// The device-facing view of USB-A, as opposed to the `usb*` commands
/// around it: `lsusb` alone shows the bus as a tree and one device's
/// descriptors, and never touches the port, the hub's control endpoint or a
/// class driver's session. The formatting lives in `app::lsusb`, the same
/// way `sdmbr`/`usbmbr` hand their output to `app::mbr`.
fn cmd_lsusb(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &usb::UsbHost,
) {
    if argument.is_empty() {
        lsusb::show_tree(console, framebuffer, usb_host);
        return;
    }
    // Addresses are handed out as 1 (whatever is plugged into USB-A) and
    // hub port + 1, so the 7-bit USB address space is never approached
    // here; anything outside it cannot name a device this registry holds.
    let Some(address) = parse_u32(argument).filter(|address| *address <= 127) else {
        console.write_output_line(framebuffer, "usage: lsusb [address] (as shown in brackets)");
        return;
    };
    lsusb::show_device(console, framebuffer, usb_host, address as u8);
}

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
    usb_host.rescan(usb::RescanReason::Manual);
    report_usb_state(console, framebuffer, usb_host);
}

/// Changes the host-speed policy and immediately rebuilds the entire USB
/// registry. A High-Speed hub attached while this is on enumerates at
/// Full-Speed and behaves as a plain repeater, which makes it useful for
/// testing non-Split periodic HID channels without special FS-only hardware.
fn cmd_usbfs(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
) {
    let forced = match argument {
        b"on" => true,
        b"off" => false,
        _ => {
            console.write_output_line(framebuffer, "usage: usbfs on|off");
            return;
        }
    };
    usb::set_fs_ls_only_host_forced(forced);
    console.write_output_line(
        framebuffer,
        if forced {
            "USB host FS/LS-only forced; resetting and rescanning..."
        } else {
            "USB host High-Speed restored; resetting and rescanning..."
        },
    );
    usb_host.rescan(usb::RescanReason::Manual);
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
    line.push_str("  config wTotalLength: ");
    line.push_u32(summary.config_total_length as u32);
    line
}

fn device_kind_text(kind: &usb::DeviceKind) -> Line {
    let mut line = Line::new();
    line.push_str("  driver: ");
    match kind {
        usb::DeviceKind::Keyboard(_) => line.push_str("HID Boot keyboard"),
        usb::DeviceKind::Mouse(_) => line.push_str("HID Boot mouse"),
        usb::DeviceKind::MassStorage(storage) => {
            line.push_str("Mass Storage (Bulk-Only Transport)");
            if storage.needs_reinit() {
                line.push_str(" -- session unusable; run usbrescan");
            }
        }
    }
    line
}

/// `docs/plans/archive/USB_MSC_PLAN.md` Stage 1-4, extended by `docs/plans/archive/USB_REFACTOR_PLAN.md` Stage F:
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
    if !require_live_usb_msc(console, framebuffer, mass_storage) {
        return;
    }

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

/// `docs/plans/archive/USB_MSC_PLAN.md` Stage 5, extended by `docs/plans/archive/USB_REFACTOR_PLAN.md` Stage F:
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
    if !require_live_usb_msc(console, framebuffer, mass_storage) {
        return;
    }

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

/// Writes one block to USB Mass Storage, verifies it against its neighbours,
/// and puts the original contents back -- the USB counterpart of
/// `cmd_sdwritetest`, with two additions the SD test does not need.
///
/// **It reads a window around the target block, not just the block itself.**
/// A test that writes LBA N and reads LBA N back cannot fail when the device
/// actually wrote somewhere else: the same wrong address is used for both
/// halves, so the comparison matches while data elsewhere is destroyed. The
/// window makes that visible, and because the window was snapshotted first,
/// a neighbour that did change can be put back.
///
/// **It flushes the device cache after every write.** A WRITE(10) that
/// succeeds has reached the device, not the medium, and the read-back may be
/// answered from the same cache -- so without SYNCHRONIZE CACHE(10) a test
/// can report "restored" for data that never left the cache.
/// What one `usbwritetest` round established, for a caller that runs
/// several and reports the tally rather than the rounds.
#[derive(Clone, Copy, Default)]
struct WriteRoundOutcome {
    /// The pattern write was actually attempted. False means the round
    /// stopped at a precondition and says nothing about the write path.
    attempted: bool,
    /// The pattern was written and read back identical.
    pattern_ok: bool,
    /// Every block in the window holds its original contents again.
    restored: bool,
    /// Blocks nobody named that changed anyway.
    collateral: u32,
    /// The BOT session needs re-enumeration before another command.
    session_lost: bool,
}

fn cmd_usb_write_test(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
) {
    let Some(lba) = parse_u32(trim(argument)) else {
        console.write_output_line(framebuffer, "usage: usbwritetest <lba>");
        return;
    };
    let _ = run_usb_write_test(console, framebuffer, lba, usb_host);
}

/// One write/read-back/restore round against `lba`.
///
/// Split from the command so `usbcheck` can run several and report the
/// tally. Every diagnostic line it used to print, it still prints: a run
/// that fails is read line by line, and a summary is only useful when the
/// detail behind it is still there.
fn run_usb_write_test(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    lba: u32,
    usb_host: &mut usb::UsbHost,
) -> WriteRoundOutcome {
    const BLOCK: usize = 512;
    /// Blocks either side of the target that are checked for collateral
    /// damage. One before and two after covers an off-by-one in any
    /// direction and a write that ran long.
    const WINDOW: usize = 4;

    let aborted = WriteRoundOutcome::default();

    console.write_output_line(
        framebuffer,
        "WARNING: temporarily overwrites 1 block, then restores it",
    );
    let Some(mass_storage) = usb_host.mass_storage_mut() else {
        console.write_output_line(
            framebuffer,
            "no Mass Storage device attached; plug one in and run 'usbrescan'",
        );
        return aborted;
    };
    if !require_live_usb_msc(console, framebuffer, mass_storage) {
        return aborted;
    }

    console.write_output_line(framebuffer, "waiting for media ready (TEST UNIT READY)...");
    if !mass_storage.wait_until_ready(10) {
        console.write_output_line(framebuffer, "media not ready; aborting (nothing written)");
        return aborted;
    }

    // Every block index below assumes 512-byte logical blocks. A device with
    // a different block length would be handed a data phase that does not
    // match what its CDB asked for, which is exactly how a write ends up
    // affecting blocks nobody named.
    let Some(capacity) = mass_storage.read_capacity() else {
        console.write_output_line(framebuffer, "READ CAPACITY(10) failed; aborting");
        return aborted;
    };
    if capacity.block_length != BLOCK as u32 {
        let mut line = Line::new();
        line.push_str("device block length is ");
        line.push_u32(capacity.block_length);
        line.push_str(" bytes, not 512; aborting");
        console.write_output_line(framebuffer, line.as_str());
        return aborted;
    }
    if lba > capacity.last_lba {
        let mut line = Line::new();
        line.push_str("LBA beyond last block (");
        line.push_u32(capacity.last_lba);
        line.push_str("); aborting");
        console.write_output_line(framebuffer, line.as_str());
        return aborted;
    }

    // The window starts one block before the target where there is room, so
    // `target_index` is 1 everywhere except at LBA 0.
    let first = lba.saturating_sub(1);
    let last = lba.saturating_add(2).min(capacity.last_lba);
    let count = (last - first + 1) as usize;
    let target_index = (lba - first) as usize;

    let mut snapshot = [[0u8; BLOCK]; WINDOW];
    for (index, block) in snapshot.iter_mut().take(count).enumerate() {
        if !mass_storage.read_blocks(first + index as u32, block) {
            console.write_output_line(
                framebuffer,
                "could not read the block window, aborting (nothing written)",
            );
            return aborted;
        }
    }

    let mut pattern = [0u8; BLOCK];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = (index as u8) ^ 0xA5;
    }

    let outcome = mass_storage.write_blocks(lba, &mut pattern);
    if outcome != usb::WriteOutcome::Written {
        console.write_output_line(
            framebuffer,
            if outcome == usb::WriteOutcome::WriteProtected {
                "medium is write protected; nothing was written"
            } else {
                "pattern write failed, see UART log"
            },
        );
        if mass_storage.needs_reinit() {
            // Every remaining step would fail the same way and take seconds
            // each to do it. Keep the failure local to MSC: resetting the
            // whole bus here would also disconnect healthy HID devices.
            console.write_output_line(
                framebuffer,
                "MSC session unusable; run 'usbrescan' before another storage command",
            );
            return WriteRoundOutcome {
                attempted: true,
                session_lost: true,
                ..aborted
            };
        }
        write_usb_sense_line(console, framebuffer, mass_storage);
        return WriteRoundOutcome {
            attempted: true,
            ..aborted
        };
    }
    let flushed = mass_storage.synchronize_cache();
    match flushed {
        usb::CacheSync::Flushed => {}
        // Not a fault: the device simply has no such command. The
        // verification reads below use Force Unit Access instead, which is
        // the other way to find out what the medium holds.
        usb::CacheSync::Unsupported => console.write_output_line(
            framebuffer,
            "device has no SYNCHRONIZE CACHE(10); verifying with FUA reads",
        ),
        usb::CacheSync::Failed => {
            console.write_output_line(framebuffer, "SYNCHRONIZE CACHE(10) failed, see UART log")
        }
    }
    // A device can still be programming its medium after answering. The SD
    // side has the same trap; give it the chance to say so before the next
    // command rather than letting a busy device fail one.
    let _ = mass_storage.wait_until_ready(10);

    // Compare the whole window, not just the block that was named.
    let mut damaged = 0u32;
    let mut target_ok = false;
    let mut window_readable = true;
    let mut fua_rejected = false;
    for (index, original) in snapshot.iter().take(count).enumerate() {
        let mut current = [0u8; BLOCK];
        // Force Unit Access first: it is the only read that reports the
        // medium rather than the cache. A device that rejects it is not
        // broken, but the comparison it feeds is then weaker, so say so
        // once instead of silently downgrading.
        if !mass_storage.read_blocks_from_medium(first + index as u32, &mut current) {
            if !mass_storage.read_blocks(first + index as u32, &mut current) {
                // Both reads failed, so this says nothing about FUA -- the
                // device is not answering at all. Claiming a rejected FUA
                // here sent the reader looking at the wrong thing.
                window_readable = false;
                continue;
            }
            fua_rejected = true;
        }
        if index == target_index {
            target_ok = current == pattern;
        } else if current != *original {
            damaged += 1;
            let mut line = Line::new();
            line.push_str("COLLATERAL DAMAGE: LBA ");
            line.push_u32(first + index as u32);
            line.push_str(" changed too");
            console.write_output_line(framebuffer, line.as_str());
        }
    }

    if fua_rejected {
        console.write_output_line(
            framebuffer,
            "FUA read rejected; read-back may come from the device cache",
        );
    }

    let mut line = Line::new();
    line.push_str("pattern write+read-back: ");
    line.push_str(if target_ok { "match" } else { "MISMATCH" });
    line.push_str(if window_readable {
        ""
    } else {
        " (part of the window could not be re-read)"
    });
    console.write_output_line(framebuffer, line.as_str());

    // Restore every block that changed, target first. The snapshot is the
    // only copy of the neighbours' contents, so this is the one chance to
    // put them back.
    let mut restore_ok = true;
    let mut restore_flush_failed = false;
    for (index, original) in snapshot.iter().take(count).enumerate() {
        if mass_storage.needs_reinit() {
            restore_ok = false;
            break;
        }
        let block_lba = first + index as u32;
        let mut current = [0u8; BLOCK];
        let needs_restore = index == target_index
            || !mass_storage.read_blocks(block_lba, &mut current)
            || current != *original;
        if !needs_restore {
            continue;
        }
        let mut restore = *original;
        if mass_storage.write_blocks(block_lba, &mut restore) != usb::WriteOutcome::Written {
            restore_ok = false;
            continue;
        }
        // A refused or failed flush is not evidence that the write failed:
        // whether the data is on the medium is what the Force Unit Access
        // read below answers. Reporting a flush the device would not
        // perform as "data not restored" sends the user looking for
        // corruption that is not there.
        if mass_storage.synchronize_cache() == usb::CacheSync::Failed {
            restore_flush_failed = true;
        }
        let _ = mass_storage.wait_until_ready(10);
        let mut check = [0u8; BLOCK];
        let checked = mass_storage.read_blocks_from_medium(block_lba, &mut check)
            || mass_storage.read_blocks(block_lba, &mut check);
        if !checked || check != *original {
            restore_ok = false;
        }
    }

    console.write_output_line(
        framebuffer,
        if restore_ok {
            "original data restored: yes"
        } else {
            "original data restored: NO -- see UART log, LBA may be corrupted"
        },
    );
    if restore_flush_failed {
        console.write_output_line(
            framebuffer,
            "note: the restore could not be flushed; verified by FUA read instead",
        );
    }
    if mass_storage.needs_reinit() {
        console.write_output_line(
            framebuffer,
            "MSC session unusable; run 'usbrescan', then usbzero this LBA",
        );
    } else if !restore_ok {
        write_usb_sense_line(console, framebuffer, mass_storage);
    }

    let mut line = Line::new();
    line.push_str("window LBA ");
    line.push_u32(first);
    line.push_str("-");
    line.push_u32(last);
    line.push_str(": collateral changes=");
    line.push_u32(damaged);
    line.push_str(match flushed {
        usb::CacheSync::Flushed => " flush=ok",
        usb::CacheSync::Unsupported => " flush=unsupported",
        usb::CacheSync::Failed => " flush=FAILED",
    });
    console.write_output_line(framebuffer, line.as_str());
    if damaged != 0 {
        console.write_output_line(
            framebuffer,
            "a block nobody named changed: the write did not land where asked",
        );
    }

    WriteRoundOutcome {
        attempted: true,
        pattern_ok: target_ok && window_readable,
        restored: restore_ok,
        collateral: damaged,
        session_lost: mass_storage.needs_reinit(),
    }
}

/// Everything one acceptance run compares, gathered before and after so the
/// run reports its own deltas.
///
/// Reading a run used to mean capturing two ten-line `usbhw` blocks and
/// diffing them by eye, once per topology and per medium. That is the
/// expensive part of the matrix in `docs/plans/archive/USB_BOT_HCD_REFACTOR_PLAN.md`, and
/// it is the part a person gets wrong.
#[derive(Clone, Copy, Default)]
struct UsbCheckCounters {
    host: usb::HostObservation,
    transport: usb::TransportObservation,
    command_retries: u32,
}

fn usb_check_counters(usb_host: &usb::UsbHost) -> UsbCheckCounters {
    let host = usb::host_observation();
    let Some(storage) = usb_host.mass_storage() else {
        return UsbCheckCounters {
            host,
            ..Default::default()
        };
    };
    UsbCheckCounters {
        host,
        transport: storage.transport_observation(),
        command_retries: storage.read_retry_count(),
    }
}

/// The change in the counters of several failure kinds, added together.
fn usb_check_kind_sum(
    after: &UsbCheckCounters,
    before: &UsbCheckCounters,
    kinds: &[usb::PacketFailureKind],
) -> u32 {
    kinds
        .iter()
        .map(|kind| {
            let index = usb::packet_failure_kind_index(*kind);
            after.host.packet_failures_by_kind[index]
                .wrapping_sub(before.host.packet_failures_by_kind[index])
        })
        .sum()
}

/// One `label+N` field of a delta line.
fn push_delta(line: &mut Line, label: &str, after: u32, before: u32) {
    line.push_str(label);
    line.push_str("+");
    line.push_u32(after.wrapping_sub(before));
}

/// Prints one gate as `PASS`/`FAIL` with the reason attached.
///
/// A run that only prints numbers still has to be interpreted against the
/// plan's Go conditions every time. Naming each gate and its verdict is what
/// makes an acceptance run readable without the plan open beside it.
fn write_gate(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    passed: bool,
    name: &str,
) -> bool {
    write_gate_named(console, framebuffer, "usbcheck", passed, name)
}

fn write_gate_named(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    command: &str,
    passed: bool,
    name: &str,
) -> bool {
    let mut line = Line::new();
    line.push_str(command);
    line.push_str(if passed { ": PASS " } else { ": FAIL " });
    line.push_str(name);
    console.write_output_line(framebuffer, line.as_str());
    passed
}

/// Exercises the raw USB WRITE(10) path without creating filesystem objects.
///
/// `fswritetest` is the wrong first gate while transport writes are unstable:
/// its first failed metadata update can leave a directory entry which only a
/// different machine can repair. This command instead confines every write
/// to an explicitly sacrificial raw range. It snapshots that range and tries
/// to restore it, but correctness never depends on restore succeeding -- the
/// caller chose blocks whose contents may be discarded.
fn cmd_usbrawcheck(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
) {
    const BLOCK: usize = 512;
    const MAX_SPAN: usize = 8;
    const DEFAULT_WRITES: u32 = 32;
    const MAX_WRITES: u32 = 256;

    const MAX_GAP_MS: u32 = 2_000;
    const USAGE: &str = "usage: usbrawcheck <lba> [writes] [span] [gap_ms]";
    let (lba_text, rest) = split_first_word(trim(argument));
    let Some(first_lba) = parse_u32(lba_text) else {
        console.write_output_line(framebuffer, USAGE);
        return;
    };
    let (writes_text, rest) = split_first_word(trim(rest));
    let writes = if writes_text.is_empty() {
        DEFAULT_WRITES
    } else {
        match parse_u32(writes_text) {
            Some(value) if value > 0 && value <= MAX_WRITES => value,
            _ => {
                console.write_output_line(framebuffer, "writes must be 1..=256");
                return;
            }
        }
    };
    let (span_text, rest) = split_first_word(trim(rest));
    let span = if span_text.is_empty() {
        1usize
    } else {
        match parse_u32(span_text) {
            Some(value) if value > 0 && value <= MAX_SPAN as u32 => value as usize,
            _ => {
                console.write_output_line(framebuffer, "span must be 1..=8 blocks");
                return;
            }
        }
    };
    let (gap_text, rest) = split_first_word(trim(rest));
    let gap_ms = if gap_text.is_empty() {
        0
    } else {
        match parse_u32(gap_text) {
            Some(value) if value <= MAX_GAP_MS => value,
            _ => {
                console.write_output_line(framebuffer, "gap_ms must be 0..=2000");
                return;
            }
        }
    };
    if !trim(rest).is_empty() {
        console.write_output_line(framebuffer, USAGE);
        return;
    }
    let Some(last_lba) = first_lba.checked_add(span as u32 - 1) else {
        console.write_output_line(framebuffer, "raw range overflows the LBA address space");
        return;
    };

    console.write_output_line(
        framebuffer,
        "WARNING: raw sacrificial range; it MUST be outside every filesystem you care about",
    );
    console.write_output_line(
        framebuffer,
        "no files or directories are created; failed restore does not require filesystem repair",
    );
    let mut line = Line::new();
    line.push_str("usbrawcheck: LBA ");
    line.push_u32(first_lba);
    line.push_str("-");
    line.push_u32(last_lba);
    line.push_str(" writes=");
    line.push_u32(writes);
    line.push_str(" span=");
    line.push_u32(span as u32);
    line.push_str(" gap-ms=");
    line.push_u32(gap_ms);
    console.write_output_line(framebuffer, line.as_str());

    let Some(mass_storage) = usb_host.mass_storage_mut() else {
        console.write_output_line(
            framebuffer,
            "no Mass Storage device attached; plug one in and run 'usbrescan'",
        );
        return;
    };
    if !require_live_usb_msc(console, framebuffer, mass_storage) {
        return;
    }
    if !mass_storage.wait_until_ready(10) {
        console.write_output_line(framebuffer, "media not ready; aborting (nothing written)");
        return;
    }
    let Some(capacity) = mass_storage.read_capacity() else {
        console.write_output_line(framebuffer, "READ CAPACITY(10) failed; aborting");
        return;
    };
    if capacity.block_length != BLOCK as u32 {
        console.write_output_line(framebuffer, "device block length is not 512; aborting");
        return;
    }
    if last_lba > capacity.last_lba {
        console.write_output_line(framebuffer, "raw range extends beyond the medium; aborting");
        return;
    }

    let mut snapshot = [[0u8; BLOCK]; MAX_SPAN];
    for (slot, block) in snapshot.iter_mut().take(span).enumerate() {
        if !mass_storage.read_blocks(first_lba + slot as u32, block) {
            console.write_output_line(
                framebuffer,
                "could not snapshot the raw range; aborting (nothing written)",
            );
            return;
        }
    }
    let mut last_sequence = [u32::MAX; MAX_SPAN];
    let mut completed = 0u32;
    let mut pattern = [0u8; BLOCK];
    for sequence in 0..writes {
        let slot = sequence as usize % span;
        fill_usb_raw_pattern(&mut pattern, first_lba + slot as u32, sequence);
        // Once WRITE starts, its outcome cannot prove the block stayed
        // untouched. Mark it before issuing the command so a live session
        // attempts restoration even when the command reports failure.
        last_sequence[slot] = sequence;
        if mass_storage.write_blocks(first_lba + slot as u32, &mut pattern)
            != usb::WriteOutcome::Written
        {
            console.write_output_line(framebuffer, "raw burst WRITE failed, see UART log");
            break;
        }
        completed += 1;
        if gap_ms != 0 && sequence + 1 < writes {
            delay::delay_ms(gap_ms);
        }
    }

    // No read, flush, or ready poll occurs inside the burst above. That is
    // the command shape filesystem metadata exposed and usbwritetest hid by
    // verifying every individual write before issuing the next one.
    let mut pattern_ok = completed == writes && !mass_storage.needs_reinit();
    if pattern_ok {
        let _ = mass_storage.synchronize_cache();
        let _ = mass_storage.wait_until_ready(10);
        for (slot, sequence) in last_sequence.iter().take(span).enumerate() {
            if *sequence == u32::MAX {
                continue;
            }
            fill_usb_raw_pattern(&mut pattern, first_lba + slot as u32, *sequence);
            let mut current = [0u8; BLOCK];
            let read = mass_storage.read_blocks_from_medium(first_lba + slot as u32, &mut current)
                || mass_storage.read_blocks(first_lba + slot as u32, &mut current);
            pattern_ok &= read && current == pattern;
        }
    }

    let mut restored = !mass_storage.needs_reinit();
    if restored {
        for (slot, original) in snapshot.iter().take(span).enumerate() {
            if last_sequence[slot] == u32::MAX {
                continue;
            }
            let mut block = *original;
            if mass_storage.write_blocks(first_lba + slot as u32, &mut block)
                != usb::WriteOutcome::Written
            {
                restored = false;
                break;
            }
        }
    }
    if restored {
        let _ = mass_storage.synchronize_cache();
        let _ = mass_storage.wait_until_ready(10);
        for (slot, original) in snapshot.iter().take(span).enumerate() {
            if last_sequence[slot] == u32::MAX {
                continue;
            }
            let mut current = [0u8; BLOCK];
            let read = mass_storage.read_blocks_from_medium(first_lba + slot as u32, &mut current)
                || mass_storage.read_blocks(first_lba + slot as u32, &mut current);
            restored &= read && current == *original;
        }
    }

    let mut line = Line::new();
    line.push_str("usbrawcheck: writes completed=");
    line.push_u32(completed);
    line.push_str("/");
    line.push_u32(writes);
    line.push_str(" pattern=");
    line.push_str(if pattern_ok { "match" } else { "FAIL" });
    line.push_str(" restored=");
    line.push_str(if restored { "yes" } else { "NO" });
    console.write_output_line(framebuffer, line.as_str());

    if mass_storage.needs_reinit() {
        console.write_output_line(
            framebuffer,
            "MSC session unusable; run usbrescan and reuse the same sacrificial range",
        );
    }
    if !restored {
        console.write_output_line(
            framebuffer,
            "sacrificial range may contain test data; no filesystem repair is needed if it is outside every partition",
        );
    }
    console.write_output_line(
        framebuffer,
        if completed == writes && pattern_ok && restored {
            "usbrawcheck: RESULT PASS"
        } else {
            "usbrawcheck: RESULT FAIL"
        },
    );
}

fn fill_usb_raw_pattern(block: &mut [u8; 512], lba: u32, sequence: u32) {
    for (index, byte) in block.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(29).wrapping_add(sequence as u8)
            ^ (lba as u8).rotate_left(sequence & 7);
    }
    block[0..4].copy_from_slice(b"URAW");
    block[4..8].copy_from_slice(&lba.to_le_bytes());
    block[8..12].copy_from_slice(&sequence.to_le_bytes());
}

/// Stage 7's deliberately narrow multi-block WRITE(10) experiment.
///
/// The normal filesystem adapter remains capped at one block per command.
/// This command is the only caller that opts into 2/4/8-block commands, so
/// an inconclusive hardware result cannot silently change production I/O.
fn cmd_usbmultiwrite(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
) {
    const BLOCK: usize = 512;
    const MAX_BLOCKS: usize = 8;
    const GUARDED_BLOCKS: usize = MAX_BLOCKS + 2;
    const MAX_TEST_BYTES: usize = MAX_BLOCKS * BLOCK;
    const MAX_GUARDED_BYTES: usize = GUARDED_BLOCKS * BLOCK;
    const ROUNDS: u32 = 10;
    const USAGE: &str = "usage: usbmultiwrite <lba> <2|4|8>";

    let (lba_text, rest) = split_first_word(trim(argument));
    let (blocks_text, trailing) = split_first_word(trim(rest));
    let (Some(first_lba), Some(blocks)) = (parse_u32(lba_text), parse_u32(blocks_text)) else {
        console.write_output_line(framebuffer, USAGE);
        return;
    };
    if !trim(trailing).is_empty() || !matches!(blocks, 2 | 4 | 8) {
        console.write_output_line(framebuffer, USAGE);
        return;
    }
    let blocks = blocks as usize;
    let Some(last_lba) = first_lba.checked_add(blocks as u32 - 1) else {
        console.write_output_line(framebuffer, "test range overflows the LBA address space");
        return;
    };
    let Some(guard_first) = first_lba.checked_sub(1) else {
        console.write_output_line(
            framebuffer,
            "LBA 0 cannot be used: a leading guard block is required",
        );
        return;
    };
    let Some(guard_last) = last_lba.checked_add(1) else {
        console.write_output_line(
            framebuffer,
            "test range has no room for the trailing guard block",
        );
        return;
    };
    let guarded_blocks = blocks + 2;
    let test_bytes = blocks * BLOCK;
    let guarded_bytes = guarded_blocks * BLOCK;

    console.write_output_line(
        framebuffer,
        "WARNING: raw multi-block test; the test AND both guard blocks must be disposable",
    );
    console.write_output_line(
        framebuffer,
        "a dead USB session can prevent restoration; do not name any filesystem block",
    );
    let mut line = Line::new();
    line.push_str("usbmultiwrite: test LBA ");
    line.push_u32(first_lba);
    line.push_str("-");
    line.push_u32(last_lba);
    line.push_str(" guards=");
    line.push_u32(guard_first);
    line.push_str("/");
    line.push_u32(guard_last);
    console.write_output_line(framebuffer, line.as_str());

    let Some(mass_storage) = usb_host.mass_storage_mut() else {
        console.write_output_line(
            framebuffer,
            "no Mass Storage device attached; plug one in and run 'usbrescan'",
        );
        return;
    };
    if !require_live_usb_msc(console, framebuffer, mass_storage) {
        return;
    }
    if !mass_storage.wait_until_ready(10) {
        console.write_output_line(framebuffer, "media not ready; aborting (nothing written)");
        return;
    }
    let Some(capacity) = mass_storage.read_capacity() else {
        console.write_output_line(framebuffer, "READ CAPACITY(10) failed; aborting");
        return;
    };
    if capacity.block_length != BLOCK as u32 {
        console.write_output_line(framebuffer, "device block length is not 512; aborting");
        return;
    }
    if guard_last > capacity.last_lba {
        console.write_output_line(
            framebuffer,
            "guarded range extends beyond the medium; aborting",
        );
        return;
    }

    let mut line = Line::new();
    line.push_str("usbmultiwrite: blocks=");
    line.push_u32(blocks as u32);
    line.push_str(" rounds=");
    line.push_u32(ROUNDS);
    line.push_str(" bulk-out-mps=");
    line.push_u32(mass_storage.bulk_out_mps() as u32);
    console.write_output_line(framebuffer, line.as_str());
    console.write_output_line(
        framebuffer,
        "packet requested/actual/PID and final CSW status/residue are in the UART log",
    );

    let mut snapshot = [0u8; MAX_GUARDED_BYTES];
    if !mass_storage.read_blocks(guard_first, &mut snapshot[..guarded_bytes]) {
        console.write_output_line(
            framebuffer,
            "could not snapshot the guarded range; aborting (nothing written)",
        );
        return;
    }

    let before = mass_storage.transport_observation();
    let mut pattern = [0u8; MAX_TEST_BYTES];
    let mut verify = [0u8; MAX_GUARDED_BYTES];
    let mut writes_accepted = 0u32;
    let mut rounds_verified = 0u32;
    let mut collateral = 0u32;
    let mut test_ok = true;

    for round in 0..ROUNDS {
        fill_usb_multiwrite_pattern(&mut pattern[..test_bytes], first_lba, blocks, round);
        let mut line = Line::new();
        line.push_str("usbmultiwrite: round ");
        line.push_u32(round + 1);
        line.push_str("/");
        line.push_u32(ROUNDS);
        console.write_output_line(framebuffer, line.as_str());

        if mass_storage.write_blocks_diagnostic(first_lba, &mut pattern[..test_bytes])
            != usb::WriteOutcome::Written
        {
            console.write_output_line(
                framebuffer,
                "multi-block WRITE failed; it was not replayed (see UART log)",
            );
            test_ok = false;
            break;
        }
        writes_accepted += 1;

        let _ = mass_storage.synchronize_cache();
        let _ = mass_storage.wait_until_ready(10);
        let read = mass_storage.read_blocks_from_medium(guard_first, &mut verify[..guarded_bytes])
            || mass_storage.read_blocks(guard_first, &mut verify[..guarded_bytes]);
        if !read {
            console.write_output_line(framebuffer, "guarded read-back failed");
            test_ok = false;
            break;
        }

        let leading_ok = verify[..BLOCK] == snapshot[..BLOCK];
        let trailing_start = (guarded_blocks - 1) * BLOCK;
        let trailing_ok =
            verify[trailing_start..guarded_bytes] == snapshot[trailing_start..guarded_bytes];
        collateral += u32::from(!leading_ok) + u32::from(!trailing_ok);
        let payload_ok = verify[BLOCK..BLOCK + test_bytes] == pattern[..test_bytes];
        if !payload_ok || !leading_ok || !trailing_ok {
            console.write_output_line(
                framebuffer,
                if payload_ok {
                    "COLLATERAL DAMAGE: a guard block changed"
                } else {
                    "multi-block pattern read-back MISMATCH"
                },
            );
            test_ok = false;
            break;
        }
        rounds_verified += 1;
    }

    // Restore with the already accepted one-block command shape. This is
    // not a replay of a failed multi-block WRITE: it writes the saved bytes
    // back to each possibly changed LBA after the experiment has stopped.
    let mut restored = !mass_storage.needs_reinit();
    if restored {
        let current_known = mass_storage.read_blocks(guard_first, &mut verify[..guarded_bytes]);
        for index in 0..guarded_blocks {
            let start = index * BLOCK;
            let end = start + BLOCK;
            if current_known && verify[start..end] == snapshot[start..end] {
                continue;
            }
            let mut original = [0u8; BLOCK];
            original.copy_from_slice(&snapshot[start..end]);
            if mass_storage.write_blocks(guard_first + index as u32, &mut original)
                != usb::WriteOutcome::Written
            {
                restored = false;
                break;
            }
        }
    }
    if restored {
        let _ = mass_storage.synchronize_cache();
        let _ = mass_storage.wait_until_ready(10);
        let read = mass_storage.read_blocks_from_medium(guard_first, &mut verify[..guarded_bytes])
            || mass_storage.read_blocks(guard_first, &mut verify[..guarded_bytes]);
        restored = read && verify[..guarded_bytes] == snapshot[..guarded_bytes];
    }

    let after = mass_storage.transport_observation();
    let mut line = Line::new();
    line.push_str("usbmultiwrite: accepted=");
    line.push_u32(writes_accepted);
    line.push_str("/");
    line.push_u32(ROUNDS);
    line.push_str(" verified=");
    line.push_u32(rounds_verified);
    line.push_str(" collateral=");
    line.push_u32(collateral);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("usbmultiwrite: delta pkt-error+");
    line.push_u32(
        after
            .packet_error_retries
            .wrapping_sub(before.packet_error_retries),
    );
    line.push_str(" timeout+");
    line.push_u32(after.timeout_retries.wrapping_sub(before.timeout_retries));
    line.push_str(" recovery+");
    line.push_u32(after.reset_recoveries.wrapping_sub(before.reset_recoveries));
    console.write_output_line(framebuffer, line.as_str());

    console.write_output_line(
        framebuffer,
        if restored {
            "usbmultiwrite: original guarded range restored: yes"
        } else {
            "usbmultiwrite: original guarded range restored: NO -- range may be corrupted"
        },
    );
    if mass_storage.needs_reinit() {
        console.write_output_line(
            framebuffer,
            "MSC session unusable; run usbrescan before any further USB command",
        );
    }

    let passed = test_ok
        && writes_accepted == ROUNDS
        && rounds_verified == ROUNDS
        && collateral == 0
        && restored;
    console.write_output_line(
        framebuffer,
        if passed {
            "usbmultiwrite: RESULT PASS"
        } else {
            "usbmultiwrite: RESULT FAIL"
        },
    );
}

fn fill_usb_multiwrite_pattern(buffer: &mut [u8], first_lba: u32, blocks: usize, round: u32) {
    debug_assert_eq!(buffer.len(), blocks * 512);
    let (block_buffers, remainder) = buffer.as_chunks_mut::<512>();
    debug_assert!(remainder.is_empty());
    for (block_index, block) in block_buffers.iter_mut().enumerate() {
        let lba = first_lba + block_index as u32;
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(37).wrapping_add(round as u8)
                ^ (lba as u8).rotate_left((round + block_index as u32) & 7);
        }
        block[0..4].copy_from_slice(b"UMW7");
        block[4..8].copy_from_slice(&lba.to_le_bytes());
        block[8..12].copy_from_slice(&round.to_le_bytes());
        block[12..16].copy_from_slice(&(block_index as u32).to_le_bytes());
    }
}

/// Runs one configuration's acceptance sequence and reports its own verdict.
///
/// This is the whole per-topology, per-medium round of
/// `docs/plans/archive/USB_BOT_HCD_REFACTOR_PLAN.md` in one command: counters before, the
/// read soak, the write rounds, counters after, the deltas, and a gate for
/// each Go condition. [`cmd_usbrawcheck`] supplies the no-filesystem burst
/// gate while raw WRITE remains unstable.
fn cmd_usbcheck(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
) {
    /// Write rounds per run when an LBA is given. Stage 0's matrix asked for
    /// ten, and ten is what caught the restore failing eight times out of
    /// ten on the Full-Speed hub path.
    const WRITE_ROUNDS: u32 = 10;

    const USAGE: &str = "usage: usbcheck [reads] [lba]";
    let (reads_text, rest) = split_first_word(trim(argument));
    let reads = if reads_text.is_empty() {
        100
    } else {
        match parse_u32(reads_text) {
            Some(value) if value > 0 && value <= 1_000 => value,
            _ => {
                console.write_output_line(framebuffer, USAGE);
                return;
            }
        }
    };
    let (lba_text, rest) = split_first_word(trim(rest));
    let lba = if lba_text.is_empty() {
        None
    } else {
        match parse_u32(lba_text) {
            Some(value) => Some(value),
            None => {
                console.write_output_line(framebuffer, USAGE);
                return;
            }
        }
    };
    if !trim(rest).is_empty() {
        console.write_output_line(framebuffer, USAGE);
        return;
    }

    if usb_host.mass_storage().is_none() {
        console.write_output_line(
            framebuffer,
            "usbcheck: no USB Mass Storage; attach one and run usbrescan",
        );
        return;
    }

    let mut line = Line::new();
    line.push_str("usbcheck: reads=");
    line.push_u32(reads);
    match lba {
        Some(value) => {
            line.push_str(" writes=");
            line.push_u32(WRITE_ROUNDS);
            line.push_str(" lba=");
            line.push_u32(value);
        }
        None => line.push_str(" writes=0 (read-only; pass an LBA to test writes)"),
    }
    console.write_output_line(framebuffer, line.as_str());

    let before = usb_check_counters(usb_host);
    let soak = run_usb_read_soak(console, framebuffer, reads, "usbcheck", usb_host);

    let mut rounds = 0u32;
    let mut pattern_ok = 0u32;
    let mut restored = 0u32;
    let mut collateral = 0u32;
    let mut session_lost = false;
    if let Some(target) = lba {
        for _ in 0..WRITE_ROUNDS {
            // A session that has to be re-enumerated fails every remaining
            // round the same way and takes seconds each to do it. Stop and
            // report the rounds that actually ran.
            if session_lost {
                break;
            }
            let outcome = run_usb_write_test(console, framebuffer, target, usb_host);
            if !outcome.attempted {
                break;
            }
            rounds += 1;
            pattern_ok += u32::from(outcome.pattern_ok);
            restored += u32::from(outcome.restored);
            collateral += outcome.collateral;
            session_lost = outcome.session_lost;
        }
    }

    let after = usb_check_counters(usb_host);
    write_usbcheck_deltas(console, framebuffer, &before, &after);

    let mut line = Line::new();
    line.push_str("usbcheck: writes ok=");
    line.push_u32(pattern_ok);
    line.push_str("/");
    line.push_u32(rounds);
    line.push_str(" restored=");
    line.push_u32(restored);
    line.push_str("/");
    line.push_u32(rounds);
    line.push_str(" collateral=");
    line.push_u32(collateral);
    console.write_output_line(framebuffer, line.as_str());

    let cache_refusals = after
        .host
        .cache_refusals
        .wrapping_sub(before.host.cache_refusals);
    let refused = after
        .transport
        .retries_refused
        .wrapping_sub(before.transport.retries_refused);
    let progressed = after
        .transport
        .retries_after_progress
        .wrapping_sub(before.transport.retries_after_progress);
    // A packet failure the bus produced and a retry then recovered from is
    // not a failed run: the plan's Go conditions allow packet retries whose
    // HCINT and actual length are explained, and whether one actually broke
    // anything is what the read and write gates answer. The kinds below are
    // different -- none of them can be produced by a device or a cable, so
    // every one is this driver breaking its own contract.
    let contract_failures = usb_check_kind_sum(
        &after,
        &before,
        &[
            usb::PacketFailureKind::ShortOut,
            usb::PacketFailureKind::CacheSyncRefused,
            usb::PacketFailureKind::QtdInvalidStatus,
            usb::PacketFailureKind::NotTransferComplete,
            usb::PacketFailureKind::StaleCompletion,
            usb::PacketFailureKind::SplitRejected,
        ],
    );
    let transport_failures = usb_check_kind_sum(
        &after,
        &before,
        &[
            usb::PacketFailureKind::HaltTimeout,
            usb::PacketFailureKind::Stall,
            usb::PacketFailureKind::TransactionError,
            usb::PacketFailureKind::QtdPacketError,
        ],
    );
    let csw_contract_failures = after
        .transport
        .csw_short
        .wrapping_sub(before.transport.csw_short)
        .saturating_add(
            after
                .transport
                .csw_bad_signature
                .wrapping_sub(before.transport.csw_bad_signature),
        )
        .saturating_add(
            after
                .transport
                .csw_tag_mismatch
                .wrapping_sub(before.transport.csw_tag_mismatch),
        )
        .saturating_add(
            after
                .transport
                .csw_phase_error
                .wrapping_sub(before.transport.csw_phase_error),
        )
        .saturating_add(
            after
                .transport
                .csw_invalid_status
                .wrapping_sub(before.transport.csw_invalid_status),
        )
        .saturating_add(
            after
                .transport
                .csw_residue_mismatch
                .wrapping_sub(before.transport.csw_residue_mismatch),
        );

    let mut all = write_gate(
        console,
        framebuffer,
        cache_refusals == 0,
        "cache sync (stage 1)",
    );
    all &= write_gate(
        console,
        framebuffer,
        contract_failures == 0,
        "driver contract (no impossible completions)",
    );
    all &= write_gate(
        console,
        framebuffer,
        csw_contract_failures == 0,
        "BOT CSW contract (stage 3)",
    );
    all &= write_gate(console, framebuffer, soak.passed(), "read soak");
    if lba.is_some() {
        all &= write_gate(
            console,
            framebuffer,
            rounds == WRITE_ROUNDS
                && pattern_ok == WRITE_ROUNDS
                && restored == WRITE_ROUNDS
                && collateral == 0,
            "write rounds",
        );
    }

    // Reported, not gated: the bus produced these and a retry dealt with
    // them. They still belong in the record, because "the run passed" and
    // "the run passed without the transport stumbling" are different
    // results and the second is the one that gets quieter as the refactor
    // lands.
    if transport_failures != 0 {
        let mut line = Line::new();
        line.push_str("usbcheck: NOTE transport events=");
        line.push_u32(transport_failures);
        line.push_str(" retried err+");
        line.push_u32(
            after
                .transport
                .packet_error_retries
                .wrapping_sub(before.transport.packet_error_retries),
        );
        line.push_str(" timeout+");
        line.push_u32(
            after
                .transport
                .timeout_retries
                .wrapping_sub(before.transport.timeout_retries),
        );
        console.write_output_line(framebuffer, line.as_str());
    }

    // Reported, not gated. A retry of a packet that had already moved bytes
    // is what stages 2 and 3 exist to make impossible; until they land it is
    // an expected observation on the Full-Speed hub path, and failing the
    // run on it would hide whether anything else regressed.
    if refused != 0 {
        let mut line = Line::new();
        line.push_str("usbcheck: NOTE refused=");
        line.push_u32(refused);
        line.push_str(" progressed=");
        line.push_u32(progressed);
        line.push_str(" bytes=");
        line.push_u32(
            after
                .transport
                .retry_progress_bytes
                .wrapping_sub(before.transport.retry_progress_bytes),
        );
        console.write_output_line(framebuffer, line.as_str());
        console.write_output_line(
            framebuffer,
            "usbcheck:   resending them could have put the same bytes on the bus twice",
        );
    }
    if session_lost {
        console.write_output_line(
            framebuffer,
            "usbcheck: MSC session was lost; run 'usbrescan' before the next command",
        );
    }

    console.write_output_line(
        framebuffer,
        if all {
            "usbcheck: RESULT PASS"
        } else {
            "usbcheck: RESULT FAIL"
        },
    );
}

/// The counter deltas across one acceptance run, one field per line group.
///
/// Only deltas: an absolute count carries every boot-time enumeration and
/// every idle keyboard poll since power-on, which is what made two `usbhw`
/// captures necessary in the first place.
fn write_usbcheck_deltas(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    before: &UsbCheckCounters,
    after: &UsbCheckCounters,
) {
    let kinds = usb::packet_failure_kind_names();

    let mut line = Line::new();
    line.push_str("usbcheck: delta ");
    push_delta(
        &mut line,
        "cache-refusals",
        after.host.cache_refusals,
        before.host.cache_refusals,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "pkt-fail",
        after.host.packet_failures,
        before.host.packet_failures,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "idle-poll",
        after.host.idle_poll_timeouts,
        before.host.idle_poll_timeouts,
    );
    console.write_output_line(framebuffer, line.as_str());

    // Position 0 of the kind table is the "no failure" placeholder. The
    // groups are sized so that five-digit counts still fit one 80-column
    // line, the same constraint the `usbhw` block is built around.
    for group in [1usize..5, 5..8, 8..kinds.len()] {
        let mut line = Line::new();
        line.push_str("usbcheck: delta pkt-fail");
        for index in group {
            line.push_str(" ");
            push_delta(
                &mut line,
                kinds[index],
                after.host.packet_failures_by_kind[index],
                before.host.packet_failures_by_kind[index],
            );
        }
        console.write_output_line(framebuffer, line.as_str());
    }

    // Flushed, timed out and skipped are three different outcomes per FIFO,
    // and a run that reports only the first cannot say whether the shared
    // FIFOs were cleaned or deliberately left alone for a live keyboard.
    let fifos = usb::fifo_names();
    for (heading, after_counts, before_counts) in [
        (
            "usbcheck: delta fifo-flush ",
            &after.host.fifo_flushes,
            &before.host.fifo_flushes,
        ),
        (
            "usbcheck: delta fifo-timeout",
            &after.host.fifo_flush_timeouts,
            &before.host.fifo_flush_timeouts,
        ),
        (
            "usbcheck: delta fifo-skipped",
            &after.host.fifo_flushes_skipped_for_periodic,
            &before.host.fifo_flushes_skipped_for_periodic,
        ),
    ] {
        let mut line = Line::new();
        line.push_str(heading);
        for (index, name) in fifos.iter().enumerate() {
            line.push_str(" ");
            push_delta(&mut line, name, after_counts[index], before_counts[index]);
        }
        console.write_output_line(framebuffer, line.as_str());
    }

    let mut line = Line::new();
    line.push_str("usbcheck: delta packet-cleanup ");
    push_delta(
        &mut line,
        "out-nptx",
        after.host.out_packet_error_nptx_cleanups,
        before.host.out_packet_error_nptx_cleanups,
    );
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("usbcheck: delta ");
    push_delta(
        &mut line,
        "cmd-retry",
        after.command_retries,
        before.command_retries,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "cleanup-failed",
        after.transport.cleanup_failures,
        before.transport.cleanup_failures,
    );
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("usbcheck: delta pkt-retry ");
    push_delta(
        &mut line,
        "err",
        after.transport.packet_error_retries,
        before.transport.packet_error_retries,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "timeout",
        after.transport.timeout_retries,
        before.transport.timeout_retries,
    );
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("usbcheck: delta resubmit ");
    push_delta(
        &mut line,
        "refused",
        after.transport.retries_refused,
        before.transport.retries_refused,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "progressed",
        after.transport.retries_after_progress,
        before.transport.retries_after_progress,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "impossible-len",
        after.host.impossible_remainders,
        before.host.impossible_remainders,
    );
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("usbcheck: delta recovery ");
    push_delta(
        &mut line,
        "ok",
        after.transport.reset_recoveries,
        before.transport.reset_recoveries,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "failed",
        after.transport.reset_recovery_failures,
        before.transport.reset_recovery_failures,
    );
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("usbcheck: delta csw ");
    push_delta(
        &mut line,
        "short",
        after.transport.csw_short,
        before.transport.csw_short,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "sig",
        after.transport.csw_bad_signature,
        before.transport.csw_bad_signature,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "tag",
        after.transport.csw_tag_mismatch,
        before.transport.csw_tag_mismatch,
    );
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("usbcheck: delta csw-detail ");
    push_delta(
        &mut line,
        "phase",
        after.transport.csw_phase_error,
        before.transport.csw_phase_error,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "status",
        after.transport.csw_invalid_status,
        before.transport.csw_invalid_status,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "residue",
        after.transport.csw_residue_mismatch,
        before.transport.csw_residue_mismatch,
    );
    line.push_str(" ");
    push_delta(
        &mut line,
        "early",
        after.transport.csw_early,
        before.transport.csw_early,
    );
    console.write_output_line(framebuffer, line.as_str());
}

/// Proves that the four faults the HCD contract exists to catch are
/// detected rather than absorbed.
///
/// Stages 1, 2 and 4 of `docs/plans/archive/USB_BOT_HCD_REFACTOR_PLAN.md` add rules that
/// working hardware never exercises: a refused DMA cache synchronization
/// must stop the packet before the channel is armed and publish nothing; a
/// completion arriving under a generation the slot no longer holds must not
/// be accepted; an OUT that moved fewer bytes than it was given must not
/// reach the layer above as a success; and a host cleanup whose FIFO flush
/// did not finish must stop the command it was preparing for. None of the
/// four can be produced on demand by a device, so each is injected here. A
/// rule that has never been seen to fire is a rule nobody knows works.
///
/// Topology-independent: this tests the driver's own logic, so one run
/// covers every configuration in the matrix.
fn cmd_usbcachefail(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &mut usb::UsbHost,
) {
    let mut all = usb_fault_cache_sync(console, framebuffer, usb_host);
    all &= usb_fault_stale_completion(console, framebuffer, usb_host);
    all &= usb_fault_short_out(console, framebuffer, usb_host);
    all &= usb_fault_fifo_flush(console, framebuffer, usb_host);

    console.write_output_line(
        framebuffer,
        if all {
            "usbcachefail: RESULT PASS"
        } else {
            "usbcachefail: RESULT FAIL"
        },
    );
    console.write_output_line(
        framebuffer,
        "usbcachefail: MSC sessions were retired as designed; run 'usbrescan'",
    );
}

/// Re-enumerates when the previous injection retired the session, so the
/// next one starts against a device that can still answer.
///
/// Each injection deliberately fails a command twice over, which is exactly
/// what the BOT layer treats as recovery that is not holding. Without this
/// the second and third checks would report the first one's wreckage. A
/// failed command can also leave a downstream device unable to answer the
/// first descriptor request immediately after reset. Retry the whole bounded
/// rescan here instead of turning that transient reacquisition failure into
/// two skipped contract checks.
fn usb_fault_reset(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &mut usb::UsbHost,
) -> bool {
    const RESCAN_ATTEMPTS: u32 = 3;
    const RESCAN_RETRY_DELAY_MS: u32 = 500;

    let storage_ready = |host: &usb::UsbHost| {
        host.mass_storage()
            .is_some_and(|storage| !storage.needs_reinit())
    };
    if storage_ready(usb_host) {
        return true;
    }

    for attempt in 0..RESCAN_ATTEMPTS {
        if attempt != 0 {
            console.write_output_line(
                framebuffer,
                "usbcachefail: retrying USB rescan after 500 ms",
            );
            delay::delay_ms(RESCAN_RETRY_DELAY_MS);
        }
        usb_host.rescan(usb::RescanReason::Recovery);
        if storage_ready(usb_host) {
            return true;
        }
    }

    if usb_host
        .mass_storage()
        .is_some_and(usb::UsbMassStorage::needs_reinit)
    {
        console.write_output_line(
            framebuffer,
            "usbcachefail: USB Mass Storage did not recover; run usbrescan",
        );
    } else {
        console.write_output_line(
            framebuffer,
            "usbcachefail: no USB Mass Storage after 3 rescans; check attachment",
        );
    }
    false
}

/// Neither a plausible block of data nor the zero a freshly staged buffer
/// holds, so finding it intact afterwards proves nothing was published
/// over it.
const USB_FAULT_SENTINEL: u8 = 0x5A;
const USB_FAULT_BLOCK: usize = 512;
/// Comfortably more than the one replay `msc.rs` performs after Reset
/// Recovery, so a check does not have to track that policy. Whatever is
/// left over is disarmed afterwards.
const USB_FAULT_ARMED: u32 = 4;

/// Stage 1: a refused cache synchronization fails the packet before the
/// channel is armed, and publishes nothing.
///
/// The injection is aimed at the **data IN** phase and armed several times
/// over, for two reasons the first version of this check got wrong. A
/// single untargeted refusal lands on the command block, which is an OUT
/// packet and so says nothing about what gets published to the caller; and
/// one refusal is healed by the READ(10) replay `msc.rs` performs after
/// Reset Recovery, which is the retry policy working rather than a fault.
fn usb_fault_cache_sync(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &mut usb::UsbHost,
) -> bool {
    if !usb_fault_reset(console, framebuffer, usb_host) {
        return false;
    }
    console.write_output_line(
        framebuffer,
        "usbcachefail: [1/4] refusing every data-IN cache sync of the next read",
    );

    let refusals_before = usb::cache_refusal_count();
    let before = usb::host_observation().packet_failures_by_kind;
    let mut buffer = [USB_FAULT_SENTINEL; USB_FAULT_BLOCK];

    let _ = usb::force_cache_refusals(USB_FAULT_ARMED, Some(usb::TransferLabel::DataIn));
    let read_ok = usb_host
        .mass_storage_mut()
        .is_some_and(|storage| storage.read_blocks(0, &mut buffer));
    // Nothing stays armed for an unrelated later transfer, whether or not
    // the read consumed the whole allowance.
    let _ = usb::force_cache_refusals(0, None);

    let refusals = usb::cache_refusal_count().wrapping_sub(refusals_before);
    let counted = usb_fault_kind_delta(&before, usb::PacketFailureKind::CacheSyncRefused);

    let mut all = write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        refusals >= 1 && counted >= 1,
        "[1/4] the refusal failed the packet as a cache-sync failure",
    );
    all &= write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        !read_ok,
        "[1/4] the read failed rather than succeeding",
    );
    all &= write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        buffer.iter().all(|byte| *byte == USB_FAULT_SENTINEL),
        "[1/4] no bytes were published over the destination",
    );
    all
}

/// Stage 2: a completion delivered under a generation the slot no longer
/// holds is rejected instead of being reaped as this packet's result.
fn usb_fault_stale_completion(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &mut usb::UsbHost,
) -> bool {
    if !usb_fault_reset(console, framebuffer, usb_host) {
        return false;
    }
    console.write_output_line(
        framebuffer,
        "usbcachefail: [2/4] delivering the next completions under a stale generation",
    );

    let before = usb::host_observation().packet_failures_by_kind;
    let stale_before = usb::interrupt_diagnostics().stale_tokens;
    let mut buffer = [USB_FAULT_SENTINEL; USB_FAULT_BLOCK];

    let _ = usb::force_stale_completions(USB_FAULT_ARMED);
    let read_ok = usb_host
        .mass_storage_mut()
        .is_some_and(|storage| storage.read_blocks(0, &mut buffer));
    let _ = usb::force_stale_completions(0);

    let counted = usb_fault_kind_delta(&before, usb::PacketFailureKind::StaleCompletion);
    let stale_tokens = usb::interrupt_diagnostics()
        .stale_tokens
        .wrapping_sub(stale_before);

    let mut all = write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        counted >= 1 && stale_tokens >= 1,
        "[2/4] the stale completion was rejected and counted",
    );
    all &= write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        !read_ok,
        "[2/4] the read failed rather than succeeding",
    );
    all &= write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        buffer.iter().all(|byte| *byte == USB_FAULT_SENTINEL),
        "[2/4] no bytes were published over the destination",
    );
    all
}

/// Stage 2: an OUT reported short does not reach the layer above as a
/// success for the length that was asked for.
fn usb_fault_short_out(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &mut usb::UsbHost,
) -> bool {
    if !usb_fault_reset(console, framebuffer, usb_host) {
        return false;
    }
    console.write_output_line(
        framebuffer,
        "usbcachefail: [3/4] reporting the next OUT packets one byte short",
    );

    let before = usb::host_observation().packet_failures_by_kind;
    let mut buffer = [USB_FAULT_SENTINEL; USB_FAULT_BLOCK];

    let _ = usb::force_short_outs(USB_FAULT_ARMED);
    // A read is used rather than a write: its command block is an OUT
    // packet, so the injection lands without putting anything on the
    // medium.
    let read_ok = usb_host
        .mass_storage_mut()
        .is_some_and(|storage| storage.read_blocks(0, &mut buffer));
    let _ = usb::force_short_outs(0);

    let counted = usb_fault_kind_delta(&before, usb::PacketFailureKind::ShortOut);

    let mut all = write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        counted >= 1,
        "[3/4] the short OUT was rejected and counted",
    );
    all &= write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        !read_ok,
        "[3/4] the command failed rather than succeeding",
    );
    all
}

/// Stage 4: when the host cleanup after a real failure cannot flush a FIFO,
/// the session is retired instead of running BOT Reset Recovery through a
/// FIFO that could not be emptied.
///
/// Two injections at once, because the cleanup being tested only runs after
/// something has already failed: a refused data-IN cache sync fails the
/// READ(10), and the FIFO flush timeout then fails the cleanup that failure
/// triggers.
///
/// Until Stage 6 this check aimed at the proactive cleanup that used to run
/// before every WRITE(10). That cleanup is gone -- three topologies ran
/// 1000 reads and 100 writes each without it -- so the contract that
/// remains is this one, on the failure path, which is also the one with
/// teeth: Reset Recovery is a pair of control transfers plus more bulk
/// traffic, all of it through the FIFO that just refused to empty.
fn usb_fault_fifo_flush(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &mut usb::UsbHost,
) -> bool {
    if !usb_fault_reset(console, framebuffer, usb_host) {
        return false;
    }
    console.write_output_line(
        framebuffer,
        "usbcachefail: [4/4] failing a read, then reporting its cleanup flush as timed out",
    );

    let Some(before) = usb_host
        .mass_storage()
        .map(usb::UsbMassStorage::transport_observation)
    else {
        console.write_output_line(framebuffer, "usbcachefail: [4/4] SKIP no USB Mass Storage");
        return false;
    };
    let timeouts_before = usb_fault_fifo_timeout_total();
    let mut buffer = [USB_FAULT_SENTINEL; USB_FAULT_BLOCK];

    let _ = usb::force_cache_refusals(USB_FAULT_ARMED, Some(usb::TransferLabel::DataIn));
    let _ = usb::force_fifo_flush_timeouts(USB_FAULT_ARMED);
    let read_ok = usb_host
        .mass_storage_mut()
        .is_some_and(|storage| storage.read_blocks(0, &mut buffer));
    let _ = usb::force_fifo_flush_timeouts(0);
    let _ = usb::force_cache_refusals(0, None);

    let after = usb_host
        .mass_storage()
        .map(usb::UsbMassStorage::transport_observation)
        .unwrap_or_default();
    let timeouts = usb_fault_fifo_timeout_total().wrapping_sub(timeouts_before);
    let cleanup_failures = after.cleanup_failures.wrapping_sub(before.cleanup_failures);
    let recoveries = after.reset_recoveries.wrapping_sub(before.reset_recoveries);
    let retired = usb_host
        .mass_storage()
        .is_some_and(usb::UsbMassStorage::needs_reinit);

    let mut all = write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        timeouts >= 1 && cleanup_failures >= 1,
        "[4/4] the flush timeout failed the cleanup and was counted",
    );
    all &= write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        !read_ok,
        "[4/4] the read failed rather than succeeding",
    );
    all &= write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        recoveries == 0,
        "[4/4] no BOT Reset Recovery was attempted through the stuck FIFO",
    );
    all &= write_gate_named(
        console,
        framebuffer,
        "usbcachefail",
        retired,
        "[4/4] the session was retired for re-enumeration",
    );
    all
}

/// Flush timeouts across all three FIFOs. Which one the cleanup reached
/// first depends on whether a periodic endpoint is armed, so the check adds
/// them up rather than naming one.
fn usb_fault_fifo_timeout_total() -> u32 {
    usb::host_observation()
        .fifo_flush_timeouts
        .iter()
        .fold(0u32, |total, count| total.wrapping_add(*count))
}

/// How many failures of `kind` have been counted since `before`.
fn usb_fault_kind_delta(
    before: &[u32; usb::PACKET_FAILURE_KIND_COUNT],
    kind: usb::PacketFailureKind,
) -> u32 {
    let index = usb::packet_failure_kind_index(kind);
    usb::host_observation().packet_failures_by_kind[index].wrapping_sub(before[index])
}

/// Overwrites blocks on USB Mass Storage with zeros, the USB counterpart of
/// `sdzero` -- and the way to clear the pattern `usbwritetest` leaves behind
/// when its restore cannot complete.
///
/// Zeroing is deliberately not folded into `usbwritetest`'s failure path: at
/// the point that test gives up, the transport is usually dead, so the write
/// that clears up would fail too. This is a separate command run afterwards,
/// against a fresh session, which is also why it re-checks the block length
/// and capacity for itself.
fn cmd_usbzero(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    argument: &[u8],
    usb_host: &mut usb::UsbHost,
) {
    const BLOCK: usize = 512;
    const MAX_BLOCKS: u32 = 8;

    let (lba_text, rest) = split_first_word(argument);
    let Some(lba) = parse_u32(lba_text) else {
        console.write_output_line(framebuffer, "usage: usbzero <lba> [count]");
        return;
    };
    let count = if trim(rest).is_empty() {
        1
    } else {
        match parse_u32(trim(rest)) {
            Some(value) if value > 0 && value <= MAX_BLOCKS => value,
            _ => {
                console.write_output_line(framebuffer, "usage: usbzero <lba> [count] (1-8)");
                return;
            }
        }
    };

    console.write_output_line(
        framebuffer,
        "WARNING: overwrites blocks with zeros for good",
    );
    let Some(mass_storage) = usb_host.mass_storage_mut() else {
        console.write_output_line(
            framebuffer,
            "no Mass Storage device attached; plug one in and run 'usbrescan'",
        );
        return;
    };
    if !require_live_usb_msc(console, framebuffer, mass_storage) {
        return;
    }

    if !mass_storage.wait_until_ready(10) {
        console.write_output_line(framebuffer, "media not ready; aborting (nothing written)");
        return;
    }
    let Some(capacity) = mass_storage.read_capacity() else {
        console.write_output_line(framebuffer, "READ CAPACITY(10) failed; aborting");
        return;
    };
    if capacity.block_length != BLOCK as u32 {
        let mut line = Line::new();
        line.push_str("device block length is ");
        line.push_u32(capacity.block_length);
        line.push_str(" bytes, not 512; aborting");
        console.write_output_line(framebuffer, line.as_str());
        return;
    }
    if lba.saturating_add(count - 1) > capacity.last_lba {
        let mut line = Line::new();
        line.push_str("range beyond last block (");
        line.push_u32(capacity.last_lba);
        line.push_str("); aborting");
        console.write_output_line(framebuffer, line.as_str());
        return;
    }

    let mut zeroed = 0u32;
    let mut failed_lba = None;
    let mut flush_failed = false;
    for offset in 0..count {
        let block_lba = lba + offset;
        let mut zero = [0u8; BLOCK];
        if mass_storage.write_blocks(block_lba, &mut zero) != usb::WriteOutcome::Written {
            failed_lba = Some(block_lba);
            break;
        }
        if mass_storage.needs_reinit() {
            failed_lba = Some(block_lba);
            break;
        }
        // Same reasoning as `usbwritetest`: a write that has been accepted
        // is not necessarily on the medium, and the check below has to see
        // the medium rather than the cache.
        if mass_storage.synchronize_cache() == usb::CacheSync::Failed {
            flush_failed = true;
        }
        let _ = mass_storage.wait_until_ready(10);
        let mut check = [0u8; BLOCK];
        let checked = mass_storage.read_blocks_from_medium(block_lba, &mut check)
            || mass_storage.read_blocks(block_lba, &mut check);
        // The read-back decides, not the flush: a device that refuses to
        // flush can still have taken the write.
        if !checked || check != [0u8; BLOCK] {
            failed_lba = Some(block_lba);
            break;
        }
        zeroed += 1;
    }

    let mut line = Line::new();
    match failed_lba {
        None => {
            line.push_str("zeroed ");
            line.push_u32(zeroed);
            line.push_str(" block(s) from LBA ");
            line.push_u32(lba);
        }
        Some(block_lba) => {
            line.push_str("stopped at LBA ");
            line.push_u32(block_lba);
            line.push_str(" after ");
            line.push_u32(zeroed);
            line.push_str(" block(s), see UART log");
        }
    }
    console.write_output_line(framebuffer, line.as_str());
    if flush_failed {
        console.write_output_line(
            framebuffer,
            "note: SYNCHRONIZE CACHE(10) failed; zeros verified by FUA read instead",
        );
    }
    if mass_storage.needs_reinit() {
        console.write_output_line(
            framebuffer,
            "MSC session unusable; run 'usbrescan', then try again",
        );
    } else if failed_lba.is_some() {
        write_usb_sense_line(console, framebuffer, mass_storage);
    }
}

/// Shows why the last command failed, in the device's own words.
fn write_usb_sense_line(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    mass_storage: &mut usb::UsbMassStorage,
) {
    let Some(sense) = mass_storage.request_sense() else {
        console.write_output_line(framebuffer, "REQUEST SENSE failed, see UART log");
        return;
    };
    let sense_key = sense[2] & 0x0F;
    let mut line = Line::new();
    line.push_str("sense key: 0x");
    line.push_hex(sense_key as u32, 1);
    line.push_str(" asc: 0x");
    line.push_hex(sense[12] as u32, 2);
    line.push_str(" ascq: 0x");
    line.push_hex(sense[13] as u32, 2);
    // Sense key 7 is DATA PROTECT: the medium is write protected, which is a
    // property of the device rather than a fault in this firmware.
    if sense_key == 0x07 {
        line.push_str(" (write protected)");
    }
    console.write_output_line(framebuffer, line.as_str());
}

/// Stops a storage command before it spends time in a BOT session already
/// known to be dead. Recovery is explicit because a full bus reset would
/// also interrupt healthy HID devices.
fn require_live_usb_msc(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    mass_storage: &usb::UsbMassStorage,
) -> bool {
    if !mass_storage.needs_reinit() {
        return true;
    }
    console.write_output_line(
        framebuffer,
        "MSC session unusable; run 'usbrescan' before another storage command",
    );
    false
}

/// `docs/plans/archive/USB_MSC_PLAN.md` Stage 6, extended by `docs/plans/archive/USB_REFACTOR_PLAN.md` Stage F:
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
    if !require_live_usb_msc(console, framebuffer, mass_storage) {
        return;
    }

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

/// `docs/plans/archive/USB_HOST_PLAN.md` Stage 4-2/4-3, generalized by `docs/plans/archive/USB_REFACTOR_PLAN.md`
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

    let mut line = Line::new();
    line.push_str("cache writebacks refused over DMA buffers: ");
    line.push_u32(usb::cache_refusal_count());
    console.write_output_line(framebuffer, line.as_str());
    console.write_output_line(
        framebuffer,
        "  -> not zero means the controller read stale RAM for that many transfers",
    );

    write_bot_baseline(console, framebuffer, usb_host);

    let irq = usb::interrupt_diagnostics();
    let mut line = Line::new();
    line.push_str("USB IRQ source=");
    line.push_u32(irq.source);
    line.push_str(" global=");
    line.push_u32(if irq.global_signal_enabled { 1 } else { 0 });
    line.push_str(" total=");
    line.push_u32(irq.total);
    line.push_str(" ch0=");
    line.push_u32(irq.channel0);
    line.push_str(" ch1=");
    line.push_u32(irq.channel1);
    line.push_str(" port=");
    line.push_u32(irq.port);
    line.push_str(" spurious=");
    line.push_u32(irq.spurious);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("IRQ waits: sleep=");
    line.push_u32(irq.sleep_waits);
    line.push_str(" poll=");
    line.push_u32(irq.poll_waits);
    line.push_str(" wfi=");
    line.push_u32(irq.wfi_count);
    line.push_str(" last/max cycles=");
    line.push_u32(irq.last_wait_cycles);
    line.push_str("/");
    line.push_u32(irq.max_wait_cycles);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("IRQ slots: submit=");
    line.push_u32(irq.submits);
    line.push_str(" reap=");
    line.push_u32(irq.reaps);
    line.push_str(" cancel=");
    line.push_u32(irq.cancels);
    line.push_str(" stale-token=");
    line.push_u32(irq.stale_tokens);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("IRQ periodic: channels=0x");
    line.push_hex(irq.periodic_channel_mask, 2);
    line.push_str(" irqs=");
    line.push_u32(irq.periodic_interrupts);
    line.push_str(" pending=0x");
    line.push_hex(irq.periodic_pending_mask, 2);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("IRQ periodic work: complete=");
    line.push_u32(irq.periodic_completions);
    line.push_str(" rearm=");
    line.push_u32(irq.periodic_rearms);
    line.push_str(" errors=");
    line.push_u32(irq.periodic_errors);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("IRQ split: packets=");
    line.push_u32(irq.split_packets);
    line.push_str(" rounds=");
    line.push_u32(irq.split_rounds);
    line.push_str(" conflicts=");
    line.push_u32(irq.split_mode_conflicts);
    line.push_str(" active=");
    line.push_u32(irq.split_mode_active as u32);
    console.write_output_line(framebuffer, line.as_str());

    // The port's change bits are cleared by the ISR, so this latched copy
    // is the only place a power event survives long enough to be asked
    // about after the fact.
    let mut line = Line::new();
    line.push_str("port events since bus came up: ");
    let history = usb::port_event_history();
    if history == 0 {
        line.push_str("none");
    } else {
        if usb::port_over_current_seen() {
            line.push_str("OVER-CURRENT ");
        }
        if usb::port_drop_seen() {
            line.push_str("device-dropped ");
        }
        line.push_str("(HPRT bits 0x");
        line.push_hex(history, 4);
        line.push_str(")");
    }
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("IRQ masks: GINT=0x");
    line.push_hex(irq.live_gintmsk, 8);
    line.push_str(" HAINT=0x");
    line.push_hex(irq.live_haintmsk, 8);
    line.push_str(" HCINT0=0x");
    line.push_hex(irq.live_hcintmsk0, 8);
    line.push_str(" HCINT1=0x");
    line.push_hex(irq.live_hcintmsk1, 8);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("IRQ last: GINT=0x");
    line.push_hex(irq.last_gintsts, 8);
    line.push_str(" HAINT=0x");
    line.push_hex(irq.last_haint, 8);
    line.push_str(" HCINT0=0x");
    line.push_hex(irq.last_hcint0, 8);
    line.push_str(" HCINT1=0x");
    line.push_hex(irq.last_hcint1, 8);
    line.push_str(" HPRT=0x");
    line.push_hex(irq.last_hprt, 8);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("IRQ pending: ch0=0x");
    line.push_hex(irq.pending_channel0, 8);
    line.push_str(" ch1=0x");
    line.push_hex(irq.pending_channel1, 8);
    line.push_str(" port=0x");
    line.push_hex(irq.pending_port, 8);
    line.push_str(" unknown-cause=0x");
    line.push_hex(interrupts::unknown_external_cause(), 8);
    line.push_str(" count=");
    line.push_u32(interrupts::unknown_external_count());
    console.write_output_line(framebuffer, line.as_str());

    // Mirror the interrupt snapshot to UART so Stage 1 hardware results can
    // be pasted verbatim into the implementation record. This is foreground
    // diagnostics; the ISR itself never logs.
    uart::log_hex(b"USB IRQ: source=", irq.source);
    uart::log_hex(
        b"USB IRQ: global enabled=",
        irq.global_signal_enabled as u32,
    );
    uart::log_hex(b"USB IRQ: total=", irq.total);
    uart::log_hex(b"USB IRQ: channel0=", irq.channel0);
    uart::log_hex(b"USB IRQ: channel1=", irq.channel1);
    uart::log_hex(b"USB IRQ: port=", irq.port);
    uart::log_hex(b"USB IRQ: spurious=", irq.spurious);
    uart::log_hex(b"USB IRQ: sleep waits=", irq.sleep_waits);
    uart::log_hex(b"USB IRQ: poll waits=", irq.poll_waits);
    uart::log_hex(b"USB IRQ: wfi=", irq.wfi_count);
    uart::log_hex(b"USB IRQ: last wait cycles=", irq.last_wait_cycles);
    uart::log_hex(b"USB IRQ: max wait cycles=", irq.max_wait_cycles);
    uart::log_hex(b"USB IRQ: submits=", irq.submits);
    uart::log_hex(b"USB IRQ: reaps=", irq.reaps);
    uart::log_hex(b"USB IRQ: cancels=", irq.cancels);
    uart::log_hex(b"USB IRQ: stale tokens=", irq.stale_tokens);
    uart::log_hex(b"USB IRQ: periodic active=", irq.periodic_active as u32);
    uart::log_hex(
        b"USB IRQ: periodic channel mask=",
        irq.periodic_channel_mask,
    );
    uart::log_hex(b"USB IRQ: periodic interrupts=", irq.periodic_interrupts);
    uart::log_hex(
        b"USB IRQ: periodic pending mask=",
        irq.periodic_pending_mask,
    );
    uart::log_hex(b"USB IRQ: periodic completions=", irq.periodic_completions);
    uart::log_hex(b"USB IRQ: periodic rearms=", irq.periodic_rearms);
    uart::log_hex(b"USB IRQ: periodic errors=", irq.periodic_errors);
    uart::log_hex(b"USB IRQ: split packets=", irq.split_packets);
    uart::log_hex(b"USB IRQ: split rounds=", irq.split_rounds);
    uart::log_hex(b"USB IRQ: split mode conflicts=", irq.split_mode_conflicts);
    uart::log_hex(b"USB IRQ: split mode active=", irq.split_mode_active as u32);
    uart::log_hex(b"USB IRQ: channel2=", irq.periodic_irq_counts[1]);
    uart::log_hex(b"USB IRQ: channel3=", irq.periodic_irq_counts[2]);
    uart::log_hex(b"USB IRQ: channel4=", irq.periodic_irq_counts[3]);
    uart::log_hex(b"USB IRQ: GINTMSK=", irq.live_gintmsk);
    uart::log_hex(b"USB IRQ: HAINTMSK=", irq.live_haintmsk);
    uart::log_hex(b"USB IRQ: HCINTMSK0=", irq.live_hcintmsk0);
    uart::log_hex(b"USB IRQ: HCINTMSK1=", irq.live_hcintmsk1);
    uart::log_hex(b"USB IRQ: HCINTMSK2=", irq.periodic_hcintmsk[1]);
    uart::log_hex(b"USB IRQ: HCINTMSK3=", irq.periodic_hcintmsk[2]);
    uart::log_hex(b"USB IRQ: HCINTMSK4=", irq.periodic_hcintmsk[3]);
    uart::log_hex(b"USB IRQ: last GINTSTS=", irq.last_gintsts);
    uart::log_hex(b"USB IRQ: last HAINT=", irq.last_haint);
    uart::log_hex(b"USB IRQ: last HCINT0=", irq.last_hcint0);
    uart::log_hex(b"USB IRQ: last HCINT1=", irq.last_hcint1);
    uart::log_hex(b"USB IRQ: last HCINT2=", irq.periodic_last_hcint[1]);
    uart::log_hex(b"USB IRQ: last HCINT3=", irq.periodic_last_hcint[2]);
    uart::log_hex(b"USB IRQ: last HCINT4=", irq.periodic_last_hcint[3]);
    uart::log_hex(b"USB IRQ: last HPRT=", irq.last_hprt);
    uart::log_hex(b"USB IRQ: pending channel0=", irq.pending_channel0);
    uart::log_hex(b"USB IRQ: pending channel1=", irq.pending_channel1);
    uart::log_hex(b"USB IRQ: pending channel2=", irq.periodic_pending[1]);
    uart::log_hex(b"USB IRQ: pending channel3=", irq.periodic_pending[2]);
    uart::log_hex(b"USB IRQ: pending channel4=", irq.periodic_pending[3]);
    uart::log_hex(b"USB IRQ: pending port=", irq.pending_port);
    uart::log_hex(
        b"USB IRQ: unknown cause=",
        interrupts::unknown_external_cause(),
    );
    uart::log_hex(
        b"USB IRQ: unknown count=",
        interrupts::unknown_external_count(),
    );
}

/// The Stage 0 baseline counters of `docs/plans/archive/USB_BOT_HCD_REFACTOR_PLAN.md`, in
/// a fixed line format so two runs can be diffed rather than read.
///
/// Every field is printed even when zero. A block that hides its zeroes
/// cannot be compared line by line, and "the counter is missing" and "the
/// counter is zero" are exactly the two states this has to keep apart.
///
/// The `BOT:` lines are controller-wide and survive re-enumeration; the
/// `MSC:` lines belong to the attached device's current session and start
/// again when that session is rebuilt.
fn write_bot_baseline(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &usb::UsbHost,
) {
    write_bot_host_baseline(console, framebuffer, &usb::host_observation());
    let Some(storage) = usb_host.mass_storage() else {
        let mut line = Line::new();
        line.push_str("MSC: no mass storage attached, session counters unavailable");
        console.write_output_line(framebuffer, line.as_str());
        return;
    };
    write_bot_session_baseline(
        console,
        framebuffer,
        &storage.transport_observation(),
        storage.read_retry_count(),
    );
}

/// The controller-wide half: these counters survive re-enumeration.
///
/// Every line is kept short enough that four-digit counts still fit the
/// console's 80-column line. The first baseline run lost the last column of
/// three lines off the end, which is the one failure mode a fixed-format
/// counter block must not have -- so the breakdowns use short column codes
/// (`TransferLabel::short_name`) and are split across two lines rather than
/// packed into one.
fn write_bot_host_baseline(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    host: &usb::HostObservation,
) {
    let labels = usb::transfer_label_names();
    let sites = usb::cache_site_names();
    let directions = usb::cache_direction_names();
    let kinds = usb::packet_failure_kind_names();

    // `Line` holds 80 bytes and truncates past that, so each breakdown is
    // split into groups that still fit when every count is five digits.
    // The positions are the order of the name tables above: none, control,
    // CBW, data IN, data OUT, CSW, interrupt IN for the phases; channel-0
    // QTD, channel-0 payload, split staging, periodic, frame list, probe
    // for the sites. Grouping is by what the counts mean, so a nonzero one
    // is read beside the counters it should be compared against.
    const ENVELOPE_PHASES: [usize; 4] = [0, 1, 2, 5];
    const DATA_PHASES: [usize; 3] = [3, 4, 6];
    const CHANNEL0_SITES: [usize; 3] = [0, 1, 2];
    const PERIODIC_SITES: [usize; 3] = [3, 4, 5];

    let mut line = Line::new();
    line.push_str("BOT: cache-refusals=");
    line.push_u32(host.cache_refusals);
    for (index, name) in directions.iter().enumerate() {
        line.push_str(" ");
        line.push_str(name);
        line.push_str("=");
        line.push_u32(host.cache_refusals_by_direction[index]);
    }
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("BOT: refusal envelope");
    for index in ENVELOPE_PHASES {
        line.push_str(" ");
        line.push_str(labels[index]);
        line.push_str("=");
        line.push_u32(host.cache_refusals_by_label[index]);
    }
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("BOT: refusal data");
    for index in DATA_PHASES {
        line.push_str(" ");
        line.push_str(labels[index]);
        line.push_str("=");
        line.push_u32(host.cache_refusals_by_label[index]);
    }
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("BOT: refusal ch0");
    for index in CHANNEL0_SITES {
        line.push_str(" ");
        line.push_str(sites[index]);
        line.push_str("=");
        line.push_u32(host.cache_refusals_by_site[index]);
    }
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("BOT: refusal periodic");
    for index in PERIODIC_SITES {
        line.push_str(" ");
        line.push_str(sites[index]);
        line.push_str("=");
        line.push_u32(host.cache_refusals_by_site[index]);
    }
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("BOT: refusal last=0x");
    line.push_hex(host.last_cache_refusal_address, 8);
    line.push_str("/");
    line.push_u32(host.last_cache_refusal_length);
    line.push_str(" phase=");
    line.push_str(host.last_cache_refusal_label.name());
    console.write_output_line(framebuffer, line.as_str());

    // Position 0 of the kind table is the "no failure" placeholder, which
    // is never counted.
    let mut line = Line::new();
    line.push_str("BOT: pkt-fail=");
    line.push_u32(host.packet_failures);
    for (index, name) in kinds.iter().enumerate().take(4).skip(1) {
        line.push_str(" ");
        line.push_str(name);
        line.push_str("=");
        line.push_u32(host.packet_failures_by_kind[index]);
    }
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("BOT: pkt-fail");
    for (index, name) in kinds.iter().enumerate().skip(4) {
        line.push_str(" ");
        line.push_str(name);
        line.push_str("=");
        line.push_u32(host.packet_failures_by_kind[index]);
    }
    console.write_output_line(framebuffer, line.as_str());

    // On its own line, and never added to the failure total: an idle
    // keyboard produces thousands of expired polls on a bus where nothing
    // is wrong, and the first baseline run buried two real failures under
    // 1131 of them.
    let mut line = Line::new();
    line.push_str("BOT: idle-poll=");
    line.push_u32(host.idle_poll_timeouts);
    line.push_str(" (idle Interrupt IN, not failures)");
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("BOT: last-fail=");
    line.push_str(host.last_packet_failure_kind.name());
    line.push_str(" phase=");
    line.push_str(host.last_packet_failure_label.name());
    line.push_str(" in=");
    line.push_u32(u32::from(host.last_packet_failure_is_in));
    line.push_str(" req=");
    line.push_u32(host.last_packet_failure_requested);
    line.push_str(" act=");
    line.push_u32(host.last_packet_failure_actual);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("BOT: last-fail HCINT=0x");
    line.push_hex(host.last_packet_failure_hcint, 8);
    line.push_str(" QTD=0x");
    line.push_hex(host.last_packet_failure_qtd, 8);
    console.write_output_line(framebuffer, line.as_str());

    let fifos = usb::fifo_names();
    for (heading, counts) in [
        ("BOT: fifo-flush", &host.fifo_flushes),
        ("BOT: fifo-timeout", &host.fifo_flush_timeouts),
        ("BOT: fifo-skipped", &host.fifo_flushes_skipped_for_periodic),
    ] {
        let mut line = Line::new();
        line.push_str(heading);
        for (index, name) in fifos.iter().enumerate() {
            line.push_str(" ");
            line.push_str(name);
            line.push_str("=");
            line.push_u32(counts[index]);
        }
        console.write_output_line(framebuffer, line.as_str());
    }

    let mut line = Line::new();
    line.push_str("BOT: packet-cleanup out-nptx=");
    line.push_u32(host.out_packet_error_nptx_cleanups);
    console.write_output_line(framebuffer, line.as_str());
}

/// The session half: these belong to the attached device's current BOT
/// session and start again when that session is rebuilt.
fn write_bot_session_baseline(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    transport: &usb::TransportObservation,
    command_retries: u32,
) {
    let mut line = Line::new();
    line.push_str("MSC: cmd-retry=");
    line.push_u32(command_retries);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("MSC: pkt-retry err=");
    line.push_u32(transport.packet_error_retries);
    line.push_str(" timeout=");
    line.push_u32(transport.timeout_retries);
    line.push_str(" progressed=");
    line.push_u32(transport.retries_after_progress);
    line.push_str(" bytes=");
    line.push_u32(transport.retry_progress_bytes);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("MSC: commands=");
    line.push_u32(transport.commands_started);
    line.push_str(" cleanup-failed=");
    line.push_u32(transport.cleanup_failures);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("MSC: recovery=");
    line.push_u32(transport.reset_recoveries);
    line.push_str(" failed=");
    line.push_u32(transport.reset_recovery_failures);
    line.push_str(" csw short=");
    line.push_u32(transport.csw_short);
    line.push_str(" sig=");
    line.push_u32(transport.csw_bad_signature);
    line.push_str(" tag=");
    line.push_u32(transport.csw_tag_mismatch);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("MSC: csw phase=");
    line.push_u32(transport.csw_phase_error);
    line.push_str(" status=");
    line.push_u32(transport.csw_invalid_status);
    line.push_str(" residue=");
    line.push_u32(transport.csw_residue_mismatch);
    line.push_str(" early=");
    line.push_u32(transport.csw_early);
    console.write_output_line(framebuffer, line.as_str());
}

fn cmd_usbperiodic(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &mut usb::UsbHost,
) {
    console.write_output_line(
        framebuffer,
        "periodic probe armed: press/release a HID key or move the mouse within 5 seconds",
    );
    let Some((kind, result)) = usb_host.probe_periodic_hid() else {
        console.write_output_line(framebuffer, "no HID keyboard or mouse attached");
        return;
    };

    let mut line = Line::new();
    line.push_str("periodic HID=");
    line.push_str(kind);
    line.push_str(" requested interval=");
    line.push_u32(result.requested_interval as u32);
    line.push_str(" scheduled=");
    line.push_u32(result.scheduled_interval as u32);
    line.push_str(" entries=");
    line.push_u32(result.scheduled_entries as u32);
    console.write_output_line(framebuffer, line.as_str());

    if !result.attempted {
        console.write_output_line(
            framebuffer,
            "periodic probe unsupported for this route or max packet size",
        );
        return;
    }

    let mut line = Line::new();
    line.push_str("frame list addr/readback=0x");
    line.push_hex(result.frame_list_address, 8);
    line.push_str("/0x");
    line.push_hex(result.frame_list_readback, 8);
    line.push_str(" HCFG=0x");
    line.push_hex(result.hcfg_during, 8);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("periodic result=");
    line.push_str(if result.completed {
        "complete"
    } else if result.timed_out {
        "timeout"
    } else {
        "error"
    });
    line.push_str(" halted=");
    line.push_u32(result.channel_halted as u32);
    line.push_str(" ch1-irqs=");
    line.push_u32(result.channel1_irqs);
    line.push_str(" wfi=");
    line.push_u32(result.wfi_count);
    line.push_str(" bytes=");
    line.push_u32(result.transferred as u32);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("periodic HCINT=0x");
    line.push_hex(result.hcint, 8);
    line.push_str(" QTD=0x");
    line.push_hex(result.qtd_control, 8);
    console.write_output_line(framebuffer, line.as_str());
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
                usb::DeviceKind::Mouse(_) => "mouse",
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
    const USAGE: &str = "usage: ppafill <x> <y> <w> <h> <color> [cpu] | ppafill sweep";
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
        framebuffer.diagnostic_fill_rect_with_cpu(x, y, width, height, color)
            && framebuffer.flush_rect(x, y, width, height)
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
            if !framebuffer.diagnostic_fill_rect_with_cpu(0, 0, width, height, color)
                || !framebuffer.flush_rect(0, 0, width, height)
            {
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

/// Splits the next argument off `input`, honouring double quotes, and
/// returns it with whatever follows.
///
/// Quoting exists because file names contain spaces. FAT long names have
/// always allowed them, and once a command takes two paths there is no
/// position-based rule that can tell where the first one ends -- so the
/// quote has to be the thing that says.
///
/// There are no escape sequences inside a quoted argument. A closing quote
/// ends it, full stop. Nothing is lost by that: `fs::path` rejects `"` in a
/// path component, so a file name can never contain the character that would
/// need escaping.
///
/// `None` means a quote was opened and never closed, which is reported
/// rather than guessed at -- the alternative is silently treating the rest
/// of the line as one argument, which is exactly wrong if the user simply
/// forgot the other quote.
fn split_argument(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let input = trim(input);
    let Some(&b'"') = input.first() else {
        let (word, rest) = split_first_word(input);
        return Some((word, trim(rest)));
    };
    let rest = &input[1..];
    let end = rest.iter().position(|&byte| byte == b'"')?;
    Some((&rest[..end], trim(&rest[end + 1..])))
}

/// Reads exactly one argument, rejecting anything left over.
///
/// Trailing text is an error rather than something to ignore, because the
/// likely cause is an unquoted path with a space in it. Ignoring it would
/// send the truncated half to the filesystem and report that no such file
/// exists, which points at the wrong thing.
fn single_argument<'a>(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    input: &'a [u8],
    usage: &str,
) -> Option<&'a [u8]> {
    let Some((argument, rest)) = split_argument(input) else {
        console.write_output_line(framebuffer, "unterminated quote");
        return None;
    };
    if argument.is_empty() {
        console.write_output_line(framebuffer, usage);
        return None;
    }
    if !rest.is_empty() {
        console.write_output_line(
            framebuffer,
            "unexpected extra argument; quote paths containing spaces",
        );
        return None;
    }
    Some(argument)
}

/// Turns a path argument into the absolute path the VFS takes, resolving it
/// against the current directory when it does not start with `/`.
///
/// Every command that takes a path goes through this one place, right after
/// `split_argument`, so none of them has to know that a current directory
/// exists -- and so a relative path means the same thing to all of them.
/// `fs::path::join` folds `.` and `..` while it normalizes, so there is no
/// second resolution rule here for those.
fn absolute(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    state: &State,
    argument: &[u8],
) -> Option<Path> {
    match path::join(&state.cwd, as_str(argument)) {
        Ok(path) => Some(path),
        Err(error) => {
            console.write_output_line(framebuffer, path::error_name(error));
            None
        }
    }
}

/// Command-line bytes only ever hold what `Console::push` accepted (printable
/// ASCII or space), so this is always valid UTF-8; the empty fallback is
/// unreachable in practice but keeps this infallible.
fn as_str(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("")
}

/// Bytes one [`Line`] can hold.
///
/// It used to be 80, which silently cut any report line longer than that --
/// `df` on a large volume, `mounts` with a long path -- although the console
/// is `console::COLUMNS` cells wide and wraps whatever is longer. Several
/// rows' worth now, so an ordinary report line is never cut; the commands
/// that print unbounded text (`cat`) break at [`Line::is_full`] rather than
/// losing it. A line lives on the stack, and 512 bytes is small beside the
/// stack `memory.x` guarantees.
pub(crate) const LINE_BYTES: usize = 512;

const _: () = assert!(
    LINE_BYTES >= crate::console::COLUMNS,
    "a Line must hold at least one console row"
);

/// Small stack-allocated line builder, shared by every command's output
/// formatting: this crate avoids `core::fmt`, so output is assembled a few
/// pieces at a time instead. `pub(crate)` so that `mbr.rs` and the other
/// app modules format their own lines the same way.
///
/// Text past [`LINE_BYTES`] is dropped a whole character at a time, so what
/// is kept is always valid UTF-8 and never shown as an empty line.
pub(crate) struct Line {
    buffer: [u8; LINE_BYTES],
    len: usize,
}

impl Line {
    pub(crate) fn new() -> Self {
        Self {
            buffer: [0; LINE_BYTES],
            len: 0,
        }
    }

    /// Whether another byte would be dropped rather than appended. `cat`
    /// uses it to break a long line at the buffer's width instead of
    /// silently truncating what it prints.
    pub(crate) fn is_full(&self) -> bool {
        self.len >= self.buffer.len()
    }

    /// Appends one ASCII byte, or drops it when the line is full.
    fn push_byte(&mut self, byte: u8) {
        if self.len < self.buffer.len() {
            self.buffer[self.len] = byte;
            self.len += 1;
        }
    }

    /// Appends `text` up to the last whole character that fits.
    pub(crate) fn push_str(&mut self, text: &str) {
        for character in text.chars() {
            let width = character.len_utf8();
            if self.len + width > self.buffer.len() {
                break;
            }
            character.encode_utf8(&mut self.buffer[self.len..self.len + width]);
            self.len += width;
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
            self.push_byte(digit);
        }
    }

    /// The 64-bit form, for the LBAs and capacities the block layer carries.
    /// Kept separate from `push_u32` rather than replacing it: the 64-bit
    /// division this needs is a called routine on RV32, and most callers are
    /// printing counters that are 32-bit by nature.
    pub(crate) fn push_u64(&mut self, value: u64) {
        if value <= u32::MAX as u64 {
            self.push_u32(value as u32);
            return;
        }
        let mut digits = [0u8; 20];
        let mut count = 0;
        let mut remaining = value;
        while remaining > 0 {
            digits[count] = b'0' + (remaining % 10) as u8;
            remaining /= 10;
            count += 1;
        }
        for &digit in digits[..count].iter().rev() {
            self.push_byte(digit);
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
            self.push_byte(ch);
        }
    }

    pub(crate) fn push_hex(&mut self, value: u32, digits: u32) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for index in 0..digits {
            let nibble = (value >> (4 * (digits - 1 - index))) & 0xF;
            self.push_byte(HEX[nibble as usize]);
        }
    }

    /// Appends an unsigned value in hexadecimal without leading zeroes.
    pub(crate) fn push_u64_hex(&mut self, value: u64) {
        if value <= u32::MAX as u64 {
            self.push_hex(value as u32, 8);
            return;
        }
        let mut digits = [0u8; 16];
        let mut count = 0;
        let mut remaining = value;
        while remaining > 0 {
            digits[count] = b"0123456789ABCDEF"[(remaining & 0xF) as usize];
            remaining >>= 4;
            count += 1;
        }
        for &digit in digits[..count].iter().rev() {
            self.push_byte(digit);
        }
    }

    /// Always valid UTF-8: text is appended whole characters at a time and
    /// every other push writes ASCII.
    pub(crate) fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buffer[..self.len]).unwrap_or("")
    }
}
