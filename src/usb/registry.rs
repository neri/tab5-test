//! Single owner of the USB-A bus, and the device registry that replaces
//! the old "one keyboard, driven from the top of `usb.rs`" model.
//!
//! `docs/USB_HOST_PLAN.md`/`docs/USB_MSC_PLAN.md` staged this project one device at a
//! time, which left two gaps once real hardware had more than one device
//! plugged in: `hub::Hub` only ever drove a single chosen port
//! (`hub.rs`'s old `find_connected_port`), so which device got noticed
//! depended on which port it happened to be plugged into; and every USB
//! shell command (`usbinfo`, `usbhub`, `usbmsc`, ...) called
//! `hcd::probe_port`/`protocol::enumerate_device` independently, which does
//! a full bus reset and silently invalidated whatever `UsbKeyboard` the
//! frame loop had going (`docs/USB_HOST_PLAN.md`'s "Stage 3, trap #2").
//!
//! `UsbHost` fixes both by being the *only* thing that ever calls
//! `hcd::probe_port`/`hub::Hub::open`, and by attaching every occupied
//! port instead of one. See `docs/USB_REFACTOR_PLAN.md` Stages A-D and F.
//!
//! What is on the bus and what this project can drive are two different
//! questions, so they are two arrays: `records` holds every device that
//! answered enumeration -- hub included, unsupported classes included --
//! and `slots` holds only the class drivers. Polling, dispatch and every
//! "is there a keyboard yet" decision read `slots`; the `lsusb` display
//! reads `records`, which is how a device with no driver can still be
//! shown as attached instead of silently missing.

use super::hcd::{self, HostPort, Route, Speed, SplitTarget};
use super::hid_keyboard::UsbKeyboard;
use super::hid_mouse::{MouseUpdate, UsbMouse};
use super::hub::{self, Hub};
use super::msc::UsbMassStorage;
use super::protocol::{
    self, CONFIG_BUFFER_MAX, ControlPipe, DEVICE_DESCRIPTOR_LEN, EnumeratedDevice,
};
use crate::input::Key;
use crate::{tick, uart};

/// Hard cap on hub ports this registry tracks. Real hubs are almost always
/// 4-7 ports; USB2.0 allows up to 255. Bounds the fixed-size slot array the
/// same way `hub::MAX_PORTS` bounds the hub descriptor's removable-port
/// bitmap -- a hub reporting more is capped, with a log line, same as
/// there.
pub const MAX_HUB_PORTS: u8 = 8;

const SLOT_COUNT: usize = MAX_HUB_PORTS as usize + 1; // index 0 = Direct, N = HubPort(N)
const ALL_SLOT_BITS: u16 = (1u16 << SLOT_COUNT) - 1;
/// A background port scan runs roughly once a second. Do not stop discovery
/// for a one-off hub/host hiccup; pause only after this many consecutive
/// full scan failures, then emit the existing one-shot diagnostic.
const HUB_PORT_SCAN_FAILURE_GIVE_UP_THRESHOLD: u8 = 3;
/// Shortest interval between two automatic power cycles (USB-A's VBUS, or
/// one hub port's power), in milliseconds.
///
/// A power cycle blocks for over a second and drops every device on the bus,
/// so a device that is simply broken must not have the frame loop cycling
/// USB-A every few seconds. One escalation, then a long wait before the
/// next: a device that needs it recovers on the first one.
const POWER_RECOVERY_INTERVAL_MS: u64 = 30_000;

/// What one pass over the bus found.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanOutcome {
    /// Nothing is plugged into USB-A.
    Empty,
    /// The device (or hub) answered enumeration, whether or not this
    /// project has a driver for it.
    Answered,
    /// HPRT reports a connected port, but the device behind it will not
    /// enumerate -- either the port never enabled, or control transfers to
    /// it fail outright. A port reset has already been tried by definition:
    /// `probe_port` performs one. What is left is removing its power.
    Unreachable,
}

/// Where an attached device is plugged in, for logging and shell display.
/// Class drivers do not need this -- it never leaves the registry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// Plugged straight into USB-A.
    Direct,
    /// Port `N` (1-based) of the hub plugged into USB-A.
    HubPort(u8),
}

/// The class driver actually driving a slot's device. Devices this project
/// has no driver for are noted in the UART log at attach time and get no
/// entry here -- there is nothing to poll or dispatch to. They are still
/// listed in `DeviceRecord`, which is what the bus inventory is for.
pub enum DeviceKind {
    Keyboard(UsbKeyboard),
    Mouse(UsbMouse),
    MassStorage(UsbMassStorage),
}

/// The handful of `EnumeratedDevice` fields worth keeping around after
/// enumeration for display, since `UsbKeyboard`/`UsbMassStorage` do not
/// carry VID/PID/class themselves (they only need the endpoint and address
/// they were built from).
#[derive(Clone, Copy)]
pub struct DeviceSummary {
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub num_interfaces: u8,
    pub config_total_length: u16,
}

impl DeviceSummary {
    fn from(device: &EnumeratedDevice) -> Self {
        Self {
            vendor_id: device.vendor_id,
            product_id: device.product_id,
            device_class: device.device_class,
            device_subclass: device.device_subclass,
            device_protocol: device.device_protocol,
            num_interfaces: device.num_interfaces,
            config_total_length: device.config_total_length,
        }
    }
}

/// Where the time in one full `rescan` went, in milliseconds from the VBUS
/// enable at the top of `hcd::probe_port`.
///
/// Kept for the boot-time storage decision rather than for its own sake: the
/// firmware wants USB mass storage to win over the SD card when one is
/// plugged in, and that only works if boot waits long enough for a device
/// that is powering up right then. `docs/USB_MSC_BOOT_MARGIN_PLAN.md` turns
/// these numbers into that budget.
#[derive(Clone, Copy, Default)]
pub struct ScanTiming {
    /// Milliseconds on the system tick when the scan started, i.e. how far
    /// into boot the initial scan runs.
    pub started_at_ms: u32,
    /// `HostPort::connect_ms` for this scan's root-port probe.
    pub connect_ms: u32,
    /// `HostPort::enabled_ms` for this scan's root-port probe.
    pub port_enabled_ms: u32,
    /// The root device (hub or plain device) finished standard enumeration
    /// by this point, or 0 if it never did.
    pub enumerated_ms: u32,
    /// The first mass-storage class driver was attached by this point, or 0
    /// if the scan found none.
    pub mass_storage_ms: u32,
    /// The whole scan, including every port behind a hub.
    pub total_ms: u32,
    /// Whether the root port reported a device at all.
    pub connected: bool,
    /// Whether the scan ended with a mass-storage device in the registry.
    pub mass_storage: bool,
}

