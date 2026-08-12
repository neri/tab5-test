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
use super::hub::{self, Hub};
use super::msc::UsbMassStorage;
use super::protocol::{self, EnumeratedDevice};
use crate::uart;

/// Hard cap on hub ports this registry tracks. Real hubs are almost always
/// 4-7 ports; USB2.0 allows up to 255. Bounds the fixed-size slot array the
/// same way `hub::MAX_PORTS` bounds the hub descriptor's removable-port
/// bitmap -- a hub reporting more is capped, with a log line, same as
/// there.
pub const MAX_HUB_PORTS: u8 = 8;

const SLOT_COUNT: usize = MAX_HUB_PORTS as usize + 1; // index 0 = Direct, N = HubPort(N)

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
/// has no driver for (a mouse, say) are noted in the UART log at attach
/// time and otherwise left out of the registry -- there is nothing to poll
/// or dispatch to.
pub enum DeviceKind {
    Keyboard(UsbKeyboard),
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
/// `lcd.rs` holds the only instance, across the lifetime of the frame
/// loop, and is the only thing that calls `rescan`. Every USB shell command
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
        self.slots.iter_mut().flatten().find_map(|slot| match &mut slot.kind {
            DeviceKind::MassStorage(storage) => Some(storage),
            DeviceKind::Keyboard(_) => None,
        })
    }

    /// Cheap liveness check (one HPRT read, no transaction): true once
    /// nothing is plugged into USB-A at all, in which case nothing behind
    /// it (hub or no hub) can still be there either. `lcd.rs` calls this
    /// every frame, the same spirit as `CardKb`'s bus-failure check, but
    /// for the whole registry at once rather than per device.
    pub fn root_disconnected(&self) -> bool {
        self.last_probe.is_some() && !hcd::port_connected()
    }

    /// Drops every slot and the hub handle without touching the bus --
    /// what `lcd.rs` calls once `root_disconnected` reports the cable
    /// itself came out.
    pub fn clear(&mut self) {
        self.hub = None;
        self.hub_speed = Speed::Unknown;
        self.hub_summary = None;
        for slot in self.slots.iter_mut() {
            *slot = None;
        }
    }

    /// True if nothing is currently registered at all -- no hub, no
    /// direct device, no hub-port device. Lets `lcd.rs` fire the
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
            DeviceKind::MassStorage(_) => false,
        })
    }

    /// True if there is room for another device to be picked up by the
    /// next `rescan`: an empty root slot with no hub attached, or (if a hub
    /// is attached) any hub port not currently holding a device. Drives
    /// `lcd.rs`'s coarse reconnect throttle, generalizing its old "no
    /// keyboard yet" check to the whole registry.
    pub fn has_room(&self) -> bool {
        match &self.hub {
            None => self.slots[0].is_none(),
            Some(hub) => {
                let port_count = hub.port_count().min(MAX_HUB_PORTS);
                (1..=port_count).any(|port| self.slots[port as usize].is_none())
            }
        }
    }

    /// Polls every attached keyboard slot in turn (root and hub ports
    /// alike) and returns the first newly-available key, mirroring
    /// `UsbKeyboard::poll`'s shape so `lcd.rs` can treat one keyboard or
    /// several the same way.
    pub fn poll_keyboards(&mut self) -> Option<u8> {
        for slot in self.slots.iter_mut().flatten() {
            if let DeviceKind::Keyboard(keyboard) = &mut slot.kind
                && let Some(byte) = keyboard.poll()
            {
                return Some(byte);
            }
        }
        None
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
        self.clear();

        let port = hcd::probe_port();
        self.last_probe = Some(port);
        if !port.enabled {
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
            self.slots[0] = Some(Slot { location: Location::Direct, summary: DeviceSummary::from(&device), kind });
        } else {
            uart::log(b"USB: no class driver for the device on USB-A\r\n");
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
                Some(false) => continue,
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
        // Taken out of `self` for the duration so the loop can borrow the
        // hub while filling in `self.slots`; nothing else can run in
        // between (this is all synchronous, single-threaded polling).
        let Some(hub) = self.hub.take() else { return };
        let hub_speed = self.hub_speed;

        let port_count = hub.port_count().min(MAX_HUB_PORTS);
        for port in 1..=port_count {
            if self.slots[port as usize].is_some() {
                continue; // already driving something here
            }
            match hub.debounce_connected_port(port) {
                Some(true) => {}
                Some(false) => continue,
                None => {
                    uart::log(b"USB: hub stopped answering while scanning ports\r\n");
                    break;
                }
            }
            self.attach_hub_port(&hub, port, hub_speed);
        }

        self.hub = Some(hub);
    }

    /// Resets one already-known-occupied hub port, enumerates whatever is
    /// on it, and files it in the matching slot. Shared by the full
    /// `attach_hub` sweep and the incremental `scan_empty_hub_ports`, so
    /// that a device found later is set up identically to one that was
    /// present at rescan time -- routing included.
    fn attach_hub_port(&mut self, hub: &Hub, port: u8, hub_speed: Speed) {
        let Some(status) = hub.reset_port(port) else { return };
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
            uart::log_hex(b"USB: enumeration failed for device on hub port ", port as u32);
            return;
        };

        match attach_class_driver(&downstream) {
            Some(kind) => {
                uart::log(match kind {
                    DeviceKind::Keyboard(_) => b"USB: keyboard attached on hub port " as &[u8],
                    DeviceKind::MassStorage(_) => b"USB: mass storage attached on hub port ",
                });
                uart::log_hex(b"", port as u32);
                self.slots[port as usize] = Some(Slot {
                    location: Location::HubPort(port),
                    summary: DeviceSummary::from(&downstream),
                    kind,
                });
            }
            None => uart::log_hex(b"USB: no class driver for device on hub port ", port as u32),
        }
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
fn route_behind_hub(
    hub_address: u8,
    port: u8,
    hub_speed: Speed,
    device_speed: Speed,
) -> Route {
    Route {
        low_speed_via_hub: device_speed == Speed::Low,
        split: if hub_speed == Speed::High && device_speed != Speed::High {
            Some(SplitTarget { hub_address, port_number: port })
        } else {
            None
        },
    }
}

/// Tries every class driver this project has, in order, and returns the
/// first that accepts the device. Both `UsbKeyboard::attach` and
/// `UsbMassStorage::attach` are no-ops (no control transfers at all) unless
/// they actually find their interface in `device`'s configuration
/// descriptor, so trying one that turns out not to match has no side
/// effect on the device.
fn attach_class_driver(device: &EnumeratedDevice) -> Option<DeviceKind> {
    if let Some(keyboard) = UsbKeyboard::attach(device) {
        return Some(DeviceKind::Keyboard(keyboard));
    }
    if let Some(storage) = UsbMassStorage::attach(device) {
        return Some(DeviceKind::MassStorage(storage));
    }
    None
}
