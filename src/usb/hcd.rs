//! ESP32-P4 High-Speed USB-DWC host controller driver: VBUS power, core
//! bring-up, host port management, and the raw channel/packet primitive
//! everything above this layer (`protocol.rs`, `hid_keyboard.rs`) is built
//! on. This layer knows about registers, channels, and packets -- it does
//! not know what a USB device, descriptor, or endpoint means; `run_packet`
//! just runs one packet on channel 0 and reports what happened.
//!
//! Tab5's USB-A connector is wired to this controller (internal UTMI PHY,
//! dedicated DM/DP pins); the Full-Speed OTG controller on GPIO26/27
//! (Tab5's USB-C port) is out of scope, as is the USB Serial/JTAG
//! controller on GPIO24/25 that `uart.rs` already uses for
//! flashing/logging.
//!
//! This is Stage 1 of `docs/USB_HOST_PLAN.md`: core bring-up and host-port
//! connect/reset/speed detection only, driven by polling (no interrupt
//! routing yet). Stage 4 added one knob here, `FORCE_FS_LS_ONLY_HOST`,
//! which holds the whole bus at Full-Speed. That is how FS/LS devices
//! behind a hub work here today: this driver implements no split
//! transactions, so it avoids ever needing one. The hardware itself does
//! support them -- see `probe_split_support`, which contradicts
//! Espressif's documentation on real silicon.
//!
//! The USB-A 5V (VBUS) switch is one bit of the second PI4IOE5V6408 I/O
//! expander (I2C address 0x44, "E2"), the counterpart to `lcd.rs`'s PI4IOE1
//! (0x43) which drives the LCD reset line. Confirmed on real hardware with
//! the `usbvbus` shell command: bit 3 raises USB-A's VBUS to 5V
//! (`VBUS_ENABLE_BIT` below).

use crate::delay::{delay_ms, delay_us};
use crate::i2c;
use crate::uart;
use core::sync::atomic::{AtomicBool, Ordering};

/// Whether the current continuously-disconnected root-port state has
/// already produced its timeout log.  `probe_port` is intentionally
/// stateless otherwise, but it is called periodically while the port is
/// empty, so this small bit prevents that normal polling from flooding the
/// UART.
static NO_DEVICE_TIMEOUT_REPORTED: AtomicBool = AtomicBool::new(false);

fn note_root_device_connected() {
    // A later disconnect is a new state transition and deserves one timeout
    // message. This flag only controls logging, so no memory synchronization
    // with USB state is needed.
    NO_DEVICE_TIMEOUT_REPORTED.store(false, Ordering::Relaxed);
}

fn log_no_device_timeout_once() {
    if !NO_DEVICE_TIMEOUT_REPORTED.swap(true, Ordering::Relaxed) {
        uart::log(b"USB: no device detected on USB-A within timeout\r\n");
    }
}

#[cfg(any())]
fn clear_last_packet_failure() {
    LAST_PACKET_FAILURE_KIND.store(PACKET_FAILURE_NONE, Ordering::Relaxed);
}

#[cfg(any())]
fn record_packet_failure(kind: u32, hcint: u32, qtd_status: u32) {
    LAST_PACKET_FAILURE_HCINT.store(hcint, Ordering::Relaxed);
    LAST_PACKET_FAILURE_HPRT.store(unsafe { read(HPRT) }, Ordering::Relaxed);
    LAST_PACKET_FAILURE_QTD_STATUS.store(qtd_status, Ordering::Relaxed);
    LAST_PACKET_FAILURE_HCCHAR.store(unsafe { read(CHAN0_HCCHAR) }, Ordering::Relaxed);
    LAST_PACKET_FAILURE_HCSPLT.store(unsafe { read(CHAN0_HCSPLT) }, Ordering::Relaxed);
    LAST_PACKET_FAILURE_HCTSIZ.store(unsafe { read(CHAN0_HCTSIZ) }, Ordering::Relaxed);
    LAST_PACKET_FAILURE_HFNUM.store(unsafe { read(HFNUM) }, Ordering::Relaxed);
    LAST_PACKET_FAILURE_GINTSTS.store(unsafe { read(GINTSTS) }, Ordering::Relaxed);
    LAST_PACKET_FAILURE_GINTMSK.store(unsafe { read(GINTMSK) }, Ordering::Relaxed);
    LAST_PACKET_FAILURE_HCFG.store(unsafe { read(HCFG) }, Ordering::Relaxed);
    LAST_PACKET_FAILURE_UTMI_FC06.store(unsafe { read(USB_UTMI_FC06) }, Ordering::Relaxed);
    LAST_PACKET_FAILURE_USBOTG20_CTRL
        .store(unsafe { read(HP_SYSTEM_USBOTG20_CTRL) }, Ordering::Relaxed);
    LAST_PACKET_FAILURE_SOC_CLK_CTRL1.store(
        unsafe { read(HP_SYS_CLKRST_SOC_CLK_CTRL1) },
        Ordering::Relaxed,
    );
    LAST_PACKET_FAILURE_HP_USB_CTRL1
        .store(unsafe { read(LP_CLKRST_HP_USB_CTRL1) }, Ordering::Relaxed);
    // Publish last so a reader never observes a new kind with stale fields.
    LAST_PACKET_FAILURE_KIND.store(kind, Ordering::Release);
}

/// Emits the diagnostic snapshot for the immediately preceding failed
/// packet. Intended for a one-shot, higher-level recovery report -- callers
/// must provide their own de-duplication policy.
#[cfg(any())]
pub fn log_last_packet_failure(context: &[u8]) {
    let kind = LAST_PACKET_FAILURE_KIND.load(Ordering::Acquire);
    uart::log(context);
    uart::log(b": ");
    uart::log(match kind {
        PACKET_FAILURE_TIMEOUT => b"channel/TT timeout\r\n" as &[u8],
        PACKET_FAILURE_STALL => b"USB STALL\r\n",
        PACKET_FAILURE_TRANSACTION => b"transaction error\r\n",
        PACKET_FAILURE_QTD => b"DMA QTD status error\r\n",
        PACKET_FAILURE_INVALID_SPLIT_LENGTH => b"invalid split packet length\r\n",
        PACKET_FAILURE_SHORT_RESPONSE => b"short control response\r\n",
        _ => b"failure state unavailable\r\n",
    });
    if kind != PACKET_FAILURE_NONE {
        uart::log_hex(
            b"USB:   HCINT=",
            LAST_PACKET_FAILURE_HCINT.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   HPRT=",
            LAST_PACKET_FAILURE_HPRT.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   HCCHAR=",
            LAST_PACKET_FAILURE_HCCHAR.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   HCSPLT=",
            LAST_PACKET_FAILURE_HCSPLT.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   HCTSIZ=",
            LAST_PACKET_FAILURE_HCTSIZ.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   HFNUM at failure=",
            LAST_PACKET_FAILURE_HFNUM.load(Ordering::Relaxed),
        );
        // A live sample separated by a millisecond tells us whether SOF/frame
        // generation is still progressing after the failed transfer.
        let hfnum_now = unsafe { read(HFNUM) };
        delay_us(1_000);
        uart::log_hex(b"USB:   HFNUM before +1ms=", hfnum_now);
        uart::log_hex(b"USB:   HFNUM +1ms=", unsafe { read(HFNUM) });
        uart::log_hex(
            b"USB:   GINTSTS=",
            LAST_PACKET_FAILURE_GINTSTS.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   GINTMSK=",
            LAST_PACKET_FAILURE_GINTMSK.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   HCFG=",
            LAST_PACKET_FAILURE_HCFG.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   UTMI_FC06=",
            LAST_PACKET_FAILURE_UTMI_FC06.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   USBOTG20_CTRL=",
            LAST_PACKET_FAILURE_USBOTG20_CTRL.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   USB SYS CLK CTRL1=",
            LAST_PACKET_FAILURE_SOC_CLK_CTRL1.load(Ordering::Relaxed),
        );
        uart::log_hex(
            b"USB:   USB PHY/CORE CTRL1=",
            LAST_PACKET_FAILURE_HP_USB_CTRL1.load(Ordering::Relaxed),
        );
        if kind == PACKET_FAILURE_QTD {
            uart::log_hex(
                b"USB:   QTD status=",
                LAST_PACKET_FAILURE_QTD_STATUS.load(Ordering::Relaxed),
            );
        }
    }
}

#[cfg(any())]
fn clear_split_trace() {
    SPLIT_TRACE_NEXT.store(0, Ordering::Relaxed);
    SPLIT_TRACE_COUNT.store(0, Ordering::Relaxed);
    LAST_SPLIT_OUTCOME.store(SPLIT_OUTCOME_NONE, Ordering::Relaxed);
    LAST_SPLIT_CHANNEL_ACTIVE.store(false, Ordering::Relaxed);
}

#[cfg(any())]
fn record_split_round(phase: u32, hcint: u32) {
    let next = SPLIT_TRACE_NEXT.fetch_add(1, Ordering::Relaxed);
    let slot = (next as usize) % SPLIT_TRACE_CAPACITY;
    SPLIT_TRACE_PHASE[slot].store(phase, Ordering::Relaxed);
    SPLIT_TRACE_HCINT[slot].store(hcint, Ordering::Relaxed);
    let count = SPLIT_TRACE_COUNT.load(Ordering::Relaxed);
    if count < SPLIT_TRACE_CAPACITY as u32 {
        SPLIT_TRACE_COUNT.store(count + 1, Ordering::Release);
    }
}

