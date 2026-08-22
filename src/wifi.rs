//! Wi-Fi through the ESP32-C6 co-processor.
//!
//! The C6 runs Espressif's ESP-Hosted slave firmware, so this is not a
//! Wi-Fi driver: it is a transport to a co-processor that owns the radio.
//! The module follows the same shape as `usb.rs` -- this file only declares
//! the submodules and re-exports what the rest of the firmware uses.
//!
//! Layering, bottom to top:
//!
//! - `sdio.rs` (outside this module) activates the C6 as an SDIO card and
//!   provides CMD52/CMD53
//! - [`hosted`] carries ESP-Hosted frames over that bus and performs the
//!   slave's initialization handshake
//! - [`proto`] is the sliver of protobuf the RPC messages are built from,
//!   and [`rpc`] turns `esp_wifi_*` calls into those messages and back
//! - [`station`] is the Wi-Fi itself: initialize, scan, connect

pub mod hosted;
pub mod proto;
pub mod rpc;
pub mod station;

pub use hosted::bring_up;
pub use rpc::Rpc;
