//! HID Boot Protocol keyboard class driver (USB HID 1.11), built on top of
//! `protocol.rs`'s generic control transfers and `hcd.rs`'s raw
//! channel/packet primitive (used directly for Interrupt IN polling, which
//! is not a control transfer and so does not go through `protocol.rs` at
//! all).
//!
//! This is Stage 3 of `docs/USB_HOST_PLAN.md`, the actual milestone: `UsbKeyboard`
//! feeds decoded keystrokes into the same `Console::push` path `cardkb.rs`
//! uses, polled by `input::InputManager` alongside `CardKb`.

use super::hcd::{self, Endpoint, HCCHAR_EPTYPE_BULK, PacketOutcome};
use super::protocol::{self, EnumeratedDevice, REQUEST_SET_CONFIGURATION};
use crate::input::Key;
use crate::uart;

// HID class-specific requests (HID 1.11 section 7.2), used only during
// `UsbKeyboard::init`.
const REQUEST_HID_SET_IDLE: u8 = 0x0A;
const REQUEST_HID_SET_PROTOCOL: u8 = 0x0B;
const HID_PROTOCOL_BOOT: u8 = 0;
const REQUEST_TYPE_HOST_TO_DEVICE_INTERFACE: u8 = 0x21; // class, interface recipient

// HID Boot Protocol keyboard interface (USB HID 1.11 section 4.2).
const INTERFACE_CLASS_HID: u8 = 3;
const INTERFACE_SUBCLASS_BOOT: u8 = 1;
const INTERFACE_PROTOCOL_KEYBOARD: u8 = 1;

// HID Boot Protocol keyboard report (HID 1.11 appendix B.1): modifier byte,
// one reserved byte, then up to 6 simultaneous keycodes.
const MODIFIER_LEFT_SHIFT: u8 = 1 << 1;
const MODIFIER_RIGHT_SHIFT: u8 = 1 << 5;
const KEYCODE_ROLLOVER_ERROR: u8 = 0x01;

// Interrupt polling runs once per rendered frame (~57 Hz), and a NAK (no
// new report yet) retries in hardware exactly like a control transfer's
// NAK does -- there is no separate "NAK interrupt" to short-circuit on.
// This has to be long enough to let at least one real transaction
// (NAK-retry-then-eventually-something, or immediate success) resolve,
// while staying a small fraction of the ~17.5ms frame budget even if it
// fires on every single idle frame; its timeout stays silent by default
// (see `hcd::run_packet`'s `quiet_timeout`) or the UART would be spammed
// dozens of times a second.
const INTERRUPT_POLL_TIMEOUT_ITERATIONS: u32 = 50_000;

/// How many SSPLIT/CSPLIT round trips one poll of a keyboard behind a
/// High-Speed hub's TT may take (`hcd::run_packet`'s `max_split_rounds`).
///
/// One attempt per poll. `hcd::run_packet`'s budget is a soft one -- it
/// stops at the first *safe* boundary at or after this many rounds, never
/// mid-handshake -- so 1 means "run the split through to its first NAK and
/// stop there", which is exactly one question asked of the keyboard.
///
/// That is the right shape for per-frame polling: an idle keyboard NAKs
/// (`SET_IDLE(0)` means it reports only on change), and the frame loop is
/// back in ~17.5ms to ask again. Retrying harder inside one frame would
/// spend the display's budget waiting on a device that has already said it
/// has nothing.
const INTERRUPT_POLL_SPLIT_ROUNDS: u32 = 1;
// Consecutive *hard* transaction errors (STALL/XACTERR/BBLERR/
// XCS_XACT_ERR; see `PacketOutcome::Error`), not plain NAK timeouts --
// those are routine while idle and never count here. A correctly-addressed
// device does not produce hard errors at all, so this only fires once the
// session itself has gone stale (typically: something else ran
// `hcd::probe_port` and reset the bus out from under an active
// `UsbKeyboard`). A small threshold is enough since real errors, unlike
// timeouts, resolve quickly. See `UsbKeyboard::needs_reinit`.
const POLL_FAILURE_GIVE_UP_THRESHOLD: u32 = 10;

