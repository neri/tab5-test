//! ESP32-P4 PPA (pixel-processing accelerator), used here only for filling.
//!
//! The reason this block is interesting is bandwidth, not features. A
//! full-screen clear written by the CPU costs ~86 ms because every 2-byte
//! pixel store misses a 64-byte cache line, and a write-allocate cache reads
//! that line back from PSRAM before overwriting it -- on the same read channel
//! the DSI scanout is fighting for. The CPU cannot even overlap those misses,
//! so it tops out at 20 MB/s (`membench`). A PPA fill issues none of those
//! reads and is not serialised on cache-miss latency; measured on this board,
//! the same clear takes 12 ms and *lowers* the panel's underrun rate.
//!
//! The PPA has no DMA of its own. Its three operations (scale/rotate/mirror,
//! blend, fill) are fed by `dma2d` channels, and fill is a mode of the
//! blending engine rather than a separate unit -- so a fill is configured on
//! both sides at once: a descriptor and a channel on the 2D-DMA, a colour and
//! a block size here. Having no input image, it needs one RX channel and
//! nothing on the TX side.
//!
//! Checked against ESP-IDF v5.5.3 (`hal/esp32p4/ppa_ll.h` and
//! `esp_driver_ppa/src/ppa_fill.c`, plus the `hw_ver1` register headers, which
//! are the ones describing this ECO2 part); no ESP-IDF code is linked in.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::dma2d::{self, Block, Descriptor};
use crate::uart;

/// Whether `init` found both this block and the 2D-DMA answering. Fills
/// consult it so a board where the bring-up failed keeps drawing through the
/// CPU rather than starting transfers that never complete and waiting out the
/// timeout on each one.
static AVAILABLE: AtomicBool = AtomicBool::new(false);

const HP_SYS_CLKRST: usize = 0x500E_6000;
/// Bus clock enables for the peripherals on the AXI interconnect.
const SOC_CLK_CTRL1: usize = HP_SYS_CLKRST + 0x18;
/// Reset holds. Asserting a bit puts the block in reset; clearing releases it.
const HP_RST_EN1: usize = HP_SYS_CLKRST + 0xC4;

const CLK_EN: u32 = 1 << 9;
const RST: u32 = 1 << 0;

const BASE: usize = 0x5008_7000;
const INT_RAW: usize = BASE + 0x10;
const INT_CLR: usize = BASE + 0x1C;
/// Colour modes for each of the blending engine's three ports; only the TX
/// (output) field matters for a fill.
const BLEND_COLOR_MODE: usize = BASE + 0x24;
const BLEND_TRANS_MODE: usize = BASE + 0x34;
/// Block size the blending engine emits, which must agree with the block the
/// descriptor describes.
const BLEND_TX_SIZE: usize = BASE + 0x3C;
/// The fill colour. Zero out of reset and fully writable, so it doubles as the
/// read-back target in `init`.
const BLEND_FIX_PIXEL: usize = BASE + 0x4C;
const DATE: usize = BASE + 0x100;

const BLEND_EN: u32 = 1 << 0;
const BLEND_BYPASS: u32 = 1 << 1;
const BLEND_FIX_PIXEL_FILL_EN: u32 = 1 << 2;
const BLEND_TRANS_MODE_UPDATE: u32 = 1 << 3;
const BLEND_RST: u32 = 1 << 4;
const BLEND_TX_CM_SHIFT: u32 = 8;
const BLEND_TX_CM_RGB565: u32 = 2;

/// Reset value of the version register on this silicon revision.
const DATE_EXPECTED: u32 = 0x0230_4041;

/// Alternates bits within every nibble and between the halves, so a stuck-at
/// bus, a narrower-than-32-bit register and a byte-swapped path each fail to
/// reproduce it.
const READBACK_PATTERN: u32 = 0xA5C3_5A3C;

/// Brings up the 2D-DMA and then the PPA, and proves both register files
/// answer. Returns whether fills can be used at all.
///
/// This must run before anything reads a PPA register: on ECO2 a bus access to
/// a block whose clock is gated never returns, the same trap
/// `lcd::quiesce_dma` checks for.
pub fn init() -> bool {
    let dma2d_ok = dma2d::init();

    unsafe {
        modify(SOC_CLK_CTRL1, CLK_EN, CLK_EN);
        modify(HP_RST_EN1, RST, RST);
        modify(HP_RST_EN1, RST, 0);
    }

    let date = unsafe { read(DATE) };
    uart::log_hex(b"PPA: version=", date);
    if date != DATE_EXPECTED {
        uart::log(b"PPA: version differs from the ECO2 reference value\r\n");
    }

    // A dead bus reads back as 0 (unclocked but completing) or all ones
    // (floating); a version register only proves reads work, so a register
    // that resets to zero is written and read back as well.
    let ppa_ok = date != 0 && date != u32::MAX && unsafe { holds_write(BLEND_FIX_PIXEL) };
    if !ppa_ok {
        uart::log(b"PPA: register file did not respond\r\n");
    }
    if ppa_ok && dma2d_ok {
        uart::log(b"PPA: clocked, out of reset, registers verified\r\n");
    }
    let available = ppa_ok && dma2d_ok;
    AVAILABLE.store(available, Ordering::Relaxed);
    available
}

