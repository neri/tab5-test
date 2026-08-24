//! Writes a few files into the freshly formatted RAM disk.
//!
//! The RAM disk is the only volume this firmware may write, and the write
//! path itself does not exist yet -- so without this there would be nothing
//! to read anywhere except a card the developer happened to prepare on a PC,
//! and no way to tell a broken reader from an empty volume.
//!
//! These are fixtures with a job each, not example content:
//!
//! - `README.TXT` is a short name in a single cluster: the simplest thing a
//!   directory walk and a file read can be asked to do.
//! - `Hello World.txt` needs long-name entries, so it only appears correctly
//!   if the reader assembles the LFN chain instead of showing the `~1` alias.
//! - `CHAIN.TXT` spans several clusters with its cluster number printed on
//!   every line, so a reader that loses the FAT chain produces visibly wrong
//!   output rather than a plausible prefix.
//!
//! The entries are written directly rather than through the filesystem
//! library. Doing it this way keeps the `write` feature out of the build
//! until the write path is actually implemented, and means these fixtures
//! test the reader against bytes laid down independently of it -- a reader
//! and writer from the same library agreeing with each other would prove
//! less.
//!
//! Everything written lands in the first FAT sector and the first root
//! directory sector, which bounds this to a few small files. That is all it
//! is for; [`SeedError::TooLarge`] says so rather than silently spilling.

use super::block::{BlockDevice, BlockError};
use super::format::{DIR_ENTRY_BYTES, Fat16Layout, NUM_FATS, ROOT_ENTRY_COUNT, SECTOR_BYTES};

/// Entries per 512-byte sector, and the ceiling on how many this may write.
const ENTRIES_PER_SECTOR: usize = SECTOR_BYTES / DIR_ENTRY_BYTES;
/// FAT16 entries in one sector: two bytes each.
const FAT_ENTRIES_PER_SECTOR: u32 = SECTOR_BYTES as u32 / 2;

const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LONG_NAME: u8 = 0x0F;
/// Marks the last long-name entry, which is written first because the chain
/// is stored in reverse.
const LFN_LAST: u8 = 0x40;
/// UTF-16 units in one long-name entry: 5 + 6 + 2.
const LFN_CHARS: usize = 13;
const FAT16_END_OF_CHAIN: u16 = 0xFFFF;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeedError {
    /// The fixtures do not fit in the first FAT sector or the first root
    /// directory sector.
    TooLarge,
    Block(BlockError),
}

pub fn error_name(error: SeedError) -> &'static str {
    match error {
        SeedError::TooLarge => "seed files do not fit the first FAT/root sector",
        SeedError::Block(error) => super::block::error_name(error),
    }
}

const README: &str = "\
Tab5 RAM disk\r\n\
\r\n\
This volume is FAT16 on a fixed 8 MiB span of PSRAM. It is created and\r\n\
filled from scratch on every boot, so nothing written here survives a\r\n\
reset or a power cycle.\r\n\
\r\n\
The three files here exist to exercise the reader: this one is a short\r\n\
8.3 name in one cluster, 'Hello World.txt' needs long-name entries, and\r\n\
CHAIN.TXT spans several clusters.\r\n";

const HELLO: &str = "\
This file is named with a long file name.\r\n\
If the reader shows HELLOW~1.TXT instead, it stopped at the 8.3 alias\r\n\
and never assembled the long-name entries in front of it.\r\n";

/// Lines written into `CHAIN.TXT`. Sized so the file needs several clusters
/// at any cluster size the formatter picks for 8 MiB.
const CHAIN_LINES: u32 = 96;

