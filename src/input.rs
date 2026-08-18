//! Unified keyboard input, pointer input, and input-source lifecycle
//! management.
//!
//! This module is the application-level owner of all sources that can emit
//! console keys.  It deliberately does not replace `usb::UsbHost`: that type
//! remains the sole owner of USB-A enumeration, hub state, and non-keyboard
//! USB devices such as Mass Storage.
//!
//! Pointer input is forwarded rather than normalized the way keys are.  A
//! key is meaningful on its own, so `Key` hides which keyboard produced it;
//! mouse motion is relative and only becomes a position once something
//! decides what it is moving across, so `poll_mouse` hands `usb::MouseUpdate`
//! straight through and leaves the cursor position to the screen that draws
//! one (`app::win`).  That also keeps this module free of any dependency on
//! the framebuffer's geometry.

use crate::cardkb::CardKb;
use crate::{interrupts, uart, usb};

const CARDKB_RECONNECT_FRAMES: u32 = 60;
const HUB_PORT_SCAN_FRAMES: u32 = 60;
const ROOT_RESCAN_FRAMES: u32 = 300;

/// A normalized key understood by application-level input consumers.
///
/// Text and non-text keys share one fixed-size representation, so input
/// consumers never need to know whether a key came from CardKB, USB HID, or a
/// future keyboard driver.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Key {
    Ascii(u8),
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Function(u8),
}

/// The physical input path that produced a key.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum KeySource {
    CardKb,
    Usb,
}

/// One normalized keyboard event.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct KeyEvent {
    pub source: KeySource,
    pub key: Key,
}

/// Owns every keyboard input source and maintains their connection state.
///
/// `service` performs periodic connection maintenance and `poll_key` only
/// reads keys.  Call both once at each display-frame boundary.
pub struct InputManager {
    cardkb: Option<CardKb>,
    cardkb_reconnect_frames: u32,
    usb_host: usb::UsbHost,
    usb_reconnect_frames: u32,
    next_source: KeySource,
}

impl InputManager {
    /// Initializes the directly connected CardKB and the USB-A host.
    pub fn new() -> Self {
        let cardkb = if crate::i2c::initialize_cardkb_bus().is_ok() {
            CardKb::init()
        } else {
            None
        };
        if cardkb.is_some() {
            uart::log(b"CardKB: ready\r\n");
        } else {
            uart::log(b"CardKB: absent\r\n");
        }

        let mut usb_host = usb::UsbHost::new();
        usb_host.rescan();
        uart::log(b"USB: initial scan complete\r\n");

        Self {
            cardkb,
            cardkb_reconnect_frames: 0,
            usb_host,
            usb_reconnect_frames: 0,
            next_source: KeySource::CardKb,
        }
    }

    /// Advances CardKB reconnection and USB device-discovery state.
    pub fn service(&mut self) {
        if self.cardkb.is_none() {
            self.cardkb_reconnect_frames += 1;
            if self.cardkb_reconnect_frames == CARDKB_RECONNECT_FRAMES {
                self.cardkb_reconnect_frames = 0;
                self.cardkb = CardKb::init();
                if self.cardkb.is_some() {
                    uart::log(b"CardKB: connected\r\n");
                }
            }
        }

        // A root-port disconnect is a cheap register read.  A stale keyboard
        // session needs immediate recovery, while an empty root port is
        // deliberately rescanned only at a coarse interval because probing
        // it performs a blocking USB reset and debounce sequence.
        if self.usb_host.root_disconnected() {
            if !self.usb_host.is_empty() {
                self.usb_host.clear();
                uart::log(b"USB: nothing connected to USB-A\r\n");
            }
        } else if self.usb_host.needs_reinit() {
            uart::log(b"USB: a device session went stale, rescanning...\r\n");
            self.usb_host.rescan();
            self.usb_reconnect_frames = 0;
        }

        if self.usb_host.has_room() {
            self.usb_reconnect_frames += 1;
            if self.usb_host.hub().is_some() {
                if self.usb_reconnect_frames >= HUB_PORT_SCAN_FRAMES {
                    self.usb_reconnect_frames = 0;
                    self.usb_host.scan_empty_hub_ports();
                }
            } else if self.usb_reconnect_frames >= ROOT_RESCAN_FRAMES {
                self.usb_reconnect_frames = 0;
                self.usb_host.rescan();
            }
        }
    }

    /// Returns at most one key, alternating the source checked first after
    /// every delivered key so a continuously active source cannot starve the
    /// other one.
    pub fn poll_key(&mut self) -> Option<KeyEvent> {
        let first = self.next_source;
        for source in [first, other_source(first)] {
            let key = match source {
                KeySource::CardKb => self
                    .cardkb
                    .as_mut()
                    .and_then(CardKb::poll)
                    .map(key_from_ascii),
                KeySource::Usb => self.usb_host.poll_keyboards(),
            };
            if let Some(key) = key {
                self.next_source = other_source(source);
                return Some(KeyEvent { source, key });
            }
        }
        None
    }

    /// Blocks until any key arrives, servicing input sources once per frame.
    ///
    /// The full-screen modes end this way, so the pairing of `service` and
    /// `poll_key` with the frame boundary lives here rather than being
    /// rewritten by each of them. Waiting on the frame interrupt is what keeps
    /// a mode that has nothing left to draw from polling I2C and USB flat out;
    /// it also means a keyboard connected *after* the mode was entered is still
    /// discovered, because `service` keeps running while the mode waits.
    pub fn wait_for_key(&mut self) {
        let mut sequence = interrupts::frame_sequence();
        loop {
            interrupts::wait_for_interrupt();
            let next_sequence = interrupts::frame_sequence();
            if next_sequence == sequence {
                continue;
            }
            sequence = next_sequence;
            self.service();
            if self.poll_key().is_some() {
                return;
            }
        }
    }

    /// Returns this frame's combined mouse motion and button state across
    /// every attached USB mouse, or `None` if none of them moved.
    ///
    /// Unlike `poll_key` this has no CardKB counterpart to alternate with,
    /// and no per-call limit: `UsbHost::poll_mice` drains and sums whatever
    /// arrived since the last call, so calling it once per frame loses no
    /// motion.
    pub fn poll_mouse(&mut self) -> Option<usb::MouseUpdate> {
        self.usb_host.poll_mice()
    }

    /// True if a USB mouse is currently attached, so a pointer-driven screen
    /// can say up front that there is nothing to move the cursor with.
    pub fn has_mouse(&self) -> bool {
        self.usb_host.has_mouse()
    }

    /// Mutable USB bus registry for commands such as `usbrescan` and MSC I/O.
    pub fn usb_host_mut(&mut self) -> &mut usb::UsbHost {
        &mut self.usb_host
    }
}

const fn other_source(source: KeySource) -> KeySource {
    match source {
        KeySource::CardKb => KeySource::Usb,
        KeySource::Usb => KeySource::CardKb,
    }
}

const fn key_from_ascii(byte: u8) -> Key {
    match byte {
        0x1B => Key::Escape,
        // CardKB v1.1's normal/caps/symbol key maps use these non-ASCII
        // values for the four printed cursor keys.
        0xB4 => Key::ArrowLeft,
        0xB5 => Key::ArrowUp,
        0xB6 => Key::ArrowDown,
        0xB7 => Key::ArrowRight,
        _ => Key::Ascii(byte),
    }
}