/// Everything the registry keeps about one device that answered
/// enumeration -- where it is, how to reach it again, and the two standard
/// descriptors it returned.
///
/// This is the registry's inventory of the bus, and it is deliberately
/// wider than `UsbHost::slots`: a device this project has no class driver for
/// still gets a record, because "attached, no driver" is a fact worth
/// showing rather than a device that silently does not exist. Nothing
/// polls or dispatches to a record; only the display reads them.
///
/// The descriptors are kept as the raw bytes the device sent. Enumeration
/// already paid for them, so a display can show any field without a single
/// additional transaction, and without this struct having to grow a
/// mirrored copy of USB2.0 tables 9-8 and 9-12.
pub struct DeviceRecord {
    pub location: Location,
    pub address: u8,
    /// The speed this device's own link came up at -- the root port's speed
    /// for a device plugged into USB-A, or the hub port's for one behind a
    /// hub, which is not necessarily the same as the hub's own.
    pub speed: Speed,
    pub summary: DeviceSummary,
    route: Route,
    max_packet_size0: u8,
    device_descriptor: [u8; DEVICE_DESCRIPTOR_LEN],
    config_descriptor: [u8; CONFIG_BUFFER_MAX],
    config_descriptor_len: usize,
}

impl DeviceRecord {
    fn from(location: Location, speed: Speed, device: &EnumeratedDevice) -> Self {
        let mut config_descriptor = [0u8; CONFIG_BUFFER_MAX];
        let config_bytes = device.config_bytes();
        config_descriptor[..config_bytes.len()].copy_from_slice(config_bytes);
        Self {
            location,
            address: device.device_address,
            speed,
            summary: DeviceSummary::from(device),
            route: device.route,
            max_packet_size0: device.max_packet_size0,
            device_descriptor: *device.device_bytes(),
            config_descriptor,
            config_descriptor_len: config_bytes.len(),
        }
    }

    // The device descriptor fields the display shows but the stack itself
    // never acts on (USB2.0 table 9-8). Reading them out here keeps the
    // byte offsets inside the USB layer rather than in a display module.

    /// `bcdUSB`: the specification revision the device claims, BCD-encoded
    /// (0x0200 = USB 2.0).
    pub fn usb_version(&self) -> u16 {
        u16::from_le_bytes([self.device_descriptor[2], self.device_descriptor[3]])
    }

    /// `bMaxPacketSize0`: endpoint 0's packet size, as re-read after the
    /// initial 8-byte peek.
    pub fn max_packet_size0(&self) -> u8 {
        self.max_packet_size0
    }

    /// `bcdDevice`: the vendor's own release number for this device.
    pub fn device_version(&self) -> u16 {
        u16::from_le_bytes([self.device_descriptor[12], self.device_descriptor[13]])
    }

    /// `iManufacturer`, `iProduct` and `iSerialNumber`: string descriptor
    /// indices, 0 where the device has no such string. Pass one to
    /// `read_string`.
    pub fn manufacturer_string_index(&self) -> u8 {
        self.device_descriptor[14]
    }

    pub fn product_string_index(&self) -> u8 {
        self.device_descriptor[15]
    }

    pub fn serial_string_index(&self) -> u8 {
        self.device_descriptor[16]
    }

    /// `bNumConfigurations`. This project only ever reads and selects
    /// configuration 0, so more than one means the rest went unexamined.
    pub fn num_configurations(&self) -> u8 {
        self.device_descriptor[17]
    }

    // The same for the configuration descriptor's own header (USB2.0
    // table 9-10), which is the first nine bytes of `config_bytes`.

    /// `bConfigurationValue`: what a `SET_CONFIGURATION` selecting this
    /// configuration carries.
    pub fn configuration_value(&self) -> u8 {
        self.config_header_byte(5)
    }

    /// `bmAttributes`: bit 6 self-powered, bit 5 remote wakeup.
    pub fn config_attributes(&self) -> u8 {
        self.config_header_byte(7)
    }

    /// `bMaxPower`, converted from the descriptor's 2 mA units.
    pub fn max_power_ma(&self) -> u16 {
        self.config_header_byte(8) as u16 * 2
    }

    /// One byte of the configuration descriptor's own nine-byte header,
    /// or 0 if the device answered with a shorter one than that. The
    /// backing array is always full length, so this deliberately bounds
    /// itself by what was actually read rather than by the buffer.
    fn config_header_byte(&self, offset: usize) -> u8 {
        self.config_bytes().get(offset).copied().unwrap_or(0)
    }

    /// Walks this device's configuration descriptor chain, in the order the
    /// device sent it: the config header, then each interface followed by
    /// its class-specific and endpoint descriptors.
    pub fn descriptors(&self) -> impl Iterator<Item = protocol::RawDescriptor<'_>> {
        protocol::descriptors(self.config_bytes())
    }

    /// The first LANGID this device supports, or `None` if it has no
    /// string descriptors at all. Costs one control transfer.
    pub fn string_language(&self) -> Option<u16> {
        protocol::read_string_language(&self.control_pipe())
    }

    /// Reads one of this device's string descriptors as ASCII. Costs one
    /// control transfer per call, which is why enumeration does not do it
    /// for every device up front: the boot-time scan is on the critical
    /// path of the storage decision (`docs/USB_MSC_BOOT_MARGIN_PLAN.md`),
    /// and nothing but a display has ever needed these.
    pub fn read_string(&self, index: u8, language: u16, out: &mut [u8]) -> Option<usize> {
        protocol::read_string_ascii(&self.control_pipe(), index, language, out)
    }

    /// The configuration descriptor as far as it was read; see
    /// `config_truncated`.
    pub fn config_bytes(&self) -> &[u8] {
        &self.config_descriptor[..self.config_descriptor_len]
    }

    /// True if the device's configuration descriptor is longer than
    /// `protocol::CONFIG_BUFFER_MAX`, so the interfaces at its end were
    /// never read and cannot be shown -- or attached to, for that matter.
    pub fn config_truncated(&self) -> bool {
        self.summary.config_total_length as usize > self.config_descriptor_len
    }

    /// True for a hub, which occupies the root port but is driven by
    /// `hub::Hub` rather than by anything in `DeviceKind`.
    pub fn is_hub(&self) -> bool {
        self.summary.device_class == hub::DEVICE_CLASS_HUB
    }

    /// The hub address and port whose Transaction Translator relays for
    /// this device, or `None` when the controller addresses it directly.
    /// Only a Full/Low-Speed device behind a High-Speed hub is split.
    pub fn split_route(&self) -> Option<(u8, u8)> {
        self.route
            .split
            .map(|target| (target.hub_address, target.port_number))
    }

    /// True if transactions to this device are prefixed with PRE tokens,
    /// i.e. it is a Low-Speed device reached through a hub.
    pub fn low_speed_via_hub(&self) -> bool {
        self.route.low_speed_via_hub
    }

    /// The control pipe this device is still reachable on, for a caller
    /// that wants a descriptor enumeration did not keep -- string
    /// descriptors, in practice. Every address stays valid until the next
    /// `rescan`, so this needs no bus traffic of its own to rebuild.
    pub fn control_pipe(&self) -> ControlPipe {
        ControlPipe {
            device_address: self.address,
            mps: self.max_packet_size0 as u16,
            route: self.route,
        }
    }
}

/// A read-only view of one attached device, for shell commands that just
/// want to list what is plugged in.
pub struct AttachedDevice<'a> {
    pub location: Location,
    pub summary: &'a DeviceSummary,
    pub kind: &'a DeviceKind,
}