pub struct HidKeyboardInterface {
    pub interface_number: u8,
    pub endpoint_address: u8,
    pub max_packet_size: u16,
}

/// Walks a configuration descriptor's interface/endpoint chain (see
/// `protocol::EnumeratedDevice::config_bytes`) looking for a HID Boot
/// Protocol keyboard interface's Interrupt IN endpoint. Ignores every
/// other interface (mice, extra composite functions); a HID mouse/other
/// class driver would need its own version of this walk.
pub fn find_hid_keyboard(config: &[u8]) -> Option<HidKeyboardInterface> {
    let mut offset = 0usize;
    while offset + 2 <= config.len() {
        let length = config[offset] as usize;
        if length < 2 || offset + length > config.len() {
            break;
        }
        let descriptor_type = config[offset + 1];
        if descriptor_type == protocol::DESCRIPTOR_TYPE_INTERFACE && length >= 9 {
            let interface_class = config[offset + 5];
            let interface_subclass = config[offset + 6];
            let interface_protocol = config[offset + 7];
            let is_target_interface = interface_class == INTERFACE_CLASS_HID
                && interface_subclass == INTERFACE_SUBCLASS_BOOT
                && interface_protocol == INTERFACE_PROTOCOL_KEYBOARD;
            if is_target_interface {
                let interface_number = config[offset + 2];
                // Keep scanning forward from here for this interface's
                // Interrupt IN endpoint (HID/other class-specific
                // descriptors come first, endpoints after).
                let mut endpoint_offset = offset + length;
                while endpoint_offset + 2 <= config.len() {
                    let endpoint_length = config[endpoint_offset] as usize;
                    if endpoint_length < 2 || endpoint_offset + endpoint_length > config.len() {
                        break;
                    }
                    let endpoint_descriptor_type = config[endpoint_offset + 1];
                    if endpoint_descriptor_type == protocol::DESCRIPTOR_TYPE_INTERFACE {
                        break; // next interface started; this one had no usable endpoint
                    }
                    if endpoint_descriptor_type == protocol::DESCRIPTOR_TYPE_ENDPOINT && endpoint_length >= 7 {
                        let endpoint_address = config[endpoint_offset + 2];
                        let attributes = config[endpoint_offset + 3];
                        let is_interrupt = attributes & 0x03 == 3;
                        let is_in = endpoint_address & 0x80 != 0;
                        if is_interrupt && is_in {
                            return Some(HidKeyboardInterface {
                                interface_number,
                                endpoint_address,
                                max_packet_size: u16::from_le_bytes([
                                    config[endpoint_offset + 4],
                                    config[endpoint_offset + 5],
                                ]),
                            });
                        }
                    }
                    endpoint_offset += endpoint_length;
                }
            }
        }
        offset += length;
    }
    None
}

pub struct UsbKeyboard {
    /// The Interrupt IN endpoint this polls, including the device address
    /// and speed it was enumerated with -- which is all `poll` needs to
    /// keep talking to it, whether it is plugged into USB-A directly or
    /// into a hub port.
    endpoint: Endpoint,
    next_pid_data1: bool,
    previous_keys: [u8; 6],
    pending: [Key; 6],
    pending_len: u8,
    pending_pos: u8,
    // Consecutive hard errors from `poll`; drives `needs_reinit` and also
    // whether `poll` asks `hcd::run_packet` to stay quiet about repeats of
    // an already-logged error.
    consecutive_hard_errors: u32,
}

