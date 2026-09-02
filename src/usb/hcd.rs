//! ESP32-P4 High-Speed USB-DWC host controller driver: VBUS power, core
//! bring-up, host port management, and the raw channel/packet primitive
//! everything above this layer (`protocol.rs`, `hid_keyboard.rs`) is built
//! on. This layer knows about registers, channels, and packets -- it does
//! not know what a USB device, descriptor, or endpoint means; `run_packet`
//! runs one single-packet QTD on channel 0 and reports what happened. Failed
//! multi-packet and multi-QTD-list experiments remain isolated below but are
//! not part of the active transfer path.
//!
//! Tab5's USB-A connector is wired to this controller (internal UTMI PHY,
//! dedicated DM/DP pins); the Full-Speed OTG controller on GPIO26/27
//! (Tab5's USB-C port) is out of scope, as is the USB Serial/JTAG
//! controller on GPIO24/25 that `uart.rs` already uses for
//! flashing/logging.
//!
//! This is Stage 1 of `docs/USB_HOST_PLAN.md`: core bring-up and host-port
//! connect/reset/speed detection. `docs/USB_INTERRUPT_REFACTOR_PLAN.md`
//! adds the first interrupt-driven completion layer while preserving the
//! synchronous packet API during migration. Stage 6 added split transactions
//! (`HCSPLT`, set up from
//! `Route` and driven by `await_packet`), so the bus runs at High-Speed
//! and an FS/LS device behind a hub is reached through that hub's TT. The
//! silicon supports them even though Espressif's documentation says it
//! does not -- see `probe_split_support`. Stage 4's
//! `FORCE_FS_LS_ONLY_HOST`, which held the whole bus at Full-Speed to
//! avoid ever needing a split, is kept as a fallback but is off.
//!
//! The USB-A 5V (VBUS) switch is one bit of the second PI4IOE5V6408 I/O
//! expander (I2C address 0x44, "E2"), the counterpart to `lcd.rs`'s PI4IOE1
//! (0x43) which drives the LCD reset line. Confirmed on real hardware with
//! the `usbvbus` shell command: bit 3 raises USB-A's VBUS to 5V
//! (`VBUS_ENABLE_BIT` below).

use crate::delay::{delay_ms, delay_us};
use crate::i2c;
use crate::uart;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Whether the current continuously-disconnected root-port state has
/// already produced its timeout log.  `probe_port` is intentionally
/// stateless otherwise, but it is called periodically while the port is
/// empty, so this small bit prevents that normal polling from flooding the
/// UART.
static NO_DEVICE_TIMEOUT_REPORTED: AtomicBool = AtomicBool::new(false);
/// How many cache writebacks over a DMA buffer the ROM routine has refused,
/// and whether the first one has been described.
///
/// A refusal is silent by construction -- the result is dropped -- and its
/// effect is that the controller reads whatever was in RAM instead of what
/// the CPU just wrote. That is invisible until a device acts on a stale
/// command, so the count is kept and the first one is named.
static CACHE_REFUSALS: AtomicU32 = AtomicU32::new(0);
static CACHE_REFUSAL_REPORTED: AtomicBool = AtomicBool::new(false);

/// Every port event seen since the bus was last brought up, as HPRT bits.
///
/// The port's change bits are write-1-to-clear, and the ISR clears them as
/// part of acknowledging the interrupt. That makes them useless to anything
/// reading HPRT afterwards -- including the failure logs, which is precisely
/// when "did the device drop off the bus?" and "did the 5V switch
/// current-limit?" are the questions worth asking. Latching them here keeps
/// the answer until the next `probe_port`.
static USB_PORT_EVENT_HISTORY: AtomicU32 = AtomicU32::new(0);

/// Port events latched since the bus was last brought up (HPRT bit
/// positions: `prtconndet`, `prtenchng`, `prtovrcurract`, `prtovrcurrchng`).
pub fn port_event_history() -> u32 {
    USB_PORT_EVENT_HISTORY.load(Ordering::Relaxed)
}

/// True if the port ever reported over-current since the last `probe_port`.
///
/// Whether this can ever be true on this board depends on the USB-A 5V
/// switch's fault output actually reaching the controller; a switch whose
/// flag pin is not wired stays silent no matter how hard it current-limits.
/// A `false` here is therefore weak evidence, while a `true` is conclusive.
pub fn port_over_current_seen() -> bool {
    port_event_history() & (HPRT_PRTOVRCURRACT | HPRT_PRTOVRCURRCHNG) != 0
}

/// True if the device dropped off the bus (or the port was disabled) since
/// the last `probe_port` -- the signature of a device that browned out and
/// re-attached, as opposed to one that simply stopped answering.
pub fn port_drop_seen() -> bool {
    port_event_history() & (HPRT_PRTCONNDET | HPRT_PRTENCHNG) != 0
}

/// Set when the bus has been left in a state no further transfer can be
/// expected to survive.
///
/// Only a controller-level failure puts it here: channel 0 could not be
/// halted after a failed packet. This driver runs every control and bulk
/// transfer on that channel, so carrying on would put every device at risk.
///
/// A class/device recovery failure is deliberately not stored here. In
/// particular, a Mass Storage device that no longer answers BOT Reset
/// Recovery has a dead BOT session, but an independently scheduled HID
/// endpoint can still be healthy. The class driver owns that narrower state.
static BUS_UNUSABLE: AtomicBool = AtomicBool::new(false);

/// Records that the bus needs rebuilding before anything else is attempted.
pub(super) fn note_bus_unusable() {
    BUS_UNUSABLE.store(true, Ordering::Release);
}

/// Whether the bus has been marked unusable since the last rescan.
///
/// Class drivers read this to stop driving a controller that is going to
/// fail every remaining step, rather than spending seconds proving it.
pub(super) fn bus_unusable() -> bool {
    BUS_UNUSABLE.load(Ordering::Acquire)
}

/// Consumes the flag, so one failure produces one escalation.
pub(super) fn take_bus_unusable() -> bool {
    BUS_UNUSABLE.swap(false, Ordering::AcqRel)
}

// The ISR only snapshots and acknowledges hardware. Foreground code owns
// every transfer state transition and consumes `CHANNEL0_PENDING`; counters
// remain monotonic across rescans so `usbhw` can expose interrupt storms.
static USB_INTERRUPT_COUNT: AtomicU32 = AtomicU32::new(0);
static USB_CHANNEL0_INTERRUPT_COUNT: AtomicU32 = AtomicU32::new(0);
static USB_PERIODIC_INTERRUPT_COUNT: [AtomicU32; 4] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];
static USB_PORT_INTERRUPT_COUNT: AtomicU32 = AtomicU32::new(0);
static USB_SPURIOUS_INTERRUPT_COUNT: AtomicU32 = AtomicU32::new(0);
static USB_CHANNEL0_PENDING: AtomicU32 = AtomicU32::new(0);
static USB_PERIODIC_PENDING: [AtomicU32; 4] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];
static USB_PORT_PENDING: AtomicU32 = AtomicU32::new(0);
static USB_LAST_GINTSTS: AtomicU32 = AtomicU32::new(0);
static USB_LAST_HAINT: AtomicU32 = AtomicU32::new(0);
static USB_LAST_HCINT0: AtomicU32 = AtomicU32::new(0);
static USB_LAST_PERIODIC_HCINT: [AtomicU32; 4] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];
static USB_LAST_HPRT: AtomicU32 = AtomicU32::new(0);
static USB_SLEEP_WAIT_COUNT: AtomicU32 = AtomicU32::new(0);
static USB_POLL_WAIT_COUNT: AtomicU32 = AtomicU32::new(0);
static USB_WFI_COUNT: AtomicU32 = AtomicU32::new(0);
static USB_LAST_WAIT_CYCLES: AtomicU32 = AtomicU32::new(0);
static USB_MAX_WAIT_CYCLES: AtomicU32 = AtomicU32::new(0);
static USB_TRANSFER_GENERATION: AtomicU32 = AtomicU32::new(0);
static USB_SUBMIT_COUNT: AtomicU32 = AtomicU32::new(0);
/// Raw completion and QTD control word from the most recently reaped
/// channel-0 descriptor. BOT reads these immediately when a short response
/// has an impossible length, tying the protocol symptom to the exact HCD
/// state without logging every successful packet.
static USB_LAST_REAP_HCINT: AtomicU32 = AtomicU32::new(0);
static USB_LAST_REAP_QTD_CONTROL: AtomicU32 = AtomicU32::new(0);
static USB_REAP_COUNT: AtomicU32 = AtomicU32::new(0);
static USB_CANCEL_COUNT: AtomicU32 = AtomicU32::new(0);
static USB_STALE_TOKEN_COUNT: AtomicU32 = AtomicU32::new(0);
static PERIODIC_HID_ACTIVE_MASK: AtomicU32 = AtomicU32::new(0);
static PERIODIC_HID_GENERATION: [AtomicU32; 4] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];
static PERIODIC_HID_MPS: [AtomicU32; 4] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];
static PERIODIC_HID_INTERVAL: [AtomicU32; 4] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];
static PERIODIC_HID_PID_DATA1: [AtomicBool; 4] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];
static PERIODIC_HID_COMPLETION_COUNT: AtomicU32 = AtomicU32::new(0);
static PERIODIC_HID_REARM_COUNT: AtomicU32 = AtomicU32::new(0);
static PERIODIC_HID_ERROR_COUNT: AtomicU32 = AtomicU32::new(0);
static SPLIT_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);
static SPLIT_PACKET_COUNT: AtomicU32 = AtomicU32::new(0);
static SPLIT_ROUND_COUNT: AtomicU32 = AtomicU32::new(0);
static SPLIT_MODE_CONFLICT_COUNT: AtomicU32 = AtomicU32::new(0);
static DIRECT_BUFFER_PACKET_COUNT: AtomicU32 = AtomicU32::new(0);
static DIRECT_BUFFER_NAK_COUNT: AtomicU32 = AtomicU32::new(0);
static CHANNEL0_QTD_NEXT: AtomicU32 = AtomicU32::new(0);

// ------------------------------------------------------------------------
// Stage 0 observation contract (`docs/USB_BOT_HCD_REFACTOR_PLAN.md`)
//
// Nothing below changes what a transfer does. It exists so that the
// proactive host cleanup this driver still performs at healthy Bulk-Only
// Transport command boundaries can eventually be removed against measured
// numbers rather than against the impression that a run felt stable: every
// later stage is compared against a baseline taken with these counters and
// the current cleanup in place. A counter that is only maintained once the
// behaviour has already changed cannot produce that comparison.
// ------------------------------------------------------------------------

/// Which transfer a DMA buffer belongs to, published by the layer that owns
/// that transfer.
///
/// The HCD never interprets this and never branches on it. It stores the
/// label and reports it back, so that a refused cache maintenance call or a
/// failed packet names the transfer that was running instead of only an
/// address. An address on its own does not say whether the controller was
/// about to read a command block or publish a status wrapper, and that is
/// exactly the distinction the cleanup removal has to be judged on.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum TransferLabel {
    /// Nothing has claimed the channel since boot or since the last owner
    /// released it. Refusals counted here mean an unattributed transfer.
    #[default]
    Unlabelled,
    /// EP0, any stage of a control transfer.
    Control,
    /// The 31-byte command block a transport sends first.
    CommandBlock,
    /// A command's data phase, device to host.
    DataIn,
    /// A command's data phase, host to device.
    DataOut,
    /// The 13-byte status wrapper a transport reads last.
    CommandStatus,
    /// An Interrupt IN report, whether persistent-periodic or the
    /// channel-0 fallback poll.
    InterruptIn,
}

const TRANSFER_LABEL_COUNT: usize = 7;

impl TransferLabel {
    fn index(self) -> usize {
        match self {
            Self::Unlabelled => 0,
            Self::Control => 1,
            Self::CommandBlock => 2,
            Self::DataIn => 3,
            Self::DataOut => 4,
            Self::CommandStatus => 5,
            Self::InterruptIn => 6,
        }
    }

    fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Control,
            2 => Self::CommandBlock,
            3 => Self::DataIn,
            4 => Self::DataOut,
            5 => Self::CommandStatus,
            6 => Self::InterruptIn,
            _ => Self::Unlabelled,
        }
    }

    /// A short, stable name for prose: log lines and the `phase=` field.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unlabelled => "none",
            Self::Control => "control",
            Self::CommandBlock => "CBW",
            Self::DataIn => "data-IN",
            Self::DataOut => "data-OUT",
            Self::CommandStatus => "CSW",
            Self::InterruptIn => "intr-IN",
        }
    }

    /// The column heading for the `usbhw` breakdown, where seven of these
    /// share one 80-column line. The first baseline run lost the last
    /// column off the end of the line, which is the one failure mode a
    /// fixed-format counter block must not have.
    pub fn short_name(self) -> &'static str {
        match self {
            Self::Unlabelled => "none",
            Self::Control => "ctl",
            Self::CommandBlock => "cbw",
            Self::DataIn => "din",
            Self::DataOut => "dout",
            Self::CommandStatus => "csw",
            Self::InterruptIn => "int",
        }
    }
}

static TRANSFER_LABEL: AtomicU32 = AtomicU32::new(0);

/// Names the transfer the next packets belong to. Purely a diagnostic
/// label: see [`TransferLabel`].
pub fn set_transfer_label(label: TransferLabel) {
    TRANSFER_LABEL.store(label.index() as u32, Ordering::Relaxed);
}

fn transfer_label() -> TransferLabel {
    TransferLabel::from_index(TRANSFER_LABEL.load(Ordering::Relaxed))
}

/// Which DMA-shared object a cache maintenance call covers.
///
/// A refusal leaves the CPU holding its own copy of the span, so the
/// consequence differs per object: a refused QTD writeback means the
/// controller may fetch a stale descriptor, while a refused payload
/// invalidate means this driver publishes pre-DMA bytes upwards. Counting
/// them apart is what tells those two failures apart after the fact.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CacheSite {
    /// The channel-0 queue transfer descriptor.
    Channel0Qtd,
    /// A channel-0 payload span.
    Channel0Payload,
    /// The buffer-DMA staging buffer used for split packets.
    SplitStaging,
    /// A persistent periodic HID descriptor or its report buffer.
    Periodic,
    /// The periodic frame list.
    PeriodicFrameList,
    /// The one-shot periodic probe's stack-owned descriptor or buffer.
    PeriodicProbe,
}

const CACHE_SITE_COUNT: usize = 6;

impl CacheSite {
    fn index(self) -> usize {
        match self {
            Self::Channel0Qtd => 0,
            Self::Channel0Payload => 1,
            Self::SplitStaging => 2,
            Self::Periodic => 3,
            Self::PeriodicFrameList => 4,
            Self::PeriodicProbe => 5,
        }
    }
}

/// Which way the bytes a cache maintenance call covers are about to move.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CacheDirection {
    /// Bytes the controller is about to read out of memory.
    HostToDevice,
    /// Bytes the controller has just written into memory.
    DeviceToHost,
    /// A descriptor or frame list, which the controller both reads and
    /// writes back within one transfer.
    Descriptor,
}

const CACHE_DIRECTION_COUNT: usize = 3;

impl CacheDirection {
    fn index(self) -> usize {
        match self {
            Self::HostToDevice => 0,
            Self::DeviceToHost => 1,
            Self::Descriptor => 2,
        }
    }
}

static CACHE_REFUSALS_BY_SITE: [AtomicU32; CACHE_SITE_COUNT] =
    [const { AtomicU32::new(0) }; CACHE_SITE_COUNT];
static CACHE_REFUSALS_BY_LABEL: [AtomicU32; TRANSFER_LABEL_COUNT] =
    [const { AtomicU32::new(0) }; TRANSFER_LABEL_COUNT];
static CACHE_REFUSALS_BY_DIRECTION: [AtomicU32; CACHE_DIRECTION_COUNT] =
    [const { AtomicU32::new(0) }; CACHE_DIRECTION_COUNT];
static LAST_CACHE_REFUSAL_ADDRESS: AtomicU32 = AtomicU32::new(0);
static LAST_CACHE_REFUSAL_LENGTH: AtomicU32 = AtomicU32::new(0);
static LAST_CACHE_REFUSAL_LABEL: AtomicU32 = AtomicU32::new(0);

/// Why one packet did not complete.
///
/// The three transport-level causes (timeout, STALL, transaction error) are
/// the ones a device or a cable can produce. The rest are contract
/// violations inside this driver's own completion accounting, and are the
/// ones the later stages of the refactor exist to remove; keeping them in
/// separate counters is what makes "the cleanup was hiding a real defect"
/// distinguishable from "the bus is noisy".
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum PacketFailureKind {
    #[default]
    None,
    /// The channel never halted within the caller's budget.
    HaltTimeout,
    Stall,
    /// `HCINT` reported CRC/babble/transaction error.
    TransactionError,
    /// QTD status 1: a packet-level failure, excessive NAK included.
    QtdPacketError,
    /// QTD status 2 or 3: a buffer or reserved status this driver has no
    /// meaning for.
    QtdInvalidStatus,
    /// The channel halted without `XferCompl`, or with the descriptor still
    /// owned by hardware.
    NotTransferComplete,
    /// A completion arrived for a generation this slot no longer holds.
    StaleCompletion,
    /// An OUT packet completed with fewer bytes than it was given.
    ShortOut,
    /// A DMA buffer or descriptor could not be cache-synchronized.
    CacheSyncRefused,
    /// A split packet could not start, or its scheduler saw no frame
    /// progress, or its length exceeded the staging buffer.
    SplitRejected,
}

pub const PACKET_FAILURE_KIND_COUNT: usize = 11;

impl PacketFailureKind {
    fn index(self) -> usize {
        match self {
            Self::None => 0,
            Self::HaltTimeout => 1,
            Self::Stall => 2,
            Self::TransactionError => 3,
            Self::ShortOut => 4,
            Self::CacheSyncRefused => 5,
            Self::QtdPacketError => 6,
            Self::QtdInvalidStatus => 7,
            Self::NotTransferComplete => 8,
            Self::StaleCompletion => 9,
            Self::SplitRejected => 10,
        }
    }

    fn from_index(index: u32) -> Self {
        match index {
            1 => Self::HaltTimeout,
            2 => Self::Stall,
            3 => Self::TransactionError,
            4 => Self::ShortOut,
            5 => Self::CacheSyncRefused,
            6 => Self::QtdPacketError,
            7 => Self::QtdInvalidStatus,
            8 => Self::NotTransferComplete,
            9 => Self::StaleCompletion,
            10 => Self::SplitRejected,
            _ => Self::None,
        }
    }

    /// A short, stable name for prose: log lines and the `last-fail=` field.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::HaltTimeout => "halt-timeout",
            Self::Stall => "STALL",
            Self::TransactionError => "xact-error",
            Self::ShortOut => "short-OUT",
            Self::CacheSyncRefused => "cache-sync-refused",
            Self::QtdPacketError => "QTD-packet-error",
            Self::QtdInvalidStatus => "QTD-bad-status",
            Self::NotTransferComplete => "not-XferCompl",
            Self::StaleCompletion => "stale-completion",
            Self::SplitRejected => "split-rejected",
        }
    }

    /// The column heading for the `usbhw` breakdown. See
    /// [`TransferLabel::short_name`] for why these are separate.
    pub fn short_name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::HaltTimeout => "timeout",
            Self::Stall => "stall",
            Self::TransactionError => "xact",
            Self::ShortOut => "shortout",
            Self::CacheSyncRefused => "cache",
            Self::QtdPacketError => "qtderr",
            Self::QtdInvalidStatus => "qtdbad",
            Self::NotTransferComplete => "nocpl",
            Self::StaleCompletion => "stale",
            Self::SplitRejected => "splitrej",
        }
    }
}

static PACKET_FAILURES_BY_KIND: [AtomicU32; PACKET_FAILURE_KIND_COUNT] =
    [const { AtomicU32::new(0) }; PACKET_FAILURE_KIND_COUNT];
/// Interrupt IN polls that ran out their budget with nothing to report.
///
/// Counted apart from every failure kind, and deliberately not added to the
/// failure total. `SET_IDLE(0)` means an idle keyboard NAKs until a key
/// moves, so this is what a working idle HID looks like -- the first
/// baseline run counted 1131 of them as packet failures on a bus where
/// nothing was wrong, which buries the two real failures beside them.
static IDLE_POLL_TIMEOUTS: AtomicU32 = AtomicU32::new(0);
/// Descriptor readbacks whose remainder exceeded the requested length.
///
/// Not a failure on its own -- it is how this core reports some packet
/// errors -- but it is the reason no byte count may be taken on trust.
static IMPOSSIBLE_REMAINDERS: AtomicU32 = AtomicU32::new(0);
static PACKET_FAILURE_TOTAL: AtomicU32 = AtomicU32::new(0);
static LAST_FAILED_PACKET_KIND: AtomicU32 = AtomicU32::new(0);
static LAST_FAILED_PACKET_LABEL: AtomicU32 = AtomicU32::new(0);
static LAST_FAILED_PACKET_REQUESTED: AtomicU32 = AtomicU32::new(0);
static LAST_FAILED_PACKET_ACTUAL: AtomicU32 = AtomicU32::new(0);
static LAST_FAILED_PACKET_HCINT: AtomicU32 = AtomicU32::new(0);
static LAST_FAILED_PACKET_QTD: AtomicU32 = AtomicU32::new(0);
static LAST_FAILED_PACKET_IS_IN: AtomicBool = AtomicBool::new(false);

/// Records one failed packet with the numbers a later stage needs to say
/// what the hardware actually did: what was asked for, what moved, the
/// `HCINT` that ended the packet, and the descriptor's final control word.
///
/// Called only on the failure path, so the cost never lands on a healthy
/// transfer. `qtd_final` is 0 where the failure happened before any
/// descriptor was written back (a split packet uses buffer DMA and has
/// none at all).
fn note_packet_failure(
    kind: PacketFailureKind,
    is_in: bool,
    requested: usize,
    actual: usize,
    hcint: u32,
    qtd_final: u32,
) {
    let label = transfer_label();
    PACKET_FAILURES_BY_KIND[kind.index()].fetch_add(1, Ordering::Relaxed);
    PACKET_FAILURE_TOTAL.fetch_add(1, Ordering::Relaxed);
    LAST_FAILED_PACKET_LABEL.store(label.index() as u32, Ordering::Relaxed);
    LAST_FAILED_PACKET_REQUESTED.store(requested as u32, Ordering::Relaxed);
    LAST_FAILED_PACKET_ACTUAL.store(actual as u32, Ordering::Relaxed);
    LAST_FAILED_PACKET_HCINT.store(hcint, Ordering::Relaxed);
    LAST_FAILED_PACKET_QTD.store(qtd_final, Ordering::Relaxed);
    LAST_FAILED_PACKET_IS_IN.store(is_in, Ordering::Relaxed);
    // Published last, so a reader never sees a new kind beside stale fields.
    LAST_FAILED_PACKET_KIND.store(kind.index() as u32, Ordering::Release);
}

/// Counts one Interrupt IN poll that expired with no report.
fn note_idle_poll_timeout() {
    IDLE_POLL_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
}