/// One device on the bus as the `lsusb` display sees it: its record, plus
/// the class driver bound to it if this project has one.
pub struct BusDevice<'a> {
    pub record: &'a DeviceRecord,
    pub driver: Option<&'a DeviceKind>,
}

/// Owns everything this project's USB-A stack can talk to at once: the
/// last root-port probe, an optional hub plugged into it, and up to
/// `MAX_HUB_PORTS` devices behind that hub (or the one device plugged into
/// USB-A directly, if it is not a hub).
///
/// `input::InputManager` holds the only instance, across the lifetime of the
/// frame loop, and is the only thing that calls `rescan`. Every USB shell command
/// in `shell.rs` takes a `&UsbHost`/`&mut UsbHost` and reads or drives
/// devices already in the registry instead of touching `hcd`/`hub`/
/// `protocol` directly -- so nothing can reset the bus out from under a
/// live session anymore (`docs/USB_REFACTOR_PLAN.md` Stage A).
pub struct UsbHost {
    last_probe: Option<HostPort>,
    hub: Option<Hub>,
    /// The speed the hub's own upstream link came up at, kept because
    /// `scan_empty_hub_ports` needs it to route a device found later and
    /// cannot re-derive it without another bus reset. `Speed::Unknown`
    /// whenever `hub` is `None`.
    hub_speed: Speed,
    /// An empty-port scan reached a hub that did not answer even after the
    /// control-transfer retry budget.  This affects only discovery of *new*
    /// devices; resetting the whole bus here would tear down working
    /// keyboards/storage every second.  `clear`/`rescan` re-arm scanning.
    hub_port_scan_paused: bool,
    /// Consecutive background scans that could not read an empty hub port.
    /// A successful full scan resets it, so transient recovery stays silent.
    hub_port_scan_failures: u8,
    /// Slot index at which the next keyboard scan starts.  Advancing it after
    /// each delivered key prevents a low-numbered USB keyboard from starving
    /// another one that is also producing input.
    next_keyboard_slot: usize,
    /// Slots whose current physical attachment could not be driven, either
    /// because enumeration failed or no class driver matched. Background
    /// discovery checks only the hub connection/change bits for these ports;
    /// it must not reset and enumerate the same failing device every second.
    /// A disconnect/reconnect edge or an explicit full `rescan` retries it.
    unhandled_slots: u16,
    /// The class driver bound to each slot, or `None` where nothing is
    /// attached *or* nothing here can drive what is. `records` is the
    /// inventory; this is only what the frame loop and the class-specific
    /// shell commands can dispatch to.
    slots: [Option<DeviceKind>; SLOT_COUNT],
    /// Every device that answered enumeration, in the same slot order --
    /// including the hub itself (index 0, where a hub leaves `slots`
    /// empty) and devices no class driver wanted. Whenever a slot holds a
    /// driver, the matching record is present too; the reverse does not
    /// hold. Display only: see `DeviceRecord`.
    records: [Option<DeviceRecord>; SLOT_COUNT],
    /// Timing of the most recent full `rescan`.
    last_scan: Option<ScanTiming>,
    /// Tick milliseconds of the last automatic power cycle, so a device
    /// that never answers cannot make the frame loop cut power on every
    /// scan.
    last_power_recovery_ms: Option<u64>,
    /// Timing of the *first* full `rescan`, which is the one
    /// `InputManager::new` runs during boot -- the only scan whose duration
    /// the boot-time storage decision has to live with. Later scans overwrite
    /// `last_scan` but never this.
    boot_scan: Option<ScanTiming>,
}

impl UsbHost {
    pub const fn new() -> Self {
        const NONE_DRIVER: Option<DeviceKind> = None;
        const NONE_RECORD: Option<DeviceRecord> = None;
        Self {
            last_probe: None,
            hub: None,
            hub_speed: Speed::Unknown,
            hub_port_scan_paused: false,
            hub_port_scan_failures: 0,
            next_keyboard_slot: 0,
            unhandled_slots: 0,
            slots: [NONE_DRIVER; SLOT_COUNT],
            records: [NONE_RECORD; SLOT_COUNT],
            last_scan: None,
            boot_scan: None,
            last_power_recovery_ms: None,
        }
    }

    /// Timing of the most recent full `rescan`.
    pub fn last_scan_timing(&self) -> Option<&ScanTiming> {
        self.last_scan.as_ref()
    }

    /// Timing of the initial scan run during boot.
    pub fn boot_scan_timing(&self) -> Option<&ScanTiming> {
        self.boot_scan.as_ref()
    }

    /// The most recent root-port probe result (VBUS/core/port state), for
    /// shell diagnostics. `None` before the first `rescan`.
    pub fn last_probe(&self) -> Option<&HostPort> {
        self.last_probe.as_ref()
    }

    /// The hub plugged into USB-A directly, if any.
    pub fn hub(&self) -> Option<&Hub> {
        self.hub.as_ref()
    }

    /// The hub's own VID/PID/class, if one is attached. A hub occupies the
    /// root port, so its record is the root one.
    pub fn hub_summary(&self) -> Option<&DeviceSummary> {
        if self.hub.is_none() {
            return None;
        }
        self.records[0].as_ref().map(|record| &record.summary)
    }

