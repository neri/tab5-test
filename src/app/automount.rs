//! Putting USB volumes into the tree as they are plugged in, and taking them
//! out again as they are pulled.
//!
//! `docs/FILESYSTEM_WORKFLOW_PLAN.md` feature 2. Until this existed a volume
//! appeared in the tree only because somebody typed `mount`, which
//! `docs/FILESYSTEM_PLAN.md` fixed on purpose: it kept decisions like "which
//! medium do we boot from" from hiding inside something that happened by
//! itself. Automount does not make that kind of decision. It makes what is
//! plugged in visible, and it says so every time, which is the half of the
//! old rule worth keeping.
//!
//! Three things it deliberately does not do:
//!
//! - It does not bypass identity. Every volume goes in through
//!   [`files::attach`], the same call the shell command uses, so a stick
//!   taken out and put back gets a fresh fingerprint and a fresh generation
//!   rather than resuming the mount it had.
//! - It does not undo the user. A drive is offered to the tree once, when it
//!   attaches. `umount` after that stays unmounted until the drive is
//!   physically removed and brought back.
//! - It does not run on the bus. The per-frame check is one integer
//!   comparison; only a change in it costs the MBR read and the volume open
//!   that a mount needs. A drive that is still coming up is the one
//!   exception: it is asked again every [`RETRY_INTERVAL_MS`] until it
//!   answers or [`READY_BUDGET_MS`] runs out.
//!
//! SD is not covered. The slot has no card-detect line, so "is a card
//! there?" is only answerable by running an activation sequence and waiting
//! for it to time out, and doing that on a timer would put a stream of
//! failures in the log to discover nothing.

use super::files::{self, MountFailure};
use super::shell::Line;
use crate::console::{Console, InputLine};
use crate::framebuffer::Framebuffer;
use crate::fs::block::{self, BlockError};
use crate::fs::vfs::{FsError, Mount, Vfs, error_name};
use crate::fs::{DeviceId, Devices, RamBlockDevice, SdSlot, mbr};
use crate::usb::{STORAGE_ID_LIMIT, UsbHost};
use crate::{tick, uart};

/// Whether volumes appear and disappear on their own, and what has already
/// been offered to the tree.
pub struct AutoMount {
    enabled: bool,
    /// The topology epoch the last reconcile ran against. `None` forces one,
    /// which is how both the first frame after boot and `automount on` get
    /// the bus looked at without waiting for something to be plugged in.
    seen_topology: Option<u32>,
    /// One bit per `usbM` number that has already been offered to the tree.
    ///
    /// This is what keeps automount from arguing with `umount`. Without it
    /// the next rescan -- and a recovery rescan can happen at any time --
    /// would put back a volume the user had just detached. A number's bit is
    /// dropped when its drive stops being attached, so the same number
    /// arriving again is a new drive and gets offered again.
    ///
    /// A `u16` because `STORAGE_ID_LIMIT` numbers exist; the assertion below
    /// keeps the two from drifting apart.
    offered: u16,
    /// Drives that have attached but could not be opened yet, still inside
    /// their [`READY_BUDGET_MS`]. See that constant for why this exists.
    pending: u16,
    /// When each pending drive's budget runs out. Per drive rather than one
    /// shared deadline, because two drives plugged in a second apart are two
    /// separate races.
    deadline_ms: [u64; STORAGE_ID_LIMIT as usize],
    /// Tick milliseconds at which a pending drive may be asked again.
    next_retry_ms: u64,
}

const _: () = assert!(STORAGE_ID_LIMIT as u32 <= u16::BITS);

/// How long a newly attached drive is given to become readable.
///
/// A drive is bound on the bus as soon as it answers enumeration, which is
/// well before its logical unit will answer a read: big sticks and card
/// readers routinely spend seconds on internal initialization.
/// `UsbMscBlockDevice::probe` runs a single TEST UNIT READY and gives up, so
/// asking once, at the moment of attachment, loses that race often enough to
/// look like an unreliable stick -- and a drive that is never asked again is
/// indistinguishable from one holding nothing readable.
///
/// The figure is the boot path's own (`BOOT_MASS_STORAGE_READY_MS` in
/// `src/input.rs`, measured in `docs/STORAGE.md`), because it is the same
/// question asked at a different moment.
const READY_BUDGET_MS: u64 = 4_000;

