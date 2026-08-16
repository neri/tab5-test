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
//!   `docs/USB_HOST_PLAN.md` Stage 3 milestone.
//! - `hub`: the USB hub class driver, a sibling of `hid_keyboard` on top
//!   of the same two layers.
//! - `msc`: the USB Mass Storage (Bulk-Only Transport) class driver, another
//!   sibling of `hid_keyboard`/`hub`.
//! - `registry`: `UsbHost`, the single owner of the bus and the device
//!   registry that decides *what is plugged in* -- one device directly, or
//!   every occupied port of a hub -- and holds every class driver handle
//!   the frame loop and shell commands share. See its module doc and
//!   `docs/USB_REFACTOR_PLAN.md`.
//!
//! Staged per `docs/USB_HOST_PLAN.md`: `hcd::probe_port` is Stage 1,
//! `protocol::enumerate_device` is Stage 2, `hid_keyboard::UsbKeyboard` is
//! Stage 3, and `hub::Hub` (plus `hcd::FORCE_FS_LS_ONLY_HOST`) is Stage 4.
//! `msc` follows `docs/USB_MSC_PLAN.md`. `registry` follows `docs/USB_REFACTOR_PLAN.md`,
//! which replaced this module's old single-keyboard `connect_keyboard`.

mod hcd;
mod hid_keyboard;
mod hub;
mod msc;
mod protocol;
mod registry;

pub use hcd::{
    FORCE_FS_LS_ONLY_HOST, Speed, probe_split_support, set_pi4ioe2_output_bit, set_vbus_bit,
};
pub use hub::{OverCurrentProtection, PortStatus, PowerSwitching};
pub use registry::{DeviceKind, DeviceSummary, Location, MAX_HUB_PORTS, UsbHost};
