//! The parts of HID 1.11's Boot Protocol that do not depend on *which*
//! boot device is being driven: the class-specific requests that put an
//! interface into Boot Protocol, the configuration-descriptor walk that
//! finds such an interface, and the Interrupt IN session that reads its
//! fixed-layout reports.
//!
//! `hid_keyboard` and `hid_mouse` are both thin layers over this: each one
//! only names its own interface protocol (HID 1.11 section 4.3) and
//! decodes its own report format. Everything between those two ends --
//! `SET_CONFIGURATION`/`SET_PROTOCOL`/`SET_IDLE`, the data toggle, the
//! NAK-versus-hard-error distinction, the give-up threshold -- is
//! identical for both and lives here.
//!
//! Like `hid_keyboard` itself, this sits on `protocol.rs` for control
//! transfers and directly on `hcd.rs` for the Interrupt IN polling, which
//! is not a control transfer and so does not go through `protocol.rs`.

use super::hcd::{self, CompletionWait, Endpoint, HCCHAR_EPTYPE_BULK, PacketOutcome, Route};
use super::protocol::{self, EnumeratedDevice, REQUEST_SET_CONFIGURATION};
use crate::uart;

// HID class-specific requests (HID 1.11 section 7.2), used only by
// `configure_boot_protocol`.
const REQUEST_HID_SET_IDLE: u8 = 0x0A;
const REQUEST_HID_SET_PROTOCOL: u8 = 0x0B;
const HID_PROTOCOL_BOOT: u8 = 0;
const REQUEST_TYPE_HOST_TO_DEVICE_INTERFACE: u8 = 0x21; // class, interface recipient

// The boot interface triple (HID 1.11 sections 4.1-4.3). Subclass 1 is what
// promises the fixed report layout; the protocol byte picks which one.
const INTERFACE_CLASS_HID: u8 = 3;
const INTERFACE_SUBCLASS_BOOT: u8 = 1;
pub const INTERFACE_PROTOCOL_KEYBOARD: u8 = 1;
pub const INTERFACE_PROTOCOL_MOUSE: u8 = 2;

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

/// How many SSPLIT/CSPLIT round trips one poll of a device behind a
/// High-Speed hub's TT may take (`hcd::run_packet`'s `max_split_rounds`).
///
/// One attempt per poll. `hcd::run_packet`'s budget is a soft one -- it
/// stops at the first *safe* boundary at or after this many rounds, never
/// mid-handshake -- so 1 means "run the split through to its first NAK and
/// stop there", which is exactly one question asked of the device.
///
/// That is the right shape for per-frame polling: an idle boot device NAKs
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
// `hcd::probe_port` and reset the bus out from under an active session). A
// small threshold is enough since real errors, unlike timeouts, resolve
// quickly. See `InterruptIn::needs_reinit`.
const POLL_FAILURE_GIVE_UP_THRESHOLD: u32 = 10;

/// A HID Boot Protocol interface and the Interrupt IN endpoint that
/// carries its reports, as found in a configuration descriptor.
pub struct BootInterface {
    pub interface_number: u8,
    pub endpoint_address: u8,
    pub max_packet_size: u16,
    pub interval: u8,
}

