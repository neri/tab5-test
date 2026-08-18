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

use super::hcd::{self, HostPort, Route, Speed, SplitTarget};
use super::hid_keyboard::UsbKeyboard;
use super::hid_mouse::{MouseUpdate, UsbMouse};
use super::hub::{self, Hub};
use super::msc::UsbMassStorage;
use super::protocol::{self, EnumeratedDevice};
use crate::input::Key;
use crate::uart;

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
/// has no driver for are noted in the UART log at attach time and otherwise
/// left out of the registry -- there is nothing to poll or dispatch to.
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

struct Slot {
    location: Location,
    summary: DeviceSummary,
    kind: DeviceKind,
}

/// A read-only view of one attached device, for shell commands that just
/// want to list what is plugged in.
pub struct AttachedDevice<'a> {
    pub location: Location,
    pub summary: &'a DeviceSummary,
    pub kind: &'a DeviceKind,
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
    /// The hub's own VID/PID/class, captured at attach time -- the hub
    /// occupies the root port but (unlike a direct device) is tracked
    /// separately from `slots`, since it is not itself something a class
    /// driver drives.
    hub_summary: Option<DeviceSummary>,
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
    /// Slots at which an enumerated device had no usable class driver. The
    /// background scanner must keep probing those slots for future support,
    /// but reports each unsupported physical attachment only once rather
    /// than filling the UART log every scan interval.
    unhandled_slots: u16,
    slots: [Option<Slot>; SLOT_COUNT],
}

impl UsbHost {
    pub const fn new() -> Self {
        const NONE_SLOT: Option<Slot> = None;
        Self {
            last_probe: None,
            hub: None,
            hub_speed: Speed::Unknown,
            hub_summary: None,
            hub_port_scan_paused: false,
            hub_port_scan_failures: 0,
            next_keyboard_slot: 0,
            unhandled_slots: 0,
            slots: [NONE_SLOT; SLOT_COUNT],
        }
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

    /// The hub's own VID/PID/class, if one is attached.
    pub fn hub_summary(&self) -> Option<&DeviceSummary> {
        self.hub_summary.as_ref()
    }

    /// Every currently attached device, root or hub port alike, in slot
    /// order (`Direct` first, then hub ports low to high).
    pub fn attached_devices(&self) -> impl Iterator<Item = AttachedDevice<'_>> {
        self.slots.iter().flatten().map(|slot| AttachedDevice {
            location: slot.location,
            summary: &slot.summary,
            kind: &slot.kind,
        })
    }

    /// The first attached Mass Storage device, if any -- `usbmsc`/
    /// `usbread`/`usbmbr` no longer enumerate their own device fresh on
    /// every call; they share whatever `rescan` already attached, wherever
    /// it is (USB-A directly or a hub port). `docs/USB_REFACTOR_PLAN.md` Stage F.
    pub fn mass_storage_mut(&mut self) -> Option<&mut UsbMassStorage> {
        self.slots
            .iter_mut()
            .flatten()
            .find_map(|slot| match &mut slot.kind {
                DeviceKind::MassStorage(storage) => Some(storage),
                DeviceKind::Keyboard(_) | DeviceKind::Mouse(_) => None,
            })
    }

