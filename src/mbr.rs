//! MBR partition table display, shared between `shell.rs`'s `sdmbr` and
//! `usbmbr` commands (`USB_MSC_PLAN.md` Stage 6, the goal of that plan:
//! reaching MBR parsing shared between the SD card and USB Mass Storage
//! block-I/O layers). Operates on an already-read 512-byte LBA 0 sector
//! only -- it does not know how that sector was read, i.e. nothing about
//! `sdmmc.rs` or `usb::UsbMassStorage`, which is what lets both commands
//! call the same function. No GPT, no filesystem parsing (both explicitly
//! out of scope, same as `SD_CARD_PLAN.md`'s Stage 4a originally was).

use crate::console::Console;
use crate::shell::Line;

/// Classic MBR layout: 4 fixed 16-byte partition entries at offset 446,
/// each `[boot flag, 3 CHS bytes, type, 3 CHS bytes, start LBA (u32 LE),
/// sector count (u32 LE)]`, followed by the `55 AA` signature at 510-511.
/// Does not look past the MBR itself -- no GPT, no filesystem parsing.
pub fn show(console: &mut Console, sector: &[u8; 512]) {
    if sector[510] != 0x55 || sector[511] != 0xAA {
        console.write_output_line("no 55 AA boot signature at LBA 0; not a valid MBR");
        return;
    }

    let mut any_entry = false;
    for entry in 0..4usize {
        let offset = 446 + entry * 16;
        let partition_type = sector[offset + 4];
        if partition_type == 0 {
            continue;
        }
        any_entry = true;
        let boot = sector[offset];
        let start_lba = u32::from_le_bytes(sector[offset + 8..offset + 12].try_into().unwrap());
        let sectors = u32::from_le_bytes(sector[offset + 12..offset + 16].try_into().unwrap());
        let size_mib = (sectors as u64) * 512 / (1024 * 1024);

        let mut line = Line::new();
        line.push_str("#");
        line.push_u32((entry + 1) as u32);
        line.push_str(if boot == 0x80 { " * type 0x" } else { "   type 0x" });
        line.push_hex(partition_type as u32, 2);
        line.push_str(" ");
        line.push_str(partition_type_name(partition_type));
        console.write_output_line(line.as_str());

        let mut line = Line::new();
        line.push_str("   start LBA ");
        line.push_u32(start_lba);
        line.push_str(", ");
        line.push_u32(size_mib as u32);
        line.push_str(" MiB");
        console.write_output_line(line.as_str());

        if partition_type == 0xEE {
            console.write_output_line("   (GPT protective MBR; GPT itself not parsed)");
        }
    }

    if !any_entry {
        console.write_output_line("no partition entries (all empty)");
    }
}

/// Short names for common partition type bytes; not exhaustive.
fn partition_type_name(partition_type: u8) -> &'static str {
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