/// Fills a `width` x `height` block of `color` at `destination`.
///
/// Coordinates are in the picture's own orientation; the caller maps its
/// logical geometry onto that. Returns false without touching the hardware if
/// the geometry does not fit, and false after the fact if the transfer did not
/// complete.
///
/// The caller owns cache coherency in both directions -- see
/// `Framebuffer::ppa_fill_rect`, which is the caller that gets it right.
pub fn fill_rgb565(destination: &Block<'_>, width: usize, height: usize, color: u16) -> bool {
    if !AVAILABLE.load(Ordering::Relaxed) {
        return false;
    }
    let mut descriptor = Descriptor::new(destination, width, height);

    unsafe {
        // Put the blending engine back to a known state; a fill abandoned on
        // timeout would otherwise still be enabled.
        modify(BLEND_TRANS_MODE, BLEND_RST, BLEND_RST);
        modify(BLEND_TRANS_MODE, BLEND_RST, 0);
        write(INT_CLR, u32::MAX);
    }

    // The channel has to be running before the engine starts: the engine
    // pushes pixels at it, it does not pull them.
    dma2d::start_peripheral_rx(&mut descriptor, dma2d::PERI_SEL_PPA_BLEND);

    unsafe {
        write(BLEND_FIX_PIXEL, argb8888(color));
        write(BLEND_TX_SIZE, ((height as u32) << 14) | (width as u32));
        modify(
            BLEND_COLOR_MODE,
            0xF << BLEND_TX_CM_SHIFT,
            BLEND_TX_CM_RGB565 << BLEND_TX_CM_SHIFT,
        );
        modify(
            BLEND_TRANS_MODE,
            BLEND_EN | BLEND_BYPASS | BLEND_FIX_PIXEL_FILL_EN,
            BLEND_EN | BLEND_FIX_PIXEL_FILL_EN,
        );
        // `TRANS_MODE_UPDATE` is what latches the mode bits and launches the
        // transaction.
        modify(
            BLEND_TRANS_MODE,
            BLEND_TRANS_MODE_UPDATE,
            BLEND_TRANS_MODE_UPDATE,
        );
    }

    if dma2d::wait_for_rx() {
        return true;
    }
    uart::log_hex(b"PPA: int_raw=", unsafe { read(INT_RAW) });
    false
}

/// Expands an RGB565 colour into the ARGB8888 word the fill-colour register
/// takes.
///
/// That register is always ARGB8888 -- B[7:0], G[15:8], R[23:16], A[31:24] --
/// whatever the output colour mode is; the engine converts on the way out.
/// Writing the RGB565 value into it directly produces a completely different
/// colour, because the 565 word's red field lands in the register's green
/// byte: 0xF800 (red) comes out as 0x00F800, pure green.
///
/// Each component is expanded by repeating its high bits into the low ones,
/// which is exact in the direction that matters: the engine takes the top 5 or
/// 6 bits back off, recovering the original field.
fn argb8888(color: u16) -> u32 {
    let red = ((color >> 11) & 0x1F) as u32;
    let green = ((color >> 5) & 0x3F) as u32;
    let blue = (color & 0x1F) as u32;
    let red = (red << 3) | (red >> 2);
    let green = (green << 2) | (green >> 4);
    let blue = (blue << 3) | (blue >> 2);
    (0xFF << 24) | (red << 16) | (green << 8) | blue
}

/// Writes a pattern to a register that resets to zero and confirms it sticks,
/// then puts back what was there.
///
/// # Safety
/// `address` must be a fully writable 32-bit register of a clocked, unreset
/// block that the hardware does not act on merely by being written.
unsafe fn holds_write(address: usize) -> bool {
    unsafe {
        let saved = read(address);
        write(address, READBACK_PATTERN);
        let observed = read(address);
        write(address, saved);
        observed == READBACK_PATTERN
    }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn write(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn modify(address: usize, mask: u32, value: u32) {
    unsafe {
        let updated = (read(address) & !mask) | (value & mask);
        write(address, updated);
    }
}
