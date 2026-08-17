//! Generic USB protocol layer: control transfer staging (SETUP/DATA/STATUS)
//! and standard descriptor enumeration (USB2.0 chapter 9). Knows about
//! device/configuration descriptors and standard requests; knows nothing
//! about any particular device class. `hid_keyboard.rs` is built on top of
//! this, the same way a real HID class driver would sit above a generic
//! USB core.
//!
//! This is Stage 2 of `docs/USB_HOST_PLAN.md`.

use super::hcd::{self, Endpoint, HCCHAR_EPTYPE_CTRL, PacketOutcome, Route};
use crate::delay::{delay_ms, delay_us};
use crate::uart;

// Standard USB descriptor type codes (USB2.0 table 9-5).
pub const DESCRIPTOR_TYPE_DEVICE: u8 = 1;
pub const DESCRIPTOR_TYPE_CONFIGURATION: u8 = 2;
pub const DESCRIPTOR_TYPE_INTERFACE: u8 = 4;
pub const DESCRIPTOR_TYPE_ENDPOINT: u8 = 5;

// Standard request (USB2.0 table 9-4); class drivers building their own
// standard requests (e.g. `SET_CONFIGURATION`) reuse this constant.
pub const REQUEST_SET_CONFIGURATION: u8 = 0x09;

/// Address given to whatever is plugged into USB-A itself: a keyboard, or
/// the hub everything else hangs off.
pub const ROOT_DEVICE_ADDRESS: u8 = 1;

/// Address given to the device behind a specific hub port (1-based).
///
/// A real free-list address pool is not worth it here: a device's hub port
/// number is already a stable, unique identifier for as long as it stays
/// plugged in (`usb::registry::UsbHost` re-derives every address from
/// scratch on every rescan anyway, so there is no "free and reuse" case to
/// get wrong). `usb::registry::MAX_HUB_PORTS` bounds how many of these are
/// ever handed out, so the resulting addresses (2..=that+1) stay well
/// inside the 7-bit USB address space.
pub fn downstream_address(port: u8) -> u8 {
    ROOT_DEVICE_ADDRESS + port
}

/// A device's default control pipe (endpoint 0). Bundled because every
/// control transfer needs all three parts, and after `enumerate_device`
/// they are simply properties of the device.
#[derive(Clone, Copy)]
pub struct ControlPipe {
    pub device_address: u8,
    pub mps: u16,
    /// How the controller reaches this device; see `hcd::Route`, and
    /// `PREAMBLE_TRANSACTION_GAP_US` for what a Low-Speed device behind a
    /// Full-Speed hub costs here.
    pub route: Route,
}

// Generous for a single HID keyboard's configuration (typically config +
// interface + HID + one endpoint descriptor, ~34 bytes); composite devices
// with a few extra interfaces still fit comfortably.
const CONFIG_BUFFER_MAX: usize = 128;

// Control transfers let NAKs retry in hardware until success or a real
// error -- this is a generous wall-clock bound, matching `sdmmc.rs`'s
// command timeouts, not a retry budget of our own.
const CONTROL_TIMEOUT_ITERATIONS: u32 = 2_000_000;

/// A control-transfer transaction error can be transient: a hub may need
/// another frame after waking its downstream logic, and the DWC core can
/// report a one-off transaction error even though the root link is still
/// enabled.  Retrying the *whole* control transfer is important here: a
/// fresh SETUP packet resets EP0's control-transfer state (USB2.0 8.5.3.1),
/// whereas retrying only the failed DATA or STATUS packet could use the
/// wrong data toggle.
///
/// Earlier attempts run quietly.  That keeps a recovered hiccup invisible
/// to the UART; callers that need diagnostics retain the detailed HCD and
/// stage report when every attempt fails.
const CONTROL_TRANSFER_ATTEMPTS: u8 = 3;
const CONTROL_RETRY_DELAY_US: u32 = 1_000;

const STAGE_SETUP: &[u8] = b"SETUP";
const STAGE_IN_DATA: &[u8] = b"IN data";
const STAGE_OUT_STATUS: &[u8] = b"OUT status";
const STAGE_IN_STATUS: &[u8] = b"IN status";