/// Writes the fixtures into a volume `format::fat16` has just created.
pub fn test_files(device: &mut dyn BlockDevice, layout: &Fat16Layout) -> Result<(), SeedError> {
    let mut root = [0u8; SECTOR_BYTES];
    let mut fat = [0u8; SECTOR_BYTES];
    // Entries 0 and 1 are reserved: the media descriptor and the end marker
    // the formatter already wrote. Data clusters start at 2.
    fat[0..4].copy_from_slice(&[0xF8, 0xFF, 0xFF, 0xFF]);

    let mut state = Seeder {
        device,
        layout,
        next_cluster: 2,
        next_entry: 0,
    };

    state.add(&mut root, &mut fat, "README.TXT", None, README.as_bytes())?;
    state.add(
        &mut root,
        &mut fat,
        "HELLOW~1.TXT",
        Some("Hello World.txt"),
        HELLO.as_bytes(),
    )?;
    state.add_chain_file(&mut root, &mut fat)?;

    // The FAT and the directory go down last. Until they do, the clusters
    // written above are unreachable, so a failure part way through leaves an
    // empty volume rather than one whose directory points at half-written
    // data.
    for copy in 0..NUM_FATS {
        state
            .device
            .write_blocks(layout.fat_copy_start(copy), &fat)
            .map_err(SeedError::Block)?;
    }
    state
        .device
        .write_blocks(layout.root_start(), &root)
        .map_err(SeedError::Block)?;
    state.device.flush().map_err(SeedError::Block)?;
    Ok(())
}

struct Seeder<'a, 'd> {
    device: &'a mut dyn BlockDevice,
    layout: &'d Fat16Layout,
    next_cluster: u32,
    next_entry: usize,
}

impl Seeder<'_, '_> {
    /// Writes `data` into fresh clusters and adds its directory entries.
    fn add(
        &mut self,
        root: &mut [u8; SECTOR_BYTES],
        fat: &mut [u8; SECTOR_BYTES],
        short_name: &str,
        long_name: Option<&str>,
        data: &[u8],
    ) -> Result<(), SeedError> {
        let first_cluster = self.write_clusters(fat, data)?;
        self.write_entries(
            root,
            short_name,
            long_name,
            first_cluster,
            data.len() as u32,
        )
    }

    /// Copies `data` into a chain of clusters and links them in the FAT.
    fn write_clusters(
        &mut self,
        fat: &mut [u8; SECTOR_BYTES],
        data: &[u8],
    ) -> Result<u32, SeedError> {
        let cluster_bytes = self.layout.cluster_bytes();
        let first = self.next_cluster;
        let mut written = 0usize;
        let mut cluster = first;

        while written < data.len() {
            let end = (written + cluster_bytes).min(data.len());
            self.write_cluster(cluster, &data[written..end])?;
            written = end;

            let next = if written < data.len() {
                cluster + 1
            } else {
                FAT16_END_OF_CHAIN as u32
            };
            set_fat_entry(fat, cluster, next as u16)?;
            cluster += 1;
        }

        self.next_cluster = cluster;
        Ok(first)
    }

    /// Writes one cluster, zero-padding the tail so the unwritten part of the
    /// last cluster is not whatever the previous boot left in PSRAM.
    fn write_cluster(&mut self, cluster: u32, data: &[u8]) -> Result<(), SeedError> {
        if cluster >= self.layout.cluster_count + 2 {
            return Err(SeedError::TooLarge);
        }
        let base = self.layout.cluster_start(cluster);
        for (index, chunk) in data.chunks(SECTOR_BYTES).enumerate() {
            let mut sector = [0u8; SECTOR_BYTES];
            sector[..chunk.len()].copy_from_slice(chunk);
            self.device
                .write_blocks(base + index as u64, &sector)
                .map_err(SeedError::Block)?;
        }
        Ok(())
    }

