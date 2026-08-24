//! Display for the VFS: the `mounts`, `ls` and `cat` commands.
//!
//! `docs/FILESYSTEM_PLAN.md` Stage 2. These are the read-only shell the plan
//! asks for, and they are also how the reader gets tested: `ls /ram` has to
//! show the long name rather than its `~1` alias, and `cat /ram/CHAIN.TXT`
//! has to keep printing the right cluster number past the end of the first
//! one.
//!
//! It sits beside `blockdev.rs` in the same way the VFS sits above the block
//! layer -- that one shows what a medium is, this one shows what is on it.

use super::shell::Line;
use crate::console::Console;
use crate::framebuffer::Framebuffer;
use crate::fs::registry::{device_name, parse_device_name};
use crate::fs::vfs::{
    DirEntry, EntryKind, FsError, MountMode, OpenMode, Vfs, error_name, format_name, verdict_name,
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

/// `ls [<path>]`, defaulting to the root, which lists the mount points.
pub fn list(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &Vfs,
    path: &str,
) {
    let mut count = 0u32;
    let outcome = vfs.list(devices, path, |entry| {
        count += 1;
        console.write_output_line(framebuffer, format_entry(&entry).as_str());
    });
    match outcome {
        Ok(()) => {
            let mut line = Line::new();
            line.push_u32(count);
            line.push_str(if count == 1 { " entry" } else { " entries" });
            console.write_output_line(framebuffer, line.as_str());
        }
        Err(error) => report(console, framebuffer, "ls", error),
    }
}

fn format_entry(entry: &DirEntry<'_>) -> Line {
    let mut line = Line::new();
    line.push_str(match entry.kind {
        // A mount point is marked apart from a directory because it is not
        // one: it belongs to the tree rather than to any volume, and `ls` of
        // the root is showing the mount table, not a filesystem.
        EntryKind::MountPoint => "mount ",
        EntryKind::Directory => "dir   ",
        EntryKind::File => "file  ",
    });
    if entry.kind == EntryKind::File {
        line.push_u64(entry.size);
        line.push_str(" ");
    }
    match entry.modified {
        Some(stamp) => {
            line.push_u32(stamp.year as u32);
            push_two_digits(&mut line, "-", stamp.month);
            push_two_digits(&mut line, "-", stamp.day);
            push_two_digits(&mut line, " ", stamp.hour);
            push_two_digits(&mut line, ":", stamp.minute);
            push_two_digits(&mut line, ":", stamp.second);
        }
        // Aligned with a dated line, so a column of entries stays readable
        // when only some of them carry a timestamp.
        None => line.push_str("       (no time)   "),
    }
    line.push_str(" ");
    line.push_str(entry.name);
    line
}

/// Pushes a separator and a zero-padded two-digit field.
fn push_two_digits(line: &mut Line, separator: &str, value: u8) {
    line.push_str(separator);
    if value < 10 {
        line.push_str("0");
    }
    line.push_u32(value as u32);
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

/// `/ram` for the RAM disk, `/vol/<name>` for removable media.
fn mount_point(name: &str, device: DeviceId) -> Line {
    let mut point = Line::new();
    if device == DeviceId::Ram {
        point.push_str("/ram");
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
