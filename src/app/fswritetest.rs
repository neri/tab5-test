//! `fswritetest`: the write-path acceptance run, as one command.
//!
//! `docs/plans/archive/FILESYSTEM_WRITE_REFACTOR_PLAN.md` asks for a list of things to try
//! by hand on a sacrificial volume. Typed one at a time they are easy to get
//! wrong -- and the check that matters most, appending to a file that was
//! just replaced with a shorter one, looks *correct* by eye unless the exact
//! bytes are compared. So the sequence lives here instead, with the
//! comparisons done rather than looked at, and answers PASS or FAIL.
//!
//! Everything it makes goes inside one scratch directory it creates itself
//! and refuses to run if that directory is already there, so it never writes
//! over anything the volume already holds. It still writes to the medium,
//! which is why the plan says to use a card you can afford to lose.
//!
//! A read-only mount is not a failure here. `mount -r` is one of the things
//! the plan asks to confirm, so a volume that refuses the first mutation is
//! checked for refusing the rest and reported as read-only -- which makes
//! the same command the test for both directions.

use alloc::vec::Vec;

use super::shell::Line;
use crate::console::Console;
use crate::framebuffer::Framebuffer;
use crate::fs::Devices;
use crate::fs::path::{self, Path};
use crate::fs::vfs::{EntryKind, FsError, OpenMode, Vfs, error_name};
use crate::tick;

/// The directory everything is made inside, relative to the path given.
///
/// An 8.3 name so that what a PC shows afterwards is the same name typed
/// here, with no long-name entries in the way of reading a `fsck` report.
const SCRATCH: &str = "FSWTEST";

/// The multi-cluster file size, in bytes.
///
/// Two clusters is what the replacement check needs -- a file that fits in
/// one cluster cannot leave a tail behind when it is replaced -- and 64 KiB
/// is at least two clusters on any volume this firmware mounts, since FAT32
/// tops out at 32 KiB per cluster.
const BIG_BYTES: usize = 64 * 1024;
/// The short content a `BIG_BYTES` file is replaced with. Well under one
/// cluster, so the replacement frees at least one.
const SHORT_BYTES: usize = 200;
/// Appended after the replacement. This is the check the whole plan is
/// about: read back, these bytes must follow the 200 above and nothing else.
const TAIL_BYTES: usize = 300;

/// Bytes per `write` and per compare. Reads are answered by walking the file
/// from its start (see `fs::vfs`), so the cost of verifying a file grows
/// with the square of the number of calls -- which is the reason this is
/// 4 KiB and not one console line's worth.
const CHUNK: usize = 4096;

/// Column each check's result is printed at.
const RESULT_COLUMN: usize = 40;
/// Rounds between progress lines. A long soak on USB is minutes of silence
/// otherwise, and silence and a hang look the same from the console.
const PROGRESS_EVERY: u32 = 16;

/// Rounds of the create/replace/delete cycle when none is asked for.
pub const DEFAULT_ROUNDS: u32 = 8;
/// Rounds accepted at most, so a mistyped argument cannot start a run
/// measured in hours.
const MAX_ROUNDS: u32 = 2000;
/// Churn file size when none is asked for, in KiB.
pub const DEFAULT_CHURN_KIB: u32 = 128;
const MAX_CHURN_KIB: u32 = 1024;

/// One run of the pattern, identified by which run it is.
///
/// A file is described as a list of these, and that description is what both
/// the writer and the comparison work from -- so "what should be there" is
/// written down once rather than twice.
#[derive(Clone, Copy)]
struct Segment {
    seed: u8,
    len: usize,
}

/// The byte belonging at `offset` within a segment.
///
/// Modulo 251 rather than 256 so the pattern does not repeat on any power of
/// two: a chain that lost a cluster and picked up a different one at the
/// same offset within it would still line up under a 256-byte cycle.
fn pattern_byte(seed: u8, offset: usize) -> u8 {
    ((offset % 251) as u8).wrapping_add(seed)
}