/// Emits the last split packet's handshake history. Called only when a later
/// hub control transfer has exhausted its retry budget.
#[cfg(any())]
pub fn log_recent_split_trace() {
    let count = SPLIT_TRACE_COUNT.load(Ordering::Acquire) as usize;
    if count == 0 {
        uart::log(b"USB: no preceding split transaction recorded\r\n");
        return;
    }
    uart::log(b"USB: preceding split transaction (oldest first)\r\n");
    let next = SPLIT_TRACE_NEXT.load(Ordering::Relaxed) as usize;
    let start = if count == SPLIT_TRACE_CAPACITY {
        next % SPLIT_TRACE_CAPACITY
    } else {
        0
    };
    for offset in 0..count {
        let slot = (start + offset) % SPLIT_TRACE_CAPACITY;
        let phase = SPLIT_TRACE_PHASE[slot].load(Ordering::Relaxed);
        let label = if phase == SPLIT_PHASE_COMPLETE {
            b"USB:   CSPLIT HCINT=" as &[u8]
        } else {
            b"USB:   SSPLIT HCINT="
        };
        uart::log_hex(label, SPLIT_TRACE_HCINT[slot].load(Ordering::Relaxed));
    }
    uart::log(b"USB:   split outcome=");
    uart::log(match LAST_SPLIT_OUTCOME.load(Ordering::Relaxed) {
        SPLIT_OUTCOME_COMPLETE => b"complete\r\n" as &[u8],
        SPLIT_OUTCOME_SAFE_TIMEOUT => b"safe timeout/NAK boundary\r\n",
        SPLIT_OUTCOME_ERROR => b"transfer error\r\n",
        _ => b"not recorded\r\n",
    });
    uart::log_hex(
        b"USB:   split HCFG after cleanup=",
        LAST_SPLIT_HCFG_AFTER.load(Ordering::Relaxed),
    );
    if LAST_SPLIT_CHANNEL_ACTIVE.load(Ordering::Relaxed) {
        uart::log(b"USB:   split channel was active during cleanup\r\n");
    }
}

/// Records a completed but too-short class/control response. This is not an
/// HCD error, but is still enough to make a hub-port scan unreliable.
#[cfg(any())]
pub fn note_short_control_response() {
    record_packet_failure(PACKET_FAILURE_SHORT_RESPONSE, 0, 0);
}

// ------------------------------------------------------------------------
// USB-A VBUS power switch (PI4IOE5V6408 "E2", I2C 0x44)
// ------------------------------------------------------------------------

const PI4IOE2_ADDRESS: u8 = 0x44;

// PI4IOE5V6408 register map, confirmed on this board's PI4IOE1 by
// `lcd.rs::reset_lcd_panel`.
const PI4IOE2_REG_DIRECTION: u8 = 0x03; // 1 = pin is an output
const PI4IOE2_REG_OUTPUT: u8 = 0x05; // driven level when direction = output
const PI4IOE2_REG_HIZ: u8 = 0x07; // 1 = output stage high-impedance (must be 0 to actually drive)

/// Confirmed on real hardware (see the module doc comment).
const VBUS_ENABLE_BIT: u8 = 3;

/// Drives a specific PI4IOE2 output bit.
///
/// Unlike `lcd.rs`'s PI4IOE1 writes (which own every pin on that expander
/// and can overwrite the whole output byte), E2 also carries WiFi chip,
/// speaker amp, and expansion-port 5V power on other pins. Every register
/// touched here is read-modify-write of a single bit so the other pins'
/// configuration is left exactly as found.
pub fn set_pi4ioe2_output_bit(bit: u8, on: bool) -> bool {
    if bit > 7 {
        return false;
    }
    rmw_bit(PI4IOE2_REG_DIRECTION, bit, true)
        && rmw_bit(PI4IOE2_REG_HIZ, bit, false)
        && rmw_bit(PI4IOE2_REG_OUTPUT, bit, on)
}

/// Enables or disables the USB-A 5V rail through a specific E2 output bit.
///
/// This remains the USB-facing spelling used by the shell's `usbvbus`
/// diagnostic. Other board functions should use [`set_pi4ioe2_output_bit`]
/// so they do not imply that an arbitrary E2 pin is a VBUS control.
pub fn set_vbus_bit(bit: u8, on: bool) -> bool {
    set_pi4ioe2_output_bit(bit, on)
}

fn set_vbus(on: bool) -> bool {
    set_vbus_bit(VBUS_ENABLE_BIT, on)
}

fn rmw_bit(register: u8, bit: u8, set_bit: bool) -> bool {
    let Some(current) = pi4ioe2_read(register) else {
        return false;
    };
    let mask = 1u8 << bit;
    let updated = if set_bit {
        current | mask
    } else {
        current & !mask
    };
    pi4ioe2_write(register, updated)
}

fn pi4ioe2_write(register: u8, value: u8) -> bool {
    i2c::board_bus()
        .write(PI4IOE2_ADDRESS, &[register, value])
        .is_ok()
}

fn pi4ioe2_read(register: u8) -> Option<u8> {
    let mut value = [0u8; 1];
    i2c::board_bus()
        .write_read(PI4IOE2_ADDRESS, &[register], &mut value)
        .ok()?;
    Some(value[0])
}

// ------------------------------------------------------------------------
// USB-DWC High-Speed core (UTMI PHY) register map
// ------------------------------------------------------------------------

const USB_DWC_HS: usize = 0x5000_0000;
const GAHBCFG: usize = USB_DWC_HS + 0x08;
const GUSBCFG: usize = USB_DWC_HS + 0x0C;
const GRSTCTL: usize = USB_DWC_HS + 0x10;
const GRXFSIZ: usize = USB_DWC_HS + 0x24;
const GNPTXFSIZ: usize = USB_DWC_HS + 0x28;
const GSNPSID: usize = USB_DWC_HS + 0x40;
const GHWCFG1: usize = USB_DWC_HS + 0x44;
const GHWCFG2: usize = USB_DWC_HS + 0x48;
const GHWCFG3: usize = USB_DWC_HS + 0x4C;
const GHWCFG4: usize = USB_DWC_HS + 0x50;
const HPTXFSIZ: usize = USB_DWC_HS + 0x100;
const HCFG: usize = USB_DWC_HS + 0x400;
const HPRT: usize = USB_DWC_HS + 0x440;

const GAHBCFG_DMAEN: u32 = 1 << 5;
const GAHBCFG_HBSTLEN_MASK: u32 = 0xF << 1;

const GUSBCFG_TOUTCAL_MASK: u32 = 0x7;
const GUSBCFG_PHYIF: u32 = 1 << 3;
const GUSBCFG_ULPIUTMISEL: u32 = 1 << 4;
const GUSBCFG_PHYSEL: u32 = 1 << 6;
const GUSBCFG_SRPCAP: u32 = 1 << 8;
const GUSBCFG_HNPCAP: u32 = 1 << 9;
const GUSBCFG_FORCEHSTMODE: u32 = 1 << 29;

const GRSTCTL_CSFTRST: u32 = 1 << 0;
const GRSTCTL_RXFFLSH: u32 = 1 << 4;
const GRSTCTL_TXFFLSH: u32 = 1 << 5;
const GRSTCTL_TXFNUM_MASK: u32 = 0x1F << 6;
const GRSTCTL_CSFTRSTDONE: u32 = 1 << 29;
const GRSTCTL_AHBIDLE: u32 = 1 << 31;

// Core version at which the soft-reset sequence gained the CSftRstDone bit
// (from ESP-IDF's `usb_dwc_ll.h`). ESP32-P4's HS core is v4.30a.
const GSNPSID_4_20A: u32 = 0x4F54_420A;

const GHWCFG2_NUMHSTCHNL_MASK: u32 = 0xF << 14;
// The core's own read-only report of the `OTG_SINGLE_POINT` synthesis
// parameter: 1 = single-point (no hub, no split transactions), 0 =
// multi-point. See `probe_split_support`.
const GHWCFG2_SINGPNT: u32 = 1 << 5;
const GHWCFG3_DFIFODEPTH_SHIFT: u32 = 16;

const HCFG_FSLSSUPP: u32 = 1 << 2;
const HCFG_DESCDMA: u32 = 1 << 23;
const HCFG_PERSCHEDENA: u32 = 1 << 26;

