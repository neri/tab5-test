//! The `lsusb` shell command's display: what is plugged into USB-A, as a
//! tree through the hub, and one device's standard descriptors in full.
//!
//! Separate from the `usb*` commands in `shell.rs` on purpose. Those exist
//! to debug this project's own USB stack -- port registers, split-transaction
//! support, BOT sessions, scan timing -- and say as much about the driver as
//! about the device. This one only answers "what is attached, and what does
//! it say about itself", the way Linux's `lsusb` does.
//!
//! Both views read `usb::UsbHost`'s device records, which enumeration
//! already paid for; the tree therefore costs no bus traffic at all. The
//! per-device view adds the string descriptors, which enumeration
//! deliberately does not fetch (see `usb::DeviceRecord::read_string`), so it
//! is the only part that talks to the device -- three control transfers on
//! the pipe every other command uses too.

use super::shell::Line;
use crate::console::Console;
use crate::framebuffer::Framebuffer;
use crate::usb::{self, DeviceKind, Location, Speed};

/// Longest string descriptor text shown. Manufacturer and product names run
/// well past this on some devices; a truncated name still identifies the
/// device, and the console is 104 columns wide.
const STRING_MAX: usize = 40;

/// Shows every enumerated device as a tree: the root port first, then --
/// when a hub occupies it -- each of its occupied ports, with every
/// interface of a composite device listed under the device it belongs to.
///
/// The registry is the only source here, so this shows what the last scan
/// found. A device plugged in a moment ago appears once the background hub
/// port scan picks it up, or immediately after `usbrescan`.
pub fn show_tree(console: &mut Console, framebuffer: &mut Framebuffer, usb_host: &usb::UsbHost) {
    let Some(port) = usb_host.last_probe() else {
        console.write_output_line(framebuffer, "USB-A: not probed yet; run usbrescan");
        return;
    };

    let mut line = Line::new();
    line.push_str("USB-A: ");
    if !port.connected {
        line.push_str("nothing attached");
        console.write_output_line(framebuffer, line.as_str());
        return;
    }
    line.push_str(speed_name(port.speed));
    if !port.enabled {
        line.push_str(", port never enabled");
    }
    console.write_output_line(framebuffer, line.as_str());

    if usb_host.bus_devices().next().is_none() {
        console.write_output_line(
            framebuffer,
            "  device connected but not enumerated; run usbrescan",
        );
        return;
    }

    let mut devices = 0u32;
    for device in usb_host.bus_devices() {
        devices += 1;
        write_device_line(console, framebuffer, &device);
        let indent = detail_indent(device.record.location);
        if device.record.is_hub() {
            write_hub_line(console, framebuffer, usb_host, indent);
        } else {
            write_driver_line(console, framebuffer, device.driver, indent);
        }
        write_interface_lines(console, framebuffer, device.record, indent);
    }

    for port_number in usb_host.unenumerated_hub_ports() {
        let mut line = Line::new();
        line.push_str("  port ");
        line.push_u32(port_number as u32);
        line.push_str(": device present, enumeration failed");
        console.write_output_line(framebuffer, line.as_str());
    }

    let mut line = Line::new();
    line.push_u32(devices);
    line.push_str(if devices == 1 {
        " device; 'lsusb <address>' for its descriptors"
    } else {
        " devices; 'lsusb <address>' for one device's descriptors"
    });
    console.write_output_line(framebuffer, line.as_str());
}

/// Shows one device's standard descriptors: the device descriptor, the
/// configuration header, and every interface with its endpoints.
///
/// `address` is the number the tree shows in brackets. The three string
/// descriptors are read here and nowhere else; a device that refuses them
/// (or has none) is shown as such rather than failing the command.
pub fn show_device(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &usb::UsbHost,
    address: u8,
) {
    let Some(device) = usb_host.bus_device(address) else {
        let mut line = Line::new();
        line.push_str("no enumerated device at address ");
        line.push_u32(address as u32);
        line.push_str("; run lsusb for the list");
        console.write_output_line(framebuffer, line.as_str());
        return;
    };
    let record = device.record;

    let mut line = Line::new();
    line.push_str("device ");
    line.push_u32(address as u32);
    line.push_str(" on ");
    push_location(&mut line, record.location);
    line.push_str(", ");
    line.push_str(speed_name(record.speed));
    console.write_output_line(framebuffer, line.as_str());

    write_route_line(console, framebuffer, record);
    if record.is_hub() {
        write_hub_line(console, framebuffer, usb_host, "  ");
    } else {
        write_driver_line(console, framebuffer, device.driver, "  ");
    }

    // One language read for the whole command: every string below is
    // requested in it, and asking twice would cost another transfer to say
    // the same thing.
    let language = record.string_language();
    write_device_descriptor(console, framebuffer, record, language);
    write_configuration_descriptor(console, framebuffer, record, language);
}

