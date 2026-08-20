//! ESP32-P4 2D-DMA.
//!
//! This engine moves rectangular blocks rather than flat buffers. A descriptor
//! names a picture -- a base address plus its width and height in pixels --
//! and a block inside it, and the hardware derives the stride from the
//! picture's width. That is what makes it the right tool for a rotated
//! framebuffer: a landscape rectangle is still a rectangle in the portrait
//! scan layout, only transposed, so no address arithmetic per row is needed on
//! either side.
//!
//! It is a shared peripheral -- JPEG, PPA and H.264 all feed through it, which
//! is why ESP-IDF keeps its channel allocator private -- but this firmware is
//! its only user, so channel 0 is simply taken. Two directions exist per
//! channel: TX reads memory, RX writes it. A PPA fill has no source image and
//! uses RX alone; a memory-to-memory copy pairs TX and RX on the same channel
//! index.
//!
//! Checked against ESP-IDF v5.5.3: `hal/esp32p4/dma2d_ll.h`,
//! `esp_hw_support/dma/dma2d.c` for the per-channel defaults, and
//! `esp_lcd/src/esp_async_fbcpy.c`, which is the only in-tree user of the
//! memory-to-memory mode. No ESP-IDF code is linked in.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::psram;
use crate::uart;

/// Whether `init` found the block answering.
static AVAILABLE: AtomicBool = AtomicBool::new(false);

const HP_SYS_CLKRST: usize = 0x500E_6000;
const SOC_CLK_CTRL1: usize = HP_SYS_CLKRST + 0x18;
const HP_RST_EN0: usize = HP_SYS_CLKRST + 0xC0;
const CLK_EN: u32 = 1 << 6;
const RST: u32 = 1 << 31;

const BASE: usize = 0x5008_8000;
const DATE: usize = BASE + 0xA2C;
/// Reset value of the version register on this silicon revision.
const DATE_EXPECTED: u32 = 0x0230_4110;

/// Channel 0's two register blocks. Channels are 0x100 apart; nothing else
/// here contends for one, so the channel is fixed rather than allocated.
const TX_CHANNEL: usize = BASE;
const RX_CHANNEL: usize = BASE + 0x500;

// Offsets within a channel's block. The two directions mirror each other
// except where noted.
const CONF0: usize = 0x00;
const INT_RAW: usize = 0x04;
const INT_CLR: usize = 0x10;
const LINK_CONF: usize = 0x1C;
const LINK_ADDR: usize = 0x20;
const TX_PERI_SEL: usize = 0x38;
const RX_PERI_SEL: usize = 0x3C;
const TX_COLOR_CONVERT: usize = 0x48;
const RX_COLOR_CONVERT: usize = 0x4C;

/// Peripheral a channel takes its data from, or delivers it to: 0 JPEG,
/// 1 PPA scale/rotate/mirror, 2 PPA blend, 7 none. Memory-to-memory has no
/// peripheral, so it borrows one of the unclaimed ids -- any of 3..7 for RX
/// and 4..7 for TX, which need not match, since what actually pairs the two
/// halves is `IN_MEM_TRANS_EN` and their sharing a channel index.
pub const PERI_SEL_PPA_BLEND: u32 = 2;
const PERI_SEL_M2M_RX: u32 = 3;
const PERI_SEL_M2M_TX: u32 = 4;

/// Production uses the largest offered burst. A display diagnostic can
/// temporarily lower this field to learn whether shorter DMA2D ownership
/// intervals improve the scanout DMA's worst-case read latency; it restores
/// 128 bytes before returning.
static BURST_CODE: AtomicU32 = AtomicU32::new(4);

/// `CONF0` fields independent of burst size: writes stay inside the selected
/// burst's address boundary (bit 12), and macro-block reordering is disabled
/// (bits 10:9 = 3 = none). Owner checking (bit 4), descriptor-from-peripheral
/// (bit 11) and reorder (bit 16) stay off.
const CONF0_FIXED: u32 = (3 << 9) | (1 << 12);
/// RX bit 0 turns on memory-to-memory, which is what makes the channel take
/// its data from its sibling TX rather than from a peripheral.
const IN_MEM_TRANS_EN: u32 = 1 << 0;
/// RX bit 2 lets descriptor fetches burst.
const INDSCR_BURST_EN: u32 = 1 << 2;
/// TX bit 1: raise EOF once the data has been popped from the FIFO. This is
/// the reset value and what the driver re-establishes.
const OUT_EOF_MODE: u32 = 1 << 1;
/// Channel FSM/FIFO reset, held with the command-disable bit as the register
/// description requires so a reset cannot land while AXI commands from an
/// earlier transfer are still outstanding.
const CHANNEL_RST: u32 = 1 << 24;
const CMD_DISABLE: u32 = 1 << 25;