impl UsbKeyboard {
    /// Takes a device that `protocol::enumerate_device` has already
    /// addressed and, if it has a HID Boot Protocol keyboard interface,
    /// activates its configuration, switches it to Boot Protocol, and
    /// disables its idle auto-repeat (report only on change) before
    /// returning a handle ready for `poll`.
    ///
    /// Where the device sits -- USB-A directly or a hub port -- is already
    /// baked into `device`'s address and speed, so this is the same code
    /// either way; `usb::connect_keyboard` is what decides which one to
    /// enumerate.
    pub fn attach(device: &EnumeratedDevice) -> Option<Self> {
        let hid = find_hid_keyboard(device.config_bytes())?;
        let pipe = device.control_pipe();

        let setup =
            protocol::build_standard_out_setup(REQUEST_SET_CONFIGURATION, device.configuration_value as u16, 0);
        if !protocol::control_transfer_out_no_data(&pipe, &setup) {
            uart::log(b"USB: SET_CONFIGURATION failed\r\n");
            return None;
        }

        let setup = build_hid_out_setup(
            REQUEST_HID_SET_PROTOCOL,
            HID_PROTOCOL_BOOT as u16,
            hid.interface_number,
        );
        if !protocol::control_transfer_out_no_data(&pipe, &setup) {
            uart::log(b"USB: HID SET_PROTOCOL(Boot) failed\r\n");
            return None;
        }

        // wValue = (Duration << 8) | ReportID; Duration 0 = report only on
        // change, no idle auto-repeat.
        let setup = build_hid_out_setup(REQUEST_HID_SET_IDLE, 0, hid.interface_number);
        if !protocol::control_transfer_out_no_data(&pipe, &setup) {
            uart::log(b"USB: HID SET_IDLE failed\r\n");
            return None;
        }

        Some(Self {
            endpoint: Endpoint {
                device_address: device.device_address,
                endpoint_number: hid.endpoint_address & 0x0F,
                endpoint_type: HCCHAR_EPTYPE_BULK,
                mps: hid.max_packet_size,
                is_in: true,
                route: device.route,
            },
            // Every endpoint's data toggle resets to DATA0 on
            // SET_CONFIGURATION (USB2.0 9.4.5).
            next_pid_data1: false,
            previous_keys: [0; 6],
            pending: [Key::Ascii(0); 6],
            pending_len: 0,
            pending_pos: 0,
            consecutive_hard_errors: 0,
        })
    }

    /// True once polling has failed for long enough that the session
    /// itself -- not just "no key pressed yet" -- is almost certainly
    /// stale and worth re-enumerating from scratch.
    ///
    /// A physical unplug is caught separately and more cheaply, at the
    /// registry level: `usb::registry::UsbHost::root_disconnected` reads
    /// the root port's connect bit once for the whole bus rather than
    /// asking each keyboard individually (and, for a keyboard behind a
    /// hub, that root-port bit does not even reflect *this* device's own
    /// hub port). What `needs_reinit` catches instead is this handle's
    /// cached address/configuration going stale while the device is still
    /// physically present -- normally only `UsbHost::rescan` can cause
    /// that now (`docs/USB_REFACTOR_PLAN.md` Stage A made it the sole owner of
    /// `hcd::probe_port`), so in practice this now means a genuine
    /// transaction error rather than another command resetting the bus
    /// out from under an active session. `InputManager` checks this and calls
    /// `UsbHost::rescan` once it's true, which re-enumerates every still
    /// physically present device and recovers within a few frames.
    pub fn needs_reinit(&self) -> bool {
        self.consecutive_hard_errors >= POLL_FAILURE_GIVE_UP_THRESHOLD
    }

