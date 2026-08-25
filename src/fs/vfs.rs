//! The virtual filesystem: one tree, fixed mount points, absolute paths.
//!
//! Volumes are combined Unix-style rather than by drive letter. A caller
//! names `/vol/sd0p1/photo.jpg`, never `0:/photo.jpg`, and the drive numbers
//! stay an implementation detail of [`super::registry`]. Offering both would
//! mean two path-resolution rules to keep in agreement.
//!
//! ## Why a mount holds no filesystem
//!
//! A [`Mount`] records an identity and a block range, and nothing else. It
//! does not hold an open `FatVolume`, and an open file does not hold the
//! library's file handle. Every operation resolves the path to a mount,
//! borrows the device from the registry, builds a volume over it, does the
//! work, and drops all of it again.
//!
//! Three things fall out of that, and they are the reason for it:
//!
//! - Two partitions of one disk can be mounted at once. Neither owns the
//!   driver, so there is no conflict over who does.
//! - A rescan can replace a USB session underneath a mount without any
//!   filesystem-side pointer being left aimed at the old one.
//! - A handle survives that replacement. It is a volume identity, a path and
//!   an offset -- all of which still mean the same thing after the medium is
//!   re-bound -- rather than a library cursor into a volume that no longer
//!   exists.
//!
//! The cost is that each operation re-reads the boot sector and walks the
//! path from the root again. That is what the per-mount sector cache in
//! [`super::stream`] is there to absorb, and it is worth measuring before it
//! is worth optimizing.

use hadris_fat::FatVolumeReadExt;
use hadris_fat::dir::FileEntry;
use hadris_fat::error::Error as FatError;
use hadris_fat::exfat::{ExFatFileReader, ExFatVolume};
use hadris_fat::sync::FatVolume;
use hadris_fat::sync::dir::FatDir;
use hadris_fat::sync::write::FileWriter;
use hadris_io::{Read, Seek, SeekFrom, Write};

use super::block::BlockError;
use super::bootsector::{self, VolumeKind};
use super::clock;
use super::fingerprint::Fingerprint;
use super::partition::{PartitionBlockDevice, PartitionRange};
use super::path::{self, Path, PathError};
use super::registry::{DeviceId, Devices};
use super::stream::BlockStream;
use crate::usb::{ConnectionEpoch, Location, UsbHost};

/// Mount points available at once. Two SD partitions, a handful of USB
/// volumes and the RAM disk fit comfortably; the table is fixed because a
/// growable one would put an allocation on the path of every mount for no
/// benefit at these numbers.
pub const MAX_MOUNTS: usize = 8;
/// Files open at once. Small on purpose: each open handle is a promise that
/// a volume cannot be unmounted, and a firmware shell that needs more than
/// this at once is doing something the VFS has not been designed for yet.
pub const MAX_OPEN_FILES: usize = 4;

/// Which medium a mount is on, and which incarnation of it.
///
/// `generation` is not a USB address or an enumeration count: it is the
/// VFS's own count of how many times the medium in this slot has been
/// established as a *different* medium. A handle carries the generation it
/// was opened against, so a number that gets reused for another card cannot
/// hand an old handle to new media.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VolumeId {
    pub device: DeviceId,
    /// MBR primary entry number, or `None` for a volume that occupies its
    /// whole device without a partition table -- which is how the RAM disk
    /// is laid out.
    pub partition: Option<u8>,
    pub generation: u32,
}

/// Whether the VFS will let a write through to this mount.
///
/// This is the policy half of the read-only rule. The other half is the
/// block adapters refusing writes outright, so a mount marked read-write by
/// mistake still cannot reach an SD card or a USB stick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MountMode {
    ReadOnly,
    ReadWrite,
}

/// Which filesystem is on a volume.
///
/// Recorded at mount time rather than sniffed per operation: the boot sector
/// is the authority, it is read once while the mount is being made anyway,
/// and a volume does not change format underneath a mount without the
/// medium changing too -- which the fingerprint catches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VolumeFormat {
    Fat,
    /// exFAT, always read-only. The library's exFAT support is an explicitly
    /// unstable preview, and every exFAT volume this firmware can reach is
    /// on removable media that is read-only by policy regardless, so there
    /// is nothing for a write path here to write to.
    Exfat,
}

pub fn format_name(format: VolumeFormat) -> &'static str {
    match format {
        VolumeFormat::Fat => "FAT",
        VolumeFormat::Exfat => "exFAT",
    }
}

pub fn mode_name(mode: OpenMode) -> &'static str {
    match mode {
        OpenMode::Read => "read",
        OpenMode::Truncate => "truncate",
        OpenMode::Append => "append",
    }
}

#[derive(Clone, Copy)]
pub struct Mount {
    pub point: Path,
    pub volume: VolumeId,
    pub range: PartitionRange,
    pub mode: MountMode,
    pub format: VolumeFormat,
    /// What the medium looked like when this mount was made. Comparing
    /// against it is how a swapped card is noticed; see
    /// [`super::fingerprint`] for what it can and cannot establish.
    pub fingerprint: Fingerprint,
    /// The connection epoch of this mount's port when it was made.
    ///
    /// This is the half a fingerprint cannot cover. Pull a stick out and put
    /// the same one back in and every byte of its identity agrees, but the
    /// medium was gone in between and an open handle must not survive that.
    /// A physical edge is observed rather than inferred, and it always wins
    /// over an identity that matches.
    pub connection_epoch: ConnectionEpoch,
    /// Where the device was on the bus, for a mount on USB.
    ///
    /// `DeviceId::Usb(n)` counts storage devices, so unplugging the first of
    /// two makes the second one's index point somewhere new. A geometry
    /// check does not catch that when both are the same size, and a full
    /// identity check is too expensive to run per operation. The location
    /// costs nothing -- it is read out of the registry's own records -- and
    /// catches exactly this.
    pub location: Option<Location>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsError {
    Path(PathError),
    /// No mount covers this path.
    NotMounted,
    /// Something is already mounted at this point.
    AlreadyMounted,
    /// The name a create asked for is already taken on the volume.
    AlreadyExists,
    MountTableFull,
    TooManyOpenFiles,
    /// The mount point exists but a handle is still open on it.
    Busy,
    NotFound,
    NotAFile,
    NotADirectory,
    /// The mount is read-only, or the handle was not opened for writing.
    ReadOnly,
    /// A write was asked for at a position the filesystem writer cannot be
    /// placed at. See [`Vfs::write`].
    NotSeekable,
    /// The volume has no room left.
    NoSpace,
    /// The device this mount names is not currently there.
    DeviceNotPresent,
    /// The volume is not a filesystem this firmware can read.
    NotAFilesystem,
    /// A rename named two different volumes. Moving between them would be a
    /// copy followed by a delete, with a different failure mode at every
    /// step, so it is refused rather than emulated.
    CrossVolume,
    /// The handle refers to a mount that has gone, or to media that has been
    /// replaced since it was opened.
    StaleHandle,
    Block(BlockError),
}

pub fn error_name(error: FsError) -> &'static str {
    match error {
        FsError::Path(error) => path::error_name(error),
        FsError::NotMounted => "no filesystem mounted on that path",
        FsError::AlreadyMounted => "already mounted",
        FsError::AlreadyExists => "already exists",
        FsError::MountTableFull => "mount table full",
        FsError::TooManyOpenFiles => "too many open files",
        FsError::Busy => "busy: files are still open",
        FsError::NotFound => "no such file or directory",
        FsError::NotAFile => "not a file",
        FsError::NotADirectory => "not a directory",
        FsError::ReadOnly => "read-only",
        FsError::NotSeekable => "can only write at the start or the end of a file",
        FsError::NoSpace => "no space left on volume",
        FsError::DeviceNotPresent => "device not present",
        FsError::NotAFilesystem => "not a readable filesystem",
        FsError::CrossVolume => "cannot move between volumes",
        FsError::StaleHandle => "handle no longer valid",
        FsError::Block(error) => super::block::error_name(error),
    }
}

