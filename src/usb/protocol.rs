//! Generic USB protocol layer: control transfer staging (SETUP/DATA/STATUS)
//! and standard descriptor enumeration (USB2.0 chapter 9). Knows about
//! device/configuration descriptors and standard requests; knows nothing
//! about any particular device class. `hid_keyboard.rs` is built on top of
//! this, the same way a real HID class driver would sit above a generic
//! USB core.
//!
//! This is Stage 2 of `USB_HOST_PLAN.md`.

use super::hcd::{self, HCCHAR_EPTYPE_CTRL, PacketOutcome};
use crate::uart;

// Standard USB descriptor type codes (USB2.0 table 9-5).
pub const DESCRIPTOR_TYPE_DEVICE: u8 = 1;
pub const DESCRIPTOR_TYPE_CONFIGURATION: u8 = 2;
pub const DESCRIPTOR_TYPE_INTERFACE: u8 = 4;
pub const DESCRIPTOR_TYPE_ENDPOINT: u8 = 5;

// Standard request (USB2.0 table 9-4); class drivers building their own
// standard requests (e.g. `SET_CONFIGURATION`) reuse this constant.
pub const REQUEST_SET_CONFIGURATION: u8 = 0x09;

/// This project only ever enumerates a single directly-attached device (no
/// hub support), so every enumerated device gets the same fixed address.
pub const DEVICE_ADDRESS: u8 = 1;

// Generous for a single HID keyboard's configuration (typically config +
// interface + HID + one endpoint descriptor, ~34 bytes); composite devices
// with a few extra interfaces still fit comfortably.
const CONFIG_BUFFER_MAX: usize = 128;

// Control transfers let NAKs retry in hardware until success or a real
// error -- this is a generous wall-clock bound, matching `sdmmc.rs`'s
// command timeouts, not a retry budget of our own.
const CONTROL_TIMEOUT_ITERATIONS: u32 = 2_000_000;

pub struct EnumeratedDevice {
    pub max_packet_size0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub num_configurations: u8,
    pub config_total_length: u16,
    pub num_interfaces: u8,
    pub configuration_value: u8,
    config_descriptor: [u8; CONFIG_BUFFER_MAX],
    config_descriptor_len: usize,
}

impl EnumeratedDevice {
    /// The raw configuration descriptor bytes (config header, then every
    /// interface/endpoint/class-specific descriptor that followed it), for
    /// a class driver to walk looking for the interface it wants. Generic
    /// enumeration does not interpret these beyond the header fields
    /// above.
    pub fn config_bytes(&self) -> &[u8] {
        &self.config_descriptor[..self.config_descriptor_len]
    }
}

/// Enumerates whatever is attached to USB-A, assuming `hcd::probe_port` has
/// already reset the port and reported `enabled`. Runs an 8-byte device
/// descriptor peek (to learn EP0's real max packet size, USB2.0 9.2.6.3),
/// `SET_ADDRESS`, the full device descriptor, and the configuration
/// descriptor.
///
/// Does not issue `SET_CONFIGURATION` or interpret the configuration
/// descriptor beyond its header; `hid_keyboard::UsbKeyboard::init` does
/// both once it has decided it actually wants to talk to a HID keyboard
/// interface found in `config_bytes()`.
pub fn enumerate_device() -> Option<EnumeratedDevice> {
    let mut peek = [0u8; 8];
    let setup = build_get_descriptor_setup(DESCRIPTOR_TYPE_DEVICE, 0, 8);
    if control_transfer_in(0, 8, &setup, &mut peek).is_none() {
        uart::log(b"USB: initial 8-byte device descriptor read failed\r\n");
        return None;
    }
    let mps0 = if peek[7] == 0 { 8 } else { peek[7] as u16 };

    let setup = build_set_address_setup(DEVICE_ADDRESS);
    if !control_transfer_out_no_data(0, mps0, &setup) {
        uart::log(b"USB: SET_ADDRESS failed\r\n");
        return None;
    }
    // USB2.0 9.2.6.3 allows the device up to 2ms to be ready to respond at
    // its new address; padded generously, matching this project's general
    // preference for margin over spec minimums.
    hcd::delay_ms(10);

    let mut device_descriptor = [0u8; 18];
    let setup = build_get_descriptor_setup(DESCRIPTOR_TYPE_DEVICE, 0, 18);
    if control_transfer_in(DEVICE_ADDRESS, mps0, &setup, &mut device_descriptor).is_none() {
        uart::log(b"USB: full device descriptor read failed\r\n");
        return None;
    }
    let mps0 = if device_descriptor[7] != 0 { device_descriptor[7] as u16 } else { mps0 };

    let mut config_header = [0u8; 9];
    let setup = build_get_descriptor_setup(DESCRIPTOR_TYPE_CONFIGURATION, 0, 9);
    if control_transfer_in(DEVICE_ADDRESS, mps0, &setup, &mut config_header).is_none() {
        uart::log(b"USB: configuration descriptor header read failed\r\n");
        return None;
    }
    let total_length = (u16::from_le_bytes([config_header[2], config_header[3]]) as usize)
        .clamp(9, CONFIG_BUFFER_MAX);

    let mut config_descriptor = [0u8; CONFIG_BUFFER_MAX];
    let setup = build_get_descriptor_setup(DESCRIPTOR_TYPE_CONFIGURATION, 0, total_length as u16);
    let Some(received) =
        control_transfer_in(DEVICE_ADDRESS, mps0, &setup, &mut config_descriptor[..total_length])
    else {
        uart::log(b"USB: full configuration descriptor read failed\r\n");
        return None;
    };

    Some(EnumeratedDevice {
        max_packet_size0: mps0 as u8,
        vendor_id: u16::from_le_bytes([device_descriptor[8], device_descriptor[9]]),
        product_id: u16::from_le_bytes([device_descriptor[10], device_descriptor[11]]),
        device_class: device_descriptor[4],
        device_subclass: device_descriptor[5],
        device_protocol: device_descriptor[6],
        num_configurations: device_descriptor[17],
        config_total_length: total_length as u16,
        num_interfaces: config_header[4],
        configuration_value: config_header[5],
        config_descriptor,
        config_descriptor_len: received,
    })
}

