//! USB hub class driver (USB2.0 chapter 11), built on `protocol.rs`'s
//! control transfers -- the hub-side counterpart to `hid_keyboard.rs`, and
//! the only module that knows what a downstream port is.
//!
//! This is Stage 4 of `docs/USB_HOST_PLAN.md`, generalized to every port by
//! `docs/USB_REFACTOR_PLAN.md` Stage C: `usb::registry::UsbHost` walks every
//! port with `debounce_connected_port`/`reset_port` rather than driving
//! just one. Nothing in this file depends on the speed the hub or its
//! devices came up at -- hub port management is identical at every speed,
//! and combining a High-Speed hub with slower devices behind it is
//! `hcd::Route`'s business. Hubs behind hubs are still out of scope.

use super::hcd::{self, Speed};
use super::protocol::{self, ControlPipe, EnumeratedDevice, REQUEST_SET_CONFIGURATION};
use crate::delay::delay_ms;
use crate::uart;

/// `bDeviceClass` of a hub (USB2.0 table 11-8). Hubs report their class at
/// the device level, not just per-interface.
pub const DEVICE_CLASS_HUB: u8 = 9;

// Hub class descriptor and requests (USB2.0 section 11.24).
const DESCRIPTOR_TYPE_HUB: u8 = 0x29;
const REQUEST_GET_STATUS: u8 = 0x00;
const REQUEST_CLEAR_FEATURE: u8 = 0x01;
const REQUEST_SET_FEATURE: u8 = 0x03;
const REQUEST_GET_DESCRIPTOR: u8 = 0x06;
// bmRequestType: device-to-host, class, device recipient.
const REQUEST_TYPE_DEVICE_TO_HOST_CLASS_DEVICE: u8 = 0xA0;
// Port requests use the "other" recipient, with the port number in wIndex.
const REQUEST_TYPE_DEVICE_TO_HOST_CLASS_OTHER: u8 = 0xA3;
const REQUEST_TYPE_HOST_TO_DEVICE_CLASS_OTHER: u8 = 0x23;

// Port feature selectors (USB2.0 table 11-17). Only the ones this driver
// actually sets or clears are listed.
const FEATURE_PORT_RESET: u16 = 4;
const FEATURE_PORT_POWER: u16 = 8;
const FEATURE_C_PORT_CONNECTION: u16 = 16;
const FEATURE_C_PORT_RESET: u16 = 20;

// wPortStatus / wPortChange bits (USB2.0 tables 11-21 and 11-22).
const PORT_STATUS_CONNECTION: u16 = 1 << 0;
const PORT_STATUS_ENABLE: u16 = 1 << 1;
const PORT_STATUS_SUSPEND: u16 = 1 << 2;
const PORT_STATUS_OVER_CURRENT: u16 = 1 << 3;
const PORT_STATUS_RESET: u16 = 1 << 4;
const PORT_STATUS_POWER: u16 = 1 << 8;
const PORT_STATUS_LOW_SPEED: u16 = 1 << 9;
const PORT_STATUS_HIGH_SPEED: u16 = 1 << 10;
const PORT_CHANGE_RESET: u16 = 1 << 4;

/// Extra settling time on top of the hub's own `bPwrOn2PwrGood`, in the
/// same spirit as `hcd.rs` padding every USB2.0 minimum rather than
/// sitting exactly on it.
const POWER_GOOD_MARGIN_MS: u16 = 50;
/// Gap between consecutive `SET_FEATURE(PORT_POWER)` requests. Nothing in
/// USB2.0 requires it, but powering a bus-powered hub's ports one at a
/// time with a pause spreads the inrush instead of stacking it.
const PORT_POWER_INTERVAL_MS: u32 = 20;
/// USB2.0 TATTDB is 100ms; `hcd.rs` uses 250ms for the root port
/// (ESP-IDF's default) and there is no reason for a hub port to be less
/// tolerant.
const PORT_DEBOUNCE_MS: u32 = 250;
/// The hub drives reset for 10-20ms on its own and then sets
/// `C_PORT_RESET`; this only bounds how long to wait for that.
const PORT_RESET_TIMEOUT_MS: u32 = 500;
/// USB2.0 TRSTRCY is 10ms, padded to match `hcd::RESET_RECOVERY_MS`.
const PORT_RESET_RECOVERY_MS: u32 = 30;
const PORT_STATUS_POLL_INTERVAL_MS: u32 = 5;