/// What went wrong, in enough detail to say where.
enum Fault {
    Fs(FsError),
    /// The bytes read back differ from the bytes written, at this offset
    /// into the file. This is what a replacement that left its old tail
    /// linked looks like from outside.
    Content(u64),
    /// The file is not as long as what was written to it.
    Length {
        want: u64,
        got: u64,
    },
    /// A path that should be gone is still there.
    StillThere,
    /// A mutation that should have been refused succeeded.
    NotRefused,
}

fn push_fault(line: &mut Line, fault: &Fault) {
    match fault {
        Fault::Fs(error) => line.push_str(error_name(*error)),
        Fault::Content(offset) => {
            line.push_str("content differs at byte ");
            line.push_u64(*offset);
        }
        Fault::Length { want, got } => {
            line.push_str("length ");
            line.push_u64(*got);
            line.push_str(", expected ");
            line.push_u64(*want);
        }
        Fault::StillThere => line.push_str("still present after removal"),
        Fault::NotRefused => line.push_str("succeeded on a read-only mount"),
    }
}

/// Prints one check's result and remembers whether anything has failed.
///
/// A struct rather than a returned `Result` chain because every check has to
/// print its own line whatever the ones before it did, and because the run
/// stops at the first failure: once a volume has misbehaved, what the checks
/// after it report is about a state nobody meant to create.
struct Run<'a> {
    console: &'a mut Console,
    framebuffer: &'a mut Framebuffer,
    number: u32,
    failed: bool,
}

impl Run<'_> {
    /// Records one check. Answers whether the run should continue.
    fn step(&mut self, label: &str, outcome: Result<(), Fault>) -> bool {
        self.number += 1;
        let mut line = Line::new();
        line.push_str("  ");
        if self.number < 10 {
            line.push_str(" ");
        }
        line.push_u32(self.number);
        line.push_str(" ");
        line.push_str(label);
        // Padded to a column so a failure is found by looking down the right
        // edge rather than by reading every line. `is_full` is what ends the
        // loop for a label longer than the column, since `Line` drops what
        // does not fit rather than growing.
        while line.as_str().len() < RESULT_COLUMN && !line.is_full() {
            line.push_str(" ");
        }
        match &outcome {
            Ok(()) => line.push_str("ok"),
            Err(fault) => {
                line.push_str("FAIL: ");
                push_fault(&mut line, fault);
            }
        }
        self.console
            .write_output_line(self.framebuffer, line.as_str());
        if outcome.is_err() {
            self.failed = true;
        }
        !self.failed
    }

    fn note(&mut self, text: &str) {
        self.console.write_output_line(self.framebuffer, text);
    }
}