/// Stage 4 of `docs/USB_HOST_PLAN.md`: when true, the host is restricted to
/// Full/Low-Speed operation (`HCFG.FSLSSupp`), so it never drives the
/// High-Speed chirp during a port reset and every attached device --
/// including High-Speed-capable ones -- falls back to Full-Speed, which
/// USB2.0 requires all of them to support.
///
/// It is now off, because it is no longer needed. It was Stage 4's way of
/// reaching a Full/Low-Speed device behind a hub without split transactions
/// (`HCSPLT`, SSPLIT/CSPLIT): a High-Speed hub whose *upstream* link is
/// Full-Speed acts as a plain Full-Speed repeater, so nothing downstream of
/// it needs a TT. The cost was that every device on the bus, High-Speed
/// ones included, dropped to 12 Mbps.
///
/// What made that look permanent rather than provisional was that
/// Espressif's synthesis parameters (`soc/esp32p4/.../usb_dwc_cfg.h`:
/// `OTG20_SINGLE_POINT 1`), their maintainer notes ("Split transfers not
/// supported"), and their host stack (`components/usb/hub.c`, which refuses
/// a speed-mismatched port with "transaction translator (TT) is not
/// supported") all say this core cannot split. The silicon disagrees --
/// `probe_split_support` measures `GHWCFG2.SingPnt` = 0 and a fully
/// functional `HCSPLT` -- and `Route`/`await_packet` now use it, so the bus
/// runs at High-Speed while slower devices behind a hub are reached through
/// that hub's TT.
///
/// The knob is kept rather than deleted: it stays the fallback if a
/// particular hub's TT misbehaves, and it is still the only thing in this
/// project that branches on host speed support. Note that ESP-IDF's own
/// host driver never sets this bit, so unlike most of this file there is no
/// reference implementation behind it.
pub const FORCE_FS_LS_ONLY_HOST: bool = false;

const HPRT_PRTCONNSTS: u32 = 1 << 0;
const HPRT_PRTENA: u32 = 1 << 2;
// Bit 4 (prtovrcurract) is not acted on, but it is included in the HPRT
// value logged when a transfer fails: a device that stops answering
// because the board's 5V switch current-limited looks exactly like one
// that stopped answering for protocol reasons, except for this bit.
const HPRT_PRTRST: u32 = 1 << 8;
const HPRT_PRTPWR: u32 = 1 << 12;
const HPRT_PRTSPD_SHIFT: u32 = 17;
const HPRT_PRTSPD_MASK: u32 = 0x3 << HPRT_PRTSPD_SHIFT;
// Write-1-to-clear status bits (prtconndet, prtena, prtenchng,
// prtovrcurrchng) that must be preserved as 0 whenever a non-W1C field
// (prtpwr, prtrst, ...) is written, or a stale status bit gets cleared as a
// side effect.
const HPRT_W1C_MASK: u32 = (1 << 1) | (1 << 2) | (1 << 3) | (1 << 5);

// Host channel registers. Only channel 0 is ever used (control transfers
// and, from Stage 3, Interrupt IN polling to a single directly-attached
// device -- no hub, no channel allocator needed), so these are fixed
// offsets rather than a per-channel accessor.
const HOST_CHANS: usize = USB_DWC_HS + 0x500; // channel stride is 0x20; channel 0 needs no offset
// Offset 0x04 is HCSPLT, the split-transaction control register. Nothing
// here programs it yet, but -- contrary to every piece of Espressif
// documentation -- it is present and functional on this silicon. See
// `probe_split_support` for the measurement.
const CHAN0_HCSPLT: usize = HOST_CHANS + 0x04;
const CHAN0_HCCHAR: usize = HOST_CHANS;
const CHAN0_HCINT: usize = HOST_CHANS + 0x08;
const CHAN0_HCTSIZ: usize = HOST_CHANS + 0x10;
const CHAN0_HCDMA: usize = HOST_CHANS + 0x14;

const HCCHAR_CHDIS: u32 = 1 << 30;
const HCCHAR_CHENA: u32 = 1 << 31;
const HCCHAR_EPDIR_IN: u32 = 1 << 15;
// A Low-Speed device reached *over a Full-Speed bus*: the core prefixes
// each of its transactions with a PRE token, which is what makes the hub
// in between repeat them onto its Low-Speed port.
//
// This is not simply "the device is Low-Speed". A Low-Speed device
// plugged straight into USB-A puts the whole bus at Low-Speed, where no
// preamble exists and this bit must stay clear -- ESP-IDF's equivalent
// flag is named `ls_via_fs_hub` and is likewise only set when the port is
// Full-Speed while the device is Low-Speed (`hcd_dwc.c`'s
// `pipe_set_ep_char`).
const HCCHAR_LSPDDEV: u32 = 1 << 17;
// usb_dwc_xfer_type_t: CTRL = 0, BULK = 2 (pre-shifted into HCCHAR's
// eptype field, bits[19:18]). There is no periodic-scheduler/frame-list
// infrastructure implemented (`probe_port` explicitly disables
// HCFG.PerSchedEna and never sets up HFLBAddr), and periodic (INTR/ISOC)
// channels are believed to depend on that frame-list scheduling to be
// serviced by the core at all in Scatter/Gather DMA mode -- a manually
// `CHENA`-activated INTR-type channel with no frame list entry may simply
// never be attempted (confirmed on real hardware: polling never completed
// with `eptype=INTR`). `hid_keyboard.rs` therefore polls the HID
// keyboard's Interrupt IN endpoint using BULK classification instead of
// INTR: at the FS/LS transaction level a bare IN token is identical
// regardless of which "channel type" the host locally used to schedule
// it (only SETUP has a distinct token type), so the device -- which knows
// its own endpoint as Interrupt-type from its own descriptor -- responds
// the same way either way, and this is confirmed working on real
// hardware.
pub const HCCHAR_EPTYPE_CTRL: u32 = 0 << 18;
pub const HCCHAR_EPTYPE_BULK: u32 = 2 << 18;

/// HCCHAR bits[21:20], the field the databook calls MC/EC. With
/// `HCSPLT.SpltEna` clear it is a periodic multi-count and 0 is harmless
/// (every unsplit transfer in this driver leaves it there). With SpltEna
/// set it becomes the split transaction's retry count, which the databook
/// requires to be at least 1 -- and Linux's dwc2 initializes to 1 for every
/// channel it allocates, split or not.
const HCCHAR_MC_ONE: u32 = 1 << 20;

// Bounds `force_halt_channel`'s wait for the halt it explicitly requested;
// short, since by that point the transfer has already been given up on
// and this is just cleanup.
const HALT_CONFIRM_ITERATIONS: u32 = 5_000;

const HCINT_XFERCOMPL: u32 = 1 << 0;
const HCINT_CHHLTD: u32 = 1 << 1;
const HCINT_STALL: u32 = 1 << 3;
const HCINT_NAK: u32 = 1 << 4;
const HCINT_XACTERR: u32 = 1 << 7;
const HCINT_BBLERR: u32 = 1 << 8;
const HCINT_XCS_XACT_ERR: u32 = 1 << 12;
const HCINT_ERROR_MASK: u32 = HCINT_STALL | HCINT_XACTERR | HCINT_BBLERR | HCINT_XCS_XACT_ERR;

// HCSPLT: the split-transaction control register, present and functional on
// this silicon despite Espressif's documentation (see
// `probe_split_support`).
//
// XactPos selects which part of a Full-Speed transaction a split carries.
// Only "ALL" is ever used here: it means the whole payload fits in one
// Full-Speed transaction, which is always true for the control and bulk
// transfers this driver runs (MPS <= 64 <= the 188-byte limit that would
// force a BEGIN/MID/END sequence). Isochronous OUT is the only transfer
// type that needs the others, and this driver has none.
const HCSPLT_PRTADDR_MASK: u32 = 0x7F; // bits[6:0]
const HCSPLT_HUBADDR_SHIFT: u32 = 7; // bits[13:7]
const HCSPLT_XACTPOS_ALL: u32 = 3 << 14;
const HCSPLT_COMPSPLT: u32 = 1 << 16;
const HCSPLT_SPLTENA: u32 = 1 << 31;

// HCTSIZi in Scatter/Gather DMA mode repurposes the low byte as SCHED_INFO
// (bits[7:0], must be 0xFF for non-periodic channels or the channel can
// freeze -- ESP-IDF's `usb_dwc_ll_hctsiz_init` comment) and bits[15:8] as
// NTD (number of transfer descriptors - 1). `run_packet` always runs one
// QTD per packet, so NTD is always 0.
const HCTSIZ_SCHED_INFO_ALL: u32 = 0xFF;
const HCTSIZ_PID_DATA1: u32 = 2 << 29; // 2'b10; DATA0 is 2'b00

// The same register's *buffer* DMA meaning, used only by `run_split_packet`
// (see there for why splits cannot use Scatter/Gather DMA). Here the low
// bits really are a byte count rather than SCHED_INFO, and the core needs
// to be told the packet count as well.
const HCTSIZ_XFERSIZE_MASK: u32 = 0x7_FFFF; // bits[18:0]
const HCTSIZ_PKTCNT_SHIFT: u32 = 19; // bits[28:19]
/// Buffer DMA marks a SETUP packet with a PID of 2'b11, where Scatter/Gather
/// DMA used the QTD's `QTD_IS_SETUP` bit.
const HCTSIZ_PID_SETUP: u32 = 3 << 29;

// QTD (Queue Transfer Descriptor), 8 bytes: control word + buffer pointer.
// The list this points into must be 512-byte aligned (`HCDMAi.dmaaddr`
// packs the list base into bits[31:9]); `run_packet` only ever uses a
// single-entry list, so a single over-aligned local is enough.
const QTD_XFER_SIZE_MASK: u32 = 0x1_FFFF; // bits[16:0]
const QTD_IS_SETUP: u32 = 1 << 24;
const QTD_INTR_CPLT: u32 = 1 << 25;
const QTD_EOL: u32 = 1 << 26;
const QTD_STATUS_SHIFT: u32 = 28;
const QTD_STATUS_MASK: u32 = 0x3 << QTD_STATUS_SHIFT;
const QTD_STATUS_SUCCESS: u32 = 0;
const QTD_ACTIVE: u32 = 1 << 31;

