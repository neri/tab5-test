//! Unified keyboard, pointer, and touch input-source lifecycle management.
//!
//! This module is the application-level owner of all sources that can emit
//! console keys.  It deliberately does not replace `usb::UsbHost`: that type
//! remains the sole owner of USB-A enumeration, hub state, and non-keyboard
//! USB devices such as Mass Storage.
//!
//! USB pointer input is forwarded rather than normalized the way keys are.  A
//! key is meaningful on its own, so `Key` hides which keyboard produced it;
//! mouse motion is relative and only becomes a position once something
//! decides what it is moving across, so `poll_mouse` hands `usb::MouseUpdate`
//! straight through and leaves the cursor position to the screen that draws
//! one (`app::win`). Touch is different: it reports absolute framebuffer
//! coordinates, so this module owns the controller lifecycle and exposes
//! contacts and a single-contact stream without exposing its I2C driver.

use crate::cardkb::CardKb;
use crate::tab5_keyboard::Tab5Keyboard;
use crate::touch::{Touch, TouchPoint as DriverTouchPoint};
use crate::{interrupts, uart, usb};

const CARDKB_RECONNECT_FRAMES: u32 = 60;
const TAB5_KEYBOARD_RECONNECT_FRAMES: u32 = 60;
const TAB5_KEYBOARD_HEALTH_CHECK_FRAMES: u32 = 60;
const TOUCH_RECONNECT_FRAMES: u32 = 60;
const HUB_PORT_SCAN_FRAMES: u32 = 60;
const ROOT_RESCAN_FRAMES: u32 = 300;
const MAX_TOUCH_POINTS: usize = 10;

/// A normalized key understood by application-level input consumers.
///
/// Text and non-text keys share one fixed-size representation, so input
/// consumers never need to know whether a key came from CardKB, the dedicated
/// Tab5 Keyboard, or USB HID.
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
    Tab5Keyboard,
    Usb,
}

/// One normalized keyboard event.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct KeyEvent {
    pub source: KeySource,
    pub key: Key,
}

/// One active touch contact in framebuffer logical (landscape) coordinates.
///
/// Contact identity belongs to the controller driver: callers receive just
/// the stable display-space coordinates needed for drawing and hit testing.
#[derive(Clone, Copy)]
pub struct TouchPoint {
    pub x: usize,
    pub y: usize,
}

/// The first finger in a touch sequence, represented as pointer-like phases.
///
/// Once a `Pressed` event is returned, contacts added later are deliberately
/// ignored until that original contact goes away. This lets a consumer use a
/// touch drag like a left-button mouse drag without accidentally switching to
/// another finger.
#[derive(Clone, Copy)]
pub enum PrimaryTouch {
    Idle,
    Pressed(TouchPoint),
    Moved(TouchPoint),
    Released,
}

/// Owns every keyboard input source and maintains their connection state.
///
/// `service` performs periodic connection maintenance and `poll_key` only
/// reads keys.  Call both once at each display-frame boundary.
pub struct InputManager {
    cardkb: Option<CardKb>,
    cardkb_reconnect_frames: u32,
    tab5_keyboard: Option<Tab5Keyboard>,
    tab5_keyboard_reconnect_frames: u32,
    tab5_keyboard_health_check_frames: u32,
    touch: Option<Touch>,
    touch_reconnect_frames: u32,
    primary_touch_id: Option<u8>,
    usb_host: usb::UsbHost,
    usb_reconnect_frames: u32,
    next_source: KeySource,
}

impl InputManager {
    /// Initializes both I2C keyboards and the USB-A host.
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

        let tab5_keyboard = if crate::i2c::initialize_tab5_keyboard_bus().is_ok() {
            Tab5Keyboard::init()
        } else {
            None
        };
        if tab5_keyboard.is_some() {
            uart::log(b"Tab5 Keyboard: ready\r\n");
        } else {
            uart::log(b"Tab5 Keyboard: absent\r\n");
        }

