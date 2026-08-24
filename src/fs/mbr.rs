//! Classic MBR: validating LBA 0, deciding whether it really is a partition
//! table, and enumerating the four primary entries.
//!
//! Only the four primary entries are supported. Extended partitions and their
//! EBR chain, GPT, multiple LUNs, and partitionless volumes are all out of
//! scope, and the classification below names each of them when it declines
//! rather than failing as though the medium were corrupt.
//!
//! The hard part is not parsing the table, it is deciding that the sector is
//! a table. `55 AA` does not settle it: a FAT or exFAT volume written
//! directly to LBA 0 with no partition table -- a "superfloppy", which is how
//! many USB sticks and SD cards leave the factory -- has the same signature
//! in the same place, and its boot code occupies the bytes where the entries
//! would be. So both readings are tested, and the medium is only used when
//! exactly one of them holds. A sector that reads as both is refused rather
//! than guessed at, because guessing wrong means handing a filesystem driver
//! a partition carved out of the middle of a live volume.
//!
//! This cannot be made exact -- an image can be crafted to satisfy both, and
//! the refusal is the answer for those too.

use super::block::{BlockDevice, BlockError, BlockGeometry, SUPPORTED_BLOCK_BYTES};
use super::bootsector::{self, VolumeKind};

/// Offset of the first partition entry, and the size of one.
const TABLE_OFFSET: usize = 446;
const ENTRY_BYTES: usize = 16;
pub const MAX_PARTITIONS: usize = 4;

/// One accepted primary partition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Partition {
    /// 1-based MBR entry index, which is also the `pN` in a mount name. It is
    /// the entry's slot, not a count: `p1` and `p3` can exist with no `p2`.
    pub number: u8,
    pub bootable: bool,
    /// The MBR type byte. A hint for display only -- what the partition
    /// actually holds is decided by reading its boot sector.
    pub partition_type: u8,
    pub start_lba: u64,
    pub block_count: u64,
}

impl Partition {
    /// One past the last block, which the range checks already proved fits.
    pub fn end_lba(&self) -> u64 {
        self.start_lba + self.block_count
    }
}

/// Why a non-empty entry was left out of the mount candidates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rejection {
    /// A zero sector count. There is nothing to mount.
    ZeroLength,
    /// Start LBA 0, which would place the partition on top of the MBR.
    StartsAtZero,
    /// The partition runs past the end of the medium, or its end overflows.
    PastEnd,
    /// The entry shares blocks with another entry. Both sides are rejected,
    /// because the table does not say which of the two is the mistake.
    Overlaps,
}

pub fn rejection_name(rejection: Rejection) -> &'static str {
    match rejection {
        Rejection::ZeroLength => "zero length",
        Rejection::StartsAtZero => "starts at LBA 0",
        Rejection::PastEnd => "extends past end of medium",
        Rejection::Overlaps => "overlaps another entry",
    }
}

/// One entry's outcome, by slot, so callers can report the empty and the
/// rejected slots as well as the usable ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    /// Type byte 0: the slot is unused. Not an error, and not reported as
    /// one -- most tables have three of these.
    Empty,
    Usable(Partition),
    Rejected {
        partition_type: u8,
        start_lba: u64,
        block_count: u64,
        reason: Rejection,
    },
}

/// The four entries in slot order, with the whole-sector verdict already made.
#[derive(Clone, Copy)]
pub struct PartitionTable {
    pub entries: [Entry; MAX_PARTITIONS],
    /// The disk signature at offset 440. Part of the medium fingerprint, and
    /// commonly 0 or duplicated across cloned images, so it is never an
    /// identity by itself.
    pub disk_signature: u32,
}

impl PartitionTable {
    /// The entry in slot `number` (1-based), if it is usable.
    pub fn partition(&self, number: u8) -> Option<Partition> {
        if number < 1 || number as usize > MAX_PARTITIONS {
            return None;
        }
        match self.entries[number as usize - 1] {
            Entry::Usable(partition) => Some(partition),
            _ => None,
        }
    }

    pub fn usable_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry, Entry::Usable(_)))
            .count()
    }
}

/// What LBA 0 turned out to be.
pub enum Layout {
    /// A sound partition table. At least one entry is usable.
    Mbr(PartitionTable),
    /// A filesystem written straight to LBA 0, with no partition table.
    /// Recognized so it can be named in the refusal; mounting it is not
    /// supported, because every mount path here is defined in terms of an
    /// entry number.
    SuperfloppyUnsupported(VolumeKind),
    /// Readable as both a partition table and a boot sector. Refused.
    Ambiguous(VolumeKind),
    /// Neither reading holds.
    Unrecognized {
        /// Whether the sector at least ends in `55 AA`. Distinguishes a blank
        /// or foreign medium from one whose table is damaged.
        has_signature: bool,
    },
}

/// Reads LBA 0 through `device` and classifies it.
///
/// The block length gate lives here rather than in the [`BlockDevice`]
/// implementations: a medium with 4096-byte blocks is perfectly usable for
/// raw I/O, and it is only the partition and filesystem paths -- whose LBA,
/// sector and cache units have not been checked against one -- that must turn
/// it away.
pub fn inspect(device: &mut dyn BlockDevice) -> Result<Layout, BlockError> {
    let geometry = device.geometry();
    if geometry.block_bytes != SUPPORTED_BLOCK_BYTES {
        return Err(BlockError::UnsupportedBlockSize);
    }
    let mut sector = [0u8; SUPPORTED_BLOCK_BYTES as usize];
    device.read_blocks(0, &mut sector)?;
    Ok(classify(&sector, &geometry))
}

