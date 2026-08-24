//! Names for block devices, and the one place that turns a name back into a
//! device to run I/O against.
//!
//! A mount stores a [`DeviceId`] and a partition range, never a device. Every
//! transfer comes back through here to be resolved. That indirection is what
//! makes two partitions of one disk independently mountable without either
//! mount owning the driver, and it is where a mount will later check that the
//! medium it is addressing is still the one it was mounted from.
//!
//! [`Devices`] is a borrowed view rather than an owner because the USB Mass
//! Storage session belongs to `UsbHost`, which owns the bus and hands out
//! sessions for the length of one operation. Building the view per command
//! keeps that ownership where it is instead of duplicating it here.

use super::block::BlockDevice;
use super::fingerprint::{self, Fingerprint};
use super::ramdisk::RamBlockDevice;
use super::sd::SdBlockDevice;
use super::usb_msc::UsbMscBlockDevice;
use crate::usb::UsbHost;

/// Which medium, and which one of it.
///
/// The USB index is a position in the host's registry, valid only for that
/// generation of it: USB addresses are reassigned on every enumeration and
/// can be handed to a different device after a removal, so neither the
/// address nor this index is a lasting identity for a piece of media. What
/// makes a medium the same medium is the fingerprint a mount records
/// alongside this identifier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceId {
    /// The PSRAM RAM disk. There is exactly one.
    Ram,
    /// The onboard microSD slot, always `sd0`: the board has one slot, and it
    /// is soldered down, so the number never moves.
    Sd,
    /// The `n`-th Mass Storage device in topology order.
    Usb(u8),
}

/// A device's shell name, built without an allocator.
///
/// Long enough for `usb255`, which is the widest [`DeviceId::Usb`] index can
/// produce.
pub struct DeviceName {
    buffer: [u8; 8],
    length: usize,
}

impl DeviceName {
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buffer[..self.length]).unwrap_or("?")
    }
}

/// The name a device is known by in the shell and in a mount point.
pub fn device_name(id: DeviceId) -> DeviceName {
    let mut buffer = [0u8; 8];
    let length = match id {
        DeviceId::Ram => {
            buffer[..3].copy_from_slice(b"ram");
            3
        }
        DeviceId::Sd => {
            buffer[..3].copy_from_slice(b"sd0");
            3
        }
        DeviceId::Usb(index) => {
            buffer[..3].copy_from_slice(b"usb");
            let mut length = 3;
            // Small enough that the digits are easier written out than
            // reached for through a formatter this crate does not link.
            if index >= 100 {
                buffer[length] = b'0' + index / 100;
                length += 1;
            }
            if index >= 10 {
                buffer[length] = b'0' + (index / 10) % 10;
                length += 1;
            }
            buffer[length] = b'0' + index % 10;
            length + 1
        }
    };
    DeviceName { buffer, length }
}

/// The inverse of [`device_name`].
///
/// Both directions live here rather than in the shell so that a name shown
/// by one command is exactly the name another command accepts.
pub fn parse_device_name(name: &str) -> Option<DeviceId> {
    match name {
        "ram" => Some(DeviceId::Ram),
        "sd0" => Some(DeviceId::Sd),
        _ => {
            let index = name.strip_prefix("usb")?;
            // `parse` rather than a hand-rolled loop, so `usb0x1` and `usb+1`
            // are rejected rather than partially read.
            Some(DeviceId::Usb(index.parse().ok()?))
        }
    }
}

/// The SD card's activation state within one command.
///
/// Activating the card is not free and not silent: it drives CMD0/CMD8/ACMD41
/// and the rest of the identification sequence and logs each step. Doing that
/// when the command in hand never mentions `sd0` puts SD traffic in the log of
/// an operation that has nothing to do with the card. So the slot starts
/// unopened and activates on the first resolution of [`DeviceId::Sd`].
///
/// Activation is not carried across commands. The slot has no card-detect
/// line, so the only way to know a card is present -- or that the one from a
/// minute ago is still the same one -- is to ask again.
pub enum SdSlot {
    Unopened,
    Open(SdBlockDevice),
    /// Activation was tried and failed. Kept apart from `Unopened` so one
    /// command that mentions `sd0` twice does not run the whole
    /// identification sequence twice to get the same answer.
    Unavailable,
}