#[repr(C, align(512))]
struct QtdSlot {
    control: u32,
    buffer: u32,
}

impl QtdSlot {
    const fn zeroed() -> Self {
        Self {
            control: 0,
            buffer: 0,
        }
    }
}

// ------------------------------------------------------------------------
// USB UTMI PHY and clock/reset control
// ------------------------------------------------------------------------

const USB_UTMI: usize = 0x5009_C000;
const USB_UTMI_FC06: usize = USB_UTMI + 0x18;
const UTMI_FC06_LS_PAR_EN: u32 = 1 << 0;
/// The PHY's preamble control, `pre_hphy_lsie` in ESP-IDF's
/// `usb_utmi_struct.h` ("Dis_preamble enable"), which resets to 0.
///
/// Without it set, every transaction to a Low-Speed device behind a
/// Full-Speed hub fails at its first packet with `XCS_XACT_ERR`: the
/// preamble the core asks for never makes it onto the wire. With it set,
/// the same device enumerates and reports keystrokes normally (both
/// confirmed on real hardware).
///
/// ESP-IDF leaves it alone because nothing it does on this chip ever
/// sends a preamble -- it drives the port at High-Speed, where Low-Speed
/// devices behind a hub are reached with split transactions instead, and
/// its hub driver simply rejects them since this core has no `HCSPLT`
/// register. Holding the bus at Full-Speed (`FORCE_FS_LS_ONLY_HOST`)
/// makes preambles the mechanism in play, so this project needs the bit
/// that ESP-IDF never does.
const UTMI_FC06_PRE_HPHY_LSIE: u32 = 1 << 2;
const UTMI_FC06_LS_KPALV_EN: u32 = 1 << 3;

// Shared with `lcd.rs`'s `HP_SYS_CLKRST` constant (same peripheral).
const HP_SYS_CLKRST: usize = 0x500E_6000;
const HP_SYS_CLKRST_SOC_CLK_CTRL1: usize = HP_SYS_CLKRST + 0x18;
const SOC_CLK_CTRL1_USB_OTG20_SYS_CLK_EN: u32 = 1 << 16;

const LP_CLKRST: usize = 0x5011_1000;
const LP_CLKRST_HP_USB_CTRL1: usize = LP_CLKRST + 0x48;
const HP_USB_CTRL1_RST_OTG20_PHY: u32 = 1 << 1;
const HP_USB_CTRL1_RST_OTG20: u32 = 1 << 2;
const HP_USB_CTRL1_PHYREF_CLK_EN: u32 = 1 << 30;

const HP_SYSTEM: usize = 0x500E_5000;
const HP_SYSTEM_USBOTG20_CTRL: usize = HP_SYSTEM + 0x15C;
// Fixes a missing-disconnect-event errata on ESP32-P4 (ESP-IDF IDF-9953):
// HP_SYSTEM_OTG_SUSPENDM is not tied to 1 by hardware, so software must set
// it for the core to notice a device detaching.
const USBOTG20_CTRL_OTG_SUSPENDM: u32 = 1 << 21;

// ESP-IDF's `hcd_dwc.c` Kconfig defaults (`CONFIG_USB_HOST_*_MS`).
const RESET_HOLD_MS: u32 = 30;
const RESET_RECOVERY_MS: u32 = 30;
const DEBOUNCE_DELAY_MS: u32 = 250;
// "A delay of at least 25ms to enter Host mode" (ESP-IDF `INIT_DELAY_MS`).
const FORCE_HOST_MODE_DELAY_MS: u32 = 30;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    High,
    Full,
    Low,
    Unknown,
}

#[derive(Clone, Copy)]
pub struct HostPort {
    pub vbus_enable_acked: bool,
    pub core_alive: bool,
    pub core_id: u32,
    pub fifo_depth_words: u32,
    pub channel_count: u32,
    pub connected: bool,
    pub enabled: bool,
    pub speed: Speed,
}

/// What the silicon itself reports about split-transaction support, as
/// gathered by `probe_split_support`.
#[derive(Clone, Copy)]
pub struct SplitSupport {
    pub hwcfg1: u32,
    pub hwcfg2: u32,
    pub hwcfg3: u32,
    pub hwcfg4: u32,
    /// `GHWCFG2.SingPnt`: the core's read-only report of its
    /// `OTG_SINGLE_POINT` synthesis parameter. True means no hub and no
    /// split transactions.
    pub single_point: bool,
    /// What `CHAN0_HCSPLT` reads back after an all-ones write. A real
    /// HCSPLT would return the mask of its implemented fields
    /// (`SpltEna` | `CompSplt` | `XactPos` | `HubAddr` | `PrtAddr` =
    /// `0x8001_FFFF`); an unimplemented register reads 0.
    pub hcsplt_readback: u32,
    /// What `CHAN0_HCSPLT` reads back after writing `0x1234_5678`. A real
    /// register storing values through the same field mask returns
    /// `0x1234_5678 & 0x8001_FFFF` = `0x0000_5678`; a constant or an
    /// aliased read returns something else.
    pub hcsplt_pattern_readback: u32,
}

/// Asks the hardware directly whether it can do split transactions, which
/// is what a Full/Low-Speed device behind a *High-Speed* hub would need
/// (see `FORCE_FS_LS_ONLY_HOST`).
///
/// **The answer on real silicon is yes**, which contradicts all of
/// Espressif's documentation. Measured on this board (ESP32-P4 v1.3):
///
/// ```text
/// GHWCFG2=0x215FFFD0  SingPnt(bit5)=0
/// HCSPLT ch0: wrote 0xFFFFFFFF -> 0x8001FFFF; wrote 0x12345678 -> 0x00005678
/// ```
///
/// Both checks agree, and each is hard to explain away:
///
/// 1. `GHWCFG2.SingPnt` is the core's own read-only report of its
///    `OTG_SINGLE_POINT` synthesis parameter. It reads 0 -- multi-point,
///    i.e. hub and split transactions supported. Every other field of the
///    same register decodes to the documented value (architecture 2, 16
///    host channels, dynamic FIFO, multi-processor interrupt), so the
///    decode is not misaligned; `SingPnt` simply disagrees with
///    `soc/esp32p4/.../usb_dwc_cfg.h`'s `OTG20_SINGLE_POINT 1`.
/// 2. `CHAN0_HCSPLT` behaves as a real register, not as a missing one
///    (which would read 0). The all-ones write returns exactly the
///    databook's implemented-field mask -- `SpltEna` | `CompSplt` |
///    `XactPos` | `HubAddr` | `PrtAddr` = `0x8001_FFFF`, with reserved
///    bits [30:17] correctly reading back 0 -- and an arbitrary pattern
///    is stored through that same mask
///    (`0x1234_5678 & 0x8001_FFFF` = `0x0000_5678`).
///
/// What this does *not* prove is that the core actually emits SSPLIT and
/// CSPLIT tokens on the wire; only a transfer to a Full/Low-Speed device
/// behind a High-Speed hub can show that.
///
/// The register writes are safe: channel 0 is idle whenever this runs
/// (`run_packet` is synchronous and the shell calls this between polls),
/// HCSPLT has no effect on a halted channel, and the previous value is
/// restored.
pub fn probe_split_support() -> SplitSupport {
    let previous = unsafe { read(CHAN0_HCSPLT) };
    unsafe { write(CHAN0_HCSPLT, 0xFFFF_FFFF) };
    let hcsplt_readback = unsafe { read(CHAN0_HCSPLT) };
    unsafe { write(CHAN0_HCSPLT, 0x1234_5678) };
    let hcsplt_pattern_readback = unsafe { read(CHAN0_HCSPLT) };
    unsafe { write(CHAN0_HCSPLT, previous) };

    let hwcfg2 = unsafe { read(GHWCFG2) };
    SplitSupport {
        hwcfg1: unsafe { read(GHWCFG1) },
        hwcfg2,
        hwcfg3: unsafe { read(GHWCFG3) },
        hwcfg4: unsafe { read(GHWCFG4) },
        single_point: hwcfg2 & GHWCFG2_SINGPNT != 0,
        hcsplt_readback,
        hcsplt_pattern_readback,
    }
}

fn dead_port(vbus_enable_acked: bool, core_alive: bool, core_id: u32) -> HostPort {
    HostPort {
        vbus_enable_acked,
        core_alive,
        core_id,
        fifo_depth_words: 0,
        channel_count: 0,
        connected: false,
        enabled: false,
        speed: Speed::Unknown,
    }
}