impl From<PathError> for FsError {
    fn from(error: PathError) -> Self {
        FsError::Path(error)
    }
}

impl From<BlockError> for FsError {
    fn from(error: BlockError) -> Self {
        FsError::Block(error)
    }
}

/// What a directory listing reports about one entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    File,
    Directory,
    /// A mount point, seen while listing the tree above the volumes. It is
    /// not an object on any volume, which is why it is not simply a
    /// directory. The nodes on the way down to one -- `/vol` -- are
    /// synthetic in the same way but report as directories: what a caller
    /// does with them is what it does with any other directory.
    MountPoint,
}

/// A directory entry's last-modified time, decoded from FAT's packed form.
///
/// The VFS's own type rather than the library's: the plan keeps
/// library-specific types out of this API, and a caller printing a date has
/// no use for the packed `u16` pair anyway.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Timestamp {
    /// Decodes FAT's packed date and time.
    ///
    /// `None` for a zero date, which is FAT's "no timestamp" rather than a
    /// date near the epoch -- this firmware writes it whenever the RTC is
    /// not set, and plenty of other tools do too.
    fn from_raw(date: u16, time: u16) -> Option<Self> {
        if date == 0 {
            return None;
        }
        Some(Self {
            year: 1980 + (date >> 9),
            month: ((date >> 5) & 0x0F) as u8,
            day: (date & 0x1F) as u8,
            hour: (time >> 11) as u8,
            minute: ((time >> 5) & 0x3F) as u8,
            // FAT stores seconds in units of two; the odd second is simply
            // not recorded, so this is the real resolution and not a
            // rounding done here.
            second: ((time & 0x1F) * 2) as u8,
        })
    }
}

impl Timestamp {
    /// Decodes an exFAT timestamp.
    ///
    /// The offset field is ignored. exFAT can record one, but FAT cannot,
    /// and this firmware writes local time to FAT volumes; converting only
    /// the exFAT side would make two cards written by the same PC disagree
    /// in a listing.
    fn from_exfat(stamp: &hadris_fat::exfat::ExFatTimestamp) -> Option<Self> {
        if stamp.raw_timestamp() == 0 {
            return None;
        }
        Some(Self {
            year: stamp.year(),
            month: stamp.month(),
            day: stamp.day(),
            hour: stamp.hour(),
            minute: stamp.minute(),
            second: stamp.second(),
        })
    }
}

/// What [`Vfs::metadata`] reports about one path.
///
/// Deliberately not a `DirEntry`: that borrows a name out of a directory
/// listing that only exists for the duration of the call, and a caller
/// asking about a path it already named has no use for the name back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Metadata {
    pub kind: EntryKind,
    /// Zero for a directory, which FAT does not record a length for.
    pub size: u64,
}

pub struct DirEntry<'a> {
    pub name: &'a str,
    pub kind: EntryKind,
    pub size: u64,
    /// When the entry was last written, or `None` if it carries no
    /// timestamp.
    pub modified: Option<Timestamp>,
}

/// How a file is being opened.
///
/// Only three, and each maps onto something the filesystem library can
/// actually do. Its writer starts either at the beginning of a file or at
/// its end and moves forward; there is no seeking within a write. So an
/// exclusive-create mode, or a mode that overwrites the middle of a file,
/// would be a promise the layer below cannot keep, and is not offered rather
/// than being offered and then refused at the point of use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpenMode {
    /// The file must exist. Nothing is written.
    Read,
    /// Create the file, or empty it if it is already there. Writes go from
    /// the beginning.
    Truncate,
    /// Create the file if it is not there. Writes go on the end.
    Append,
}

impl OpenMode {
    fn writable(self) -> bool {
        !matches!(self, OpenMode::Read)
    }
}

/// An open file: a volume, a path and a position, and deliberately not a
/// cursor belonging to the filesystem library. See this module's header.
#[derive(Clone, Copy)]
struct OpenFile {
    volume: VolumeId,
    /// Path within the volume, not within the tree.
    path: Path,
    /// Where the mount was when this was opened, so the handle can be
    /// resolved again without searching by path.
    point: Path,
    offset: u64,
    size: u64,
    mode: OpenMode,
    /// Whether anything has been written through this handle yet.
    ///
    /// The first write of a `Truncate` open is the one that empties the
    /// file; every write after it has to append instead, or each call would
    /// throw away what the last one wrote.
    written: bool,
}

/// Index into the open-file table. Not `Copy` on purpose: a handle that has
/// been closed should not still be lying around in a caller's variable.
pub struct FileHandle(usize);

/// What an open handle refers to, for a listing.
///
/// A copy rather than a borrow of the table entry: the entry is the VFS's
/// own bookkeeping, and handing out references into it would make the
/// open-file table part of the public surface.
#[derive(Clone, Copy)]
pub struct OpenFileInfo {
    /// The mount point the handle was opened through.
    pub point: Path,
    /// Path within the volume, not within the tree.
    pub path: Path,
    pub volume: VolumeId,
    pub offset: u64,
    pub size: u64,
    pub mode: OpenMode,
    /// Whether the volume this was opened against is still mounted and
    /// still the same medium.
    ///
    /// Once this is false every read through the handle fails with
    /// [`FsError::StaleHandle`], and nothing brings it back -- re-mounting
    /// the same stick gives it a new generation, which is the whole point of
    /// generations. Answered from the mount table alone, so asking costs
    /// nothing and works with the medium physically gone.
    pub live: bool,
}

/// What re-checking one mount's medium concluded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// The medium answers as it did when it was mounted.
    Unchanged,
    /// The device is not there. Nothing is concluded about the medium: it
    /// may be the same one behind a transport that is currently down, so the
    /// mount is left alone for a later check to settle.
    DeviceAbsent,
    /// The device answered but its identity could not be gathered -- the
    /// same "not established" as above, reached a different way, and treated
    /// the same.
    Undetermined,
    /// A different medium. The mount has been dropped and any handle on it
    /// now fails.
    Changed,
    /// The medium was physically disconnected at some point since it was
    /// mounted. The mount has been dropped, whatever its identity now says.
    Disconnected,
}

pub fn verdict_name(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Unchanged => "unchanged",
        Verdict::DeviceAbsent => "device absent; mount kept",
        Verdict::Undetermined => "identity not established; mount kept",
        Verdict::Changed => "MEDIA CHANGED; mount dropped",
        Verdict::Disconnected => "DISCONNECTED since mount; mount dropped",
    }
}

pub struct Vfs {
    mounts: [Option<Mount>; MAX_MOUNTS],
    files: [Option<OpenFile>; MAX_OPEN_FILES],
    /// Source of media generations.
    ///
    /// One counter for every slot rather than one per device, because a
    /// generation only has to differ from the ones that came before it in
    /// the same slot, and a single counter gives that for free while also
    /// making two mounts' numbers comparable in a listing.
    next_generation: u32,
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}

impl Vfs {
    pub const fn new() -> Self {
        Self {
            mounts: [None; MAX_MOUNTS],
            files: [None; MAX_OPEN_FILES],
            // Generation 0 is never handed out, so a zero in a handle or a
            // listing is visibly "never established" rather than a real
            // generation that happens to be first.
            next_generation: 1,
        }
    }

