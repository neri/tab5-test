//! Display for the VFS: `mounts`, `ls`, `cat`, and the commands that change
//! something -- `write`, `mkdir` and the current directory `cd` moves.
//!
//! `docs/FILESYSTEM_PLAN.md` Stage 2. These are the read-only shell the plan
//! asks for, and they are also how the reader gets tested: `ls /tmp` has to
//! show the long name rather than its `~1` alias, and `cat /tmp/CHAIN.TXT`
//! has to keep printing the right cluster number past the end of the first
//! one.
//!
//! It sits beside `blockdev.rs` in the same way the VFS sits above the block
//! layer -- that one shows what a medium is, this one shows what is on it.

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use super::shell::Line;
use crate::console::{COLUMNS, Console};
use crate::framebuffer::Framebuffer;
use crate::fs::path::Path;
use crate::fs::registry::{device_name, parse_device_name};
use crate::fs::vfs::{
    EntryKind, FsError, MountMode, OpenMode, Timestamp, Vfs, error_name, format_name, verdict_name,
};
use crate::fs::{DeviceId, Devices, mbr};
use crate::uart;

/// Bytes per `read` while printing a file.
///
/// Reads are answered by walking the file from its start, so a handle's
/// offset costs time proportional to itself (see `fs::vfs`). One console
/// line's worth per call would make printing a few kilobytes quadratic in a
/// way the user would feel, so this takes a whole cluster-ish chunk and
/// splits it into lines here.
const READ_CHUNK: usize = 512;

pub fn show_mounts(console: &mut Console, framebuffer: &mut Framebuffer, vfs: &Vfs) {
    let mut any = false;
    for mount in vfs.mounts() {
        any = true;
        let mut line = Line::new();
        line.push_str(mount.point.as_str());
        line.push_str("  ");
        let device = device_name(mount.volume.device);
        line.push_str(device.as_str());
        if let Some(partition) = mount.volume.partition {
            line.push_str("p");
            line.push_u32(partition as u32);
        }
        line.push_str("  ");
        line.push_str(format_name(mount.format));
        line.push_str(if mount.mode == MountMode::ReadOnly {
            "  ro  "
        } else {
            "  rw  "
        });
        line.push_u64(mount.range.block_count / 2048);
        line.push_str(" MiB at LBA ");
        line.push_u64(mount.range.start_lba);
        console.write_output_line(framebuffer, line.as_str());

        // The generation and what the identity rests on. Both matter when
        // deciding whether a mount can be trusted across a removal: a
        // fingerprint with nothing behind it but a capacity cannot tell two
        // same-sized cards apart, and saying so is more use than a digest
        // that looks equally authoritative either way.
        let mut line = Line::new();
        line.push_str("    gen ");
        line.push_u32(mount.volume.generation);
        line.push_str(", id ");
        line.push_hex((mount.fingerprint.digest >> 32) as u32, 8);
        line.push_hex(mount.fingerprint.digest as u32, 8);
        line.push_str(" from ");
        push_sources(&mut line, &mount.fingerprint.sources);
        console.write_output_line(framebuffer, line.as_str());
    }
    if !any {
        console.write_output_line(framebuffer, "nothing mounted");
    }
}

/// Names the identity sources that answered, or says that none did.
fn push_sources(line: &mut Line, sources: &crate::fs::fingerprint::Sources) {
    let mut any = false;
    let push = |line: &mut Line, name: &str, present: bool, any: &mut bool| {
        if !present {
            return;
        }
        if *any {
            line.push_str("+");
        }
        line.push_str(name);
        *any = true;
    };
    push(line, "cid", sources.sd_cid, &mut any);
    push(line, "inquiry", sources.inquiry, &mut any);
    push(line, "serial", sources.unit_serial, &mut any);
    push(line, "devid", sources.device_id, &mut any);
    push(line, "mbr", sources.disk_signature, &mut any);
    push(line, "boot", sources.boot_sector, &mut any);
    if !any {
        // Worth saying outright: a match on this fingerprint means only that
        // the medium is the same size as it was.
        line.push_str("size only");
    }
}