/// Gap between attempts at a drive that will not open yet.
///
/// One TEST UNIT READY per attempt, so the cost is small; the spacing is
/// there so a device that takes its time is not asked once per frame while
/// it does. An empty card reader spends the whole budget this way and is
/// then reported as unreadable, which is a fair description of it.
const RETRY_INTERVAL_MS: u64 = 250;

impl Default for AutoMount {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoMount {
    pub const fn new() -> Self {
        Self {
            enabled: true,
            seen_topology: None,
            offered: 0,
            pending: 0,
            deadline_ms: [0; STORAGE_ID_LIMIT as usize],
            next_retry_ms: 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turns automount on or off.
    ///
    /// Turning it on reconciles on the next frame rather than only on the
    /// next plug event: what is in the tree while it was off is not what it
    /// would have put there, and leaving that difference to be discovered by
    /// the next removal would make `automount on` mean nothing until
    /// something moved.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled {
            self.seen_topology = None;
        }
    }

    /// Brings the mount table into line with the bus, if the bus has moved.
    ///
    /// Called once per frame from `app::run`, at the same level as command
    /// dispatch rather than from `InputManager::service`. Opening a volume
    /// is bus I/O and FAT parsing and can take tens of milliseconds; that is
    /// fine where a typed command would have paid it too, and it is not fine
    /// inside the frame loop's input servicing, which the console's redraw
    /// is waiting on.
    pub fn service(
        &mut self,
        console: &mut Console,
        framebuffer: &mut Framebuffer,
        vfs: &mut Vfs,
        ram_disk: Option<&mut RamBlockDevice>,
        usb_host: &mut UsbHost,
    ) {
        let mut report = Report::console(console);
        self.service_with_report(&mut report, framebuffer, vfs, ram_disk, usb_host);
        report.finish(framebuffer);
    }

    /// Runs the same reconciliation while a full-screen mode owns the pixels.
    /// Lines go to UART instead of drawing through the hidden console.
    pub fn service_silent(
        &mut self,
        framebuffer: &mut Framebuffer,
        vfs: &mut Vfs,
        ram_disk: Option<&mut RamBlockDevice>,
        usb_host: &mut UsbHost,
    ) -> bool {
        let mut report = Report::uart();
        self.service_with_report(&mut report, framebuffer, vfs, ram_disk, usb_host);
        report.has_warning()
    }

    /// True while an attached drive is still inside its readiness budget.
    pub fn has_pending(&self) -> bool {
        self.pending != 0
    }

    fn service_with_report(
        &mut self,
        report: &mut Report<'_>,
        framebuffer: &mut Framebuffer,
        vfs: &mut Vfs,
        ram_disk: Option<&mut RamBlockDevice>,
        usb_host: &mut UsbHost,
    ) {
        if !self.enabled {
            return;
        }
        let topology = usb_host.topology_epoch();
        let moved = self.seen_topology != Some(topology);
        let now = tick::now_ms();
        // The second reason to run: a drive that attached earlier and has
        // not answered yet. Nothing on the bus changes while it is coming
        // up, so waiting for the topology to move again would mean waiting
        // forever.
        let retry_due = self.pending != 0 && now >= self.next_retry_ms;
        if !moved && !retry_due {
            return;
        }
        self.seen_topology = Some(topology);
        self.next_retry_ms = now.saturating_add(RETRY_INTERVAL_MS);
        self.reconcile(report, framebuffer, vfs, ram_disk, usb_host, now);
    }

