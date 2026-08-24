//! Plausibility checks for a FAT or exFAT boot sector.
//!
//! Two callers need this. `mbr.rs` runs it on LBA 0 to tell a partition table
//! apart from a partitionless "superfloppy" volume, and the filesystem
//! drivers will run it on the first sector of a partition to decide what is
//! in it. Neither one may trust the MBR partition type byte for that: it is a
//! hint written by whatever tool made the table, and the volume itself is the
//! authority on its own format.
//!
//! Everything here treats the sector as untrusted input. The point is not to
//! parse the volume -- that is the driver's job -- but to answer "could this
//! possibly be a boot sector?" strictly enough that the answer is useful for
//! classification, using only fields whose legal values the specifications
//! pin down.

/// Which family a boot sector claims to belong to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VolumeKind {
    /// FAT12, FAT16 or FAT32. The three are told apart by cluster count,
    /// which needs more of the volume than this sector, so the boot sector
    /// alone only gets as far as "a FAT BPB".
    Fat,
    Exfat,
}

pub fn kind_name(kind: VolumeKind) -> &'static str {
    match kind {
        VolumeKind::Fat => "FAT",
        VolumeKind::Exfat => "exFAT",
    }
}

/// The `55 AA` at the end of the sector, shared by MBRs and boot sectors.
pub fn has_signature(sector: &[u8; 512]) -> bool {
    sector[510] == 0x55 && sector[511] == 0xAA
}

fn u16_at(sector: &[u8; 512], offset: usize) -> u16 {
    u16::from_le_bytes([sector[offset], sector[offset + 1]])
}

fn u32_at(sector: &[u8; 512], offset: usize) -> u32 {
    u32::from_le_bytes([
        sector[offset],
        sector[offset + 1],
        sector[offset + 2],
        sector[offset + 3],
    ])
}

/// Identifies the sector as a FAT or exFAT boot sector, or `None`.
///
/// exFAT is checked first because its header leaves the FAT BPB fields
/// reserved and zero, so a valid exFAT sector can never also satisfy the FAT
/// checks -- there is no ambiguity to resolve between the two.
pub fn identify(sector: &[u8; 512]) -> Option<VolumeKind> {
    if !has_signature(sector) {
        return None;
    }
    if is_exfat(sector) {
        return Some(VolumeKind::Exfat);
    }
    if is_fat(sector) {
        return Some(VolumeKind::Fat);
    }
    None
}

/// exFAT's boot sector carries the file system name at offset 3 and requires
/// bytes 11..64 -- exactly the range a FAT BPB occupies -- to be zero. That
/// `MustBeZero` field is what makes the check strong: it is not a magic
/// string a FAT volume could coincidentally contain, it is a positive
/// statement that there is no BPB here.
fn is_exfat(sector: &[u8; 512]) -> bool {
    if &sector[3..11] != b"EXFAT   " {
        return false;
    }
    sector[11..64].iter().all(|&byte| byte == 0)
}

/// A FAT BPB, checked field by field against the values FAT32 specification
/// section 3.1 permits. Anything outside them means the sector is not a BPB,
/// whatever else it may be.
fn is_fat(sector: &[u8; 512]) -> bool {
    // The volume starts with a jump over the BPB: a short jump followed by a
    // NOP, or a near jump. Every formatter emits one of the two.
    let jump_ok = (sector[0] == 0xEB && sector[2] == 0x90) || sector[0] == 0xE9;
    if !jump_ok {
        return false;
    }

    let bytes_per_sector = u16_at(sector, 11);
    if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
        return false;
    }

    // Sectors per cluster is a power of two from 1 to 128, and the cluster
    // must not exceed 32 KiB.
    let sectors_per_cluster = sector[13];
    if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() {
        return false;
    }
    if (bytes_per_sector as u32) * (sectors_per_cluster as u32) > 32 * 1024 {
        return false;
    }

    // At least one reserved sector, because the boot sector itself is one.
    if u16_at(sector, 14) == 0 {
        return false;
    }

    // One or two FATs. Other counts are legal in the abstract but no
    // formatter writes them, and accepting them widens the check for nothing.
    if !matches!(sector[16], 1 | 2) {
        return false;
    }

    // Legal media descriptors: the fixed-disk value and the removable set.
    if sector[21] != 0xF0 && sector[21] < 0xF8 {
        return false;
    }

    // A fixed root directory (FAT12/16) or none (FAT32), never both and
    // never neither: exactly one of the two root layouts must be in use.
    let root_entry_count = u16_at(sector, 17);
    let sectors_per_fat_16 = u16_at(sector, 22);
    let fat32_shape = root_entry_count == 0 && sectors_per_fat_16 == 0;
    let fat16_shape = root_entry_count != 0 && sectors_per_fat_16 != 0;
    if fat32_shape == fat16_shape {
        return false;
    }
    // FAT12/16 root directories occupy whole sectors.
    if fat16_shape && (root_entry_count as usize * 32) % bytes_per_sector as usize != 0 {
        return false;
    }
    if fat32_shape && u32_at(sector, 36) == 0 {
        return false;
    }

    // Exactly one of the two total-sector fields is used; the volume cannot
    // claim both sizes, and a volume of no sectors is not a volume.
    let total_sectors_16 = u16_at(sector, 19);
    let total_sectors_32 = u32_at(sector, 32);
    if (total_sectors_16 == 0) == (total_sectors_32 == 0) {
        return false;
    }

    true
}