/// `fsverify`: re-checks every mount's medium against the identity it was
/// mounted with.
///
/// Explicit rather than automatic because gathering an identity costs
/// several bus commands per device -- not something to put on the way to
/// reading a file. What it is for is the moment after a card has been
/// swapped or a stick re-seated, when the question "is this still what I
/// mounted" is the one being asked.
pub fn verify(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
) {
    let mut any = false;
    vfs.verify(devices, |mount, verdict| {
        any = true;
        let mut line = Line::new();
        line.push_str(mount.point.as_str());
        line.push_str("  ");
        line.push_str(verdict_name(verdict));
        console.write_output_line(framebuffer, line.as_str());
    });
    if !any {
        console.write_output_line(framebuffer, "nothing mounted");
    }
}

/// What `ls` was asked to show.
#[derive(Clone, Copy, Default)]
pub struct ListOptions {
    /// `-l`: one entry per line with kind, size and timestamp, instead of
    /// names packed into columns.
    pub long: bool,
    /// `-a`: include the entries whose names start with a dot, which on FAT
    /// means the `.` and `..` every subdirectory carries.
    pub all: bool,
}

/// One entry, held long enough to be sorted.
///
/// The name is owned. `Vfs::list` lends each name out of a directory buffer
/// that only exists for the duration of the call -- which is what lets it
/// avoid an allocation per listing -- so anything that has to outlive the
/// walk, as sorting requires, must copy.
struct Entry {
    name: String,
    kind: EntryKind,
    size: u64,
    modified: Option<Timestamp>,
}

/// `ls [-l] [-a] [<path>]`. The shell has already resolved the path against
/// the current directory, and passes that one when the command was given
/// none.
///
/// Every entry is collected before any is printed. That is the cost of
/// sorting: a directory arrives in whatever order it sits on the medium,
/// and there is no way to put it in name order while streaming it. The
/// buffer is on the PSRAM heap and holds one `String` per entry, which at
/// the sizes a FAT directory reaches is not worth avoiding.
pub fn list(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &Vfs,
    path: &str,
    options: ListOptions,
) {
    let mut entries: Vec<Entry> = Vec::new();
    let outcome = vfs.list(devices, path, |entry| {
        // FAT gives every subdirectory a `.` and a `..`; hiding them by
        // default is what makes a listing here look like a listing anywhere
        // else. Nothing is lost -- `-a` shows them, and they are the only
        // dotted names these volumes produce.
        if !options.all && entry.name.starts_with('.') {
            return;
        }
        entries.push(Entry {
            name: String::from(entry.name),
            kind: entry.kind,
            size: entry.size,
            modified: entry.modified,
        });
    });
    if let Err(error) = outcome {
        return report(console, framebuffer, "ls", error);
    }

    // Sorted by name, mount points and directories included. A listing that
    // sorted a volume but left the tree above it in mount order would be
    // two different commands wearing one name.
    entries.sort_by(|left, right| compare_names(&left.name, &right.name));

    if options.long {
        for entry in &entries {
            console.write_output_line(framebuffer, format_entry(entry).as_str());
        }
        let mut line = Line::new();
        line.push_u32(entries.len() as u32);
        line.push_str(if entries.len() == 1 {
            " entry"
        } else {
            " entries"
        });
        console.write_output_line(framebuffer, line.as_str());
    } else {
        write_columns(console, framebuffer, &entries);
    }
}

/// Orders two names the way a listing should read.
///
/// ASCII case is folded first, so `README.TXT` and `readme.txt` sort next to
/// each other rather than in two blocks either side of the lower-case
/// letters -- which is what a raw byte comparison gives, and which reads as
/// unsorted to anyone looking for a name. Case then breaks the tie, so the
/// order is total and two listings of the same directory agree.
fn compare_names(left: &str, right: &str) -> Ordering {
    left.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()))
        .then_with(|| left.cmp(right))
}

/// Gap between one column and the next.
const COLUMN_GAP: usize = 2;

