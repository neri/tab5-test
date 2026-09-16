//! Filesystem support, from the block interface upward.
//!
//! Staged per `docs/plans/archive/FILESYSTEM_PLAN.md`. What is here is that plan's Stage 1:
//! the block layer, MBR handling, and the PSRAM RAM disk. The VFS, the mount
//! table and the FAT drivers are not here yet, so nothing above this module
//! opens a file -- the shell reaches the layer directly.
//!
//! Layering, bottom to top:
//!
//! - [`block`] defines the one synchronous [`BlockDevice`] every medium
//!   implements, in logical blocks, with no separate read-only variant.
//! - [`ramdisk`], [`sd`] and [`usb_msc`] are the three implementations. The
//!   last two are adapters: `sdmmc.rs` and `usb/msc.rs` keep the DMA
//!   constraints, the BOT recovery and everything else medium-specific, and
//!   are not rewritten to fit the trait.
//! - [`bootsector`] and [`mbr`] read LBA 0 -- the first to recognize a FAT or
//!   exFAT volume, the second to decide whether the sector is a partition
//!   table at all and to enumerate the four primary entries.
//! - [`partition`] cuts a device down to one entry's block range.
//! - [`registry`] names devices and resolves a name back to a device, so a
//!   mount can hold an identity instead of a driver.
//! - [`format`] writes a fresh FAT16 volume, which is how the RAM disk gets
//!   one at every boot.
//!
//! Partitioning is deliberately below the filesystem drivers rather than
//! beside them: a partition is a way of making a block device, and a driver
//! that had to know whether its volume came from an entry in a table or from
//! a whole device would be carrying a distinction that means nothing to it.

pub mod block;
pub mod bootsector;
pub mod clock;
pub mod fingerprint;
pub mod format;
pub mod mbr;
pub mod partition;
pub mod path;
pub mod ramdisk;
pub mod registry;
pub mod sd;
pub mod seed;
pub mod stream;
pub mod usb_msc;
pub mod vfs;

// Only the names the rest of the firmware reaches for are lifted here. The
// submodules stay public, so a caller that wants `fs::mbr::Entry` or
// `fs::block::BlockDevice` names the layer it is working at.
pub use block::{BlockError, error_name};
pub use partition::{PartitionBlockDevice, PartitionRange};
pub use ramdisk::RamBlockDevice;
pub use registry::{DeviceId, Devices, SdSlot};