/// One USB frame, waited between the packets of a control transfer to a
/// Low-Speed device behind a Full-Speed hub.
///
/// This core cannot run two preamble-prefixed transactions inside a single
/// frame; back-to-back SETUP/DATA/STATUS packets to such a device fail
/// with `HCINT.XCS_XACT_ERR` (confirmed on real hardware). ESP-IDF hits
/// the same limit and works around it identically, with an
/// `esp_rom_delay_us(1000)` between control transfer stages guarded by its
/// `ls_via_fs_hub` flag (`hcd_dwc.c`'s `_buffer_check_done`, "The HW can't
/// handle two transactions with preamble in one frame", IDF-12986).
///
/// Only Low-Speed-behind-a-hub pays this: a Low-Speed device plugged
/// straight into USB-A uses no preambles at all, and Full-Speed devices
/// are unaffected either way.
const PREAMBLE_TRANSACTION_GAP_US: u32 = 1_000;

/// How many SSPLIT/CSPLIT round trips one control packet to a device behind
/// a High-Speed hub's TT may take (`hcd::run_packet`'s `max_split_rounds`).
///
/// Generous, because for a control transfer a NAK means "busy, ask again"
/// and retrying is the whole point -- this stands in for the hardware NAK
/// retrying an unsplit control packet gets for free.
///
/// A successful split takes at least three rounds and a NAK restart costs
/// two, so the useful floor is well under ten; the headroom here is for a
/// device that NAKs its way through a slow moment, which enumeration hits
/// in practice. Measured on real hardware: a Low-Speed keyboard behind a
/// High-Speed hub enumerates fully at this budget, and failed to at 32.
const CONTROL_SPLIT_ROUNDS: u32 = 512;

pub struct EnumeratedDevice {
    pub device_address: u8,
    pub route: Route,
    pub max_packet_size0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub config_total_length: u16,
    pub num_interfaces: u8,
    pub configuration_value: u8,
    config_descriptor: [u8; CONFIG_BUFFER_MAX],
    config_descriptor_len: usize,
}

impl EnumeratedDevice {
    /// The control pipe to keep talking to this device on, so class
    /// drivers do not have to carry its address, packet size and speed
    /// around separately.
    pub fn control_pipe(&self) -> ControlPipe {
        ControlPipe {
            device_address: self.device_address,
            mps: self.max_packet_size0 as u16,
            route: self.route,
        }
    }

    /// The raw configuration descriptor bytes (config header, then every
    /// interface/endpoint/class-specific descriptor that followed it), for
    /// a class driver to walk looking for the interface it wants. Generic
    /// enumeration does not interpret these beyond the header fields
    /// above.
    pub fn config_bytes(&self) -> &[u8] {
        &self.config_descriptor[..self.config_descriptor_len]
    }
}

