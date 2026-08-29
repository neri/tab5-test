//! FAT16 formatter, used at boot to give the PSRAM RAM disk a filesystem.
//!
//! Written here rather than taken from the filesystem library on purpose.
//! The on-disk layout is fully specified, the whole job is a few hundred
//! bytes of header plus two FATs and a zeroed root directory, and doing it
//! this way means the library choice never has to be constrained by whether
//! it can also format -- a read-only driver is a perfectly good answer for
//! everything else this firmware mounts.
//!
//! It runs against the [`BlockDevice`] interface, not against PSRAM, so it
//! has no idea it is formatting RAM -- and nothing below it would stop it
//! writing to an SD card either, now that removable media are writable.
//! What keeps it aimed at the RAM disk is that it is called from one place
//! at boot; there is no `format` command, and adding one would be adding the
//! confirmation and the volume naming that go with it.

use super::block::{BlockDevice, BlockError, SUPPORTED_BLOCK_BYTES};
use crate::uart;

pub const SECTOR_BYTES: usize = SUPPORTED_BLOCK_BYTES as usize;
/// One reserved sector: the boot sector itself. FAT16 has no FSInfo sector.
pub const RESERVED_SECTORS: u32 = 1;
pub const NUM_FATS: u32 = 2;
/// 512 root entries is the conventional FAT16 root size, and at 32 bytes each
/// it comes to exactly 32 sectors, so the data area stays sector-aligned.
pub const ROOT_ENTRY_COUNT: u32 = 512;
pub const ROOT_SECTORS: u32 = ROOT_ENTRY_COUNT * 32 / SUPPORTED_BLOCK_BYTES;
/// Bytes per directory entry, short and long-name alike.
pub const DIR_ENTRY_BYTES: usize = 32;

/// The cluster counts that make a volume FAT16 rather than FAT12 or FAT32.
/// These bounds are the definition of the type -- nothing in the boot sector
/// records which FAT width is in use, so a driver derives it from the count
/// and a volume outside the range would be read as the wrong format.
const MIN_FAT16_CLUSTERS: u32 = 4085;
const MAX_FAT16_CLUSTERS: u32 = 65524;

/// Volume serial. Fixed, because this volume is recreated identically on
/// every boot: a serial that changed each time would suggest the medium had
/// been swapped, which for a RAM disk is never the useful reading.
const VOLUME_SERIAL: u32 = 0x5441_4235;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FormatError {
    /// No cluster size makes this volume a valid FAT16.
    UnsuitableSize,
    Block(BlockError),
}

/// The geometry the formatter settled on, reported so the caller can log it
/// and so `seed.rs` can address the volume's regions without re-deriving them
/// from the boot sector it just wrote.
#[derive(Clone, Copy)]
pub struct Fat16Layout {
    pub total_sectors: u32,
    pub sectors_per_cluster: u8,
    pub sectors_per_fat: u32,
    pub cluster_count: u32,
}

impl Fat16Layout {
    /// First sector of the `index`-th FAT copy.
    pub fn fat_copy_start(&self, index: u32) -> u64 {
        (RESERVED_SECTORS + index * self.sectors_per_fat) as u64
    }

    /// First sector of the fixed root directory.
    pub fn root_start(&self) -> u64 {
        (RESERVED_SECTORS + NUM_FATS * self.sectors_per_fat) as u64
    }

    /// First sector of the data area, which is cluster 2.
    pub fn data_start(&self) -> u64 {
        self.root_start() + ROOT_SECTORS as u64
    }

    /// First sector of `cluster`. Cluster numbering starts at 2: entries 0
    /// and 1 of the FAT are reserved, so there is no cluster 0 or 1 to
    /// address.
    pub fn cluster_start(&self, cluster: u32) -> u64 {
        self.data_start() + (cluster as u64 - 2) * self.sectors_per_cluster as u64
    }

    pub fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR_BYTES
    }
}