/// The classification itself, split out from the read so it is a pure
/// function of the sector and the geometry.
pub fn classify(sector: &[u8; 512], geometry: &BlockGeometry) -> Layout {
    let signature = bootsector::has_signature(sector);
    let volume = bootsector::identify(sector);
    let table = if signature {
        parse_table(sector, geometry)
    } else {
        None
    };

    match (table, volume) {
        (Some(table), None) => Layout::Mbr(table),
        (None, Some(kind)) => Layout::SuperfloppyUnsupported(kind),
        (Some(_), Some(kind)) => Layout::Ambiguous(kind),
        (None, None) => Layout::Unrecognized {
            has_signature: signature,
        },
    }
}

/// Parses the table, returning `Some` only if the sector holds together as an
/// MBR: every boot indicator legal, and at least one entry usable.
///
/// The boot indicator sweep is what carries most of the discrimination. In an
/// MBR those four bytes are `0x00` or `0x80` by definition; in a boot sector
/// they are whatever the boot code happens to be at offsets 446, 462, 478 and
/// 494, and the odds of all four landing on one of two values are small. The
/// requirement of a usable entry then rules out an all-zero table, which
/// would otherwise pass the sweep trivially.
fn parse_table(sector: &[u8; 512], geometry: &BlockGeometry) -> Option<PartitionTable> {
    let mut entries = [Entry::Empty; MAX_PARTITIONS];

    for slot in 0..MAX_PARTITIONS {
        let offset = TABLE_OFFSET + slot * ENTRY_BYTES;
        let boot = sector[offset];
        if boot != 0x00 && boot != 0x80 {
            return None;
        }
        let partition_type = sector[offset + 4];
        if partition_type == 0 {
            continue;
        }
        let start_lba = u32::from_le_bytes([
            sector[offset + 8],
            sector[offset + 9],
            sector[offset + 10],
            sector[offset + 11],
        ]) as u64;
        let block_count = u32::from_le_bytes([
            sector[offset + 12],
            sector[offset + 13],
            sector[offset + 14],
            sector[offset + 15],
        ]) as u64;

        // The fields are `u32` on disk, so the sum cannot overflow `u64`
        // today. It is still computed with `checked_add` so that widening the
        // on-disk fields later cannot turn a wrapped end into an in-range
        // one without this check failing first.
        let reason = if block_count == 0 {
            Some(Rejection::ZeroLength)
        } else if start_lba == 0 {
            Some(Rejection::StartsAtZero)
        } else if !geometry.contains(start_lba, block_count) {
            Some(Rejection::PastEnd)
        } else {
            None
        };

        entries[slot] = match reason {
            Some(reason) => Entry::Rejected {
                partition_type,
                start_lba,
                block_count,
                reason,
            },
            None => Entry::Usable(Partition {
                number: (slot + 1) as u8,
                bootable: boot == 0x80,
                partition_type,
                start_lba,
                block_count,
            }),
        };
    }

    reject_overlaps(&mut entries);

    let table = PartitionTable {
        entries,
        disk_signature: u32::from_le_bytes([sector[440], sector[441], sector[442], sector[443]]),
    };
    if table.usable_count() == 0 {
        return None;
    }
    Some(table)
}

/// Rejects *both* sides of every overlapping pair.
///
/// Keeping the first and dropping the second would be arbitrary: the table
/// gives no reason to believe the earlier slot is the correct one, and the
/// cost of picking wrong is a filesystem driver writing inside another
/// volume. Two passes, because an entry only learns it overlaps after the
/// entries it conflicts with have been examined.
fn reject_overlaps(entries: &mut [Entry; MAX_PARTITIONS]) {
    let mut overlapping = [false; MAX_PARTITIONS];
    for left in 0..MAX_PARTITIONS {
        for right in (left + 1)..MAX_PARTITIONS {
            let (Entry::Usable(a), Entry::Usable(b)) = (entries[left], entries[right]) else {
                continue;
            };
            if a.start_lba < b.end_lba() && b.start_lba < a.end_lba() {
                overlapping[left] = true;
                overlapping[right] = true;
            }
        }
    }
    for slot in 0..MAX_PARTITIONS {
        if !overlapping[slot] {
            continue;
        }
        if let Entry::Usable(partition) = entries[slot] {
            entries[slot] = Entry::Rejected {
                partition_type: partition.partition_type,
                start_lba: partition.start_lba,
                block_count: partition.block_count,
                reason: Rejection::Overlaps,
            };
        }
    }
}

/// Short names for the common type bytes. Display only: the byte says what
/// the formatting tool intended, and the volume's own boot sector says what
/// is really there.
pub fn partition_type_name(partition_type: u8) -> &'static str {
    match partition_type {
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0E => "FAT16",
        0x0B | 0x0C => "FAT32",
        0x05 | 0x0F => "Extended",
        0x07 => "NTFS/exFAT",
        0x82 => "Linux swap",
        0x83 => "Linux",
        0xEE => "GPT protective",
        0xEF => "EFI System",
        _ => "unknown",
    }
}