/// `port 1: [2] 046D:C31C per-interface, Low-Speed via split`: what the
/// device is, and where it hangs. The class shown is the device-level one,
/// which for a composite device says only "per-interface" -- the interface
/// lines under it are where its actual function shows up.
fn write_device_line(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    device: &usb::BusDevice<'_>,
) {
    let record = device.record;
    let mut line = Line::new();
    if let Location::HubPort(port) = record.location {
        line.push_str("  port ");
        line.push_u32(port as u32);
        line.push_str(": ");
    }
    line.push_str("[");
    line.push_u32(record.address as u32);
    line.push_str("] ");
    push_vendor_product(&mut line, record);
    line.push_str(" ");
    line.push_str(class_name(record.summary.device_class));
    line.push_str(", ");
    line.push_str(speed_name(record.speed));
    if record.split_route().is_some() {
        line.push_str(" via split");
    }
    console.write_output_line(framebuffer, line.as_str());
}

/// `if0: 03/01/01 HID Boot keyboard`, one line per interface, which is what
/// makes a composite device readable: the device-level class of such a
/// device is 00 (per-interface) and says nothing about what it does.
fn write_interface_lines(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    record: &usb::DeviceRecord,
    indent: &str,
) {
    for raw in record.descriptors() {
        if raw.descriptor_type != usb::DESCRIPTOR_TYPE_INTERFACE {
            continue;
        }
        let Some(interface) = usb::InterfaceDescriptor::parse(raw.bytes) else {
            continue;
        };
        let mut line = Line::new();
        line.push_str(indent);
        line.push_str("if");
        line.push_u32(interface.number as u32);
        if interface.alternate_setting != 0 {
            line.push_str(".");
            line.push_u32(interface.alternate_setting as u32);
        }
        line.push_str(": ");
        push_interface_class(&mut line, &interface);
        console.write_output_line(framebuffer, line.as_str());
    }
    if record.config_truncated() {
        let mut line = Line::new();
        line.push_str(indent);
        line.push_str("(configuration descriptor longer than this host reads)");
        console.write_output_line(framebuffer, line.as_str());
    }
}

/// The class driver bound to the device, if this project has one for it.
fn write_driver_line(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    driver: Option<&DeviceKind>,
    indent: &str,
) {
    let mut line = Line::new();
    line.push_str(indent);
    line.push_str("driver: ");
    match driver {
        Some(DeviceKind::Keyboard(_)) => line.push_str("HID Boot keyboard"),
        Some(DeviceKind::Mouse(_)) => line.push_str("HID Boot mouse"),
        Some(DeviceKind::MassStorage(storage)) => {
            line.push_str("Mass Storage (Bulk-Only Transport)");
            if storage.needs_reinit() {
                line.push_str(" -- session unusable; run usbrescan");
            }
        }
        None => line.push_str("none for this class"),
    }
    console.write_output_line(framebuffer, line.as_str());
}

/// A hub is driven by `hub::Hub` rather than by anything in `DeviceKind`,
/// so it gets its own line -- and a pointer at the command that shows its
/// live per-port status, which this one deliberately does not read.
fn write_hub_line(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    usb_host: &usb::UsbHost,
    indent: &str,
) {
    let mut line = Line::new();
    line.push_str(indent);
    line.push_str("driver: hub");
    match usb_host.hub() {
        Some(hub) => {
            line.push_str(", ");
            line.push_u32(hub.port_count() as u32);
            line.push_str(" ports; usbhub shows their status");
        }
        // Enumerated as a hub, but its class descriptor never came back, so
        // nothing behind it was ever looked at.
        None => line.push_str(" -- not open; run usbrescan"),
    }
    console.write_output_line(framebuffer, line.as_str());
}