    /// Every device the last scan enumerated, driver or not, in slot order
    /// (the root port first, then hub ports low to high). This is what the
    /// `lsusb` display walks; `attached_devices` below is the narrower
    /// "what can this project actually talk to" view.
    pub fn bus_devices(&self) -> impl Iterator<Item = BusDevice<'_>> {
        self.records
            .iter()
            .zip(self.slots.iter())
            .filter_map(|(record, driver)| {
                record.as_ref().map(|record| BusDevice {
                    record,
                    driver: driver.as_ref(),
                })
            })
    }

    /// One enumerated device by its USB address, for a command that takes
    /// the address the display just showed.
    pub fn bus_device(&self, address: u8) -> Option<BusDevice<'_>> {
        self.bus_devices()
            .find(|device| device.record.address == address)
    }

    /// Hub ports holding a device that could not be enumerated at all, and
    /// so has no record to show. Worth naming in the display: the
    /// alternative is a port that looks empty when something is plugged
    /// into it. Costs no bus traffic -- this is the state the last scan
    /// left behind, not a fresh port poll.
    pub fn unenumerated_hub_ports(&self) -> impl Iterator<Item = u8> + '_ {
        (1..SLOT_COUNT).filter_map(move |index| {
            let occupied_but_unknown =
                self.unhandled_slots & (1u16 << index) != 0 && self.records[index].is_none();
            occupied_but_unknown.then_some(index as u8)
        })
    }

    /// Every currently attached device, root or hub port alike, in slot
    /// order (`Direct` first, then hub ports low to high).
    pub fn attached_devices(&self) -> impl Iterator<Item = AttachedDevice<'_>> {
        self.bus_devices().filter_map(|device| {
            device.driver.map(|kind| AttachedDevice {
                location: device.record.location,
                summary: &device.record.summary,
                kind,
            })
        })
    }

    /// The first attached Mass Storage device, if any -- `usbmsc`/
    /// `usbread`/`usbmbr` no longer enumerate their own device fresh on
    /// every call; they share whatever `rescan` already attached, wherever
    /// it is (USB-A directly or a hub port). `docs/USB_REFACTOR_PLAN.md` Stage F.
    pub fn mass_storage_mut(&mut self) -> Option<&mut UsbMassStorage> {
        self.slots.iter_mut().flatten().find_map(|slot| match slot {
            DeviceKind::MassStorage(storage) => Some(storage),
            DeviceKind::Keyboard(_) | DeviceKind::Mouse(_) => None,
        })
    }

    /// Runs the opt-in periodic-scheduler diagnostic on the first HID slot.
    /// Normal frame polling is paused while the shell owns `&mut UsbHost`.
    pub fn probe_periodic_hid(&mut self) -> Option<(&'static str, hcd::PeriodicProbeResult)> {
        self.slots.iter_mut().flatten().find_map(|slot| match slot {
            DeviceKind::Keyboard(keyboard) => Some(("keyboard", keyboard.probe_periodic())),
            DeviceKind::Mouse(mouse) => Some(("mouse", mouse.probe_periodic())),
            DeviceKind::MassStorage(_) => None,
        })
    }

    /// True if any attached device is a HID Boot mouse. Lets a pointer-driven
    /// screen say so up front instead of leaving the user to guess why the
    /// cursor never moves.
    pub fn has_mouse(&self) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|slot| matches!(slot, DeviceKind::Mouse(_)))
    }

    /// Cheap liveness check (one HPRT read, no transaction): true once
    /// nothing is plugged into USB-A at all, in which case nothing behind
    /// it (hub or no hub) can still be there either. `InputManager` calls this
    /// every frame, the same spirit as `CardKb`'s bus-failure check, but
    /// for the whole registry at once rather than per device.
    pub fn root_disconnected(&self) -> bool {
        self.last_probe.is_some() && !hcd::port_connected()
    }

    /// Takes the root-port connection event captured by the USB ISR. The
    /// registry remains the sole bus owner, so only `InputManager` consumes
    /// this edge and decides whether to rebuild every address/session.
    pub fn take_root_connection_change(&mut self) -> bool {
        hcd::take_root_connection_change()
    }

    /// Drops every slot and the hub handle without touching the bus --
    /// what `InputManager` calls once `root_disconnected` reports the cable
    /// itself came out.
    pub fn clear(&mut self) {
        self.clear_registry();
        self.last_probe = None;
        self.unhandled_slots = 0;
    }

    /// Drops registered driver state but preserves which still-connected
    /// unsupported slots have already been reported. `rescan` uses this
    /// variant so its periodic retry remains quiet; `clear` above is the
    /// physical root-disconnect path and re-arms diagnostics for the next
    /// attachment.
    fn clear_registry(&mut self) {
        let _ = hcd::disable_periodic_hid();
        self.hub = None;
        self.hub_speed = Speed::Unknown;
        self.hub_port_scan_paused = false;
        self.hub_port_scan_failures = 0;
        self.next_keyboard_slot = 0;
        for slot in self.slots.iter_mut() {
            *slot = None;
        }
        for record in self.records.iter_mut() {
            *record = None;
        }
    }

    /// True if nothing is currently registered at all -- no hub, no
    /// direct device, no hub-port device. Lets `InputManager` fire the
    /// "disconnected" log line once on the transition rather than every
    /// single frame the cable stays unplugged (`root_disconnected` alone
    /// is a raw per-frame register read with no memory of the last call).
    pub fn is_empty(&self) -> bool {
        self.hub.is_none() && self.slots.iter().all(|slot| slot.is_none())
    }

    /// True once at least one live keyboard slot's session has gone stale
    /// (`UsbKeyboard::needs_reinit`) and the whole bus needs re-probing. A
    /// bus reset invalidates every device's address at once (`rescan`
    /// tears down and rebuilds every slot, not just the stale one) -- there
    /// is no such thing as reinitializing just one.
    ///
    /// Mass Storage slots have no per-frame polling session and deliberately
    /// do not contribute here. A dead BOT session fails subsequent commands
    /// quickly and waits for an explicit `usbrescan`; resetting the whole bus
    /// automatically would interrupt otherwise healthy keyboards and mice.
    pub fn needs_reinit(&self) -> bool {
        // A bus marked unusable is specifically a controller-level problem:
        // channel 0 could not be halted. It is safe to rebuild every session
        // here because no later control or bulk transfer can be trusted.
        if hcd::bus_unusable() {
            return true;
        }
        self.slots.iter().flatten().any(|slot| match slot {
            DeviceKind::Keyboard(keyboard) => keyboard.needs_reinit(),
            DeviceKind::Mouse(mouse) => mouse.needs_reinit(),
            DeviceKind::MassStorage(_) => false,
        })
    }

    /// Removes a stale serialized Split HID if its downstream hub port was
    /// physically disconnected or changed attachment.
    ///
    /// A hub-port unplug does not change root HPRT: the High-Speed hub is
    /// still present. The first evidence is therefore the in-flight Split
    /// transaction failing. Rebuilding the root bus at that point would
    /// invalidate an unrelated MSC session and, on a self-powered hub, can
    /// race a still-changing downstream status. Confirm the owning port and
    /// drop only that slot instead. A still-connected, unchanged port is a
    /// genuine stale session and returns `false` so the existing full-rescan
    /// recovery remains available.
    pub fn detach_disconnected_stale_split_hid(&mut self) -> bool {
        let mut stale_ports = 0u16;
        for (index, slot) in self.slots.iter().enumerate().skip(1) {
            let Some(slot) = slot else { continue };
            let stale_split_hid = match slot {
                DeviceKind::Keyboard(keyboard) => {
                    keyboard.split_poll_interval_ms().is_some() && keyboard.needs_reinit()
                }
                DeviceKind::Mouse(mouse) => {
                    mouse.split_poll_interval_ms().is_some() && mouse.needs_reinit()
                }
                DeviceKind::MassStorage(_) => false,
            };
            if stale_split_hid {
                stale_ports |= 1u16 << index;
            }
        }
        if stale_ports == 0 {
            return false;
        }

        let Some(hub) = self.hub.take() else {
            return false;
        };
        let mut detached = 0u16;
        for port in 1..=hub.port_count().min(MAX_HUB_PORTS) {
            let bit = 1u16 << port;
            if stale_ports & bit == 0 {
                continue;
            }
            let changed_or_gone = match hub.port_status(port) {
                Some(status) if !status.connected() => true,
                Some(status) if status.connection_changed() => {
                    let _ = hub.clear_port_connection_change(port);
                    true
                }
                Some(_) => matches!(hub.debounce_connected_port(port), Some(false)),
                None => false,
            };
            if changed_or_gone {
                self.slots[port as usize] = None;
                self.records[port as usize] = None;
                self.clear_unhandled_slot(port as usize);
                detached |= bit;
                uart::log_hex(b"USB: HID disconnected from hub port ", port as u32);
            }
        }
        self.hub = Some(hub);

        if detached != 0 {
            self.hub_port_scan_paused = false;
            self.hub_port_scan_failures = 0;
            self.next_keyboard_slot = 0;
            true
        } else {
            false
        }
    }

    /// True if there is room for another device to be picked up by the
    /// next `rescan`: an empty root slot with no hub attached, or (if a hub
    /// is attached) any hub port not currently holding a device. Drives
    /// `InputManager`'s coarse reconnect throttle, generalizing its old "no
    /// keyboard yet" check to the whole registry.
    pub fn has_room(&self) -> bool {
        match &self.hub {
            None => self.slots[0].is_none(),
            Some(hub) => {
                if self.hub_port_scan_paused {
                    return false;
                }
                let port_count = hub.port_count().min(MAX_HUB_PORTS);
                (1..=port_count).any(|port| self.slots[port as usize].is_none())
            }
        }
    }

    /// Polls every attached keyboard slot in round-robin order (root and hub
    /// ports alike) and returns the first newly-available key.  The next scan
    /// begins after the slot that won this one, preventing a low-numbered
    /// slot from monopolizing input when more than one keyboard is active.
    pub fn poll_keyboards(&mut self) -> Option<Key> {
        for offset in 0..SLOT_COUNT {
            let index = (self.next_keyboard_slot + offset) % SLOT_COUNT;
            let Some(slot) = self.slots[index].as_mut() else {
                continue;
            };
            if let DeviceKind::Keyboard(keyboard) = slot
                && keyboard.split_poll_interval_ms().is_none()
                && let Some(byte) = keyboard.poll()
            {
                self.next_keyboard_slot = (index + 1) % SLOT_COUNT;
                return Some(byte);
            }
        }
        None
    }

    /// Fast-path interval requested by serialized keyboards behind a
    /// High-Speed hub. Direct and Full-Speed-hub devices either use the DWC
    /// periodic scheduler or retain the display-frame fallback and do not
    /// participate here.
    pub fn split_keyboard_poll_interval_ms(&self) -> Option<u64> {
        self.slots
            .iter()
            .flatten()
            .filter_map(|slot| match slot {
                DeviceKind::Keyboard(keyboard) => keyboard.split_poll_interval_ms(),
                DeviceKind::Mouse(_) | DeviceKind::MassStorage(_) => None,
            })
            .min()
    }

    /// Polls only serialized Split keyboards, retaining the normal
    /// round-robin starting point used by the display-frame path.
    pub fn poll_split_keyboards(&mut self) -> Option<Key> {
        for offset in 0..SLOT_COUNT {
            let index = (self.next_keyboard_slot + offset) % SLOT_COUNT;
            let Some(slot) = self.slots[index].as_mut() else {
                continue;
            };
            if let DeviceKind::Keyboard(keyboard) = slot
                && keyboard.split_poll_interval_ms().is_some()
                && let Some(key) = keyboard.poll()
            {
                self.next_keyboard_slot = (index + 1) % SLOT_COUNT;
                return Some(key);
            }
        }
        None
    }

    /// Polls every attached mouse and returns their combined motion for this
    /// frame, or `None` if none of them reported anything.
    ///
    /// Unlike `poll_keyboards` there is no round-robin here, and nothing to
    /// starve: a key is a discrete event that has to be delivered one at a
    /// time, whereas motion is additive, so every mouse can be drained on
    /// every call and the results summed. Two mice therefore both move the
    /// one pointer, which is also how a desktop OS behaves.
    pub fn poll_mice(&mut self) -> Option<MouseUpdate> {
        let mut combined: Option<MouseUpdate> = None;
        for slot in self.slots.iter_mut().flatten() {
            let DeviceKind::Mouse(mouse) = slot else {
                continue;
            };
            let Some(update) = mouse.poll() else { continue };
            match &mut combined {
                None => combined = Some(update),
                Some(total) => {
                    total.dx += update.dx;
                    total.dy += update.dy;
                    total.wheel += update.wheel;
                    total.buttons |= update.buttons;
                    total.pressed |= update.pressed;
                    total.released |= update.released;
                }
            }
        }
        combined
    }

    /// Tears down and rebuilds the entire registry from scratch: probes
    /// the root port, enumerates whatever is plugged into USB-A, and (if
    /// it is a hub) attaches every occupied port. Exactly like the old
    /// `usb::connect_keyboard` -- the port is reset and addresses are
    /// reassigned unconditionally -- generalized to every device instead
    /// of just one, since a bus reset invalidates all of them together
    /// anyway; there is no persistent bus state to keep in sync
    /// incrementally.
    pub fn rescan(&mut self) {
        // Drop an edge which led us here, then discard the connection/enable
        // changes generated by our own reset sequence. A later physical edge
        // is the only event `InputManager` should treat as hotplug.
        let _ = hcd::take_root_connection_change();
        self.rescan_inner();
        let _ = hcd::take_root_connection_change();
    }

    /// Drops every live address/session, removes USB-A VBUS long enough to
    /// reset the hub and all downstream devices, restores power, then builds
    /// the registry from scratch. This is deliberately separate from normal
    /// `rescan`: a port reset is the cheap first-line recovery, while a power
    /// cycle is reserved for an explicitly exhausted recovery sequence.
    pub fn power_cycle_and_rescan(&mut self) -> bool {
        self.clear();
        if !hcd::power_cycle_vbus() {
            uart::log(b"USB: VBUS power cycle failed at PI4IOE2\r\n");
            return false;
        }
        // Counts towards the automatic escalation's interval as well: the
        // rail has just been cycled, so the scan below finding nothing is
        // not a reason to cycle it again.
        self.last_power_recovery_ms = Some(tick::now_ms());
        self.rescan();
        true
    }

    /// Wraps the scan itself in the timing the boot-time storage decision is
    /// sized from. The measurement is three tick reads and costs nothing on
    /// the paths that do the actual work.
    fn rescan_inner(&mut self) {
        let start = tick::now_ms();
        let mut timing = ScanTiming {
            started_at_ms: start as u32,
            ..ScanTiming::default()
        };
        // One failed channel recovery is worth exactly one escalation. Take
        // it here so the flag cannot make every later scan escalate too.
        let recovery_failed = hcd::take_bus_unusable();
        let outcome = self.rescan_devices(&mut timing, start);
        // A device that answers nothing after a port reset has one remedy
        // left that software can reach: taking its power away. The same
        // applies when a channel could not be recovered, because then it is
        // this host, not the device, that is in an unknown state.
        if (outcome == ScanOutcome::Unreachable || recovery_failed) && self.may_power_cycle() {
            uart::log(b"USB: device unreachable after a port reset; power-cycling USB-A\r\n");
            self.clear_registry();
            if hcd::power_cycle_vbus() {
                self.last_power_recovery_ms = Some(tick::now_ms());
                self.rescan_devices(&mut timing, start);
            } else {
                uart::log(b"USB: VBUS power cycle failed at PI4IOE2\r\n");
            }
        }
        timing.total_ms = milliseconds_since(start);
        timing.mass_storage = timing.mass_storage_ms != 0;
        self.last_scan = Some(timing);
        if self.boot_scan.is_none() {
            self.boot_scan = Some(timing);
        }
    }

    /// True if enough time has passed since the last automatic power cycle.
    fn may_power_cycle(&self) -> bool {
        match self.last_power_recovery_ms {
            None => true,
            Some(previous) => tick::now_ms().saturating_sub(previous) >= POWER_RECOVERY_INTERVAL_MS,
        }
    }

    fn rescan_devices(&mut self, timing: &mut ScanTiming, start: u64) -> ScanOutcome {
        self.clear_registry();

        let port = hcd::probe_port();
        self.last_probe = Some(port);
        timing.connect_ms = port.connect_ms;
        timing.port_enabled_ms = port.enabled_ms;
        timing.connected = port.connected;
        if !port.enabled {
            if !port.connected {
                self.clear_unhandled_slot(0);
                return ScanOutcome::Empty;
            }
            // Connected but never enabled: the reset pulse in `probe_port`
            // did not bring the device up.
            return ScanOutcome::Unreachable;
        }

        // Nothing plugged into USB-A directly ever needs preambles or
        // splits: the bus itself runs at the device's speed.
        let Some(device) =
            protocol::enumerate_device(protocol::ROOT_DEVICE_ADDRESS, Route::default())
        else {
            uart::log(b"USB: root device enumeration failed\r\n");
            return ScanOutcome::Unreachable;
        };
        timing.enumerated_ms = milliseconds_since(start);
        // Recorded before any driver decision, so the inventory covers the
        // hub, the driven device and the unsupported device alike.
        self.records[0] = Some(DeviceRecord::from(Location::Direct, port.speed, &device));

        if device.device_class == hub::DEVICE_CLASS_HUB {
            if !self.attach_hub(&device, port.speed, timing, start) {
                return ScanOutcome::Unreachable;
            }
        } else if let Some(mut kind) = attach_class_driver(&device) {
            if matches!(kind, DeviceKind::Keyboard(_) | DeviceKind::Mouse(_)) {
                if port.speed == Speed::High {
                    uart::log(b"USB HID: high-speed periodic unverified, using frame poll\r\n");
                } else {
                    log_periodic_result(enable_periodic_kind(&mut kind));
                }
            }
            self.clear_unhandled_slot(0);
            let is_mass_storage = matches!(kind, DeviceKind::MassStorage(_));
            self.slots[0] = Some(kind);
            if is_mass_storage {
                timing.mass_storage_ms = milliseconds_since(start);
            }
        } else {
            self.report_unhandled_slot(0, &device);
        }
        ScanOutcome::Answered
    }

    /// Opens the hub plugged into USB-A, powers its ports, and attaches
    /// whatever is connected on each one in turn -- up to `MAX_HUB_PORTS`
    /// of them, unlike the old `hub::Hub::find_connected_port`'s "first
    /// port only" (`docs/USB_REFACTOR_PLAN.md` Stage C).
    ///
    /// Ports are enumerated one at a time, never interleaved, per
    /// `protocol::enumerate_device`'s "only one device may be in the
    /// unaddressed default state at a time" constraint.
    ///
    /// `hub_speed` is the speed the *root port* came up at, which is the
    /// speed of the hub's own upstream link since the hub is plugged
    /// straight into USB-A. It decides how each downstream device has to be
    /// reached: a High-Speed hub relays traffic for anything slower through
    /// its Transaction Translator, while a hub running at the same speed as
    /// its devices is a plain repeater. See `route_behind_hub`.
    fn attach_hub(
        &mut self,
        device: &EnumeratedDevice,
        hub_speed: Speed,
        timing: &mut ScanTiming,
        start: u64,
    ) -> bool {
        let Some(hub) = Hub::open(device) else {
            // The hub answered its device descriptor but not its class
            // descriptor: it is on the bus without being usable, which is
            // the same dead end as a device that will not enumerate.
            return false;
        };
        if hub.descriptor.port_count > MAX_HUB_PORTS {
            uart::log_hex(
                b"USB: hub reports more ports than this registry tracks, capping at ",
                MAX_HUB_PORTS as u32,
            );
        }
        if !hub.power_on_all_ports() {
            // The descriptor read already succeeded, so track the hub
            // anyway: a diagnostic display still has something to show,
            // even with no ports attached.
            self.hub = Some(hub);
            return true;
        }

        let port_count = hub.port_count().min(MAX_HUB_PORTS);
        let mut new_slots = 0u16;
        for port in 1..=port_count {
            match hub.debounce_connected_port(port) {
                Some(true) => {}
                Some(false) => {
                    self.clear_unhandled_slot(port as usize);
                    continue;
                }
                None => {
                    uart::log(b"USB: hub stopped answering while scanning ports\r\n");
                    break;
                }
            }
            if self.attach_hub_port(&hub, port, hub_speed) {
                new_slots |= 1u16 << port;
            }
            if timing.mass_storage_ms == 0
                && matches!(self.slots[port as usize], Some(DeviceKind::MassStorage(_)))
            {
                timing.mass_storage_ms = milliseconds_since(start);
            }
        }

        // Do not arm periodic HID DMA while other already-connected ports
        // are still being reset and enumerated. A HID on a lower-numbered
        // port used to start channel 1 here, then channel-0 control traffic
        // to a later port intermittently failed. A HID plugged in after the
        // hub worked because there was no remaining enumeration traffic.
        self.configure_new_hub_hid_slots(new_slots, hub_speed);

        self.hub = Some(hub);
        self.hub_speed = hub_speed;
        true
    }

    /// Picks up devices plugged into hub ports that were empty last time,
    /// leaving every already-attached device exactly as it is.
    ///
    /// This is what the frame loop polls with, instead of the `rescan` it
    /// used to call on a timer. `rescan` resets the bus, which
    /// invalidates every device address on it, so running it on a timer
    /// tore down and re-enumerated working devices every few seconds --
    /// visible as a stall, and long enough to drop a keystroke.
    ///
    /// Nothing here touches the bus state: an empty port costs one
    /// `GET_STATUS` control transfer to the hub and no delay at all
    /// (`Hub::debounce_connected_port` returns immediately when the port
    /// reads as unoccupied), and only a port that has actually gained a
    /// device gets reset and enumerated.
    pub fn scan_empty_hub_ports(&mut self) {
        if self.hub_port_scan_paused {
            return;
        }
        // Taken out of `self` for the duration so the loop can borrow the
        // hub while filling in `self.slots`; nothing else can run in
        // between (this is all synchronous, single-threaded polling).
        let Some(hub) = self.hub.take() else { return };
        let hub_speed = self.hub_speed;
        let mut pause_hub_scan = false;
        let mut new_slots = 0u16;

        let port_count = hub.port_count().min(MAX_HUB_PORTS);
        for port in 1..=port_count {
            if self.slots[port as usize].is_some() {
                continue; // already driving something here
            }
            let slot_bit = 1u16 << port;
            if self.unhandled_slots & slot_bit != 0 {
                match hub.port_status_quiet(port) {
                    Some(status) if status.connected() && !status.connection_changed() => {
                        // Same attachment that already failed. Leave its
                        // address-0 state and every working peer untouched.
                        continue;
                    }
                    Some(status) if !status.connected() => {
                        self.clear_unhandled_slot(port as usize);
                        continue;
                    }
                    Some(_) => {
                        // A connection-change edge while currently connected
                        // means the port was unplugged/replugged between polls.
                        // Debounce and enumerate the new attachment below.
                        self.clear_unhandled_slot(port as usize);
                    }
                    None => {
                        pause_hub_scan = true;
                        break;
                    }
                }
            }
            match hub.debounce_connected_port_quiet(port) {
                Some(true) => {}
                Some(false) => {
                    self.clear_unhandled_slot(port as usize);
                    continue;
                }
                None => {
                    // This scan is only an opportunistic way to notice new
                    // devices.  A root reset here would discard every
                    // working keyboard/storage session, then produce a
                    // misleading stream of "attached" logs. Keep the live
                    // registry and wait for an explicit or genuine-device
                    // error rescan instead.
                    pause_hub_scan = true;
                    break;
                }
            }
            if self.attach_hub_port(&hub, port, hub_speed) {
                new_slots |= 1u16 << port;
            }
        }

        self.configure_new_hub_hid_slots(new_slots, hub_speed);

        self.hub = Some(hub);
        if pause_hub_scan {
            self.hub_port_scan_failures = self.hub_port_scan_failures.saturating_add(1);
            if self.hub_port_scan_failures < HUB_PORT_SCAN_FAILURE_GIVE_UP_THRESHOLD {
                // Keep the live registry and try again on the next coarse
                // scan. The control failure is commonly transient and a
                // later request succeeds without a root-port reset.
                return;
            }
            self.hub_port_scan_paused = true;
            uart::log(b"USB: hub port scan paused after repeated recovery failures; run usbrescan to retry\r\n");
        } else {
            self.hub_port_scan_failures = 0;
        }
    }

    /// Resets one already-known-occupied hub port, enumerates whatever is
    /// on it, and files it in the matching slot. Shared by the full
    /// `attach_hub` sweep and the incremental `scan_empty_hub_ports`, so
    /// that a device found later is set up identically to one that was
    /// present at rescan time -- routing included.
    fn attach_hub_port(&mut self, hub: &Hub, port: u8, hub_speed: Speed) -> bool {
        let Some((downstream, speed)) = self.enumerate_hub_port(hub, port, hub_speed) else {
            return false;
        };
        self.records[port as usize] = Some(DeviceRecord::from(
            Location::HubPort(port),
            speed,
            &downstream,
        ));

        match attach_class_driver(&downstream) {
            Some(kind) => {
                self.clear_unhandled_slot(port as usize);
                uart::log(match kind {
                    DeviceKind::Keyboard(_) => b"USB: keyboard attached on hub port " as &[u8],
                    DeviceKind::Mouse(_) => b"USB: mouse attached on hub port ",
                    DeviceKind::MassStorage(_) => b"USB: mass storage attached on hub port ",
                });
                uart::log_hex(b"", port as u32);
                self.slots[port as usize] = Some(kind);
                true
            }
            None => {
                self.report_unhandled_slot(port as usize, &downstream);
                false
            }
        }
    }

    /// Chooses HID's steady-state transfer path only after the current hub
    /// sweep has finished enumerating every occupied port. Starting a
    /// persistent periodic channel inside `attach_hub_port` changes the
    /// controller underneath the channel-0 control transfers still needed
    /// by later ports. Deferring this also makes initial and hot-plug scans
    /// use the same ordering.
    fn configure_new_hub_hid_slots(&mut self, new_slots: u16, hub_speed: Speed) {
        if new_slots == 0 {
            return;
        }

        let has_mass_storage = self
            .slots
            .iter()
            .flatten()
            .any(|slot| matches!(slot, DeviceKind::MassStorage(_)));
        let has_hid = self
            .slots
            .iter()
            .flatten()
            .any(|slot| matches!(slot, DeviceKind::Keyboard(_) | DeviceKind::Mouse(_)));
        if hub_speed == Speed::High {
            for (index, slot) in self.slots.iter().enumerate() {
                if new_slots & (1u16 << index) == 0 {
                    continue;
                }
                let Some(DeviceKind::Keyboard(keyboard)) = slot else {
                    continue;
                };
                if let Some(interval_ms) = keyboard.split_poll_interval_ms() {
                    uart::log_u32(
                        b"USB HID: Split foreground poll interval ms=",
                        interval_ms as u32,
                    );
                }
            }
        }
        if has_mass_storage && has_hid {
            self.serialize_hid_with_mass_storage();
            return;
        }

        // A Full-Speed hub has no Transaction Translator traffic, so its
        // HID endpoints may use descriptor-DMA periodic channels. A
        // High-Speed hub can mix these with Split transfers, whose
        // controller-wide DMA-mode arbitration remains Stage 5 work.
        if hub_speed == Speed::High {
            return;
        }
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if new_slots & (1u16 << index) == 0 {
                continue;
            }
            let Some(slot) = slot else { continue };
            if matches!(slot, DeviceKind::Keyboard(_) | DeviceKind::Mouse(_)) {
                log_periodic_result(enable_periodic_kind(slot));
            }
        }
    }

    /// Resets and enumerates one hub port, escalating to a port power cycle
    /// if the device does not answer.
    ///
    /// The root-level VBUS cycle cannot help here. A self-powered hub keeps
    /// its downstream ports live when USB-A's 5V goes away, so a device
    /// wedged behind one survives every root-level recovery with its state
    /// intact. Removing the hub port's own power is what a user does by
    /// unplugging the device, and it is the only equivalent software has.
    fn enumerate_hub_port(
        &mut self,
        hub: &Hub,
        port: u8,
        hub_speed: Speed,
    ) -> Option<(EnumeratedDevice, Speed)> {
        if let Some(device) = reset_and_enumerate_hub_port(hub, port, hub_speed) {
            return Some(device);
        }
        uart::log_hex(
            b"USB: enumeration failed for device on hub port ",
            port as u32,
        );

        if !self.may_power_cycle() || !hub.supports_per_port_power() {
            self.mark_unhandled_slot(port as usize);
            return None;
        }
        uart::log_hex(b"USB: power-cycling hub port ", port as u32);
        self.last_power_recovery_ms = Some(tick::now_ms());
        if !hub.power_cycle_port(port) {
            uart::log_hex(b"USB: hub port power cycle failed on port ", port as u32);
            self.mark_unhandled_slot(port as usize);
            return None;
        }
        if hub.debounce_connected_port(port) != Some(true) {
            self.mark_unhandled_slot(port as usize);
            return None;
        }
        let device = reset_and_enumerate_hub_port(hub, port, hub_speed);
        if device.is_none() {
            uart::log_hex(
                b"USB: still unreachable after a port power cycle; hub port ",
                port as u32,
            );
            self.mark_unhandled_slot(port as usize);
        }
        device
    }

    fn report_unhandled_slot(&mut self, slot_index: usize, device: &EnumeratedDevice) {
        let bit = 1u16 << slot_index;
        if self.unhandled_slots & bit != 0 {
            return;
        }
        self.mark_unhandled_slot(slot_index);
        if slot_index == 0 {
            uart::log(b"USB: no class driver for the device on USB-A\r\n");
        } else {
            uart::log_hex(
                b"USB: no class driver for device on hub port ",
                slot_index as u32,
            );
        }
        log_unhandled_interfaces(device);
    }

    /// Persistent HID DMA and MSC bulk DMA share the controller's RX FIFO.
    /// The real hardware can leave channel 0 timing out after dozens of
    /// otherwise successful reads while an idle periodic QTD remains armed.
    /// When both classes are present, keep all traffic serialized through
    /// the proven channel-0 path. HID state and DATA PID are preserved; this
    /// is not a device or bus reset.
    fn serialize_hid_with_mass_storage(&mut self) {
        let has_mass_storage = self
            .slots
            .iter()
            .flatten()
            .any(|slot| matches!(slot, DeviceKind::MassStorage(_)));
        let has_hid = self
            .slots
            .iter()
            .flatten()
            .any(|slot| matches!(slot, DeviceKind::Keyboard(_) | DeviceKind::Mouse(_)));
        if !has_mass_storage || !has_hid {
            return;
        }

        uart::log(b"USB: MSC present, serializing HID and bulk on channel 0\r\n");
        let all_halted = hcd::disable_periodic_hid();
        for slot in self.slots.iter_mut().flatten() {
            match slot {
                DeviceKind::Keyboard(keyboard) => keyboard.use_frame_poll(),
                DeviceKind::Mouse(mouse) => mouse.use_frame_poll(),
                DeviceKind::MassStorage(_) => {}
            }
        }
        if !all_halted {
            uart::log(b"USB: periodic channel did not halt; the bus needs re-enumeration\r\n");
            hcd::note_bus_unusable();
        }
    }

    fn clear_unhandled_slot(&mut self, slot_index: usize) {
        self.unhandled_slots &= ALL_SLOT_BITS ^ (1u16 << slot_index);
    }

    fn mark_unhandled_slot(&mut self, slot_index: usize) {
        self.unhandled_slots |= 1u16 << slot_index;
    }
}

