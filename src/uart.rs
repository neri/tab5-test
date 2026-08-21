//! USB/UART serial output.

//! USB Serial/JTAG output for ESP32-P4 ECO2.

use core::sync::atomic::{AtomicBool, Ordering};

const USB_SERIAL_JTAG_BASE: usize = 0x500D_2000;
const EP1_FIFO: *mut u32 = USB_SERIAL_JTAG_BASE as *mut u32;
const EP1_CONF: *mut u32 = (USB_SERIAL_JTAG_BASE + 0x04) as *mut u32;
const INT_RAW: *mut u32 = (USB_SERIAL_JTAG_BASE + 0x08) as *mut u32;
const INT_CLR: *mut u32 = (USB_SERIAL_JTAG_BASE + 0x14) as *mut u32;
const TX_FIFO_FREE: u32 = 1 << 1;
const WR_DONE: u32 = 1;
const SOF_INT: u32 = 1 << 1;

/// Polls spent waiting for a host that enumerated but stopped draining the CDC
/// endpoint. Reached only when the SOF probe says someone is still on the bus.
const FIFO_POLL_LIMIT: u32 = 1_000_000;

/// A host frames the bus every 1 ms, so two frames are enough to tell "the raw
/// bit was cleared a moment ago" from "nobody is driving the bus". The count
/// assumes the 360 MHz application clock; on the 90 MHz boot clock the probe
/// just waits four times longer, which is still bounded.
const SOF_PROBE_CYCLES: u32 = 2 * 360_000;

/// Last observed bus state. Starts optimistic so that boot logs are not dropped
/// before the first probe has had a chance to run, and flips back on its own as
/// soon as SOF packets reappear, so plugging USB in after boot resumes output.
static HOST_ATTACHED: AtomicBool = AtomicBool::new(true);

#[used]
#[unsafe(link_section = ".dram.rodata.uart")]
static HEX: [u8; 16] = *b"0123456789ABCDEF";

/// Sends the hello-world line over the Tab5's native USB serial port.
pub fn hello_world() {
    write(b"Hello, world! (UART)\r\n");
}

/// Writes a diagnostic line from another hardware module.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.uart")]
pub fn log(bytes: &[u8]) {
    write(bytes);
}

/// Writes a memory-mapped register value without requiring formatting support.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.uart")]
pub fn log_hex(label: &[u8], value: u32) {
    let mut output = [0u8; 11];
    output[0] = b'0';
    output[1] = b'x';
    for index in 0..8 {
        output[index + 2] = HEX[((value >> (28 - index * 4)) & 0xF) as usize];
    }
    output[10] = b'\n';
    write(label);
    write(&output);
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.uart")]
fn write(bytes: &[u8]) {
    for &byte in bytes {
        if !wait_for_fifo_room() {
            // Drop the rest of the line instead of stalling the firmware on a
            // FIFO that nobody is draining.
            break;
        }

        unsafe { EP1_FIFO.write_volatile(byte as u32) };
    }

    // Commit the short USB CDC packet to the host. Harmless while detached: the
    // hardware has no way to discard whatever is already queued, so those bytes
    // simply surface as a stale fragment once a host shows up.
    unsafe { EP1_CONF.write_volatile(WR_DONE) };
}

/// Waits for space in the TX FIFO. Returns false once the transfer should be
/// abandoned, either because no host is on the bus or because an attached host
/// stopped reading.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.uart")]
fn wait_for_fifo_room() -> bool {
    if fifo_has_room() {
        return true;
    }

    // A full FIFO is the only case worth asking about the bus: while a host is
    // draining the endpoint this check never runs.
    if !host_attached() {
        return false;
    }

    let mut remaining = FIFO_POLL_LIMIT;
    while !fifo_has_room() {
        if remaining == 0 {
            return false;
        }
        remaining -= 1;
    }

    true
}

/// Reports whether a host is currently framing the bus, using the SOF raw
/// interrupt bit as the evidence. Nothing else in the firmware touches that
/// bit, so a set bit means a frame arrived since the last check here.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.uart")]
fn host_attached() -> bool {
    if take_sof() {
        HOST_ATTACHED.store(true, Ordering::Relaxed);
        return true;
    }

    // While detached the bit stays clear, so repeated calls cost one read and
    // the next frame after a re-plug turns output back on above.
    if !HOST_ATTACHED.load(Ordering::Relaxed) {
        return false;
    }

    // Otherwise the bit may just have been consumed less than a frame ago; give
    // the host two frames to prove it is still there.
    let start = cycle_count();
    while cycle_count().wrapping_sub(start) < SOF_PROBE_CYCLES {
        if take_sof() {
            return true;
        }
    }

    HOST_ATTACHED.store(false, Ordering::Relaxed);
    false
}

/// Consumes the SOF raw interrupt bit, reporting whether it was set.
#[inline(always)]
fn take_sof() -> bool {
    unsafe {
        if INT_RAW.read_volatile() & SOF_INT == 0 {
            return false;
        }
        INT_CLR.write_volatile(SOF_INT);
    }

    true
}

#[inline(always)]
fn fifo_has_room() -> bool {
    unsafe { EP1_CONF.read_volatile() & TX_FIFO_FREE != 0 }
}

#[inline(always)]
fn cycle_count() -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("rdcycle {value}", value = out(reg) value, options(nomem, nostack));
    }
    value
}