    /// Writes the long-name entries, if any, followed by the 8.3 entry.
    ///
    /// The long-name chain goes in front of the short entry and in reverse
    /// order, each entry tagged with its position and tied to the short name
    /// by a checksum -- which is what lets a reader that does not understand
    /// long names skip the chain as a set of volume labels and still find the
    /// file.
    fn write_entries(
        &mut self,
        root: &mut [u8; SECTOR_BYTES],
        short_name: &str,
        long_name: Option<&str>,
        first_cluster: u32,
        size: u32,
    ) -> Result<(), SeedError> {
        let short = pack_short_name(short_name).ok_or(SeedError::TooLarge)?;

        if let Some(long_name) = long_name {
            let units: LongName = encode_utf16(long_name)?;
            let checksum = short_name_checksum(&short);
            let entry_count = units.length.div_ceil(LFN_CHARS);
            for index in (0..entry_count).rev() {
                let mut entry = [0u8; DIR_ENTRY_BYTES];
                entry[0] = (index as u8 + 1)
                    | if index + 1 == entry_count {
                        LFN_LAST
                    } else {
                        0
                    };
                entry[11] = ATTR_LONG_NAME;
                entry[13] = checksum;
                for slot in 0..LFN_CHARS {
                    let position = index * LFN_CHARS + slot;
                    // Positions past the name hold the terminator once and
                    // then 0xFFFF padding, which is what tells a reader where
                    // the name stops inside a fixed-size entry.
                    let unit = match position.cmp(&units.length) {
                        core::cmp::Ordering::Less => units.units[position],
                        core::cmp::Ordering::Equal => 0x0000,
                        core::cmp::Ordering::Greater => 0xFFFF,
                    };
                    let offset = LFN_CHAR_OFFSETS[slot];
                    entry[offset..offset + 2].copy_from_slice(&unit.to_le_bytes());
                }
                self.push_entry(root, &entry)?;
            }
        }

        let mut entry = [0u8; DIR_ENTRY_BYTES];
        entry[0..11].copy_from_slice(&short);
        entry[11] = ATTR_ARCHIVE;
        // Every timestamp field stays zero. The RTC has not been read at this
        // point in the boot, and zero is FAT's "not set" rather than a claim
        // about when this was written (`docs/FILESYSTEM_PLAN.md`).
        entry[26..28].copy_from_slice(&(first_cluster as u16).to_le_bytes());
        entry[28..32].copy_from_slice(&size.to_le_bytes());
        self.push_entry(root, &entry)
    }

    fn push_entry(
        &mut self,
        root: &mut [u8; SECTOR_BYTES],
        entry: &[u8; DIR_ENTRY_BYTES],
    ) -> Result<(), SeedError> {
        if self.next_entry >= ENTRIES_PER_SECTOR || self.next_entry >= ROOT_ENTRY_COUNT as usize {
            return Err(SeedError::TooLarge);
        }
        let offset = self.next_entry * DIR_ENTRY_BYTES;
        root[offset..offset + DIR_ENTRY_BYTES].copy_from_slice(entry);
        self.next_entry += 1;
        Ok(())
    }

    /// Builds `CHAIN.TXT` a line at a time, straight into its clusters.
    ///
    /// The content is generated rather than held as a constant so the file
    /// can be large enough to need a chain without putting that many bytes in
    /// flash. Each line names the cluster it lands in, so a reader that
    /// follows the chain wrongly shows the wrong number instead of merely
    /// stopping early.
    fn add_chain_file(
        &mut self,
        root: &mut [u8; SECTOR_BYTES],
        fat: &mut [u8; SECTOR_BYTES],
    ) -> Result<(), SeedError> {
        const LINE_BYTES: usize = 64;
        let cluster_bytes = self.layout.cluster_bytes();
        let total = CHAIN_LINES as usize * LINE_BYTES;
        let first = self.next_cluster;

        let mut cluster = first;
        let mut written = 0usize;
        let mut sector = [0u8; SECTOR_BYTES];
        let mut sector_used = 0usize;
        let mut sector_index = 0u64;

        while written < total {
            let line_number = (written / LINE_BYTES) as u32;
            let line = chain_line(line_number, cluster);
            sector[sector_used..sector_used + LINE_BYTES].copy_from_slice(&line);
            sector_used += LINE_BYTES;
            written += LINE_BYTES;

            if sector_used == SECTOR_BYTES {
                self.device
                    .write_blocks(self.layout.cluster_start(cluster) + sector_index, &sector)
                    .map_err(SeedError::Block)?;
                sector_used = 0;
                sector_index += 1;
                if sector_index as usize * SECTOR_BYTES == cluster_bytes && written < total {
                    set_fat_entry(fat, cluster, cluster as u16 + 1)?;
                    cluster += 1;
                    sector_index = 0;
                }
            }
        }
        if sector_used > 0 {
            sector[sector_used..].fill(0);
            self.device
                .write_blocks(self.layout.cluster_start(cluster) + sector_index, &sector)
                .map_err(SeedError::Block)?;
        }
        set_fat_entry(fat, cluster, FAT16_END_OF_CHAIN)?;
        self.next_cluster = cluster + 1;

        self.write_entries(root, "CHAIN.TXT", None, first, total as u32)
    }
}