/// How the controller reaches this device, which is only worth a line when
/// it is not simply "directly".
fn write_route_line(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    record: &usb::DeviceRecord,
) {
    let mut line = Line::new();
    line.push_str("  reached ");
    match record.split_route() {
        Some((hub_address, port)) => {
            line.push_str("by split transactions through the TT of hub ");
            line.push_u32(hub_address as u32);
            line.push_str(" port ");
            line.push_u32(port as u32);
            if record.low_speed_via_hub() {
                line.push_str(", Low-Speed");
            }
        }
        // PRE tokens without a split means the hub runs at Full-Speed and
        // simply repeats for the Low-Speed device behind it.
        None if record.low_speed_via_hub() => {
            line.push_str("with PRE tokens through the hub (Low-Speed on a Full-Speed bus)");
        }
        // Plugged into USB-A directly: the bus runs at the device's own
        // speed and there is nothing to say.
        None => return,
    }
    console.write_output_line(framebuffer, line.as_str());
}

fn write_device_descriptor(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    record: &usb::DeviceRecord,
    language: Option<u16>,
) {
    console.write_output_line(framebuffer, "Device Descriptor:");

    let mut line = Line::new();
    line.push_str("  bcdUSB ");
    push_bcd(&mut line, record.usb_version());
    line.push_str("  bMaxPacketSize0 ");
    line.push_u32(record.max_packet_size0() as u32);
    line.push_str("  bNumConfigurations ");
    line.push_u32(record.num_configurations() as u32);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("  idVendor 0x");
    line.push_hex(record.summary.vendor_id as u32, 4);
    line.push_str("  idProduct 0x");
    line.push_hex(record.summary.product_id as u32, 4);
    line.push_str("  bcdDevice ");
    push_bcd(&mut line, record.device_version());
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("  bDeviceClass ");
    push_class_triple(
        &mut line,
        record.summary.device_class,
        record.summary.device_subclass,
        record.summary.device_protocol,
    );
    line.push_str(" ");
    line.push_str(class_name(record.summary.device_class));
    console.write_output_line(framebuffer, line.as_str());

    write_string_lines(console, framebuffer, record, language);
}

/// The three device-descriptor strings, each read from the device now.
///
/// A device is free to have none at all, which is what an index of 0 means
/// and what a refused language read means; both are shown rather than
/// silently skipped, so a blank name is never mistaken for a failure of
/// this command.
fn write_string_lines(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    record: &usb::DeviceRecord,
    language: Option<u16>,
) {
    let Some(language) = language else {
        console.write_output_line(framebuffer, "  strings: device reports none");
        return;
    };

    for (name, index) in [
        ("iManufacturer", record.manufacturer_string_index()),
        ("iProduct", record.product_string_index()),
        ("iSerialNumber", record.serial_string_index()),
    ] {
        let mut line = Line::new();
        line.push_str("  ");
        line.push_str(name);
        line.push_str(" ");
        line.push_u32(index as u32);
        line.push_str(": ");
        if index == 0 {
            line.push_str("(none)");
        } else {
            push_string(&mut line, record, index, language);
        }
        console.write_output_line(framebuffer, line.as_str());
    }
}