/// Leaves enough descriptor evidence in the UART log to decide whether a
/// newly seen device needs a class driver or merely reported a different
/// subclass/transport than the intended one. Enumeration already fetched the
/// complete configuration descriptor, so this costs no additional USB traffic.
fn log_unhandled_interfaces(device: &EnumeratedDevice) {
    for raw in protocol::descriptors(device.config_bytes()) {
        if raw.descriptor_type != protocol::DESCRIPTOR_TYPE_INTERFACE {
            continue;
        }
        let Some(interface) = protocol::InterfaceDescriptor::parse(raw.bytes) else {
            continue;
        };
        let packed = u32::from_be_bytes([
            interface.number,
            interface.class,
            interface.subclass,
            interface.protocol,
        ]);
        uart::log_hex(
            b"USB: unhandled interface (number/class/subclass/protocol)=",
            packed,
        );
    }
}

/// Works out how the controller has to reach a device on a hub port, from
/// the two speeds involved.
///
/// A device slower than the hub it hangs off cannot be addressed directly:
/// the hub's Transaction Translator has to run the transaction on the
/// host's behalf, and the host reaches that TT with a split transaction
/// naming the hub and the port (`hcd::SplitTarget`). A device at the hub's
/// own speed needs nothing special, except that Low-Speed on a Full-Speed
/// bus still needs PRE tokens.
///
/// Both conditions can hold at once -- a Low-Speed keyboard behind a
/// High-Speed hub is split *and* Low-Speed -- which is why these are two
/// independent fields of `hcd::Route` rather than one enum.
///
/// `low_speed_via_hub` is required even under a split, despite the field's
/// PRE-token rationale suggesting it should only apply on a Full-Speed bus:
/// clearing it for split routes was tried on real hardware and the very
/// first SETUP came back STALL, so the core does need `HCCHAR.LSpdDev` to
/// describe the device at the far end of the TT.
fn route_behind_hub(hub_address: u8, port: u8, hub_speed: Speed, device_speed: Speed) -> Route {
    Route {
        low_speed_via_hub: device_speed == Speed::Low,
        split: if hub_speed == Speed::High && device_speed != Speed::High {
            Some(SplitTarget {
                hub_address,
                port_number: port,
            })
        } else {
            None
        },
    }
}

