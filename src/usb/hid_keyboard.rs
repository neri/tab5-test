//! HID Boot Protocol keyboard class driver (USB HID 1.11), built on top of
//! `hid.rs`'s shared Boot Protocol plumbing -- which is itself built on
//! `protocol.rs`'s generic control transfers and `hcd.rs`'s raw
//! channel/packet primitive.
//!
//! What is left here is only what is specific to a keyboard: naming the
//! keyboard boot interface, diffing successive reports to find newly
//! pressed keys, and translating HID usage IDs into `input::Key`.
//!
//! This is Stage 3 of `docs/USB_HOST_PLAN.md`, the actual milestone: `UsbKeyboard`
//! feeds decoded keystrokes into the same `Console::push` path `cardkb.rs`
//! uses, polled by `input::InputManager` alongside `CardKb`.

use super::hid::{self, InterruptIn};
use super::protocol::EnumeratedDevice;
use crate::input::Key;

// HID Boot Protocol keyboard report (HID 1.11 appendix B.1): modifier byte,
// one reserved byte, then up to 6 simultaneous keycodes.
const REPORT_BYTES: usize = 8;
const MODIFIER_LEFT_SHIFT: u8 = 1 << 1;
const MODIFIER_RIGHT_SHIFT: u8 = 1 << 5;
const KEYCODE_ROLLOVER_ERROR: u8 = 0x01;

pub struct UsbKeyboard {
    endpoint: InterruptIn,
    previous_keys: [u8; 6],
    pending: [Key; 6],
    pending_len: u8,
    pending_pos: u8,
}

impl UsbKeyboard {
    /// Takes a device that `protocol::enumerate_device` has already
    /// addressed and, if it has a HID Boot Protocol keyboard interface,
    /// puts that interface into Boot Protocol (see
    /// `hid::configure_boot_protocol`) before returning a handle ready for
    /// `poll`.
    ///
    /// Where the device sits -- USB-A directly or a hub port -- is already
    /// baked into `device`'s address and route, so this is the same code
    /// either way; `registry::UsbHost` is what decides which one to
    /// enumerate.
    pub fn attach(device: &EnumeratedDevice) -> Option<Self> {
        let interface =
            hid::find_boot_interface(device.config_bytes(), hid::INTERFACE_PROTOCOL_KEYBOARD)?;
        if !hid::configure_boot_protocol(device, interface.interface_number) {
            return None;
        }
        Some(Self {
            endpoint: InterruptIn::new(device.device_address, device.route, &interface),
            previous_keys: [0; 6],
            pending: [Key::Ascii(0); 6],
            pending_len: 0,
            pending_pos: 0,
        })
    }

    /// True once this keyboard's polling session has gone stale; see
    /// `hid::InterruptIn::needs_reinit`.
    pub fn needs_reinit(&self) -> bool {
        self.endpoint.needs_reinit()
    }

    pub fn probe_periodic(&mut self) -> super::hcd::PeriodicProbeResult {
        self.endpoint.probe_periodic()
    }

    pub fn enable_periodic(&mut self) -> Option<u8> {
        self.endpoint.enable_periodic()
    }

    /// Returns the next newly-pressed key, or `None` if nothing new is
    /// available this frame. ASCII and HID-only keys (Esc, arrows, and so
    /// on) use the same `input::Key` contract that `InputManager` exposes.
    ///
    /// A single Boot report can carry up to 6 simultaneously-pressed keys;
    /// since this returns one key at a time, newly-pressed keys from one
    /// report are queued and drained a key per call (at ~57 polls/sec,
    /// the queue is essentially never more than a key or two deep).
    pub fn poll(&mut self) -> Option<Key> {
        if let Some(key) = self.next_pending() {
            return Some(key);
        }

        let mut report = [0u8; REPORT_BYTES];
        let transferred = self.endpoint.read_report(&mut report)?;
        if transferred < REPORT_BYTES {
            return None; // no real report this frame (or a malformed one)
        }

        self.queue_new_keys(&report);
        self.next_pending()
    }

    fn next_pending(&mut self) -> Option<Key> {
        if self.pending_pos < self.pending_len {
            let key = self.pending[self.pending_pos as usize];
            self.pending_pos += 1;
            Some(key)
        } else {
            None
        }
    }

