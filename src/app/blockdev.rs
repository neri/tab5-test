//! Display for the block layer: the `devices` and `blkread` commands.
//!
//! `docs/FILESYSTEM_PLAN.md` Stage 1. These exist to make the layer visible
//! from the shell before there is a VFS above it -- `devices` shows what each
//! medium reports and what its LBA 0 turned out to be, and `blkread` reads
//! one block through the same path a filesystem driver will, optionally
//! through a partition so the range check and LBA translation are exercised
//! on real media rather than assumed.
//!
//! It is the counterpart of `mbr.rs`, which is the old raw-sector display the
//! `sdmbr`/`usbmbr` commands still use. The difference is where the parsing
//! happens: `mbr.rs` formats a sector the caller already read, while this
//! goes through `fs::mbr`, which decides whether the sector is a partition
//! table at all before it reports any entries.

use super::shell::Line;
use crate::console::Console;
use crate::framebuffer::Framebuffer;
use crate::fs::block::BlockDevice;
use crate::fs::bootsector::kind_name;
use crate::fs::mbr::{Entry, Layout, PartitionTable, rejection_name};
use crate::fs::registry::{device_name, parse_device_name};
use crate::fs::{self, DeviceId, Devices, PartitionBlockDevice, PartitionRange};
use crate::sdmmc;
use crate::usb::{Location, MAX_HUB_PORTS};

/// Storage devices this listing can describe: the direct port plus every hub
/// port. Matches the registry's own slot count, so the cap can only be
/// reached by a bus that has already filled it.
const MAX_USB_STORAGE: usize = MAX_HUB_PORTS as usize + 1;

/// Lists every block device, its geometry, and what LBA 0 says it is.
///
/// The USB devices are enumerated rather than assumed: how many there are is
/// a property of what is plugged in right now, and a fixed list would either
/// report a `usb1` that is not there or hide one that is.
pub fn show_devices(console: &mut Console, framebuffer: &mut Framebuffer, devices: &mut Devices) {
    show_device(console, framebuffer, devices, DeviceId::Ram);
    show_device(console, framebuffer, devices, DeviceId::Sd);

    // The inventory is taken first, in one immutable borrow. Reading it per
    // device would mean borrowing the registry again inside a loop that is
    // already borrowing it mutably to run I/O.
    let mut inventory = [None; MAX_USB_STORAGE];
    let mut attached = 0usize;
    for (slot, (location, summary)) in devices.usb.mass_storage_inventory().enumerate() {
        if slot >= MAX_USB_STORAGE {
            break;
        }
        inventory[slot] = Some((location, summary.vendor_id, summary.product_id));
        attached = slot + 1;
    }

    if attached == 0 {
        // Named anyway, so an empty bus reads as "nothing plugged in" rather
        // than as USB having been left out of the listing.
        console.write_output_line(framebuffer, "usb0  not present");
        return;
    }
    for (index, entry) in inventory.iter().take(attached).enumerate() {
        show_device(console, framebuffer, devices, DeviceId::Usb(index as u8));
        // Where it is plugged in and what it says it is. Two identical
        // sticks are otherwise indistinguishable in this listing, and which
        // one `usb0` refers to is exactly what a caller about to mount needs
        // to know.
        if let Some((location, vendor, product)) = entry {
            let mut line = Line::new();
            line.push_str("  at ");
            match location {
                Location::Direct => line.push_str("USB-A"),
                Location::HubPort(port) => {
                    line.push_str("hub port ");
                    line.push_u32(*port as u32);
                }
            }
            line.push_str(", ");
            line.push_hex(*vendor as u32, 4);
            line.push_str(":");
            line.push_hex(*product as u32, 4);
            console.write_output_line(framebuffer, line.as_str());
        }
    }
}