/// Colour conversion off: input stage disabled (bits 5:3 = 7), no generic
/// 3-byte conversion (bit 2), output passed through (bits 1:0 = 1). Only the
/// input field resets to the value wanted, so the register is written.
const COLOR_CONVERT_NONE: u32 = (7 << 3) | 1;

/// `LINK_CONF` bits, which differ between the directions: RX puts its
/// auto-return-owner control at bit 20 and pushes stop/start up by one.
/// Start and stop are self-clearing, so this register is always written whole
/// -- a read-modify-write could re-trigger one of them. Leaving RX's
/// `INLINK_AUTO_RET` clear makes the hardware hand the descriptor's owner
/// field back to the CPU when it is done.
const INLINK_STOP: u32 = 1 << 21;
const INLINK_START: u32 = 1 << 22;
const OUTLINK_STOP: u32 = 1 << 20;
const OUTLINK_START: u32 = 1 << 21;

/// Interrupt bits. `SUC_EOF` is raised once the descriptor carrying `suc_eof`
/// has been fully processed; on the RX side that means every pixel has reached
/// the memory interface.
const SUC_EOF_INT: u32 = 1 << 1;
const RX_DESCR_ERR_INT: u32 = 1 << 3;

/// Two bytes per pixel, in the descriptor's encoding (0 is half a byte,
/// 1 one byte, 2 one and a half, 3 two).
const PBYTE_RGB565: u32 = 3;

/// `hb_length`, `vb_size`, `x` and `y` are 14 bits each.
const FIELD_MAX: usize = 0x3FFF;

/// Ceiling on the completion poll. A full-screen transfer takes tens of
/// milliseconds; at 360 MHz this is far beyond that, so reaching it means the
/// transfer never started rather than that it is slow.
const POLL_LIMIT: u32 = 50_000_000;

/// Alternates bits within every nibble and between the halves, so a stuck-at
/// bus, a narrower-than-32-bit register and a byte-swapped path each fail to
/// reproduce it.
const READBACK_PATTERN: u32 = 0xA5C3_5A3C;

/// An RGB565 image in memory, in its own orientation.
pub struct Picture {
    /// Base of the whole image. The descriptor addresses blocks relative to
    /// this, never to the block's own first pixel.
    pub buffer: usize,
    /// Pixels per row.
    pub width: usize,
    /// Rows.
    pub height: usize,
}

impl Picture {
    fn valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= FIELD_MAX
            && self.height <= FIELD_MAX
            // The AXI path moves whole words, so an odd pixel address would
            // have it write across the neighbouring pixel.
            && self.buffer.is_multiple_of(4)
    }
}

/// Where a block sits within a picture. Its size is passed alongside, because
/// a copy's two ends always share one.
pub struct Block<'a> {
    pub picture: &'a Picture,
    pub x: usize,
    pub y: usize,
}

impl Block<'_> {
    fn holds(&self, width: usize, height: usize) -> bool {
        self.picture.valid()
            && width > 0
            && height > 0
            && width <= self.picture.width.saturating_sub(self.x)
            && height <= self.picture.height.saturating_sub(self.y)
    }
}

/// A 2D-DMA descriptor: three packed bit-field words then two pointers, on an
/// 8-byte boundary.
///
/// Built as explicit `u32`s because the fields cross byte boundaries at 14
/// bits, where a bit-field struct would only be restating the same shifts less
/// directly.
#[repr(C, align(8))]
pub struct Descriptor {
    /// `vb_size` [13:0], `hb_length` [27:14], `err_eof` [28], `dma2d_en` [29],
    /// `suc_eof` [30], `owner` [31] -- the block's own size, plus the flags.
    size_and_flags: u32,
    /// `va_size` [13:0], `ha_length` [27:14], `pbyte` [31:28] -- the picture
    /// the block sits in, which is what turns its width into a stride.
    picture: u32,
    /// `y` [13:0], `x` [27:14], `mode` [28] -- where the block sits.
    position: u32,
    /// Base of the whole picture, not of the block.
    buffer: u32,
    /// Next descriptor, or null for the last one.
    next: u32,
    /// The hardware reads 24 bytes per descriptor.
    _padding: u32,
}