    /// True if any attached device is a HID Boot mouse. Lets a pointer-driven
    /// screen say so up front instead of leaving the user to guess why the
    /// cursor never moves.
    pub fn has_mouse(&self) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|slot| matches!(slot.kind, DeviceKind::Mouse(_)))
    }

    /// Cheap liveness check (one HPRT read, no transaction): true once
    /// nothing is plugged into USB-A at all, in which case nothing behind
    /// it (hub or no hub) can still be there either. `InputManager` calls this
    /// every frame, the same spirit as `CardKb`'s bus-failure check, but
    /// for the whole registry at once rather than per device.
    pub fn root_disconnected(&self) -> bool {
        self.last_probe.is_some() && !hcd::port_connected()
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
        self.hub = None;
        self.hub_speed = Speed::Unknown;
        self.hub_summary = None;
        self.hub_port_scan_paused = false;
        self.hub_port_scan_failures = 0;
        self.next_keyboard_slot = 0;
        for slot in self.slots.iter_mut() {
            *slot = None;
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
    /// Mass Storage slots have no per-frame polling session to go stale
    /// between shell commands, so they do not contribute here; a stale MSC
    /// handle just fails its next command and is only cleared by the next
    /// `rescan` (see `docs/USB_REFACTOR_PLAN.md`'s notes on this gap).
    pub fn needs_reinit(&self) -> bool {
        self.slots.iter().flatten().any(|slot| match &slot.kind {
            DeviceKind::Keyboard(keyboard) => keyboard.needs_reinit(),
            DeviceKind::Mouse(mouse) => mouse.needs_reinit(),
            DeviceKind::MassStorage(_) => false,
        })
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
            if let DeviceKind::Keyboard(keyboard) = &mut slot.kind
                && let Some(byte) = keyboard.poll()
            {
                self.next_keyboard_slot = (index + 1) % SLOT_COUNT;
                return Some(byte);
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
            let DeviceKind::Mouse(mouse) = &mut slot.kind else {
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
        self.clear_registry();

        let port = hcd::probe_port();
        self.last_probe = Some(port);
        if !port.enabled {
            if !port.connected {
                self.clear_unhandled_slot(0);
            }
            return;
        }

        // Nothing plugged into USB-A directly ever needs preambles or
        // splits: the bus itself runs at the device's speed.
        let Some(device) =
            protocol::enumerate_device(protocol::ROOT_DEVICE_ADDRESS, Route::default())
        else {
            uart::log(b"USB: root device enumeration failed\r\n");
            return;
        };

        if device.device_class == hub::DEVICE_CLASS_HUB {
            self.attach_hub(&device, port.speed);
        } else if let Some(kind) = attach_class_driver(&device) {
            self.clear_unhandled_slot(0);
            self.slots[0] = Some(Slot {
                location: Location::Direct,
                summary: DeviceSummary::from(&device),
                kind,
            });
        } else {
            self.report_unhandled_slot(0, &device);
        }
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
    fn attach_hub(&mut self, device: &EnumeratedDevice, hub_speed: Speed) {
        let Some(hub) = Hub::open(device) else { return };
        self.hub_summary = Some(DeviceSummary::from(device));
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
            return;
        }

        let port_count = hub.port_count().min(MAX_HUB_PORTS);
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
            self.attach_hub_port(&hub, port, hub_speed);
        }

        self.hub = Some(hub);
        self.hub_speed = hub_speed;
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

        let port_count = hub.port_count().min(MAX_HUB_PORTS);
        for port in 1..=port_count {
            if self.slots[port as usize].is_some() {
                continue; // already driving something here
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
            self.attach_hub_port(&hub, port, hub_speed);
        }

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
    fn attach_hub_port(&mut self, hub: &Hub, port: u8, hub_speed: Speed) {
        let Some(status) = hub.reset_port(port) else {
            return;
        };
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
        let Some(downstream) = protocol::enumerate_device(address, route) else {
            uart::log_hex(
                b"USB: enumeration failed for device on hub port ",
                port as u32,
            );
            return;
        };

        match attach_class_driver(&downstream) {
            Some(kind) => {
                self.clear_unhandled_slot(port as usize);
                uart::log(match kind {
                    DeviceKind::Keyboard(_) => b"USB: keyboard attached on hub port " as &[u8],
                    DeviceKind::Mouse(_) => b"USB: mouse attached on hub port ",
                    DeviceKind::MassStorage(_) => b"USB: mass storage attached on hub port ",
                });
                uart::log_hex(b"", port as u32);
                self.slots[port as usize] = Some(Slot {
                    location: Location::HubPort(port),
                    summary: DeviceSummary::from(&downstream),
                    kind,
                });
            }
            None => self.report_unhandled_slot(port as usize, &downstream),
        }
    }

    fn report_unhandled_slot(&mut self, slot_index: usize, device: &EnumeratedDevice) {
        let bit = 1u16 << slot_index;
        if self.unhandled_slots & bit != 0 {
            return;
        }
        self.unhandled_slots |= bit;
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

    fn clear_unhandled_slot(&mut self, slot_index: usize) {
        self.unhandled_slots &= ALL_SLOT_BITS ^ (1u16 << slot_index);
    }
}

/// Leaves enough descriptor evidence in the UART log to decide whether a
/// newly seen device needs a class driver or merely reported a different
/// subclass/transport than the intended one. Enumeration already fetched the
/// complete configuration descriptor, so this costs no additional USB traffic.
fn log_unhandled_interfaces(device: &EnumeratedDevice) {
    let config = device.config_bytes();
    let mut offset = 0usize;
    while offset + 2 <= config.len() {
        let length = config[offset] as usize;
        if length < 2 || offset + length > config.len() {
            break;
        }
        if config[offset + 1] == protocol::DESCRIPTOR_TYPE_INTERFACE && length >= 9 {
            let descriptor = u32::from_be_bytes([
                config[offset + 2],
                config[offset + 5],
                config[offset + 6],
                config[offset + 7],
            ]);
            uart::log_hex(
                b"USB: unhandled interface (number/class/subclass/protocol)=",
                descriptor,
            );
        }
        offset += length;
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