/// Runs the full Stage 1 sequence from scratch: VBUS on, UTMI PHY and
/// USB-DWC core bring-up, host port power-on, and (if a device is already
/// plugged into USB-A) connect debounce, port reset, and speed read.
///
/// Like `sdmmc::init`, this re-initializes everything on every call; there
/// is no persistent handle at this layer (that is `hid_keyboard::UsbKeyboard`,
/// built on top).
pub fn probe_port() -> HostPort {
    let vbus_enable_acked = set_vbus(true);
    if !vbus_enable_acked {
        uart::log(b"USB: VBUS enable (PI4IOE2 @ 0x44) not acknowledged; continuing anyway\r\n");
    }
    delay_ms(50); // let VBUS settle before touching the host port

    enable_utmi_clocks();
    reset_utmi_and_core();
    configure_utmi_phy();

    let core_id = unsafe { read(GSNPSID) };
    let core_alive = (core_id & 0xFFFF_0000) == 0x4F54_0000;
    if !core_alive {
        uart::log_hex(b"USB: DWC core not responding, GSNPSID=", core_id);
        return dead_port(vbus_enable_acked, core_alive, core_id);
    }

    if !core_soft_reset() {
        uart::log(b"USB: core soft reset did not complete\r\n");
        return dead_port(vbus_enable_acked, core_alive, core_id);
    }
    set_core_defaults();

    let hwcfg2 = unsafe { read(GHWCFG2) };
    let hwcfg3 = unsafe { read(GHWCFG3) };
    let channel_count = ((hwcfg2 & GHWCFG2_NUMHSTCHNL_MASK) >> 14) + 1;
    let fifo_depth_words = hwcfg3 >> GHWCFG3_DFIFODEPTH_SHIFT;

    configure_fifos(fifo_depth_words);
    delay_ms(FORCE_HOST_MODE_DELAY_MS);

    configure_host_speed_support();
    hprt_modify(HPRT_PRTPWR, HPRT_PRTPWR); // port power on

    if !wait_for_connect() {
        // The foreground periodically probes an empty root port. Emit this
        // once for that disconnected interval, then wait until a connection
        // has actually been observed before allowing it again.
        log_no_device_timeout_once();
        return HostPort {
            vbus_enable_acked,
            core_alive,
            core_id,
            fifo_depth_words,
            channel_count,
            connected: false,
            enabled: false,
            speed: Speed::Unknown,
        };
    }

    note_root_device_connected();

    delay_ms(DEBOUNCE_DELAY_MS);
    if unsafe { read(HPRT) } & HPRT_PRTCONNSTS == 0 {
        uart::log(b"USB: connection bounced away during debounce\r\n");
        return HostPort {
            vbus_enable_acked,
            core_alive,
            core_id,
            fifo_depth_words,
            channel_count,
            connected: false,
            enabled: false,
            speed: Speed::Unknown,
        };
    }

    reset_pulse();

    let hprt = unsafe { read(HPRT) };
    let enabled = hprt & HPRT_PRTENA != 0;
    let speed = match (hprt & HPRT_PRTSPD_MASK) >> HPRT_PRTSPD_SHIFT {
        0 => Speed::High,
        1 => Speed::Full,
        2 => Speed::Low,
        _ => Speed::Unknown,
    };
    if enabled {
        finish_port_enable();
    } else {
        uart::log(b"USB: port reset completed but the port did not enable\r\n");
    }

    HostPort {
        vbus_enable_acked,
        core_alive,
        core_id,
        fifo_depth_words,
        channel_count,
        connected: true,
        enabled,
        speed,
    }
}

/// Cheap liveness check (one HPRT read, no transaction) used by
/// `hid_keyboard::UsbKeyboard::is_connected`.
pub fn port_connected() -> bool {
    let connected = unsafe { read(HPRT) & HPRT_PRTCONNSTS != 0 };
    if connected {
        // This cheap per-frame check lets an insertion re-arm the timeout
        // message even before the next full (and comparatively slow) probe.
        note_root_device_connected();
    }
    connected
}

fn enable_utmi_clocks() {
    unsafe {
        modify(
            HP_SYS_CLKRST_SOC_CLK_CTRL1,
            SOC_CLK_CTRL1_USB_OTG20_SYS_CLK_EN,
            SOC_CLK_CTRL1_USB_OTG20_SYS_CLK_EN,
        );
        modify(
            LP_CLKRST_HP_USB_CTRL1,
            HP_USB_CTRL1_PHYREF_CLK_EN,
            HP_USB_CTRL1_PHYREF_CLK_EN,
        );
    }
}

fn reset_utmi_and_core() {
    unsafe {
        // Assert both resets, then release PHY before controller, matching
        // ESP-IDF's `_usb_utmi_ll_reset_register`.
        modify(
            LP_CLKRST_HP_USB_CTRL1,
            HP_USB_CTRL1_RST_OTG20,
            HP_USB_CTRL1_RST_OTG20,
        );
        modify(
            LP_CLKRST_HP_USB_CTRL1,
            HP_USB_CTRL1_RST_OTG20_PHY,
            HP_USB_CTRL1_RST_OTG20_PHY,
        );
        modify(LP_CLKRST_HP_USB_CTRL1, HP_USB_CTRL1_RST_OTG20_PHY, 0);
        modify(LP_CLKRST_HP_USB_CTRL1, HP_USB_CTRL1_RST_OTG20, 0);
    }
}

fn configure_utmi_phy() {
    unsafe {
        modify(
            HP_SYSTEM_USBOTG20_CTRL,
            USBOTG20_CTRL_OTG_SUSPENDM,
            USBOTG20_CTRL_OTG_SUSPENDM,
        );
        // ESP-IDF's `usb_utmi_ll_configure_ls(hw, true)`: parallel
        // Low-Speed mode plus Low-Speed keep-alive, and then the preamble
        // bit it does not set.
        const LOW_SPEED_BITS: u32 =
            UTMI_FC06_LS_PAR_EN | UTMI_FC06_LS_KPALV_EN | UTMI_FC06_PRE_HPHY_LSIE;
        modify(USB_UTMI_FC06, LOW_SPEED_BITS, LOW_SPEED_BITS);
    }
}

/// Core soft reset, following the version-dependent sequence from
/// ESP-IDF's `usb_dwc_ll_grstctl_core_soft_reset` (our core is >= v4.20a,
/// so it uses the CSftRstDone handshake).
fn core_soft_reset() -> bool {
    let core_id = unsafe { read(GSNPSID) };
    unsafe {
        modify(GRSTCTL, GRSTCTL_CSFTRST, GRSTCTL_CSFTRST);
    }
    if core_id < GSNPSID_4_20A {
        if !poll_until(GRSTCTL, GRSTCTL_CSFTRST, false, 200_000) {
            return false;
        }
    } else {
        if !poll_until(GRSTCTL, GRSTCTL_CSFTRSTDONE, true, 200_000) {
            return false;
        }
        unsafe {
            let mut value = read(GRSTCTL);
            value &= !GRSTCTL_CSFTRST;
            value |= GRSTCTL_CSFTRSTDONE; // W1C
            write(GRSTCTL, value);
        }
    }
    poll_until(GRSTCTL, GRSTCTL_AHBIDLE, true, 200_000)
}

fn set_core_defaults() {
    unsafe {
        modify(GAHBCFG, GAHBCFG_DMAEN, GAHBCFG_DMAEN);
        modify(GAHBCFG, GAHBCFG_HBSTLEN_MASK, 0); // AHB burst = SINGLE

        modify(GUSBCFG, GUSBCFG_HNPCAP, 0);
        modify(GUSBCFG, GUSBCFG_SRPCAP, 0);
        modify(GUSBCFG, GUSBCFG_TOUTCAL_MASK, 5); // 5 PHY clocks, matching ESP-IDF's HS PHY setting
        modify(GUSBCFG, GUSBCFG_PHYIF, GUSBCFG_PHYIF); // 16-bit interface
        modify(GUSBCFG, GUSBCFG_ULPIUTMISEL, 0); // UTMI+
        modify(GUSBCFG, GUSBCFG_PHYSEL, 0); // HS PHY
        modify(GUSBCFG, GUSBCFG_FORCEHSTMODE, GUSBCFG_FORCEHSTMODE);
    }
}

/// Applies `FORCE_FS_LS_ONLY_HOST`. Runs after the force-host-mode delay
/// (HCFG is a host-mode register) but before the port is powered and
/// reset, since `HCFG.FSLSSupp` is what decides whether the core chirps
/// during that reset.
///
/// `HCFG.FSLSPclkSel` is deliberately left at its reset value (30/60 MHz):
/// its 48 MHz setting is for a dedicated Full-Speed PHY, whereas this core
/// keeps running its UTMI+ High-Speed PHY at 30/60 MHz and merely refrains
/// from chirping. `finish_port_enable`'s later HCFG writes are
/// read-modify-write, so they preserve this bit.
fn configure_host_speed_support() {
    if FORCE_FS_LS_ONLY_HOST {
        unsafe {
            modify(HCFG, HCFG_FSLSSUPP, HCFG_FSLSSUPP);
        }
    }
}

fn configure_fifos(fifo_depth_words: u32) {
    // No channels are allocated yet at core init, so the exact split does
    // not matter functionally. An even three-way split just needs to fit
    // within the core's total FIFO depth (from GHWCFG3).
    let rx_lines = fifo_depth_words / 2;
    let remaining = fifo_depth_words - rx_lines;
    let nptx_lines = remaining / 2;
    let ptx_lines = remaining - nptx_lines;

    unsafe {
        write(GRXFSIZ, rx_lines);
        write(GNPTXFSIZ, (rx_lines & 0xFFFF) | (nptx_lines << 16));
        write(
            HPTXFSIZ,
            ((rx_lines + nptx_lines) & 0xFFFF) | (ptx_lines << 16),
        );
    }
    flush_fifos();
}

