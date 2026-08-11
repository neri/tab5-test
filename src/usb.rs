//! USB-A host support, layered like a real USB stack:
//!
//! - `hcd`: the ESP32-P4 High-Speed USB-DWC host controller driver (VBUS
//!   power, core/port bring-up, the raw channel/packet primitive). Knows
//!   about registers, channels, and packets; nothing about USB devices or
//!   descriptors. See its module doc for the hardware background (UTMI
//!   PHY, dedicated pins, the VBUS switch bit).
//! - `protocol`: generic USB control transfers and standard descriptor
//!   enumeration (USB2.0 chapter 9), independent of any device class.
//! - `hid_keyboard`: the HID Boot Protocol keyboard class driver
//!   (`UsbKeyboard`) built on top of the two layers above -- the actual
//!   `USB_HOST_PLAN.md` Stage 3 milestone.
//! - `hub`: the USB hub class driver, a sibling of `hid_keyboard` on top
//!   of the same two layers.
//!
//! `connect_keyboard` below is the one place that combines all four, since
//! deciding *what is plugged in* is not any single layer's job.
//!
//! Staged per `USB_HOST_PLAN.md`: `hcd::probe_port` is Stage 1,
//! `protocol::enumerate_device` is Stage 2, `hid_keyboard::UsbKeyboard` is
//! Stage 3, and `hub::Hub` (plus `hcd::FORCE_FS_LS_ONLY_HOST`) is Stage 4.

mod hcd;
mod hid_keyboard;
mod hub;
mod protocol;

pub use hcd::{FORCE_FS_LS_ONLY_HOST, Speed, probe_port, set_vbus_bit};
pub use hid_keyboard::{UsbKeyboard, find_hid_keyboard};
pub use hub::{Hub, OverCurrentProtection, PortStatus, PowerSwitching};
pub use protocol::{DOWNSTREAM_DEVICE_ADDRESS, ROOT_DEVICE_ADDRESS, enumerate_device};

use crate::uart;

/// Brings USB-A up from scratch and returns a keyboard handle if one can
/// be reached -- either plugged in directly, or through the first usable
/// port of a hub plugged in directly.
///
/// This is what `lcd.rs`'s frame loop calls, both at startup and whenever
/// it decides to reconnect, so the two topologies are interchangeable from
/// its point of view. Everything is re-done on every call (the port is
/// reset, addresses are reassigned), exactly like `hcd::probe_port`: there
/// is no persistent bus state to keep in sync.
///
/// Only one device is driven at a time, per `USB_HOST_PLAN.md` Stage 4 --
/// a keyboard in the hub and another one in USB-A directly is out of
/// scope, and so is a second device on another hub port.
pub fn connect_keyboard() -> Option<UsbKeyboard> {
    let port = probe_port();
    if !port.enabled {
        return None;
    }

    // Whatever is plugged straight into USB-A sets the bus speed itself,
    // so it never needs preambles -- even a Low-Speed keyboard, which just
    // puts the whole bus at Low-Speed.
    let device = enumerate_device(ROOT_DEVICE_ADDRESS, false)?;
    if device.device_class == hub::DEVICE_CLASS_HUB {
        return connect_keyboard_through_hub(&device);
    }
    UsbKeyboard::attach(&device)
}

fn connect_keyboard_through_hub(device: &protocol::EnumeratedDevice) -> Option<UsbKeyboard> {
    let hub = Hub::open(device)?;
    if !hub.power_on_all_ports() {
        return None;
    }
    let port = hub.find_connected_port()?;
    let status = hub.reset_port(port)?;

    uart::log_hex(b"USB: enumerating device on hub port ", port as u32);
    // The hub keeps address 1; the device behind it gets address 2, and
    // its speed comes from the hub port rather than the root port -- a
    // Low-Speed keyboard on a Full-Speed bus needs the host to send PRE
    // tokens for it (`hcd::Endpoint::low_speed_via_hub`).
    let downstream = enumerate_device(DOWNSTREAM_DEVICE_ADDRESS, status.speed() == Speed::Low)?;
    UsbKeyboard::attach(&downstream)
}