fn write_configuration_descriptor(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    record: &usb::DeviceRecord,
    language: Option<u16>,
) {
    console.write_output_line(framebuffer, "Configuration Descriptor:");

    let mut line = Line::new();
    line.push_str("  bConfigurationValue ");
    line.push_u32(record.configuration_value() as u32);
    line.push_str("  bNumInterfaces ");
    line.push_u32(record.summary.num_interfaces as u32);
    line.push_str("  wTotalLength ");
    line.push_u32(record.summary.config_total_length as u32);
    if record.config_truncated() {
        line.push_str(" (");
        line.push_u32(record.config_bytes().len() as u32);
        line.push_str(" read)");
    }
    console.write_output_line(framebuffer, line.as_str());

    let attributes = record.config_attributes();
    let mut line = Line::new();
    line.push_str("  bmAttributes 0x");
    line.push_hex(attributes as u32, 2);
    line.push_str(if attributes & 0x40 != 0 {
        " self-powered"
    } else {
        " bus-powered"
    });
    if attributes & 0x20 != 0 {
        line.push_str(", remote wakeup");
    }
    line.push_str("  bMaxPower ");
    line.push_u32(record.max_power_ma() as u32);
    line.push_str(" mA");
    console.write_output_line(framebuffer, line.as_str());

    for raw in record.descriptors() {
        match raw.descriptor_type {
            usb::DESCRIPTOR_TYPE_INTERFACE => {
                let Some(interface) = usb::InterfaceDescriptor::parse(raw.bytes) else {
                    continue;
                };
                let mut line = Line::new();
                line.push_str("  Interface ");
                line.push_u32(interface.number as u32);
                line.push_str(" alt ");
                line.push_u32(interface.alternate_setting as u32);
                line.push_str(": ");
                push_interface_class(&mut line, &interface);
                line.push_str(", ");
                line.push_u32(interface.num_endpoints as u32);
                line.push_str(if interface.num_endpoints == 1 {
                    " endpoint"
                } else {
                    " endpoints"
                });
                console.write_output_line(framebuffer, line.as_str());
                write_interface_string_line(console, framebuffer, record, &interface, language);
            }
            usb::DESCRIPTOR_TYPE_ENDPOINT => {
                let Some(endpoint) = usb::EndpointDescriptor::parse(raw.bytes) else {
                    continue;
                };
                let mut line = Line::new();
                line.push_str("    Endpoint 0x");
                line.push_hex(endpoint.address as u32, 2);
                line.push_str(": ");
                line.push_str(transfer_type_name(endpoint.transfer_type()));
                line.push_str(if endpoint.is_in() { " IN" } else { " OUT" });
                line.push_str(", mps ");
                line.push_u32(endpoint.max_packet_size as u32);
                line.push_str(", interval ");
                line.push_u32(endpoint.interval as u32);
                console.write_output_line(framebuffer, line.as_str());
            }
            usb::DESCRIPTOR_TYPE_HID => {
                let Some(hid) = usb::HidDescriptor::parse(raw.bytes) else {
                    continue;
                };
                let mut line = Line::new();
                line.push_str("    HID Descriptor: bcdHID ");
                push_bcd(&mut line, hid.version);
                line.push_str("  country ");
                line.push_u32(hid.country_code as u32);
                line.push_str("  report descriptor ");
                line.push_u32(hid.report_descriptor_len as u32);
                line.push_str(" bytes");
                console.write_output_line(framebuffer, line.as_str());
            }
            _ => {}
        }
    }

    if record.config_truncated() {
        console.write_output_line(
            framebuffer,
            "  (the rest of this configuration is longer than this host reads)",
        );
    }
}

/// An interface's own name, when it has one. Composite devices are where
/// this earns its control transfer: "Keyboard" and "Consumer Control" on
/// two HID interfaces say more than either interface descriptor does.
fn write_interface_string_line(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    record: &usb::DeviceRecord,
    interface: &usb::InterfaceDescriptor,
    language: Option<u16>,
) {
    let (Some(language), index) = (language, interface.string_index) else {
        return;
    };
    if index == 0 {
        return;
    }
    let mut line = Line::new();
    line.push_str("    iInterface ");
    line.push_u32(index as u32);
    line.push_str(": ");
    push_string(&mut line, record, index, language);
    console.write_output_line(framebuffer, line.as_str());
}

/// One string descriptor, read now and folded to ASCII; a device that does
/// not answer says so in place of the text rather than aborting the view.
fn push_string(line: &mut Line, record: &usb::DeviceRecord, index: u8, language: u16) {
    let mut text = [0u8; STRING_MAX];
    match record.read_string(index, language, &mut text) {
        Some(length) => line.push_ascii(&text[..length]),
        None => line.push_str("(device did not answer)"),
    }
}

fn push_vendor_product(line: &mut Line, record: &usb::DeviceRecord) {
    line.push_hex(record.summary.vendor_id as u32, 4);
    line.push_str(":");
    line.push_hex(record.summary.product_id as u32, 4);
}

fn push_location(line: &mut Line, location: Location) {
    match location {
        Location::Direct => line.push_str("USB-A directly"),
        Location::HubPort(port) => {
            line.push_str("hub port ");
            line.push_u32(port as u32);
        }
    }
}