/// Packs names into as many columns as the console is wide enough for.
///
/// Two rules here are `ls`'s, and following them is what makes the output
/// look like a listing rather than an approximation of one:
///
/// - Each column is as wide as the longest name *in that column*, not as the
///   longest name anywhere. One global width lets a single long name push
///   every column apart.
/// - Names fill down each column before moving to the next. With the names
///   in name order, reading a column top to bottom keeps neighbours
///   together; filling across rows would scatter them along a row instead.
///
/// A line must stay strictly inside the console's width. One that ends
/// exactly on the last cell wraps and then ends, which leaves a blank row
/// behind it -- see `Console::write_output_line`.
fn write_columns(console: &mut Console, framebuffer: &mut Framebuffer, entries: &[Entry]) {
    if entries.is_empty() {
        return;
    }
    // The widest layout that fits, tried from the most columns down. There
    // is always a fit at one column: a name too long for the line is left to
    // the console to wrap rather than being dropped.
    let mut chosen = (1usize, entries.len());
    for columns in (2..=entries.len().min(COLUMNS)).rev() {
        let rows = entries.len().div_ceil(columns);
        // With this many rows the last column would be empty, so the layout
        // is not really the column count it claims to be.
        if (columns - 1) * rows >= entries.len() {
            continue;
        }
        let mut total = 0;
        for column in 0..columns {
            total += column_width(entries, column, rows)
                + if column + 1 < columns { COLUMN_GAP } else { 0 };
            if total >= COLUMNS {
                break;
            }
        }
        if total < COLUMNS {
            chosen = (columns, rows);
            break;
        }
    }
    let (columns, rows) = chosen;

    let mut line = String::with_capacity(COLUMNS);
    for row in 0..rows {
        line.clear();
        let mut start = 0;
        for column in 0..columns {
            let Some(entry) = entries.get(column * rows + row) else {
                continue;
            };
            // Padded up to where the column starts rather than after each
            // name, so the line carries no trailing spaces to be painted
            // and mirrored to the log.
            for _ in line.chars().count()..start {
                line.push(' ');
            }
            line.push_str(&entry.name);
            start += column_width(entries, column, rows) + COLUMN_GAP;
        }
        console.write_output_line(framebuffer, &line);
    }
}

/// The longest name in one column of a `rows`-deep column-major layout.
fn column_width(entries: &[Entry], column: usize, rows: usize) -> usize {
    entries[column * rows..]
        .iter()
        .take(rows)
        .map(|entry| entry.name.chars().count())
        .max()
        .unwrap_or(0)
}

/// One `-l` line: kind, size for a file, timestamp, name.
///
/// A `String` rather than a `Line`, which holds 80 bytes: a long FAT name
/// plus the columns in front of it goes past that, and a listing that
/// silently shortened a name would be worse than one that wraps.
fn format_entry(entry: &Entry) -> String {
    let mut line = String::new();
    line.push_str(match entry.kind {
        // A mount point is marked apart from a directory because it is not
        // one: it belongs to the tree rather than to any volume, and `ls` of
        // the root is showing the mount table, not a filesystem.
        EntryKind::MountPoint => "mount ",
        EntryKind::Directory => "dir   ",
        EntryKind::File => "file  ",
    });
    if entry.kind == EntryKind::File {
        push_u64(&mut line, entry.size);
        line.push(' ');
    }
    match entry.modified {
        Some(stamp) => {
            push_u64(&mut line, stamp.year as u64);
            push_two_digits(&mut line, '-', stamp.month);
            push_two_digits(&mut line, '-', stamp.day);
            push_two_digits(&mut line, ' ', stamp.hour);
            push_two_digits(&mut line, ':', stamp.minute);
            push_two_digits(&mut line, ':', stamp.second);
        }
        // Aligned with a dated line, so a column of entries stays readable
        // when only some of them carry a timestamp.
        None => line.push_str("       (no time)   "),
    }
    line.push(' ');
    line.push_str(&entry.name);
    line
}

/// Appends a decimal number. `core::fmt` is deliberately not linked here, so
/// the digits are produced by hand as they are for [`Line`].
fn push_u64(line: &mut String, value: u64) {
    if value == 0 {
        line.push('0');
        return;
    }
    let mut digits = [0u8; 20];
    let mut length = 0;
    let mut value = value;
    while value > 0 {
        digits[length] = b'0' + (value % 10) as u8;
        value /= 10;
        length += 1;
    }
    for &digit in digits[..length].iter().rev() {
        line.push(digit as char);
    }
}

/// Pushes a separator and a zero-padded two-digit field.
fn push_two_digits(line: &mut String, separator: char, value: u8) {
    line.push(separator);
    if value < 10 {
        line.push('0');
    }
    push_u64(line, value as u64);
}