/// Zeroes `device` and writes a fresh FAT16 volume over the whole of it, with
/// no partition table.
///
/// No MBR is written because this volume is not mounted through one: it has a
/// mount point of its own rather than a `pN` entry number. That also keeps it
/// clear of the partition-table classification in `mbr.rs`, which would see a
/// bare FAT volume at LBA 0 as an unsupported superfloppy -- correct for
/// removable media, and simply not the path this device takes.
pub fn fat16(device: &mut dyn BlockDevice) -> Result<Fat16Layout, FormatError> {
    let geometry = device.geometry();
    if geometry.block_bytes != SUPPORTED_BLOCK_BYTES {
        return Err(FormatError::Block(BlockError::UnsupportedBlockSize));
    }
    // The whole volume is addressed by the 16-bit total-sector field below,
    // and by `u32` arithmetic throughout, so an oversized device is rejected
    // rather than silently truncated.
    let total_sectors =
        u32::try_from(geometry.block_count).map_err(|_| FormatError::UnsuitableSize)?;
    let layout = choose_layout(total_sectors).ok_or(FormatError::UnsuitableSize)?;

    // Start from a blank image so nothing the previous boot left in PSRAM
    // can be read back as file data through a stale directory entry or a
    // cluster chain that runs somewhere unexpected.
    let mut sector = [0u8; SECTOR_BYTES];
    for lba in 0..total_sectors as u64 {
        device
            .write_blocks(lba, &sector)
            .map_err(FormatError::Block)?;
    }

    build_boot_sector(&mut sector, &layout);
    device
        .write_blocks(0, &sector)
        .map_err(FormatError::Block)?;

    // Both FATs get the same two reserved entries: the media descriptor in
    // entry 0 and the end-of-chain marker in entry 1. Every other entry stays
    // zero, which is what marks a cluster free.
    sector = [0u8; SECTOR_BYTES];
    sector[0] = 0xF8;
    sector[1] = 0xFF;
    sector[2] = 0xFF;
    sector[3] = 0xFF;
    for fat in 0..NUM_FATS {
        let lba = (RESERVED_SECTORS + fat * layout.sectors_per_fat) as u64;
        device
            .write_blocks(lba, &sector)
            .map_err(FormatError::Block)?;
    }

    // The root directory is already zero from the pass above, and a zero
    // first byte is what ends the directory. Flushing here is what the RAM
    // disk's `flush` promises and no more, but the call keeps the sequence
    // the same shape it would have on a medium where it means something.
    device.flush().map_err(FormatError::Block)?;
    Ok(layout)
}

/// Finds the smallest cluster that puts the volume in the FAT16 cluster
/// range, so a given size gets the finest granularity it can have.
///
/// The search runs over the legal cluster sizes rather than assuming one,
/// because the RAM disk's capacity is a constant that may be changed and a
/// hard-coded cluster size would quietly produce a FAT12 or FAT32 volume that
/// a FAT16 driver then misreads.
fn choose_layout(total_sectors: u32) -> Option<Fat16Layout> {
    for sectors_per_cluster in [1u32, 2, 4, 8, 16, 32, 64] {
        // The standard sizing formula: each FAT sector holds 256 FAT16
        // entries, so this divides the space not taken by the reserved and
        // root areas between the clusters and the FAT sectors describing
        // them, in one step instead of iterating to a fixed point.
        let available = total_sectors.checked_sub(RESERVED_SECTORS + ROOT_SECTORS)?;
        let divisor = 256 * sectors_per_cluster + NUM_FATS;
        let sectors_per_fat = available.div_ceil(divisor);
        if sectors_per_fat == 0 || sectors_per_fat > u16::MAX as u32 {
            continue;
        }
        let Some(data_sectors) =
            total_sectors.checked_sub(RESERVED_SECTORS + ROOT_SECTORS + NUM_FATS * sectors_per_fat)
        else {
            continue;
        };
        let cluster_count = data_sectors / sectors_per_cluster;
        if cluster_count < MIN_FAT16_CLUSTERS || cluster_count > MAX_FAT16_CLUSTERS {
            continue;
        }
        // The FAT has to be big enough for the clusters it just sized, plus
        // the two reserved entries. The formula above guarantees this; the
        // check is here because the rest of the volume is laid out on the
        // assumption and a silent shortfall would corrupt the last clusters.
        if (cluster_count + 2) * 2 > sectors_per_fat * SUPPORTED_BLOCK_BYTES {
            continue;
        }
        return Some(Fat16Layout {
            total_sectors,
            sectors_per_cluster: sectors_per_cluster as u8,
            sectors_per_fat,
            cluster_count,
        });
    }
    None
}