/// Tries every class driver this project has, in order, and returns the
/// first that accepts the device. Each `attach` is a no-op (no control
/// transfers at all) unless it actually finds its interface in `device`'s
/// configuration descriptor, so trying one that turns out not to match has
/// no side effect on the device.
///
/// A device exposing more than one of these interfaces -- a keyboard with
/// an integrated trackpad, or a wireless dongle presenting both -- is
/// driven by whichever comes first here, since a slot holds one driver.
/// Order therefore matters, and keyboard is first because it is the one
/// that also serves as the way out of every full-screen mode.
fn attach_class_driver(device: &EnumeratedDevice) -> Option<DeviceKind> {
    if let Some(keyboard) = UsbKeyboard::attach(device) {
        return Some(DeviceKind::Keyboard(keyboard));
    }
    if let Some(mouse) = UsbMouse::attach(device) {
        return Some(DeviceKind::Mouse(mouse));
    }
    if let Some(storage) = UsbMassStorage::attach(device) {
        return Some(DeviceKind::MassStorage(storage));
    }
    None
}

fn enable_periodic_kind(kind: &mut DeviceKind) -> Option<u8> {
    match kind {
        DeviceKind::Keyboard(keyboard) => keyboard.enable_periodic(),
        DeviceKind::Mouse(mouse) => mouse.enable_periodic(),
        DeviceKind::MassStorage(_) => None,
    }
}