/// `cat <path> [offset]`: prints a file from `offset`, splitting it into
/// console lines.
///
/// The content is untrusted: it is whatever is on the medium. Bytes that are
/// not printable are shown as `.` rather than sent to the console, which
/// would otherwise interpret control characters as its own.
///
/// The byte count is compared against the size in the directory entry, and a
/// mismatch is called out. A reader that loses a cluster chain part way
/// stops early and otherwise prints a perfectly plausible prefix; the
/// directory's own size is the thing that says it should not have.
pub fn concatenate(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    path: &str,
    offset: u64,
) {
    let handle = match vfs.open(devices, path, OpenMode::Read) {
        Ok(handle) => handle,
        Err(error) => return report(console, framebuffer, "cat", error),
    };
    let size = vfs.size(&handle).unwrap_or(0);
    if offset > 0 && vfs.seek(&handle, offset).is_err() {
        vfs.close(handle);
        return console.write_output_line(framebuffer, "cat: seek failed");
    }

    let mut line = Line::new();
    let mut buffer = [0u8; READ_CHUNK];
    let mut total = 0u64;
    loop {
        let count = match vfs.read(devices, &handle, &mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => {
                if !line.as_str().is_empty() {
                    console.write_output_line(framebuffer, line.as_str());
                }
                report(console, framebuffer, "cat", error);
                vfs.close(handle);
                return;
            }
        };
        total += count as u64;
        for &byte in &buffer[..count] {
            // A line ends at a newline or when the console's width is used
            // up; a carriage return is dropped so CRLF does not leave an
            // empty line between every pair.
            if byte == b'\n' || line.is_full() {
                console.write_output_line(framebuffer, line.as_str());
                line = Line::new();
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\r' {
                continue;
            }
            line.push_ascii(&[byte]);
        }
    }
    if !line.as_str().is_empty() {
        console.write_output_line(framebuffer, line.as_str());
    }
    vfs.close(handle);

    let mut summary = Line::new();
    summary.push_str("(");
    summary.push_u64(total);
    summary.push_str(" of ");
    summary.push_u64(size.saturating_sub(offset));
    summary.push_str(" bytes");
    if total != size.saturating_sub(offset) {
        summary.push_str(" -- SHORT READ");
    }
    summary.push_str(")");
    console.write_output_line(framebuffer, summary.as_str());
}

/// `write <path> <text>` and `append <path> <text>`.
///
/// A shell needs some way to put bytes on a volume, and this is the smallest
/// one that exercises the whole path: create an entry, allocate clusters,
/// write, commit the size and the timestamp. It is also the only way to see
/// the read-only policy refuse anything, since every other command here
/// reads.
pub fn write(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    path: &str,
    text: &str,
    mode: OpenMode,
) {
    let handle = match vfs.open(devices, path, mode) {
        Ok(handle) => handle,
        Err(error) => return report(console, framebuffer, "write", error),
    };

    // A trailing newline, so a file built up by repeated `append` reads back
    // as lines rather than as one run-together string.
    let mut written = 0usize;
    let mut outcome = Ok(());
    for chunk in [text.as_bytes(), b"\r\n"] {
        let mut offset = 0usize;
        while offset < chunk.len() {
            match vfs.write(devices, &handle, &chunk[offset..]) {
                Ok(0) => {
                    outcome = Err(FsError::NoSpace);
                    break;
                }
                Ok(count) => {
                    offset += count;
                    written += count;
                }
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            }
        }
        if outcome.is_err() {
            break;
        }
    }
    vfs.close(handle);

    match outcome {
        Err(error) => report(console, framebuffer, "write", error),
        Ok(()) => {
            let mut line = Line::new();
            line.push_str("wrote ");
            line.push_u64(written as u64);
            line.push_str(" bytes to ");
            line.push_str(path);
            console.write_output_line(framebuffer, line.as_str());
        }
    }
}

/// `cd [<path>]`: moves the shell's current directory, once the target has
/// been confirmed to be a directory.
///
/// The check is the whole point of doing this here rather than in the shell:
/// a current directory that has never been looked at would let `cd` succeed
/// on a typo and then fail every command afterwards, at which point the
/// message names the command instead of the mistake.
///
/// The VFS is not told about any of this. It has no current directory --
/// see `fs::path` -- so what moves is a `Path` the shell owns, and what the
/// VFS sees is still an absolute path.
pub fn change_directory(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &Vfs,
    cwd: &mut Path,
    target: Path,
) {
    match vfs.metadata(devices, target.as_str()) {
        Ok(metadata) if metadata.kind != EntryKind::Directory => {
            report(console, framebuffer, "cd", FsError::NotADirectory);
        }
        Ok(_) => *cwd = target,
        Err(error) => report(console, framebuffer, "cd", error),
    }
}