/// `fswritetest <dir> [rounds] [KiB]`.
pub fn run(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    directory: &Path,
    rounds: u32,
    churn_kib: u32,
) {
    let rounds = rounds.clamp(1, MAX_ROUNDS);
    let churn_bytes = churn_kib.clamp(1, MAX_CHURN_KIB) as usize * 1024;

    // The directory has to be there already. Creating it would mean the
    // command could make a mess somewhere the user did not mean, and a
    // mistyped mount point is exactly the way that happens.
    match vfs.metadata(devices, directory.as_str()) {
        Ok(metadata) if metadata.kind == EntryKind::Directory => {}
        Ok(_) => {
            console.write_output_line(framebuffer, "fswritetest: that path is not a directory");
            return;
        }
        Err(error) => {
            let mut line = Line::new();
            line.push_str("fswritetest: ");
            line.push_str(error_name(error));
            console.write_output_line(framebuffer, line.as_str());
            return;
        }
    }

    let Some(scratch) = child(console, framebuffer, directory, SCRATCH) else {
        return;
    };
    // Refused rather than reused. A directory of this name that is already
    // there was either left by a run that failed -- in which case its
    // contents are the evidence -- or is somebody's own, and this command
    // deletes everything it finds inside its own scratch directory.
    if vfs.metadata(devices, scratch.as_str()).is_ok() {
        let mut line = Line::new();
        line.push_str("fswritetest: ");
        line.push_str(scratch.as_str());
        line.push_str(" already exists; remove it first");
        console.write_output_line(framebuffer, line.as_str());
        return;
    }

    let mut line = Line::new();
    line.push_str("fswritetest: ");
    line.push_str(scratch.as_str());
    line.push_str(" (created and removed)");
    console.write_output_line(framebuffer, line.as_str());

    // The first mutation decides which of the two runs this is. A read-only
    // mount answers here, before anything has been written.
    let created = vfs.create_dir(devices, scratch.as_str());
    if created == Err(FsError::ReadOnly) {
        read_only_run(console, framebuffer, devices, vfs, directory);
        return;
    }

    let started = tick::now_ms();
    let mut run = Run {
        console,
        framebuffer,
        number: 0,
        failed: false,
    };
    let outcome = checks(
        &mut run,
        devices,
        vfs,
        &scratch,
        created.map_err(Fault::Fs),
        rounds,
        churn_bytes,
    );
    let failed = run.failed;
    let elapsed = tick::now_ms().saturating_sub(started);

    // Cleanup runs whether or not the checks passed, but it is only a check
    // itself when they did: after a failure the volume is in a state nobody
    // asked for, and reporting the tidy-up as a second failure would bury
    // the first one.
    let swept = sweep(devices, vfs, &scratch);
    let mut line = Line::new();
    line.push_str("fswritetest: ");
    if failed {
        line.push_str("FAIL at check ");
        line.push_u32(outcome);
    } else {
        line.push_str("PASS, ");
        line.push_u32(outcome);
        line.push_str(" checks, ");
        line.push_u32(rounds);
        line.push_str(" rounds, ");
        line.push_u64(elapsed);
        line.push_str(" ms");
    }
    console.write_output_line(framebuffer, line.as_str());
    if let Err(error) = swept {
        // Worth a line either way. After a pass it means the volume kept
        // something this command made, which is itself wrong; after a
        // failure it is usually the mount having been dropped by the write
        // policy, and saying which is what tells the user where to look.
        let mut line = Line::new();
        line.push_str("  scratch not removed: ");
        line.push_str(error_name(error));
        console.write_output_line(framebuffer, line.as_str());
        // Both of these mean the mount is no longer in the table. `NotMounted`
        // is the plain form; `ReservedPath` is what a path under `/vol` gives
        // once its child mount has gone, because it then resolves to the RAM
        // root's reserved namespace instead.
        if error == FsError::NotMounted || error == FsError::ReservedPath {
            console.write_output_line(
                framebuffer,
                "  the mount was dropped by the write-failure policy; see the UART log",
            );
        }
    }
}