    /// Returns the next newly-pressed key, or `None` if nothing new is
    /// available this frame. ASCII and HID-only keys (Esc, arrows, and so
    /// on) use the same `input::Key` contract that `InputManager` exposes.
    ///
    /// A single Boot report can carry up to 6 simultaneously-pressed keys;
    /// since this returns one key at a time, newly-pressed keys from one
    /// report are queued and drained a byte per call (at ~57 polls/sec,
    /// the queue is essentially never more than a key or two deep).
    pub fn poll(&mut self) -> Option<Key> {
        if let Some(key) = self.next_pending() {
            return Some(key);
        }

        let mut report = [0u8; 8];
        // Once a streak of hard errors has already logged its first
        // occurrence, every repeat is diagnosing the exact same stale
        // session (see `hcd::run_packet`'s `quiet_errors` doc) -- log
        // once, not ~`POLL_FAILURE_GIVE_UP_THRESHOLD` times before
        // `needs_reinit` finally gives up.
        let quiet_errors = self.consecutive_hard_errors > 0;
        let outcome = hcd::run_packet(
            &self.endpoint,
            false,
            self.next_pid_data1,
            INTERRUPT_POLL_TIMEOUT_ITERATIONS,
            INTERRUPT_POLL_SPLIT_ROUNDS,
            true,
            quiet_errors,
            &mut report,
        );
        let transferred = match outcome {
            PacketOutcome::Ok(n) => {
                self.consecutive_hard_errors = 0;
                n
            }
            PacketOutcome::Timeout => {
                // Routine while idle: `SET_IDLE(0)` means the device NAKs
                // (silently, in `hcd::run_packet`) until something
                // actually changes. Not a sign the session is stale, so it
                // does not count toward `needs_reinit`.
                self.consecutive_hard_errors = 0;
                return None;
            }
            PacketOutcome::Error => {
                // `hcd::run_packet` already logged the specific HCINT/QTD
                // status (unless this streak already has). A real
                // transaction error (as opposed to a NAK timeout) usually
                // means this handle's cached address/configuration went
                // stale -- most commonly because something else ran
                // `hcd::probe_port` and reset the bus out from under it.
                // `needs_reinit` surfaces this so `InputManager` can drop and
                // re-enumerate instead of erroring forever.
                self.consecutive_hard_errors = self.consecutive_hard_errors.wrapping_add(1);
                return None;
            }
        };
        // Any successful completion consumed this PID per USB2.0's toggle
        // rule, even if the report body ends up unusable below.
        self.next_pid_data1 = !self.next_pid_data1;
        if transferred < 8 {
            return None; // no real report this frame (or a malformed one)
        }

        self.queue_new_keys(&report);
        self.next_pending()
    }

    fn next_pending(&mut self) -> Option<Key> {
        if self.pending_pos < self.pending_len {
            let byte = self.pending[self.pending_pos as usize];
            self.pending_pos += 1;
            Some(byte)
        } else {
            None
        }
    }

    /// Compares `report`'s keycodes against the last report to find newly
    /// pressed keys (held-down keys are not repeated every poll) and queues
    /// their normalized translations.
    fn queue_new_keys(&mut self, report: &[u8; 8]) {
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
            Some(Key::Ascii(if shift { letter.to_ascii_uppercase() } else { letter }))
        }
        0x1E..=0x27 => {
            const UNSHIFTED: &[u8; 10] = b"1234567890";
            const SHIFTED: &[u8; 10] = b"!@#$%^&*()";
            let index = (keycode - 0x1E) as usize;
            Some(Key::Ascii(if shift { SHIFTED[index] } else { UNSHIFTED[index] }))
        }
        0x28 => Some(Key::Ascii(b'\r')), // Enter
        0x29 => Some(Key::Escape),
        0x2A => Some(Key::Ascii(0x08)), // Backspace
        0x2B => Some(Key::Ascii(b'\t')), // Tab
        0x2C => Some(Key::Ascii(b' ')), // Space
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

/// Builds a HID class-specific (bmRequestType 0x21, interface recipient)
/// host-to-device setup packet with no data stage, e.g. `SET_PROTOCOL` or
/// `SET_IDLE`.
fn build_hid_out_setup(request: u8, value: u16, interface_number: u8) -> [u8; 8] {
    [
        REQUEST_TYPE_HOST_TO_DEVICE_INTERFACE,
        request,
        (value & 0xFF) as u8,
        (value >> 8) as u8,
        interface_number,
        0,
        0,
        0,
    ]
}