fn build_get_descriptor_setup(descriptor_type: u8, index: u8, length: u16) -> [u8; 8] {
    let value = ((descriptor_type as u16) << 8) | index as u16;
    [
        0x80, // bmRequestType: device-to-host, standard, device
        0x06, // bRequest: GET_DESCRIPTOR
        (value & 0xFF) as u8,
        (value >> 8) as u8,
        0,
        0,
        (length & 0xFF) as u8,
        (length >> 8) as u8,
    ]
}

fn build_set_address_setup(address: u8) -> [u8; 8] {
    [0x00, 0x05, address, 0, 0, 0, 0, 0] // bRequest 0x05 = SET_ADDRESS, wValue = address
}

/// Builds a standard (bmRequestType 0x00) host-to-device setup packet with
/// no data stage, e.g. `SET_CONFIGURATION`.
pub fn build_standard_out_setup(request: u8, value: u16, index: u16) -> [u8; 8] {
    [
        0x00,
        request,
        (value & 0xFF) as u8,
        (value >> 8) as u8,
        (index & 0xFF) as u8,
        (index >> 8) as u8,
        0,
        0,
    ]
}

/// Runs a full IN control transfer (SETUP, IN data stage if `buffer` is
/// non-empty, OUT status stage) and returns the number of bytes actually
/// received -- which can be less than `buffer.len()` on a short packet,
/// exactly as with the deliberate 8-byte device descriptor peek.
fn control_transfer_in(device_address: u8, mps: u16, setup: &[u8; 8], buffer: &mut [u8]) -> Option<usize> {
    let mut setup_buf = *setup;
    run_control_packet(device_address, mps, false, true, false, &mut setup_buf)?;

    let received = if buffer.is_empty() {
        0
    } else {
        data_stage_in(device_address, mps, buffer)?
    };

    run_control_packet(device_address, mps, false, false, true, &mut [])?;
    Some(received)
}

/// Runs a control transfer with no data stage (SETUP, IN status stage),
/// e.g. `SET_ADDRESS`, `SET_CONFIGURATION`, or a class request.
pub fn control_transfer_out_no_data(device_address: u8, mps: u16, setup: &[u8; 8]) -> bool {
    let mut setup_buf = *setup;
    if run_control_packet(device_address, mps, false, true, false, &mut setup_buf).is_none() {
        return false;
    }
    run_control_packet(device_address, mps, true, false, true, &mut []).is_some()
}

/// Repeats MPS-sized IN packets (data stage PID always starts at DATA1,
/// toggling per packet) until `buffer` is full or a short packet signals
/// the end of the data, per USB2.0 8.5.3.
fn data_stage_in(device_address: u8, mps: u16, buffer: &mut [u8]) -> Option<usize> {
    let mut received = 0usize;
    let mut pid_data1 = true;
    while received < buffer.len() {
        let chunk_len = (buffer.len() - received).min(mps.max(1) as usize);
        let got = run_control_packet(
            device_address,
            mps,
            true,
            false,
            pid_data1,
            &mut buffer[received..received + chunk_len],
        )?;
        received += got;
        pid_data1 = !pid_data1;
        if got < chunk_len {
            break; // short packet: device has no more data
        }
    }
    Some(received)
}

/// `hcd::run_packet` on EP0 (control transfers): errors are always worth
/// logging (they are rare and something is actually wrong), and NAKs are
/// expected to retry in hardware until success or a real error, so the
/// timeout is long and never treated as routine.
fn run_control_packet(
    device_address: u8,
    mps: u16,
    is_in: bool,
    is_setup: bool,
    pid_data1: bool,
    buffer: &mut [u8],
) -> Option<usize> {
    match hcd::run_packet(
        device_address,
        0,
        HCCHAR_EPTYPE_CTRL,
        mps,
        is_in,
        is_setup,
        pid_data1,
        CONTROL_TIMEOUT_ITERATIONS,
        false,
        false,
        buffer,
    ) {
        PacketOutcome::Ok(n) => Some(n),
        PacketOutcome::Timeout | PacketOutcome::Error => None,
    }
}
