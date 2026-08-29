//! Deciding whether the medium in front of us is the one a mount was made
//! against.
//!
//! Nothing here is a unique identifier, and no combination of it is. USB's
//! `iSerialNumber` is optional, is left empty by some products and identical
//! across every unit of others. SCSI's VPD pages are optional too. The MBR
//! disk signature and the FAT volume serial are 32 bits, are often zero, and
//! are duplicated wholesale by any image-cloning tool -- which is why this
//! module never calls them UUIDs, whatever a partition editor might.
//!
//! So the question is not "which medium is this" but "is this still the same
//! one", and that is answerable. Every source that *did* answer is folded
//! together with the capacity, the partition table and the volume's boot
//! sector; if any of them differs, the medium differs. The converse does not
//! hold -- two identical clones agree on all of it -- and that is accepted
//! here, because the case that matters is a user swapping a card, and two
//! cards holding byte-identical images are interchangeable for reading
//! anyway.
//!
//! What is *not* left to this is a medium that was physically unplugged.
//! A connection change is observed directly, and it always wins: the same
//! stick pulled out and pushed back in gets a new generation even though
//! every byte of this agrees, because an open handle must not survive the
//! user having taken the medium away.

use super::block::{BlockDevice, BlockError, BlockGeometry};
use super::mbr;

/// Which optional identity sources answered.
///
/// Recorded rather than folded away, because "these two fingerprints agree"
/// means something quite different when the only thing either could offer
/// was its capacity. A caller deciding how much to trust a match needs to
/// see what went into it -- and so does anyone reading `mounts`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Sources {
    /// The SD card's CID register: manufacturer, product name and revision,
    /// serial number and date. Always present on an activated card, and the
    /// strongest source available on any medium here.
    pub sd_cid: bool,
    /// SCSI standard INQUIRY: vendor, product and revision strings.
    pub inquiry: bool,
    /// VPD page 0x80, the unit serial number.
    pub unit_serial: bool,
    /// VPD page 0x83, the device identification list.
    pub device_id: bool,
    /// A non-zero MBR disk signature.
    pub disk_signature: bool,
    /// The first sector of the mounted partition.
    pub boot_sector: bool,
}

impl Sources {
    /// How many independent sources contributed. A match on capacity alone
    /// is worth much less than one that also matched a serial number, and
    /// this is the number that says which happened.
    pub fn count(&self) -> u32 {
        [
            self.sd_cid,
            self.inquiry,
            self.unit_serial,
            self.device_id,
            self.disk_signature,
            self.boot_sector,
        ]
        .iter()
        .filter(|present| **present)
        .count() as u32
    }

    /// The sources that answered, as one value.
    ///
    /// For logging a disagreement: two fingerprints that differ are much
    /// easier to tell apart by which sources each had than by reading six
    /// booleans out of a diagnostic line.
    pub fn bits(&self) -> u32 {
        let mut bits = 0;
        for (index, present) in [
            self.sd_cid,
            self.inquiry,
            self.unit_serial,
            self.device_id,
            self.disk_signature,
            self.boot_sector,
        ]
        .iter()
        .enumerate()
        {
            if *present {
                bits |= 1 << index;
            }
        }
        bits
    }

    /// Whether anything beyond the medium's geometry answered.
    ///
    /// A fingerprint with nothing but a capacity behind it cannot tell two
    /// cards of the same size apart, and a caller about to keep an open file
    /// handle across a rescan deserves to know that.
    pub fn beyond_geometry(&self) -> bool {
        self.count() > 0
    }
}

/// A medium's identity as far as it can be established.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fingerprint {
    pub block_bytes: u32,
    pub block_count: u64,
    /// The MBR disk signature, or 0 when there is no partition table or the
    /// signature is genuinely zero. `sources.disk_signature` tells them apart.
    pub disk_signature: u32,
    /// Everything gathered, folded into one value for comparison.
    ///
    /// A digest rather than the bytes themselves: the inputs run to a few
    /// hundred bytes per medium, and a mount table holding all of them for
    /// every volume would cost more than the question is worth. Collisions
    /// would be a problem for a security decision; this one only has to
    /// notice a user swapping media.
    pub digest: u64,
    /// The same inputs, kept apart. Index by the `SOURCE_*` order that
    /// [`Sources::bits`] uses. A source that did not answer is zero.
    pub parts: [u64; SOURCE_COUNT],
    pub sources: Sources,
}