    fn reconcile(
        &mut self,
        report: &mut Report<'_>,
        framebuffer: &mut Framebuffer,
        vfs: &mut Vfs,
        ram_disk: Option<&mut RamBlockDevice>,
        usb_host: &mut UsbHost,
        now: u64,
    ) {
        // Removals first. A drive that has gone must leave the tree before
        // its number can be handed to another one, and dropping a mount
        // costs nothing on a bus that may no longer have the device on it.
        vfs.prune_disconnected(usb_host, |mount| {
            report.line(framebuffer, &removal_line(mount));
        });

        let mut attached = 0u16;
        for (id, _, _) in usb_host.mass_storage_inventory() {
            attached |= 1u16 << id;
        }
        // A drive that has gone takes its offer and its budget with it.
        self.offered &= attached;
        self.pending &= attached;
        let fresh = attached & !self.offered;
        // Marked as offered before anything is tried, so a drive that
        // answers and holds nothing readable is looked at once rather than
        // on every rescan for as long as it stays plugged in. Whether it
        // answered *at all* is the separate question `pending` tracks.
        self.offered = attached;
        for id in 0..STORAGE_ID_LIMIT {
            if fresh & (1u16 << id) != 0 {
                self.deadline_ms[id as usize] = now.saturating_add(READY_BUDGET_MS);
            }
        }
        self.pending |= fresh;

        if self.pending == 0 {
            return;
        }

        let examine = self.pending;
        let mut sd = SdSlot::new();
        let mut devices = Devices {
            ram: ram_disk,
            sd: &mut sd,
            usb: usb_host,
        };
        for id in 0..STORAGE_ID_LIMIT {
            if examine & (1u16 << id) == 0 {
                continue;
            }
            match attach_device(report, framebuffer, &mut devices, vfs, id) {
                // Answered, one way or the other. Whatever it holds has been
                // reported and there is nothing left to wait for.
                Outcome::Examined => self.pending &= !(1u16 << id),
                // Keep asking until the budget is gone, then say so once --
                // silence would leave something plugged in and simply
                // missing from the tree.
                Outcome::Retry(unsettled) => {
                    if now >= self.deadline_ms[id as usize] {
                        self.pending &= !(1u16 << id);
                        report.warning(framebuffer, &unsettled_line(DeviceId::Usb(id), unsettled));
                    }
                }
            }
        }
    }
}

/// Mounts every partition of one newly attached drive that holds a
/// filesystem this firmware can read.
///
/// Every usable primary entry is tried rather than stopping at the first
/// one, so a stick partitioned in two shows both halves. What decides
/// whether an entry appears is whether its boot sector identifies as FAT or
/// exFAT, which `Vfs::mount` establishes by opening it; an entry holding
/// something else -- the NTFS recovery partition a Windows installer leaves
/// behind is the usual one -- is skipped without a console line, because
/// "there is a partition here this cannot read" is not news about a stick
/// the user plugged in to read the other partition of.
/// What one pass at a drive settled.
enum Outcome {
    /// The drive answered for everything it holds, and whatever came of that
    /// has been reported.
    Examined,
    /// Something did not answer and may on the next pass. Nothing has been
    /// reported: a stick that is still coming up, or a transfer that failed
    /// once and recovered, is the normal texture of this bus rather than
    /// news. The caller keeps the reason in case the budget runs out.
    Retry(Unsettled),
}

/// What was left unanswered, kept so that giving up can name it.
#[derive(Clone, Copy)]
struct Unsettled {
    /// The entry that would not mount, or `None` when the drive itself would
    /// not open.
    partition: Option<u8>,
    reason: &'static str,
}

/// Whether a failed mount is worth trying again.
///
/// A transport that fails once and recovers is a known property of this bus
/// (`docs/USB_WRITE_STABILITY_PLAN.md`), and the established answer to it is
/// that the caller retries rather than the mount being given up on
/// (`docs/FILESYSTEM.md`, "rescan を跨いでもマウントは生き残る"). Treating
/// one timed-out READ CAPACITY as this partition's final answer leaves it
/// out of the tree until the drive is physically replugged, which is what
/// made automount look unreliable on drives that are slow to settle.
///
/// Everything else is an answer rather than a failure to get one: the entry
/// holds nothing readable, or the tree already has it.
fn worth_retrying(failure: &MountFailure) -> bool {
    match failure {
        // The device would not open. `UsbMscBlockDevice::probe` reports
        // TEST UNIT READY and READ CAPACITY(10) failures this way, and the
        // BOT layer has already run its own reset recovery underneath.
        MountFailure::NotPresent => true,
        MountFailure::NoSuchPartition => false,
        MountFailure::Fs(error) => matches!(
            error,
            // Same thing as `NotPresent`, reached from inside `Vfs::mount`:
            // the fingerprint, the boot-sector read and the volume open each
            // resolve the device again, and any of them can be the one that
            // meets a failing transfer.
            FsError::DeviceNotPresent
                | FsError::Block(BlockError::DeviceError)
                | FsError::Block(BlockError::NotReady)
                | FsError::Block(BlockError::TemporarilyUnavailable)
        ),
    }
}

fn failure_reason(failure: &MountFailure) -> &'static str {
    match failure {
        MountFailure::NotPresent => "device not present",
        MountFailure::NoSuchPartition => "no such usable partition",
        MountFailure::Fs(error) => error_name(*error),
    }
}