fn show_device(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    id: DeviceId,
) {
    let name = device_name(id);
    let name = name.as_str();
    // Each device is inspected inside its own borrow. The USB session in
    // particular only exists as a block device for the length of the
    // closure, so the geometry and the layout are collected together
    // rather than in two passes.
    let inspected = devices.with_device(id, |device| {
        let geometry = device.geometry();
        (geometry, fs::mbr::inspect(device))
    });

    let Some((geometry, layout)) = inspected else {
        let mut line = Line::new();
        line.push_str(name);
        line.push_str("  not present");
        console.write_output_line(framebuffer, line.as_str());
        return;
    };

    let mut line = Line::new();
    line.push_str(name);
    line.push_str("  ");
    line.push_u64(geometry.block_count);
    line.push_str(" x ");
    line.push_u32(geometry.block_bytes);
    line.push_str(" = ");
    match geometry.capacity_bytes() {
        Some(bytes) => {
            line.push_u64(bytes / (1024 * 1024));
            line.push_str(" MiB");
        }
        None => line.push_str("overflow"),
    }
    console.write_output_line(framebuffer, line.as_str());

    match layout {
        Ok(Layout::Mbr(table)) => show_table(console, framebuffer, &table),
        Ok(Layout::SuperfloppyUnsupported(kind)) => {
            let mut line = Line::new();
            line.push_str("  no partition table: ");
            line.push_str(kind_name(kind));
            line.push_str(" volume at LBA 0 (not mountable)");
            console.write_output_line(framebuffer, line.as_str());
        }
        // Both readings held. Naming the volume kind matters here: it is
        // the thing that would have been mounted if this had been guessed
        // at instead of refused.
        Ok(Layout::Ambiguous(kind)) => {
            let mut line = Line::new();
            line.push_str("  ambiguous: valid MBR and valid ");
            line.push_str(kind_name(kind));
            line.push_str(" boot sector; refused");
            console.write_output_line(framebuffer, line.as_str());
        }
        Ok(Layout::Unrecognized { has_signature }) => {
            console.write_output_line(
                framebuffer,
                if has_signature {
                    "  LBA 0 has 55 AA but is neither a valid MBR nor a boot sector"
                } else {
                    "  LBA 0 has no 55 AA signature"
                },
            );
        }
        Err(error) => {
            let mut line = Line::new();
            line.push_str("  LBA 0 read failed: ");
            line.push_str(fs::error_name(error));
            console.write_output_line(framebuffer, line.as_str());
        }
    }
}

fn show_table(console: &mut Console, framebuffer: &mut Framebuffer, table: &PartitionTable) {
    let mut line = Line::new();
    line.push_str("  MBR, disk signature 0x");
    line.push_hex(table.disk_signature, 8);
    console.write_output_line(framebuffer, line.as_str());

    for (index, entry) in table.entries.iter().enumerate() {
        let number = (index + 1) as u32;
        match entry {
            // Unused slots are the normal case -- most tables have three --
            // so they are not printed at all.
            Entry::Empty => {}
            Entry::Usable(partition) => {
                let mut line = Line::new();
                line.push_str("  p");
                line.push_u32(number);
                line.push_str(if partition.bootable { " * 0x" } else { "   0x" });
                line.push_hex(partition.partition_type as u32, 2);
                line.push_str(" ");
                line.push_str(fs::mbr::partition_type_name(partition.partition_type));
                line.push_str(" start ");
                line.push_u64(partition.start_lba);
                line.push_str(", ");
                line.push_u64(partition.block_count / 2048);
                line.push_str(" MiB");
                console.write_output_line(framebuffer, line.as_str());
            }
            Entry::Rejected {
                partition_type,
                start_lba,
                reason,
                ..
            } => {
                let mut line = Line::new();
                line.push_str("  p");
                line.push_u32(number);
                line.push_str("   0x");
                line.push_hex(*partition_type as u32, 2);
                line.push_str(" start ");
                line.push_u64(*start_lba);
                line.push_str(": rejected, ");
                line.push_str(rejection_name(*reason));
                console.write_output_line(framebuffer, line.as_str());
            }
        }
    }
}

/// `blkread <device> [pN] <lba>`: reads one block through the block layer.
///
/// With a `pN` the read goes through a [`PartitionBlockDevice`], so the LBA
/// is partition-relative and a block past the partition's end is refused
/// here rather than reaching the medium -- which is the behaviour worth
/// checking on real media, since the bounds come from a table the medium
/// itself supplied.
pub fn read_block(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    devices: &mut Devices,
    device_name: &str,
    partition_number: Option<u8>,
    lba: u64,
) {
    let Some(id) = parse_device_name(device_name) else {
        console.write_output_line(framebuffer, "unknown device; try 'devices'");
        return;
    };

    let mut sector = [0u8; 512];
    let outcome = devices.with_device(id, |device| match partition_number {
        None => device.read_blocks(lba, &mut sector),
        Some(number) => {
            // The table is re-read for this one command rather than cached.
            // Nothing here holds a mount, so the partition's extent has to
            // come from the medium as it is right now.
            let range = match fs::mbr::inspect(device)? {
                Layout::Mbr(table) => match table.partition(number) {
                    Some(partition) => PartitionRange::from_partition(&partition),
                    None => return Err(fs::BlockError::OutOfRange),
                },
                _ => return Err(fs::BlockError::OutOfRange),
            };
            let mut partition = PartitionBlockDevice::new(device, range)?;
            partition.read_blocks(lba, &mut sector)
        }
    });

    match outcome {
        None => console.write_output_line(framebuffer, "device not present"),
        Some(Err(error)) => {
            let mut line = Line::new();
            line.push_str("read failed: ");
            line.push_str(fs::error_name(error));
            console.write_output_line(framebuffer, line.as_str());
        }
        Some(Ok(())) => {
            // The dump goes to the UART only, the same as every other block
            // dump: 32 lines of hex would scroll the console clean.
            sdmmc::dump_block(&sector);
            console.write_output_line(framebuffer, "block read, dumped to UART log");
        }
    }
}
