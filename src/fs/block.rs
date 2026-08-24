//! The one synchronous block interface every medium below the filesystem
//! layer implements: PSRAM, SD and USB Mass Storage.
//!
//! There is deliberately no separate read-only trait. Whether a volume may be
//! written is a mount policy, decided above this layer by the VFS, not a
//! property of the transport -- an SD card is perfectly capable of taking a
//! WRITE, and the reason the filesystem must not send one is a decision about
//! this firmware, not about the card. Splitting the trait would encode that
//! decision in the type system at the wrong altitude and would still not stop
//! a driver that held the read-write half.
//!
//! LBAs and capacities are `u64` and the logical block length is carried in
//! [`BlockGeometry`] rather than fixed at 512, so the interface does not have
//! to change for a medium that reports something else. Accepting such a medium
//! is a separate question: the MBR and filesystem paths in this module tree
//! reject anything but 512 bytes (see [`BlockError::UnsupportedBlockSize`]),
//! because the units of MBR LBAs, filesystem sectors and the sector cache have
//! not been checked against real hardware that uses another size.

/// Logical block length and capacity, as the medium reports them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlockGeometry {
    /// Logical block length in bytes.
    pub block_bytes: u32,
    /// Total number of addressable logical blocks.
    pub block_count: u64,
}

/// The only logical block length the MBR and filesystem paths accept. The
/// geometry type stays general; this is the gate applied before parsing.
pub const SUPPORTED_BLOCK_BYTES: u32 = 512;

impl BlockGeometry {
    /// Total capacity in bytes, or `None` if the reported geometry overflows
    /// `u64` -- the product is not trusted just because both factors are.
    pub fn capacity_bytes(&self) -> Option<u64> {
        self.block_count.checked_mul(self.block_bytes as u64)
    }

    /// Whether `lba..lba + blocks` lies inside the medium. Computed with
    /// checked arithmetic so a wrapped end never reads as in range.
    pub fn contains(&self, lba: u64, blocks: u64) -> bool {
        match lba.checked_add(blocks) {
            Some(end) => end <= self.block_count,
            None => false,
        }
    }

    /// Splits a byte count into whole logical blocks, or `None` when it is
    /// not a multiple of the block length. Zero-length transfers are rejected
    /// here rather than treated as trivially successful, so a caller that
    /// computed an empty range finds out instead of proceeding.
    pub fn blocks_for(&self, bytes: usize) -> Option<u64> {
        if self.block_bytes == 0 || bytes == 0 {
            return None;
        }
        let block_bytes = self.block_bytes as usize;
        if bytes % block_bytes != 0 {
            return None;
        }
        Some((bytes / block_bytes) as u64)
    }
}

/// Failures every medium maps onto, so the layers above do not branch on
/// whether a volume happens to sit on SD or USB.
///
/// The three "the medium is not there" cases are kept apart on purpose. A
/// physical disconnect and a swapped card are final -- open handles are dead
/// and must not be retried. A transfer that merely failed leaves the medium's
/// identity unknown, and treating that as removal would throw away handles
/// that are about to come back.
// `MediaRemoved` and `MediaChanged` have no source yet: nothing tracks media
// identity until mounts carry a generation. They are defined here anyway
// because the vocabulary is fixed for the whole plan, and the layers being
// written against it now have to map onto the final set rather than a
// temporary one that grows a case at a time.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockError {
    /// The buffer length is not a whole number of logical blocks, or is zero.
    BadBufferLength,
    /// The requested range runs past the end of the medium, or overflows.
    OutOfRange,
    /// The medium's logical block length is not [`SUPPORTED_BLOCK_BYTES`].
    UnsupportedBlockSize,
    /// The transfer failed. Nothing about the medium's identity is implied;
    /// the caller keeps its handles and may retry or re-verify.
    DeviceError,
    /// The medium is present but not ready to transfer yet.
    NotReady,
    /// A physical connection change was observed. Handles are invalid.
    MediaRemoved,
    /// The medium's identity no longer matches. Handles are invalid.
    MediaChanged,
    /// Identity is being re-verified after a transport failure. Handles stay
    /// valid; the caller retries later rather than tearing the mount down.
    TemporarilyUnavailable,
    /// A write reached a medium this firmware mounts read-only. No command
    /// was issued.
    ///
    /// This is an error rather than a silently ignored no-op because a
    /// successful return would tell the filesystem driver its metadata update
    /// had landed, and it would go on to build the rest of the volume's state
    /// on top of a write that never happened.
    WriteSuppressed,
}

/// Short label for shell and UART output. Kept here so every layer prints the
/// same word for the same condition.
pub fn error_name(error: BlockError) -> &'static str {
    match error {
        BlockError::BadBufferLength => "bad buffer length",
        BlockError::OutOfRange => "out of range",
        BlockError::UnsupportedBlockSize => "unsupported block size",
        BlockError::DeviceError => "device error",
        BlockError::NotReady => "not ready",
        BlockError::MediaRemoved => "media removed",
        BlockError::MediaChanged => "media changed",
        BlockError::TemporarilyUnavailable => "temporarily unavailable",
        BlockError::WriteSuppressed => "write suppressed",
    }
}

/// A medium addressed in whole logical blocks.
///
/// Implementations must validate before transferring and must not report a
/// partial transfer as success: a caller that gets `Ok` may assume every
/// requested block is in the buffer. `read_blocks` leaving the buffer
/// untouched on failure is not required -- the caller must not read it -- but
/// the count of blocks transferred is never negotiated downward.
pub trait BlockDevice {
    fn geometry(&self) -> BlockGeometry;

    /// Reads `buffer.len() / block_bytes` blocks starting at `lba`.
    fn read_blocks(&mut self, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError>;

    /// Writes `buffer.len() / block_bytes` blocks starting at `lba`.
    fn write_blocks(&mut self, lba: u64, buffer: &[u8]) -> Result<(), BlockError>;

    /// Pushes any buffering this device holds toward the medium. What that
    /// guarantees is per-medium and is documented on each implementation; for
    /// the RAM disk it guarantees nothing about persistence.
    fn flush(&mut self) -> Result<(), BlockError>;
}

/// Shared precondition check for both directions: the buffer is a whole
/// number of blocks and the resulting range is inside the medium.
///
/// Every implementation calls this before touching hardware, so the error a
/// caller sees for a bad request does not depend on which medium it asked.
pub fn check_range(
    geometry: &BlockGeometry,
    lba: u64,
    buffer_bytes: usize,
) -> Result<u64, BlockError> {
    let blocks = geometry
        .blocks_for(buffer_bytes)
        .ok_or(BlockError::BadBufferLength)?;
    if !geometry.contains(lba, blocks) {
        return Err(BlockError::OutOfRange);
    }
    Ok(blocks)
}