/// The `owner` bit has to say DMA before a transfer starts; the hardware
/// clears it once it is finished with the descriptor.
const OWNER_DMA: u32 = 1 << 31;
const SUC_EOF: u32 = 1 << 30;
const DMA2D_EN: u32 = 1 << 29;

impl Descriptor {
    pub fn new(block: &Block<'_>, width: usize, height: usize) -> Self {
        Self {
            size_and_flags: OWNER_DMA
                | SUC_EOF
                | DMA2D_EN
                | ((width as u32) << 14)
                | (height as u32),
            picture: (PBYTE_RGB565 << 28)
                | ((block.picture.width as u32) << 14)
                | (block.picture.height as u32),
            position: ((block.x as u32) << 14) | (block.y as u32),
            buffer: block.picture.buffer as u32,
            next: 0,
            _padding: 0,
        }
    }

    /// Publishes the descriptor to memory and returns the address to hand the
    /// hardware. The write-back also invalidates, which is harmless for the
    /// surrounding stack and is what `sdmmc` does with its IDMAC descriptor.
    fn publish(&mut self) -> u32 {
        let address = &raw mut *self as usize;
        psram::writeback_invalidate(address, size_of::<Self>());
        address as u32
    }
}

/// Clocks the block, releases it from reset, and proves its register file
/// answers. Returns whether it can be used at all.
///
/// Must run before anything reads a 2D-DMA register: on ECO2 a bus access to a
/// block whose clock is gated never returns, the same trap `lcd::quiesce_dma`
/// checks for. The reset pulse is also what makes this safe to call after a
/// CPU-only reboot, which leaves peripherals running -- it stops any transfer
/// the previous boot had in flight before `psram::init` resets the memory
/// controller underneath it.
pub fn init() -> bool {
    unsafe {
        modify(SOC_CLK_CTRL1, CLK_EN, CLK_EN);
        modify(HP_RST_EN0, RST, RST);
        modify(HP_RST_EN0, RST, 0);
    }

    let date = unsafe { read(DATE) };
    uart::log_hex(b"DMA2D: version=", date);
    if date != DATE_EXPECTED {
        uart::log(b"DMA2D: version differs from the ECO2 reference value\r\n");
    }

    // A dead bus reads back as 0 (unclocked but completing) or all ones
    // (floating); a version register only proves reads work, so a register
    // that resets to zero is written and read back as well.
    let available = date != 0 && date != u32::MAX && unsafe { holds_write(RX_CHANNEL + LINK_ADDR) };
    if !available {
        uart::log(b"DMA2D: register file did not respond\r\n");
    }
    AVAILABLE.store(available, Ordering::Relaxed);
    available
}

pub fn is_available() -> bool {
    AVAILABLE.load(Ordering::Relaxed)
}

/// Returns the DMA2D data burst selected for the next transfer.
pub(crate) fn diagnostic_burst_bytes() -> u32 {
    8 << BURST_CODE.load(Ordering::Relaxed)
}

/// Selects a DMA2D data burst for a bounded diagnostic run.
///
/// This does not touch an active channel; the value is consumed when the next
/// transfer configures RX/TX. Callers must restore the returned previous value
/// before resuming normal rendering.
pub(crate) fn diagnostic_set_burst_bytes(bytes: u32) -> Option<u32> {
    let code = match bytes {
        8 => 0,
        16 => 1,
        32 => 2,
        64 => 3,
        128 => 4,
        _ => return None,
    };
    let previous = BURST_CODE.swap(code, Ordering::Relaxed);
    Some(8 << previous)
}

/// Arms RX channel 0 to receive into `descriptor` from `peripheral`, and
/// starts it.
///
/// The channel has to be running before the peripheral is told to produce:
/// the peripheral pushes pixels at it, it does not pull them. Callers own the
/// cache on both sides of the transfer and must wait with `wait_for_rx`.
pub fn start_peripheral_rx(descriptor: &mut Descriptor, peripheral: u32) {
    let address = descriptor.publish();
    unsafe {
        configure_rx(peripheral, false);
        write(RX_CHANNEL + INT_CLR, u32::MAX);
        write(RX_CHANNEL + LINK_ADDR, address);
        write(RX_CHANNEL + LINK_CONF, INLINK_START);
    }
}

