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
use crate::{interrupts, tick, uart, usb};

const CARDKB_RECONNECT_FRAMES: u32 = 60;
const TAB5_KEYBOARD_RECONNECT_FRAMES: u32 = 60;
const TAB5_KEYBOARD_HEALTH_CHECK_FRAMES: u32 = 60;
const TOUCH_RECONNECT_FRAMES: u32 = 60;
const HUB_PORT_SCAN_FRAMES: u32 = 60;
const ROOT_RESCAN_FRAMES: u32 = 300;
const PENDING_KEY_EVENTS: usize = 16;
/// Frame gap enforced between two rescans caused by a stale device session,
/// multiplied by how many have happened in a row.
///
/// A rescan resets the whole bus. A device that fails immediately after
/// being attached therefore produces a loop -- attach, fail, reset, attach
/// -- that takes every *other* device on the bus down with it several times
/// a second, which is how one misbehaving keyboard makes mass storage
/// unusable. Backing off leaves the working devices alone between attempts.
const STALE_RESCAN_BACKOFF_FRAMES: u32 = 60;
/// Upper bound on that gap, about ten seconds at the panel's 57.3 Hz.
const STALE_RESCAN_BACKOFF_MAX_FRAMES: u32 = 600;
/// Frames a session must survive before the backoff is considered recovered.
const STALE_RESCAN_SETTLED_FRAMES: u32 = 600;
const MAX_TOUCH_POINTS: usize = 10;

/// A normalized key understood by application-level input consumers.
///
/// Text and non-text keys share one fixed-size representation, so input
/// consumers never need to know whether a key came from CardKB, the dedicated
/// Tab5 Keyboard, or USB HID.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Key {
    Ascii(u8),
    /// A letter held with Ctrl, as the lowercase letter itself.
    ///
    /// A variant rather than the C0 control code the letter stands for
    /// (`Ctrl+Q` as `Ascii(0x11)`). The control codes are not free: three of
    /// them are keys of their own -- `Ctrl+H` is 0x08, `Ctrl+I` is 0x09,
    /// `Ctrl+M` is 0x0D -- so an application binding one of those to a
    /// command would silently bind Backspace, Tab or Enter with it. Keeping
    /// them apart means the aliasing is decided once, where the bytes are
    /// translated, instead of in every `match` on a key.
    Control(u8),
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
    primary_touch_blocked: bool,
    primary_touch_id: Option<u8>,
    usb_host: usb::UsbHost,
    usb_reconnect_frames: u32,
    /// Frames since the last sweep for devices removed from hub ports. Kept
    /// apart from `usb_reconnect_frames` because that one is reset by
    /// discovery and by reconnect backoff, and a removal sweep that only ran
    /// when discovery happened to be idle would be exactly as blind as
    /// having no sweep.
    usb_port_sweep_frames: u32,
    /// Rescans caused by a stale session with no settled interval between
    /// them, and the frames still to wait before the next one.
    usb_stale_rescans: u32,
    usb_stale_backoff_frames: u32,
    usb_frames_since_stale: u32,
    next_source: KeySource,
    /// Split HID is serviced from the 1kHz tick between display frames. Its
    /// events wait here until the application consumes them at its normal
    /// frame boundary.
    pending_key_events: [Option<KeyEvent>; PENDING_KEY_EVENTS],
    pending_key_head: usize,
    pending_key_len: usize,
    usb_split_poll_due_ms: u64,
}

impl InputManager {
    /// Initializes both I2C keyboards and an empty USB-A host registry.
    ///
    /// The startup screen owns the first USB scan so it can show progress and
    /// remain cancellable. Steady-state scans remain in [`Self::service`].
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

        let usb_host = usb::UsbHost::new();
        uart::log(b"USB ENUM: bounded retry v9\r\n");
        uart::log(b"USB STABILITY: fault-rescan retry v42\r\n");