/// Writes the requested/actual pair and the raw hardware words for a packet
/// that has just failed.
///
/// The existing failure logs say *that* a packet failed and which HCINT
/// ended it. What they never said is how many of the requested bytes had
/// already moved, which is the number that decides whether a retry can
/// resend the packet without publishing the same bytes twice.
fn log_packet_failure(kind: PacketFailureKind, requested: usize, actual: usize, qtd_final: u32) {
    uart::log(b"USB:   failure=");
    uart::log(kind.name().as_bytes());
    uart::log(b" phase=");
    uart::log(transfer_label().name().as_bytes());
    uart::log(b"\r\n");
    uart::log_u32(b"USB:   requested bytes=", requested as u32);
    uart::log_u32(b"USB:   actual bytes=", actual as u32);
    uart::log_hex(b"USB:   QTD final=", qtd_final);
}

/// Which FIFO a flush timed out on. The three are shared very differently:
/// the non-periodic TX FIFO belongs to channel 0 alone, while the periodic
/// TX and RX FIFOs are shared with every armed Interrupt endpoint.
const FIFO_NON_PERIODIC_TX: usize = 0;
const FIFO_PERIODIC_TX: usize = 1;
const FIFO_RX: usize = 2;
const FIFO_COUNT: usize = 3;

static FIFO_FLUSH_TIMEOUTS: [AtomicU32; FIFO_COUNT] = [const { AtomicU32::new(0) }; FIFO_COUNT];
static FIFO_FLUSHES_SKIPPED_FOR_PERIODIC: AtomicU32 = AtomicU32::new(0);
/// Reported channel-0 Bulk OUT packet errors recovered by flushing only the
/// FIFO that can contain that packet's transmit residue.
///
/// This counter distinguishes the directional Stage 3 experiment from both
/// the old all-FIFO recovery and the rejected no-cleanup experiment.
static OUT_PACKET_ERROR_NPTX_CLEANUPS: AtomicU32 = AtomicU32::new(0);

fn note_fifo_flush_timeout(fifo: usize) {
    FIFO_FLUSH_TIMEOUTS[fifo].fetch_add(1, Ordering::Relaxed);
}

/// Everything the Stage 0 baseline is compared on, in one snapshot so that
/// a `usbhw` taken before and after an acceptance run describes the same
/// instant for every counter.
#[derive(Clone, Copy, Default)]
pub struct HostObservation {
    pub cache_refusals: u32,
    pub cache_refusals_by_label: [u32; TRANSFER_LABEL_COUNT],
    pub cache_refusals_by_site: [u32; CACHE_SITE_COUNT],
    pub cache_refusals_by_direction: [u32; CACHE_DIRECTION_COUNT],
    pub last_cache_refusal_address: u32,
    pub last_cache_refusal_length: u32,
    pub last_cache_refusal_label: TransferLabel,
    pub packet_failures: u32,
    pub packet_failures_by_kind: [u32; PACKET_FAILURE_KIND_COUNT],
    /// Expired Interrupt IN polls, which are not failures. See
    /// [`IDLE_POLL_TIMEOUTS`].
    pub idle_poll_timeouts: u32,
    /// Descriptor readbacks whose remainder exceeded the request.
    pub impossible_remainders: u32,
    pub last_packet_failure_kind: PacketFailureKind,
    pub last_packet_failure_label: TransferLabel,
    pub last_packet_failure_is_in: bool,
    pub last_packet_failure_requested: u32,
    pub last_packet_failure_actual: u32,
    pub last_packet_failure_hcint: u32,
    pub last_packet_failure_qtd: u32,
    pub fifo_flush_timeouts: [u32; FIFO_COUNT],
    pub fifo_flushes_skipped_for_periodic: u32,
    pub out_packet_error_nptx_cleanups: u32,
}

/// Live host FIFO partition in 32-bit lines.
#[derive(Clone, Copy)]
pub struct FifoConfiguration {
    pub rx_lines: u32,
    pub non_periodic_tx_lines: u32,
    pub periodic_tx_lines: u32,
}

pub fn fifo_configuration() -> FifoConfiguration {
    FifoConfiguration {
        rx_lines: unsafe { read(GRXFSIZ) } & 0xFFFF,
        non_periodic_tx_lines: unsafe { read(GNPTXFSIZ) } >> 16,
        periodic_tx_lines: unsafe { read(HPTXFSIZ) } >> 16,
    }
}

/// The Stage 0 baseline counters. Names for the array positions are
/// [`cache_site_names`], [`cache_direction_names`], [`transfer_label_names`]
/// and [`packet_failure_kind_names`], so a caller printing them does not
/// have to keep its own copy of this driver's ordering.
pub fn host_observation() -> HostObservation {
    let mut observation = HostObservation {
        cache_refusals: CACHE_REFUSALS.load(Ordering::Relaxed),
        last_cache_refusal_address: LAST_CACHE_REFUSAL_ADDRESS.load(Ordering::Relaxed),
        last_cache_refusal_length: LAST_CACHE_REFUSAL_LENGTH.load(Ordering::Relaxed),
        last_cache_refusal_label: TransferLabel::from_index(
            LAST_CACHE_REFUSAL_LABEL.load(Ordering::Relaxed),
        ),
        packet_failures: PACKET_FAILURE_TOTAL.load(Ordering::Relaxed),
        idle_poll_timeouts: IDLE_POLL_TIMEOUTS.load(Ordering::Relaxed),
        impossible_remainders: IMPOSSIBLE_REMAINDERS.load(Ordering::Relaxed),
        last_packet_failure_kind: PacketFailureKind::from_index(
            LAST_FAILED_PACKET_KIND.load(Ordering::Acquire),
        ),
        last_packet_failure_label: TransferLabel::from_index(
            LAST_FAILED_PACKET_LABEL.load(Ordering::Relaxed),
        ),
        last_packet_failure_is_in: LAST_FAILED_PACKET_IS_IN.load(Ordering::Relaxed),
        last_packet_failure_requested: LAST_FAILED_PACKET_REQUESTED.load(Ordering::Relaxed),
        last_packet_failure_actual: LAST_FAILED_PACKET_ACTUAL.load(Ordering::Relaxed),
        last_packet_failure_hcint: LAST_FAILED_PACKET_HCINT.load(Ordering::Relaxed),
        last_packet_failure_qtd: LAST_FAILED_PACKET_QTD.load(Ordering::Relaxed),
        fifo_flushes_skipped_for_periodic: FIFO_FLUSHES_SKIPPED_FOR_PERIODIC
            .load(Ordering::Relaxed),
        out_packet_error_nptx_cleanups: OUT_PACKET_ERROR_NPTX_CLEANUPS.load(Ordering::Relaxed),
        ..Default::default()
    };
    for (index, counter) in CACHE_REFUSALS_BY_LABEL.iter().enumerate() {
        observation.cache_refusals_by_label[index] = counter.load(Ordering::Relaxed);
    }
    for (index, counter) in CACHE_REFUSALS_BY_SITE.iter().enumerate() {
        observation.cache_refusals_by_site[index] = counter.load(Ordering::Relaxed);
    }
    for (index, counter) in CACHE_REFUSALS_BY_DIRECTION.iter().enumerate() {
        observation.cache_refusals_by_direction[index] = counter.load(Ordering::Relaxed);
    }
    for (index, counter) in PACKET_FAILURES_BY_KIND.iter().enumerate() {
        observation.packet_failures_by_kind[index] = counter.load(Ordering::Relaxed);
    }
    for (index, counter) in FIFO_FLUSH_TIMEOUTS.iter().enumerate() {
        observation.fifo_flush_timeouts[index] = counter.load(Ordering::Relaxed);
    }
    observation
}

/// Names for [`HostObservation::cache_refusals_by_site`], in order.
pub fn cache_site_names() -> [&'static str; CACHE_SITE_COUNT] {
    ["qtd", "buf", "split", "per", "flist", "probe"]
}

/// Names for [`HostObservation::cache_refusals_by_direction`], in order.
pub fn cache_direction_names() -> [&'static str; CACHE_DIRECTION_COUNT] {
    ["out", "in", "desc"]
}

/// Column headings for [`HostObservation::cache_refusals_by_label`].
pub fn transfer_label_names() -> [&'static str; TRANSFER_LABEL_COUNT] {
    [
        TransferLabel::Unlabelled.short_name(),
        TransferLabel::Control.short_name(),
        TransferLabel::CommandBlock.short_name(),
        TransferLabel::DataIn.short_name(),
        TransferLabel::DataOut.short_name(),
        TransferLabel::CommandStatus.short_name(),
        TransferLabel::InterruptIn.short_name(),
    ]
}

/// The position of one kind in [`HostObservation::packet_failures_by_kind`].
///
/// Exported so a caller checking one specific kind names it rather than
/// searching the heading table for a string: a renamed heading would then
/// silently select the "no failure" placeholder, whose count is always zero.
pub fn packet_failure_kind_index(kind: PacketFailureKind) -> usize {
    kind.index()
}

/// Column headings for [`HostObservation::packet_failures_by_kind`].
pub fn packet_failure_kind_names() -> [&'static str; PACKET_FAILURE_KIND_COUNT] {
    [
        PacketFailureKind::None.short_name(),
        PacketFailureKind::HaltTimeout.short_name(),
        PacketFailureKind::Stall.short_name(),
        PacketFailureKind::TransactionError.short_name(),
        PacketFailureKind::ShortOut.short_name(),
        PacketFailureKind::CacheSyncRefused.short_name(),
        PacketFailureKind::QtdPacketError.short_name(),
        PacketFailureKind::QtdInvalidStatus.short_name(),
        PacketFailureKind::NotTransferComplete.short_name(),
        PacketFailureKind::StaleCompletion.short_name(),
        PacketFailureKind::SplitRejected.short_name(),
    ]
}

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

/// Switches USB-A's 5V rail without the caller having to know which expander
/// bit carries it. `probe_port` turns the rail back on itself, so a caller
/// that wants to measure a device coming up from cold only has to switch it
/// off, discard every session, and rescan.
pub fn set_vbus_power(on: bool) -> bool {
    set_vbus(on)
}

/// Fully removes and restores USB-A device power. A root-port reset does not
/// discharge a hub or MSC, so a device-side EP0/TT state can otherwise
/// survive every software rescan. Callers must discard all live USB sessions
/// before invoking this function.
pub(super) fn power_cycle_vbus() -> bool {
    if !set_vbus(false) {
        return false;
    }
    delay_ms(1_000);
    if !set_vbus(true) {
        return false;
    }
    delay_ms(250);
    true
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

/// Reads one PI4IOE2 register. Exposed so `sdio.rs` can log the expander's
/// direction/high-impedance/output state when the ESP32-C6 does not answer
/// on the SDIO bus (its power line is E2's P0).
pub fn pi4ioe2_register(register: u8) -> Option<u8> {
    pi4ioe2_read(register)
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
const GINTSTS: usize = USB_DWC_HS + 0x14;
const GINTMSK: usize = USB_DWC_HS + 0x18;
const GRXFSIZ: usize = USB_DWC_HS + 0x24;
const GNPTXFSIZ: usize = USB_DWC_HS + 0x28;
const GSNPSID: usize = USB_DWC_HS + 0x40;
const GHWCFG1: usize = USB_DWC_HS + 0x44;
const GHWCFG2: usize = USB_DWC_HS + 0x48;
const GHWCFG3: usize = USB_DWC_HS + 0x4C;
const GHWCFG4: usize = USB_DWC_HS + 0x50;
const HPTXFSIZ: usize = USB_DWC_HS + 0x100;
const HCFG: usize = USB_DWC_HS + 0x400;
const HFNUM: usize = USB_DWC_HS + 0x408;
const HAINT: usize = USB_DWC_HS + 0x414;
const HAINTMSK: usize = USB_DWC_HS + 0x418;
const HFLBADDR: usize = USB_DWC_HS + 0x41C;
const HPRT: usize = USB_DWC_HS + 0x440;

const GAHBCFG_GLBLINTRMSK: u32 = 1 << 0;
const GAHBCFG_DMAEN: u32 = 1 << 5;
const GAHBCFG_HBSTLEN_MASK: u32 = 0xF << 1;

const GINT_HCHINT: u32 = 1 << 25;
const GINT_PRTINT: u32 = 1 << 24;
const GINT_DISCONNINT: u32 = 1 << 29;
const GINT_ENABLED_MASK: u32 = GINT_HCHINT | GINT_PRTINT | GINT_DISCONNINT;

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
const HCFG_FRLISTEN_MASK: u32 = 0x3 << 24;
const HCFG_FRLISTEN_32: u32 = 0x2 << 24;
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
static FORCE_FS_LS_ONLY_HOST_RUNTIME: AtomicBool = AtomicBool::new(FORCE_FS_LS_ONLY_HOST);

/// Current runtime value of the diagnostic FS/LS-only host mode.
pub fn fs_ls_only_host_forced() -> bool {
    FORCE_FS_LS_ONLY_HOST_RUNTIME.load(Ordering::Acquire)
}

/// Selects the speed policy applied by the next root-port probe/reset.
/// Foreground must rescan after changing it; the shell's `usbfs` command
/// does both as one operation.
pub fn set_fs_ls_only_host_forced(forced: bool) {
    FORCE_FS_LS_ONLY_HOST_RUNTIME.store(forced, Ordering::Release);
}

const HPRT_PRTCONNSTS: u32 = 1 << 0;
const HPRT_PRTENA: u32 = 1 << 2;
// A device that stops answering because the board's 5V switch
// current-limited looks exactly like one that stopped answering for
// protocol reasons -- except for these bits. `prtovrcurract` is a live
// level, while `prtconndet`/`prtenchng`/`prtovrcurrchng` are the W1C events
// the ISR consumes, which is why they are latched into
// `USB_PORT_EVENT_HISTORY` rather than read back afterwards.
const HPRT_PRTCONNDET: u32 = 1 << 1;
const HPRT_PRTENCHNG: u32 = 1 << 3;
const HPRT_PRTOVRCURRACT: u32 = 1 << 4;
const HPRT_PRTOVRCURRCHNG: u32 = 1 << 5;
const HPRT_PRTRST: u32 = 1 << 8;
const HPRT_PRTPWR: u32 = 1 << 12;
const HPRT_PRTSPD_SHIFT: u32 = 17;
const HPRT_PRTSPD_MASK: u32 = 0x3 << HPRT_PRTSPD_SHIFT;
// Write-1-to-clear status bits (prtconndet, prtena, prtenchng,
// prtovrcurrchng) that must be preserved as 0 whenever a non-W1C field
// (prtpwr, prtrst, ...) is written, or a stale status bit gets cleared as a
// side effect.
const HPRT_W1C_MASK: u32 = (1 << 1) | (1 << 2) | (1 << 3) | (1 << 5);

// Host channel registers. Channel 0 owns synchronous control/bulk work;
// channels 1..=4 are fixed periodic HID slots.
const HOST_CHANS: usize = USB_DWC_HS + 0x500; // channel stride is 0x20; channel 0 needs no offset
// Offset 0x04 is HCSPLT, the split-transaction control register. Nothing
// here programs it yet, but -- contrary to every piece of Espressif
// documentation -- it is present and functional on this silicon. See
// `probe_split_support` for the measurement.
const CHAN0_HCSPLT: usize = HOST_CHANS + 0x04;
const CHAN0_HCCHAR: usize = HOST_CHANS;
const CHAN0_HCINT: usize = HOST_CHANS + 0x08;
const CHAN0_HCINTMSK: usize = HOST_CHANS + 0x0C;
const CHAN0_HCTSIZ: usize = HOST_CHANS + 0x10;
const CHAN0_HCDMA: usize = HOST_CHANS + 0x14;
const CHAN1_HCCHAR: usize = HOST_CHANS + 0x20;
const CHAN1_HCSPLT: usize = HOST_CHANS + 0x24;
const CHAN1_HCINT: usize = HOST_CHANS + 0x28;
const CHAN1_HCINTMSK: usize = HOST_CHANS + 0x2C;
const CHAN1_HCTSIZ: usize = HOST_CHANS + 0x30;
const CHAN1_HCDMA: usize = HOST_CHANS + 0x34;

const fn channel_register(channel: usize, offset: usize) -> usize {
    HOST_CHANS + channel * 0x20 + offset
}

const HCCHAR_OFFSET: usize = 0x00;
const HCSPLT_OFFSET: usize = 0x04;
const HCINT_OFFSET: usize = 0x08;
const HCINTMSK_OFFSET: usize = 0x0C;
const HCTSIZ_OFFSET: usize = 0x10;
const HCDMA_OFFSET: usize = 0x14;

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
pub const HCCHAR_EPTYPE_INTR: u32 = 3 << 18;

/// HCCHAR bits[21:20], the field the databook calls MC/EC. With
/// `HCSPLT.SpltEna` clear it is a periodic multi-count. Direct descriptor-DMA
/// transfers leave it at 0, including a multi-entry Bulk QTD list: ESP-IDF
/// v5.5.3's `usb_dwc_ll_hcchar_init` does not set this field. Setting it to 1
/// on every descriptor transfer made a Full-Speed Bulk IN session fail after
/// four reads, and limiting it to a 512-byte WRITE QTD did not improve that
/// transfer either. Direct buffer DMA needs 1 to launch its one programmed
/// transaction. With SpltEna set it becomes the split transaction's retry
/// count, which the databook requires to be at least 1.
const HCCHAR_MC_ONE: u32 = 1 << 20;

// Bounds `force_halt_channel`'s wait for the halt it explicitly requested;
// short, since by that point the transfer has already been given up on
// and this is just cleanup.
const HALT_CONFIRM_ITERATIONS: u32 = 5_000;

// The old timeout unit was one foreground loop containing atomics plus an
// HCINT MMIO read. Eight CPU cycles is deliberately conservative: successful
// interrupt waits return on the USB IRQ, while a missing IRQ is still bounded
// by approximately the same order of wall-clock time as the former loop.
const WAIT_TIMEOUT_CYCLES_PER_ITERATION: u32 = 8;

const HCINT_XFERCOMPL: u32 = 1 << 0;
const HCINT_CHHLTD: u32 = 1 << 1;
const HCINT_STALL: u32 = 1 << 3;
const HCINT_NAK: u32 = 1 << 4;
const HCINT_NYET: u32 = 1 << 6;
const HCINT_XACTERR: u32 = 1 << 7;
const HCINT_BBLERR: u32 = 1 << 8;
const HCINT_XCS_XACT_ERR: u32 = 1 << 12;
const HCINT_ERROR_MASK: u32 = HCINT_STALL | HCINT_XACTERR | HCINT_BBLERR | HCINT_XCS_XACT_ERR;
// Matches ESP-IDF v5.5.3's channel mask and additionally retains every
// handshake needed by this driver's software-driven split state machine.
const HCINT_ENABLED_MASK: u32 = 0x0000_3FFF;

/// Minimal USB-DWC ISR: acknowledge hardware and publish raw snapshots.
///
/// Transfer parsing, cache maintenance, retries, logging, and port state
/// transitions all remain foreground work. Keeping this function limited to
/// MMIO and atomics makes it safe to call from the shared IRAM trap entry.
#[unsafe(link_section = ".iram.text.critical.usb.interrupt")]
pub(crate) fn handle_interrupt() {
    unsafe {
        let active = read(GINTSTS) & read(GINTMSK);
        USB_LAST_GINTSTS.store(active, Ordering::Relaxed);
        if active == 0 {
            USB_SPURIOUS_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);
            return;
        }

        if active & GINT_HCHINT != 0 {
            let channels = read(HAINT) & read(HAINTMSK);
            USB_LAST_HAINT.store(channels, Ordering::Relaxed);
            if channels & 1 != 0 {
                let hcint = read(CHAN0_HCINT) & read(CHAN0_HCINTMSK);
                if hcint != 0 {
                    write(CHAN0_HCINT, hcint);
                    USB_LAST_HCINT0.store(hcint, Ordering::Relaxed);
                    USB_CHANNEL0_PENDING.fetch_or(hcint, Ordering::Release);
                    USB_CHANNEL0_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);
                } else {
                    USB_SPURIOUS_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);
                }
            }
            handle_periodic_channel_interrupt(channels, 1, 0);
            handle_periodic_channel_interrupt(channels, 2, 1);
            handle_periodic_channel_interrupt(channels, 3, 2);
            handle_periodic_channel_interrupt(channels, 4, 3);
        }

        if active & (GINT_PRTINT | GINT_DISCONNINT) != 0 {
            let hprt = read(HPRT);
            USB_LAST_HPRT.store(hprt, Ordering::Relaxed);
            let changes = hprt & (HPRT_W1C_MASK & !HPRT_PRTENA);
            // Latched separately from `USB_PORT_PENDING`, which is consumed
            // by whoever takes the hotplug edge; this one survives so a
            // later failure log can still say what the port has been through.
            USB_PORT_EVENT_HISTORY
                .fetch_or(changes | (hprt & HPRT_PRTOVRCURRACT), Ordering::Relaxed);
            if active & GINT_PRTINT != 0 {
                // Match `usb_dwc_ll_hprt_intr_read_and_clear`: preserve the
                // control fields, W1C the change bits, but write PRTENA as 0
                // because writing it as 1 disables the port.
                write(HPRT, hprt & !HPRT_PRTENA);
            }
            USB_PORT_PENDING.fetch_or(changes | (active & GINT_DISCONNINT), Ordering::Release);
            USB_PORT_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);
        }

        // GINTSTS fields are W1C or read-only. Channel and HPRT causes have
        // already been acknowledged above, so clear the latched core causes.
        write(GINTSTS, active);
        USB_INTERRUPT_COUNT.fetch_add(1, Ordering::Release);
    }
}