    /// Compares `report`'s keycodes against the last report to find newly
    /// pressed keys (held-down keys are not repeated every poll) and queues
    /// their normalized translations.
    fn queue_new_keys(&mut self, report: &[u8; REPORT_BYTES]) {
        let modifiers = report[0];
        let keys: [u8; 6] = report[2..8].try_into().unwrap();
        if keys == [KEYCODE_ROLLOVER_ERROR; 6] {
            // Phantom state (HID 1.11 appendix B.1): more keys are pressed
            // than the report can represent. Nothing reliable to queue,
            // and `previous_keys` is deliberately left alone so the next
            // real report is compared against the last real baseline
            // rather than this phantom one.
            return;
        }
        let shift = modifiers & (MODIFIER_LEFT_SHIFT | MODIFIER_RIGHT_SHIFT) != 0;

        self.pending_len = 0;
        self.pending_pos = 0;
        for &keycode in &keys {
            if keycode == 0 || self.previous_keys.contains(&keycode) {
                continue; // empty slot, or already held from the last report
            }
            if let Some(key) = translate_keycode(keycode, shift)
                && (self.pending_len as usize) < self.pending.len()
            {
                self.pending[self.pending_len as usize] = key;
                self.pending_len += 1;
            }
        }
        self.previous_keys = keys;
    }
}

/// Translates one HID Keyboard/Keypad usage ID (HID Usage Tables 1.4,
/// page 0x07) into the application-wide key representation. Covers ASCII,
/// editing/navigation keys, and F1 through F12. Modifier keys by themselves
/// still have no event; their Shift state modifies printable ASCII only.
fn translate_keycode(keycode: u8, shift: bool) -> Option<Key> {
    match keycode {
        0x04..=0x1D => {
            let letter = b'a' + (keycode - 0x04);
            Some(Key::Ascii(if shift {
                letter.to_ascii_uppercase()
            } else {
                letter
            }))
        }
        0x1E..=0x27 => {
            const UNSHIFTED: &[u8; 10] = b"1234567890";
            const SHIFTED: &[u8; 10] = b"!@#$%^&*()";
            let index = (keycode - 0x1E) as usize;
            Some(Key::Ascii(if shift {
                SHIFTED[index]
            } else {
                UNSHIFTED[index]
            }))
        }
        0x28 => Some(Key::Ascii(b'\r')), // Enter
        0x29 => Some(Key::Escape),
        0x2A => Some(Key::Ascii(0x08)),  // Backspace
        0x2B => Some(Key::Ascii(b'\t')), // Tab
        0x2C => Some(Key::Ascii(b' ')),  // Space
        0x2D => Some(Key::Ascii(if shift { b'_' } else { b'-' })),
        0x2E => Some(Key::Ascii(if shift { b'+' } else { b'=' })),
        0x2F => Some(Key::Ascii(if shift { b'{' } else { b'[' })),
        0x30 => Some(Key::Ascii(if shift { b'}' } else { b']' })),
        0x31 => Some(Key::Ascii(if shift { b'|' } else { b'\\' })),
        0x33 => Some(Key::Ascii(if shift { b':' } else { b';' })),
        0x34 => Some(Key::Ascii(if shift { b'"' } else { b'\'' })),
        0x35 => Some(Key::Ascii(if shift { b'~' } else { b'`' })),
        0x36 => Some(Key::Ascii(if shift { b'<' } else { b',' })),
        0x37 => Some(Key::Ascii(if shift { b'>' } else { b'.' })),
        0x38 => Some(Key::Ascii(if shift { b'?' } else { b'/' })),
        0x3A..=0x45 => Some(Key::Function(keycode - 0x39)),
        0x49 => Some(Key::Insert),
        0x4A => Some(Key::Home),
        0x4B => Some(Key::PageUp),
        0x4C => Some(Key::Delete),
        0x4D => Some(Key::End),
        0x4E => Some(Key::PageDown),
        0x4F => Some(Key::ArrowRight),
        0x50 => Some(Key::ArrowLeft),
        0x51 => Some(Key::ArrowDown),
        0x52 => Some(Key::ArrowUp),
        _ => None,
    }
}