/// Runs every check in order, stopping at the first failure. Answers how
/// many checks ran.
fn checks(
    run: &mut Run,
    devices: &mut Devices,
    vfs: &mut Vfs,
    scratch: &Path,
    created: Result<(), Fault>,
    rounds: u32,
    churn_bytes: usize,
) -> u32 {
    if !run.step("mkdir scratch directory", created) {
        return run.number;
    }

    let Some(stream_file) = run_child(run, scratch, "S.BIN") else {
        return run.number;
    };
    let Some(handle_file) = run_child(run, scratch, "H.BIN") else {
        return run.number;
    };
    let Some(sub) = run_child(run, scratch, "SUB") else {
        return run.number;
    };
    let Some(moved) = run_child(run, &sub, "M.BIN") else {
        return run.number;
    };
    let Some(part) = run_child(run, scratch, "P.PART") else {
        return run.number;
    };
    let Some(finished) = run_child(run, scratch, "P.BIN") else {
        return run.number;
    };

    let big = [Segment {
        seed: 0,
        len: BIG_BYTES,
    }];
    let short = [Segment {
        seed: 61,
        len: SHORT_BYTES,
    }];
    let tail = [Segment {
        seed: 127,
        len: TAIL_BYTES,
    }];
    // What the file must read back as once the short replacement has been
    // appended to: the replacement, then the appended bytes, and nothing of
    // the 64 KiB that was there before.
    let replaced_then_appended = [short[0], tail[0]];

    // --- the write_stream path -------------------------------------------

    let outcome = write_stream(devices, vfs, &stream_file, OpenMode::Truncate, &big)
        .and_then(|()| verify(devices, vfs, &stream_file, &big));
    if !run.step("stream: write 64 KiB", outcome) {
        return run.number;
    }

    let outcome = write_stream(devices, vfs, &stream_file, OpenMode::Truncate, &short)
        .and_then(|()| verify(devices, vfs, &stream_file, &short));
    if !run.step("stream: replace with 200 bytes", outcome) {
        return run.number;
    }

    let outcome = write_stream(devices, vfs, &stream_file, OpenMode::Append, &tail)
        .and_then(|()| verify(devices, vfs, &stream_file, &replaced_then_appended));
    if !run.step("stream: append after replace", outcome) {
        return run.number;
    }

    // --- the open/write handle path --------------------------------------

    let outcome = write_handle(devices, vfs, &handle_file, OpenMode::Truncate, &big)
        .and_then(|()| verify(devices, vfs, &handle_file, &big));
    if !run.step("handle: write 64 KiB", outcome) {
        return run.number;
    }

    let outcome = write_handle(devices, vfs, &handle_file, OpenMode::Truncate, &short)
        .and_then(|()| verify(devices, vfs, &handle_file, &short));
    if !run.step("handle: replace with 200 bytes", outcome) {
        return run.number;
    }

    let outcome = write_handle(devices, vfs, &handle_file, OpenMode::Append, &tail)
        .and_then(|()| verify(devices, vfs, &handle_file, &replaced_then_appended));
    if !run.step("handle: append after replace", outcome) {
        return run.number;
    }

    // --- directories and renames -----------------------------------------

    let outcome = vfs.create_dir(devices, sub.as_str()).map_err(Fault::Fs);
    if !run.step("mkdir subdirectory", outcome) {
        return run.number;
    }

    let outcome = vfs
        .rename(devices, handle_file.as_str(), moved.as_str())
        .map_err(Fault::Fs)
        .and_then(|()| verify(devices, vfs, &moved, &replaced_then_appended));
    if !run.step("mv into subdirectory", outcome) {
        return run.number;
    }

    // The shape every download takes: write to a `.part` name, rename to the
    // real one once it is complete.
    let outcome = write_stream(devices, vfs, &part, OpenMode::Truncate, &big)
        .and_then(|()| {
            vfs.rename(devices, part.as_str(), finished.as_str())
                .map_err(Fault::Fs)
        })
        .and_then(|()| verify(devices, vfs, &finished, &big));
    if !run.step("mv .part to final name", outcome) {
        return run.number;
    }

    // --- churn ------------------------------------------------------------

    let mut line = Line::new();
    line.push_str("  running ");
    line.push_u32(rounds);
    line.push_str(" rounds of ");
    line.push_u64(churn_bytes as u64 / 1024);
    line.push_str(" KiB create/append/replace/delete...");
    run.note(line.as_str());

    let outcome = churn(run, devices, vfs, scratch, rounds, churn_bytes);
    if !run.step("create/append/replace/delete", outcome) {
        return run.number;
    }

    // --- removal ----------------------------------------------------------

    let outcome = remove_all(devices, vfs, &[&stream_file, &moved, &finished])
        .and_then(|()| vfs.remove_dir(devices, sub.as_str()).map_err(Fault::Fs))
        .and_then(|()| gone(devices, vfs, &sub));
    if !run.step("rm and rmdir", outcome) {
        return run.number;
    }

    run.number
}