fn attach_device(
    report: &mut Report,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    vfs: &mut Vfs,
    id: u8,
) -> Outcome {
    let device = DeviceId::Usb(id);
    // Read once for the whole drive. `files::attach` reads it again per
    // entry, which is the right thing for a typed command naming one volume,
    // but this needs to know which entries exist before it can name any.
    let layout = devices.with_device(device, mbr::inspect);
    let table = match layout {
        Some(Ok(mbr::Layout::Mbr(table))) => table,
        // A filesystem written straight to LBA 0. Named rather than lumped
        // in with the unreadable media below, because there *is* something
        // here to read and the reason it stays out of the tree is this
        // firmware's -- every mount point is defined by an entry number.
        Some(Ok(mbr::Layout::SuperfloppyUnsupported(_))) => {
            report.warning(
                framebuffer,
                &nothing_line(device, "filesystem with no partition table; not mountable"),
            );
            return Outcome::Examined;
        }
        // The device would not open: `probe` runs one TEST UNIT READY and
        // gives up, so this is what a drive that is still coming up looks
        // like. The caller keeps asking until the budget is gone.
        None => {
            return Outcome::Retry(Unsettled {
                partition: None,
                reason: "did not become readable",
            });
        }
        // It opened -- so the unit is ready -- and the read of LBA 0 failed
        // anyway. That is a transfer failure rather than a race, so it is
        // reported now with its own reason rather than retried behind a
        // message about partition tables.
        Some(Err(error)) => {
            report.warning(framebuffer, &nothing_line(device, block::error_name(error)));
            return Outcome::Examined;
        }
        // LBA 0 is not a partition table this can work from. The drive is
        // attached, so silence would read as automount not having noticed
        // it.
        Some(Ok(_)) => {
            report.warning(
                framebuffer,
                &nothing_line(device, "no usable partition table"),
            );
            return Outcome::Examined;
        }
    };

    let mut mounted = 0usize;
    // Kept rather than reported. An entry that did not answer is retried,
    // and only named if it is still not answering when the budget is gone.
    let mut unsettled: Option<Unsettled> = None;
    for number in 1..=mbr::MAX_PARTITIONS as u8 {
        if table.partition(number).is_none() {
            continue;
        }
        // Already in the tree, so there is nothing to offer and nothing to
        // say. Reached when a rescan lost the drive for a moment and found
        // it again: the mounts survive that, because a device that does not
        // answer is not evidence that anything was unplugged, and the drive
        // keeps its number for the same reason -- so it arrives back here
        // looking new.
        if is_mounted(vfs, device, number) {
            mounted += 1;
            continue;
        }
        match files::attach(devices, vfs, device, Some(number)) {
            Ok(point) => {
                mounted += 1;
                report.line(
                    framebuffer,
                    &mounted_line(device, Some(number), point.as_str()),
                );
            }
            Err(failure) => {
                let reason = failure_reason(&failure);
                if worth_retrying(&failure) {
                    // Only the first is kept: the give-up line names one
                    // thing, and the entry that failed first is the one the
                    // reader is most likely looking for.
                    unsettled.get_or_insert(Unsettled {
                        partition: Some(number),
                        reason,
                    });
                } else if !matches!(
                    failure,
                    // The entry is there but holds nothing this can read.
                    // Ordinary enough on real media -- a Windows recovery
                    // partition sits in the table of plenty of sticks --
                    // that it gets no console line of its own; the summary
                    // below covers a drive where nothing mounted at all.
                    MountFailure::Fs(FsError::NotAFilesystem) | MountFailure::NoSuchPartition
                ) {
                    report.warning(framebuffer, &failure_line(device, number, reason));
                }
            }
        }
    }
    // Entries that answered are left mounted either way, so a retry only
    // has the ones that did not left to do -- `is_mounted` above skips the
    // rest. The summary waits until nothing is outstanding, or it would
    // announce an empty drive while an entry is still being asked.
    if let Some(unsettled) = unsettled {
        return Outcome::Retry(unsettled);
    }
    if mounted == 0 {
        report.warning(framebuffer, &nothing_line(device, "no readable filesystem"));
    }
    Outcome::Examined
}