    pub fn mounts(&self) -> impl Iterator<Item = &Mount> {
        self.mounts.iter().flatten()
    }

    /// Attaches a volume at `point`.
    ///
    /// The volume is opened once here and thrown away. That is not wasted
    /// work: it is the difference between finding out at mount time that a
    /// partition holds no filesystem this firmware reads, and finding out at
    /// the first `ls` with a mount in the table that never worked.
    pub fn mount(
        &mut self,
        devices: &mut Devices,
        point: &str,
        device: DeviceId,
        partition: Option<u8>,
        range: PartitionRange,
        mode: MountMode,
    ) -> Result<(), FsError> {
        let point = path::normalize(point)?;
        if point.is_root() {
            // The root is the synthetic node that lists the mount points; a
            // volume there would have nowhere to list them.
            return Err(FsError::AlreadyMounted);
        }
        if self.find_mount_exact(&point).is_some() {
            return Err(FsError::AlreadyMounted);
        }
        let slot = self
            .mounts
            .iter()
            .position(Option::is_none)
            .ok_or(FsError::MountTableFull)?;

        // The identity is captured before the volume is opened, so that what
        // gets recorded describes the medium this mount is actually being
        // made against rather than whatever is there by the time the
        // filesystem check finishes.
        let fingerprint = devices
            .fingerprint(device, Some(range.start_lba))
            .ok_or(FsError::DeviceNotPresent)?;

        // The boot sector decides the format, not the MBR type byte, which
        // says only what the tool that made the table intended.
        let format = devices
            .with_device(device, |block| {
                let mut sector = [0u8; 512];
                block.read_blocks(range.start_lba, &mut sector)?;
                match bootsector::identify(&sector) {
                    Some(VolumeKind::Fat) => Ok(VolumeFormat::Fat),
                    Some(VolumeKind::Exfat) => Ok(VolumeFormat::Exfat),
                    None => Err(FsError::NotAFilesystem),
                }
            })
            .ok_or(FsError::DeviceNotPresent)??;

        let location = match device {
            DeviceId::Usb(id) => devices.usb.mass_storage_location(id),
            DeviceId::Ram | DeviceId::Sd => None,
        };
        let candidate = Mount {
            point,
            volume: VolumeId {
                device,
                partition,
                generation: self.next_generation,
            },
            range,
            mode,
            format,
            fingerprint,
            location,
            connection_epoch: devices.usb.connection_epoch_at(location),
        };
        with_volume(devices, &candidate, |_| Ok(()))?;

        self.next_generation += 1;
        self.mounts[slot] = Some(candidate);
        Ok(())
    }

    /// Re-checks every mount's medium against the fingerprint it was made
    /// with, reporting each one through `out`.
    ///
    /// A mount whose medium has changed is dropped, which is what makes its
    /// open handles start failing. A mount whose device is simply absent, or
    /// whose identity could not be gathered, is left exactly as it was:
    /// neither of those establishes that the medium is different, and
    /// tearing a mount down on "we could not tell" would throw away handles
    /// that are about to work again.
    ///
    /// This is explicit rather than automatic. Gathering an identity costs
    /// several bus commands per device, which is not something to do on the
    /// way to reading a file.
    pub fn verify(&mut self, devices: &mut Devices, mut out: impl FnMut(&Mount, Verdict)) {
        for slot in 0..MAX_MOUNTS {
            let Some(mount) = self.mounts[slot] else {
                continue;
            };
            // The physical edge is checked first, and settles the question
            // on its own. An identity that still matches after a disconnect
            // means the same medium came back, not that it never left.
            let moved = match mount.volume.device {
                DeviceId::Usb(id) => {
                    // The port's own count, so unplugging a neighbour on the
                    // same hub leaves this mount alone.
                    devices.usb.connection_epoch_at(mount.location) != mount.connection_epoch
                        || devices.usb.mass_storage_location(id) != mount.location
                }
                DeviceId::Ram | DeviceId::Sd => false,
            };
            let verdict = if moved {
                Verdict::Disconnected
            } else {
                match devices.fingerprint(mount.volume.device, Some(mount.range.start_lba)) {
                    None => Verdict::DeviceAbsent,
                    Some(current) if current.matches(&mount.fingerprint) => Verdict::Unchanged,
                    Some(current) if !current.sources.beyond_geometry() => {
                        // Nothing but a capacity answered. A capacity that
                        // differs is still proof of a different medium; a
                        // capacity that matches proves very little, so it is
                        // reported as undetermined rather than as agreement.
                        if current.block_count == mount.fingerprint.block_count {
                            Verdict::Undetermined
                        } else {
                            Verdict::Changed
                        }
                    }
                    Some(_) => Verdict::Changed,
                }
            };
            if matches!(verdict, Verdict::Changed | Verdict::Disconnected) {
                self.next_generation += 1;
                self.mounts[slot] = None;
            }
            out(&mount, verdict);
        }
    }

    /// Drops every mount whose medium has been physically taken away,
    /// reporting each one through `out`.
    ///
    /// The cheap half of [`Self::verify`]: one pair of integers per mount
    /// and no bus traffic at all, which is what lets automount run it off
    /// the frame loop rather than only when a command asks.
    ///
    /// Only a *removal* counts. A device that is merely absent -- a session
    /// being rebuilt, a transfer that failed -- keeps its mount, because
    /// absence is not evidence that anything was unplugged and a rescan
    /// often brings the same medium straight back. The connection epoch is
    /// the one thing here that reports a physical edge rather than inferring
    /// one from a device that did not answer.
    ///
    /// Open files are not consulted. `umount` refuses a busy mount because
    /// the caller is better placed than the VFS to decide what to do with
    /// its handles, but that argument needs the medium to still be there.
    /// Once the drive is out there is nothing for a handle to go back to, so
    /// the mount goes and the handles start failing -- exactly what
    /// [`Self::verify`] does for a `Disconnected` verdict, reached the same
    /// way.
    pub fn prune_disconnected(&mut self, usb: &UsbHost, mut out: impl FnMut(&Mount)) {
        for slot in 0..MAX_MOUNTS {
            let Some(mount) = self.mounts[slot] else {
                continue;
            };
            if !matches!(mount.volume.device, DeviceId::Usb(_)) {
                continue;
            }
            if usb.connection_epoch_at(mount.location) == mount.connection_epoch {
                continue;
            }
            self.next_generation += 1;
            self.mounts[slot] = None;
            out(&mount);
        }
    }

    /// Detaches the volume at `point`.
    ///
    /// Refuses while any handle is open on it. Dropping the mount out from
    /// under a handle would leave the handle naming a volume the VFS no
    /// longer knows how to reach, and the caller is better placed to decide
    /// whether to close its files than this is to decide for it.
    pub fn umount(&mut self, point: &str) -> Result<(), FsError> {
        let point = path::normalize(point)?;
        let slot = self.find_mount_exact(&point).ok_or(FsError::NotMounted)?;
        let volume = self.mounts[slot].as_ref().expect("checked").volume;
        if self
            .files
            .iter()
            .flatten()
            .any(|file| file.volume == volume)
        {
            return Err(FsError::Busy);
        }
        self.mounts[slot] = None;
        Ok(())
    }

    fn find_mount_exact(&self, point: &Path) -> Option<usize> {
        self.mounts.iter().position(|mount| {
            mount
                .as_ref()
                .is_some_and(|mount| mount.point.as_str() == point.as_str())
        })
    }