/// One 64-byte line of `CHAIN.TXT`, padded to a fixed width so the file's
/// size is exactly predictable.
fn chain_line(line: u32, cluster: u32) -> [u8; 64] {
    let mut out = [b' '; 64];
    let mut position = 0usize;
    let push = |text: &[u8], out: &mut [u8; 64], position: &mut usize| {
        for &byte in text {
            if *position < 62 {
                out[*position] = byte;
                *position += 1;
            }
        }
    };
    push(b"line ", &mut out, &mut position);
    push(&decimal(line), &mut out, &mut position);
    push(b" of CHAIN.TXT, cluster ", &mut out, &mut position);
    push(&decimal(cluster), &mut out, &mut position);
    out[62] = b'\r';
    out[63] = b'\n';
    out
}

/// Right-trimmed decimal text for the small numbers above.
fn decimal(value: u32) -> [u8; 5] {
    let mut out = [b' '; 5];
    let mut remaining = value;
    let mut index = 5;
    loop {
        index -= 1;
        out[index] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 || index == 0 {
            break;
        }
    }
    // Shift the digits to the front so the caller can push the whole array.
    let mut packed = [b' '; 5];
    let digits = 5 - index;
    packed[..digits].copy_from_slice(&out[index..]);
    packed
}

/// Byte offset of each of the 13 name slots within a long-name entry. They
/// are split into three runs because the entry shares its layout with the
/// short entry, whose attribute, type and checksum bytes sit between them.
const LFN_CHAR_OFFSETS: [usize; LFN_CHARS] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

/// A long name as UTF-16 units, bounded to what the fixtures need.
struct LongName {
    units: [u16; 64],
    length: usize,
}

fn encode_utf16(name: &str) -> Result<LongName, SeedError> {
    let mut units = [0u16; 64];
    let mut length = 0;
    for unit in name.encode_utf16() {
        if length == units.len() {
            return Err(SeedError::TooLarge);
        }
        units[length] = unit;
        length += 1;
    }
    Ok(LongName { units, length })
}

/// Packs `NAME.EXT` into the 11-byte space-padded on-disk form.
fn pack_short_name(name: &str) -> Option<[u8; 11]> {
    let (base, extension) = match name.split_once('.') {
        Some((base, extension)) => (base, extension),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || extension.len() > 3 {
        return None;
    }
    let mut out = [b' '; 11];
    for (index, byte) in base.bytes().enumerate() {
        out[index] = byte.to_ascii_uppercase();
    }
    for (index, byte) in extension.bytes().enumerate() {
        out[8 + index] = byte.to_ascii_uppercase();
    }
    Some(out)
}

/// The checksum tying a long-name chain to its 8.3 entry, defined by the FAT
/// specification as this exact rotate-and-add over the 11 packed bytes.
fn short_name_checksum(short: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &byte in short {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(byte);
    }
    sum
}

/// Sets one FAT16 entry, refusing anything outside the first FAT sector.
fn set_fat_entry(fat: &mut [u8; SECTOR_BYTES], cluster: u32, value: u16) -> Result<(), SeedError> {
    if cluster >= FAT_ENTRIES_PER_SECTOR {
        return Err(SeedError::TooLarge);
    }
    let offset = cluster as usize * 2;
    fat[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    Ok(())
}