/// Whether this volume is already in the tree.
///
/// Asked of the mount table rather than of the mount point's name, so that a
/// volume the user mounted by hand counts as mounted whatever it is called.
fn is_mounted(vfs: &Vfs, device: DeviceId, partition: u8) -> bool {
    vfs.mounts()
        .any(|mount| mount.volume.device == device && mount.volume.partition == Some(partition))
}

fn unsettled_line(device: DeviceId, unsettled: Unsettled) -> Line {
    match unsettled.partition {
        None => nothing_line(device, unsettled.reason),
        Some(number) => failure_line(device, number, unsettled.reason),
    }
}

fn removal_line(mount: &Mount) -> Line {
    let mut line = Line::new();
    line.push_str("automount: ");
    line.push_str(mount.point.as_str());
    line.push_str(" unmounted, device removed");
    line
}

fn mounted_line(device: DeviceId, partition: Option<u8>, point: &str) -> Line {
    let mut line = Line::new();
    line.push_str("automount: mounted ");
    line.push_str(files::volume_name(device, partition).as_str());
    line.push_str(" on ");
    line.push_str(point);
    line
}

fn nothing_line(device: DeviceId, reason: &str) -> Line {
    let mut line = Line::new();
    line.push_str("automount: ");
    line.push_str(files::volume_name(device, None).as_str());
    line.push_str(": ");
    line.push_str(reason);
    line
}

fn failure_line(device: DeviceId, partition: u8, reason: &str) -> Line {
    let mut line = Line::new();
    line.push_str("automount: ");
    line.push_str(files::volume_name(device, Some(partition)).as_str());
    line.push_str(": ");
    line.push_str(reason);
    line
}

/// Writes automount's lines without destroying whatever is being typed.
///
/// The line being edited is lifted out of the way on the first line written
/// and put back at the end. It is done lazily because a reconcile that finds
/// nothing to say is the common case -- every periodic rescan produces one
/// -- and taking the input line down and back up on each of those would make
/// the cursor twitch for no reason.
struct Report<'a> {
    console: Option<&'a mut Console>,
    saved: Option<InputLine>,
    warned: bool,
}

impl<'a> Report<'a> {
    fn console(console: &'a mut Console) -> Self {
        Self {
            console: Some(console),
            saved: None,
            warned: false,
        }
    }

    fn uart() -> Self {
        Self {
            console: None,
            saved: None,
            warned: false,
        }
    }

    fn has_warning(&self) -> bool {
        self.warned
    }

    fn warning(&mut self, framebuffer: &mut Framebuffer, line: &Line) {
        self.warned = true;
        self.line(framebuffer, line);
    }

    fn line(&mut self, framebuffer: &mut Framebuffer, line: &Line) {
        let Some(console) = self.console.as_mut() else {
            uart::log(line.as_str().as_bytes());
            uart::log(b"\r\n");
            return;
        };
        if self.saved.is_none() {
            self.saved = Some(console.take_input_line(framebuffer));
        }
        console.write_output_line(framebuffer, line.as_str());
    }

    fn finish(mut self, framebuffer: &mut Framebuffer) {
        if let (Some(console), Some(saved)) = (self.console.as_mut(), self.saved.take()) {
            console.restore_input_line(framebuffer, saved);
        }
    }
}