    /// The mount covering `path`, and the path within its volume.
    ///
    /// The longest matching mount point wins, so a volume mounted inside
    /// another mount's subtree would take precedence there. Nothing creates
    /// such a layout today; the rule is fixed now so that adding one later
    /// does not change how existing paths resolve.
    fn resolve(&self, path: &Path) -> Option<(Mount, Path)> {
        let mut best: Option<(Mount, Path)> = None;
        for mount in self.mounts.iter().flatten() {
            let Some(within) = path.strip_prefix(&mount.point) else {
                continue;
            };
            let longer = best.as_ref().is_none_or(|(current, _)| {
                mount.point.as_str().len() > current.point.as_str().len()
            });
            if longer {
                best = Some((*mount, within));
            }
        }
        best
    }

    /// Lists a directory, calling `out` once per entry.
    ///
    /// A callback rather than an iterator because the entries live inside a
    /// volume that only exists for the duration of this call, and a returned
    /// iterator would have to keep that volume alive -- which is exactly the
    /// borrow this design is avoiding.
    pub fn list(
        &self,
        devices: &mut Devices,
        path: &str,
        mut out: impl FnMut(DirEntry<'_>),
    ) -> Result<(), FsError> {
        let path = path::normalize(path)?;

        // Above the volumes the tree belongs to no medium, so there is
        // nothing to open: the entries are the mount table, read sideways.
        let Some((mount, within)) = self.resolve(&path) else {
            return self.list_tree(&path, out);
        };
        with_volume(devices, &mount, |volume| match volume {
            AnyVolume::Fat(volume) => {
                let directory = match lookup(volume, &within)? {
                    None => volume.root_dir(),
                    Some(entry) => {
                        if !entry.is_directory() {
                            return Err(FsError::NotADirectory);
                        }
                        volume
                            .open_dir_entry(&entry)
                            .map_err(|_| FsError::NotADirectory)?
                    }
                };
                let mut entries = directory.entries();
                while let Some(entry) = entries.next_entry() {
                    let entry = entry.map_err(|_| FsError::NotAFilesystem)?;
                    let Some(file) = entry.as_entry() else {
                        continue;
                    };
                    let (date, time, _) = file.modified().to_raw();
                    out(DirEntry {
                        name: &entry.name(),
                        kind: if file.is_directory() {
                            EntryKind::Directory
                        } else {
                            EntryKind::File
                        },
                        size: file.len(),
                        modified: Timestamp::from_raw(date, time),
                    });
                }
                Ok(())
            }
            AnyVolume::Exfat(volume) => {
                // exFAT resolves paths itself, so there is no walk to do
                // here -- only the root has to be named separately, since
                // it has no entry to open.
                let directory = if within.is_root() {
                    volume.root_dir()
                } else {
                    volume
                        .open_dir(within.as_str())
                        .map_err(|_| FsError::NotADirectory)?
                };
                for entry in directory.entries() {
                    let entry = entry.map_err(|_| FsError::NotAFilesystem)?;
                    out(DirEntry {
                        name: &entry.name,
                        kind: if entry.is_directory() {
                            EntryKind::Directory
                        } else {
                            EntryKind::File
                        },
                        // The content length, not the allocated one: a file
                        // occupying whole clusters is still only as long as
                        // its valid data.
                        size: entry.valid_data_length,
                        modified: Timestamp::from_exfat(&entry.modified),
                    });
                }
                Ok(())
            }
        })
    }

    /// Lists a node of the tree that sits above the volumes: the root, or
    /// one of the directories a mount point passes through on its way down.
    ///
    /// `/vol` is on no medium. It exists because something is mounted under
    /// it and stops existing when the last of those goes. Synthesizing it
    /// here is what makes the tree navigable by the names it shows: the
    /// root used to list `sd0p1` for a volume that only answers to
    /// `/vol/sd0p1`, so the one name a listing offered was the one a
    /// caller could not then use.
    fn list_tree(&self, path: &Path, mut out: impl FnMut(DirEntry<'_>)) -> Result<(), FsError> {
        let mut found = false;
        for (index, mount) in self.mounts.iter().enumerate() {
            let Some(mount) = mount else {
                continue;
            };
            let Some((name, is_point)) = tree_child(&mount.point, path) else {
                continue;
            };
            found = true;
            // Two volumes under one node contribute a single entry: both
            // `/vol/sd0p1` and `/vol/usb0p1` put `vol` under the root. The
            // earlier mount is the one that emits it, so the order stays
            // the mount table's, which is the order things were mounted in.
            let earlier = self.mounts[..index].iter().flatten().any(|earlier| {
                tree_child(&earlier.point, path)
                    .is_some_and(|(earlier, _)| path::names_equal(earlier, name))
            });
            if earlier {
                continue;
            }
            out(DirEntry {
                name,
                kind: if is_point {
                    EntryKind::MountPoint
                } else {
                    EntryKind::Directory
                },
                size: 0,
                // Nothing here is an object on a volume, so there is
                // nothing whose modification time this could be.
                modified: None,
            });
        }
        // The root is there with nothing mounted at all; every other node
        // of the tree exists only for as long as something is under it.
        if found || path.is_root() {
            Ok(())
        } else {
            Err(FsError::NotMounted)
        }
    }

    /// Whether `path` names a directory of the tree above the volumes.
    fn is_tree_node(&self, path: &Path) -> bool {
        path.is_root()
            || self
                .mounts
                .iter()
                .flatten()
                .any(|mount| tree_child(&mount.point, path).is_some())
    }

    /// What is at `path`, without listing anything.
    ///
    /// `cd` is the caller this exists for. Asking `list` whether a path is a
    /// directory would answer by walking every entry in it, which is a great
    /// deal of work to establish one bit -- and gives the wrong answer for a
    /// path that is a file, since a file simply lists as nothing.
    pub fn metadata(&self, devices: &mut Devices, path: &str) -> Result<Metadata, FsError> {
        let path = path::normalize(path)?;
        let Some((mount, within)) = self.resolve(&path) else {
            if self.is_tree_node(&path) {
                return Ok(Metadata {
                    kind: EntryKind::Directory,
                    size: 0,
                });
            }
            return Err(FsError::NotMounted);
        };
        with_volume(devices, &mount, |volume| match volume {
            AnyVolume::Fat(volume) => match lookup(volume, &within)? {
                // The volume's own root, which has no directory entry to
                // report and is a directory whether or not it holds one.
                None => Ok(Metadata {
                    kind: EntryKind::Directory,
                    size: 0,
                }),
                Some(entry) => Ok(Metadata {
                    kind: if entry.is_directory() {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    },
                    size: entry.len(),
                }),
            },
            AnyVolume::Exfat(volume) => {
                if within.is_root() {
                    return Ok(Metadata {
                        kind: EntryKind::Directory,
                        size: 0,
                    });
                }
                let entry = volume
                    .open_path(within.as_str())
                    .map_err(|_| FsError::NotFound)?;
                Ok(Metadata {
                    kind: if entry.is_directory() {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    },
                    size: entry.valid_data_length,
                })
            }
        })
    }

    /// Creates a directory at `path`, whose parent must already exist.
    ///
    /// Only the last component is created, which is the rule the file path
    /// already follows. A caller naming a parent that is not there has
    /// almost always mistyped it, and building the whole chain silently
    /// would turn that typo into a directory tree.
    ///
    /// FAT only. exFAT is read-only here for the reasons in
    /// [`VolumeFormat::Exfat`], and so is every mount that is not the RAM
    /// disk.
    pub fn create_dir(&mut self, devices: &mut Devices, path: &str) -> Result<(), FsError> {
        let path = path::normalize(path)?;
        let Some((mount, within)) = self.resolve(&path) else {
            // Either nothing is mounted here, or this is a node of the tree
            // above the volumes -- which is made by mounting something, not
            // by asking for a directory.
            return Err(FsError::NotMounted);
        };
        if mount.mode == MountMode::ReadOnly || mount.format == VolumeFormat::Exfat {
            return Err(FsError::ReadOnly);
        }
        if within.is_root() {
            // The volume's root, which is already there.
            return Err(FsError::AlreadyExists);
        }

        // One reading of the clock for the whole operation. See
        // `super::clock`.
        clock::sample();
        let outcome = with_volume(devices, &mount, |volume| {
            let AnyVolume::Fat(volume) = volume else {
                // Unreachable: the format was checked above.
                return Err(FsError::ReadOnly);
            };
            create_directory(volume, &within)?;
            volume.sync().map_err(map_write_error)
        });
        clock::clear();
        outcome
    }

    /// Opens a file for reading.
    ///
    /// There is no mode argument because there is only one mode. Creating,
    /// truncating and appending arrive with the write path; until then an
    /// open that could do any of them would be a promise this cannot keep.
    pub fn open(
        &mut self,
        devices: &mut Devices,
        path: &str,
        mode: OpenMode,
    ) -> Result<FileHandle, FsError> {
        let path = path::normalize(path)?;
        let (mount, within) = self.resolve(&path).ok_or(FsError::NotMounted)?;
        // Refused here rather than at the first write, so a caller that
        // cannot write finds out while it still has nothing invested.
        if mode.writable()
            && (mount.mode == MountMode::ReadOnly || mount.format == VolumeFormat::Exfat)
        {
            return Err(FsError::ReadOnly);
        }
        let slot = self
            .files
            .iter()
            .position(Option::is_none)
            .ok_or(FsError::TooManyOpenFiles)?;

        let size = with_volume(devices, &mount, |volume| match volume {
            AnyVolume::Fat(volume) => match lookup(volume, &within) {
                Ok(Some(entry)) if entry.is_directory() => Err(FsError::NotAFile),
                Ok(Some(entry)) => Ok(entry.len()),
                Ok(None) => Err(FsError::NotAFile),
                // A missing file is only an error for a mode that is not
                // there to create one.
                Err(FsError::NotFound) if mode.writable() => {
                    create(volume, &within)?;
                    Ok(0)
                }
                Err(error) => Err(error),
            },
            AnyVolume::Exfat(volume) => {
                // Nothing to create: an exFAT mount is read-only, and `open`
                // has already refused a writable mode above.
                let entry = volume
                    .open_path(within.as_str())
                    .map_err(|_| FsError::NotFound)?;
                if entry.is_directory() {
                    return Err(FsError::NotAFile);
                }
                Ok(entry.valid_data_length)
            }
        })?;

        self.files[slot] = Some(OpenFile {
            volume: mount.volume,
            path: within,
            point: mount.point,
            // `Truncate` leaves the existing bytes in place until the first
            // write, so the size is what is on disk now either way.
            offset: if mode == OpenMode::Append { size } else { 0 },
            size,
            mode,
            written: false,
        });
        Ok(FileHandle(slot))
    }

    pub fn close(&mut self, handle: FileHandle) {
        self.files[handle.0] = None;
    }

    pub fn size(&self, handle: &FileHandle) -> Result<u64, FsError> {
        Ok(self.files[handle.0].ok_or(FsError::StaleHandle)?.size)
    }

    /// What `handle` names, and whether it still reaches anything.
    ///
    /// The liveness test is the same one [`Self::read`] applies before it
    /// touches the medium -- the mount point still holds a volume, and that
    /// volume is the same generation the handle was opened against -- so a
    /// listing and the next read agree without the listing having to run a
    /// read to find out.
    pub fn describe(&self, handle: &FileHandle) -> Result<OpenFileInfo, FsError> {
        let file = self.files[handle.0].ok_or(FsError::StaleHandle)?;
        let live = self
            .find_mount_exact(&file.point)
            .and_then(|slot| self.mounts[slot])
            .is_some_and(|mount| mount.volume == file.volume);
        Ok(OpenFileInfo {
            point: file.point,
            path: file.path,
            volume: file.volume,
            offset: file.offset,
            size: file.size,
            mode: file.mode,
            live,
        })
    }

    pub fn seek(&mut self, handle: &FileHandle, offset: u64) -> Result<u64, FsError> {
        let file = self.files[handle.0].as_mut().ok_or(FsError::StaleHandle)?;
        file.offset = offset;
        Ok(offset)
    }

    /// Reads from the handle's current position, advancing it by what was
    /// actually read. Returns 0 at end of file.
    ///
    /// The position advances only after a successful read. A read that fails
    /// part way leaves the handle exactly where it was, so the caller can
    /// retry -- after a transport recovery, say -- from a position it knows
    /// is still correct rather than one that counted bytes it never got.
    pub fn read(
        &mut self,
        devices: &mut Devices,
        handle: &FileHandle,
        buffer: &mut [u8],
    ) -> Result<usize, FsError> {
        let file = self.files[handle.0].ok_or(FsError::StaleHandle)?;
        // The mount is looked up again, and its identity compared, rather
        // than trusted from open time: between then and now the medium may
        // have been unmounted, or replaced by another that took the same
        // name.
        let slot = self
            .find_mount_exact(&file.point)
            .ok_or(FsError::StaleHandle)?;
        let mount = self.mounts[slot].expect("checked");
        if mount.volume != file.volume {
            return Err(FsError::StaleHandle);
        }
        if buffer.is_empty() || file.offset >= file.size {
            return Ok(0);
        }

        let count = with_volume(devices, &mount, |volume| {
            let AnyVolume::Fat(volume) = volume else {
                return read_exfat(volume, &file, buffer);
            };
            let entry = lookup(volume, &file.path)?.ok_or(FsError::NotFound)?;
            let mut reader = volume.read_file(&entry).map_err(|_| FsError::NotAFile)?;
            // The library's reader starts at the beginning of the file and
            // has no seek, so the offset is reached by reading and
            // discarding. That is linear in the offset, which is why the
            // caller is better off with one large buffer than many small
            // ones; a cursor that could be resumed cheaply is the thing the
            // handle design gives up in exchange for surviving a rescan.
            let mut skipped = 0u64;
            let mut discard = [0u8; 512];
            while skipped < file.offset {
                let want = discard.len().min((file.offset - skipped) as usize);
                let read = reader
                    .read(&mut discard[..want])
                    .map_err(|_| FsError::Block(BlockError::DeviceError))?;
                if read == 0 {
                    return Ok(0);
                }
                skipped += read as u64;
            }
            reader
                .read(buffer)
                .map_err(|_| FsError::Block(BlockError::DeviceError))
        })?;

        if let Some(file) = self.files[handle.0].as_mut() {
            file.offset += count as u64;
        }
        Ok(count)
    }

    /// Writes at the handle's position, advancing it by what was written.
    ///
    /// Only two positions are writable, because only two are reachable: the
    /// beginning of a file opened to be replaced, and the end of one. The
    /// library's writer moves forward from wherever it starts and cannot
    /// seek, so a write into the middle is refused rather than emulated by
    /// reading the file back and rewriting it -- which is a different
    /// operation with different failure modes, and not one anything here
    /// needs.
    ///
    /// The position advances only on success, for the same reason `read`'s
    /// does: a partial write leaves the handle where the caller can decide
    /// what to do, not counting bytes that never landed.
    pub fn write(
        &mut self,
        devices: &mut Devices,
        handle: &FileHandle,
        buffer: &[u8],
    ) -> Result<usize, FsError> {
        let file = self.files[handle.0].ok_or(FsError::StaleHandle)?;
        if !file.mode.writable() {
            return Err(FsError::ReadOnly);
        }
        let slot = self
            .find_mount_exact(&file.point)
            .ok_or(FsError::StaleHandle)?;
        let mount = self.mounts[slot].expect("checked");
        if mount.volume != file.volume {
            return Err(FsError::StaleHandle);
        }
        if mount.mode == MountMode::ReadOnly || mount.format == VolumeFormat::Exfat {
            return Err(FsError::ReadOnly);
        }
        if buffer.is_empty() {
            return Ok(0);
        }

        // The first write of a truncating open starts the file over; every
        // other write goes on the end. Anything else is a position the
        // writer cannot be put at.
        let from_start = file.mode == OpenMode::Truncate && !file.written;
        if !from_start && file.offset != file.size {
            return Err(FsError::NotSeekable);
        }

        // One reading of the clock for the whole operation, taken before any
        // of it starts. See `super::clock`.
        clock::sample();
        let outcome = with_volume(devices, &mount, |volume| {
            let AnyVolume::Fat(volume) = volume else {
                // Unreachable: the mount's format was checked above. Kept as
                // a refusal rather than a panic, because "this volume is not
                // writable" is a true statement whatever led here.
                return Err(FsError::ReadOnly);
            };
            let entry = lookup(volume, &file.path)?.ok_or(FsError::NotFound)?;
            let mut writer = if from_start {
                FileWriter::new(volume, &entry).map_err(|_| FsError::NotAFile)?
            } else {
                FileWriter::new_append(volume, &entry).map_err(|_| FsError::NotAFile)?
            };
            let count = writer.write(buffer).map_err(map_write_error)?;
            // `finish` is what commits the size and timestamps into the
            // directory entry. Without it the bytes are on the medium and
            // nothing points at them, so its failure is the write's failure
            // even though every byte reached the disk.
            writer.finish().map_err(map_write_error)?;
            volume.sync().map_err(map_write_error)?;
            Ok(count)
        });
        clock::clear();
        let count = outcome?;

        if let Some(file) = self.files[handle.0].as_mut() {
            if from_start {
                // The truncating write replaced the file, so its size is
                // exactly what was just written -- not what it was before.
                file.size = count as u64;
                file.offset = count as u64;
            } else {
                file.size += count as u64;
                file.offset = file.size;
            }
            file.written = true;
        }
        Ok(count)
    }

    /// Runs `body` with a sink that writes straight into `path`, keeping the
    /// volume and one `FileWriter` open for the whole of it.
    ///
    /// This exists because [`Self::write`] is the wrong shape for a stream.
    /// A handle holds no library cursor -- that is what lets it survive a
    /// rescan -- so every `write` opens the volume, walks the directory,
    /// builds a `FileWriter` (whose `new_append` finds the end of the file
    /// by walking the FAT chain), commits the entry and syncs. All of that
    /// is per call, so the cost follows the *number of writes*: writing a
    /// file in pieces half the size takes about twice as long.
    ///
    /// The chain walk also makes that per-call cost grow with the file, so
    /// the total has an `n^2` term in it. Measured on the 8 MiB RAM disk it
    /// does not show: doubling the size doubles the time, because the fixed
    /// part of each call dominates the walk at that many clusters. It is the
    /// fixed part that this avoids.
    ///
    /// Inside one call there is nothing to survive. `ls` and `cat` already
    /// hold a volume open for the length of one operation; what a handle has
    /// to outlive is the gap *between* operations. So the writer is built
    /// once, `write` is called on it repeatedly -- which is linear, and is
    /// how the library expects to be used -- and everything is torn down at
    /// the end.
    ///
    /// The bytes that arrived are committed even when `body` fails. Leaving
    /// them uncommitted would put clusters on the medium that no directory
    /// entry points at, and with no way to reach them there would be no way
    /// to get them back either. Committing means the caller can look at the
    /// partial file, or remove it and reclaim the space; both need it to
    /// exist. What stopped the sink comes back in
    /// [`StreamWrite::interrupted`].
    pub fn write_stream<T>(
        &mut self,
        devices: &mut Devices,
        path: &str,
        mode: OpenMode,
        body: impl FnOnce(&mut dyn FnMut(&[u8]) -> bool) -> T,
    ) -> Result<StreamWrite<T>, FsError> {
        if !mode.writable() {
            return Err(FsError::ReadOnly);
        }
        let path = path::normalize(path)?;
        let (mount, within) = self.resolve(&path).ok_or(FsError::NotMounted)?;
        if mount.mode == MountMode::ReadOnly || mount.format == VolumeFormat::Exfat {
            return Err(FsError::ReadOnly);
        }

        // One reading of the clock for the whole transfer, as everywhere
        // else that writes. See `super::clock`.
        clock::sample();
        let outcome = with_volume(devices, &mount, |volume| {
            let AnyVolume::Fat(volume) = volume else {
                return Err(FsError::ReadOnly);
            };
            // `lookup` reports a missing entry as `Err(NotFound)`, not as
            // `Ok(None)` -- which it answers only for the volume's root.
            // Creating on the `None` arm therefore never creates anything,
            // and every write to a file that is not there yet fails.
            let entry = match lookup(volume, &within) {
                Ok(Some(entry)) if entry.is_directory() => return Err(FsError::NotAFile),
                Ok(Some(entry)) => entry,
                Ok(None) => return Err(FsError::NotAFile),
                Err(FsError::NotFound) => {
                    create(volume, &within)?;
                    lookup(volume, &within)?.ok_or(FsError::NotAFile)?
                }
                Err(error) => return Err(error),
            };
            let mut writer = match mode {
                OpenMode::Truncate => {
                    FileWriter::new(volume, &entry).map_err(|_| FsError::NotAFile)?
                }
                // `Read` was refused above; this is `Append`.
                _ => FileWriter::new_append(volume, &entry).map_err(|_| FsError::NotAFile)?,
            };

            let mut written = 0u64;
            let mut interrupted = None;
            // Scoped so the sink's borrow of `writer` is over before the
            // commit below needs it.
            let value = {
                let mut sink = |bytes: &[u8]| {
                    let mut offset = 0;
                    while offset < bytes.len() {
                        match writer.write(&bytes[offset..]) {
                            // The library takes what it can and says so; a
                            // zero-length take with bytes still in hand is a
                            // volume with nowhere left to put them.
                            Ok(0) => {
                                interrupted = Some(FsError::NoSpace);
                                return false;
                            }
                            Ok(count) => {
                                offset += count;
                                written += count as u64;
                            }
                            Err(error) => {
                                interrupted = Some(map_write_error(error));
                                return false;
                            }
                        }
                    }
                    true
                };
                body(&mut sink)
            };

            // `finish` is what puts the size and timestamp in the directory
            // entry, so its failure is the write's failure however many
            // bytes reached the medium.
            writer.finish().map_err(map_write_error)?;
            volume.sync().map_err(map_write_error)?;
            Ok(StreamWrite {
                value,
                written,
                interrupted,
            })
        });
        clock::clear();
        outcome
    }

    /// Removes a file.
    ///
    /// Refuses while a handle is open on it, for the same reason `umount`
    /// refuses a busy mount: the caller knows what its own handles are for,
    /// and a read through one after this would fail in a way that looks like
    /// a damaged volume rather than like a file somebody deleted.
    pub fn remove_file(&mut self, devices: &mut Devices, path: &str) -> Result<(), FsError> {
        self.remove(devices, path, EntryKind::File)
    }

    /// Removes an empty directory.
    ///
    /// Only an empty one: the library refuses the rest, and recursive
    /// deletion is a different operation -- one whose failure halfway
    /// through leaves a shape nobody asked for.
    pub fn remove_dir(&mut self, devices: &mut Devices, path: &str) -> Result<(), FsError> {
        self.remove(devices, path, EntryKind::Directory)
    }

    fn remove(
        &mut self,
        devices: &mut Devices,
        path: &str,
        kind: EntryKind,
    ) -> Result<(), FsError> {
        let path = path::normalize(path)?;
        let (mount, within) = self.writable_mount(&path)?;
        if within.is_root() {
            // The volume's root is the mount, not an entry in it.
            return Err(FsError::Busy);
        }
        if self.is_open(mount.volume, &within) {
            return Err(FsError::Busy);
        }
        clock::sample();
        let outcome = with_volume(devices, &mount, |volume| {
            let AnyVolume::Fat(volume) = volume else {
                return Err(FsError::ReadOnly);
            };
            let entry = lookup(volume, &within)?.ok_or(FsError::NotFound)?;
            // Asked for by kind so that `rm` cannot take a directory and
            // `rmdir` cannot take a file. The library would delete either.
            match kind {
                EntryKind::File if entry.is_directory() => return Err(FsError::NotAFile),
                EntryKind::Directory if !entry.is_directory() => {
                    return Err(FsError::NotADirectory);
                }
                _ => {}
            }
            volume.delete(&entry).map_err(map_write_error)?;
            volume.sync().map_err(map_write_error)?;
            Ok(())
        });
        clock::clear();
        outcome
    }

    /// Renames `from` to `to`, which may move it to another directory on the
    /// same volume.
    ///
    /// Only the directory entry moves; the cluster chain stays where it is.
    /// That is what makes it cheap, and also what makes crossing volumes
    /// impossible -- there the bytes would have to be copied, which is a
    /// different operation with a different way of failing halfway.
    ///
    /// `to` must not exist. Replacing it would be a remove and a rename with
    /// a window in between where neither name works, and a caller that wants
    /// that is better placed to decide when to take the risk.
    pub fn rename(&mut self, devices: &mut Devices, from: &str, to: &str) -> Result<(), FsError> {
        let from = path::normalize(from)?;
        let to = path::normalize(to)?;
        let (mount, source) = self.resolve(&from).ok_or(FsError::NotMounted)?;
        let (destination_mount, destination) = self.resolve(&to).ok_or(FsError::NotMounted)?;
        // Asked before whether either side is writable, so that a move
        // between volumes says so whichever of them happens to be read-only.
        // The other order made the answer depend on that -- and since `/tmp`
        // is the only writable volume, every cross-volume move would have
        // been reported as `read-only` instead, which is true but is not the
        // reason it cannot work.
        if destination_mount.volume != mount.volume {
            return Err(FsError::CrossVolume);
        }
        // One volume, so one check.
        if mount.mode == MountMode::ReadOnly || mount.format == VolumeFormat::Exfat {
            return Err(FsError::ReadOnly);
        }
        if source.is_root() || destination.is_root() {
            return Err(FsError::Busy);
        }
        if self.is_open(mount.volume, &source) {
            return Err(FsError::Busy);
        }
        let name = destination.file_name().ok_or(FsError::NotAFile)?;
        clock::sample();
        let outcome = with_volume(devices, &mount, |volume| {
            let AnyVolume::Fat(volume) = volume else {
                return Err(FsError::ReadOnly);
            };
            let entry = lookup(volume, &source)?.ok_or(FsError::NotFound)?;
            let parent = parent_of(volume, &destination)?;
            volume
                .rename(&entry, &parent, name)
                .map_err(map_write_error)?;
            volume.sync().map_err(map_write_error)?;
            Ok(())
        });
        clock::clear();
        outcome
    }

    /// The mount covering `path`, refusing one that cannot be written to.
    ///
    /// The same two-line check that opens every write path, kept in one
    /// place so a new one cannot forget half of it.
    fn writable_mount(&self, path: &Path) -> Result<(Mount, Path), FsError> {
        let (mount, within) = self.resolve(path).ok_or(FsError::NotMounted)?;
        if mount.mode == MountMode::ReadOnly || mount.format == VolumeFormat::Exfat {
            return Err(FsError::ReadOnly);
        }
        Ok((mount, within))
    }

    /// Whether a handle is open on this exact file.
    fn is_open(&self, volume: VolumeId, path: &Path) -> bool {
        // Both are normalized paths within the same volume, so the
        // separators line up and only the names differ in case.
        self.files.iter().flatten().any(|file| {
            file.volume == volume && path::names_equal(file.path.as_str(), path.as_str())
        })
    }
}

/// What [`Vfs::write_stream`] did.
pub struct StreamWrite<T> {
    /// Whatever the body returned. The body is where the transfer lives, so
    /// this is usually its own success or failure.
    pub value: T,
    /// Bytes written and committed to the directory entry.
    pub written: u64,
    /// What stopped the sink taking more, if anything did. The bytes before
    /// it are still on the volume: see [`Vfs::write_stream`].
    pub interrupted: Option<FsError>,
}

/// Turns a filesystem library error into this layer's vocabulary.
///
/// Most of the library's variants describe on-disk damage in more detail
/// than a caller of `write` can act on; what a caller can act on is whether
/// the volume is full, whether the path named something writable, and
/// whether the medium itself failed. A block error that travelled down
/// through the byte stream comes back out here rather than being flattened,
/// so a write that failed because a card was pulled says so.
fn map_write_error(error: FatError) -> FsError {
    match error {
        FatError::NoFreeSpace | FatError::DirectoryFull => FsError::NoSpace,
        FatError::NotAFile => FsError::NotAFile,
        FatError::NotADirectory => FsError::NotADirectory,
        FatError::EntryNotFound => FsError::NotFound,
        FatError::InvalidFilename | FatError::InvalidShortFilename | FatError::InvalidPath => {
            FsError::Path(PathError::InvalidCharacter)
        }
        FatError::AlreadyExists => FsError::AlreadyExists,
        // Everything left is either an I/O failure or the volume not being
        // what it claimed. Neither is something the caller distinguishes by
        // acting differently, and both mean the same thing: this volume did
        // not do what was asked.
        _ => FsError::NotAFilesystem,
    }
}

/// Creates an empty file at `path`, whose parent directory must exist.
///
/// Only the last component is created. Creating the directories above it
/// would be `mkdir -p`, which is a different operation and is not in this
/// VFS's range -- a caller that names a directory that is not there has
/// almost always mistyped it, and silently building the path would hide
/// that.
fn create<DATA: Read + Write + Seek>(volume: &FatVolume<DATA>, path: &Path) -> Result<(), FsError> {
    let name = path.file_name().ok_or(FsError::NotAFile)?;
    let parent = parent_of(volume, path)?;
    volume.create_file(&parent, name).map_err(map_write_error)?;
    Ok(())
}

/// Creates a directory at `path`, whose parent directory must exist.
///
/// The library writes the `.` and `..` entries into the new directory, so
/// what comes back is a directory a PC will also accept -- which matters
/// here, since the point of writing to a medium is usually to read it
/// somewhere else.
fn create_directory<DATA: Read + Write + Seek>(
    volume: &FatVolume<DATA>,
    path: &Path,
) -> Result<(), FsError> {
    let name = path.file_name().ok_or(FsError::NotADirectory)?;
    let parent = parent_of(volume, path)?;
    volume.create_dir(&parent, name).map_err(map_write_error)?;
    Ok(())
}

/// The directory holding `path`.
fn parent_of<'a, DATA: Read + Write + Seek>(
    volume: &'a FatVolume<DATA>,
    path: &Path,
) -> Result<FatDir<'a, DATA>, FsError> {
    match path.parent() {
        // The file sits directly in the volume's root, which is by far the
        // common case and is also the one there is no entry to look up.
        None => Ok(volume.root_dir()),
        Some(parent) if parent.is_root() => Ok(volume.root_dir()),
        Some(parent) => {
            let entry = lookup(volume, &parent)?.ok_or(FsError::NotADirectory)?;
            volume
                .open_dir_entry(&entry)
                .map_err(|_| FsError::NotADirectory)
        }
    }
}