#[inline(always)]
unsafe fn handle_periodic_channel_interrupt(channels: u32, channel: usize, slot: usize) {
    if channels & (1 << channel) == 0 {
        return;
    }
    let hcint_address = channel_register(channel, HCINT_OFFSET);
    let hcintmsk_address = channel_register(channel, HCINTMSK_OFFSET);
    let hcint = unsafe { read(hcint_address) & read(hcintmsk_address) };
    if hcint != 0 {
        unsafe { write(hcint_address, hcint) };
        USB_LAST_PERIODIC_HCINT[slot].store(hcint, Ordering::Relaxed);
        USB_PERIODIC_PENDING[slot].fetch_or(hcint, Ordering::Release);
        USB_PERIODIC_INTERRUPT_COUNT[slot].fetch_add(1, Ordering::Relaxed);
    } else {
        USB_SPURIOUS_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

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

// HFNUM.FRNUM advances once per High-Speed microframe; its low three bits
// are therefore the 0..7 microframe index within a 1ms frame.
const HFNUM_FRNUM_MASK: u32 = 0xFFFF;
const HFNUM_UFRAME_MASK: u32 = 0x7;
const LAST_SSPLIT_UFRAME: u32 = 5;
const PERIODIC_SSPLIT_UFRAME: u32 = 0;
const SPLIT_FRAME_WAIT_ITERATIONS: u32 = 1_000_000;

// HCTSIZi in Scatter/Gather DMA mode repurposes the low byte as SCHED_INFO
// (bits[7:0], must be 0xFF for non-periodic channels or the channel can
// freeze -- ESP-IDF's `usb_dwc_ll_hctsiz_init` comment) and bits[15:8] as
// NTD (number of transfer descriptors - 1). The normal `run_packet` path
// uses one QTD. The Full-Speed WRITE packet-list experiment uses up to eight.
const HCTSIZ_SCHED_INFO_ALL: u32 = 0xFF;
#[allow(dead_code)] // rejected v38/v39 QTD-list experiment retained for its exact diagnostics
const HCTSIZ_NTD_SHIFT: u32 = 8;
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
// packs the list base into bits[31:9]). Channel 0 uses two 512-byte list
// slots; each periodic `QtdSlot` is independently padded to one list base.
const QTD_XFER_SIZE_MASK: u32 = 0x1_FFFF; // bits[16:0]
const QTD_IS_SETUP: u32 = 1 << 24;
const QTD_INTR_CPLT: u32 = 1 << 25;
const QTD_EOL: u32 = 1 << 26;
const QTD_STATUS_SHIFT: u32 = 28;
const QTD_STATUS_MASK: u32 = 0x3 << QTD_STATUS_SHIFT;
const QTD_STATUS_SUCCESS: u32 = 0;
const QTD_STATUS_PACKET_ERROR: u32 = 1 << QTD_STATUS_SHIFT;
const QTD_ACTIVE: u32 = 1 << 31;

/// Cache line size at both ESP32-P4 levels, and therefore the granularity
/// every DMA-shared object in this driver is aligned and sized to. Cache
/// maintenance cannot address anything finer, so an object that shares a
/// line with another owner cannot be synchronized without also writing back
/// -- or discarding -- that owner's data.
const DMA_ALIGN: usize = crate::psram::CACHE_LINE_BYTES;

/// The largest channel-0 QTD list payload. This equals the High-Speed Bulk
/// maximum packet size; direct Full-Speed BOT WRITE data can describe it as
/// eight 64-byte QTDs, while other callers remain MPS-bounded.
const PACKET_STAGING_BYTES: usize = 512;

/// Channel 0's DMA payload buffer.
///
/// The controller reads and writes this, never a caller's slice. A caller
/// hands `run_packet` any `&mut [u8]` it likes -- a 13-byte status wrapper,
/// an 8-byte SETUP packet, a slice starting part-way through a 4 KiB read --
/// and none of those can be given a cache-line start address by anything
/// the caller does. Propagating an alignment requirement outward does not
/// work either: a sub-slice at any MPS offset breaks it again at the next
/// packet. So the alignment lives here, once, on an object this module
/// owns, and each QTD list costs one copy of at most 512 bytes.
///
/// The whole buffer is synchronized, not just the bytes in use, which is
/// exactly `PACKET_STAGING_BYTES / DMA_ALIGN` whole lines belonging to
/// nothing else.
#[repr(C, align(64))]
struct PacketStaging {
    bytes: [u8; PACKET_STAGING_BYTES],
}

impl PacketStaging {
    const fn zeroed() -> Self {
        Self {
            bytes: [0; PACKET_STAGING_BYTES],
        }
    }
}

/// How many upcoming cache maintenance calls must refuse regardless of what
/// the hardware would have done.
///
/// Fault injection for the Stage 1 contract of
/// `docs/USB_BOT_HCD_REFACTOR_PLAN.md`: a refused synchronization has to
/// fail its transfer *before* the channel is armed, so that no packet
/// succeeds and no IN buffer is published from a staging area DMA never
/// wrote. There is no other way to reach that path on working hardware.
///
/// This is a self-draining count rather than a mode: each forced refusal
/// consumes one, so an armed injection cannot outlive the transfers it was
/// armed for, and there is no state a person can leave the bus in. A bus
/// that permanently cannot synchronize its DMA buffers is still not
/// something anyone should be able to select at a prompt.
static FORCED_CACHE_REFUSALS: AtomicU32 = AtomicU32::new(0);
/// Which transfer phase the armed refusals apply to, or
/// `TRANSFER_LABEL_COUNT` for any phase.
///
/// Counting operations to reach a particular phase does not work: the number
/// of cache calls before a command's data phase is an implementation detail
/// that every later stage of the refactor changes, and BOT Reset Recovery
/// runs its own control transfers in between. Naming the phase reaches the
/// intended packet whatever the count is, and leaves recovery -- which is
/// labelled `Control` -- able to run.
static FORCED_CACHE_REFUSAL_PHASE: AtomicU32 = AtomicU32::new(TRANSFER_LABEL_COUNT as u32);

/// Arms `count` forced cache refusals on transfers labelled `phase`, or on
/// any transfer when `phase` is `None`. Returns the count that was already
/// armed. See [`FORCED_CACHE_REFUSALS`].
pub fn force_cache_refusals(count: u32, phase: Option<TransferLabel>) -> u32 {
    FORCED_CACHE_REFUSAL_PHASE.store(
        phase.map_or(TRANSFER_LABEL_COUNT as u32, |label| label.index() as u32),
        Ordering::Relaxed,
    );
    FORCED_CACHE_REFUSALS.swap(count, Ordering::Relaxed)
}

/// Completions to hand to the slot under a generation it no longer holds.
///
/// The synchronous packet API cannot produce a stale generation on its own:
/// one `Channel0Transfer` is created, submitted and reaped inside a single
/// `run_packet`, so the token it checks is always the token it issued. The
/// check exists for the queued scheduler that replaces it, and a check that
/// has never been observed to fire is a check nobody knows works.
static FORCED_STALE_COMPLETIONS: AtomicU32 = AtomicU32::new(0);

/// Arms `count` completions delivered under the wrong generation.
pub fn force_stale_completions(count: u32) -> u32 {
    FORCED_STALE_COMPLETIONS.swap(count, Ordering::Relaxed)
}

fn take_forced_stale_completion() -> bool {
    FORCED_STALE_COMPLETIONS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |armed| {
            armed.checked_sub(1)
        })
        .is_ok()
}

/// OUT packets to report one byte short of what was asked for.
///
/// A device that takes part of a packet is the fault this models. It cannot
/// be produced on demand from a healthy device, and the consequence of
/// mishandling it -- the data toggle and the caller's offset advancing over
/// bytes that were never delivered -- is silent.
static FORCED_SHORT_OUTS: AtomicU32 = AtomicU32::new(0);

/// Arms `count` OUT completions reported one byte short.
pub fn force_short_outs(count: u32) -> u32 {
    FORCED_SHORT_OUTS.swap(count, Ordering::Relaxed)
}

fn take_forced_short_out() -> bool {
    FORCED_SHORT_OUTS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |armed| {
            armed.checked_sub(1)
        })
        .is_ok()
}

/// Consumes one armed forced refusal if `label` is the phase it was armed
/// for.
fn take_forced_cache_refusal(label: TransferLabel) -> bool {
    let armed_phase = FORCED_CACHE_REFUSAL_PHASE.load(Ordering::Relaxed);
    if armed_phase != TRANSFER_LABEL_COUNT as u32 && armed_phase != label.index() as u32 {
        return false;
    }
    FORCED_CACHE_REFUSALS
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |armed| {
            armed.checked_sub(1)
        })
        .is_ok()
}

#[repr(C, align(512))]
struct QtdSlot {
    control: u32,
    buffer: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct RawQtd {
    control: u32,
    buffer: u32,
}

impl RawQtd {
    const fn zeroed() -> Self {
        Self {
            control: 0,
            buffer: 0,
        }
    }
}

const CHANNEL0_QTD_LIST_CAPACITY: usize = 8;
const CHANNEL0_QTD_LIST_PADDING: usize =
    512 - CHANNEL0_QTD_LIST_CAPACITY * core::mem::size_of::<RawQtd>();
const _: () = assert!(core::mem::size_of::<RawQtd>() == 8);

/// One 512-byte descriptor-list base for synchronous channel 0.
///
/// The first eight entries are real 8-byte QTDs. The padding keeps the next
/// ping-pong slot on another HCDMA list base while allowing a WRITE data phase
/// to chain eight Full-Speed packets under one channel activation.
#[derive(Clone, Copy)]
#[repr(C, align(512))]
struct Channel0QtdListSlot {
    qtds: [RawQtd; CHANNEL0_QTD_LIST_CAPACITY],
    padding: [u8; CHANNEL0_QTD_LIST_PADDING],
}

impl Channel0QtdListSlot {
    const fn zeroed() -> Self {
        Self {
            qtds: [RawQtd::zeroed(); CHANNEL0_QTD_LIST_CAPACITY],
            padding: [0; CHANNEL0_QTD_LIST_PADDING],
        }
    }
}

const _: () = assert!(core::mem::size_of::<Channel0QtdListSlot>() == 512);
const _: () = assert!(core::mem::align_of::<Channel0QtdListSlot>() == 512);

impl QtdSlot {
    const fn zeroed() -> Self {
        Self {
            control: 0,
            buffer: 0,
        }
    }
}

const CHANNEL0_QTD_SLOT_COUNT: usize = 2;

/// Descriptor storage for synchronous channel 0.
///
/// Each list slot occupies one 512-byte descriptor-list base. Alternating the
/// slots prevents a retry from re-arming the exact address the DWC descriptor
/// engine just wrote back as failed.
#[repr(C, align(512))]
struct Channel0QtdBank {
    slots: [Channel0QtdListSlot; CHANNEL0_QTD_SLOT_COUNT],
}

/// What one descriptor readback established about a finished packet.
///
/// A `QTD_XFER_SIZE_MASK` remainder is only a byte count if the descriptor
/// it came from was actually written back by hardware. Real hardware left a
/// control word of `0x00000000` behind on a Full-Speed hub timeout, whose
/// remainder of zero says "every requested byte moved" -- and the same
/// zero is what an unfetched, never-written-back or stale-cache descriptor
/// reads as. Software writes `QTD_EOL` and `QTD_INTR_CPLT` at submit and
/// hardware preserves them (`0x16000200` on a real packet error), so their
/// absence is the difference between the two readings.
///
/// Everything downstream of this decision has to treat `Unknown` as "the
/// bytes on the wire cannot be accounted for" rather than as zero: a
/// resubmitted OUT that had in fact completed puts the same bytes on the
/// bus twice.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TransferProgress {
    /// The descriptor was written back and its remainder is within the
    /// requested length. The payload is the byte count that moved.
    Known(usize),
    /// The descriptor cannot be trusted, so no byte count exists.
    Unknown,
}

impl TransferProgress {
    /// The count for logs and counters, where an unknown length is reported
    /// as zero *and named as unknown beside it* -- never silently.
    pub fn count(self) -> usize {
        match self {
            Self::Known(bytes) => bytes,
            Self::Unknown => 0,
        }
    }

