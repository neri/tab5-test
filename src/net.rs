//! IPv4 over the ESP32-C6 station interface.
//!
//! Unlike the rest of the crate root this is not a hardware layer: the
//! protocols are [smoltcp](https://docs.rs/smoltcp)'s. The repository
//! implements its own hardware layers to keep vendor code from hiding what
//! the silicon is doing, and that reason does not carry over to ARP, IPv4,
//! ICMP, UDP, DHCP and TCP -- those are fully specified by RFCs and
//! observable with a packet capture, so an outside implementation stays
//! auditable in a way a vendor HAL does not.
//!
//! Like `usb.rs` and `wifi.rs`, this file only declares the submodules and
//! re-exports what the rest of the firmware uses.
//!
//! Layering, bottom to top:
//!
//! - `wifi::Rpc` (outside this module) queues received 802.3 frames from
//!   the C6 and sends frames back to it
//! - [`device`] is smoltcp's `phy::Device` over that queue
//! - [`stack`] owns the interface, the socket set and the DHCP client, and
//!   is what a shell command drives
//! - [`tls`] is a transport rather than a client: it owns a TCP socket like
//!   the clients do, but what comes out of it is plaintext for one of them
//!   to read
//! - [`pins`] is the SPKI pin table, generated from text a person edits and
//!   compiled in: what makes a TLS connection to a known host authenticated
//! - [`transport`] is the choice between a plain TCP stream and a TLS one,
//!   made once by whoever knows the URL's scheme. [`http`] reads bytes from
//!   it and never sees a socket
//! - [`dns`], [`ping`], [`tftp`] and [`http`] are the clients that run on
//!   that stack's sockets, one file each in the same way `usb/` splits its
//!   class drivers. [`dns`] is the odd one: its socket belongs to
//!   [`stack`], because a resolver setting outlives any one command

pub mod device;
pub mod dns;
pub mod http;
pub mod pins;
pub mod ping;
pub mod stack;
pub mod tftp;
pub mod tls;
pub mod transport;

pub use stack::{AddressSource, Stack};