/// `mkdir <path>`.
pub fn make_directory(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    path: &str,
) {
    match vfs.create_dir(devices, path) {
        Ok(()) => {
            let mut line = Line::new();
            line.push_str("created ");
            line.push_str(path);
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => report(console, framebuffer, "mkdir", error),
    }
}

/// Resolves a `ram`/`sd0pN`/`usb0pN` name to something the VFS can mount.
///
/// The mount point is derived from the name rather than given: `sd0p1` always
/// lands on `/vol/sd0p1`. Letting a caller choose would mean the same volume
/// could be reached by two different paths depending on how it was mounted,
/// and nothing here needs that.
pub fn mount(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    name: &str,
) {
    let Some((device, partition)) = parse_volume_name(name) else {
        console.write_output_line(framebuffer, "usage: mount <ram|sd0pN|usbMpN>");
        return;
    };

    let range = match partition {
        None => {
            let Some(block_count) =
                devices.with_device(device, |block| block.geometry().block_count)
            else {
                console.write_output_line(framebuffer, "device not present");
                return;
            };
            crate::fs::PartitionRange {
                start_lba: 0,
                block_count,
            }
        }
        Some(number) => {
            // The partition table is read now rather than remembered from a
            // previous `devices`: the medium is the authority on its own
            // layout, and it may have been swapped since.
            let found = devices.with_device(device, |block| match mbr::inspect(block) {
                Ok(mbr::Layout::Mbr(table)) => table.partition(number),
                _ => None,
            });
            match found {
                None => {
                    console.write_output_line(framebuffer, "device not present");
                    return;
                }
                Some(None) => {
                    console
                        .write_output_line(framebuffer, "no such usable partition; run 'devices'");
                    return;
                }
                Some(Some(entry)) => crate::fs::PartitionRange::from_partition(&entry),
            }
        }
    };

    // Only the RAM disk is mounted read-write. SD and USB stay read-only in
    // every stage of this plan, and there is no option here to change that.
    let mode = if device == DeviceId::Ram {
        MountMode::ReadWrite
    } else {
        MountMode::ReadOnly
    };
    let point = mount_point(name, device);

    match vfs.mount(devices, point.as_str(), device, partition, range, mode) {
        Ok(()) => {
            let mut line = Line::new();
            line.push_str("mounted ");
            line.push_str(name);
            line.push_str(" on ");
            line.push_str(point.as_str());
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => report(console, framebuffer, "mount", error),
    }
}

pub fn unmount(console: &mut Console, framebuffer: &mut Framebuffer, vfs: &mut Vfs, path: &str) {
    match vfs.umount(path) {
        Ok(()) => {
            let mut line = Line::new();
            line.push_str("unmounted ");
            line.push_str(path);
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => report(console, framebuffer, "umount", error),
    }
}

/// `/tmp` for the RAM disk, `/vol/<name>` for removable media.
fn mount_point(name: &str, device: DeviceId) -> Line {
    let mut point = Line::new();
    if device == DeviceId::Ram {
        point.push_str("/tmp");
    } else {
        point.push_str("/vol/");
        point.push_str(name);
    }
    point
}

/// Splits `sd0p1` into a device and an entry number, or `ram` into a device
/// with no partition.
///
/// The device half is parsed by `fs::registry`, which is also what renders
/// these names, so what `devices` prints is exactly what `mount` accepts.
/// The split is on the *last* `p` because the device name itself can end in
/// a digit and a future device name might contain one.
fn parse_volume_name(name: &str) -> Option<(DeviceId, Option<u8>)> {
    if let Some(device) = parse_device_name(name) {
        // A bare device name, with no partition: only the RAM disk is
        // mounted that way, since every removable volume here is reached
        // through an MBR entry.
        return (device == DeviceId::Ram).then_some((device, None));
    }
    let split = name.rfind('p')?;
    let device = parse_device_name(&name[..split])?;
    let number: u8 = name[split + 1..].parse().ok()?;
    if !(1..=mbr::MAX_PARTITIONS as u8).contains(&number) {
        return None;
    }
    Some((device, Some(number)))
}

fn report(console: &mut Console, framebuffer: &mut Framebuffer, command: &str, error: FsError) {
    let mut line = Line::new();
    line.push_str(command);
    line.push_str(": ");
    line.push_str(error_name(error));
    console.write_output_line(framebuffer, line.as_str());
    uart::log(b"FS: command failed\r\n");
}