impl SdSlot {
    pub fn new() -> Self {
        Self::Unopened
    }

    fn get(&mut self) -> Option<&mut SdBlockDevice> {
        if matches!(self, Self::Unopened) {
            *self = match SdBlockDevice::open() {
                Some(device) => Self::Open(device),
                None => Self::Unavailable,
            };
        }
        match self {
            Self::Open(device) => Some(device),
            Self::Unopened | Self::Unavailable => None,
        }
    }
}

impl Default for SdSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything currently available to resolve a [`DeviceId`] against.
///
/// None of these is guaranteed: PSRAM may have come up too small for a RAM
/// disk, the SD slot may be empty or the card unusable, and USB Mass Storage
/// may simply not be plugged in.
pub struct Devices<'a> {
    pub ram: Option<&'a mut RamBlockDevice>,
    pub sd: &'a mut SdSlot,
    pub usb: &'a mut UsbHost,
}

impl<'a> Devices<'a> {
    /// Runs `body` against the device `id` names.
    ///
    /// A closure rather than a returned reference because the USB device does
    /// not exist as a `BlockDevice` until one is built over the borrowed
    /// session, and that view cannot outlive the borrow. Callers that need
    /// the result get it through the closure's return value.
    pub fn with_device<T>(
        &mut self,
        id: DeviceId,
        body: impl FnOnce(&mut dyn BlockDevice) -> T,
    ) -> Option<T> {
        match id {
            DeviceId::Ram => {
                let ram = self.ram.as_deref_mut()?;
                Some(body(ram))
            }
            // The card is activated here, on first use, rather than by the
            // caller building this view.
            DeviceId::Sd => {
                let sd = self.sd.get()?;
                Some(body(sd))
            }
            // Addressed by position on the bus, never by "the first one
            // found": a mount records this identifier, and a name that means
            // something different depending on what else is plugged in
            // cannot be recorded.
            DeviceId::Usb(index) => {
                let storage = self.usb.mass_storage_at(index as usize)?;
                let mut device = UsbMscBlockDevice::probe(storage).ok()?;
                Some(body(&mut device))
            }
        }
    }

    /// Captures what can be established about the identity of the medium
    /// behind `id`.
    ///
    /// `partition_start` names the mounted partition's first block, so its
    /// boot sector joins the fingerprint. Passing `None` fingerprints the
    /// device without reference to any one volume on it, which is what a
    /// listing wants.
    ///
    /// This has its own match rather than going through
    /// [`Self::with_device`] because the sources are device-specific: the
    /// CID belongs to the SD driver and the SCSI pages to the USB one, and
    /// neither is reachable through `dyn BlockDevice` -- nor should it be,
    /// since a block device's job is to move blocks.
    pub fn fingerprint(
        &mut self,
        id: DeviceId,
        partition_start: Option<u64>,
    ) -> Option<Fingerprint> {
        match id {
            // The RAM disk is rebuilt byte for byte on every boot and cannot
            // be swapped for another, so its geometry is the whole of its
            // identity. There is no weaker case here, only a simpler one.
            DeviceId::Ram => {
                let ram = self.ram.as_deref_mut()?;
                let mut builder = fingerprint::Builder::new(&ram.geometry());
                let _ = fingerprint::add_volume_sources(&mut builder, ram, partition_start);
                Some(builder.finish())
            }
            DeviceId::Sd => {
                let sd = self.sd.get()?;
                let mut builder = fingerprint::Builder::new(&sd.geometry());
                sd.add_identity(&mut builder);
                let _ = fingerprint::add_volume_sources(&mut builder, sd, partition_start);
                Some(builder.finish())
            }
            DeviceId::Usb(index) => {
                let storage = self.usb.mass_storage_at(index as usize)?;
                let mut device = UsbMscBlockDevice::probe(storage).ok()?;
                let mut builder = fingerprint::Builder::new(&device.geometry());
                device.add_identity(&mut builder);
                let _ = fingerprint::add_volume_sources(&mut builder, &mut device, partition_start);
                Some(builder.finish())
            }
        }
    }
}