/// The run against a mount that refuses to be written to.
///
/// Not a shorter version of the checks above: what is being confirmed is
/// that nothing happens, so the only thing to do is ask for each kind of
/// mutation and see it refused.
fn read_only_run(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    directory: &Path,
) {
    console.write_output_line(framebuffer, "  the mount is read-only; nothing was written");
    let mut run = Run {
        console,
        framebuffer,
        number: 0,
        failed: false,
    };

    let Some(file) = run_child(&mut run, directory, "RO.TMP") else {
        return;
    };
    let Some(dir) = run_child(&mut run, directory, "RO.DIR") else {
        return;
    };

    let segment = [Segment { seed: 0, len: 64 }];
    run.step(
        "mkdir refused",
        refused(vfs.create_dir(devices, dir.as_str())),
    );
    run.step(
        "write refused",
        refused(
            write_stream(devices, vfs, &file, OpenMode::Truncate, &segment).map_err(|fault| {
                match fault {
                    Fault::Fs(error) => error,
                    // A non-filesystem fault here means bytes moved, which
                    // is the thing being ruled out.
                    _ => FsError::NotAFilesystem,
                }
            }),
        ),
    );
    run.step(
        "rm refused",
        refused(vfs.remove_file(devices, file.as_str())),
    );
    let failed = run.failed;

    console.write_output_line(
        framebuffer,
        if failed {
            "fswritetest: FAIL, a read-only mount accepted a change"
        } else {
            "fswritetest: PASS, read-only mount refuses every change"
        },
    );
}

/// Turns "this was refused" into a passing check and anything else into a
/// failing one.
fn refused(outcome: Result<(), FsError>) -> Result<(), Fault> {
    match outcome {
        Err(FsError::ReadOnly) => Ok(()),
        Ok(()) => Err(Fault::NotRefused),
        Err(error) => Err(Fault::Fs(error)),
    }
}

/// Repeats the whole life of a small file, `rounds` times.
///
/// Two things at once, because they are the same loop. Each round is four
/// separate medium writes -- data, FAT and directory entry do not share one
/// -- so a high round count is the WRITE-volume soak the plan asks USB for.
/// And because every round replaces the big content with a short one before
/// deleting it, a volume that fails to give clusters back runs out: on the
/// 8 MiB RAM disk, enough rounds of 128 KiB cannot fit unless the space is
/// really being reclaimed.
fn churn(
    run: &mut Run,
    devices: &mut Devices,
    vfs: &mut Vfs,
    scratch: &Path,
    rounds: u32,
    churn_bytes: usize,
) -> Result<(), Fault> {
    let file = path::join(scratch, "C.BIN").map_err(|error| Fault::Fs(FsError::Path(error)))?;
    for round in 0..rounds {
        if round > 0 && round % PROGRESS_EVERY == 0 {
            let mut line = Line::new();
            line.push_str("    round ");
            line.push_u32(round);
            line.push_str(" of ");
            line.push_u32(rounds);
            run.note(line.as_str());
        }
        // A different seed each round, so a round reading back what the
        // previous one wrote is a mismatch rather than a match.
        let seed = (round % 251) as u8;
        let head = [Segment { seed, len: 400 }];
        let added = [Segment {
            seed: seed.wrapping_add(1),
            len: 600,
        }];
        let both = [head[0], added[0]];
        let big = [Segment {
            seed: seed.wrapping_add(2),
            len: churn_bytes,
        }];

        write_stream(devices, vfs, &file, OpenMode::Truncate, &head)?;
        write_stream(devices, vfs, &file, OpenMode::Append, &added)?;
        verify(devices, vfs, &file, &both)?;
        // The replacement is what puts the clusters back. Verified by length
        // only: the content check above already covers the pattern, and this
        // loop runs enough times that a full compare would dominate it.
        write_stream(devices, vfs, &file, OpenMode::Truncate, &big)?;
        write_stream(devices, vfs, &file, OpenMode::Truncate, &head)?;
        vfs.remove_file(devices, file.as_str()).map_err(Fault::Fs)?;
    }
    gone(devices, vfs, &file)
}

