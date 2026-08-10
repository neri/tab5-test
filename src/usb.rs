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
//!
//! Staged per `USB_HOST_PLAN.md`: `hcd::probe_port` is Stage 1,
//! `protocol::enumerate_device` is Stage 2, `hid_keyboard::UsbKeyboard` is
//! Stage 3.

mod hcd;
mod hid_keyboard;
mod protocol;

pub use hcd::{Speed, probe_port, set_vbus_bit};
pub use hid_keyboard::{UsbKeyboard, find_hid_keyboard};
pub use protocol::enumerate_device;