impl Fingerprint {
    /// Which sources these two disagree on, as a bitmask in
    /// [`Sources::bits`] order.
    ///
    /// A source only one of them has counts as a disagreement, which is what
    /// makes "the device stopped answering this page" visible rather than
    /// hidden inside the combined digest.
    pub fn differing_sources(&self, other: &Fingerprint) -> u32 {
        let mut bits = 0;
        for index in 0..SOURCE_COUNT {
            if self.parts[index] != other.parts[index] {
                bits |= 1 << index;
            }
        }
        bits
    }
}

impl Fingerprint {
    /// Whether these describe the same medium.
    ///
    /// Geometry is compared separately from the digest so the common case --
    /// a different-sized card in the same slot -- is decided without needing
    /// the digest's inputs to have been gathered at all.
    pub fn matches(&self, other: &Fingerprint) -> bool {
        self.block_bytes == other.block_bytes
            && self.block_count == other.block_count
            && self.sources == other.sources
            && self.digest == other.digest
    }
}

/// FNV-1a, 64-bit.
///
/// Chosen for being a few lines with no table and no dependency. The digest
/// is compared against one this same firmware produced minutes earlier, so
/// nothing depends on the choice being standard, only on it being stable
/// within a build and sensitive to every input byte.
struct Digest(u64);

impl Digest {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= byte as u64;
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    /// Mixes in a label before its data, so that the same bytes arriving
    /// from a different source do not produce the same digest -- a serial
    /// number moving from VPD page 0x80 to page 0x83 is a change.
    fn field(&mut self, tag: u8, bytes: &[u8]) {
        self.push(&[tag]);
        self.push(&(bytes.len() as u32).to_le_bytes());
        self.push(bytes);
    }

    fn finish(self) -> u64 {
        self.0
    }
}

/// Identity sources a fingerprint can carry, in the order
/// [`Sources::bits`] numbers them. One digest is kept per source as well as
/// the combined one, so a disagreement can name what moved instead of only
/// that something did -- which matters because that verdict tears a mount
/// down, and "a stick that did not answer one optional page this time" and
/// "a different stick" are not the same event.
pub const SOURCE_COUNT: usize = 6;

const SOURCE_SD_CID: usize = 0;
const SOURCE_INQUIRY: usize = 1;
const SOURCE_UNIT_SERIAL: usize = 2;
const SOURCE_DEVICE_ID: usize = 3;
const SOURCE_PARTITION_TABLE: usize = 4;
const SOURCE_BOOT_SECTOR: usize = 5;

/// One source's bytes on their own, for the per-source comparison.
fn part(tag: u8, bytes: &[u8]) -> u64 {
    let mut digest = Digest::new();
    digest.field(tag, bytes);
    digest.finish()
}

const TAG_GEOMETRY: u8 = 1;
const TAG_SD_CID: u8 = 2;
const TAG_INQUIRY: u8 = 3;
const TAG_UNIT_SERIAL: u8 = 4;
const TAG_DEVICE_ID: u8 = 5;
const TAG_PARTITION_TABLE: u8 = 6;
const TAG_BOOT_SECTOR: u8 = 7;

/// Collects a fingerprint while the caller supplies whatever sources it can
/// reach.
///
/// A builder rather than one function because the sources live in different
/// places: the CID belongs to the SD driver, the VPD pages to the SCSI
/// driver, and the partition table to the block layer. Each caller adds what
/// it has, and the ones it cannot reach are recorded as absent rather than
/// as empty.
pub struct Builder {
    digest: Digest,
    parts: [u64; SOURCE_COUNT],
    sources: Sources,
    block_bytes: u32,
    block_count: u64,
    disk_signature: u32,
}

impl Builder {
    pub fn new(geometry: &BlockGeometry) -> Self {
        let mut digest = Digest::new();
        let mut geometry_bytes = [0u8; 12];
        geometry_bytes[..4].copy_from_slice(&geometry.block_bytes.to_le_bytes());
        geometry_bytes[4..].copy_from_slice(&geometry.block_count.to_le_bytes());
        digest.field(TAG_GEOMETRY, &geometry_bytes);
        Self {
            digest,
            parts: [0; SOURCE_COUNT],
            sources: Sources::default(),
            block_bytes: geometry.block_bytes,
            block_count: geometry.block_count,
            disk_signature: 0,
        }
    }