fn flush_fifos() {
    unsafe {
        modify(GRSTCTL, GRSTCTL_TXFNUM_MASK, 0); // select non-periodic TX FIFO
        modify(GRSTCTL, GRSTCTL_TXFFLSH, GRSTCTL_TXFFLSH);
    }
    if !poll_until(GRSTCTL, GRSTCTL_TXFFLSH, false, 100_000) {
        uart::log(b"USB: non-periodic TX FIFO flush timed out\r\n");
    }
    unsafe {
        modify(GRSTCTL, GRSTCTL_TXFNUM_MASK, 1 << 6); // select periodic TX FIFO
        modify(GRSTCTL, GRSTCTL_TXFFLSH, GRSTCTL_TXFFLSH);
    }
    if !poll_until(GRSTCTL, GRSTCTL_TXFFLSH, false, 100_000) {
        uart::log(b"USB: periodic TX FIFO flush timed out\r\n");
    }
    unsafe {
        modify(GRSTCTL, GRSTCTL_RXFFLSH, GRSTCTL_RXFFLSH);
    }
    if !poll_until(GRSTCTL, GRSTCTL_RXFFLSH, false, 100_000) {
        uart::log(b"USB: RX FIFO flush timed out\r\n");
    }
}

/// Restores channel 0 to the baseline used by descriptor-DMA transfers after
/// a packet failure.  The driver is synchronous and uses no other channel,
/// so flushing the FIFOs here cannot discard another in-flight transfer.
///
/// This is intentionally lighter than a root-port reset: devices keep their
/// address/configuration and a retry can resume without re-enumerating live
/// keyboard or storage sessions.
pub fn recover_channel_after_packet_failure() {
    if unsafe { read(CHAN0_HCCHAR) } & HCCHAR_CHENA != 0 {
        force_halt_channel();
    }
    unsafe {
        write(CHAN0_HCINT, 0xFFFF_FFFF);
        write(CHAN0_HCSPLT, 0);
        modify(HCFG, HCFG_DESCDMA, HCFG_DESCDMA);
    }
    flush_fifos();
}

fn wait_for_connect() -> bool {
    const POLL_INTERVAL_US: u32 = 2_000;
    const MAX_POLLS: u32 = 250; // ~500ms; run `usbinfo` after plugging in the device
    for _ in 0..MAX_POLLS {
        if unsafe { read(HPRT) } & HPRT_PRTCONNSTS != 0 {
            return true;
        }
        delay_us(POLL_INTERVAL_US);
    }
    false
}

fn reset_pulse() {
    hprt_modify(HPRT_PRTRST, HPRT_PRTRST);
    delay_ms(RESET_HOLD_MS);
    hprt_modify(HPRT_PRTRST, 0);
    delay_ms(RESET_RECOVERY_MS);
}

fn finish_port_enable() {
    unsafe {
        modify(HCFG, HCFG_DESCDMA, HCFG_DESCDMA);
        modify(HCFG, HCFG_PERSCHEDENA, 0); // periodic scheduler stays off; see HCCHAR_EPTYPE_BULK's doc comment
    }
}

/// Writes one non-W1C HPRT field (power, reset, suspend, resume, test
/// control) without clobbering the interrupt-status bits that share the
/// register, mirroring ESP-IDF's `usb_dwc_ll_hprt_*` setters.
fn hprt_modify(field_mask: u32, field_value: u32) {
    unsafe {
        let current = read(HPRT);
        let base = current & !HPRT_W1C_MASK;
        write(HPRT, (base & !field_mask) | (field_value & field_mask));
    }
}

fn poll_until(address: usize, mask: u32, want_set: bool, timeout_iterations: u32) -> bool {
    let mut timeout = timeout_iterations;
    loop {
        let bit_set = unsafe { read(address) } & mask != 0;
        if bit_set == want_set {
            return true;
        }
        if timeout == 0 {
            return false;
        }
        timeout -= 1;
    }
}

// ------------------------------------------------------------------------
// Channel / packet primitive
// ------------------------------------------------------------------------

/// What one `run_packet` call produced. Kept distinct from a plain
/// `Option<usize>` so callers that care (`hid_keyboard::UsbKeyboard::poll`)
/// can tell "the device just hasn't NAK-retried into a real response yet"
/// apart from "the core reported an actual transaction error" -- the
/// former is routine while idle, the latter usually means the session is
/// stale (see `UsbKeyboard::needs_reinit`).
pub enum PacketOutcome {
    Ok(usize),
    Timeout,
    Error,
}

/// Where a packet is going. Grouped into one value because the same
/// destination is reused across the packets of a transfer (and, for an
/// interrupt endpoint, across every poll), while only the per-packet
/// details -- SETUP or not, which data toggle -- change.
///
/// `is_in` lives here too even though a control transfer flips direction
/// between its stages: `Endpoint` is `Copy`, so a stage can just say
/// `Endpoint { is_in: true, ..pipe }`.
/// The High-Speed hub whose Transaction Translator has to relay every
/// transaction to a slower device behind it, identified the way `HCSPLT`
/// wants it: the hub's own USB device address and the 1-based downstream
/// port the device is on.
#[derive(Clone, Copy)]
pub struct SplitTarget {
    pub hub_address: u8,
    pub port_number: u8,
}

/// How the controller has to reach a device, as opposed to what is being
/// sent to it. This is a property of where the device is plugged in, fixed
/// for as long as it stays there, so it is carried around with the device
/// (`protocol::ControlPipe`, `Endpoint`) rather than passed per transfer.
///
/// The three cases that matter, in the order the bus produces them:
///
/// - Plugged straight into USB-A: `Route::default()`. The whole bus runs at
///   the device's own speed, so there is nothing special to do.
/// - Behind a hub that runs at the same speed as the device: only
///   `low_speed_via_hub` applies (a Low-Speed device on a Full-Speed bus
///   needs PRE tokens).
/// - Behind a *High-Speed* hub at Full or Low Speed: `split` is set, and
///   every transaction becomes an SSPLIT/CSPLIT pair aimed at the hub's TT.
#[derive(Clone, Copy, Default)]
pub struct Route {
    /// A Low-Speed device reached through a hub, which needs PRE tokens --
    /// *not* just "the device is Low-Speed". See `HCCHAR_LSPDDEV`.
    pub low_speed_via_hub: bool,
    /// Set only for a Full/Low-Speed device behind a High-Speed hub.
    pub split: Option<SplitTarget>,
}