    pub fn is_known(self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// Whether an **abandoned** packet may be put on the bus again.
    ///
    /// Only one that provably moved nothing. A packet the channel never
    /// halted for is genuinely ambiguous: the core may have been part-way
    /// through it, and there is no completion status to say otherwise. Real
    /// hardware produced exactly that shape -- a 64-byte OUT whose
    /// descriptor read `0x06000000`, a success status with zero remaining,
    /// while `HCINT` was `0x00000000` and the channel had to be force
    /// halted. Read as a byte count that says all 64 bytes went out, and it
    /// was resubmitted four times.
    ///
    /// This does **not** apply to a packet the core reported as failed. See
    /// `bot::run_bulk_packet` for why a reported packet error is safe to
    /// resend where an abandoned packet is not.
    pub fn safe_to_retry(self) -> bool {
        self == Self::Known(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransferSlotState {
    Idle,
    Armed,
    CompletionPending,
    Reaped,
}

/// Identifies one use of the reusable channel-0 transfer slot.
///
/// The current synchronous compatibility wrapper cannot observe an old token,
/// but carrying the generation now makes the submit/reap boundary safe to
/// expose to the Stage 3 scheduler without changing its completion contract.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TransferToken(u32);

/// One caller-owned transfer generation for an unsplit packet.
///
/// The payload staging remains inside this object for its full
/// submit-to-reap lifetime. The QTD itself comes from the fixed channel-0
/// bank so consecutive generations do not reuse one physical descriptor.
struct Channel0Transfer<'a> {
    qtd_slot: usize,
    /// The bytes the controller actually moves. `buffer` is the caller's
    /// slice, which DMA never touches; see [`PacketStaging`].
    staging: PacketStaging,
    endpoint: Endpoint,
    is_setup: bool,
    pid_data1: bool,
    buffer: &'a mut [u8],
    state: TransferSlotState,
    token: TransferToken,
    completion: u32,
}

impl<'a> Channel0Transfer<'a> {
    fn new(endpoint: &Endpoint, is_setup: bool, pid_data1: bool, buffer: &'a mut [u8]) -> Self {
        Self {
            // Do not immediately re-arm the physical descriptor which just
            // completed or failed. Real FS hardware repeatedly reported a
            // status-1 QTD when every synchronous call reused one stack
            // address (0x4FF77C00), then stopped servicing the following
            // channel. A fixed two-slot bank gives descriptor ownership a
            // real generation boundary while channel 0 remains synchronous.
            qtd_slot: CHANNEL0_QTD_NEXT.fetch_add(1, Ordering::Relaxed) as usize
                % CHANNEL0_QTD_SLOT_COUNT,
            staging: PacketStaging::zeroed(),
            endpoint: *endpoint,
            is_setup,
            pid_data1,
            buffer,
            state: TransferSlotState::Idle,
            token: TransferToken(0),
            completion: 0,
        }
    }

    /// Publishes the QTD and starts channel 0, returning this slot
    /// generation, or `None` when the DMA buffers could not be
    /// synchronized. A refusal must stop the transfer here: arming the
    /// channel anyway would send whatever was in RAM, or fill a buffer the
    /// CPU then reads its own stale copy of.
    fn submit(&mut self) -> Option<TransferToken> {
        debug_assert!(self.state == TransferSlotState::Idle);
        let xfer_len = self.buffer.len();
        debug_assert!(xfer_len <= PACKET_STAGING_BYTES);
        if !self.endpoint.is_in {
            self.staging.bytes[..xfer_len].copy_from_slice(self.buffer);
        }
        let data_ptr = self.staging.bytes.as_mut_ptr();
        if !cache_writeback_invalidate(
            CacheSite::Channel0Payload,
            if self.endpoint.is_in {
                CacheDirection::DeviceToHost
            } else {
                CacheDirection::HostToDevice
            },
            data_ptr as usize,
            PACKET_STAGING_BYTES,
        ) {
            self.note_failure(PacketFailureKind::CacheSyncRefused, 0, 0);
            return None;
        }

        let mut qtd_control = xfer_len as u32 & QTD_XFER_SIZE_MASK;
        if self.is_setup {
            qtd_control |= QTD_IS_SETUP;
        }
        qtd_control |= QTD_INTR_CPLT | QTD_EOL | QTD_ACTIVE;

        let qtd_address = self.qtd_address();
        unsafe {
            write(qtd_address, qtd_control);
            write(qtd_address + 4, data_ptr as u32);
        }
        if !self.sync_qtd() {
            self.note_failure(PacketFailureKind::CacheSyncRefused, 0, 0);
            return None;
        }

        let endpoint = self.endpoint;
        let hcchar = (endpoint.mps as u32 & 0x7FF)
            | ((endpoint.endpoint_number as u32 & 0xF) << 11)
            | (if endpoint.is_in { HCCHAR_EPDIR_IN } else { 0 })
            | (if endpoint.route.low_speed_via_hub {
                HCCHAR_LSPDDEV
            } else {
                0
            })
            | endpoint.endpoint_type
            | ((endpoint.device_address as u32 & 0x7F) << 22);
        let hctsiz = HCTSIZ_SCHED_INFO_ALL
            | if self.is_setup {
                HCTSIZ_PID_SETUP
            } else if self.pid_data1 {
                HCTSIZ_PID_DATA1
            } else {
                0
            };

        let generation = USB_TRANSFER_GENERATION
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.token = TransferToken(generation);
        self.state = TransferSlotState::Armed;
        USB_SUBMIT_COUNT.fetch_add(1, Ordering::Relaxed);
        prepare_channel0_interrupt();
        unsafe {
            write(CHAN0_HCSPLT, 0);
            write(CHAN0_HCCHAR, hcchar);
            write(CHAN0_HCTSIZ, hctsiz);
            write(CHAN0_HCDMA, (qtd_address as u32) & 0xFFFF_FE00);
            modify(CHAN0_HCCHAR, HCCHAR_CHENA, HCCHAR_CHENA);
        }
        Some(self.token)
    }

    /// Judges one descriptor readback and returns how many bytes it proves
    /// moved.
    ///
    /// This is the only place `actual` is derived. Three ways a remainder
    /// can be meaningless are rejected here rather than at each caller:
    /// hardware still owning the descriptor, a remainder larger than what
    /// was asked for, and -- when the channel never reported the transfer
    /// complete -- a descriptor that does not look written back at all.
    ///
    /// `completed` is whether `HCINT.XferCompl` ended this packet. When it
    /// did not, an **all-zero** control word is rejected: real hardware left
    /// exactly `0x00000000` behind after a Full-Speed hub timeout, and the
    /// remainder of zero read out of it says "every requested byte moved" --
    /// the same thing an unfetched or stale-cached descriptor says. A word
    /// of zero is also self-contradictory on its own terms, claiming a full
    /// transfer while carrying none of the `QTD_EOL`/`QTD_INTR_CPLT` bits
    /// this driver set at submit and hardware preserves (`0x07000000` on a
    /// completed SETUP, `0x16000200` on a packet error).
    ///
    /// The test is deliberately this narrow. Requiring `QTD_EOL` on every
    /// uncompleted packet was tried first and was wrong: a Full-Speed hub
    /// path whose packet errors had always retried successfully with a
    /// descriptor reporting zero progress started reporting that progress
    /// as unknown, and ten write rounds out of ten failed where ten out of
    /// ten had passed. A rule that fails safe still has to be right about
    /// which readings are impossible.
    fn progress_from(&self, control_after: u32, completed: bool) -> TransferProgress {
        let requested = self.buffer.len();
        let hardware_owns = control_after & QTD_ACTIVE != 0;
        let remaining = (control_after & QTD_XFER_SIZE_MASK) as usize;
        let never_written_back = !completed && control_after == 0 && requested > 0;
        if remaining > requested {
            // Counted rather than only rejected: this core really does
            // write one back. A 64-byte OUT that failed came back with
            // `0x16018889` -- a genuine writeback (`Active` clear, status
            // 1, `EOL`/`IOC` preserved) carrying a remainder of 100,489,
            // and a 31-byte command block came back with a remainder of
            // 128. The old arithmetic saturated both to "nothing
            // transferred"; naming them is what makes the acceptance
            // criterion "an impossible length is never used as a byte
            // count" rather than "impossible lengths do not occur".
            IMPOSSIBLE_REMAINDERS.fetch_add(1, Ordering::Relaxed);
        }
        if hardware_owns || remaining > requested || never_written_back {
            return TransferProgress::Unknown;
        }
        TransferProgress::Known(requested - remaining)
    }

    /// Synchronizes this slot's descriptor. The QTD is over-aligned to 512
    /// bytes for `HCDMA`, which makes its whole allocation a run of cache
    /// lines nothing else shares.
    fn sync_qtd(&mut self) -> bool {
        cache_writeback_invalidate(
            CacheSite::Channel0Qtd,
            CacheDirection::Descriptor,
            self.qtd_address(),
            core::mem::size_of::<Channel0QtdListSlot>(),
        )
    }

    fn qtd_address(&self) -> usize {
        channel0_qtd_address(self.qtd_slot)
    }

    /// Invalidates the staging buffer and copies `transferred` received
    /// bytes out to the caller. Returns false if the buffer could not be
    /// synchronized, in which case nothing is published: the alternative is
    /// handing the caller the CPU's pre-DMA copy, which for a freshly
    /// zeroed staging area is a buffer full of zeroes that looks like a
    /// successful short packet.
    fn publish_in_bytes(&mut self, transferred: usize) -> bool {
        if !self.endpoint.is_in || transferred == 0 {
            return true;
        }
        if !cache_writeback_invalidate(
            CacheSite::Channel0Payload,
            CacheDirection::DeviceToHost,
            self.staging.bytes.as_mut_ptr() as usize,
            PACKET_STAGING_BYTES,
        ) {
            return false;
        }
        let published = transferred.min(self.buffer.len());
        self.buffer[..published].copy_from_slice(&self.staging.bytes[..published]);
        true
    }

    fn note_completion(&mut self, token: TransferToken, hcint: u32) -> bool {
        if self.state != TransferSlotState::Armed || token != self.token {
            USB_STALE_TOKEN_COUNT.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.completion = hcint;
        self.state = TransferSlotState::CompletionPending;
        true
    }

    /// Cache-synchronizes and classifies one ISR-completed QTD.
    fn reap(&mut self, token: TransferToken, quiet_errors: bool) -> PacketOutcome {
        if self.state != TransferSlotState::CompletionPending || token != self.token {
            USB_STALE_TOKEN_COUNT.fetch_add(1, Ordering::Relaxed);
            note_packet_failure(
                PacketFailureKind::StaleCompletion,
                self.endpoint.is_in,
                self.buffer.len(),
                0,
                self.completion,
                0,
            );
            return PacketOutcome::Error;
        }

        let qtd_address = self.qtd_address();
        if !self.sync_qtd() {
            self.note_failure(PacketFailureKind::CacheSyncRefused, 0, 0);
            return PacketOutcome::CacheSyncFailed;
        }
        let control_after = unsafe { read(qtd_address) };
        USB_LAST_REAP_HCINT.store(self.completion, Ordering::Release);
        USB_LAST_REAP_QTD_CONTROL.store(control_after, Ordering::Release);
        let status = control_after & QTD_STATUS_MASK;
        let mut progress =
            self.progress_from(control_after, self.completion & HCINT_XFERCOMPL != 0);
        if !self.endpoint.is_in
            && !self.buffer.is_empty()
            && progress.is_known()
            && take_forced_short_out()
        {
            progress = TransferProgress::Known(progress.count().saturating_sub(1));
        }
        let transferred = progress.count();
        self.state = TransferSlotState::Reaped;
        USB_REAP_COUNT.fetch_add(1, Ordering::Relaxed);

        if self.completion & HCINT_STALL != 0 {
            self.note_failure(PacketFailureKind::Stall, transferred, control_after);
            if !quiet_errors {
                uart::log(b"USB: transfer STALL\r\n");
                log_packet_failure(
                    PacketFailureKind::Stall,
                    self.buffer.len(),
                    transferred,
                    control_after,
                );
            }
            return PacketOutcome::Error;
        }
        if self.completion & HCINT_ERROR_MASK != 0 {
            self.note_failure(
                PacketFailureKind::TransactionError,
                transferred,
                control_after,
            );
            if !quiet_errors {
                uart::log_hex(b"USB: transfer transaction error, HCINT=", self.completion);
                log_packet_failure(
                    PacketFailureKind::TransactionError,
                    self.buffer.len(),
                    transferred,
                    control_after,
                );
                log_port_state();
            }
            return PacketOutcome::Error;
        }
        if status == QTD_STATUS_PACKET_ERROR {
            self.note_failure(
                PacketFailureKind::QtdPacketError,
                transferred,
                control_after,
            );
            if !self.publish_in_bytes(transferred) {
                self.note_failure(
                    PacketFailureKind::CacheSyncRefused,
                    transferred,
                    control_after,
                );
                return PacketOutcome::CacheSyncFailed;
            }
            if !quiet_errors {
                uart::log_hex(
                    b"USB: transfer QTD packet error, status=",
                    status >> QTD_STATUS_SHIFT,
                );
                // QTD status 1 covers CRC, transaction timeout, stuffing,
                // false EOP *and* excessive NAK, which are not the same
                // problem: a device that is merely busy NAKs, while the
                // others mean the transaction itself went wrong. HCINT
                // separates them, and the channel/port state says whether
                // the host is still able to run transactions at all --
                // which is the question when the control endpoint stops
                // working right after a bulk failure.
                uart::log_hex(b"USB:   HCINT=", self.completion);
                uart::log_hex(b"USB:   HCCHAR=", unsafe { read(CHAN0_HCCHAR) });
                uart::log_hex(b"USB:   HCTSIZ=", unsafe { read(CHAN0_HCTSIZ) });
                uart::log_hex(b"USB:   HCDMA=", unsafe { read(CHAN0_HCDMA) });
                log_packet_failure(
                    PacketFailureKind::QtdPacketError,
                    self.buffer.len(),
                    transferred,
                    control_after,
                );
                log_port_state();
            }
            return PacketOutcome::PacketError(progress);
        }
        if status != QTD_STATUS_SUCCESS {
            self.note_failure(
                PacketFailureKind::QtdInvalidStatus,
                transferred,
                control_after,
            );
            if !quiet_errors {
                uart::log_hex(
                    b"USB: transfer QTD buffer/reserved error, status=",
                    status >> QTD_STATUS_SHIFT,
                );
                log_packet_failure(
                    PacketFailureKind::QtdInvalidStatus,
                    self.buffer.len(),
                    transferred,
                    control_after,
                );
            }
            return PacketOutcome::Error;
        }
        // A halted channel is not by itself a completed transfer. In
        // descriptor-DMA mode the successful boundary requires both the
        // channel's XferCompl cause and hardware clearing QTD.Active. The
        // periodic HID path already enforces XferCompl; omitting it here let
        // a stale ChHltd snapshot/reused descriptor be accepted as a fresh
        // short packet, observed as the previous command's 13-byte CSW in
        // the next READ CAPACITY data phase.
        // A halted channel with an untrustworthy descriptor is not a
        // completed transfer either: `progress_from` rejects a descriptor
        // hardware never wrote back, one it still owns, and a remainder
        // larger than the request, and none of those can be reported as a
        // successful packet of any length.
        if self.completion & HCINT_XFERCOMPL == 0
            || control_after & QTD_ACTIVE != 0
            || !progress.is_known()
        {
            self.note_failure(
                PacketFailureKind::NotTransferComplete,
                transferred,
                control_after,
            );
            if !quiet_errors {
                uart::log(b"USB: halted QTD was not transfer-complete\r\n");
                uart::log_hex(b"USB:   HCINT=", self.completion);
                uart::log_hex(b"USB:   QTD control=", control_after);
                uart::log_hex(
                    b"USB:   XferCompl=",
                    u32::from(self.completion & HCINT_XFERCOMPL != 0),
                );
                uart::log_hex(
                    b"USB:   QTD active=",
                    u32::from(control_after & QTD_ACTIVE != 0),
                );
                log_packet_failure(
                    PacketFailureKind::NotTransferComplete,
                    self.buffer.len(),
                    transferred,
                    control_after,
                );
            }
            return PacketOutcome::Error;
        }
        // An OUT that completed with fewer bytes than it was given never
        // reaches the layer above. There is no useful partial OUT here: the
        // caller chunked by MPS, so a short one means the device took part
        // of a packet, and calling that success advances the data toggle
        // and the caller's offset over bytes that were never delivered.
        if !self.endpoint.is_in && transferred != self.buffer.len() {
            self.note_failure(PacketFailureKind::ShortOut, transferred, control_after);
            if !quiet_errors {
                uart::log(b"USB: OUT packet completed short\r\n");
                log_packet_failure(
                    PacketFailureKind::ShortOut,
                    self.buffer.len(),
                    transferred,
                    control_after,
                );
            }
            return PacketOutcome::Error;
        }
        if !self.publish_in_bytes(transferred) {
            self.note_failure(
                PacketFailureKind::CacheSyncRefused,
                transferred,
                control_after,
            );
            return PacketOutcome::CacheSyncFailed;
        }
        PacketOutcome::Ok(transferred)
    }

    /// Records one failed packet against this slot's own requested length,
    /// direction and completion cause, so every failure carries the same
    /// four numbers regardless of which check rejected it.
    fn note_failure(&self, kind: PacketFailureKind, actual: usize, qtd_final: u32) {
        note_packet_failure(
            kind,
            self.endpoint.is_in,
            self.buffer.len(),
            actual,
            self.completion,
            qtd_final,
        );
    }

    /// Retires an abandoned packet and reports what its descriptor proves
    /// about it -- which, after a timeout, is very often nothing at all.
    fn cancel(&mut self, token: TransferToken) -> TransferProgress {
        if token == self.token {
            let qtd_address = self.qtd_address();
            if !self.sync_qtd() {
                self.note_failure(PacketFailureKind::CacheSyncRefused, 0, 0);
                self.state = TransferSlotState::Reaped;
                USB_CANCEL_COUNT.fetch_add(1, Ordering::Relaxed);
                return TransferProgress::Unknown;
            }
            let control_after = unsafe { read(qtd_address) };
            // Published for the same reason `reap` publishes it: a refusal
            // to resubmit has to be explainable from the descriptor it was
            // based on.
            USB_LAST_REAP_HCINT.store(self.completion, Ordering::Release);
            USB_LAST_REAP_QTD_CONTROL.store(control_after, Ordering::Release);
            // A cancelled packet never reported completion by definition.
            let progress = self.progress_from(control_after, false);
            // A refusal here means the received bytes cannot be published,
            // so as far as the caller is concerned none arrived -- and how
            // many there were is then unknown, not zero. Reporting them as
            // transferred would let a retry skip a prefix that was never
            // handed over.
            let progress = if self.publish_in_bytes(progress.count()) {
                progress
            } else {
                self.note_failure(
                    PacketFailureKind::CacheSyncRefused,
                    progress.count(),
                    control_after,
                );
                TransferProgress::Unknown
            };
            self.state = TransferSlotState::Reaped;
            USB_CANCEL_COUNT.fetch_add(1, Ordering::Relaxed);
            progress
        } else {
            USB_STALE_TOKEN_COUNT.fetch_add(1, Ordering::Relaxed);
            TransferProgress::Unknown
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

/// How long `probe_port` waits for a device to pull its data line up after
/// VBUS came on, in milliseconds.
///
/// The frame loop re-probes an empty root port on a timer and blocks the
/// whole loop while it waits, so the steady-state value has to stay short.
/// Boot is the one caller that can afford to wait longer, because finding a
/// USB mass-storage device there decides which filesystem the firmware comes
/// up on; `input::InputManager::new` raises the limit for its one initial
/// scan and puts it back. See `docs/USB_MSC_BOOT_MARGIN_PLAN.md`.
const DEFAULT_CONNECT_WAIT_MS: u32 = 500;
/// Upper bound accepted by [`set_connect_wait_ms`], so the poll count it is
/// converted into cannot overflow and no caller can block the foreground
/// for an unbounded time.
const MAX_CONNECT_WAIT_MS: u32 = 60_000;

static CONNECT_WAIT_MS: AtomicU32 = AtomicU32::new(DEFAULT_CONNECT_WAIT_MS);

/// Sets the root-port connect wait used by the next [`probe_port`] and
/// returns the previous value, so a caller that needs a longer one-off wait
/// can restore the steady-state limit afterwards.
pub fn set_connect_wait_ms(milliseconds: u32) -> u32 {
    CONNECT_WAIT_MS.swap(
        milliseconds.clamp(1, MAX_CONNECT_WAIT_MS),
        Ordering::Relaxed,
    )
}

/// The connect wait the next [`probe_port`] will use.
pub fn connect_wait_ms() -> u32 {
    CONNECT_WAIT_MS.load(Ordering::Relaxed)
}

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
    /// Milliseconds from the VBUS enable write at the top of `probe_port`
    /// until HPRT first reported a device connected, or 0 if none did.
    /// This is the part of the boot path whose length is set by the device
    /// rather than by this driver's fixed delays, and the reason
    /// [`set_connect_wait_ms`] exists.
    pub connect_ms: u32,
    /// Milliseconds from the same VBUS enable write until the port was
    /// enabled after debounce and the reset pulse, or 0 if it never was.
    pub enabled_ms: u32,
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

/// Read-only snapshot exposed by `usbhw` while the interrupt migration is
/// being validated on real hardware.
#[derive(Clone, Copy)]
pub struct InterruptDiagnostics {
    pub source: u32,
    pub total: u32,
    pub channel0: u32,
    pub channel1: u32,
    pub port: u32,
    pub spurious: u32,
    pub pending_channel0: u32,
    pub pending_channel1: u32,
    pub pending_port: u32,
    pub last_gintsts: u32,
    pub last_haint: u32,
    pub last_hcint0: u32,
    pub last_hcint1: u32,
    pub last_hprt: u32,
    pub live_gintmsk: u32,
    pub live_haintmsk: u32,
    pub live_hcintmsk0: u32,
    pub live_hcintmsk1: u32,
    pub global_signal_enabled: bool,
    pub sleep_waits: u32,
    pub poll_waits: u32,
    pub wfi_count: u32,
    pub last_wait_cycles: u32,
    pub max_wait_cycles: u32,
    pub submits: u32,
    pub reaps: u32,
    pub cancels: u32,
    pub stale_tokens: u32,
    pub periodic_active: bool,
    pub periodic_channel_mask: u32,
    pub periodic_interrupts: u32,
    pub periodic_pending_mask: u32,
    pub periodic_irq_counts: [u32; PERIODIC_HID_SLOT_COUNT],
    pub periodic_pending: [u32; PERIODIC_HID_SLOT_COUNT],
    pub periodic_last_hcint: [u32; PERIODIC_HID_SLOT_COUNT],
    pub periodic_hcintmsk: [u32; PERIODIC_HID_SLOT_COUNT],
    pub periodic_completions: u32,
    pub periodic_rearms: u32,
    pub periodic_errors: u32,
    pub split_mode_active: bool,
    pub split_packets: u32,
    pub split_rounds: u32,
    pub split_mode_conflicts: u32,
}

pub fn interrupt_diagnostics() -> InterruptDiagnostics {
    InterruptDiagnostics {
        source: crate::interrupts::USB_OTG_HS_INTERRUPT_SOURCE as u32,
        total: USB_INTERRUPT_COUNT.load(Ordering::Acquire),
        channel0: USB_CHANNEL0_INTERRUPT_COUNT.load(Ordering::Acquire),
        channel1: USB_PERIODIC_INTERRUPT_COUNT[0].load(Ordering::Acquire),
        port: USB_PORT_INTERRUPT_COUNT.load(Ordering::Acquire),
        spurious: USB_SPURIOUS_INTERRUPT_COUNT.load(Ordering::Acquire),
        pending_channel0: USB_CHANNEL0_PENDING.load(Ordering::Acquire),
        pending_channel1: USB_PERIODIC_PENDING[0].load(Ordering::Acquire),
        pending_port: USB_PORT_PENDING.load(Ordering::Acquire),
        last_gintsts: USB_LAST_GINTSTS.load(Ordering::Acquire),
        last_haint: USB_LAST_HAINT.load(Ordering::Acquire),
        last_hcint0: USB_LAST_HCINT0.load(Ordering::Acquire),
        last_hcint1: USB_LAST_PERIODIC_HCINT[0].load(Ordering::Acquire),
        last_hprt: USB_LAST_HPRT.load(Ordering::Acquire),
        live_gintmsk: unsafe { read(GINTMSK) },
        live_haintmsk: unsafe { read(HAINTMSK) },
        live_hcintmsk0: unsafe { read(CHAN0_HCINTMSK) },
        live_hcintmsk1: unsafe { read(CHAN1_HCINTMSK) },
        global_signal_enabled: unsafe { read(GAHBCFG) } & GAHBCFG_GLBLINTRMSK != 0,
        sleep_waits: USB_SLEEP_WAIT_COUNT.load(Ordering::Acquire),
        poll_waits: USB_POLL_WAIT_COUNT.load(Ordering::Acquire),
        wfi_count: USB_WFI_COUNT.load(Ordering::Acquire),
        last_wait_cycles: USB_LAST_WAIT_CYCLES.load(Ordering::Acquire),
        max_wait_cycles: USB_MAX_WAIT_CYCLES.load(Ordering::Acquire),
        submits: USB_SUBMIT_COUNT.load(Ordering::Acquire),
        reaps: USB_REAP_COUNT.load(Ordering::Acquire),
        cancels: USB_CANCEL_COUNT.load(Ordering::Acquire),
        stale_tokens: USB_STALE_TOKEN_COUNT.load(Ordering::Acquire),
        periodic_active: PERIODIC_HID_ACTIVE_MASK.load(Ordering::Acquire) != 0,
        periodic_channel_mask: PERIODIC_HID_ACTIVE_MASK.load(Ordering::Acquire) << 1,
        periodic_interrupts: USB_PERIODIC_INTERRUPT_COUNT
            .iter()
            .fold(0u32, |total, count| {
                total.wrapping_add(count.load(Ordering::Acquire))
            }),
        periodic_pending_mask: USB_PERIODIC_PENDING.iter().enumerate().fold(
            0u32,
            |mask, (slot, pending)| {
                mask | if pending.load(Ordering::Acquire) != 0 {
                    1 << (slot + 1)
                } else {
                    0
                }
            },
        ),
        periodic_irq_counts: core::array::from_fn(|slot| {
            USB_PERIODIC_INTERRUPT_COUNT[slot].load(Ordering::Acquire)
        }),
        periodic_pending: core::array::from_fn(|slot| {
            USB_PERIODIC_PENDING[slot].load(Ordering::Acquire)
        }),
        periodic_last_hcint: core::array::from_fn(|slot| {
            USB_LAST_PERIODIC_HCINT[slot].load(Ordering::Acquire)
        }),
        periodic_hcintmsk: core::array::from_fn(|slot| unsafe {
            read(channel_register(slot + 1, HCINTMSK_OFFSET))
        }),
        periodic_completions: PERIODIC_HID_COMPLETION_COUNT.load(Ordering::Acquire),
        periodic_rearms: PERIODIC_HID_REARM_COUNT.load(Ordering::Acquire),
        periodic_errors: PERIODIC_HID_ERROR_COUNT.load(Ordering::Acquire),
        split_mode_active: SPLIT_MODE_ACTIVE.load(Ordering::Acquire),
        split_packets: SPLIT_PACKET_COUNT.load(Ordering::Acquire),
        split_rounds: SPLIT_ROUND_COUNT.load(Ordering::Acquire),
        split_mode_conflicts: SPLIT_MODE_CONFLICT_COUNT.load(Ordering::Acquire),
    }
}

/// Consumes root-port IRQ state and reports whether the physical connection
/// changed. Enable/over-current changes remain visible in diagnostics but do
/// not by themselves invalidate the device registry.
pub fn take_root_connection_change() -> bool {
    let events = USB_PORT_PENDING.swap(0, Ordering::AcqRel);
    events & ((1 << 1) | GINT_DISCONNINT) != 0
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
        connect_ms: 0,
        enabled_ms: 0,
    }
}

/// Milliseconds elapsed since a `tick::now_ms` reading, saturated into the
/// `u32` the timing fields carry. Every measurement here is 0 until
/// `tick::init` has run, which is fine: `app::run` starts the tick before
/// it builds the `InputManager` that owns the USB host.
fn milliseconds_since(start: u64) -> u32 {
    crate::tick::now_ms().saturating_sub(start) as u32
}

/// Runs the full Stage 1 sequence from scratch: VBUS on, UTMI PHY and
/// USB-DWC core bring-up, host port power-on, and (if a device is already
/// plugged into USB-A) connect debounce, port reset, and speed read.
///
/// Like `sdmmc::init`, this re-initializes everything on every call; there
/// is no persistent handle at this layer (that is `hid_keyboard::UsbKeyboard`,
/// built on top).
pub fn probe_port() -> HostPort {
    let started_ms = crate::tick::now_ms();
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

    delay_ms(FORCE_HOST_MODE_DELAY_MS);

    configure_host_speed_support();
    hprt_modify(HPRT_PRTPWR, HPRT_PRTPWR); // port power on
    enable_controller_interrupts();

    let connect_ms = match wait_for_connect(started_ms) {
        Some(elapsed) => elapsed,
        None => {
            // The foreground periodically probes an empty root port. Emit
            // this once for that disconnected interval, then wait until a
            // connection has actually been observed before allowing it again.
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
                connect_ms: 0,
                enabled_ms: 0,
            };
        }
    };

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
            connect_ms,
            enabled_ms: 0,
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
        finish_port_enable(fifo_depth_words);
        // Only now is the history worth keeping: this driver's own reset
        // pulse sets `prtenchng` (and can set `prtconndet`), so clearing it
        // any earlier would leave every later log claiming the device had
        // dropped off the bus. "Since the bus came up" means since here.
        USB_PORT_EVENT_HISTORY.store(0, Ordering::Relaxed);
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
        connect_ms,
        enabled_ms: if enabled {
            milliseconds_since(started_ms)
        } else {
            0
        },
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
        // A rescan resets the DWC while the CLIC route remains installed.
        // Keep the peripheral signal quiet until host-mode registers and all
        // stale status have been initialized again.
        modify(GAHBCFG, GAHBCFG_GLBLINTRMSK, 0);
        write(GINTMSK, 0);
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

/// Clears stale causes and enables the minimal Stage 1 interrupt set.
///
/// This runs after the force-host-mode delay, so HAINT and channel registers
/// are valid. Port interrupts are diagnostic for now; foreground still uses
/// the current HPRT connection state and its existing debounce policy.
fn enable_controller_interrupts() {
    unsafe {
        modify(GAHBCFG, GAHBCFG_GLBLINTRMSK, 0);
        write(GINTMSK, 0);
        write(HAINTMSK, 0);
        write(CHAN0_HCINTMSK, 0);
        write(CHAN0_HCINT, 0xFFFF_FFFF);
        for channel in 1..=PERIODIC_HID_SLOT_COUNT {
            write(channel_register(channel, HCINTMSK_OFFSET), 0);
            write(channel_register(channel, HCINT_OFFSET), 0xFFFF_FFFF);
        }
        write(GINTSTS, 0xFFFF_FFFF);
    }
    USB_CHANNEL0_PENDING.store(0, Ordering::Release);
    for pending in &USB_PERIODIC_PENDING {
        pending.store(0, Ordering::Release);
    }
    USB_PORT_PENDING.store(0, Ordering::Release);

    crate::interrupts::install_usb();

    unsafe {
        write(CHAN0_HCINTMSK, HCINT_ENABLED_MASK);
        write(HAINTMSK, 1);
        write(GINTMSK, GINT_ENABLED_MASK);
        modify(GAHBCFG, GAHBCFG_GLBLINTRMSK, GAHBCFG_GLBLINTRMSK);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}

/// Starts a new channel-0 generation without inheriting an ISR snapshot from
/// the preceding packet. The channel is idle at every call site.
fn prepare_channel0_interrupt() {
    unsafe {
        write(CHAN0_HCINTMSK, 0);
        modify(HAINTMSK, 1, 0);
        core::arch::asm!("fence iorw, iorw", options(nostack));
        write(CHAN0_HCINT, 0xFFFF_FFFF);
        write(GINTSTS, GINT_HCHINT);
    }
    USB_CHANNEL0_PENDING.store(0, Ordering::Release);
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack));
        write(CHAN0_HCINTMSK, HCINT_ENABLED_MASK);
        modify(HAINTMSK, 1, 1);
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
    unsafe {
        modify(
            HCFG,
            HCFG_FSLSSUPP,
            if fs_ls_only_host_forced() {
                HCFG_FSLSSUPP
            } else {
                0
            },
        );
    }
}

fn configure_fifos(fifo_depth_words: u32) {
    // Match ESP-IDF v5.5.3's default balanced partition for a High-Speed
    // DWC instance. It derives TX sizes from the 1024-line OTG data FIFO
    // synthesis depth, then gives the remaining implemented lines to RX.
    // ESP32-P4 reports 896 usable lines, producing RX/NPTX/PTX=512/256/128.
    // The former 448/224/224 split was locally invented and was the largest
    // remaining global-controller difference in the BOT path. Apply this
    // only after a successful root-port reset: that reset restores the FIFO
    // registers to their hardware defaults. ESP-IDF likewise reapplies its
    // saved FIFO configuration at this point.
    const HS_OTG_DFIFO_DEPTH: u32 = 1024;
    let nptx_lines = HS_OTG_DFIFO_DEPTH / 4;
    let ptx_lines = HS_OTG_DFIFO_DEPTH / 8;
    let tx_lines = nptx_lines + ptx_lines;
    let (rx_lines, nptx_lines, ptx_lines) = if fifo_depth_words > tx_lines {
        (fifo_depth_words - tx_lines, nptx_lines, ptx_lines)
    } else {
        // Defensive fallback for an unexpected smaller DWC configuration.
        let rx = fifo_depth_words / 2;
        let remaining = fifo_depth_words - rx;
        (rx, remaining / 2, remaining - remaining / 2)
    };

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

/// True while any periodic HID channel holds an armed QTD.
///
/// The periodic TX FIFO and the single host RX FIFO belong to the whole
/// controller, not to the channel that happened to fail. Flushing them
/// while a keyboard or mouse is waiting on channel 1-4 throws that
/// endpoint's in-flight data away, and the device's session dies with it --
/// which is how a failed mass-storage transfer used to take the keyboard
/// down with it.
fn periodic_channels_armed() -> bool {
    const PERIODIC_CHANNEL_MASK: u32 = 0x1E;
    PERIODIC_HID_ACTIVE_MASK.load(Ordering::Acquire) != 0
        || unsafe { read(HAINTMSK) } & PERIODIC_CHANNEL_MASK != 0
}

/// Flushes what channel 0's failed transfer can have left behind, without
/// disturbing periodic endpoints when any are armed.
///
/// Channel 0 sends through the non-periodic TX FIFO, so that one is always
/// safe to flush. The periodic TX FIFO and the RX FIFO are shared, so they
/// are only flushed when nothing periodic is running. The cost of skipping
/// them is that residue from the failed transfer may remain in the RX FIFO;
/// the cost of not skipping them is a working keyboard destroyed by an
/// unrelated device's failure, which is the worse of the two and the one
/// seen on real hardware.
fn flush_channel0_fifos() {
    if periodic_channels_armed() {
        flush_non_periodic_tx_fifo();
        // Counted, not just logged: "all cleanups succeeded" and "two of
        // the three cleanups were skipped" are different states, and a
        // baseline that reports them as the same one cannot tell whether a
        // later run kept the residue this cleanup exists to remove.
        FIFO_FLUSHES_SKIPPED_FOR_PERIODIC.fetch_add(1, Ordering::Relaxed);
        uart::log(b"USB: periodic channels armed, flushed only the non-periodic FIFO\r\n");
        return;
    }
    flush_fifos();
}

fn flush_non_periodic_tx_fifo() {
    unsafe {
        modify(GRSTCTL, GRSTCTL_TXFNUM_MASK, 0); // select non-periodic TX FIFO
        modify(GRSTCTL, GRSTCTL_TXFFLSH, GRSTCTL_TXFFLSH);
    }
    if !poll_until(GRSTCTL, GRSTCTL_TXFFLSH, false, 100_000) {
        note_fifo_flush_timeout(FIFO_NON_PERIODIC_TX);
        uart::log(b"USB: non-periodic TX FIFO flush timed out\r\n");
    }
}

fn flush_fifos() {
    flush_non_periodic_tx_fifo();
    unsafe {
        modify(GRSTCTL, GRSTCTL_TXFNUM_MASK, 1 << 6); // select periodic TX FIFO
        modify(GRSTCTL, GRSTCTL_TXFFLSH, GRSTCTL_TXFFLSH);
    }
    if !poll_until(GRSTCTL, GRSTCTL_TXFFLSH, false, 100_000) {
        note_fifo_flush_timeout(FIFO_PERIODIC_TX);
        uart::log(b"USB: periodic TX FIFO flush timed out\r\n");
    }
    unsafe {
        modify(GRSTCTL, GRSTCTL_RXFFLSH, GRSTCTL_RXFFLSH);
    }
    if !poll_until(GRSTCTL, GRSTCTL_RXFFLSH, false, 100_000) {
        note_fifo_flush_timeout(FIFO_RX);
        uart::log(b"USB: RX FIFO flush timed out\r\n");
    }
}

/// Restores channel 0 to the baseline used by descriptor-DMA transfers after
/// an abandoned transfer or before a proactive BOT boundary. Periodic HID
/// channels may be active concurrently, so [`flush_channel0_fifos`] limits
/// the flush scope when their shared FIFOs cannot safely be discarded.
///
/// This is intentionally lighter than a root-port reset: devices keep their
/// address/configuration and a retry can resume without re-enumerating live
/// keyboard or storage sessions.
pub fn recover_channel_after_packet_failure() {
    if unsafe { read(CHAN0_HCCHAR) } & HCCHAR_CHENA != 0 {
        force_halt_channel();
    }
    unsafe {
        write(CHAN0_HCSPLT, 0);
        modify(HCFG, HCFG_DESCDMA, HCFG_DESCDMA);
    }
    prepare_channel0_interrupt();
    flush_channel0_fifos();
}

/// Restores channel 0 after hardware has reported a completed packet error.
///
/// Unlike a timeout, the channel has already halted and the descriptor has
/// been reaped. An OUT failure can only leave payload residue in the
/// non-periodic TX FIFO; flushing the RX and periodic TX FIFOs as well is
/// unrelated to that packet and used to happen only because the generic
/// recovery API had no direction. IN failures keep the conservative existing
/// cleanup because receive residue can remain in the shared RX FIFO.
pub fn recover_reported_packet_error(is_in: bool) {
    if unsafe { read(CHAN0_HCCHAR) } & HCCHAR_CHENA != 0 {
        force_halt_channel();
    }
    unsafe {
        write(CHAN0_HCSPLT, 0);
        modify(HCFG, HCFG_DESCDMA, HCFG_DESCDMA);
    }
    prepare_channel0_interrupt();
    if is_in {
        flush_channel0_fifos();
    } else {
        OUT_PACKET_ERROR_NPTX_CLEANUPS.fetch_add(1, Ordering::Relaxed);
        flush_non_periodic_tx_fifo();
    }
}

/// Waits for HPRT to report a connected device, returning the milliseconds
/// it took (measured from `started_ms`, so it includes this driver's own
/// VBUS-settle and core bring-up delays) or `None` if the wait ran out.
///
/// Run `usbinfo` after plugging a device in later than this budget allows.
fn wait_for_connect(started_ms: u64) -> Option<u32> {
    const POLL_INTERVAL_US: u32 = 2_000;
    let max_polls = (connect_wait_ms() * 1_000).div_ceil(POLL_INTERVAL_US);
    for _ in 0..max_polls {
        if unsafe { read(HPRT) } & HPRT_PRTCONNSTS != 0 {
            return Some(milliseconds_since(started_ms));
        }
        delay_us(POLL_INTERVAL_US);
    }
    None
}

fn reset_pulse() {
    hprt_modify(HPRT_PRTRST, HPRT_PRTRST);
    delay_ms(RESET_HOLD_MS);
    hprt_modify(HPRT_PRTRST, 0);
    delay_ms(RESET_RECOVERY_MS);
}

fn finish_port_enable(fifo_depth_words: u32) {
    configure_fifos(fifo_depth_words);
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
    /// Channel did not halt within the budget. The payload is what the
    /// descriptor proves about the bytes that moved, which after an
    /// abandoned packet is frequently [`TransferProgress::Unknown`].
    Timeout(TransferProgress),
    /// QTD status 1: CRC/transaction timeout/stuff/false-EOP/excessive-NAK.
    /// The payload is what the descriptor proves about the bytes completed
    /// before the failed packet.
    PacketError(TransferProgress),
    /// A DMA buffer or descriptor could not be cache-synchronized, so the
    /// packet was never started -- or its received bytes were never
    /// published. Distinct from `Error` because retrying is pointless: the
    /// same buffer will be refused again. See `cache_writeback_invalidate`.
    CacheSyncFailed,
    Error,
}

/// How foreground should wait for this packet's channel halt.
///
/// Logging policy is deliberately separate: control-transfer retries often
/// suppress diagnostics on early attempts, but those packets still have a
/// real completion IRQ and should sleep. Only the manually scheduled HID
/// idle poll needs the bounded polling exception described below.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CompletionWait {
    /// Control, bulk, and any packet expected to halt with a completion or
    /// handshake interrupt.
    Interrupt,
    /// A directly addressed HID Interrupt IN endpoint presented to the DWC
    /// as BULK. Descriptor DMA retries idle NAK internally without CHHLTD,
    /// so sleeping here would wait for an unrelated display frame.
    PollIdleNak,
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
    /// `HCCHAR_EPTYPE_CTRL`, `HCCHAR_EPTYPE_BULK`, or
    /// `HCCHAR_EPTYPE_INTR` for a software-scheduled split HID packet.
    pub endpoint_type: u32,
    pub mps: u16,
    pub is_in: bool,
    pub route: Route,
}

const PERIODIC_FRAME_LIST_ENTRIES: usize = 32;
const PERIODIC_PROBE_TIMEOUT_SECONDS: u32 = 5;
const PERIODIC_HID_SLOT_COUNT: usize = 4;

#[repr(C, align(512))]
struct PeriodicFrameList {
    entries: [u32; PERIODIC_FRAME_LIST_ENTRIES],
}

/// One persistent periodic slot's DMA report buffer.
///
/// Aligned and sized to exactly one cache line. It was `align(4)` and the
/// release image placed it at `0x4FF515D8`, which is 0x18 past a line
/// boundary -- so every synchronization of it started mid-line. The ROM
/// routine did not refuse that in the Stage 0 baseline, but "was not
/// refused" is not the contract; `psram::writeback_invalidate` requires a
/// line-aligned start and does not round down.
#[repr(C, align(64))]
struct PeriodicReportBuffer {
    bytes: [u8; DMA_ALIGN],
}

#[repr(C, align(512))]
struct PeriodicQtdBank {
    slots: [QtdSlot; PERIODIC_HID_SLOT_COUNT],
}

/// The four slots' report buffers. Declared at `DMA_ALIGN` rather than
/// inheriting it from the element type, so that weakening
/// `PeriodicReportBuffer` cannot silently un-align the bank.
#[repr(C, align(64))]
struct PeriodicBufferBank {
    slots: [PeriodicReportBuffer; PERIODIC_HID_SLOT_COUNT],
}

#[repr(transparent)]
struct DmaCell<T>(UnsafeCell<T>);

// One foreground USB owner mutates these cells. The ISR never dereferences
// them; it only publishes HCINT into `USB_PERIODIC_PENDING`.
unsafe impl<T> Sync for DmaCell<T> {}

impl<T> DmaCell<T> {
    const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    fn get(&self) -> *mut T {
        self.0.get()
    }
}

static PERIODIC_HID_FRAME_LIST: DmaCell<PeriodicFrameList> = DmaCell::new(PeriodicFrameList {
    entries: [0; PERIODIC_FRAME_LIST_ENTRIES],
});
static CHANNEL0_QTD_BANK: DmaCell<Channel0QtdBank> = DmaCell::new(Channel0QtdBank {
    slots: [Channel0QtdListSlot::zeroed(), Channel0QtdListSlot::zeroed()],
});
static PERIODIC_HID_QTD: DmaCell<PeriodicQtdBank> = DmaCell::new(PeriodicQtdBank {
    slots: [
        QtdSlot::zeroed(),
        QtdSlot::zeroed(),
        QtdSlot::zeroed(),
        QtdSlot::zeroed(),
    ],
});

fn channel0_qtd_address(slot: usize) -> usize {
    debug_assert!(slot < CHANNEL0_QTD_SLOT_COUNT);
    CHANNEL0_QTD_BANK.get() as usize + slot * core::mem::size_of::<Channel0QtdListSlot>()
}
static PERIODIC_HID_BUFFER: DmaCell<PeriodicBufferBank> = DmaCell::new(PeriodicBufferBank {
    slots: [
        PeriodicReportBuffer {
            bytes: [0; DMA_ALIGN],
        },
        PeriodicReportBuffer {
            bytes: [0; DMA_ALIGN],
        },
        PeriodicReportBuffer {
            bytes: [0; DMA_ALIGN],
        },
        PeriodicReportBuffer {
            bytes: [0; DMA_ALIGN],
        },
    ],
});

#[derive(Clone, Copy)]
pub struct PeriodicHandle {
    slot: u8,
    generation: u32,
}

impl PeriodicHandle {
    pub fn channel(self) -> u8 {
        self.slot + 1
    }
}

pub enum PeriodicRead {
    Pending,
    Complete(usize),
    Error,
}

/// Permanently assigns one of channels 1..=4 to a non-Split HID endpoint.
/// All active slots share one 32-entry frame list; each owns its QTD, report
/// buffer, data toggle, pending IRQ state, and generation token.
pub fn enable_periodic_hid(endpoint: &Endpoint, interval: u8) -> Option<PeriodicHandle> {
    let mps = endpoint.mps as usize;
    if endpoint.route.split.is_some() || mps == 0 || mps > 64 {
        return None;
    }

    let scheduled_interval = periodic_interval_frames(interval);
    let mut active = PERIODIC_HID_ACTIVE_MASK.load(Ordering::Acquire);
    let slot = loop {
        if active == (1 << PERIODIC_HID_SLOT_COUNT) - 1
            || (active == 0 && unsafe { read(HCFG) } & HCFG_PERSCHEDENA != 0)
        {
            return None;
        }
        let free = (!active & ((1 << PERIODIC_HID_SLOT_COUNT) - 1)).trailing_zeros() as usize;
        match PERIODIC_HID_ACTIVE_MASK.compare_exchange_weak(
            active,
            active | (1 << free),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break free,
            Err(updated) => active = updated,
        }
    };
    let channel = slot + 1;
    let hcchar_address = channel_register(channel, HCCHAR_OFFSET);
    if unsafe { read(hcchar_address) } & HCCHAR_CHENA != 0 {
        PERIODIC_HID_ACTIVE_MASK.fetch_and(!(1 << slot), Ordering::AcqRel);
        return None;
    }

    PERIODIC_HID_MPS[slot].store(mps as u32, Ordering::Release);
    PERIODIC_HID_INTERVAL[slot].store(scheduled_interval as u32, Ordering::Release);
    PERIODIC_HID_PID_DATA1[slot].store(false, Ordering::Release);
    USB_PERIODIC_PENDING[slot].store(0, Ordering::Release);
    if !rebuild_periodic_frame_list() {
        // The controller would walk a frame list the CPU still holds a
        // dirty copy of. Give the slot back and let the caller fall back to
        // channel-0 polling, which stages its own DMA buffer.
        uart::log(b"USB HID: periodic frame list cache sync refused, staying on channel 0\r\n");
        PERIODIC_HID_ACTIVE_MASK.fetch_and(!(1 << slot), Ordering::AcqRel);
        return None;
    }

    let frame_list_address = PERIODIC_HID_FRAME_LIST.get() as usize;
    let generation = PERIODIC_HID_GENERATION[slot]
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);

    unsafe {
        write(channel_register(channel, HCINTMSK_OFFSET), 0);
        modify(HAINTMSK, 1 << channel, 0);
        write(channel_register(channel, HCINT_OFFSET), u32::MAX);
        write(channel_register(channel, HCSPLT_OFFSET), 0);
        write(HFLBADDR, frame_list_address as u32);
        modify(
            HCFG,
            HCFG_FRLISTEN_MASK | HCFG_PERSCHEDENA,
            HCFG_FRLISTEN_32 | HCFG_PERSCHEDENA,
        );
        let hcchar = (endpoint.mps as u32 & 0x7FF)
            | ((endpoint.endpoint_number as u32 & 0xF) << 11)
            | HCCHAR_EPDIR_IN
            | (if endpoint.route.low_speed_via_hub {
                HCCHAR_LSPDDEV
            } else {
                0
            })
            | HCCHAR_EPTYPE_INTR
            | ((endpoint.device_address as u32 & 0x7F) << 22);
        write(hcchar_address, hcchar);
        write(
            channel_register(channel, HCINTMSK_OFFSET),
            HCINT_ENABLED_MASK,
        );
        modify(HAINTMSK, 1 << channel, 1 << channel);
    }
    arm_periodic_hid(slot);
    Some(PeriodicHandle {
        slot: slot as u8,
        generation,
    })
}

/// Takes one completed periodic report without waiting. The next QTD is
/// rearmed before returning the bytes, so idle CPU polling is eliminated and
/// the controller resumes polling at the descriptor's interval immediately.
pub fn take_periodic_hid_report(handle: PeriodicHandle, report: &mut [u8]) -> PeriodicRead {
    // The persistent periodic path does not go through `run_packet`, so it
    // claims the diagnostic label itself; otherwise its cache work would be
    // attributed to whichever bulk phase happened to run last.
    set_transfer_label(TransferLabel::InterruptIn);
    let slot = handle.slot as usize;
    if slot >= PERIODIC_HID_SLOT_COUNT
        || PERIODIC_HID_ACTIVE_MASK.load(Ordering::Acquire) & (1 << slot) == 0
        || handle.generation != PERIODIC_HID_GENERATION[slot].load(Ordering::Acquire)
    {
        return PeriodicRead::Error;
    }
    let mut hcint = USB_PERIODIC_PENDING[slot].swap(0, Ordering::AcqRel);
    if hcint & HCINT_CHHLTD == 0 {
        // An idle Interrupt IN endpoint stays pending indefinitely: with
        // descriptor DMA the core NAK-retries without halting the channel,
        // so "no report yet" and "this channel is dead" look identical from
        // the pending mask alone. The channel's own enable bit tells them
        // apart -- a channel the core is still polling has it set.
        if unsafe { read(channel_register(slot + 1, HCCHAR_OFFSET)) } & HCCHAR_CHENA == 0 {
            // Completion clears ChEna before (or concurrently with) the ISR
            // publishing HCINT. Without this masked recheck, foreground can
            // land in that window and call a healthy, just-completed report
            // a stalled channel. That false error eventually resets every
            // USB session, including unrelated MSC.
            let interrupts_were_enabled = crate::interrupts::mask_machine_interrupts();
            hcint |= USB_PERIODIC_PENDING[slot].swap(0, Ordering::AcqRel);
            let hcint_address = channel_register(slot + 1, HCINT_OFFSET);
            let hardware = unsafe { read(hcint_address) };
            if hardware != 0 {
                unsafe { write(hcint_address, hardware) };
                hcint |= hardware;
            }
            let still_disabled =
                unsafe { read(channel_register(slot + 1, HCCHAR_OFFSET)) } & HCCHAR_CHENA == 0;
            crate::interrupts::restore_machine_interrupts(interrupts_were_enabled);

            if hcint & HCINT_CHHLTD == 0 && still_disabled {
                report_stalled_periodic_channel(slot);
                // Invalidate the driver's token so every later read reports
                // an error too, which is what drives the HID driver's
                // consecutive error threshold into asking for a rescan.
                // Without this the endpoint simply goes quiet: no keys, no
                // log, and a device that still shows up in `usbinfo`.
                PERIODIC_HID_GENERATION[slot].fetch_add(1, Ordering::AcqRel);
                return PeriodicRead::Error;
            }
        }
        if hcint & HCINT_CHHLTD == 0 {
            return PeriodicRead::Pending;
        }
    }

    let qtd_address = periodic_qtd_address(slot);
    if !sync_periodic_qtd(slot) {
        return fail_periodic_slot(slot);
    }
    let control_after = unsafe { read(qtd_address) };
    let status = control_after & QTD_STATUS_MASK;
    let mps = PERIODIC_HID_MPS[slot].load(Ordering::Acquire) as usize;
    let remaining = (control_after & QTD_XFER_SIZE_MASK) as usize;
    let transferred = mps.saturating_sub(remaining.min(mps));
    if hcint & HCINT_XFERCOMPL == 0 || hcint & (HCINT_STALL | HCINT_ERROR_MASK) != 0 || status != 0
    {
        PERIODIC_HID_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
        // Leave the channel halted and invalidate the driver's token. Every
        // later foreground read then returns Error as well, allowing the HID
        // driver's consecutive-error threshold to request a full rescan
        // instead of getting stuck forever after one failed completion.
        PERIODIC_HID_GENERATION[slot].fetch_add(1, Ordering::AcqRel);
        return PeriodicRead::Error;
    }

    let buffer_address = periodic_buffer_address(slot);
    if transferred > 0 {
        // Publishing without a successful invalidate would hand the caller
        // the CPU's pre-DMA copy of this slot's buffer, which is the report
        // before the device wrote it -- a keystroke that never happened, or
        // the previous one repeated.
        if !sync_periodic_buffer(slot, CacheDirection::DeviceToHost) {
            return fail_periodic_slot(slot);
        }
        let source =
            unsafe { core::slice::from_raw_parts(buffer_address as *const u8, transferred) };
        let copied = transferred.min(report.len());
        report[..copied].copy_from_slice(&source[..copied]);
    }
    PERIODIC_HID_COMPLETION_COUNT.fetch_add(1, Ordering::Relaxed);
    PERIODIC_HID_PID_DATA1[slot].fetch_xor(true, Ordering::AcqRel);
    arm_periodic_hid(slot);
    PeriodicRead::Complete(transferred.min(report.len()))
}

/// Reports a periodic channel the core has stopped servicing, with the
/// registers needed to find out why it stopped.
fn report_stalled_periodic_channel(slot: usize) {
    let channel = slot + 1;
    PERIODIC_HID_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
    uart::log_u32(
        b"USB HID: periodic channel stalled, channel=",
        channel as u32,
    );
    uart::log_hex(b"USB HID:   HCCHAR=", unsafe {
        read(channel_register(channel, HCCHAR_OFFSET))
    });
    uart::log_hex(b"USB HID:   HCINT=", unsafe {
        read(channel_register(channel, HCINT_OFFSET))
    });
    uart::log_hex(b"USB HID:   HCINTMSK=", unsafe {
        read(channel_register(channel, HCINTMSK_OFFSET))
    });
    uart::log_hex(b"USB HID:   HAINTMSK=", unsafe { read(HAINTMSK) });
    uart::log_hex(b"USB HID:   HCFG=", unsafe { read(HCFG) });
    log_port_state();
}

fn arm_periodic_hid(slot: usize) {
    set_transfer_label(TransferLabel::InterruptIn);
    let channel = slot + 1;
    let mps = PERIODIC_HID_MPS[slot].load(Ordering::Acquire) as usize;
    let qtd_address = periodic_qtd_address(slot);
    let buffer_address = periodic_buffer_address(slot);
    unsafe {
        core::slice::from_raw_parts_mut(buffer_address as *mut u8, mps).fill(0);
        write(
            qtd_address,
            (mps as u32 & QTD_XFER_SIZE_MASK) | QTD_INTR_CPLT | QTD_EOL | QTD_ACTIVE,
        );
        write(qtd_address + 4, buffer_address as u32);
    }
    if !sync_periodic_buffer(slot, CacheDirection::DeviceToHost) || !sync_periodic_qtd(slot) {
        // Arming anyway would point the controller at a descriptor the CPU
        // still holds a dirty copy of. Invalidating this slot's generation
        // makes every later foreground read report an error, which is what
        // drives the HID driver to re-enumerate.
        uart::log_u32(
            b"USB HID: cache sync refused, channel not armed=",
            channel as u32,
        );
        PERIODIC_HID_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
        PERIODIC_HID_GENERATION[slot].fetch_add(1, Ordering::AcqRel);
        return;
    }

    unsafe {
        write(channel_register(channel, HCINTMSK_OFFSET), 0);
        write(channel_register(channel, HCINT_OFFSET), u32::MAX);
    }
    USB_PERIODIC_PENDING[slot].store(0, Ordering::Release);
    unsafe {
        write(
            channel_register(channel, HCINTMSK_OFFSET),
            HCINT_ENABLED_MASK,
        );
        write(
            channel_register(channel, HCTSIZ_OFFSET),
            HCTSIZ_SCHED_INFO_ALL
                | if PERIODIC_HID_PID_DATA1[slot].load(Ordering::Acquire) {
                    HCTSIZ_PID_DATA1
                } else {
                    0
                },
        );
        write(
            channel_register(channel, HCDMA_OFFSET),
            (qtd_address as u32) & 0xFFFF_FE00,
        );
        modify(
            channel_register(channel, HCCHAR_OFFSET),
            HCCHAR_CHENA | HCCHAR_CHDIS,
            HCCHAR_CHENA,
        );
    }
    PERIODIC_HID_REARM_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Synchronizes one persistent periodic slot's descriptor. The bank is
/// 512-byte aligned and each `QtdSlot` occupies a whole 512-byte
/// allocation, so a slot's cache lines belong to nothing else.
fn sync_periodic_qtd(slot: usize) -> bool {
    cache_writeback_invalidate(
        CacheSite::Periodic,
        CacheDirection::Descriptor,
        periodic_qtd_address(slot),
        core::mem::size_of::<QtdSlot>(),
    )
}

/// Synchronizes one persistent periodic slot's report buffer, which is
/// exactly one cache line (`PeriodicReportBuffer`).
fn sync_periodic_buffer(slot: usize, direction: CacheDirection) -> bool {
    cache_writeback_invalidate(
        CacheSite::Periodic,
        direction,
        periodic_buffer_address(slot),
        core::mem::size_of::<PeriodicReportBuffer>(),
    )
}

/// Retires one periodic slot after a refused cache operation, the same way
/// a failed completion is retired: the generation is invalidated so every
/// later read reports an error and the HID driver re-enumerates.
fn fail_periodic_slot(slot: usize) -> PeriodicRead {
    PERIODIC_HID_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
    PERIODIC_HID_GENERATION[slot].fetch_add(1, Ordering::AcqRel);
    PeriodicRead::Error
}

fn periodic_qtd_address(slot: usize) -> usize {
    PERIODIC_HID_QTD.get() as usize + slot * core::mem::size_of::<QtdSlot>()
}

fn periodic_buffer_address(slot: usize) -> usize {
    PERIODIC_HID_BUFFER.get() as usize + slot * core::mem::size_of::<PeriodicReportBuffer>()
}

#[must_use]
fn rebuild_periodic_frame_list() -> bool {
    set_transfer_label(TransferLabel::InterruptIn);
    let active = PERIODIC_HID_ACTIVE_MASK.load(Ordering::Acquire);
    let frame_list = unsafe { &mut *PERIODIC_HID_FRAME_LIST.get() };
    frame_list.entries.fill(0);
    for slot in 0..PERIODIC_HID_SLOT_COUNT {
        if active & (1 << slot) == 0 {
            continue;
        }
        let interval = PERIODIC_HID_INTERVAL[slot].load(Ordering::Acquire) as usize;
        for index in (0..PERIODIC_FRAME_LIST_ENTRIES).step_by(interval.max(1)) {
            frame_list.entries[index] |= 1 << (slot + 1);
        }
    }
    cache_writeback_invalidate(
        CacheSite::PeriodicFrameList,
        CacheDirection::Descriptor,
        PERIODIC_HID_FRAME_LIST.get() as usize,
        core::mem::size_of::<PeriodicFrameList>(),
    )
}

/// Stops the persistent periodic HID channel before a registry teardown or
/// controller reset. Static DMA storage remains valid even if halt recovery is
/// delayed, but periodic scheduling and both DMA addresses are cleared before
/// returning.
pub fn disable_periodic_hid() -> bool {
    set_transfer_label(TransferLabel::InterruptIn);
    let active = PERIODIC_HID_ACTIVE_MASK.load(Ordering::Acquire);
    if active == 0 {
        return true;
    }
    let mut all_halted = true;
    for slot in 0..PERIODIC_HID_SLOT_COUNT {
        if active & (1 << slot) == 0 {
            continue;
        }
        let channel = slot + 1;
        let hcchar_address = channel_register(channel, HCCHAR_OFFSET);
        let mut completion = USB_PERIODIC_PENDING[slot].swap(0, Ordering::AcqRel);
        if unsafe { read(hcchar_address) } & HCCHAR_CHENA != 0 {
            unsafe { modify(hcchar_address, HCCHAR_CHDIS, HCCHAR_CHDIS) };
            if let (Some(hcint), _) =
                wait_for_periodic_channel_halt(slot, crate::startup::cpu_hz() / 10)
            {
                completion |= hcint;
            }
        } else {
            let hcint_address = channel_register(channel, HCINT_OFFSET);
            let hardware = unsafe { read(hcint_address) };
            if hardware != 0 {
                unsafe { write(hcint_address, hardware) };
                completion |= hardware;
            }
        }
        let halted = unsafe { read(hcchar_address) } & HCCHAR_CHENA == 0;
        all_halted &= halted;

        // Preserve the endpoint toggle when changing from persistent
        // periodic DMA to channel-0 frame polling. A report that completed
        // just before the halt consumed the programmed PID even if foreground
        // had not reaped it yet; account for that dropped report here.
        let qtd_address = periodic_qtd_address(slot);
        // A refusal here only costs the toggle bookkeeping below: the
        // channel is already being torn down, and a stale descriptor read
        // must not be treated as a completed report.
        let qtd_synced = sync_periodic_qtd(slot);
        let qtd_status = unsafe { read(qtd_address) } & QTD_STATUS_MASK;
        if qtd_synced
            && completion & HCINT_XFERCOMPL != 0
            && completion & (HCINT_STALL | HCINT_ERROR_MASK) == 0
            && qtd_status == QTD_STATUS_SUCCESS
        {
            PERIODIC_HID_PID_DATA1[slot].fetch_xor(true, Ordering::AcqRel);
        }
        unsafe {
            write(channel_register(channel, HCINTMSK_OFFSET), 0);
            modify(HAINTMSK, 1 << channel, 0);
            write(channel_register(channel, HCDMA_OFFSET), 0);
            write(channel_register(channel, HCSPLT_OFFSET), 0);
            write(channel_register(channel, HCINT_OFFSET), u32::MAX);
            if !halted {
                write(hcchar_address, 0);
            }
        }
        USB_PERIODIC_PENDING[slot].store(0, Ordering::Release);
        PERIODIC_HID_MPS[slot].store(0, Ordering::Release);
        PERIODIC_HID_INTERVAL[slot].store(0, Ordering::Release);
    }
    unsafe {
        modify(HCFG, HCFG_PERSCHEDENA, 0);
        write(HFLBADDR, 0);
    }
    PERIODIC_HID_ACTIVE_MASK.store(0, Ordering::Release);
    // Deliberately not folded into the return value. `HCFG.PerSchedEna` and
    // both DMA addresses have already been cleared, so nothing walks this
    // list any more and a refused writeback of it cannot start anything.
    // The caller treats `false` as "a periodic channel is wedged, the bus
    // needs re-enumeration", which this is not; the refusal is counted and
    // named by `cache_writeback_invalidate` on its own.
    let _ = rebuild_periodic_frame_list();
    all_halted
}

/// Returns the DATA PID to use when a disabled persistent endpoint falls
/// back to channel-0 polling. Valid after [`disable_periodic_hid`] and until
/// the slot is allocated again.
pub fn periodic_hid_fallback_pid(handle: PeriodicHandle) -> Option<bool> {
    let slot = handle.slot as usize;
    if slot >= PERIODIC_HID_SLOT_COUNT
        || handle.generation != PERIODIC_HID_GENERATION[slot].load(Ordering::Acquire)
    {
        return None;
    }
    Some(PERIODIC_HID_PID_DATA1[slot].load(Ordering::Acquire))
}

/// Result of the opt-in channel-1 periodic scheduler diagnostic.
#[derive(Clone, Copy)]
pub struct PeriodicProbeResult {
    pub attempted: bool,
    pub completed: bool,
    pub timed_out: bool,
    pub channel_halted: bool,
    pub requested_interval: u8,
    pub scheduled_interval: u8,
    pub scheduled_entries: u8,
    pub frame_list_address: u32,
    pub frame_list_readback: u32,
    pub hcfg_during: u32,
    pub hcint: u32,
    pub qtd_control: u32,
    pub transferred: usize,
    pub channel1_irqs: u32,
    pub wfi_count: u32,
}

impl PeriodicProbeResult {
    fn unsupported(interval: u8) -> Self {
        Self {
            attempted: false,
            completed: false,
            timed_out: false,
            channel_halted: true,
            requested_interval: interval,
            scheduled_interval: 0,
            scheduled_entries: 0,
            frame_list_address: 0,
            frame_list_readback: 0,
            hcfg_during: unsafe { read(HCFG) },
            hcint: 0,
            qtd_control: 0,
            transferred: 0,
            channel1_irqs: 0,
            wfi_count: 0,
        }
    }
}

/// Runs one HID-sized Interrupt IN QTD through the DWC periodic scheduler.
///
/// This is deliberately an opt-in diagnostic rather than the live HID path:
/// channel 1, the frame list, and `HCFG.PerSchedEna` have not yet been proven
/// on ESP32-P4's Low-Speed root-port mode. Channel 0 remains idle while the
/// shell invokes this function, and every periodic register is disabled again
/// before the stack-owned DMA objects leave scope.
pub fn probe_periodic_interrupt_in(
    endpoint: &Endpoint,
    interval: u8,
    pid_data1: bool,
    buffer: &mut [u8],
) -> PeriodicProbeResult {
    if endpoint.route.split.is_some()
        || buffer.is_empty()
        || buffer.len() > QTD_XFER_SIZE_MASK as usize
        || unsafe { read(HCFG) } & HCFG_PERSCHEDENA != 0
    {
        return PeriodicProbeResult::unsupported(interval);
    }
    set_transfer_label(TransferLabel::InterruptIn);

    let scheduled_interval = periodic_interval_frames(interval);
    let mut frame_list = PeriodicFrameList {
        entries: [0; PERIODIC_FRAME_LIST_ENTRIES],
    };
    let mut scheduled_entries = 0u8;
    for index in (0..PERIODIC_FRAME_LIST_ENTRIES).step_by(scheduled_interval as usize) {
        frame_list.entries[index] = 1 << 1; // periodic channel 1
        scheduled_entries += 1;
    }
    let frame_list_address = &raw mut frame_list as usize;
    let mut qtd = QtdSlot::zeroed();
    let qtd_address = &raw mut qtd as usize;
    // The probe stages its own payload for the same reason `run_packet`
    // does: the caller's `&mut [u8]` has no cache-line alignment to offer.
    let mut staging = PacketStaging::zeroed();
    if buffer.len() > PACKET_STAGING_BYTES {
        return PeriodicProbeResult::unsupported(interval);
    }
    let data_address = staging.bytes.as_mut_ptr() as usize;
    if !cache_writeback_invalidate(
        CacheSite::PeriodicFrameList,
        CacheDirection::Descriptor,
        frame_list_address,
        core::mem::size_of::<PeriodicFrameList>(),
    ) || !cache_writeback_invalidate(
        CacheSite::PeriodicProbe,
        CacheDirection::DeviceToHost,
        data_address,
        PACKET_STAGING_BYTES,
    ) {
        return PeriodicProbeResult::unsupported(interval);
    }
    let qtd_control =
        (buffer.len() as u32 & QTD_XFER_SIZE_MASK) | QTD_INTR_CPLT | QTD_EOL | QTD_ACTIVE;
    unsafe {
        write(qtd_address, qtd_control);
        write(qtd_address + 4, data_address as u32);
    }
    if !cache_writeback_invalidate(
        CacheSite::PeriodicProbe,
        CacheDirection::Descriptor,
        qtd_address,
        core::mem::size_of::<QtdSlot>(),
    ) {
        return PeriodicProbeResult::unsupported(interval);
    }

    let saved_hcfg = unsafe { read(HCFG) };
    let saved_hflbaddr = unsafe { read(HFLBADDR) };
    let irq_before = USB_PERIODIC_INTERRUPT_COUNT[0].load(Ordering::Acquire);
    USB_PERIODIC_PENDING[0].store(0, Ordering::Release);
    unsafe {
        write(CHAN1_HCINTMSK, 0);
        modify(HAINTMSK, 1 << 1, 0);
        write(CHAN1_HCINT, u32::MAX);
        write(CHAN1_HCSPLT, 0);
        write(HFLBADDR, frame_list_address as u32);
        modify(
            HCFG,
            HCFG_FRLISTEN_MASK | HCFG_PERSCHEDENA,
            HCFG_FRLISTEN_32 | HCFG_PERSCHEDENA,
        );
        write(CHAN1_HCINTMSK, HCINT_ENABLED_MASK);
        modify(HAINTMSK, 1 << 1, 1 << 1);

        let hcchar = (endpoint.mps as u32 & 0x7FF)
            | ((endpoint.endpoint_number as u32 & 0xF) << 11)
            | HCCHAR_EPDIR_IN
            | (if endpoint.route.low_speed_via_hub {
                HCCHAR_LSPDDEV
            } else {
                0
            })
            | HCCHAR_EPTYPE_INTR
            | ((endpoint.device_address as u32 & 0x7F) << 22);
        write(CHAN1_HCCHAR, hcchar);
        // FS/LS periodic channels use all eight schedule-info bits. The
        // 32-entry frame list above selects which USB frames own channel 1.
        write(
            CHAN1_HCTSIZ,
            HCTSIZ_SCHED_INFO_ALL | if pid_data1 { HCTSIZ_PID_DATA1 } else { 0 },
        );
        write(CHAN1_HCDMA, (qtd_address as u32) & 0xFFFF_FE00);
        modify(CHAN1_HCCHAR, HCCHAR_CHENA, HCCHAR_CHENA);
    }

    let frame_list_readback = unsafe { read(HFLBADDR) };
    let hcfg_during = unsafe { read(HCFG) };
    let timeout_cycles = crate::startup::cpu_hz().saturating_mul(PERIODIC_PROBE_TIMEOUT_SECONDS);
    let (completion, wfi_count) = wait_for_channel1_halt(timeout_cycles);

    let mut channel_halted = unsafe { read(CHAN1_HCCHAR) } & HCCHAR_CHENA == 0;
    if !channel_halted {
        unsafe {
            modify(CHAN1_HCCHAR, HCCHAR_CHDIS, HCCHAR_CHDIS);
        }
        let _ = wait_for_channel1_halt(crate::startup::cpu_hz() / 10);
        channel_halted = unsafe { read(CHAN1_HCCHAR) } & HCCHAR_CHENA == 0;
    }

    unsafe {
        write(CHAN1_HCINTMSK, 0);
        modify(HAINTMSK, 1 << 1, 0);
        modify(
            HCFG,
            HCFG_FRLISTEN_MASK | HCFG_PERSCHEDENA,
            saved_hcfg & (HCFG_FRLISTEN_MASK | HCFG_PERSCHEDENA),
        );
        write(HFLBADDR, saved_hflbaddr);
        write(CHAN1_HCSPLT, 0);
        write(CHAN1_HCINT, u32::MAX);
        if !channel_halted {
            // Periodic scheduling is now disabled, so channel 1 cannot fetch
            // either stack-owned DMA object. Also remove the stale addresses
            // before returning a failed diagnostic to foreground.
            write(CHAN1_HCDMA, 0);
            write(CHAN1_HCCHAR, 0);
        }
    }
    USB_PERIODIC_PENDING[0].store(0, Ordering::Release);

    let qtd_synced = cache_writeback_invalidate(
        CacheSite::PeriodicProbe,
        CacheDirection::Descriptor,
        qtd_address,
        core::mem::size_of::<QtdSlot>(),
    );
    let control_after = unsafe { read(qtd_address) };
    let remaining = (control_after & QTD_XFER_SIZE_MASK) as usize;
    let status = control_after & QTD_STATUS_MASK;
    let transferred = buffer.len().saturating_sub(remaining.min(buffer.len()));
    let payload_synced = transferred == 0
        || cache_writeback_invalidate(
            CacheSite::PeriodicProbe,
            CacheDirection::DeviceToHost,
            data_address,
            PACKET_STAGING_BYTES,
        );
    if payload_synced && transferred > 0 {
        let published = transferred.min(buffer.len());
        buffer[..published].copy_from_slice(&staging.bytes[..published]);
    }
    let hcint = completion.unwrap_or(0);
    let completed = completion.is_some()
        && qtd_synced
        && payload_synced
        && hcint & HCINT_XFERCOMPL != 0
        && status == QTD_STATUS_SUCCESS;

    PeriodicProbeResult {
        attempted: true,
        completed,
        timed_out: completion.is_none(),
        channel_halted,
        requested_interval: interval,
        scheduled_interval,
        scheduled_entries,
        frame_list_address: frame_list_address as u32,
        frame_list_readback,
        hcfg_during,
        hcint,
        qtd_control: control_after,
        transferred,
        channel1_irqs: USB_PERIODIC_INTERRUPT_COUNT[0]
            .load(Ordering::Acquire)
            .wrapping_sub(irq_before),
        wfi_count,
    }
}

fn periodic_interval_frames(interval: u8) -> u8 {
    let capped = interval.clamp(1, PERIODIC_FRAME_LIST_ENTRIES as u8);
    1 << (7 - capped.leading_zeros() as u8)
}

fn wait_for_channel1_halt(timeout_cycles: u32) -> (Option<u32>, u32) {
    wait_for_periodic_channel_halt(0, timeout_cycles)
}

fn wait_for_periodic_channel_halt(slot: usize, timeout_cycles: u32) -> (Option<u32>, u32) {
    let channel = slot + 1;
    let start = cycle_count();
    let mut observed = 0u32;
    let mut wfi_count = 0u32;
    loop {
        observed |= USB_PERIODIC_PENDING[slot].swap(0, Ordering::AcqRel);
        if observed & HCINT_CHHLTD != 0 {
            return (Some(observed), wfi_count);
        }
        if cycle_count().wrapping_sub(start) >= timeout_cycles {
            return (None, wfi_count);
        }

        let interrupts_were_enabled = crate::interrupts::mask_machine_interrupts();
        observed |= USB_PERIODIC_PENDING[slot].swap(0, Ordering::AcqRel);
        let hardware = unsafe { read(channel_register(channel, HCINT_OFFSET)) };
        if hardware != 0 {
            unsafe { write(channel_register(channel, HCINT_OFFSET), hardware) };
            observed |= hardware;
        }
        if observed & HCINT_CHHLTD == 0 && cycle_count().wrapping_sub(start) < timeout_cycles {
            wfi_count += 1;
            USB_WFI_COUNT.fetch_add(1, Ordering::Relaxed);
            crate::interrupts::wait_for_interrupt();
        }
        crate::interrupts::restore_machine_interrupts(interrupts_were_enabled);
    }
}

/// Runs one single-entry, halt-on-complete QTD on channel 0 and returns the
/// number of bytes actually transferred. Most callers supply one MPS-sized
/// packet. An unsplit descriptor-DMA Bulk IN QTD may instead contain a larger
/// MPS-multiple transfer, which the DWC hardware splits into USB packets while
/// advancing DATA PID internally.
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
    completion_wait: CompletionWait,
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

    let buffer_len = buffer.len();
    if buffer_len > PACKET_STAGING_BYTES {
        // Unreachable through the current callers, which all chunk by an
        // endpoint MPS of at most 512. Truncating a transfer to fit the
        // staging buffer would be far worse than refusing it.
        uart::log_u32(
            b"USB: packet larger than the DMA staging buffer, len=",
            buffer_len as u32,
        );
        return PacketOutcome::Error;
    }
    let mut transfer = Channel0Transfer::new(endpoint, is_setup, pid_data1, buffer);
    let Some(token) = transfer.submit() else {
        return PacketOutcome::CacheSyncFailed;
    };
    let sleep_on_interrupt = completion_wait == CompletionWait::Interrupt;
    let hcint = match await_packet(
        0,
        0,
        timeout_iterations,
        max_split_rounds,
        sleep_on_interrupt,
        false,
    ) {
        Some(halt) => halt.hcint,
        None => {
            if !quiet_timeout {
                uart::log(b"USB: packet timed out waiting for channel halt\r\n");
                let (hcchar, hctsiz, hcdma) = channel0_diagnostic_registers();
                uart::log_hex(b"USB:   before halt HCCHAR=", hcchar);
                uart::log_hex(b"USB:   before halt HCTSIZ=", hctsiz);
                uart::log_hex(b"USB:   before halt HCDMA=", hcdma);
                log_port_state();
            }
            // Leave the channel in a known-idle state regardless of why we
            // gave up, so the next call's fresh HCCHAR/HCTSIZ/HCDMA write is
            // not racing whatever the core was still doing.
            force_halt_channel();
            let progress = transfer.cancel(token);
            let transferred = progress.count();
            // `PollIdleNak` is the manually scheduled Interrupt IN poll,
            // whose caller treats a timeout as "no new report" rather than
            // an error (`hid.rs`). Counting it as a packet failure makes
            // the failure total a measure of how long a keyboard sat idle.
            if completion_wait == CompletionWait::PollIdleNak {
                note_idle_poll_timeout();
            } else {
                transfer.note_failure(PacketFailureKind::HaltTimeout, transferred, 0);
                if !quiet_timeout {
                    log_packet_failure(PacketFailureKind::HaltTimeout, buffer_len, transferred, 0);
                    if !progress.is_known() {
                        uart::log(b"USB:   actual bytes are UNKNOWN, not zero\r\n");
                    }
                }
            }
            return PacketOutcome::Timeout(progress);
        }
    };
    // Fault injection: deliver this completion under a generation the slot
    // never issued, which is what a queued scheduler could do with a
    // completion left over from an earlier packet.
    let completion_token = if take_forced_stale_completion() {
        TransferToken(token.0.wrapping_add(1))
    } else {
        token
    };
    if !transfer.note_completion(completion_token, hcint) {
        note_packet_failure(
            PacketFailureKind::StaleCompletion,
            endpoint.is_in,
            buffer_len,
            0,
            hcint,
            0,
        );
        if !quiet_errors {
            uart::log(b"USB: stale channel completion token\r\n");
        }
        return PacketOutcome::Error;
    }
    transfer.reap(token, quiet_errors)
}

#[allow(dead_code)]
fn log_out_qtd_list_failure(message: &[u8], index: usize, control: u32, hcint: u32) {
    uart::log(message);
    uart::log_u32(b"USB:   QTD index=", index as u32);
    uart::log_hex(b"USB:   QTD control=", control);
    uart::log_hex(b"USB:   HCINT=", hcint);
    uart::log_hex(b"USB:   HCCHAR=", unsafe { read(CHAN0_HCCHAR) });
    uart::log_hex(b"USB:   HCTSIZ=", unsafe { read(CHAN0_HCTSIZ) });
    uart::log_hex(b"USB:   HCDMA=", unsafe { read(CHAN0_HCDMA) });
    log_port_state();
}

/// Rejected v38/v39 experiment: runs one Bulk OUT train as one-packet QTDs.
///
/// The active BOT path halts and rearms channel 0 after every Full-Speed
/// packet. A single 512-byte QTD avoids that boundary but is unreliable on
/// the forced-FS hub topology. This variant keeps each descriptor at one MPS
/// while setting HCTSIZ.NTD so hardware walks all eight descriptors under one
/// activation. After a reported packet error, BOT may rebuild the uncompleted
/// suffix only when this function proves the QTD boundary; an ambiguous list
/// is never replayed.
#[allow(dead_code)]
fn run_out_packet_list(
    endpoint: &Endpoint,
    pid_data1: bool,
    timeout_iterations: u32,
    quiet_errors: bool,
    buffer: &mut [u8],
) -> PacketOutcome {
    let mps = endpoint.mps.max(1) as usize;
    let qtd_count = buffer.len().div_ceil(mps);
    if endpoint.is_in
        || endpoint.route.split.is_some()
        || endpoint.endpoint_type != HCCHAR_EPTYPE_BULK
        || buffer.len() <= mps
        || buffer.len() > PACKET_STAGING_BYTES
        || qtd_count > CHANNEL0_QTD_LIST_CAPACITY
    {
        uart::log(b"USB: invalid direct Bulk OUT QTD-list request\r\n");
        return PacketOutcome::Error;
    }

    let mut staging = PacketStaging::zeroed();
    staging.bytes[..buffer.len()].copy_from_slice(buffer);
    let data_address = staging.bytes.as_mut_ptr() as usize;
    if !cache_writeback_invalidate(
        CacheSite::Channel0Payload,
        CacheDirection::HostToDevice,
        data_address,
        PACKET_STAGING_BYTES,
    ) {
        note_packet_failure(
            PacketFailureKind::CacheSyncRefused,
            false,
            buffer.len(),
            0,
            0,
            0,
        );
        return PacketOutcome::CacheSyncFailed;
    }

    let slot = CHANNEL0_QTD_NEXT.fetch_add(1, Ordering::Relaxed) as usize % CHANNEL0_QTD_SLOT_COUNT;
    let qtd_address = channel0_qtd_address(slot);
    for index in 0..CHANNEL0_QTD_LIST_CAPACITY {
        let descriptor = qtd_address + index * core::mem::size_of::<RawQtd>();
        unsafe {
            write(descriptor, 0);
            write(descriptor + 4, 0);
        }
    }
    let mut submitted_controls = [0u32; CHANNEL0_QTD_LIST_CAPACITY];
    let mut submitted_buffers = [0u32; CHANNEL0_QTD_LIST_CAPACITY];
    for (index, submitted_control) in submitted_controls.iter_mut().take(qtd_count).enumerate() {
        let offset = index * mps;
        let length = (buffer.len() - offset).min(mps);
        let is_last = index + 1 == qtd_count;
        let mut control = length as u32 | QTD_ACTIVE;
        if is_last {
            control |= QTD_INTR_CPLT | QTD_EOL;
        }
        *submitted_control = control;
        submitted_buffers[index] = (data_address + offset) as u32;
        let descriptor = qtd_address + index * core::mem::size_of::<RawQtd>();
        unsafe {
            write(descriptor, control);
            write(descriptor + 4, submitted_buffers[index]);
        }
    }
    if !cache_writeback_invalidate(
        CacheSite::Channel0Qtd,
        CacheDirection::Descriptor,
        qtd_address,
        core::mem::size_of::<Channel0QtdListSlot>(),
    ) {
        note_packet_failure(
            PacketFailureKind::CacheSyncRefused,
            false,
            buffer.len(),
            0,
            0,
            0,
        );
        return PacketOutcome::CacheSyncFailed;
    }

    let hcchar = (endpoint.mps as u32 & 0x7FF)
        | ((endpoint.endpoint_number as u32 & 0xF) << 11)
        | (if endpoint.route.low_speed_via_hub {
            HCCHAR_LSPDDEV
        } else {
            0
        })
        | endpoint.endpoint_type
        | ((endpoint.device_address as u32 & 0x7F) << 22);
    let hctsiz = HCTSIZ_SCHED_INFO_ALL
        | (((qtd_count - 1) as u32) << HCTSIZ_NTD_SHIFT)
        | if pid_data1 { HCTSIZ_PID_DATA1 } else { 0 };

    USB_TRANSFER_GENERATION.fetch_add(1, Ordering::Relaxed);
    USB_SUBMIT_COUNT.fetch_add(1, Ordering::Relaxed);
    prepare_channel0_interrupt();
    unsafe {
        write(CHAN0_HCSPLT, 0);
        write(CHAN0_HCCHAR, hcchar);
        write(CHAN0_HCTSIZ, hctsiz);
        write(CHAN0_HCDMA, (qtd_address as u32) & 0xFFFF_FE00);
        modify(CHAN0_HCCHAR, HCCHAR_CHENA, HCCHAR_CHENA);
    }

    let Some(halt) = await_packet(0, 0, timeout_iterations, 0, true, false) else {
        uart::log(b"USB: QTD list timed out waiting for channel halt\r\n");
        let (hcchar, hctsiz, hcdma) = channel0_diagnostic_registers();
        uart::log_hex(b"USB:   before halt HCCHAR=", hcchar);
        uart::log_hex(b"USB:   before halt HCTSIZ=", hctsiz);
        uart::log_hex(b"USB:   before halt HCDMA=", hcdma);
        log_port_state();
        force_halt_channel();
        USB_CANCEL_COUNT.fetch_add(1, Ordering::Relaxed);
        note_packet_failure(PacketFailureKind::HaltTimeout, false, buffer.len(), 0, 0, 0);
        return PacketOutcome::Timeout(TransferProgress::Unknown);
    };
    let hcint = halt.hcint;
    if !cache_writeback_invalidate(
        CacheSite::Channel0Qtd,
        CacheDirection::Descriptor,
        qtd_address,
        core::mem::size_of::<Channel0QtdListSlot>(),
    ) {
        note_packet_failure(
            PacketFailureKind::CacheSyncRefused,
            false,
            buffer.len(),
            0,
            hcint,
            0,
        );
        return PacketOutcome::CacheSyncFailed;
    }
    USB_REAP_COUNT.fetch_add(1, Ordering::Relaxed);

    let mut controls = [0u32; CHANNEL0_QTD_LIST_CAPACITY];
    let mut buffers_after = [0u32; CHANNEL0_QTD_LIST_CAPACITY];
    for (index, control) in controls.iter_mut().take(qtd_count).enumerate() {
        let descriptor = qtd_address + index * core::mem::size_of::<RawQtd>();
        *control = unsafe { read(descriptor) };
        buffers_after[index] = unsafe { read(descriptor + 4) };
    }
    let failed_index = controls[..qtd_count]
        .iter()
        .position(|control| *control & QTD_STATUS_MASK != QTD_STATUS_SUCCESS);
    let active_index = controls[..qtd_count]
        .iter()
        .position(|control| *control & QTD_ACTIVE != 0);
    let incomplete_index = controls[..qtd_count]
        .iter()
        .position(|control| *control & QTD_XFER_SIZE_MASK != 0);
    for (index, control) in controls[..qtd_count].iter().enumerate() {
        let requested = (buffer.len() - index * mps).min(mps);
        if (*control & QTD_XFER_SIZE_MASK) as usize > requested {
            IMPOSSIBLE_REMAINDERS.fetch_add(1, Ordering::Relaxed);
        }
    }
    let diagnostic_index = failed_index
        .or(active_index)
        .or(incomplete_index)
        .unwrap_or(qtd_count - 1);
    let diagnostic_control = controls[diagnostic_index];
    USB_LAST_REAP_HCINT.store(hcint, Ordering::Release);
    USB_LAST_REAP_QTD_CONTROL.store(diagnostic_control, Ordering::Release);

    if hcint & HCINT_STALL != 0 {
        note_packet_failure(
            PacketFailureKind::Stall,
            false,
            buffer.len(),
            0,
            hcint,
            diagnostic_control,
        );
        if !quiet_errors {
            log_out_qtd_list_failure(
                b"USB: WRITE packet-list stalled\r\n",
                diagnostic_index,
                diagnostic_control,
                hcint,
            );
        }
        return PacketOutcome::Error;
    }
    if hcint & HCINT_ERROR_MASK != 0 {
        note_packet_failure(
            PacketFailureKind::TransactionError,
            false,
            buffer.len(),
            0,
            hcint,
            diagnostic_control,
        );
        if !quiet_errors {
            log_out_qtd_list_failure(
                b"USB: WRITE packet-list transaction error\r\n",
                diagnostic_index,
                diagnostic_control,
                hcint,
            );
        }
        return PacketOutcome::Error;
    }
    if let Some(index) = failed_index {
        let status = controls[index] & QTD_STATUS_MASK;
        let kind = if status == QTD_STATUS_PACKET_ERROR {
            PacketFailureKind::QtdPacketError
        } else {
            PacketFailureKind::QtdInvalidStatus
        };
        note_packet_failure(kind, false, buffer.len(), 0, hcint, controls[index]);
        if !quiet_errors {
            log_out_qtd_list_failure(
                b"USB: WRITE packet-list QTD failed\r\n",
                index,
                controls[index],
                hcint,
            );
        }
        if status != QTD_STATUS_PACKET_ERROR {
            return PacketOutcome::Error;
        }

        // A status-1 QTD is one atomic USB packet that may be retried with
        // the same DATA PID. Earlier descriptors form a reusable prefix only
        // when hardware completed every one, and later descriptors must be
        // bit-for-bit untouched. This is stronger than trusting the failed
        // descriptor's remainder (which is frequently impossible on this
        // core) and lets BOT rebuild a list beginning at the failed packet
        // without replaying an accepted prefix.
        let prefix_complete = controls[..index]
            .iter()
            .all(|control| control & (QTD_ACTIVE | QTD_STATUS_MASK | QTD_XFER_SIZE_MASK) == 0);
        let suffix_untouched = controls[index + 1..qtd_count]
            == submitted_controls[index + 1..qtd_count]
            && buffers_after[index + 1..qtd_count] == submitted_buffers[index + 1..qtd_count];
        if prefix_complete && suffix_untouched {
            let completed_prefix = index * mps;
            if !quiet_errors {
                uart::log_u32(
                    b"USB:   completed QTD-list prefix bytes=",
                    completed_prefix as u32,
                );
            }
            return PacketOutcome::PacketError(TransferProgress::Known(completed_prefix));
        }
        if !quiet_errors {
            uart::log(b"USB: QTD-list boundary is ambiguous; refusing resume\r\n");
        }
        return PacketOutcome::PacketError(TransferProgress::Unknown);
    }

    let every_qtd_complete = controls[..qtd_count]
        .iter()
        .enumerate()
        .all(|(index, control)| {
            let requested = (buffer.len() - index * mps).min(mps);
            control & QTD_ACTIVE == 0
                && (*control & QTD_XFER_SIZE_MASK) as usize == 0
                && requested > 0
        });
    if hcint & HCINT_XFERCOMPL == 0 || !every_qtd_complete {
        note_packet_failure(
            PacketFailureKind::NotTransferComplete,
            false,
            buffer.len(),
            0,
            hcint,
            diagnostic_control,
        );
        if !quiet_errors {
            log_out_qtd_list_failure(
                b"USB: WRITE packet-list did not complete every QTD\r\n",
                diagnostic_index,
                diagnostic_control,
                hcint,
            );
        }
        return PacketOutcome::Error;
    }
    if take_forced_short_out() {
        note_packet_failure(
            PacketFailureKind::ShortOut,
            false,
            buffer.len(),
            buffer.len().saturating_sub(1),
            hcint,
            controls[qtd_count - 1],
        );
        return PacketOutcome::Error;
    }
    PacketOutcome::Ok(buffer.len())
}

/// Failed v30/v31 experiment for directly addressed Full-Speed buffer DMA.
///
/// This is deliberately not called by BOT. Both MC/EC=0 and MC/EC=1 made the
/// first CBW fail on real FS hardware; descriptor DMA with the fixed QTD bank
/// is the active non-Split path. The helper remains temporarily as the exact
/// measured rejected implementation while Stage 3 continues, rather than
/// being mistaken later for an untried fallback.
///
/// `HCFG.DescDMA` belongs to the whole controller. The registry already
/// moves HID to serialized channel-0 polling whenever MSC and HID coexist;
/// this function still refuses to switch modes if a periodic channel is
/// armed, making that topology rule an HCD invariant.
#[allow(dead_code, clippy::too_many_arguments)]
fn run_direct_fs_bulk_packet(
    endpoint: &Endpoint,
    pid_data1: bool,
    timeout_iterations: u32,
    max_nak_rounds: u32,
    quiet_timeout: bool,
    quiet_errors: bool,
    buffer: &mut [u8],
) -> PacketOutcome {
    let xfer_len = buffer.len();
    if endpoint.route.split.is_some()
        || endpoint.endpoint_type != HCCHAR_EPTYPE_BULK
        || endpoint.mps > 64
        || xfer_len > endpoint.mps as usize
        || xfer_len > PACKET_STAGING_BYTES
    {
        if !quiet_errors {
            uart::log(b"USB: direct buffer-DMA request is not FS Bulk\r\n");
        }
        return PacketOutcome::Error;
    }
    if periodic_channels_armed()
        || SPLIT_MODE_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        SPLIT_MODE_CONFLICT_COUNT.fetch_add(1, Ordering::Relaxed);
        if !quiet_errors {
            uart::log(b"USB: direct buffer-DMA blocked by another DMA-mode owner\r\n");
        }
        return PacketOutcome::Error;
    }

    let mut staging = PacketStaging::zeroed();
    if !endpoint.is_in {
        staging.bytes[..xfer_len].copy_from_slice(buffer);
    }
    let data_address = staging.bytes.as_mut_ptr() as usize;
    if !cache_writeback_invalidate(
        CacheSite::Channel0Payload,
        if endpoint.is_in {
            CacheDirection::DeviceToHost
        } else {
            CacheDirection::HostToDevice
        },
        data_address,
        PACKET_STAGING_BYTES,
    ) {
        SPLIT_MODE_ACTIVE.store(false, Ordering::Release);
        note_packet_failure(
            PacketFailureKind::CacheSyncRefused,
            endpoint.is_in,
            xfer_len,
            0,
            0,
            0,
        );
        return PacketOutcome::CacheSyncFailed;
    }

    let hcchar = (endpoint.mps as u32 & 0x7FF)
        | ((endpoint.endpoint_number as u32 & 0xF) << 11)
        | (if endpoint.is_in { HCCHAR_EPDIR_IN } else { 0 })
        | (if endpoint.route.low_speed_via_hub {
            HCCHAR_LSPDDEV
        } else {
            0
        })
        | HCCHAR_EPTYPE_BULK
        // In buffer-DMA mode the DWC host channel expects one transaction
        // in MC/EC. Leaving the field at descriptor-DMA's working value 0
        // made the first 31-byte CBW halt immediately with
        // ChHltd|NAK|XactErr (HCINT=0x92) and HCTSIZ unchanged. Keep this
        // confined to the buffer-DMA path: setting it on direct descriptor
        // DMA was the v26 regression which stopped B2 after four reads.
        | HCCHAR_MC_ONE
        | ((endpoint.device_address as u32 & 0x7F) << 22);
    let hctsiz = (xfer_len as u32 & HCTSIZ_XFERSIZE_MASK)
        | (1 << HCTSIZ_PKTCNT_SHIFT)
        | if pid_data1 { HCTSIZ_PID_DATA1 } else { 0 };

    USB_LAST_REAP_HCINT.store(0, Ordering::Release);
    // There is deliberately no QTD on this path. Clearing the old snapshot
    // prevents BOT retry diagnostics from attributing the preceding
    // descriptor to this packet.
    USB_LAST_REAP_QTD_CONTROL.store(0, Ordering::Release);
    prepare_channel0_interrupt();
    unsafe {
        modify(HCFG, HCFG_DESCDMA, 0);
        write(CHAN0_HCSPLT, 0);
        write(CHAN0_HCCHAR, hcchar);
        write(CHAN0_HCTSIZ, hctsiz);
        write(CHAN0_HCDMA, data_address as u32);
        modify(CHAN0_HCCHAR, HCCHAR_CHENA, HCCHAR_CHENA);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
    DIRECT_BUFFER_PACKET_COUNT.fetch_add(1, Ordering::Relaxed);

    let outcome =
        await_direct_buffer_packet(hctsiz, data_address, timeout_iterations, max_nak_rounds);
    let channel_active = unsafe { read(CHAN0_HCCHAR) } & HCCHAR_CHENA != 0;
    if outcome.is_none() && channel_active {
        force_halt_channel();
    }
    let hctsiz_after = unsafe { read(CHAN0_HCTSIZ) };
    unsafe {
        write(CHAN0_HCSPLT, 0);
        modify(HCFG, HCFG_DESCDMA, HCFG_DESCDMA);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
    SPLIT_MODE_ACTIVE.store(false, Ordering::Release);

    let Some(halt) = outcome else {
        note_packet_failure(
            PacketFailureKind::HaltTimeout,
            endpoint.is_in,
            xfer_len,
            0,
            0,
            0,
        );
        if !quiet_timeout {
            uart::log(b"USB: direct buffer-DMA packet timed out\r\n");
            log_port_state();
        }
        // An abandoned buffer-DMA OUT has no completion proving whether a
        // packet reached the device, so BOT must not resubmit it.
        return PacketOutcome::Timeout(TransferProgress::Unknown);
    };
    let hcint = halt.hcint;
    USB_LAST_REAP_HCINT.store(hcint, Ordering::Release);

    if take_forced_stale_completion() {
        USB_STALE_TOKEN_COUNT.fetch_add(1, Ordering::Relaxed);
        note_packet_failure(
            PacketFailureKind::StaleCompletion,
            endpoint.is_in,
            xfer_len,
            0,
            hcint,
            0,
        );
        if !quiet_errors {
            uart::log(b"USB: stale direct buffer-DMA completion token\r\n");
        }
        return PacketOutcome::Error;
    }

    let remaining = (hctsiz_after & HCTSIZ_XFERSIZE_MASK) as usize;
    let mut progress = if remaining <= xfer_len {
        TransferProgress::Known(xfer_len - remaining)
    } else {
        TransferProgress::Unknown
    };
    if !endpoint.is_in && xfer_len > 0 && progress.is_known() && take_forced_short_out() {
        progress = TransferProgress::Known(progress.count().saturating_sub(1));
    }
    let transferred = progress.count();

    if hcint & HCINT_STALL != 0 {
        note_packet_failure(
            PacketFailureKind::Stall,
            endpoint.is_in,
            xfer_len,
            transferred,
            hcint,
            0,
        );
        if !quiet_errors {
            uart::log(b"USB: direct buffer-DMA transfer STALL\r\n");
        }
        return PacketOutcome::Error;
    }
    if hcint & HCINT_ERROR_MASK != 0 || hcint & HCINT_XFERCOMPL == 0 || !progress.is_known() {
        note_packet_failure(
            PacketFailureKind::TransactionError,
            endpoint.is_in,
            xfer_len,
            transferred,
            hcint,
            0,
        );
        if !quiet_errors {
            uart::log_hex(b"USB: direct buffer-DMA packet error, HCINT=", hcint);
            uart::log_hex(b"USB:   HCTSIZ=", hctsiz_after);
            log_port_state();
        }
        return PacketOutcome::PacketError(progress);
    }
    if !endpoint.is_in && transferred != xfer_len {
        note_packet_failure(
            PacketFailureKind::ShortOut,
            false,
            xfer_len,
            transferred,
            hcint,
            0,
        );
        if !quiet_errors {
            uart::log(b"USB: direct buffer-DMA short OUT\r\n");
        }
        return PacketOutcome::Error;
    }
    if endpoint.is_in && transferred > 0 {
        if !cache_writeback_invalidate(
            CacheSite::Channel0Payload,
            CacheDirection::DeviceToHost,
            data_address,
            PACKET_STAGING_BYTES,
        ) {
            note_packet_failure(
                PacketFailureKind::CacheSyncRefused,
                true,
                xfer_len,
                transferred,
                hcint,
                0,
            );
            return PacketOutcome::CacheSyncFailed;
        }
        buffer[..transferred].copy_from_slice(&staging.bytes[..transferred]);
    }
    PacketOutcome::Ok(transferred)
}

/// Buffer DMA halts a non-periodic channel on NAK instead of letting a QTD
/// absorb the retries. Re-arm the identical one-packet transfer in software,
/// under one cumulative timeout and the caller's bounded round budget.
fn await_direct_buffer_packet(
    hctsiz: u32,
    data_address: usize,
    timeout_iterations: u32,
    max_nak_rounds: u32,
) -> Option<PacketHalt> {
    let start = cycle_count();
    let cycle_budget = timeout_iterations
        .saturating_mul(WAIT_TIMEOUT_CYCLES_PER_ITERATION)
        .max(1);
    let mut rounds = 0u32;
    loop {
        let elapsed = cycle_count().wrapping_sub(start);
        if elapsed >= cycle_budget {
            return None;
        }
        let remaining_iterations = cycle_budget
            .saturating_sub(elapsed)
            .div_ceil(WAIT_TIMEOUT_CYCLES_PER_ITERATION)
            .max(1);
        let hcint = wait_for_channel0_halt(remaining_iterations, WaitStrategy::Interrupt)?;
        if hcint & (HCINT_XFERCOMPL | HCINT_STALL | HCINT_ERROR_MASK) != 0 {
            return Some(PacketHalt {
                hcint,
                complete_split: false,
            });
        }
        if hcint & (HCINT_NAK | HCINT_NYET) == 0 || rounds >= max_nak_rounds {
            return Some(PacketHalt {
                hcint,
                complete_split: false,
            });
        }
        rounds += 1;
        DIRECT_BUFFER_NAK_COUNT.fetch_add(1, Ordering::Relaxed);
        prepare_channel0_interrupt();
        unsafe {
            write(CHAN0_HCTSIZ, hctsiz);
            write(CHAN0_HCDMA, data_address as u32);
            modify(CHAN0_HCCHAR, HCCHAR_CHENA | HCCHAR_CHDIS, HCCHAR_CHENA);
        }
    }
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

/// A cache-line-aligned staging buffer for split packets. Buffer DMA hands
/// `HCDMA` the data pointer itself (Scatter/Gather DMA pointed it at a
/// descriptor instead), and the core requires that pointer to be word
/// aligned -- which an arbitrary `&mut [u8]` sub-slice from a caller is
/// not. Copying through a fixed aligned buffer is cheaper than propagating
/// an alignment requirement up through every caller, at
/// `SPLIT_STAGING_MAX` bytes a packet.
///
/// The alignment is `DMA_ALIGN` rather than the core's word requirement
/// because the buffer also has to be cache-synchronized, and that operation
/// cannot address anything finer than a line. At exactly `SPLIT_STAGING_MAX`
/// = one line, the span this driver writes back belongs to nothing else.
#[repr(C, align(64))]
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
        note_packet_failure(
            PacketFailureKind::SplitRejected,
            endpoint.is_in,
            xfer_len,
            0,
            0,
            0,
        );
        uart::log_hex(
            b"USB: split packet larger than the staging buffer, len=",
            xfer_len as u32,
        );
        return PacketOutcome::Error;
    }
    if !enter_split_mode() {
        note_packet_failure(
            PacketFailureKind::SplitRejected,
            endpoint.is_in,
            xfer_len,
            0,
            0,
            0,
        );
        if !quiet_errors {
            uart::log(b"USB: split transfer blocked by active periodic channels\r\n");
        }
        return PacketOutcome::Error;
    }

    let mut staging = SplitStaging {
        bytes: [0u8; SPLIT_STAGING_MAX],
    };
    if !endpoint.is_in {
        staging.bytes[..xfer_len].copy_from_slice(buffer);
    }
    let data_address = staging.bytes.as_mut_ptr() as usize;
    if !cache_writeback_invalidate(
        CacheSite::SplitStaging,
        if endpoint.is_in {
            CacheDirection::DeviceToHost
        } else {
            CacheDirection::HostToDevice
        },
        data_address,
        SPLIT_STAGING_MAX,
    ) {
        // Nothing has been programmed yet, but split mode has been entered
        // and has to be given back before returning.
        leave_split_mode();
        note_packet_failure(
            PacketFailureKind::CacheSyncRefused,
            endpoint.is_in,
            xfer_len,
            0,
            0,
            0,
        );
        return PacketOutcome::CacheSyncFailed;
    }

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

    let periodic_split = endpoint.endpoint_type == HCCHAR_EPTYPE_INTR;
    // A periodic Interrupt split gets a fixed SSPLIT/CSPLIT mask within one
    // High-Speed full frame. Put its SSPLIT in uframe 0 so the three result
    // checks at 2/3/4 always fit. Control/Bulk may start through uframe 5.
    let start_slot_ready = if periodic_split {
        wait_for_periodic_split_start_slot()
    } else {
        wait_for_split_start_slot()
    };
    if !start_slot_ready {
        leave_split_mode();
        note_packet_failure(
            PacketFailureKind::SplitRejected,
            endpoint.is_in,
            xfer_len,
            0,
            0,
            0,
        );
        if !quiet_errors {
            uart::log(b"USB: split scheduler saw no High-Speed frame progress\r\n");
        }
        return PacketOutcome::Error;
    }
    prepare_channel0_interrupt();
    unsafe {
        modify(HCFG, HCFG_DESCDMA, 0); // buffer DMA for this packet only
        write(CHAN0_HCSPLT, hcsplt);
        write(CHAN0_HCCHAR, hcchar);
        write(CHAN0_HCTSIZ, hctsiz);
        write(CHAN0_HCDMA, data_address as u32);
        modify(CHAN0_HCCHAR, HCCHAR_CHENA, HCCHAR_CHENA);
    }

    // Unlike a directly addressed HID endpoint, each software-driven split
    // phase halts on its ACK/NAK/NYET handshake. It can therefore sleep for
    // the channel IRQ even when an idle HID caller requested a quiet timeout.
    let outcome = await_packet(
        hcsplt,
        hctsiz,
        timeout_iterations,
        max_split_rounds,
        true,
        periodic_split,
    );

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
        // -- would read as its own data. Split mode excludes periodic
        // channels by construction (`enter_split_mode`), so this one can
        // always take the full flush.
        flush_fifos();
    }
    // Restore Scatter/Gather DMA before anything can return: every other
    // packet in this driver depends on it. Splitting is cleared with it, so
    // no half-configured split outlives this call either.
    unsafe {
        write(CHAN0_HCSPLT, 0);
        modify(HCFG, HCFG_DESCDMA, HCFG_DESCDMA);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
    leave_split_mode();

    let Some(halt) = outcome else {
        // Buffer DMA leaves no descriptor to read back, so the only honest
        // actual length after an abandoned split is 0 -- which is also why
        // an OUT that gets here must never be resent as if nothing moved.
        // A periodic Interrupt split whose CSPLIT window ends in NYET
        // expires at the next full-frame boundary by design, and the next
        // rendered frame starts a fresh SSPLIT. That is an idle HID, not a
        // failed packet -- see `await_packet`.
        if periodic_split {
            note_idle_poll_timeout();
        } else {
            note_packet_failure(
                PacketFailureKind::HaltTimeout,
                endpoint.is_in,
                xfer_len,
                0,
                0,
                0,
            );
        }
        if !quiet_timeout {
            uart::log(b"USB: split transfer timed out waiting for the hub's TT\r\n");
            log_packet_failure(PacketFailureKind::HaltTimeout, xfer_len, 0, 0);
            log_port_state();
        }
        // Buffer DMA leaves no descriptor behind, so an abandoned split
        // has no accounting for what reached the wire.
        return PacketOutcome::Timeout(TransferProgress::Unknown);
    };
    let hcint = halt.hcint;

    if hcint & HCINT_STALL != 0 {
        note_packet_failure(
            PacketFailureKind::Stall,
            endpoint.is_in,
            xfer_len,
            0,
            hcint,
            0,
        );
        if !quiet_errors {
            uart::log(b"USB: split transfer STALL\r\n");
        }
        return PacketOutcome::Error;
    }
    if hcint & HCINT_ERROR_MASK != 0 {
        note_packet_failure(
            PacketFailureKind::TransactionError,
            endpoint.is_in,
            xfer_len,
            0,
            hcint,
            0,
        );
        if !quiet_errors {
            uart::log(if halt.complete_split {
                b"USB: split failed during CSPLIT\r\n"
            } else {
                b"USB: split failed during SSPLIT\r\n"
            });
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
        if !cache_writeback_invalidate(
            CacheSite::SplitStaging,
            CacheDirection::DeviceToHost,
            data_address,
            SPLIT_STAGING_MAX,
        ) {
            note_packet_failure(
                PacketFailureKind::CacheSyncRefused,
                true,
                xfer_len,
                transferred,
                hcint,
                0,
            );
            return PacketOutcome::CacheSyncFailed;
        }
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
/// `max_split_rounds` is a *soft* budget for non-periodic Control/Bulk
/// transfers, and deliberately so: it stops the sequence at the first safe
/// boundary at or after that many rounds, rather than the moment it is
/// reached. A non-periodic split may only be abandoned once the TT has let
/// go of it -- USB2.0 11.17 requires the host to keep issuing complete splits
/// until the TT answers something other than NYET, so a NAK (the TT
/// discarding its buffer) or a conclusion is the only legal place to walk
/// away.
///
/// Periodic Interrupt splits have a different boundary: all of their start
/// and complete splits belong to one High-Speed full frame. If its scheduled
/// CSPLIT window ends in NYET, the periodic transaction expires at the next
/// full-frame boundary. `periodic_split` waits for that boundary and returns
/// a quiet timeout; the next rendered frame starts a new SSPLIT instead of
/// spinning thousands of times on a transaction whose schedule has ended.
///
/// Getting this wrong is not a subtle protocol nicety. When an idle
/// keyboard poll gave up as soon as its budget ran out, it left the hub's
/// TT holding a transaction nobody ever collected, and the *next* unrelated
/// control transfer to that hub failed with `XCS_XACT_ERR` -- the hub
/// looked like it had died. It went unnoticed at first only because the
/// frame loop was resetting the whole bus every few seconds anyway, which
/// cleared the wedged TT as a side effect.
#[derive(Clone, Copy)]
struct PacketHalt {
    hcint: u32,
    complete_split: bool,
}

fn await_packet(
    hcsplt: u32,
    hctsiz: u32,
    timeout_iterations: u32,
    max_split_rounds: u32,
    sleep_on_interrupt: bool,
    periodic_split: bool,
) -> Option<PacketHalt> {
    let mut rounds = 0u32;
    let mut complete_split = false;
    let split_full_frame = hs_full_frame();
    loop {
        let strategy = if sleep_on_interrupt {
            WaitStrategy::Interrupt
        } else {
            WaitStrategy::Poll
        };
        let hcint = wait_for_channel0_halt(timeout_iterations, strategy)?;

        if hcsplt & HCSPLT_SPLTENA != 0 {
            SPLIT_ROUND_COUNT.fetch_add(1, Ordering::Relaxed);
        }

        if hcsplt & HCSPLT_SPLTENA == 0 {
            return Some(PacketHalt {
                hcint,
                complete_split: false,
            });
        }
        // Only a bare handshake keeps a split packet going; anything that
        // concludes it (data moved, STALL, error) is the caller's business.
        if hcint & (HCINT_XFERCOMPL | HCINT_STALL | HCINT_ERROR_MASK) != 0 {
            return Some(PacketHalt {
                hcint,
                complete_split,
            });
        }
        rounds += 1;

        if periodic_split && hcint & HCINT_NAK != 0 {
            // NAK is the completed periodic transaction's ordinary "no
            // report" result (or a TT which could not accept this slot).
            // Its buffer is already released. Starting another SSPLIT in
            // the same scheduling window both violates bInterval and uses
            // up the CSPLIT slots reserved for the next transaction.
            return None;
        }

        // A NAK invalidates the TT's buffer for this transaction, so the
        // next step is a fresh start split rather than another complete
        // split. Anything else (the ACK that accepts a start split, a NYET
        // that says "not yet") continues into the complete-split half.
        let next_is_complete_split = hcint & HCINT_NAK == 0;

        if !next_is_complete_split && rounds >= max_split_rounds {
            return None; // out of budget, and the TT has let go: safe to stop
        }
        if periodic_split
            && complete_split
            && hcint & HCINT_NYET != 0
            && (rounds >= max_split_rounds
                || hs_full_frame() != split_full_frame
                || unsafe { read(HFNUM) } & HFNUM_UFRAME_MASK == HFNUM_UFRAME_MASK)
        {
            // An idle Interrupt IN commonly ends its available CSPLITs in
            // NYET. Do not apply the Control/Bulk rule that can chase NYET
            // indefinitely: after this periodic frame expires, the TT is no
            // longer holding a transaction that must be collected. Waiting
            // for the boundary before restoring descriptor DMA also keeps a
            // fresh packet from colliding with the expiring TT state.
            let _ = wait_for_next_hs_full_frame(split_full_frame);
            return None;
        }
        if rounds >= SPLIT_HARD_ROUND_CAP {
            // Last resort against a wedged TT that answers NYET forever.
            // Leaving mid-sequence is exactly what the doc comment above
            // warns about, so this is set high enough never to be the
            // ordinary way out.
            uart::log(b"USB: giving up mid-split; the hub's TT never answered\r\n");
            return None;
        }
        // The TT needs downstream bus time after accepting an SSPLIT. DWC2's
        // reference scheduler advances two microframes after a start split;
        // immediately re-enabling the channel can put CSPLIT in the same
        // microframe and stricter hubs answer XactErr. Repeated CSPLITs after
        // NYET advance one microframe. A NAK releases the TT buffer and starts
        // a new SSPLIT in a legal 0..5 slot.
        let schedule_ready = if next_is_complete_split {
            wait_split_microframes(if complete_split { 1 } else { 2 })
        } else {
            wait_split_microframes(1) && wait_for_split_start_slot()
        };
        if !schedule_ready {
            uart::log(b"USB: split scheduler lost High-Speed frame progress\r\n");
            return None;
        }
        if periodic_split && next_is_complete_split && hs_full_frame() != split_full_frame {
            // Foreground interrupt latency can skip over the intended
            // microframe even though HFNUM itself advanced normally. Never
            // launch a periodic CSPLIT in the next full frame: its TT window
            // belonged to the frame containing the SSPLIT and has expired.
            return None;
        }
        complete_split = next_is_complete_split;
        prepare_channel0_interrupt();
        unsafe {
            if complete_split {
                write(CHAN0_HCSPLT, hcsplt | HCSPLT_COMPSPLT);
            } else {
                // Starting over: put back the packet count and PID the core
                // may have consumed on the attempt that just NAKed. HCDMA
                // still points at the same staging buffer, and nothing was
                // transferred, so this re-arms the identical packet.
                write(CHAN0_HCSPLT, hcsplt);
                write(CHAN0_HCTSIZ, hctsiz);
            }
            modify(CHAN0_HCCHAR, HCCHAR_CHENA | HCCHAR_CHDIS, HCCHAR_CHENA);
        }
    }
}

/// Waits until an SSPLIT can leave at least two later microframes in the
/// current frame for its first CSPLIT.
fn wait_for_split_start_slot() -> bool {
    for _ in 0..SPLIT_FRAME_WAIT_ITERATIONS {
        let microframe = unsafe { read(HFNUM) } & HFNUM_UFRAME_MASK;
        if microframe <= LAST_SSPLIT_UFRAME {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Aligns a periodic SSPLIT with uframe 0. The following CSPLIT attempts are
/// then deterministically placed in uframes 2, 3, and 4 instead of inheriting
/// the fixed phase difference between the 1kHz system tick and USB SOF.
fn wait_for_periodic_split_start_slot() -> bool {
    let initial = unsafe { read(HFNUM) } & HFNUM_FRNUM_MASK;
    let initial_full_frame = initial >> 3;
    let started_in_uframe_zero = initial & HFNUM_UFRAME_MASK == PERIODIC_SSPLIT_UFRAME;
    for _ in 0..SPLIT_FRAME_WAIT_ITERATIONS {
        let now = unsafe { read(HFNUM) } & HFNUM_FRNUM_MASK;
        if now & HFNUM_UFRAME_MASK == PERIODIC_SSPLIT_UFRAME
            && (!started_in_uframe_zero || now >> 3 != initial_full_frame)
        {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Waits for High-Speed bus time rather than a CPU-delay approximation, so
/// CPU clock changes cannot collapse SSPLIT/CSPLIT spacing.
fn wait_split_microframes(count: u32) -> bool {
    let start = unsafe { read(HFNUM) } & HFNUM_FRNUM_MASK;
    for _ in 0..SPLIT_FRAME_WAIT_ITERATIONS {
        let now = unsafe { read(HFNUM) } & HFNUM_FRNUM_MASK;
        if now.wrapping_sub(start) & HFNUM_FRNUM_MASK >= count {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

#[inline]
fn hs_full_frame() -> u32 {
    (unsafe { read(HFNUM) } & HFNUM_FRNUM_MASK) >> 3
}

/// Waits for the periodic TT transaction's 1ms scheduling window to expire.
/// A fresh HID poll may issue a new SSPLIT after this returns.
fn wait_for_next_hs_full_frame(start_full_frame: u32) -> bool {
    for _ in 0..SPLIT_FRAME_WAIT_ITERATIONS {
        if hs_full_frame() != start_full_frame {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Enters the controller-wide buffer-DMA mode used by Split transactions.
///
/// Periodic descriptor DMA and Split buffer DMA cannot coexist on this DWC.
/// The registry deliberately leaves HS-hub HIDs on the serialized Split
/// fallback; this guard turns that topology rule into an HCD invariant so a
/// future allocator change cannot silently switch DMA mode underneath an
/// active periodic channel.
fn enter_split_mode() -> bool {
    if periodic_channels_armed()
        || SPLIT_MODE_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        SPLIT_MODE_CONFLICT_COUNT.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    SPLIT_PACKET_COUNT.fetch_add(1, Ordering::Relaxed);
    true
}

fn leave_split_mode() {
    SPLIT_MODE_ACTIVE.store(false, Ordering::Release);
}

/// Logs the raw root-port register alongside a failed transfer. A device
/// that has stopped answering says nothing about *why* on its own, while
/// HPRT distinguishes the main cases at a glance: still connected and
/// enabled (bits 0 and 2 set) means the failure is a protocol-level one,
/// a cleared enable bit means the core dropped the port, and
/// prtovrcurract (bit 4) means the board's 5V supply gave up.
fn log_port_state() {
    let hprt = unsafe { read(HPRT) };
    uart::log_hex(b"USB:   HPRT=", hprt);
    log_port_flags(hprt);
    // A Full-Speed device may enter suspend after a few milliseconds without
    // bus activity. Sampling HFNUM across one millisecond tells a failed
    // control-transfer caller whether this host is still generating frames.
    let hfnum = unsafe { read(HFNUM) };
    uart::log_hex(b"USB:   HFNUM before +1ms=", hfnum);
    delay_us(1_000);
    uart::log_hex(b"USB:   HFNUM +1ms=", unsafe { read(HFNUM) });
}

/// Spells out the port state and everything the port has been through since
/// the bus came up, so a failure log answers "was this a power problem?"
/// without anyone decoding HPRT by hand.
fn log_port_flags(hprt: u32) {
    uart::log(b"USB:   port now:");
    uart::log(if hprt & HPRT_PRTCONNSTS != 0 {
        b" connected" as &[u8]
    } else {
        b" DISCONNECTED"
    });
    uart::log(if hprt & HPRT_PRTENA != 0 {
        b" enabled" as &[u8]
    } else {
        b" NOT-ENABLED"
    });
    uart::log(if hprt & HPRT_PRTPWR != 0 {
        b" powered" as &[u8]
    } else {
        b" UNPOWERED"
    });
    if hprt & HPRT_PRTOVRCURRACT != 0 {
        uart::log(b" OVER-CURRENT");
    }
    uart::log(b"\r\n");

    let history = port_event_history();
    uart::log(b"USB:   port since bus came up:");
    if history == 0 {
        uart::log(b" no events");
    } else {
        if history & (HPRT_PRTOVRCURRACT | HPRT_PRTOVRCURRCHNG) != 0 {
            uart::log(b" OVER-CURRENT");
        }
        if history & HPRT_PRTCONNDET != 0 {
            uart::log(b" connect-change");
        }
        if history & HPRT_PRTENCHNG != 0 {
            uart::log(b" enable-change");
        }
    }
    uart::log(b"\r\n");
}

/// Explicitly requests a channel halt and waits (briefly) for it, so a
/// channel left mid-transaction by a timed-out packet does not race the
/// next packet's configuration. Best-effort: even if the halt never
/// confirms, `HCINT` is still cleared so stale bits cannot be misread as
/// belonging to the next transfer.
fn force_halt_channel() {
    // A completed channel has already cleared ChEna. Asking ChDis of an
    // inactive channel cannot produce another halt interrupt on this core;
    // treating that missing interrupt as a controller failure creates a
    // false global re-enumeration after an ordinary completion race.
    if unsafe { read(CHAN0_HCCHAR) } & HCCHAR_CHENA == 0 {
        prepare_channel0_interrupt();
        return;
    }
    unsafe {
        modify(CHAN0_HCCHAR, HCCHAR_CHDIS, HCCHAR_CHDIS);
    }
    // Cleanup must remain short even if the core fails to raise a halt IRQ;
    // sleeping until the next display frame would unnecessarily add ~17 ms
    // to an already failed transfer.
    let halted = wait_for_channel0_halt(HALT_CONFIRM_ITERATIONS, WaitStrategy::Poll).is_some();
    if !halted {
        // Everything this driver does goes through channel 0. Carrying on
        // as if the cleanup had worked is what turned one failed transfer
        // into a bus that stayed dead: say so, and let the registry decide
        // how far to escalate.
        uart::log(b"USB: channel 0 did not halt; the bus needs re-enumeration\r\n");
        note_bus_unusable();
    }
    prepare_channel0_interrupt();
}

#[derive(Clone, Copy)]
enum WaitStrategy {
    /// Sleep until the USB or another enabled interrupt fires. Used for
    /// control, bulk, and split phases which are expected to halt normally.
    Interrupt,
    /// Retain the bounded foreground poll for directly addressed idle HID
    /// endpoints (descriptor DMA retries NAK without halting) and cleanup.
    Poll,
}

/// Waits for channel 0 to halt and consumes the status published by the ISR.
///
/// The interrupt path masks `mstatus.MIE`, rechecks both the Atomic snapshot
/// and HCINT, then executes `wfi`. This closes the classic check-before-sleep
/// race: a USB source arriving after the recheck remains pending and wakes the
/// core, then is dispatched as soon as MIE is restored. The direct HCINT read
/// is a recovery path for a routing failure, not a continuous success-path
/// poll.
fn wait_for_channel0_halt(timeout_iterations: u32, strategy: WaitStrategy) -> Option<u32> {
    let start = cycle_count();
    let mut observed = 0u32;
    match strategy {
        WaitStrategy::Poll => {
            USB_POLL_WAIT_COUNT.fetch_add(1, Ordering::Relaxed);
            let mut remaining = timeout_iterations;
            loop {
                observed |= USB_CHANNEL0_PENDING.swap(0, Ordering::AcqRel);
                observed |= take_channel0_hardware_status();
                if observed & HCINT_CHHLTD != 0 {
                    record_wait_cycles(start);
                    return Some(observed);
                }
                if remaining == 0 {
                    record_wait_cycles(start);
                    return None;
                }
                remaining -= 1;
                core::hint::spin_loop();
            }
        }
        WaitStrategy::Interrupt => {
            USB_SLEEP_WAIT_COUNT.fetch_add(1, Ordering::Relaxed);
            let cycle_budget = timeout_iterations
                .saturating_mul(WAIT_TIMEOUT_CYCLES_PER_ITERATION)
                .max(1);
            loop {
                observed |= USB_CHANNEL0_PENDING.swap(0, Ordering::AcqRel);
                if observed & HCINT_CHHLTD != 0 {
                    record_wait_cycles(start);
                    return Some(observed);
                }
                if cycle_count().wrapping_sub(start) >= cycle_budget {
                    record_wait_cycles(start);
                    return None;
                }

                let interrupts_were_enabled = crate::interrupts::mask_machine_interrupts();
                observed |= USB_CHANNEL0_PENDING.swap(0, Ordering::AcqRel);
                observed |= take_channel0_hardware_status();
                if observed & HCINT_CHHLTD == 0 && cycle_count().wrapping_sub(start) < cycle_budget
                {
                    USB_WFI_COUNT.fetch_add(1, Ordering::Relaxed);
                    crate::interrupts::wait_for_interrupt();
                }
                crate::interrupts::restore_machine_interrupts(interrupts_were_enabled);
            }
        }
    }
}

/// Acknowledges channel status only when foreground has beaten the ISR or the
/// interrupt route failed. The normal interrupt wait reads this once inside
/// its masked check-before-sleep window rather than continuously.
fn take_channel0_hardware_status() -> u32 {
    let hardware = unsafe { read(CHAN0_HCINT) };
    if hardware != 0 {
        unsafe { write(CHAN0_HCINT, hardware) };
    }
    hardware
}

fn record_wait_cycles(start: u32) {
    let elapsed = cycle_count().wrapping_sub(start);
    USB_LAST_WAIT_CYCLES.store(elapsed, Ordering::Release);
    let mut maximum = USB_MAX_WAIT_CYCLES.load(Ordering::Relaxed);
    while elapsed > maximum {
        match USB_MAX_WAIT_CYCLES.compare_exchange_weak(
            maximum,
            elapsed,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(current) => maximum = current,
        }
    }
}

#[inline(always)]
fn cycle_count() -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("rdcycle {value}", value = out(reg) value, options(nomem, nostack));
    }
    value
}

// ------------------------------------------------------------------------
// Cache sync, timing, and raw MMIO
// ------------------------------------------------------------------------

/// Writes back dirty cache lines over `address..address+length` and
/// invalidates them, matching `sdmmc.rs`'s helper of the same name (same
/// ROM call, same reasoning: the QTD list and transfer buffers here are
/// DMA-shared memory, exactly like SD's IDMAC descriptors).
///
/// The result is deliberately dropped here while `sdmmc.rs` treats it as a
/// transfer failure. The buffers on this side are declared with an explicit
/// alignment where DMA touches them, and this path has been through the
/// acceptance testing in `docs/USB_WRITE_STABILITY_PLAN.md` as it stands;
/// turning refusals into failures here is a change to a verified transport
/// that belongs with its own bus testing, not with an SD card fix. Stage 1
/// of `docs/USB_BOT_HCD_REFACTOR_PLAN.md` is that testing; until then this
/// records what a refusal would have cost instead of acting on it.
///
/// `site` and `direction` say which DMA-shared object this covers, and the
/// transfer label the owning layer published says which transfer it belongs
/// to. A refusal counted only as a total cannot be told apart from an
/// unrelated one on the periodic HID path, and the two have nothing to do
/// with each other.
#[must_use]
fn cache_writeback_invalidate(
    site: CacheSite,
    direction: CacheDirection,
    address: usize,
    length: usize,
) -> bool {
    // The start address is this driver's own responsibility, not the ROM
    // routine's: it refuses a span that begins mid-line rather than rounding
    // down, and rounding down here would drag another owner's dirty lines
    // into the operation. Every DMA-shared object below is declared with at
    // least `DMA_ALIGN`, so a failure here is a declaration that was
    // weakened, not a runtime condition -- which is why it is checked
    // rather than assumed.
    let aligned = address.is_multiple_of(DMA_ALIGN);
    // Whole cache lines, upward only. Each object's allocated size is a
    // multiple of its own alignment, so the rounded span stays inside it.
    let lines = length.div_ceil(DMA_ALIGN) * DMA_ALIGN;
    let label = transfer_label();
    let forced = take_forced_cache_refusal(label);
    if aligned && !forced && crate::psram::writeback_invalidate(address, lines) {
        return true;
    }
    CACHE_REFUSALS.fetch_add(1, Ordering::Relaxed);
    CACHE_REFUSALS_BY_SITE[site.index()].fetch_add(1, Ordering::Relaxed);
    CACHE_REFUSALS_BY_DIRECTION[direction.index()].fetch_add(1, Ordering::Relaxed);
    CACHE_REFUSALS_BY_LABEL[label.index()].fetch_add(1, Ordering::Relaxed);
    LAST_CACHE_REFUSAL_ADDRESS.store(address as u32, Ordering::Relaxed);
    LAST_CACHE_REFUSAL_LENGTH.store(length as u32, Ordering::Relaxed);
    LAST_CACHE_REFUSAL_LABEL.store(label.index() as u32, Ordering::Relaxed);
    // Described once. A refusal repeats for every transfer that uses the
    // same buffer, and the address and length are what identify which one:
    // a 31-byte span is a CBW, 13 is a CSW, a whole number of blocks is
    // payload.
    if !CACHE_REFUSAL_REPORTED.swap(true, Ordering::Relaxed) {
        uart::log(b"USB: cache writeback REFUSED over a DMA buffer\r\n");
        uart::log_hex(b"USB:   address=", address as u32);
        uart::log_u32(b"USB:   length=", length as u32);
        uart::log(b"USB:   phase=");
        uart::log(label.name().as_bytes());
        uart::log(b"\r\n");
        uart::log(if forced {
            b"USB:   refusal injected on purpose by 'usbcachefail'\r\n" as &[u8]
        } else if aligned {
            b"USB:   the cache controller refused the span\r\n"
        } else {
            b"USB:   the span does not start on a cache-line boundary\r\n"
        });
        uart::log(b"USB:   the transfer is failed rather than started\r\n");
    }
    false
}

/// How many cache writebacks have been refused since boot.
pub fn cache_refusal_count() -> u32 {
    CACHE_REFUSALS.load(Ordering::Relaxed)
}

/// Most recently reaped channel-0 HCINT and QTD control word. Intended for
/// rare upper-layer diagnostics immediately after an impossible response.
pub fn last_channel0_reap() -> (u32, u32) {
    (
        USB_LAST_REAP_HCINT.load(Ordering::Acquire),
        USB_LAST_REAP_QTD_CONTROL.load(Ordering::Acquire),
    )
}

/// Channel-0 registers at a rare upper-layer diagnostic point. Callers use
/// this immediately after a failed descriptor has been reaped and before its
/// recovery cleanup, so the values describe the channel that actually failed.
pub fn channel0_diagnostic_registers() -> (u32, u32, u32) {
    unsafe { (read(CHAN0_HCCHAR), read(CHAN0_HCTSIZ), read(CHAN0_HCDMA)) }
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
