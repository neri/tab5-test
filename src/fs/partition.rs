//! A [`BlockDevice`] restricted to one partition's block range.
//!
//! This is split into two types on purpose. [`PartitionRange`] is plain data
//! -- a start and a length -- and is what a mount table stores, alongside the
//! device identity. [`PartitionBlockDevice`] is the short-lived view that
//! borrows the underlying device for the duration of one operation.
//!
//! Keeping them apart is what lets two partitions of the same disk be mounted
//! at once. If a mount held the device, `p1` and `p2` of one SD card would
//! need two owners of the same driver. Instead each mount holds only its
//! identity and range, and asks the registry to resolve the device each time
//! it needs I/O -- so the SD and USB drivers stay singly owned, and the
//! serialization that gives is exactly what the single-threaded VFS wants
//! anyway.

use super::block::{BlockDevice, BlockError, BlockGeometry, check_range};
use super::mbr::Partition;

/// A partition's extent on its device, in the device's logical blocks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PartitionRange {
    pub start_lba: u64,
    pub block_count: u64,
}

impl PartitionRange {
    pub fn from_partition(partition: &Partition) -> Self {
        Self {
            start_lba: partition.start_lba,
            block_count: partition.block_count,
        }
    }
}

pub struct PartitionBlockDevice<'a> {
    device: &'a mut dyn BlockDevice,
    range: PartitionRange,
    geometry: BlockGeometry,
}

impl<'a> PartitionBlockDevice<'a> {
    /// Builds the view, re-checking the range against the device it is about
    /// to be applied to.
    ///
    /// The range was already validated when the table was parsed, but that
    /// was against the geometry of whatever was in the slot at the time. This
    /// check is against the device in front of us now, so a range outliving
    /// the medium it was measured on cannot address past the end of a smaller
    /// replacement.
    pub fn new(device: &'a mut dyn BlockDevice, range: PartitionRange) -> Result<Self, BlockError> {
        let device_geometry = device.geometry();
        if range.block_count == 0 {
            return Err(BlockError::BadBufferLength);
        }
        if !device_geometry.contains(range.start_lba, range.block_count) {
            return Err(BlockError::OutOfRange);
        }
        let geometry = BlockGeometry {
            block_bytes: device_geometry.block_bytes,
            block_count: range.block_count,
        };
        Ok(Self {
            device,
            range,
            geometry,
        })
    }

    /// Translates a partition-relative LBA to a device LBA, after the shared
    /// range check has confirmed the transfer fits inside the partition.
    fn device_lba(&self, lba: u64) -> u64 {
        self.range.start_lba + lba
    }
}

impl BlockDevice for PartitionBlockDevice<'_> {
    /// The partition's own geometry: block 0 is the partition's first block,
    /// and the count stops at its end. Nothing above this type is given the
    /// device's addresses, which is what keeps a filesystem driver from
    /// reaching outside its volume even if its own bounds arithmetic is wrong.
    fn geometry(&self) -> BlockGeometry {
        self.geometry
    }

    fn read_blocks(&mut self, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        check_range(&self.geometry, lba, buffer.len())?;
        let device_lba = self.device_lba(lba);
        self.device.read_blocks(device_lba, buffer)
    }

    fn write_blocks(&mut self, lba: u64, buffer: &[u8]) -> Result<(), BlockError> {
        check_range(&self.geometry, lba, buffer.len())?;
        let device_lba = self.device_lba(lba);
        self.device.write_blocks(device_lba, buffer)
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.device.flush()
    }
}