impl Route {
    /// The `HCSPLT` value for this route: a programmed split target, or 0
    /// to leave splitting off for a device the host can address directly.
    fn hcsplt(&self) -> u32 {
        match self.split {
            Some(target) => {
                HCSPLT_SPLTENA
                    | HCSPLT_XACTPOS_ALL
                    | ((target.hub_address as u32 & 0x7F) << HCSPLT_HUBADDR_SHIFT)
                    | (target.port_number as u32 & HCSPLT_PRTADDR_MASK)
            }
            None => 0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Endpoint {
    pub device_address: u8,
    pub endpoint_number: u8,
    /// `HCCHAR_EPTYPE_CTRL` or `HCCHAR_EPTYPE_BULK`.
    pub endpoint_type: u32,
    pub mps: u16,
    pub is_in: bool,
    pub route: Route,
}

/// Runs one packet (a single-entry, halt-on-complete QTD) on channel 0 to
/// completion and returns the number of bytes actually transferred.
///
/// Every field is rewritten from scratch on every call (HCCHAR, HCTSIZ,
/// the QTD) rather than incrementally patched, so a single packet is fully
/// self-describing and there is no cross-call state to get out of sync.
///
/// `quiet_timeout` suppresses the "timed out" log on a `timeout_iterations`
/// expiry: interrupt polling hits this whenever the device is simply still
/// NAKing (nothing new to report yet, expected while idle since
/// `SET_IDLE(0)` disables auto-repeat), which is routine and not worth a
/// UART line every time (unlike a control transfer actually failing to
/// complete).
///
/// `quiet_errors` similarly suppresses the STALL/transaction-error/QTD-
/// error logs. Unlike a timeout these are never routine, but a stale
/// `UsbKeyboard` session (e.g. after something else ran `probe_port`) hits
/// the *same* real error on every poll until `needs_reinit` gives up and
/// re-enumerates -- logging every repeat of an already-diagnosed error
/// adds nothing, so callers doing repeated polling pass `true` here once
/// they have already logged the first one in a streak.
///
/// `max_split_rounds` bounds how many SSPLIT/CSPLIT round trips a split
/// packet (`Endpoint::route`'s `split`) may take before giving up with
/// `Timeout`; it is ignored for a device the host addresses directly. It
/// exists because splitting moves NAK retrying out of the hardware and into
/// this function: a device that has nothing to say NAKs the complete split,
/// and whether that should be retried for a while (a control transfer,
/// where a NAK means "busy, ask again") or abandoned immediately (interrupt
/// polling, where it means "no new keystrokes" and the frame loop will be
/// back in 16ms) is the caller's call, not something `timeout_iterations`
/// -- a per-halt spin budget -- can express.
pub fn run_packet(
    endpoint: &Endpoint,
    is_setup: bool,
    pid_data1: bool,
    timeout_iterations: u32,
    max_split_rounds: u32,
    quiet_timeout: bool,
    quiet_errors: bool,
    buffer: &mut [u8],
) -> PacketOutcome {
    if endpoint.route.split.is_some() {
        return run_split_packet(
            endpoint,
            is_setup,
            pid_data1,
            timeout_iterations,
            max_split_rounds,
            quiet_timeout,
            quiet_errors,
            buffer,
        );
    }

    let xfer_len = buffer.len();
    let data_ptr = buffer.as_mut_ptr();
    if xfer_len > 0 {
        cache_writeback_invalidate(data_ptr as usize, xfer_len);
    }

    let mut qtd_control = xfer_len as u32 & QTD_XFER_SIZE_MASK;
    if is_setup {
        qtd_control |= QTD_IS_SETUP;
    }
    qtd_control |= QTD_INTR_CPLT | QTD_EOL | QTD_ACTIVE;

    let mut qtd = QtdSlot::zeroed();
    let qtd_address = &raw mut qtd as usize;
    unsafe {
        write(qtd_address, qtd_control);
        write(qtd_address + 4, data_ptr as u32);
    }
    cache_writeback_invalidate(qtd_address, 8);

    let hcchar = (endpoint.mps as u32 & 0x7FF) // bits[10:0]: MPS
        | ((endpoint.endpoint_number as u32 & 0xF) << 11)
        | (if endpoint.is_in { HCCHAR_EPDIR_IN } else { 0 })
        | (if endpoint.route.low_speed_via_hub { HCCHAR_LSPDDEV } else { 0 })
        | endpoint.endpoint_type
        | ((endpoint.device_address as u32 & 0x7F) << 22);
    let hctsiz = HCTSIZ_SCHED_INFO_ALL | (if pid_data1 { HCTSIZ_PID_DATA1 } else { 0 });

    unsafe {
        write(CHAN0_HCINT, 0xFFFF_FFFF); // clear stale status from a previous packet
        write(CHAN0_HCSPLT, 0); // this path never splits; see `run_split_packet`
        write(CHAN0_HCCHAR, hcchar);
        write(CHAN0_HCTSIZ, hctsiz);
        write(CHAN0_HCDMA, (qtd_address as u32) & 0xFFFF_FE00);
        modify(CHAN0_HCCHAR, HCCHAR_CHENA, HCCHAR_CHENA);
    }

    let hcint = match await_packet(0, 0, timeout_iterations, max_split_rounds) {
        Some(hcint) => hcint,
        None => {
            if !quiet_timeout {
                uart::log(b"USB: control transfer timed out waiting for channel halt\r\n");
                log_port_state();
            }
            // Leave the channel in a known-idle state regardless of why we
            // gave up, so the next call's fresh HCCHAR/HCTSIZ/HCDMA write is
            // not racing whatever the core was still doing.
            force_halt_channel();
            return PacketOutcome::Timeout;
        }
    };

    cache_writeback_invalidate(qtd_address, 8); // see the QTD fields the core wrote back
    let control_after = unsafe { read(qtd_address) };
    let remaining = (control_after & QTD_XFER_SIZE_MASK) as usize;
    let status = control_after & QTD_STATUS_MASK;
    let transferred = xfer_len.saturating_sub(remaining);

    if hcint & HCINT_STALL != 0 {
        if !quiet_errors {
            uart::log(b"USB: transfer STALL\r\n");
        }
        return PacketOutcome::Error;
    }
    if hcint & HCINT_ERROR_MASK != 0 {
        if !quiet_errors {
            uart::log_hex(b"USB: transfer transaction error, HCINT=", hcint);
            log_port_state();
        }
        return PacketOutcome::Error;
    }
    if status != QTD_STATUS_SUCCESS {
        if !quiet_errors {
            uart::log_hex(
                b"USB: transfer QTD error, status=",
                status >> QTD_STATUS_SHIFT,
            );
        }
        return PacketOutcome::Error;
    }
    if endpoint.is_in && transferred > 0 {
        cache_writeback_invalidate(data_ptr as usize, transferred);
    }
    PacketOutcome::Ok(transferred)
}

/// Largest split packet `run_split_packet` will stage. A device reached
/// through a hub's TT is Full or Low Speed by definition, so its endpoints
/// cap out at a 64-byte max packet size (USB2.0 5.5.3/5.7.3/5.8.3), and
/// every caller chunks by MPS before getting here.
const SPLIT_STAGING_MAX: usize = 64;

/// Absolute ceiling on the rounds one split packet may take, whatever the
/// caller's soft budget. Only a TT that never stops answering NYET reaches
/// it; see `await_packet` for why walking away before a safe boundary is a
/// last resort rather than the normal path.
const SPLIT_HARD_ROUND_CAP: u32 = 5_000;

/// A word-aligned staging buffer for split packets. Buffer DMA hands
/// `HCDMA` the data pointer itself (Scatter/Gather DMA pointed it at a
/// descriptor instead), and the core requires that pointer to be word
/// aligned -- which an arbitrary `&mut [u8]` sub-slice from a caller is
/// not. Copying through a fixed aligned buffer is cheaper than propagating
/// an alignment requirement up through every caller, at
/// `SPLIT_STAGING_MAX` bytes a packet.
#[repr(C, align(4))]
struct SplitStaging {
    bytes: [u8; SPLIT_STAGING_MAX],
}

/// Runs one packet to a device behind a High-Speed hub's Transaction
/// Translator, using **buffer DMA** rather than the Scatter/Gather DMA the
/// rest of this driver runs on.
///
/// That switch is the whole reason this function exists. The DWC_OTG core
/// cannot do split transactions in Scatter/Gather DMA mode: with
/// `HCFG.DescDMA` set and `HCSPLT.SpltEna` programmed, enabling the channel
/// does nothing at all -- confirmed on real hardware, where the channel sat
/// with `ChEna` still set and `HCINT` all zero until the timeout, having
/// never attempted a transaction. (Linux's dwc2 driver reaches the same
/// conclusion from the other direction: it turns descriptor DMA off when it
/// needs splits.) In buffer DMA the core does the start split, halts, and
/// leaves software to ask for the result -- which is what `await_packet`
/// drives.
///
/// `HCFG.DescDMA` is a whole-controller setting, not a per-channel one, so
/// it is cleared for the duration of this packet and restored afterwards.
/// That is safe here only because `run_packet` is synchronous and channel 0
/// is the only channel this driver ever uses: there is never another
/// transfer in flight to be switched out from under.
#[allow(clippy::too_many_arguments)]
fn run_split_packet(
    endpoint: &Endpoint,
    is_setup: bool,
    pid_data1: bool,
    timeout_iterations: u32,
    max_split_rounds: u32,
    quiet_timeout: bool,
    quiet_errors: bool,
    buffer: &mut [u8],
) -> PacketOutcome {
    let xfer_len = buffer.len();
    if xfer_len > SPLIT_STAGING_MAX {
        // Unreachable via the current callers (all chunk by MPS, which is
        // at most 64 on a Full/Low-Speed endpoint), but silently truncating
        // a transfer would be far worse than refusing it.
        uart::log_hex(
            b"USB: split packet larger than the staging buffer, len=",
            xfer_len as u32,
        );
        return PacketOutcome::Error;
    }

    let mut staging = SplitStaging {
        bytes: [0u8; SPLIT_STAGING_MAX],
    };
    if !endpoint.is_in {
        staging.bytes[..xfer_len].copy_from_slice(buffer);
    }
    let data_address = staging.bytes.as_mut_ptr() as usize;
    cache_writeback_invalidate(data_address, SPLIT_STAGING_MAX);

    let hcchar = (endpoint.mps as u32 & 0x7FF)
        | ((endpoint.endpoint_number as u32 & 0xF) << 11)
        | (if endpoint.is_in { HCCHAR_EPDIR_IN } else { 0 })
        | (if endpoint.route.low_speed_via_hub {
            HCCHAR_LSPDDEV
        } else {
            0
        })
        | endpoint.endpoint_type
        | HCCHAR_MC_ONE
        | ((endpoint.device_address as u32 & 0x7F) << 22);
    let pid = if is_setup {
        HCTSIZ_PID_SETUP
    } else if pid_data1 {
        HCTSIZ_PID_DATA1
    } else {
        0
    };
    // One packet per call, as everywhere else in this driver -- including
    // for a zero-length status stage, which is still one (empty) packet.
    let hctsiz = (xfer_len as u32 & HCTSIZ_XFERSIZE_MASK) | (1 << HCTSIZ_PKTCNT_SHIFT) | pid;
    let hcsplt = endpoint.route.hcsplt();

    unsafe {
        modify(HCFG, HCFG_DESCDMA, 0); // buffer DMA for this packet only
        write(CHAN0_HCINT, 0xFFFF_FFFF);
        write(CHAN0_HCSPLT, hcsplt);
        write(CHAN0_HCCHAR, hcchar);
        write(CHAN0_HCTSIZ, hctsiz);
        write(CHAN0_HCDMA, data_address as u32);
        modify(CHAN0_HCCHAR, HCCHAR_CHENA, HCCHAR_CHENA);
    }

    let outcome = await_packet(hcsplt, hctsiz, timeout_iterations, max_split_rounds);

    // Giving up leaves the channel enabled in the middle of a split, so it
    // has to be stopped *before* the controller's DMA mode changes back
    // underneath it. Switching `HCFG.DescDMA` with a transfer still in
    // flight corrupts the core: confirmed on real hardware, where doing it
    // in the other order made the *next*, unrelated, unsplit control
    // transfer to the hub fail with `XCS_XACT_ERR` every time -- the hub
    // looked like it had stopped answering, when in fact the abandoned
    // split had been left mid-flight across the mode switch.
    // Only tear anything down if the channel is genuinely still in flight.
    // Giving up at a safe boundary (`await_packet`) leaves it already
    // halted, and forcing `ChDis` onto a channel that is not enabled is not
    // a harmless no-op on this core -- the halt it asks for never
    // completes, because a disabled channel generates no halt. That path
    // runs on *every* idle keyboard poll, so getting it wrong corrupts the
    // core dozens of times a second rather than once in a rare timeout.
    let channel_active_during_cleanup = unsafe { read(CHAN0_HCCHAR) } & HCCHAR_CHENA != 0;
    if outcome.is_none() && channel_active_during_cleanup {
        force_halt_channel();
        // An abandoned in-flight split can also leave residue in the
        // FIFOs, which the next transfer -- on any endpoint, to any device
        // -- would read as its own data.
        flush_fifos();
    }
    // Restore Scatter/Gather DMA before anything can return: every other
    // packet in this driver depends on it. Splitting is cleared with it, so
    // no half-configured split outlives this call either.
    unsafe {
        write(CHAN0_HCSPLT, 0);
        modify(HCFG, HCFG_DESCDMA, HCFG_DESCDMA);
    }

    let Some(hcint) = outcome else {
        if !quiet_timeout {
            uart::log(b"USB: split transfer timed out waiting for the hub's TT\r\n");
            log_port_state();
        }
        return PacketOutcome::Timeout;
    };

    if hcint & HCINT_STALL != 0 {
        if !quiet_errors {
            uart::log(b"USB: split transfer STALL\r\n");
        }
        return PacketOutcome::Error;
    }
    if hcint & HCINT_ERROR_MASK != 0 {
        if !quiet_errors {
            uart::log_hex(b"USB: split transfer transaction error, HCINT=", hcint);
            log_port_state();
        }
        return PacketOutcome::Error;
    }

    // Buffer DMA reports progress by counting `HCTSIZ.XferSize` down as
    // bytes move, where Scatter/Gather DMA wrote the remainder back into
    // the QTD.
    let remaining = (unsafe { read(CHAN0_HCTSIZ) } & HCTSIZ_XFERSIZE_MASK) as usize;
    let transferred = xfer_len.saturating_sub(remaining.min(xfer_len));
    if endpoint.is_in && transferred > 0 {
        cache_writeback_invalidate(data_address, SPLIT_STAGING_MAX);
        buffer[..transferred].copy_from_slice(&staging.bytes[..transferred]);
    }
    PacketOutcome::Ok(transferred)
}

/// Waits for the channel `run_packet` just enabled to halt, and returns the
/// `HCINT` that ended it (already write-1-cleared). `None` on timeout.
///
/// For a device the host addresses directly this is a single wait: the core
/// retries NAKs itself and halts once when the QTD is done.
///
/// A split packet takes more than one channel activation. The host asks the
/// hub's Transaction Translator to run the transaction on its behalf (the
/// *start split*), then asks for the result (the *complete split*), and the
/// core halts the channel between the two:
///
/// - `NAK` means the TT has nothing for us -- either it would not take the
///   job (its buffer is busy) or the device itself NAKed, e.g. an idle
///   keyboard with no keystroke to report. Either way its buffer no longer
///   holds this transaction, so the sequence restarts from a fresh start
///   split.
/// - a bare `ACK` (the TT accepted the start split) or `NYET` (it has not
///   finished with the slow device yet) both mean "ask for the result",
///   i.e. run the complete split.
/// - anything else -- transfer complete, STALL, or a real error -- is the
///   end of the packet either way, and is returned to `run_packet` to
///   classify exactly as an unsplit one.
///
/// `hctsiz` is the value `run_split_packet` programmed, needed to re-arm the
/// packet when a NAK sends the sequence back to a fresh start split; it is
/// ignored when `hcsplt` says this is not a split packet.
///
/// `max_split_rounds` is a *soft* budget, and deliberately so: it stops the
/// sequence at the first safe boundary at or after that many rounds, rather
/// than the moment it is reached. A split may only be abandoned once the TT
/// has let go of it -- USB2.0 11.17 requires the host to keep issuing
/// complete splits until the TT answers something other than NYET, so a
/// NAK (the TT discarding its buffer) or a conclusion is the only legal
/// place to walk away.
///
/// Getting this wrong is not a subtle protocol nicety. When an idle
/// keyboard poll gave up as soon as its budget ran out, it left the hub's
/// TT holding a transaction nobody ever collected, and the *next* unrelated
/// control transfer to that hub failed with `XCS_XACT_ERR` -- the hub
/// looked like it had died. It went unnoticed at first only because the
/// frame loop was resetting the whole bus every few seconds anyway, which
/// cleared the wedged TT as a side effect.
fn await_packet(
    hcsplt: u32,
    hctsiz: u32,
    timeout_iterations: u32,
    max_split_rounds: u32,
) -> Option<u32> {
    let mut rounds = 0u32;
    loop {
        if !poll_until(CHAN0_HCINT, HCINT_CHHLTD, true, timeout_iterations) {
            return None;
        }
        let hcint = unsafe { read(CHAN0_HCINT) };
        unsafe { write(CHAN0_HCINT, hcint) }; // W1C

        if hcsplt & HCSPLT_SPLTENA == 0 {
            return Some(hcint);
        }
        // Only a bare handshake keeps a split packet going; anything that
        // concludes it (data moved, STALL, error) is the caller's business.
        if hcint & (HCINT_XFERCOMPL | HCINT_STALL | HCINT_ERROR_MASK) != 0 {
            return Some(hcint);
        }
        rounds += 1;

        // A NAK invalidates the TT's buffer for this transaction, so the
        // next step is a fresh start split rather than another complete
        // split. Anything else (the ACK that accepts a start split, a NYET
        // that says "not yet") continues into the complete-split half.
        let in_complete_split = hcint & HCINT_NAK == 0;

        if !in_complete_split && rounds >= max_split_rounds {
            return None; // out of budget, and the TT has let go: safe to stop
        }
        if rounds >= SPLIT_HARD_ROUND_CAP {
            // Last resort against a wedged TT that answers NYET forever.
            // Leaving mid-sequence is exactly what the doc comment above
            // warns about, so this is set high enough never to be the
            // ordinary way out.
            uart::log(b"USB: giving up mid-split; the hub's TT never answered\r\n");
            return None;
        }
        unsafe {
            if in_complete_split {
                write(CHAN0_HCSPLT, hcsplt | HCSPLT_COMPSPLT);
            } else {
                // Starting over: put back the packet count and PID the core
                // may have consumed on the attempt that just NAKed. HCDMA
                // still points at the same staging buffer, and nothing was
                // transferred, so this re-arms the identical packet.
                write(CHAN0_HCSPLT, hcsplt);
                write(CHAN0_HCTSIZ, hctsiz);
            }
            write(CHAN0_HCINT, 0xFFFF_FFFF);
            modify(CHAN0_HCCHAR, HCCHAR_CHENA | HCCHAR_CHDIS, HCCHAR_CHENA);
        }
    }
}

/// Logs the raw root-port register alongside a failed transfer. A device
/// that has stopped answering says nothing about *why* on its own, while
/// HPRT distinguishes the main cases at a glance: still connected and
/// enabled (bits 0 and 2 set) means the failure is a protocol-level one,
/// a cleared enable bit means the core dropped the port, and
/// prtovrcurract (bit 4) means the board's 5V supply gave up.
fn log_port_state() {
    uart::log_hex(b"USB:   HPRT=", unsafe { read(HPRT) });
}

/// Explicitly requests a channel halt and waits (briefly) for it, so a
/// channel left mid-transaction by a timed-out packet does not race the
/// next packet's configuration. Best-effort: even if the halt never
/// confirms, `HCINT` is still cleared so stale bits cannot be misread as
/// belonging to the next transfer.
fn force_halt_channel() {
    unsafe {
        modify(CHAN0_HCCHAR, HCCHAR_CHDIS, HCCHAR_CHDIS);
    }
    let _ = poll_until(CHAN0_HCINT, HCINT_CHHLTD, true, HALT_CONFIRM_ITERATIONS);
    unsafe {
        write(CHAN0_HCINT, 0xFFFF_FFFF);
    }
}

// ------------------------------------------------------------------------
// Cache sync, timing, and raw MMIO
// ------------------------------------------------------------------------

/// Writes back dirty cache lines over `address..address+length` and
/// invalidates them, matching `sdmmc.rs`'s helper of the same name (same
/// ROM call, same reasoning: the QTD list and transfer buffers here are
/// DMA-shared memory, exactly like SD's IDMAC descriptors).
fn cache_writeback_invalidate(address: usize, length: usize) {
    crate::psram::writeback_invalidate(address, length);
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
#[inline(always)]
unsafe fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
#[inline(always)]
unsafe fn write(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
#[inline(always)]
unsafe fn modify(address: usize, mask: u32, value: u32) {
    unsafe {
        write(address, (read(address) & !mask) | (value & mask));
    }
}