/// Copies a `width` x `height` block of RGB565 pixels from `source` to
/// `destination`, which may be regions of the same picture.
///
/// **Overlap has a safe direction and an unsafe one.** The engine walks both
/// blocks in the same order -- along a row, then down the rows -- and RX
/// necessarily trails TX, since it is consuming what TX produced. That makes
/// the copy safe when the destination sits at a *lower* address than the
/// source, which is the case for scrolling text upwards: each byte is read
/// before the write that lands on it. Copying the other way round would
/// overwrite source pixels the engine has not reached yet, and this function
/// does not detect that.
pub fn copy_rgb565(
    source: &Block<'_>,
    destination: &Block<'_>,
    width: usize,
    height: usize,
) -> bool {
    if !is_available() || !source.holds(width, height) || !destination.holds(width, height) {
        return false;
    }

    // Both live until the function returns, which is after the hardware has
    // finished with them.
    let mut source_descriptor = Descriptor::new(source, width, height);
    let mut destination_descriptor = Descriptor::new(destination, width, height);
    let source_address = source_descriptor.publish();
    let destination_address = destination_descriptor.publish();

    unsafe {
        configure_tx(PERI_SEL_M2M_TX);
        configure_rx(PERI_SEL_M2M_RX, true);
        write(TX_CHANNEL + INT_CLR, u32::MAX);
        write(RX_CHANNEL + INT_CLR, u32::MAX);
        write(TX_CHANNEL + LINK_ADDR, source_address);
        write(RX_CHANNEL + LINK_ADDR, destination_address);
        write(TX_CHANNEL + LINK_CONF, OUTLINK_START);
        write(RX_CHANNEL + LINK_CONF, INLINK_START);
    }

    wait_for_rx()
}

/// Spins until RX channel 0 reports its descriptor finished.
///
/// Polling rather than an interrupt: every caller has to wait anyway, because
/// whatever draws next over the same region would otherwise race the transfer
/// and lose.
pub fn wait_for_rx() -> bool {
    let mut remaining = POLL_LIMIT;
    loop {
        let status = unsafe { read(RX_CHANNEL + INT_RAW) };
        if status & SUC_EOF_INT != 0 {
            unsafe { write(RX_CHANNEL + INT_CLR, u32::MAX) };
            return true;
        }
        if status & RX_DESCR_ERR_INT != 0 {
            uart::log(b"DMA2D: descriptor rejected\r\n");
            unsafe { write(RX_CHANNEL + INT_CLR, u32::MAX) };
            return false;
        }
        if remaining == 0 {
            uart::log_hex(b"DMA2D: transfer timed out, in_int_raw=", status);
            uart::log_hex(b"DMA2D: out_int_raw=", unsafe {
                read(TX_CHANNEL + INT_RAW)
            });
            return false;
        }
        remaining -= 1;
        core::hint::spin_loop();
    }
}

/// # Safety
/// The block must be clocked and out of reset.
unsafe fn configure_rx(peripheral: u32, memory_to_memory: bool) {
    let conf0 =
        common_conf0() | INDSCR_BURST_EN | if memory_to_memory { IN_MEM_TRANS_EN } else { 0 };
    unsafe {
        write(RX_CHANNEL + LINK_CONF, INLINK_STOP);
        reset_channel(RX_CHANNEL, conf0);
        write(RX_CHANNEL + RX_PERI_SEL, peripheral);
        write(RX_CHANNEL + RX_COLOR_CONVERT, COLOR_CONVERT_NONE);
    }
}

/// # Safety
/// The block must be clocked and out of reset.
unsafe fn configure_tx(peripheral: u32) {
    let conf0 = common_conf0() | OUT_EOF_MODE;
    unsafe {
        write(TX_CHANNEL + LINK_CONF, OUTLINK_STOP);
        reset_channel(TX_CHANNEL, conf0);
        write(TX_CHANNEL + TX_PERI_SEL, peripheral);
        write(TX_CHANNEL + TX_COLOR_CONVERT, COLOR_CONVERT_NONE);
    }
}

fn common_conf0() -> u32 {
    CONF0_FIXED | (BURST_CODE.load(Ordering::Relaxed) << 6)
}

/// Pulses a channel's reset with command-disable held, then leaves `conf0` in
/// place. `CONF0` is written whole rather than read-modified so that no stale
/// field can survive from an earlier, differently-configured transfer.
///
/// # Safety
/// `channel` must be a 2D-DMA channel register block of a clocked, unreset
/// peripheral.
unsafe fn reset_channel(channel: usize, conf0: u32) {
    unsafe {
        write(channel + CONF0, conf0 | CMD_DISABLE);
        write(channel + CONF0, conf0 | CMD_DISABLE | CHANNEL_RST);
        write(channel + CONF0, conf0 | CMD_DISABLE);
        write(channel + CONF0, conf0);
    }
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
