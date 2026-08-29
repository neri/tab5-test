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
use crate::font;
use crate::framebuffer::Framebuffer;
use crate::fs::path::{self, Path, PathError};
use crate::fs::registry::{device_name, parse_device_name};
use crate::fs::vfs::{
    EntryKind, FileHandle, FsError, MountMode, MountRequest, OpenMode, Timestamp, Vfs, error_name,
    format_name, mode_name, verdict_name,
};
use crate::fs::{DeviceId, Devices, mbr};
use crate::tick;
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
            for _ in font::console::cell_count(&line)..start {
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
        .map(|entry| font::console::cell_count(&entry.name))
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
        // A mount point is marked apart from a directory because it is an
        // overlay from the mount table rather than an entry of the directory
        // being listed.
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

/// `fsopen` with no argument: what is being held open, and whether it still
/// reaches anything.
///
/// The `live`/`stale` column is the point of the command. A handle goes
/// stale the moment its volume leaves the mount table -- which is what the
/// automatic unmount does when a drive is pulled -- and nothing brings it
/// back, because putting the same stick in gives it a new generation.
pub fn show_open_files(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    vfs: &Vfs,
    held: &[Option<FileHandle>],
) {
    let mut any = false;
    for (slot, handle) in held.iter().enumerate() {
        let Some(handle) = handle else {
            continue;
        };
        let Ok(info) = vfs.describe(handle) else {
            continue;
        };
        any = true;
        let mut line = Line::new();
        line.push_u32(slot as u32);
        line.push_str("  ");
        line.push_str(if info.live { "live  " } else { "stale " });
        line.push_str(mode_name(info.mode));
        line.push_str("  gen ");
        line.push_u32(info.volume.generation);
        line.push_str("  ");
        line.push_u64(info.offset);
        line.push_str("/");
        line.push_u64(info.size);
        line.push_str("  ");
        line.push_str(info.point.as_str());
        // The handle keeps the path within its volume, so the tree path is
        // put back together here rather than stored twice.
        if !info.path.is_root() {
            if !info.point.is_root() {
                line.push_str("/");
            }
            line.push_str(info.path.as_str().trim_start_matches('/'));
        }
        console.write_output_line(framebuffer, line.as_str());
    }
    if !any {
        console.write_output_line(framebuffer, "no files held open");
    }
}

/// `fsopen <path>`: open a file and leave it open.
pub fn open_file(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    held: &mut [Option<FileHandle>],
    path: &str,
) {
    let Some(slot) = held.iter().position(Option::is_none) else {
        console.write_output_line(
            framebuffer,
            "fsopen: all slots are in use; fsclose one first",
        );
        return;
    };
    // Read-only on purpose. Holding a writable handle open across commands
    // would leave a `Truncate` half-applied for as long as the user left it
    // there, and this exists to watch a handle go stale, not to write.
    match vfs.open(devices, path, OpenMode::Read) {
        Ok(handle) => {
            let mut line = Line::new();
            line.push_str("held open as ");
            line.push_u32(slot as u32);
            line.push_str(": ");
            line.push_str(path);
            held[slot] = Some(handle);
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => report(console, framebuffer, "fsopen", error),
    }
}

/// `fsread <slot> [bytes]`: read through a held handle and say what happened.
///
/// It prints the outcome rather than the bytes. What this is for is seeing
/// whether the handle still works, and `cat` already prints file contents.
pub fn read_open_file(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    held: &mut [Option<FileHandle>],
    slot: usize,
    count: usize,
) {
    let Some(handle) = held.get(slot).and_then(Option::as_ref) else {
        console.write_output_line(framebuffer, "fsread: no file held in that slot");
        return;
    };
    let mut buffer = [0u8; READ_CHUNK];
    let count = count.min(buffer.len());
    match vfs.read(devices, handle, &mut buffer[..count]) {
        Ok(read) => {
            let mut line = Line::new();
            line.push_str("read ");
            line.push_u64(read as u64);
            line.push_str(" bytes, now at ");
            line.push_u64(vfs.describe(handle).map(|info| info.offset).unwrap_or(0));
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => report(console, framebuffer, "fsread", error),
    }
}

/// `fsclose <slot>`.
///
/// A stale handle still occupies a slot in the VFS's open-file table -- the
/// automatic unmount drops the mount without touching handles, exactly as
/// `fsverify` does -- so closing it is how the slot comes back.
pub fn close_open_file(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    vfs: &mut Vfs,
    held: &mut [Option<FileHandle>],
    slot: usize,
) {
    let Some(handle) = held.get_mut(slot).and_then(Option::take) else {
        console.write_output_line(framebuffer, "fsclose: no file held in that slot");
        return;
    };
    vfs.close(handle);
    let mut line = Line::new();
    line.push_str("closed ");
    line.push_u32(slot as u32);
    console.write_output_line(framebuffer, line.as_str());
}

/// Where a downloaded file is being put, and under what temporary name.
///
/// The transfer writes to `part` and only becomes `destination` once it has
/// finished. A file that appears under the name the user asked for is
/// therefore complete: the half-written state has a different name, and
/// nothing that reads `destination` can find it part-way through. Losing
/// power mid-transfer leaves the `.part` file rather than a truncated file
/// wearing the real name.
pub struct Download {
    pub destination: Path,
    pub part: Path,
}

/// Suffix for the in-progress name. Long enough to be obvious in a listing
/// if a failure ever leaves one behind.
const PART_SUFFIX: &str = ".part";

/// Works out where `remote` should land, given the shell's current
/// directory.
///
/// Only the last component of the remote name is used. A TFTP server names
/// files in its own namespace -- `pub/images/thing.bin` is common -- and
/// taking that as a path here would either need directories that do not
/// exist locally or would smuggle `..` into the destination.
pub fn download_to(cwd: &Path, remote: &[u8]) -> Result<Download, FsError> {
    download_named(cwd, remote)
}

/// Whether a remote name has a last component to make a filename out of.
///
/// An HTTP path often has none -- `/`, or a directory that ends in `/` --
/// and there is nothing to call the file then. TFTP names always have one.
pub fn names_a_file(remote: &[u8]) -> bool {
    !last_component(remote).is_empty()
}

fn last_component(remote: &[u8]) -> &[u8] {
    // A query string is not part of the name, and `?` is one of the
    // characters FAT reserves, so it would be refused later anyway.
    let remote = match remote.iter().position(|&byte| byte == b'?') {
        Some(cut) => &remote[..cut],
        None => remote,
    };
    match remote.iter().rposition(|&byte| byte == b'/') {
        Some(cut) => &remote[cut + 1..],
        None => remote,
    }
}

fn download_named(cwd: &Path, remote: &[u8]) -> Result<Download, FsError> {
    // The remote name came off a command line, so it is already printable
    // ASCII; anything else means the caller built it from somewhere else and
    // the name is not one this VFS can hold.
    let name = core::str::from_utf8(last_component(remote))
        .map_err(|_| FsError::Path(PathError::InvalidCharacter))?;
    if name.is_empty() {
        return Err(FsError::NotAFile);
    }
    let destination = path::join(cwd, name)?;
    let mut part_name = String::from(name);
    part_name.push_str(PART_SUFFIX);
    let part = path::join(cwd, &part_name)?;
    Ok(Download { destination, part })
}

/// Puts a finished download under its real name.
///
/// The existing file is removed first: `rename` refuses to land on a name
/// that is taken, and overwriting is what the caller asked for by naming a
/// destination that already exists. The window where neither name resolves
/// is why `Vfs::rename` does not do this itself -- here it is wanted, since
/// the alternative is a download that cannot be repeated.
pub fn commit_download(
    devices: &mut Devices,
    vfs: &mut Vfs,
    download: &Download,
) -> Result<(), FsError> {
    match vfs.remove_file(devices, download.destination.as_str()) {
        Ok(()) => {}
        // Nothing there to replace, which is the ordinary case.
        Err(FsError::NotFound) => {}
        Err(error) => return Err(error),
    }
    vfs.rename(
        devices,
        download.part.as_str(),
        download.destination.as_str(),
    )
}

/// Reports what a download left behind, and clears it up if it failed.
///
/// Called for both outcomes so that the `.part` file has exactly one place
/// it can be dealt with. A failure to remove it is worth a line of its own:
/// the file is still there, and the next attempt at the same name will
/// silently replace it, so saying nothing would leave the volume quietly
/// holding something the user never asked for.
pub fn finish_download(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    download: &Download,
    complete: bool,
) {
    if !complete {
        let mut line = Line::new();
        match vfs.remove_file(devices, download.part.as_str()) {
            Ok(()) => line.push_str("incomplete download discarded"),
            Err(error) => {
                line.push_str("incomplete download left at ");
                line.push_str(download.part.as_str());
                line.push_str(": ");
                line.push_str(error_name(error));
            }
        }
        console.write_output_line(framebuffer, line.as_str());
        return;
    }
    match commit_download(devices, vfs, download) {
        Ok(()) => {
            let mut line = Line::new();
            line.push_str("saved ");
            line.push_str(download.destination.as_str());
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => {
            let mut line = Line::new();
            line.push_str("saved as ");
            line.push_str(download.part.as_str());
            line.push_str(": could not rename: ");
            line.push_str(error_name(error));
            console.write_output_line(framebuffer, line.as_str());
        }
    }
}

/// `rm <path>` and `rmdir <path>`.
///
/// Two commands over one VFS call, each naming the kind it will take, so
/// that `rm` cannot quietly remove a directory. The library would delete
/// either, and an empty directory removed by a mistyped `rm` looks exactly
/// like one that was never there.
pub fn remove(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    path: &str,
    directory: bool,
) {
    let command = if directory { "rmdir" } else { "rm" };
    let outcome = if directory {
        vfs.remove_dir(devices, path)
    } else {
        vfs.remove_file(devices, path)
    };
    match outcome {
        Ok(()) => {
            let mut line = Line::new();
            line.push_str("removed ");
            line.push_str(path);
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => report(console, framebuffer, command, error),
    }
}

/// `mv <from> <to>`.
pub fn rename(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    from: &str,
    to: &str,
) {
    match vfs.rename(devices, from, to) {
        Ok(()) => {
            let mut line = Line::new();
            line.push_str("renamed ");
            line.push_str(from);
            line.push_str(" to ");
            line.push_str(to);
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => report(console, framebuffer, "mv", error),
    }
}

/// Bytes generated per sink call by [`fill`].
///
/// The size of one call is the variable the linearity check varies, so it is
/// a parameter of the command rather than a constant. This is only the
/// ceiling the stack buffer sets.
const FILL_MAX_CHUNK: usize = 4096;

/// `fill <path> <KiB> [chunk]`: writes a known pattern and reports how long
/// it took.
///
/// This is the measurement `docs/FILESYSTEM_WORKFLOW_PLAN.md` Stage 3-2 asks
/// for, kept independent of the network so that "is the write path linear?"
/// and "does the transfer work?" are two questions with two answers.
/// `repeated` takes the old path -- one `Vfs::write` per chunk, each of
/// which builds a `FileWriter` and walks the FAT chain to find the end --
/// so the two shapes can be compared on the same volume with the same
/// arguments.
pub fn fill(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    path: &str,
    kib: usize,
    chunk: usize,
    repeated: bool,
) {
    let chunk = chunk.clamp(1, FILL_MAX_CHUNK);
    let total = kib * 1024;
    // A counting pattern rather than a constant byte, so a file written by
    // one chunk size and read back can be told from one written by another.
    let mut buffer = [0u8; FILL_MAX_CHUNK];
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }

    let started = tick::now_ms();
    let outcome = if repeated {
        write_repeatedly(devices, vfs, path, &buffer[..chunk], total)
    } else {
        let mut remaining = total;
        let stream = vfs.write_stream(devices, path, OpenMode::Truncate, |sink| {
            while remaining > 0 {
                let take = chunk.min(remaining);
                if !sink(&buffer[..take]) {
                    return;
                }
                remaining -= take;
            }
        });
        stream.and_then(|stream| stream.interrupted.map_or(Ok(stream.written), Err))
    };
    let elapsed = tick::now_ms().saturating_sub(started);

    match outcome {
        Ok(written) => {
            let mut line = Line::new();
            line.push_str(if repeated {
                "write x N: "
            } else {
                "write_stream: "
            });
            line.push_u64(written);
            line.push_str(" bytes in ");
            line.push_u64(elapsed);
            line.push_str(" ms, chunk ");
            line.push_u64(chunk as u64);
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => report(console, framebuffer, "fill", error),
    }
}

/// The pre-`write_stream` shape, kept only so `fill` can measure it.
fn write_repeatedly(
    devices: &mut Devices,
    vfs: &mut Vfs,
    path: &str,
    chunk: &[u8],
    total: usize,
) -> Result<u64, FsError> {
    let handle = vfs.open(devices, path, OpenMode::Truncate)?;
    let mut written = 0u64;
    let mut remaining = total;
    while remaining > 0 {
        let take = chunk.len().min(remaining);
        match vfs.write(devices, &handle, &chunk[..take]) {
            Ok(count) => {
                written += count as u64;
                remaining -= count;
            }
            Err(error) => {
                vfs.close(handle);
                return Err(error);
            }
        }
    }
    vfs.close(handle);
    Ok(written)
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
/// The mount point is derived from the name rather than given: `ram` is `/`
/// and `sd0p1` always lands on `/vol/sd0p1`. Letting a caller choose would
/// mean the same volume could be reached by two different paths depending on
/// how it was mounted, and nothing here needs that.
pub fn mount(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    name: &str,
    request: MountRequest,
) {
    let Some((device, partition)) = parse_volume_name(name) else {
        console.write_output_line(framebuffer, "usage: mount [-r] <ram|sd0pN|usbMpN>");
        return;
    };

    match attach(devices, vfs, device, partition, request) {
        Ok(point) => {
            let mut line = Line::new();
            line.push_str("mounted ");
            line.push_str(name);
            line.push_str(" on ");
            line.push_str(point.as_str());
            // Said here rather than left to `mounts`, because whether a
            // volume can be written to is the thing a caller is most likely
            // to have assumed wrongly -- an exFAT stick is read-only however
            // it was asked for.
            line.push_str(
                match vfs
                    .mounts()
                    .find(|mount| mount.point.as_str() == point.as_str())
                {
                    Some(mount) if mount.mode == MountMode::ReadOnly => " (read-only)",
                    _ => " (read-write)",
                },
            );
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(MountFailure::NotPresent) => {
            console.write_output_line(framebuffer, "device not present")
        }
        Err(MountFailure::NoSuchPartition) => {
            console.write_output_line(framebuffer, "no such usable partition; run 'devices'")
        }
        Err(MountFailure::Fs(error)) => report(console, framebuffer, "mount", error),
    }
}

/// Why a volume could not be attached.
///
/// The two cases in front of [`FsError`] are about the *name* rather than
/// the volume: the medium is not there at all, or it is but has no such
/// entry. Neither is a filesystem error, and automount treats them
/// differently from one -- they mean there is nothing to mount, not that
/// mounting failed.
pub enum MountFailure {
    NotPresent,
    NoSuchPartition,
    Fs(FsError),
}

/// Attaches one volume and answers where it landed.
///
/// The whole of what `mount` does apart from printing, so that automount
/// puts a volume in the tree by exactly the same route a typed command does
/// -- fingerprint, generation and read-only policy included. A second path
/// that agreed with this one today would only have to be kept agreeing.
pub fn attach(
    devices: &mut Devices,
    vfs: &mut Vfs,
    device: DeviceId,
    partition: Option<u8>,
    request: MountRequest,
) -> Result<Line, MountFailure> {
    let range = match partition {
        None => {
            let block_count = devices
                .with_device(device, |block| block.geometry().block_count)
                .ok_or(MountFailure::NotPresent)?;
            crate::fs::PartitionRange {
                start_lba: 0,
                block_count,
            }
        }
        Some(number) => {
            // The partition table is read now rather than remembered from a
            // previous `devices`: the medium is the authority on its own
            // layout, and it may have been swapped since.
            let found = devices
                .with_device(device, |block| match mbr::inspect(block) {
                    Ok(mbr::Layout::Mbr(table)) => table.partition(number),
                    _ => None,
                })
                .ok_or(MountFailure::NotPresent)?
                .ok_or(MountFailure::NoSuchPartition)?;
            crate::fs::PartitionRange::from_partition(&found)
        }
    };

    // What the volume ends up mounted as is not decided here: the caller
    // says whether it wants the default or read-only, and the VFS settles it
    // against the format it reads out of the boot sector. Deciding by medium
    // -- as this used to, when only the RAM disk was writable -- would mean
    // the shell and the VFS each holding half a policy.
    let point = mount_point(volume_name(device, partition).as_str(), device);

    vfs.mount(devices, point.as_str(), device, partition, range, request)
        .map_err(MountFailure::Fs)?;
    Ok(point)
}

/// `usb0p1` from a device and an entry number: the inverse of
/// [`parse_volume_name`], and the name both the mount point and the shell's
/// messages are built from.
pub fn volume_name(device: DeviceId, partition: Option<u8>) -> Line {
    let mut name = Line::new();
    name.push_str(device_name(device).as_str());
    if let Some(number) = partition {
        name.push_str("p");
        name.push_u32(number as u32);
    }
    name
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

/// `/` for the RAM disk, `/vol/<name>` for removable media.
fn mount_point(name: &str, device: DeviceId) -> Line {
    let mut point = Line::new();
    if device == DeviceId::Ram {
        point.push_str("/");
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