fn log_periodic_result(channel: Option<u8>) {
    if let Some(channel) = channel {
        uart::log_hex(b"USB HID: periodic channel enabled: ", channel as u32);
    } else {
        uart::log(b"USB HID: periodic unavailable, using frame poll\r\n");
    }
}

fn milliseconds_since(start: u64) -> u32 {
    tick::now_ms().saturating_sub(start) as u32
}

/// One reset-and-enumerate attempt on a hub port, with no recovery of its
/// own -- `UsbHost::enumerate_hub_port` owns that decision.
fn reset_and_enumerate_hub_port(
    hub: &Hub,
    port: u8,
    hub_speed: Speed,
) -> Option<(EnumeratedDevice, Speed)> {
    let status = hub.reset_port(port)?;
    let address = protocol::downstream_address(port);
    let route = route_behind_hub(hub.device_address(), port, hub_speed, status.speed());
    if route.split.is_some() {
        // Worth a line: this is the path that was believed impossible
        // on this chip, and it is the first thing to look at if a
        // device behind a High-Speed hub misbehaves.
        uart::log(match status.speed() {
            Speed::Low => b"USB: Low-Speed device behind a High-Speed hub" as &[u8],
            Speed::Full => b"USB: Full-Speed device behind a High-Speed hub",
            _ => b"USB: slower device behind a High-Speed hub",
        });
        uart::log_hex(b", reached with split transactions; hub port ", port as u32);
    }
    protocol::enumerate_device(address, route).map(|device| (device, status.speed()))
}