/// Enumerates a device whose port has already been reset and enabled --
/// the root port by `hcd::probe_port`, or a hub's downstream port by
/// `hub::Hub::reset_port`. Runs an 8-byte device descriptor peek (to learn
/// EP0's real max packet size, USB2.0 9.2.6.3), `SET_ADDRESS`, the full
/// device descriptor, and the configuration descriptor.
///
/// `address` is the address to assign. `route` says how the controller has
/// to reach the device -- preambles, split transactions, or neither -- and
/// is `Route::default()` for anything plugged into USB-A directly, whatever
/// its speed, since then the bus itself runs at the device's speed. The
/// caller derives it from where the device turned up; see
/// `hcd::Route` and `usb::registry::UsbHost::attach_hub`.
///
/// Only one device may be in the unaddressed default state at a time, so
/// this must not be interleaved with another enumeration.
///
/// Does not issue `SET_CONFIGURATION` or interpret the configuration
/// descriptor beyond its header; the class driver does both once it has
/// decided it actually wants to talk to an interface found in
/// `config_bytes()`.
pub fn enumerate_device(address: u8, route: Route) -> Option<EnumeratedDevice> {
    // Before SET_ADDRESS the device answers on address 0, and EP0's real
    // packet size is not known yet -- 8 bytes is the one size every
    // device supports (USB2.0 5.5.3).
    let mut pipe = ControlPipe {
        device_address: 0,
        mps: 8,
        route,
    };

    let mut peek = [0u8; 8];
    let setup = build_get_descriptor_setup(DESCRIPTOR_TYPE_DEVICE, 0, 8);
    if control_transfer_in(&pipe, &setup, &mut peek).is_none() {
        uart::log(b"USB: initial 8-byte device descriptor read failed\r\n");
        return None;
    }
    if peek[7] != 0 {
        pipe.mps = peek[7] as u16;
    }

    let setup = build_set_address_setup(address);
    if !control_transfer_out_no_data(&pipe, &setup) {
        uart::log(b"USB: SET_ADDRESS failed\r\n");
        return None;
    }
    pipe.device_address = address;
    // USB2.0 9.2.6.3 allows the device up to 2ms to be ready to respond at
    // its new address; padded generously, matching this project's general
    // preference for margin over spec minimums.
    delay_ms(10);

    let mut device_descriptor = [0u8; 18];
    let setup = build_get_descriptor_setup(DESCRIPTOR_TYPE_DEVICE, 0, 18);
    if control_transfer_in(&pipe, &setup, &mut device_descriptor).is_none() {
        uart::log(b"USB: full device descriptor read failed\r\n");
        return None;
    }
    if device_descriptor[7] != 0 {
        pipe.mps = device_descriptor[7] as u16;
    }

    let mut config_header = [0u8; 9];
    let setup = build_get_descriptor_setup(DESCRIPTOR_TYPE_CONFIGURATION, 0, 9);
    if control_transfer_in(&pipe, &setup, &mut config_header).is_none() {
        uart::log(b"USB: configuration descriptor header read failed\r\n");
        return None;
    }
    let total_length = (u16::from_le_bytes([config_header[2], config_header[3]]) as usize)
        .clamp(9, CONFIG_BUFFER_MAX);

    let mut config_descriptor = [0u8; CONFIG_BUFFER_MAX];
    let setup = build_get_descriptor_setup(DESCRIPTOR_TYPE_CONFIGURATION, 0, total_length as u16);
    let Some(received) = control_transfer_in(&pipe, &setup, &mut config_descriptor[..total_length])
    else {
        uart::log(b"USB: full configuration descriptor read failed\r\n");
        return None;
    };

    Some(EnumeratedDevice {
        device_address: address,
        route,
        max_packet_size0: pipe.mps as u8,
        vendor_id: u16::from_le_bytes([device_descriptor[8], device_descriptor[9]]),
        product_id: u16::from_le_bytes([device_descriptor[10], device_descriptor[11]]),
        device_class: device_descriptor[4],
        device_subclass: device_descriptor[5],
        device_protocol: device_descriptor[6],
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
///
/// `setup` is passed in whole rather than built here, so class drivers can
/// issue their own class-specific IN requests (`hub.rs`'s hub descriptor
/// and status reads) through the same staging as standard ones.
pub fn control_transfer_in(
    pipe: &ControlPipe,
    setup: &[u8; 8],
    buffer: &mut [u8],
) -> Option<usize> {
    control_transfer_in_with_diagnostics(pipe, setup, buffer, true)
}

/// The same complete-transfer recovery as [`control_transfer_in`], but with
/// no UART diagnostics if its retry budget is exhausted.  The hub registry
/// uses this while doing its opportunistic empty-port scan, where a failure
/// must not disturb already-attached devices.  That scan reports one concise
/// summary and pauses until an explicit or genuine-device-error rescan.
pub fn control_transfer_in_quiet(
    pipe: &ControlPipe,
    setup: &[u8; 8],
    buffer: &mut [u8],
) -> Option<usize> {
    control_transfer_in_with_diagnostics(pipe, setup, buffer, false)
}

fn control_transfer_in_with_diagnostics(
    pipe: &ControlPipe,
    setup: &[u8; 8],
    buffer: &mut [u8],
    log_failure: bool,
) -> Option<usize> {
    retry_control_transfer(log_failure, |quiet_errors| {
        let mut setup_buf = *setup;
        run_control_packet(pipe, false, true, false, quiet_errors, &mut setup_buf)
            .ok_or(STAGE_SETUP)?;

        let received = if buffer.is_empty() {
            0
        } else {
            data_stage_in(pipe, buffer, quiet_errors).ok_or(STAGE_IN_DATA)?
        };

        run_control_packet(pipe, false, false, true, quiet_errors, &mut [])
            .ok_or(STAGE_OUT_STATUS)?;
        Ok(received)
    })
}

/// Runs a control transfer with no data stage (SETUP, IN status stage),
/// e.g. `SET_ADDRESS`, `SET_CONFIGURATION`, or a class request.
pub fn control_transfer_out_no_data(pipe: &ControlPipe, setup: &[u8; 8]) -> bool {
    retry_control_transfer(true, |quiet_errors| {
        let mut setup_buf = *setup;
        run_control_packet(pipe, false, true, false, quiet_errors, &mut setup_buf)
            .ok_or(STAGE_SETUP)?;
        run_control_packet(pipe, true, false, true, quiet_errors, &mut [])
            .ok_or(STAGE_IN_STATUS)?;
        Ok(())
    })
    .is_some()
}

/// Re-runs a complete EP0 transaction after a transient packet failure.
///
/// The HCD's first two attempts are deliberately quiet.  When diagnostics
/// are requested, the final attempt retains its raw HCINT/HPRT diagnostic
/// and is followed by one stage name, so a persistent failure remains
/// actionable without flooding normal recovered errors into the UART log.
fn retry_control_transfer<T>(
    log_failure: bool,
    mut transfer: impl FnMut(bool) -> Result<T, &'static [u8]>,
) -> Option<T> {
    for attempt in 0..CONTROL_TRANSFER_ATTEMPTS {
        let final_attempt = attempt + 1 == CONTROL_TRANSFER_ATTEMPTS;
        match transfer(!final_attempt || !log_failure) {
            Ok(result) => return Some(result),
            Err(stage) if final_attempt => {
                if log_failure {
                    log_failed_stage(stage);
                }
                return None;
            }
            Err(_) => delay_us(CONTROL_RETRY_DELAY_US),
        }
    }
    unreachable!("control-transfer retry loop always returns");
}

/// Names the control transfer stage that just failed.
///
/// Which stage died is the first thing worth knowing about a failing
/// transfer -- a SETUP that never lands means the device is not reachable
/// at all, while a SETUP that works followed by a failing data stage is a
/// timing or toggle problem -- and `hcd.rs` cannot tell them apart, since
/// at its level all three are just packets.
fn log_failed_stage(stage: &[u8]) {
    uart::log(b"USB: control transfer failed at the ");
    uart::log(stage);
    uart::log(b" stage\r\n");
}

/// Repeats MPS-sized IN packets (data stage PID always starts at DATA1,
/// toggling per packet) until `buffer` is full or a short packet signals
/// the end of the data, per USB2.0 8.5.3.
fn data_stage_in(pipe: &ControlPipe, buffer: &mut [u8], quiet_errors: bool) -> Option<usize> {
    let mut received = 0usize;
    let mut pid_data1 = true;
    while received < buffer.len() {
        let chunk_len = (buffer.len() - received).min(pipe.mps.max(1) as usize);
        let got = run_control_packet(
            pipe,
            true,
            false,
            pid_data1,
            quiet_errors,
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

/// `hcd::run_packet` on EP0 (control transfers). NAKs are expected to retry
/// in hardware until success or a real error, so the timeout is long and
/// never treated as routine. `quiet_errors` is used by the whole-transfer
/// recovery loop above; it suppresses diagnostics only until an attempt has
/// actually exhausted the retry budget.
fn run_control_packet(
    pipe: &ControlPipe,
    is_in: bool,
    is_setup: bool,
    pid_data1: bool,
    quiet_errors: bool,
    buffer: &mut [u8],
) -> Option<usize> {
    // Only the preamble path needs the inter-packet gap. A split transfer
    // never puts a PRE token on the wire at all: the host talks High-Speed
    // to the hub, and it is the hub's TT that does the Low-Speed signalling
    // downstream, entirely out of this core's frame budget.
    if pipe.route.low_speed_via_hub && pipe.route.split.is_none() {
        delay_us(PREAMBLE_TRANSACTION_GAP_US);
    }
    let endpoint = Endpoint {
        device_address: pipe.device_address,
        endpoint_number: 0,
        endpoint_type: HCCHAR_EPTYPE_CTRL,
        mps: pipe.mps,
        is_in,
        route: pipe.route,
    };
    match hcd::run_packet(
        &endpoint,
        is_setup,
        pid_data1,
        CONTROL_TIMEOUT_ITERATIONS,
        CONTROL_SPLIT_ROUNDS,
        quiet_errors,
        quiet_errors,
        buffer,
    ) {
        PacketOutcome::Ok(n) => Some(n),
        PacketOutcome::Timeout | PacketOutcome::Error => None,
    }
}