fn build_boot_sector(sector: &mut [u8; SECTOR_BYTES], layout: &Fat16Layout) {
    // A short jump over the BPB followed by a NOP. There is no boot code to
    // jump to -- the target is a zeroed byte -- but the instruction is what
    // every driver checks for, and omitting it would make the volume look
    // malformed to them and to this firmware's own boot sector check.
    sector[0] = 0xEB;
    sector[1] = 0x3C;
    sector[2] = 0x90;
    sector[3..11].copy_from_slice(b"MSWIN4.1");

    sector[11..13].copy_from_slice(&(SUPPORTED_BLOCK_BYTES as u16).to_le_bytes());
    sector[13] = layout.sectors_per_cluster;
    sector[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    sector[16] = NUM_FATS as u8;
    sector[17..19].copy_from_slice(&(ROOT_ENTRY_COUNT as u16).to_le_bytes());

    // Volumes of 65,535 sectors or fewer use the 16-bit count and leave the
    // 32-bit one zero; larger ones do the reverse. Exactly one is in use,
    // which is also what this firmware's boot sector check requires.
    if layout.total_sectors <= u16::MAX as u32 {
        sector[19..21].copy_from_slice(&(layout.total_sectors as u16).to_le_bytes());
    } else {
        sector[32..36].copy_from_slice(&layout.total_sectors.to_le_bytes());
    }

    sector[21] = 0xF8; // Fixed disk. The RAM disk is not removable.
    sector[22..24].copy_from_slice(&(layout.sectors_per_fat as u16).to_le_bytes());
    // Geometry for a medium that has no geometry. These two fields describe
    // the CHS addressing of a floppy or an early hard disk; nothing reads
    // them for a volume this firmware creates, and the conventional values
    // are written so a host tool that does read them sees something sane.
    sector[24..26].copy_from_slice(&63u16.to_le_bytes());
    sector[26..28].copy_from_slice(&255u16.to_le_bytes());
    // No hidden sectors: the volume starts at LBA 0 of its device, because
    // there is no partition table in front of it.
    sector[28..32].copy_from_slice(&0u32.to_le_bytes());

    sector[36] = 0x80;
    sector[38] = 0x29; // Extended boot signature: serial, label and type follow.
    sector[39..43].copy_from_slice(&VOLUME_SERIAL.to_le_bytes());
    // No label is written into the root directory, so the BPB does not claim
    // one either -- the two are supposed to agree, and a label that exists in
    // only one of the places is what host repair tools flag.
    sector[43..54].copy_from_slice(b"NO NAME    ");
    sector[54..62].copy_from_slice(b"FAT16   ");

    sector[510] = 0x55;
    sector[511] = 0xAA;
}

pub fn log_layout(layout: &Fat16Layout) {
    uart::log_hex(b"FS: RAM disk sectors=", layout.total_sectors);
    uart::log_hex(b"FS:   sectors/cluster=", layout.sectors_per_cluster as u32);
    uart::log_hex(b"FS:   sectors/FAT=", layout.sectors_per_fat);
    uart::log_hex(b"FS:   clusters=", layout.cluster_count);
}

pub fn error_name(error: FormatError) -> &'static str {
    match error {
        FormatError::UnsuitableSize => "volume size has no valid FAT16 layout",
        FormatError::Block(error) => super::block::error_name(error),
    }
}