        let touch = Touch::init();
        if let Some(panel) = touch.as_ref() {
            uart::log(b"Touch: ready (");
            uart::log(panel.controller_name().as_bytes());
            uart::log(b")\r\n");
        } else {
            uart::log(b"Touch: absent\r\n");
        }

        let mut usb_host = usb::UsbHost::new();
        usb_host.rescan();
        uart::log(b"USB: initial scan complete\r\n");

        Self {
            cardkb,
            cardkb_reconnect_frames: 0,
            tab5_keyboard,
            tab5_keyboard_reconnect_frames: 0,
            tab5_keyboard_health_check_frames: 0,
            touch,
            touch_reconnect_frames: 0,
            primary_touch_id: None,
            usb_host,
            usb_reconnect_frames: 0,
            next_source: KeySource::CardKb,
        }
    }

    /// Advances I2C-keyboard reconnection and USB device-discovery state.
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

        if self.tab5_keyboard.is_none() {
            self.tab5_keyboard_reconnect_frames += 1;
            if self.tab5_keyboard_reconnect_frames == TAB5_KEYBOARD_RECONNECT_FRAMES {
                self.tab5_keyboard_reconnect_frames = 0;
                self.tab5_keyboard = Tab5Keyboard::init();
                if self.tab5_keyboard.is_some() {
                    uart::log(b"Tab5 Keyboard: connected\r\n");
                }
            }
        } else {
            self.tab5_keyboard_health_check_frames += 1;
            if self.tab5_keyboard_health_check_frames == TAB5_KEYBOARD_HEALTH_CHECK_FRAMES {
                self.tab5_keyboard_health_check_frames = 0;
                let result = self
                    .tab5_keyboard
                    .as_mut()
                    .map(Tab5Keyboard::ensure_hid_mode);
                if matches!(result, Some(Err(_))) {
                    self.tab5_keyboard = None;
                    self.tab5_keyboard_reconnect_frames = 0;
                    uart::log(b"Tab5 Keyboard: disconnected\r\n");
                }
            }
        }

        if self.touch.is_none() {
            self.touch_reconnect_frames += 1;
            if self.touch_reconnect_frames == TOUCH_RECONNECT_FRAMES {
                self.touch_reconnect_frames = 0;
                self.touch = Touch::init();
                if let Some(panel) = self.touch.as_ref() {
                    uart::log(b"Touch: connected (");
                    uart::log(panel.controller_name().as_bytes());
                    uart::log(b")\r\n");
                }
            }
        }

        // The ISR supplies the connection edge; HPRT's current status remains
        // the authoritative disconnect check. Rescans caused by a real insert
        // happen immediately, while the coarse fallback below remains for a
        // missed interrupt or a device already present before IRQ setup.
        let root_connection_changed = self.usb_host.take_root_connection_change();
        if self.usb_host.root_disconnected() {
            let had_registered_device = !self.usb_host.is_empty();
            self.usb_host.clear();
            self.usb_reconnect_frames = 0;
            if had_registered_device {
                uart::log(b"USB: nothing connected to USB-A\r\n");
            }
        } else if root_connection_changed {
            uart::log(b"USB: root-port connection changed, rescanning...\r\n");
            self.usb_host.rescan();
            self.usb_reconnect_frames = 0;
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

    /// Returns at most one key, rotating the source checked first after every
    /// delivered key so a continuously active source cannot starve the others.
    pub fn poll_key(&mut self) -> Option<KeyEvent> {
        let first = self.next_source;
        let second = source_after(first);
        for source in [first, second, source_after(second)] {
            let key = match source {
                KeySource::CardKb => self
                    .cardkb
                    .as_mut()
                    .and_then(CardKb::poll)
                    .map(key_from_ascii),
                KeySource::Tab5Keyboard => {
                    let result = self.tab5_keyboard.as_mut().map(Tab5Keyboard::poll);
                    match result {
                        Some(Ok(key)) => key,
                        Some(Err(_)) => {
                            self.tab5_keyboard = None;
                            self.tab5_keyboard_reconnect_frames = 0;
                            self.tab5_keyboard_health_check_frames = 0;
                            uart::log(b"Tab5 Keyboard: disconnected\r\n");
                            None
                        }
                        None => None,
                    }
                }
                KeySource::Usb => self.usb_host.poll_keyboards(),
            };
            if let Some(key) = key {
                self.next_source = source_after(source);
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

    /// Short controller name for a touch diagnostic, if a panel is present.
    pub fn touch_controller_name(&self) -> Option<&'static str> {
        self.touch.as_ref().map(Touch::controller_name)
    }

    /// Number of contacts the active touch controller is configured to report.
    pub fn touch_max_points(&self) -> Option<usize> {
        self.touch.as_ref().map(Touch::max_touches)
    }

    /// Reads all currently active touch contacts into `points`.
    ///
    /// The hardware-specific tracking identifier stays inside the driver. Use
    /// this for multi-touch views such as `touchtest`; consumers that need a
    /// one-finger pointer gesture should use `poll_primary_touch` instead.
    pub fn poll_touch_points(&mut self, points: &mut [TouchPoint]) -> usize {
        let mut driver_points = [DriverTouchPoint::EMPTY; MAX_TOUCH_POINTS];
        let Some(touch) = self.touch.as_ref() else {
            return 0;
        };
        let count = touch.poll_points(&mut driver_points).min(points.len());
        for index in 0..count {
            points[index] = TouchPoint {
                x: driver_points[index].x,
                y: driver_points[index].y,
            };
        }
        count
    }

    /// Converts the first contact of a touch sequence into pointer phases.
    ///
    /// Contact IDs (GT911) or fixed report slots (ST7121/ST7123) keep the
    /// original finger selected. A second finger never replaces it; when the
    /// selected finger lifts this returns `Released` even if other fingers
    /// remain down.
    pub fn poll_primary_touch(&mut self) -> PrimaryTouch {
        let mut points = [DriverTouchPoint::EMPTY; MAX_TOUCH_POINTS];
        let count = self
            .touch
            .as_ref()
            .map(|touch| touch.poll_points(&mut points))
            .unwrap_or(0);

        if let Some(id) = self.primary_touch_id {
            if let Some(point) = points[..count].iter().find(|point| point.id == id) {
                return PrimaryTouch::Moved(TouchPoint {
                    x: point.x,
                    y: point.y,
                });
            }
            self.primary_touch_id = None;
            return PrimaryTouch::Released;
        }

        let Some(point) = points.first().filter(|_| count != 0) else {
            return PrimaryTouch::Idle;
        };
        self.primary_touch_id = Some(point.id);
        PrimaryTouch::Pressed(TouchPoint {
            x: point.x,
            y: point.y,
        })
    }

    /// Discards a saved primary-contact selection before starting a new
    /// pointer gesture consumer. The next active contact becomes `Pressed`.
    pub fn reset_primary_touch(&mut self) {
        self.primary_touch_id = None;
    }

    /// Mutable USB bus registry for commands such as `usbrescan` and MSC I/O.
    pub fn usb_host_mut(&mut self) -> &mut usb::UsbHost {
        &mut self.usb_host
    }
}

const fn source_after(source: KeySource) -> KeySource {
    match source {
        KeySource::CardKb => KeySource::Tab5Keyboard,
        KeySource::Tab5Keyboard => KeySource::Usb,
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

/// Translates an HID Keyboard/Keypad usage ID and modifier byte into the
/// application-wide key representation.  USB HID and the Tab5 Keyboard use
/// this one conversion so their printable and navigation keys stay identical.
pub(crate) fn key_from_hid_usage(keycode: u8, modifiers: u8) -> Option<Key> {
    let shift = modifiers & ((1 << 1) | (1 << 5)) != 0;
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
        0x28 => Some(Key::Ascii(b'\r')),
        0x29 => Some(Key::Escape),
        0x2A => Some(Key::Ascii(0x08)),
        0x2B => Some(Key::Ascii(b'\t')),
        0x2C => Some(Key::Ascii(b' ')),
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
