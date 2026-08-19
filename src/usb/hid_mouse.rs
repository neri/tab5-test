//! HID Boot Protocol mouse class driver (USB HID 1.11 appendix B.2), the
//! sibling of `hid_keyboard` on top of `hid.rs`'s shared Boot Protocol
//! plumbing.
//!
//! What is specific to a mouse, and so lives here rather than in `hid.rs`,
//! is the report layout: a boot mouse reports *relative* motion since the
//! last report plus a button level, where a boot keyboard reports the
//! absolute set of keys currently held. That difference is what shapes
//! `poll`: successive reports have to be summed rather than diffed, and
//! more than one of them can arrive between two display frames (a mouse
//! commonly reports at 125 Hz against this panel's ~57 Hz), so a frame's
//! motion is the sum of every report drained in it. Dropping the extras
//! would visibly slow the pointer down.
//!
//! `input::InputManager` exposes the result as `poll_mouse`, next to
//! `poll_key`; `app::win` is the first consumer.

use super::hid::{self, InterruptIn};
use super::protocol::EnumeratedDevice;

/// Button bits in a boot mouse report's first byte, and in
/// `MouseUpdate`'s `buttons`/`pressed`/`released`.
pub const MOUSE_BUTTON_LEFT: u8 = 1 << 0;
pub const MOUSE_BUTTON_RIGHT: u8 = 1 << 1;
pub const MOUSE_BUTTON_MIDDLE: u8 = 1 << 2;

/// Bytes a boot mouse report must have to be usable: buttons, dX, dY.
/// Anything shorter is not a report this can decode.
const REPORT_BYTES: usize = 3;
/// Offset of the wheel byte some mice add beyond the boot layout; see
/// `decode_report`.
const WHEEL_OFFSET: usize = 3;
/// Largest report this reads in one transaction. The boot layout is 3 or 4
/// bytes and endpoints declare 4 or 8; this bounds the buffer for a device
/// that declares more.
const MAX_REPORT_BYTES: usize = 8;

/// How many reports one `poll` will drain before returning what it has.
///
/// The loop stops at the first report that is *not* already waiting, so in
/// practice this cap is never the thing that ends it -- draining is what
/// keeps a 125 Hz mouse's motion intact across a ~57 Hz frame, and only a
/// device reporting far faster than that would reach 8 in one frame. What
/// the cap buys is a hard bound on how long `poll` can hold the frame.
const MAX_REPORTS_PER_POLL: u32 = 8;

/// One frame's worth of mouse activity: motion summed over every report
/// drained, and the button state as of the last of them.
///
/// `dx`/`dy` are in the mouse's own counts, positive right and *down* --
/// HID's Y axis already points down, the same direction as the
/// framebuffer's logical Y, so a consumer moving a pointer adds them
/// as-is.
#[derive(Clone, Copy, Default)]
pub struct MouseUpdate {
    pub dx: i32,
    pub dy: i32,
    /// Wheel detents, positive away from the user. Always 0 for a mouse
    /// that reports the bare 3-byte boot layout.
    pub wheel: i32,
    /// Buttons held as of the newest report drained.
    pub buttons: u8,
    /// Buttons that went down during this update, and were not held before
    /// it. Consumers wanting a click rather than a level use this, so they
    /// do not each have to keep their own copy of the previous state.
    pub pressed: u8,
    /// Buttons that came up during this update.
    pub released: u8,
}

pub struct UsbMouse {
    endpoint: InterruptIn,
    /// Bytes to ask for per transaction: the endpoint's wMaxPacketSize,
    /// capped. Asking for more than one packet only makes the core run a
    /// second transaction the device answers short anyway.
    report_bytes: usize,
    /// Button level as of the last report, so `pressed`/`released` can be
    /// derived across polls as well as within one.
    buttons: u8,
}

impl UsbMouse {
    /// Takes a device that `protocol::enumerate_device` has already
    /// addressed and, if it has a HID Boot Protocol mouse interface, puts
    /// that interface into Boot Protocol (see
    /// `hid::configure_boot_protocol`) before returning a handle ready for
    /// `poll`.
    ///
    /// Boot Protocol is what makes this driver possible without parsing
    /// report descriptors: it replaces whatever the mouse's own report
    /// format is with the fixed layout `decode_report` reads.
    pub fn attach(device: &EnumeratedDevice) -> Option<Self> {
        let interface =
            hid::find_boot_interface(device.config_bytes(), hid::INTERFACE_PROTOCOL_MOUSE)?;
        if !hid::configure_boot_protocol(device, interface.interface_number) {
            return None;
        }
        let endpoint = InterruptIn::new(device.device_address, device.route, &interface);
        let report_bytes = endpoint
            .max_packet_size()
            .clamp(REPORT_BYTES, MAX_REPORT_BYTES);
        Some(Self {
            endpoint,
            report_bytes,
            buttons: 0,
        })
    }

    /// True once this mouse's polling session has gone stale; see
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

    /// Drains every report the mouse already has waiting and returns their
    /// combined effect, or `None` if it had nothing to say this frame.
    ///
    /// The drain stops at the first read that does not complete, which is
    /// what keeps its cost close to the keyboard's: a report already
    /// waiting is read immediately, and only the one read that finds
    /// nothing pays `hid`'s NAK timeout. So an idle mouse costs exactly one
    /// timeout per frame, and a busy one costs a handful of fast reads plus
    /// that same single timeout.
    pub fn poll(&mut self) -> Option<MouseUpdate> {
        let previous_buttons = self.buttons;
        let mut update = MouseUpdate::default();
        let mut reports = 0u32;

        while reports < MAX_REPORTS_PER_POLL {
            let mut report = [0u8; MAX_REPORT_BYTES];
            let Some(transferred) = self.endpoint.read_report(&mut report[..self.report_bytes])
            else {
                break; // nothing more waiting (or the transaction failed)
            };
            if transferred < REPORT_BYTES {
                break; // not a decodable boot report
            }
            let (dx, dy, wheel, buttons) = decode_report(&report[..transferred]);
            update.dx += dx;
            update.dy += dy;
            update.wheel += wheel;
            self.buttons = buttons;
            reports += 1;
        }

        if reports == 0 {
            return None;
        }
        update.buttons = self.buttons;
        update.pressed = self.buttons & !previous_buttons;
        update.released = previous_buttons & !self.buttons;
        Some(update)
    }
}

/// Decodes one boot mouse report: button bits, then relative X and Y as
/// signed bytes.
///
/// The wheel byte is read when the device sent one. It is not part of the
/// boot report descriptor (HID 1.11 appendix B.2 defines three bytes), but
/// most wheel mice append it in boot mode anyway, and a device that does
/// not simply reports three bytes and leaves `wheel` at 0 -- so reading it
/// costs nothing and is never wrong for a device that omits it.
fn decode_report(report: &[u8]) -> (i32, i32, i32, u8) {
    let buttons = report[0] & (MOUSE_BUTTON_LEFT | MOUSE_BUTTON_RIGHT | MOUSE_BUTTON_MIDDLE);
    let dx = report[1] as i8 as i32;
    let dy = report[2] as i8 as i32;
    let wheel = report
        .get(WHEEL_OFFSET)
        .map_or(0, |&byte| byte as i8 as i32);
    (dx, dy, wheel, buttons)
}