/// Writes `segments` through [`Vfs::write_stream`], and checks that as many
/// bytes were taken as were offered.
fn write_stream(
    devices: &mut Devices,
    vfs: &mut Vfs,
    path: &Path,
    mode: OpenMode,
    segments: &[Segment],
) -> Result<(), Fault> {
    let want: u64 = segments.iter().map(|segment| segment.len as u64).sum();
    let stream = vfs
        .write_stream(devices, path.as_str(), mode, |sink| {
            let mut buffer = [0u8; CHUNK];
            for segment in segments {
                let mut done = 0;
                while done < segment.len {
                    let take = CHUNK.min(segment.len - done);
                    for (index, byte) in buffer[..take].iter_mut().enumerate() {
                        *byte = pattern_byte(segment.seed, done + index);
                    }
                    if !sink(&buffer[..take]) {
                        return;
                    }
                    done += take;
                }
            }
        })
        .map_err(Fault::Fs)?;
    if let Some(error) = stream.interrupted {
        return Err(Fault::Fs(error));
    }
    if stream.written != want {
        return Err(Fault::Length {
            want,
            got: stream.written,
        });
    }
    Ok(())
}

/// Writes `segments` through [`Vfs::open`] and repeated [`Vfs::write`].
///
/// The other write path, and the one the truncation rule is written against:
/// only the first `write` of a truncating open empties the file, so a
/// multi-chunk write here is also a check that the ones after it append
/// rather than starting over.
fn write_handle(
    devices: &mut Devices,
    vfs: &mut Vfs,
    path: &Path,
    mode: OpenMode,
    segments: &[Segment],
) -> Result<(), Fault> {
    let handle = vfs.open(devices, path.as_str(), mode).map_err(Fault::Fs)?;
    let mut buffer = [0u8; CHUNK];
    let mut written = 0u64;
    let mut outcome = Ok(());
    'segments: for segment in segments {
        let mut done = 0;
        while done < segment.len {
            let take = CHUNK.min(segment.len - done);
            for (index, byte) in buffer[..take].iter_mut().enumerate() {
                *byte = pattern_byte(segment.seed, done + index);
            }
            match vfs.write(devices, &handle, &buffer[..take]) {
                // A zero-length take with bytes still in hand is a volume
                // with nowhere to put them; looping would not change that.
                Ok(0) => {
                    outcome = Err(Fault::Fs(FsError::NoSpace));
                    break 'segments;
                }
                Ok(count) => {
                    done += count;
                    written += count as u64;
                }
                Err(error) => {
                    outcome = Err(Fault::Fs(error));
                    break 'segments;
                }
            }
        }
    }
    vfs.close(handle);
    outcome?;
    let want: u64 = segments.iter().map(|segment| segment.len as u64).sum();
    if written != want {
        return Err(Fault::Length { want, got: written });
    }
    Ok(())
}

/// Reads the whole file back and compares it against `segments`.
fn verify(
    devices: &mut Devices,
    vfs: &mut Vfs,
    path: &Path,
    segments: &[Segment],
) -> Result<(), Fault> {
    let handle = vfs
        .open(devices, path.as_str(), OpenMode::Read)
        .map_err(Fault::Fs)?;
    let outcome = compare(devices, vfs, &handle, segments);
    vfs.close(handle);
    outcome
}

fn compare(
    devices: &mut Devices,
    vfs: &mut Vfs,
    handle: &crate::fs::vfs::FileHandle,
    segments: &[Segment],
) -> Result<(), Fault> {
    let want: u64 = segments.iter().map(|segment| segment.len as u64).sum();
    let size = vfs.size(handle).map_err(Fault::Fs)?;
    // Checked before the content, so a file that is simply the wrong length
    // says so rather than reporting the first byte past the shorter one.
    if size != want {
        return Err(Fault::Length { want, got: size });
    }

    let mut buffer = [0u8; CHUNK];
    let mut offset = 0u64;
    loop {
        let count = vfs.read(devices, handle, &mut buffer).map_err(Fault::Fs)?;
        if count == 0 {
            break;
        }
        for (index, byte) in buffer[..count].iter().enumerate() {
            let at = offset + index as u64;
            if *byte != expected_byte(segments, at) {
                return Err(Fault::Content(at));
            }
        }
        offset += count as u64;
    }
    if offset != want {
        return Err(Fault::Length { want, got: offset });
    }
    Ok(())
}