// The hub descriptor's fixed part (USB2.0 table 11-13): bDescLength,
// bDescriptorType, bNbrPorts, wHubCharacteristics, bPwrOn2PwrGood,
// bHubContrCurrent. The DeviceRemovable and PortPwrCtrlMask bitmaps that
// follow are sized from bNbrPorts, so the length has to be read first.
const DESCRIPTOR_FIXED_LEN: usize = 7;
const DESCRIPTOR_BUFFER_MAX: usize = 16;

/// USB2.0 allows up to 255 ports; real hubs have at most 7, and this
/// project only ever drives one of them. Capping keeps the descriptor
/// buffer and the `device_removable` bitmap fixed-size.
const MAX_PORTS: u8 = 31;

/// `bPwrOn2PwrGood` counts 2ms intervals (USB2.0 table 11-13).
const POWER_GOOD_UNIT_MS: u16 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PowerSwitching {
    /// All ports switch together.
    Ganged,
    PerPort,
    /// Ports are powered whenever the hub is, and `SET_FEATURE(PORT_POWER)`
    /// is accepted but does nothing.
    AlwaysOn,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OverCurrentProtection {
    /// One detector for the whole hub.
    Global,
    PerPort,
    Unsupported,
}

pub struct HubDescriptor {
    pub port_count: u8,
    pub characteristics: u16,
    /// Already converted from `bPwrOn2PwrGood`'s 2ms units: how long to
    /// wait after powering a port before its connect status means
    /// anything.
    pub power_on_to_power_good_ms: u16,
    /// `bHubContrCurrent`: the hub controller's own draw, in mA.
    pub control_current_ma: u8,
    /// `DeviceRemovable` as a bitmap, bit N = port N (bit 0 describes the
    /// hub itself and is reserved). Ports above `MAX_PORTS` are not
    /// represented.
    pub device_removable: u32,
    pub descriptor_len: u8,
}

impl HubDescriptor {
    pub fn power_switching(&self) -> PowerSwitching {
        match self.characteristics & 0x3 {
            0 => PowerSwitching::Ganged,
            1 => PowerSwitching::PerPort,
            _ => PowerSwitching::AlwaysOn,
        }
    }

    pub fn over_current_protection(&self) -> OverCurrentProtection {
        match (self.characteristics >> 3) & 0x3 {
            0 => OverCurrentProtection::Global,
            1 => OverCurrentProtection::PerPort,
            _ => OverCurrentProtection::Unsupported,
        }
    }

    /// True if the hub is part of a compound device (a keyboard with a
    /// built-in hub, say) rather than a standalone one.
    pub fn is_compound_device(&self) -> bool {
        self.characteristics & (1 << 2) != 0
    }

    /// Transaction-translator think time in Full-Speed bit times. Only
    /// meaningful for a hub operating at High-Speed, which is now the
    /// normal case -- that TT is what carries every transaction to a
    /// slower device behind it (`hcd::Route`). Still informational: this
    /// driver waits for the TT by retrying the complete split rather than
    /// by scheduling around a known think time.
    pub fn tt_think_time_bits(&self) -> u16 {
        (((self.characteristics >> 5) & 0x3) + 1) * 8
    }

    pub fn has_port_indicators(&self) -> bool {
        self.characteristics & (1 << 7) != 0
    }

    /// Whether a device on `port` can be unplugged. Permanently attached
    /// ports (compound devices) report `false`.
    pub fn port_is_removable(&self, port: u8) -> bool {
        port <= MAX_PORTS && self.device_removable & (1 << port) == 0
    }
}

pub struct HubStatus {
    pub status: u16,
    pub change: u16,
}

impl HubStatus {
    /// USB2.0 table 11-19: the bit is set when local power is *lost*, so a
    /// bus-powered-only hub reads back true here.
    pub fn local_power_lost(&self) -> bool {
        self.status & (1 << 0) != 0
    }

    pub fn over_current(&self) -> bool {
        self.status & (1 << 1) != 0
    }

    pub fn local_power_changed(&self) -> bool {
        self.change & (1 << 0) != 0
    }

    pub fn over_current_changed(&self) -> bool {
        self.change & (1 << 1) != 0
    }
}

/// One downstream port's `wPortStatus`/`wPortChange` pair (USB2.0 tables
/// 11-21 and 11-22), the hub-port counterpart to `hcd`'s `HPRT` read.
pub struct PortStatus {
    pub status: u16,
    pub change: u16,
}

impl PortStatus {
    pub fn connected(&self) -> bool {
        self.status & PORT_STATUS_CONNECTION != 0
    }

    pub fn enabled(&self) -> bool {
        self.status & PORT_STATUS_ENABLE != 0
    }

    pub fn suspended(&self) -> bool {
        self.status & PORT_STATUS_SUSPEND != 0
    }

    pub fn over_current(&self) -> bool {
        self.status & PORT_STATUS_OVER_CURRENT != 0
    }

    pub fn in_reset(&self) -> bool {
        self.status & PORT_STATUS_RESET != 0
    }

    pub fn powered(&self) -> bool {
        self.status & PORT_STATUS_POWER != 0
    }

    /// The attached device's speed, as the hub determined it during reset.
    /// Compared against the hub's own speed by
    /// `usb::registry::UsbHost::attach_hub` to decide whether the device
    /// needs a split transaction to reach.
    pub fn speed(&self) -> Speed {
        if self.status & PORT_STATUS_HIGH_SPEED != 0 {
            Speed::High
        } else if self.status & PORT_STATUS_LOW_SPEED != 0 {
            Speed::Low
        } else {
            Speed::Full
        }
    }

    pub fn reset_changed(&self) -> bool {
        self.change & PORT_CHANGE_RESET != 0
    }
}

pub struct Hub {
    pipe: ControlPipe,
    pub descriptor: HubDescriptor,
}

impl Hub {
    /// Takes an already-enumerated device (`protocol::enumerate_device`),
    /// confirms it really is a hub, configures it, and reads its class
    /// descriptor.
    ///
    /// `SET_CONFIGURATION` happens here, as in `UsbKeyboard::init`: a
    /// device in the Address state is only required to answer *standard*
    /// requests, so every hub class request -- the descriptor read below
    /// included -- needs the device configured first.
    pub fn open(device: &EnumeratedDevice) -> Option<Self> {
        if device.device_class != DEVICE_CLASS_HUB {
            uart::log(b"USB: attached device is not a hub\r\n");
            return None;
        }
        let pipe = device.control_pipe();

        let setup = protocol::build_standard_out_setup(
            REQUEST_SET_CONFIGURATION,
            device.configuration_value as u16,
            0,
        );
        if !protocol::control_transfer_out_no_data(&pipe, &setup) {
            uart::log(b"USB: hub SET_CONFIGURATION failed\r\n");
            return None;
        }

        let descriptor = read_descriptor(&pipe)?;
        Some(Self { pipe, descriptor })
    }

    /// The hub's own USB device address, which is what `HCSPLT.HubAddr`
    /// needs to name it as the Transaction Translator for a slower device
    /// behind one of its ports (`hcd::SplitTarget`).
    pub fn device_address(&self) -> u8 {
        self.pipe.device_address
    }

    /// The hub's own status and change bits (`GET_STATUS` addressed to the
    /// hub itself, not to one of its ports).
    pub fn status(&self) -> Option<HubStatus> {
        let mut buffer = [0u8; 4];
        let setup = build_class_in_setup(
            REQUEST_TYPE_DEVICE_TO_HOST_CLASS_DEVICE,
            REQUEST_GET_STATUS,
            0,
            0,
            buffer.len() as u16,
        );
        let received =
            protocol::control_transfer_in(&self.pipe, &setup, &mut buffer)?;
        if received < buffer.len() {
            uart::log(b"USB: short hub GET_STATUS response\r\n");
            return None;
        }
        Some(HubStatus {
            status: u16::from_le_bytes([buffer[0], buffer[1]]),
            change: u16::from_le_bytes([buffer[2], buffer[3]]),
        })
    }

    pub fn port_count(&self) -> u8 {
        self.descriptor.port_count
    }

    /// One downstream port's status (`GET_STATUS` with the port number in
    /// wIndex).
    pub fn port_status(&self, port: u8) -> Option<PortStatus> {
        self.port_status_with_diagnostics(port, true)
    }

    /// Quiet form of [`Self::port_status`] for the periodic empty-port scan.
    /// A failed scan must not reset devices that are already working, so its
    /// caller emits a single summary and pauses future background scans.
    fn port_status_quiet(&self, port: u8) -> Option<PortStatus> {
        self.port_status_with_diagnostics(port, false)
    }

    fn port_status_with_diagnostics(&self, port: u8, log_failure: bool) -> Option<PortStatus> {
        let mut buffer = [0u8; 4];
        let setup = build_class_in_setup(
            REQUEST_TYPE_DEVICE_TO_HOST_CLASS_OTHER,
            REQUEST_GET_STATUS,
            0,
            port as u16,
            buffer.len() as u16,
        );
        let mut received = if log_failure {
            protocol::control_transfer_in(&self.pipe, &setup, &mut buffer)
        } else {
            protocol::control_transfer_in_quiet(&self.pipe, &setup, &mut buffer)
        };
        if received.is_none() && !log_failure {
            // A hub can be left with its EP0 state needing a fresh SETUP
            // after delayed downstream/TT work. The post-failure probe
            // showed that a harmless hub-recipient GET_STATUS, even when it
            // itself reports an error, restores the following port request.
            // Keep this recovery entirely quiet and retry the original
            // request from a fresh control-transfer SETUP.
            hcd::recover_channel_after_packet_failure();
            self.reprime_control_pipe_quiet();
            received = protocol::control_transfer_in_quiet(&self.pipe, &setup, &mut buffer);
        }
        let received = received?;
        if received < buffer.len() {
            if log_failure {
                uart::log(b"USB: short hub GET_PORT_STATUS response\r\n");
            }
            return None;
        }
        Some(PortStatus {
            status: u16::from_le_bytes([buffer[0], buffer[1]]),
            change: u16::from_le_bytes([buffer[2], buffer[3]]),
        })
    }

    fn reprime_control_pipe_quiet(&self) {
        let mut buffer = [0u8; 4];
        let setup = build_class_in_setup(
            REQUEST_TYPE_DEVICE_TO_HOST_CLASS_DEVICE,
            REQUEST_GET_STATUS,
            0,
            0,
            buffer.len() as u16,
        );
        let _ = protocol::control_transfer_in_quiet(&self.pipe, &setup, &mut buffer);
    }

    /// Separates a failed hub-port request from a failed hub control endpoint.
    ///
    /// The registry calls this only after it has logged the original failure
    /// and paused background scanning. Both reads use the quiet recovery
    /// path, so this adds two result lines without turning a persistent
    /// failure into a stream of packet diagnostics.
    #[cfg(any())]
    pub fn log_control_probe_after_failure(&self, port: u8) {
        uart::log_hex(b"USB: post-failure hub EP0 probe, failed port=", port as u32);

        let mut hub_buffer = [0u8; 4];
        let hub_setup = build_class_in_setup(
            REQUEST_TYPE_DEVICE_TO_HOST_CLASS_DEVICE,
            REQUEST_GET_STATUS,
            0,
            0,
            hub_buffer.len() as u16,
        );
        let hub_received =
            protocol::control_transfer_in_quiet(&self.pipe, &hub_setup, &mut hub_buffer);
        Self::log_control_probe_result(b"USB:   GET_STATUS(hub)=", hub_received, hub_buffer.len());

        let mut port_buffer = [0u8; 4];
        let port_setup = build_class_in_setup(
            REQUEST_TYPE_DEVICE_TO_HOST_CLASS_OTHER,
            REQUEST_GET_STATUS,
            0,
            port as u16,
            port_buffer.len() as u16,
        );
        let port_received =
            protocol::control_transfer_in_quiet(&self.pipe, &port_setup, &mut port_buffer);
        Self::log_control_probe_result(
            b"USB:   GET_PORT_STATUS(failed port)=",
            port_received,
            port_buffer.len(),
        );
    }

    #[cfg(any())]
    fn log_control_probe_result(label: &[u8], received: Option<usize>, expected: usize) {
        uart::log(label);
        match received {
            Some(bytes) if bytes >= expected => uart::log(b"ok\r\n"),
            Some(bytes) => {
                uart::log(b"short response, bytes=");
                uart::log_hex(b"", bytes as u32);
            }
            None => uart::log(b"failed\r\n"),
        }
    }

    pub fn set_port_feature(&self, port: u8, feature: u16) -> bool {
        self.port_feature_request(REQUEST_SET_FEATURE, port, feature)
    }

    pub fn clear_port_feature(&self, port: u8, feature: u16) -> bool {
        self.port_feature_request(REQUEST_CLEAR_FEATURE, port, feature)
    }

    fn port_feature_request(&self, request: u8, port: u8, feature: u16) -> bool {
        let setup = [
            REQUEST_TYPE_HOST_TO_DEVICE_CLASS_OTHER,
            request,
            (feature & 0xFF) as u8,
            (feature >> 8) as u8,
            port,
            0,
            0,
            0,
        ];
        protocol::control_transfer_out_no_data(&self.pipe, &setup)
    }

    /// Raises `PORT_POWER` on every port and waits out the hub's own
    /// power-on-to-power-good time, after which a port's connect status
    /// means something.
    ///
    /// All ports are powered rather than just the one that ends up being
    /// used, because the port to use is chosen *from* the connect status
    /// that only appears once a port is powered. A hub whose
    /// `power_switching` is `AlwaysOn` accepts the request and ignores it,
    /// which is fine -- its ports are already live.
    pub fn power_on_all_ports(&self) -> bool {
        for port in 1..=self.descriptor.port_count {
            if !self.set_port_feature(port, FEATURE_PORT_POWER) {
                // Stop rather than carry on: every request goes to the hub
                // itself, so one failing means the hub is no longer
                // answering and the remaining ports would each burn
                // another full control-transfer timeout to say so.
                uart::log_hex(b"USB: hub SET_FEATURE(PORT_POWER) failed on port ", port as u32);
                return false;
            }
            // Let each port's power switch settle before hitting the next
            // one, instead of asking a bus-powered hub to bring all of
            // them up back-to-back.
            delay_ms(PORT_POWER_INTERVAL_MS);
        }
        delay_ms((self.descriptor.power_on_to_power_good_ms + POWER_GOOD_MARGIN_MS) as u32);
        true
    }

    /// Checks one port for a connected device and, if there is one, lets it
    /// settle before confirming it is still there -- the per-port building
    /// block `usb::registry::UsbHost` uses to attach every occupied port
    /// instead of picking a single one (`docs/USB_REFACTOR_PLAN.md` Stage C).
    ///
    /// Returns `Some(true)` if a device is connected and stayed connected
    /// through debounce, `Some(false)` if the port is simply empty (no
    /// debounce wait spent), and `None` if the hub itself stopped
    /// answering -- which the caller should treat as "stop scanning
    /// further ports", not "keep trying this one", the same way
    /// `port_status`'s other callers already do.
    ///
    /// Ports are assumed to be powered already (`power_on_all_ports`).
    pub fn debounce_connected_port(&self, port: u8) -> Option<bool> {
        self.debounce_connected_port_with_diagnostics(port, true)
    }

    /// Quiet form of [`Self::debounce_connected_port`] for background scans
    /// that must leave already-attached devices alone on an error. The
    /// registry defers a failed scan to its next coarse polling interval;
    /// this method itself keeps `protocol.rs`'s normal whole-transfer retry.
    pub fn debounce_connected_port_quiet(&self, port: u8) -> Option<bool> {
        self.debounce_connected_port_with_diagnostics(port, false)
    }

    fn debounce_connected_port_with_diagnostics(&self, port: u8, log_failure: bool) -> Option<bool> {
        let first_status = if log_failure {
            self.port_status(port)
        } else {
            self.port_status_quiet(port)
        };
        if !first_status?.connected() {
            return Some(false);
        }
        // Mirrors `hcd::probe_port`'s debounce (including its "bounced
        // away" case) on the root port.
        delay_ms(PORT_DEBOUNCE_MS);
        let second_status = if log_failure {
            self.port_status(port)
        } else {
            self.port_status_quiet(port)
        };
        if !second_status?.connected() {
            if log_failure {
                uart::log_hex(b"USB: hub port connection bounced away during debounce, port ", port as u32);
            }
            return Some(false);
        }
        self.clear_port_feature(port, FEATURE_C_PORT_CONNECTION);
        Some(true)
    }

    /// Resets one port and reports the status it came up with, which is
    /// where the attached device's speed finally becomes known (the hub
    /// determines it during reset, exactly as the root port does).
    ///
    /// Returns `None` only if the reset never completed or left the port
    /// disabled. Every speed the port can report is now usable: one slower
    /// than the hub's own is reached through its Transaction Translator
    /// (`hcd::Route`'s `split`), which is why this no longer refuses a
    /// High-Speed port the way it did while the bus was pinned to
    /// Full-Speed.
    pub fn reset_port(&self, port: u8) -> Option<PortStatus> {
        if !self.set_port_feature(port, FEATURE_PORT_RESET) {
            uart::log(b"USB: hub SET_FEATURE(PORT_RESET) failed\r\n");
            return None;
        }

        let mut waited_ms = 0;
        loop {
            let status = self.port_status(port)?;
            // The hub signals completion by setting C_PORT_RESET and
            // dropping PORT_RESET; either alone is enough to stop waiting.
            if status.reset_changed() || !status.in_reset() {
                break;
            }
            if waited_ms >= PORT_RESET_TIMEOUT_MS {
                uart::log(b"USB: hub port reset did not complete\r\n");
                return None;
            }
            delay_ms(PORT_STATUS_POLL_INTERVAL_MS);
            waited_ms += PORT_STATUS_POLL_INTERVAL_MS;
        }

        self.clear_port_feature(port, FEATURE_C_PORT_RESET);
        delay_ms(PORT_RESET_RECOVERY_MS);

        // Re-read rather than reusing the status that ended the loop: the
        // speed and enable bits are only final once reset recovery is
        // over.
        let status = self.port_status(port)?;
        if !status.enabled() {
            uart::log(b"USB: hub port reset completed but the port did not enable\r\n");
            return None;
        }
        Some(status)
    }
}

/// Reads the hub class descriptor in two passes: the fixed part first (the
/// only way to learn `bDescLength`, since the trailing bitmaps are sized by
/// the port count), then the whole thing. Same shape as `protocol.rs`'s
/// configuration descriptor header-then-full read.
fn read_descriptor(pipe: &ControlPipe) -> Option<HubDescriptor> {
    let mut header = [0u8; DESCRIPTOR_FIXED_LEN];
    let setup = build_class_in_setup(
        REQUEST_TYPE_DEVICE_TO_HOST_CLASS_DEVICE,
        REQUEST_GET_DESCRIPTOR,
        (DESCRIPTOR_TYPE_HUB as u16) << 8,
        0,
        header.len() as u16,
    );
    if protocol::control_transfer_in(pipe, &setup, &mut header)? < header.len() {
        uart::log(b"USB: short hub descriptor header\r\n");
        return None;
    }
    if header[1] != DESCRIPTOR_TYPE_HUB {
        uart::log_hex(b"USB: unexpected hub descriptor type=", header[1] as u32);
        return None;
    }
    let total_length = (header[0] as usize).clamp(DESCRIPTOR_FIXED_LEN, DESCRIPTOR_BUFFER_MAX);

    let mut bytes = [0u8; DESCRIPTOR_BUFFER_MAX];
    let setup = build_class_in_setup(
        REQUEST_TYPE_DEVICE_TO_HOST_CLASS_DEVICE,
        REQUEST_GET_DESCRIPTOR,
        (DESCRIPTOR_TYPE_HUB as u16) << 8,
        0,
        total_length as u16,
    );
    let received =
        protocol::control_transfer_in(pipe, &setup, &mut bytes[..total_length])?;
    if received < DESCRIPTOR_FIXED_LEN {
        uart::log(b"USB: short hub descriptor\r\n");
        return None;
    }

    let mut port_count = bytes[2];
    if port_count > MAX_PORTS {
        uart::log_hex(b"USB: hub reports more ports than supported, capping: ", port_count as u32);
        port_count = MAX_PORTS;
    }

    // DeviceRemovable follows the fixed part, one bit per port starting at
    // bit 0 (= the hub itself), little-endian across bytes -- so it is
    // `port_count / 8 + 1` bytes long, and PortPwrCtrlMask (which this
    // driver has no use for) starts right after it.
    let removable_end = received.min(DESCRIPTOR_FIXED_LEN + port_count as usize / 8 + 1);
    let mut device_removable = 0u32;
    for (index, &byte) in bytes[DESCRIPTOR_FIXED_LEN..removable_end].iter().enumerate().take(4) {
        device_removable |= (byte as u32) << (8 * index);
    }

    Some(HubDescriptor {
        port_count,
        characteristics: u16::from_le_bytes([bytes[3], bytes[4]]),
        power_on_to_power_good_ms: bytes[5] as u16 * POWER_GOOD_UNIT_MS,
        control_current_ma: bytes[6],
        device_removable,
        descriptor_len: bytes[0],
    })
}

/// Builds a hub class-specific device-to-host setup packet, the read
/// counterpart to `protocol::build_standard_out_setup`. `request_type`
/// selects the recipient: the hub itself (0xA0) or one of its ports
/// (0xA3, port number in `index`).
fn build_class_in_setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> [u8; 8] {
    [
        request_type,
        request,
        (value & 0xFF) as u8,
        (value >> 8) as u8,
        (index & 0xFF) as u8,
        (index >> 8) as u8,
        (length & 0xFF) as u8,
        (length >> 8) as u8,
    ]
}