        Self {
            cardkb,
            cardkb_reconnect_frames: 0,
            tab5_keyboard,
            tab5_keyboard_reconnect_frames: 0,
            tab5_keyboard_health_check_frames: 0,
            touch,
            touch_reconnect_frames: 0,
            primary_touch_blocked: false,
            primary_touch_id: None,
            usb_host,
            usb_reconnect_frames: 0,
            usb_port_sweep_frames: 0,
            usb_stale_rescans: 0,
            usb_stale_backoff_frames: 0,
            usb_frames_since_stale: 0,
            next_source: KeySource::CardKb,
            pending_key_events: [None; PENDING_KEY_EVENTS],
            pending_key_head: 0,
            pending_key_len: 0,
            usb_split_poll_due_ms: tick::now_ms(),
        }
    }

    /// Services serialized Split keyboards at their descriptor's
    /// `bInterval`, independently of the panel's ~57Hz frame boundary.
    ///
    /// The 1kHz SYSTIMER already wakes foreground `wfi` loops. Callers invoke
    /// this only for non-frame wakeups, so it adds no timer and never performs
    /// connection scans or I2C work. A received key is queued for the next
    /// ordinary `poll_key` call.
    pub fn service_fast(&mut self) {
        let Some(interval_ms) = self.usb_host.split_keyboard_poll_interval_ms() else {
            self.usb_split_poll_due_ms = tick::now_ms();
            return;
        };
        let now_ms = tick::now_ms();
        if now_ms < self.usb_split_poll_due_ms {
            return;
        }
        self.usb_split_poll_due_ms = now_ms.saturating_add(interval_ms.max(1));
        if let Some(key) = self.usb_host.poll_split_keyboards() {
            self.push_pending_key(KeyEvent {
                source: KeySource::Usb,
                key,
            });
        }
    }

    /// Advances I2C-keyboard reconnection and USB device-discovery state.
    pub fn service(&mut self) {
        // A bus that has been quiet for a while has recovered, so the next
        // isolated failure is treated as a first one again rather than
        // inheriting an old backoff.
        self.usb_frames_since_stale = self.usb_frames_since_stale.saturating_add(1);
        if self.usb_frames_since_stale >= STALE_RESCAN_SETTLED_FRAMES && self.usb_stale_rescans != 0
        {
            self.usb_stale_rescans = 0;
            self.usb_stale_backoff_frames = 0;
        }
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
            self.usb_host.clear_disconnected();
            self.usb_reconnect_frames = 0;
            if had_registered_device {
                uart::log(b"USB: nothing connected to USB-A\r\n");
            }
        } else if root_connection_changed {
            uart::log(b"USB: root-port connection changed, rescanning...\r\n");
            self.usb_host
                .rescan(usb::RescanReason::PhysicalConnectionChange);
            self.usb_reconnect_frames = 0;
        } else if self.usb_host.needs_reinit() {
            if self.usb_host.detach_disconnected_stale_split_hid() {
                self.clear_pending_keys();
                self.usb_reconnect_frames = 0;
            } else {
                self.rescan_stale_session();
            }
        }

        // Occupied hub ports are swept for removals on their own timer,
        // outside the `has_room` check below. That check is about where a
        // *new* device could go, and it is false exactly when every port is
        // occupied -- which is also the state a port stuck holding a device
        // that has already been unplugged produces. Gating the sweep on it
        // would mean the one case that needs noticing is the one case that
        // is never looked at.
        if self.usb_host.hub().is_some() {
            self.usb_port_sweep_frames += 1;
            if self.usb_port_sweep_frames >= HUB_PORT_SCAN_FRAMES {
                self.usb_port_sweep_frames = 0;
                if self.usb_host.detach_disconnected_hub_ports() {
                    self.clear_pending_keys();
                    // Let discovery run on the next tick rather than after a
                    // full interval: the port is free now, and something is
                    // often plugged straight back into it.
                    self.usb_reconnect_frames = HUB_PORT_SCAN_FRAMES;
                }
            }
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
                self.usb_host.rescan(usb::RescanReason::Manual);
            }
        }
    }

    /// Rescans after a device session went stale, backing off when that keeps
    /// happening.
    ///
    /// The first stale session is rescanned at once: that is the ordinary
    /// case of a device that was unplugged mid-transfer, and waiting would
    /// only make the keyboard feel broken. Repeats are different -- a device
    /// that fails again the moment it is attached will do so forever, and
    /// each attempt resets the bus underneath every working device.
    fn rescan_stale_session(&mut self) {
        if self.usb_stale_backoff_frames > 0 {
            self.usb_stale_backoff_frames -= 1;
            return;
        }
        uart::log(b"USB: a device session went stale, rescanning...\r\n");
        self.usb_host.rescan(usb::RescanReason::Recovery);
        self.usb_reconnect_frames = 0;
        self.usb_stale_rescans = self.usb_stale_rescans.saturating_add(1);
        self.usb_frames_since_stale = 0;
        if self.usb_stale_rescans > 1 {
            self.usb_stale_backoff_frames = (STALE_RESCAN_BACKOFF_FRAMES
                * (self.usb_stale_rescans - 1))
                .min(STALE_RESCAN_BACKOFF_MAX_FRAMES);
            uart::log_u32(
                b"USB: repeated stale sessions, next rescan in frames=",
                self.usb_stale_backoff_frames,
            );
        }
    }

    /// Returns at most one key, rotating the source checked first after every
    /// delivered key so a continuously active source cannot starve the others.
    pub fn poll_key(&mut self) -> Option<KeyEvent> {
        if let Some(event) = self.pop_pending_key() {
            self.next_source = source_after(event.source);
            return Some(event);
        }
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

    fn push_pending_key(&mut self, event: KeyEvent) {
        if self.pending_key_len == PENDING_KEY_EVENTS {
            return;
        }
        let tail = (self.pending_key_head + self.pending_key_len) % PENDING_KEY_EVENTS;
        self.pending_key_events[tail] = Some(event);
        self.pending_key_len += 1;
    }

    fn pop_pending_key(&mut self) -> Option<KeyEvent> {
        if self.pending_key_len == 0 {
            return None;
        }
        let event = self.pending_key_events[self.pending_key_head].take();
        self.pending_key_head = (self.pending_key_head + 1) % PENDING_KEY_EVENTS;
        self.pending_key_len -= 1;
        event
    }

    /// Discards only acquired events, without polling new hardware input.
    pub fn discard_queued_keys(&mut self) {
        self.clear_pending_keys();
        self.usb_host.discard_queued_keys();
    }

    fn clear_pending_keys(&mut self) {
        self.pending_key_events = [None; PENDING_KEY_EVENTS];
        self.pending_key_head = 0;
        self.pending_key_len = 0;
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
                self.service_fast();
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

        if self.primary_touch_blocked {
            if count == 0 {
                self.primary_touch_blocked = false;
            }
            return PrimaryTouch::Idle;
        }
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
    /// Cancels a route-owned gesture and waits for every finger to lift.
    pub fn cancel_primary_touch(&mut self) {
        self.primary_touch_id = None;
        self.primary_touch_blocked = true;
    }

    /// Mutable USB bus registry for commands such as `usbrescan` and MSC I/O.
    pub fn usb_host_mut(&mut self) -> &mut usb::UsbHost {
        &mut self.usb_host
    }

    /// Finalizes and logs the frame-driven initial USB scan.
    pub fn finish_boot_usb_scan(&mut self, started_ms: u64) {
        let needs_retry = self.usb_host.bus_devices().next().is_none();
        self.usb_host.finish_boot_scan_campaign(started_ms);
        if needs_retry {
            // The startup screen has made its bounded verdict, but the bus
            // remains live. Make the first ordinary fallback rescan happen
            // on the next `service` call instead of waiting 300 frames.
            self.usb_reconnect_frames = ROOT_RESCAN_FRAMES;
        }
        uart::log(b"USB: initial scan complete\r\n");
        log_boot_usb_timing(&self.usb_host);
    }
}

/// Reports how long the boot scan took to reach a mass-storage device that
/// could actually be read, one UART line per step.
///
/// This is the measurement the boot-time filesystem choice is sized from: it
/// has to wait for USB before falling back to another medium, and the wait is
/// only defensible if the numbers behind it came from real devices. The three
/// SCSI commands it ends with are the same ones a filesystem probe issues, so
/// they cost the boot path nothing it would not spend anyway.
fn log_boot_usb_timing(usb_host: &usb::UsbHost) {
    let Some(timing) = usb_host.boot_scan_timing().copied() else {
        return;
    };
    uart::log_u32(b"USB BOOT: scan began at uptime ms=", timing.started_at_ms);
    if !timing.connected {
        // The connect wait is spent in full here, and this is the only path
        // that spends it: the cost a boot with nothing plugged into USB-A
        // pays for the storage decision. It belongs in the log for exactly
        // the same reason the successful path's total does.
        uart::log_u32(b"USB BOOT: scan total ms=", timing.total_ms);
        uart::log(b"USB BOOT: no device on USB-A during the initial scan\r\n");
        return;
    }
    uart::log_u32(b"USB BOOT: root connect ms=", timing.connect_ms);
    uart::log_u32(b"USB BOOT: port enabled ms=", timing.port_enabled_ms);
    uart::log_u32(b"USB BOOT: root enumerated ms=", timing.enumerated_ms);
    uart::log_u32(b"USB BOOT: scan total ms=", timing.total_ms);

    if usb_host.mass_storage_inventory().next().is_none() {
        uart::log(b"USB BOOT: initial scan found no mass storage\r\n");
        return;
    }
    uart::log_u32(
        b"USB BOOT: mass storage attached ms=",
        timing.mass_storage_ms,
    );
    uart::log(b"USB BOOT: readiness is handled by startup automount\r\n");
}

const fn source_after(source: KeySource) -> KeySource {
    match source {
        KeySource::CardKb => KeySource::Tab5Keyboard,
        KeySource::Tab5Keyboard => KeySource::Usb,
        KeySource::Usb => KeySource::CardKb,
    }
}

/// Translates one CardKB byte into the application-wide key representation.
///
/// There is no `Key::Control` arm here: CardKB v1.1 has no Ctrl key, so no
/// C0 code for a letter can arrive on this path. `Control` comes from the
/// HID conversion below and nowhere else, which is why the browser keeps a
/// plain `q` and `F2` beside its `Ctrl+Q` and `Ctrl+L`.
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
    // Left Ctrl is bit 0 and right Ctrl bit 4 (HID 1.11 section 8.3).
    let control = modifiers & (1 | (1 << 4)) != 0;
    // Only letters. Ctrl with a digit or a punctuation key keeps producing
    // the character it prints, which is what it did before this existed:
    // nothing binds those, and dropping them would only lose input.
    if control && (0x04..=0x1D).contains(&keycode) {
        return Some(Key::Control(b'a' + (keycode - 0x04)));
    }
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