/// Reads from an exFAT file at the handle's offset.
///
/// Unlike the FAT reader, exFAT's can seek, so reaching an offset costs one
/// seek instead of reading and discarding everything before it. `read` on a
/// FAT volume is linear in the offset for that reason and this one is not.
fn read_exfat(
    volume: &mut AnyVolume<'_>,
    file: &OpenFile,
    buffer: &mut [u8],
) -> Result<usize, FsError> {
    let AnyVolume::Exfat(volume) = volume else {
        return Err(FsError::NotAFilesystem);
    };
    let entry = volume
        .open_path(file.path.as_str())
        .map_err(|_| FsError::NotFound)?;
    let mut reader = ExFatFileReader::new(volume, &entry).map_err(|_| FsError::NotAFile)?;
    reader
        .seek(SeekFrom::Start(file.offset))
        .map_err(|_| FsError::Block(BlockError::DeviceError))?;
    reader
        .read(buffer)
        .map_err(|_| FsError::Block(BlockError::DeviceError))
}

/// An open volume of either format.
///
/// The two library types share no trait, and wrapping them in one would mean
/// inventing an abstraction over two filesystems this firmware handles quite
/// differently -- one writable, one not; one needing a path walked by hand,
/// one resolving paths itself. An enum keeps the difference visible at each
/// of the four places that has to care.
enum AnyVolume<'d> {
    Fat(FatVolume<BlockStream<'d>>),
    Exfat(ExFatVolume<BlockStream<'d>>),
}

/// Builds the volume for `mount` and runs `body` against it.
///
/// Everything the filesystem library touches is created and destroyed inside
/// this call: the device borrow, the partition view, the byte stream and its
/// cache, and the volume itself.
fn with_volume<T>(
    devices: &mut Devices,
    mount: &Mount,
    body: impl FnOnce(&mut AnyVolume<'_>) -> Result<T, FsError>,
) -> Result<T, FsError> {
    // A physical disconnect is free to check -- one integer -- so unlike the
    // full identity it is checked on every operation rather than only when
    // `verify` is asked for.
    if let DeviceId::Usb(id) = mount.volume.device {
        if devices.usb.connection_epoch_at(mount.location) != mount.connection_epoch {
            return Err(FsError::Block(BlockError::MediaRemoved));
        }
        // Also free, and what catches the drive having been moved to another
        // port -- a physical move, and so a different mount -- even when the
        // two ports held media of exactly the same size.
        if devices.usb.mass_storage_location(id) != mount.location {
            return Err(FsError::Block(BlockError::MediaChanged));
        }
    }
    let outcome = devices.with_device(mount.volume.device, |device| {
        // The one identity check cheap enough to run on every operation.
        // Resolving the device already costs a READ CAPACITY on USB and
        // nothing at all on SD, so comparing the geometry is free -- and it
        // catches the most common swap there is, a card of a different size
        // in the same slot, without the several SCSI commands a full
        // fingerprint would need. A medium that changed to one of exactly
        // the same size gets past this and is caught by `Vfs::verify`.
        let geometry = device.geometry();
        if geometry.block_bytes != mount.fingerprint.block_bytes
            || geometry.block_count != mount.fingerprint.block_count
        {
            return Err(FsError::Block(BlockError::MediaChanged));
        }
        let mut partition = PartitionBlockDevice::new(device, mount.range)?;
        let stream = BlockStream::new(&mut partition);
        let mut volume = match mount.format {
            // Built rather than opened, so the volume stamps directory
            // entries from the RTC instead of from the library's `no_std`
            // default, which is a fixed 1980-01-01. See `super::clock`.
            VolumeFormat::Fat => AnyVolume::Fat(
                FatVolume::builder(stream)
                    .time_provider(&clock::PROVIDER)
                    .open()
                    .map_err(|_| FsError::NotAFilesystem)?,
            ),
            // No clock is configured for exFAT: nothing here writes to one,
            // so there is no entry for a timestamp to end up in.
            VolumeFormat::Exfat => {
                AnyVolume::Exfat(ExFatVolume::open(stream).map_err(|_| FsError::NotAFilesystem)?)
            }
        };
        body(&mut volume)
    });
    outcome.ok_or(FsError::DeviceNotPresent)?
}

/// The child of `parent` that `point` lies under: the first component of
/// `point` below `parent`, and whether that child is `point` itself.
///
/// `None` when `point` is not below `parent` at all, and also when the two
/// are equal -- a node is not its own child. Matching is by whole
/// components, so `/tmpfiles` is not below `/tmp`.
fn tree_child<'a>(point: &'a Path, parent: &Path) -> Option<(&'a str, bool)> {
    let text = point.as_str();
    let below = if parent.is_root() {
        text
    } else {
        let prefix = parent.as_str();
        if !text.starts_with(prefix) || text.as_bytes().get(prefix.len()) != Some(&b'/') {
            return None;
        }
        &text[prefix.len()..]
    };
    // A canonical non-root path starts with `/` and has no trailing one, so
    // what is left is either empty or `/name` followed by the rest.
    let below = below.strip_prefix('/')?;
    match below.find('/') {
        Some(cut) => Some((&below[..cut], false)),
        None => Some((below, true)),
    }
}

/// Walks `path` from the volume's root.
///
/// `Ok(None)` is the root directory itself, which has no directory entry to
/// return and is not an error.
/// Walks `path` from the volume's root and answers with its entry.
///
/// The two "nothing here" answers mean different things, and callers have to
/// keep them apart:
///
/// - `Err(FsError::NotFound)` -- a component of the path is not there. This
///   is what a caller that is about to create the file looks for.
/// - `Ok(None)` -- the path *is* the volume's root, which has no directory
///   entry of its own. Not a missing file, and not something to create.
fn lookup<DATA: Read + Seek>(
    volume: &FatVolume<DATA>,
    path: &Path,
) -> Result<Option<FileEntry>, FsError> {
    if path.is_root() {
        return Ok(None);
    }
    let mut directory = volume.root_dir();
    let mut found: Option<FileEntry> = None;
    let mut components = path.components().peekable();

    while let Some(component) = components.next() {
        // Anything already found has to be a directory for the walk to
        // continue into it; this is what turns `/tmp/FILE.TXT/x` into
        // `NotADirectory` rather than `NotFound`.
        if let Some(entry) = &found {
            if !entry.is_directory() {
                return Err(FsError::NotADirectory);
            }
            directory = volume
                .open_dir_entry(entry)
                .map_err(|_| FsError::NotADirectory)?;
        }

        let mut entries = directory.entries();
        let mut matched = None;
        while let Some(entry) = entries.next_entry() {
            let entry = entry.map_err(|_| FsError::NotAFilesystem)?;
            let Some(file) = entry.as_entry() else {
                continue;
            };
            if path::names_equal(&entry.name(), component) {
                matched = Some(file.clone());
                break;
            }
        }
        let Some(matched) = matched else {
            return Err(FsError::NotFound);
        };
        // A component that is not the last one must be traversable.
        if components.peek().is_some() && !matched.is_directory() {
            return Err(FsError::NotADirectory);
        }
        found = Some(matched);
    }

    Ok(found)
}