    /// The SD card's CID, all four words.
    ///
    /// The whole register rather than the serial field alone: two cards from
    /// one production run can share a serial, and the manufacturer, product
    /// name, revision and manufacturing date narrow that considerably.
    pub fn sd_cid(&mut self, cid: &[u32; 4]) -> &mut Self {
        let mut bytes = [0u8; 16];
        for (index, word) in cid.iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        self.digest.field(TAG_SD_CID, &bytes);
        self.parts[SOURCE_SD_CID] = part(TAG_SD_CID, &bytes);
        self.sources.sd_cid = true;
        self
    }

    pub fn inquiry(&mut self, response: &[u8]) -> &mut Self {
        // Bytes 8..36 are the vendor, product and revision strings. The
        // bytes before them include the peripheral qualifier and flags,
        // which some devices vary between one INQUIRY and the next.
        let identity = response.get(8..36).unwrap_or(response);
        self.digest.field(TAG_INQUIRY, identity);
        self.parts[SOURCE_INQUIRY] = part(TAG_INQUIRY, identity);
        self.sources.inquiry = true;
        self
    }

    pub fn unit_serial(&mut self, serial: &[u8]) -> &mut Self {
        self.digest.field(TAG_UNIT_SERIAL, serial);
        self.parts[SOURCE_UNIT_SERIAL] = part(TAG_UNIT_SERIAL, serial);
        self.sources.unit_serial = true;
        self
    }

    pub fn device_id(&mut self, identification: &[u8]) -> &mut Self {
        self.digest.field(TAG_DEVICE_ID, identification);
        self.parts[SOURCE_DEVICE_ID] = part(TAG_DEVICE_ID, identification);
        self.sources.device_id = true;
        self
    }

    /// The MBR's partition table and disk signature, from LBA 0.
    pub fn partition_table(&mut self, sector: &[u8; 512]) -> &mut Self {
        // Bytes 440..510: the disk signature, the reserved pair, and all four
        // entries. The boot code in front of them is not part of the layout
        // and differs between machines that have written to the same disk.
        self.digest.field(TAG_PARTITION_TABLE, &sector[440..510]);
        self.parts[SOURCE_PARTITION_TABLE] = part(TAG_PARTITION_TABLE, &sector[440..510]);
        self.disk_signature =
            u32::from_le_bytes([sector[440], sector[441], sector[442], sector[443]]);
        self.sources.disk_signature = self.disk_signature != 0;
        self
    }

    /// The first sector of the mounted partition.
    ///
    /// Only the parts that identify the volume are taken: the OEM name, the
    /// BPB, and -- for FAT -- the serial and label. The boot code around
    /// them says nothing about which volume this is.
    pub fn boot_sector(&mut self, sector: &[u8; 512]) -> &mut Self {
        self.digest.field(TAG_BOOT_SECTOR, &sector[3..64]);
        self.parts[SOURCE_BOOT_SECTOR] = part(TAG_BOOT_SECTOR, &sector[3..64]);
        self.sources.boot_sector = true;
        self
    }

    pub fn finish(self) -> Fingerprint {
        Fingerprint {
            block_bytes: self.block_bytes,
            block_count: self.block_count,
            disk_signature: self.disk_signature,
            digest: self.digest.finish(),
            parts: self.parts,
            sources: self.sources,
        }
    }
}

/// Adds whatever LBA 0 and the partition's first sector can contribute.
///
/// Failures are not errors. A medium with no partition table still has a
/// capacity to fingerprint, and refusing to produce one at all would leave
/// the caller with nothing to compare instead of with something weaker.
pub fn add_volume_sources(
    builder: &mut Builder,
    device: &mut dyn BlockDevice,
    partition_start: Option<u64>,
) -> Result<(), BlockError> {
    let mut sector = [0u8; 512];
    device.read_blocks(0, &mut sector)?;
    let geometry = device.geometry();
    if matches!(mbr::classify(&sector, &geometry), mbr::Layout::Mbr(_)) {
        builder.partition_table(&sector);
    }
    if let Some(start) = partition_start {
        device.read_blocks(start, &mut sector)?;
        builder.boot_sector(&sector);
    }
    Ok(())
}