/// The byte belonging at `offset` in a file described by `segments`.
fn expected_byte(segments: &[Segment], offset: u64) -> u8 {
    let mut start = 0u64;
    for segment in segments {
        let end = start + segment.len as u64;
        if offset < end {
            return pattern_byte(segment.seed, (offset - start) as usize);
        }
        start = end;
    }
    // Past the end of everything written. Nothing should read this; a
    // distinct value makes a mismatch report point at the right place.
    0xFF
}

fn remove_all(devices: &mut Devices, vfs: &mut Vfs, paths: &[&Path]) -> Result<(), Fault> {
    for path in paths {
        vfs.remove_file(devices, path.as_str()).map_err(Fault::Fs)?;
        gone(devices, vfs, path)?;
    }
    Ok(())
}

/// Confirms a path is no longer there.
fn gone(devices: &mut Devices, vfs: &mut Vfs, path: &Path) -> Result<(), Fault> {
    match vfs.metadata(devices, path.as_str()) {
        Err(FsError::NotFound) => Ok(()),
        Ok(_) => Err(Fault::StillThere),
        Err(error) => Err(Fault::Fs(error)),
    }
}

/// Removes whatever is left inside the scratch directory, then the directory.
///
/// Best effort by design: it runs after a failure too, when some of what it
/// names was never made. What it must not do is leave a scratch directory
/// behind after a passing run, which is the one case it is allowed to
/// report on.
fn sweep(devices: &mut Devices, vfs: &mut Vfs, scratch: &Path) -> Result<(), FsError> {
    // Deepest first: a directory cannot be removed until it is empty.
    let mut directories: Vec<Path> = Vec::new();
    let mut files: Vec<Path> = Vec::new();
    collect(vfs, devices, scratch, &mut directories, &mut files);
    for file in &files {
        let _ = vfs.remove_file(devices, file.as_str());
    }
    for directory in directories.iter().rev() {
        let _ = vfs.remove_dir(devices, directory.as_str());
    }
    vfs.remove_dir(devices, scratch.as_str())
}

/// Lists everything under `directory`, one level at a time.
///
/// Written as an explicit queue rather than a recursion because the scratch
/// tree is shallow but the stack here is shared with the write path, and a
/// depth that depends on what is on the medium is not something to put on it.
fn collect(
    vfs: &Vfs,
    devices: &mut Devices,
    directory: &Path,
    directories: &mut Vec<Path>,
    files: &mut Vec<Path>,
) {
    let mut pending: Vec<Path> = Vec::new();
    pending.push(*directory);
    while let Some(current) = pending.pop() {
        let mut children: Vec<(Path, bool)> = Vec::new();
        let _ = vfs.list(devices, current.as_str(), |entry| {
            // FAT keeps `.` and `..` as real entries in every subdirectory;
            // following them would walk in circles.
            if entry.name == "." || entry.name == ".." {
                return;
            }
            if let Ok(child) = path::join(&current, entry.name) {
                children.push((child, entry.kind == EntryKind::Directory));
            }
        });
        for (child, is_directory) in children {
            if is_directory {
                directories.push(child);
                pending.push(child);
            } else {
                files.push(child);
            }
        }
    }
}

/// `parent/name`, reporting a path that will not fit rather than truncating.
fn child(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    parent: &Path,
    name: &str,
) -> Option<Path> {
    match path::join(parent, name) {
        Ok(path) => Some(path),
        Err(error) => {
            let mut line = Line::new();
            line.push_str("fswritetest: ");
            line.push_str(path::error_name(error));
            console.write_output_line(framebuffer, line.as_str());
            None
        }
    }
}

/// [`child`], reporting through the run so the failure is counted.
fn run_child(run: &mut Run, parent: &Path, name: &str) -> Option<Path> {
    match path::join(parent, name) {
        Ok(path) => Some(path),
        Err(error) => {
            run.step("build scratch path", Err(Fault::Fs(FsError::Path(error))));
            None
        }
    }
}