fn push_class_triple(line: &mut Line, class: u8, subclass: u8, protocol: u8) {
    line.push_hex(class as u32, 2);
    line.push_str("/");
    line.push_hex(subclass as u32, 2);
    line.push_str("/");
    line.push_hex(protocol as u32, 2);
}

/// `03/01/01 HID Boot keyboard`: the raw triple, then whatever this
/// firmware can say about it in words.
fn push_interface_class(line: &mut Line, interface: &usb::InterfaceDescriptor) {
    push_class_triple(
        line,
        interface.class,
        interface.subclass,
        interface.protocol,
    );
    line.push_str(" ");
    line.push_str(class_name(interface.class));
    match interface.class {
        CLASS_HID => {
            if interface.subclass == HID_SUBCLASS_BOOT {
                line.push_str(match interface.protocol {
                    1 => " Boot keyboard",
                    2 => " Boot mouse",
                    _ => " Boot device",
                });
            }
        }
        CLASS_MASS_STORAGE => {
            line.push_str(" ");
            line.push_str(match interface.subclass {
                0x01 => "RBC",
                0x02 => "MMC-5",
                0x03 => "QIC-157",
                0x04 => "UFI",
                0x05 => "SFF-8070i",
                0x06 => "SCSI",
                0x07 => "LSD FS",
                0x08 => "IEEE 1667",
                _ => "unknown command set",
            });
            line.push_str(match interface.protocol {
                0x00 => ", CBI",
                0x01 => ", CBI (no interrupt)",
                0x50 => ", Bulk-Only",
                0x62 => ", UAS",
                _ => ", unknown transport",
            });
        }
        _ => {}
    }
}

const CLASS_HID: u8 = 0x03;
const CLASS_MASS_STORAGE: u8 = 0x08;
const HID_SUBCLASS_BOOT: u8 = 0x01;

/// The base class codes USB-IF assigns (`bDeviceClass`/`bInterfaceClass`).
/// Only the name; the subclass and protocol are shown as numbers next to
/// it, and spelled out above for the two classes this project drives.
fn class_name(class: u8) -> &'static str {
    match class {
        0x00 => "per-interface",
        0x01 => "Audio",
        0x02 => "Communications",
        CLASS_HID => "HID",
        0x05 => "Physical",
        0x06 => "Image",
        0x07 => "Printer",
        CLASS_MASS_STORAGE => "Mass Storage",
        0x09 => "Hub",
        0x0A => "CDC-Data",
        0x0B => "Smart Card",
        0x0D => "Content Security",
        0x0E => "Video",
        0x0F => "Personal Healthcare",
        0x10 => "Audio/Video",
        0x11 => "Billboard",
        0x12 => "USB Type-C Bridge",
        0xDC => "Diagnostic",
        0xE0 => "Wireless Controller",
        0xEF => "Miscellaneous",
        0xFE => "Application Specific",
        0xFF => "Vendor Specific",
        _ => "unknown class",
    }
}

fn transfer_type_name(transfer_type: u8) -> &'static str {
    match transfer_type {
        0 => "Control",
        1 => "Isochronous",
        2 => "Bulk",
        _ => "Interrupt",
    }
}

fn speed_name(speed: Speed) -> &'static str {
    match speed {
        Speed::High => "High-Speed",
        Speed::Full => "Full-Speed",
        Speed::Low => "Low-Speed",
        Speed::Unknown => "speed unknown",
    }
}

/// What a device's own lines (driver, interfaces) are indented by, so they
/// line up under the address in brackets rather than under the "port N:"
/// prefix that a device behind a hub is listed with.
fn detail_indent(location: Location) -> &'static str {
    match location {
        Location::Direct => "    ",
        Location::HubPort(_) => "          ",
    }
}

/// A BCD-encoded version (`bcdUSB`, `bcdDevice`, `bcdHID`) as `major.minor`:
/// each nibble is a decimal digit, so 0x0110 is 1.10 and 0x6400 is 64.00 --
/// not the 100.00 that reading the high byte as a number would give.
fn push_bcd(line: &mut Line, value: u16) {
    let major = (value >> 8) as u32;
    if major >= 0x10 {
        line.push_hex(major, 2);
    } else {
        line.push_hex(major, 1);
    }
    line.push_str(".");
    line.push_hex(value as u32 & 0xFF, 2);
}