/// Walks a configuration descriptor's interface/endpoint chain (see
/// `protocol::EnumeratedDevice::config_bytes`) looking for the HID Boot
/// Protocol interface with `interface_protocol`, and that interface's
/// Interrupt IN endpoint. Ignores every other interface -- the other boot
/// device on a combo dongle, extra composite functions -- so a keyboard
/// and a mouse sharing one physical receiver each find their own.
pub fn find_boot_interface(config: &[u8], interface_protocol: u8) -> Option<BootInterface> {
    let mut offset = 0usize;
    while offset + 2 <= config.len() {
        let length = config[offset] as usize;
        if length < 2 || offset + length > config.len() {
            break;
        }
        let descriptor_type = config[offset + 1];
        if descriptor_type == protocol::DESCRIPTOR_TYPE_INTERFACE && length >= 9 {
            let is_target_interface = config[offset + 5] == INTERFACE_CLASS_HID
                && config[offset + 6] == INTERFACE_SUBCLASS_BOOT
                && config[offset + 7] == interface_protocol;
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
                    if endpoint_descriptor_type == protocol::DESCRIPTOR_TYPE_ENDPOINT
                        && endpoint_length >= 7
                    {
                        let endpoint_address = config[endpoint_offset + 2];
                        let attributes = config[endpoint_offset + 3];
                        let is_interrupt = attributes & 0x03 == 3;
                        let is_in = endpoint_address & 0x80 != 0;
                        if is_interrupt && is_in {
                            return Some(BootInterface {
                                interface_number,
                                endpoint_address,
                                max_packet_size: u16::from_le_bytes([
                                    config[endpoint_offset + 4],
                                    config[endpoint_offset + 5],
                                ]) & 0x07FF,
                                interval: config[endpoint_offset + 6],
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

/// Activates `device`'s configuration, switches `interface_number` to Boot
/// Protocol, and disables its idle auto-repeat (report only on change).
///
/// Every HID boot driver needs exactly this sequence before its endpoint
/// produces the fixed-layout reports it expects, so both `UsbKeyboard` and
/// `UsbMouse` call it rather than each ordering the three requests itself.
///
/// `SET_CONFIGURATION` is repeated per driver rather than hoisted into
/// enumeration, which is deliberate and harmless: it is idempotent for a
/// configuration that is already selected, and keeping it here is what lets
/// `registry::attach_class_driver` try one driver after another without a
/// driver that declines having touched the device at all.
pub fn configure_boot_protocol(device: &EnumeratedDevice, interface_number: u8) -> bool {
    let pipe = device.control_pipe();

    let setup = protocol::build_standard_out_setup(
        REQUEST_SET_CONFIGURATION,
        device.configuration_value as u16,
        0,
    );
    if !protocol::control_transfer_out_no_data(&pipe, &setup) {
        uart::log(b"USB: SET_CONFIGURATION failed\r\n");
        return false;
    }

    let setup = build_hid_out_setup(
        REQUEST_HID_SET_PROTOCOL,
        HID_PROTOCOL_BOOT as u16,
        interface_number,
    );
    if !protocol::control_transfer_out_no_data(&pipe, &setup) {
        uart::log(b"USB: HID SET_PROTOCOL(Boot) failed\r\n");
        return false;
    }

    // wValue = (Duration << 8) | ReportID; Duration 0 = report only on
    // change, no idle auto-repeat.
    let setup = build_hid_out_setup(REQUEST_HID_SET_IDLE, 0, interface_number);
    if !protocol::control_transfer_out_no_data(&pipe, &setup) {
        uart::log(b"USB: HID SET_IDLE failed\r\n");
        return false;
    }
    true
}

/// One device's Interrupt IN report stream: the endpoint, its data toggle,
/// and how long it has been failing.
///
/// This is the whole of what a boot class driver shares with its sibling
/// once the interface is configured -- `read_report` is the only way either
/// of them touches the bus per frame.
pub struct InterruptIn {
    /// The Interrupt IN endpoint this polls, including the device address
    /// and speed it was enumerated with -- which is all `read_report` needs
    /// to keep talking to it, whether the device is plugged into USB-A
    /// directly or into a hub port.
    endpoint: Endpoint,
    next_pid_data1: bool,
    interval: u8,
    periodic: Option<hcd::PeriodicHandle>,
    /// Consecutive hard errors from `read_report`; drives `needs_reinit`
    /// and also whether it asks `hcd::run_packet` to stay quiet about
    /// repeats of an already-logged error.
    consecutive_hard_errors: u32,
}

impl InterruptIn {
    pub fn new(device_address: u8, route: Route, interface: &BootInterface) -> Self {
        Self {
            endpoint: Endpoint {
                device_address,
                endpoint_number: interface.endpoint_address & 0x0F,
                endpoint_type: HCCHAR_EPTYPE_BULK,
                mps: interface.max_packet_size,
                is_in: true,
                route,
            },
            // Every endpoint's data toggle resets to DATA0 on
            // SET_CONFIGURATION (USB2.0 9.4.5).
            next_pid_data1: false,
            interval: interface.interval.max(1),
            periodic: None,
            consecutive_hard_errors: 0,
        }
    }

    /// The endpoint's wMaxPacketSize, which is how many bytes a driver
    /// should ask `read_report` for: a boot report is one packet, and
    /// requesting more only makes the core run a second transaction the
    /// device answers with a short packet anyway.
    pub fn max_packet_size(&self) -> usize {
        self.endpoint.mps as usize
    }

    /// Exercises the controller's periodic frame-list path on channel 1.
    /// The live HID path remains on its proven fallback until this diagnostic
    /// succeeds on each relevant speed/topology.
    pub fn probe_periodic(&mut self) -> hcd::PeriodicProbeResult {
        const PROBE_BUFFER_BYTES: usize = 64;
        let mps = self.max_packet_size();
        if mps == 0 || mps > PROBE_BUFFER_BYTES {
            return hcd::probe_periodic_interrupt_in(
                &self.endpoint,
                self.interval,
                self.next_pid_data1,
                &mut [],
            );
        }
        let mut report = [0u8; PROBE_BUFFER_BYTES];
        let result = hcd::probe_periodic_interrupt_in(
            &self.endpoint,
            self.interval,
            self.next_pid_data1,
            &mut report[..mps],
        );
        if result.completed {
            self.next_pid_data1 = !self.next_pid_data1;
        }
        result
    }

    /// Promotes this endpoint from the frame-driven fallback to the proven
    /// persistent periodic path. Returns the allocated host-channel number.
    pub fn enable_periodic(&mut self) -> Option<u8> {
        if let Some(handle) = self.periodic {
            return Some(handle.channel());
        }
        self.periodic = hcd::enable_periodic_hid(&self.endpoint, self.interval);
        self.periodic.map(hcd::PeriodicHandle::channel)
    }

    /// True once polling has failed for long enough that the session
    /// itself -- not just "nothing has happened yet" -- is almost certainly
    /// stale and worth re-enumerating from scratch.
    ///
    /// A physical unplug is caught separately and more cheaply, at the
    /// registry level: `usb::registry::UsbHost::root_disconnected` reads
    /// the root port's connect bit once for the whole bus rather than
    /// asking each device individually (and, for a device behind a hub,
    /// that root-port bit does not even reflect *this* device's own hub
    /// port). What this catches instead is the owning handle's cached
    /// address/configuration going stale while the device is still
    /// physically present -- normally only `UsbHost::rescan` can cause
    /// that now (`docs/USB_REFACTOR_PLAN.md` Stage A made it the sole owner
    /// of `hcd::probe_port`), so in practice this now means a genuine
    /// transaction error rather than another command resetting the bus out
    /// from under an active session. `InputManager` checks this and calls
    /// `UsbHost::rescan` once it's true, which re-enumerates every still
    /// physically present device and recovers within a few frames.
    pub fn needs_reinit(&self) -> bool {
        self.consecutive_hard_errors >= POLL_FAILURE_GIVE_UP_THRESHOLD
    }

    /// Runs one Interrupt IN transaction and returns how many bytes the
    /// device actually sent, or `None` if it had nothing to report this
    /// frame (or the transaction failed).
    ///
    /// Callers must treat a short read as "no report": the report layout is
    /// fixed per boot protocol, and a device that answered with fewer bytes
    /// than that layout needs has not produced one.
    pub fn read_report(&mut self, report: &mut [u8]) -> Option<usize> {
        if let Some(handle) = self.periodic {
            return match hcd::take_periodic_hid_report(handle, report) {
                hcd::PeriodicRead::Pending => None,
                hcd::PeriodicRead::Complete(transferred) => {
                    self.consecutive_hard_errors = 0;
                    Some(transferred)
                }
                hcd::PeriodicRead::Error => {
                    self.consecutive_hard_errors = self.consecutive_hard_errors.wrapping_add(1);
                    None
                }
            };
        }

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
            CompletionWait::PollIdleNak,
            true,
            quiet_errors,
            report,
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
                // `needs_reinit` surfaces this so `InputManager` can drop
                // and re-enumerate instead of erroring forever.
                self.consecutive_hard_errors = self.consecutive_hard_errors.wrapping_add(1);
                return None;
            }
        };
        // Any successful completion consumed this PID per USB2.0's toggle
        // rule, even if the report body ends up unusable to the caller.
        self.next_pid_data1 = !self.next_pid_data1;
        Some(transferred)
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
