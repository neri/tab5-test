//! ESP32-P4 system AXI interconnect (`sys_icm`) arbitration.
//!
//! Every master on the interconnect resets to arbitration priority 0 and
//! ARQOS 0, so the DSI scanout DMA competes for PSRAM on equal terms with the
//! CPU and the cache's writeback traffic. Scanning a 720x1280 RGB565 frame out
//! at ~57 Hz already needs ~105 MB/s of the 80 MHz DDR PSRAM's usable
//! bandwidth; a full-framebuffer redraw adds its own line fills and writebacks
//! on top. When the DSI bridge's FIFO loses that race it runs dry and the
//! panel shows the bridge's underrun output (a solid light blue) for the rest
//! of that frame.
//!
//! Raising just the DW-GDMA read ports is the intended remedy: nothing else in
//! this firmware has a hard real-time deadline, so the display always winning
//! arbitration costs the other masters only latency, never correctness.
//!
//! The two knobs are separate and are both applied. `MST_ARB_PRIORITY`
//! arbitrates command channels *inside* the interconnect; `MST_ARQOS` is the
//! AXI QoS value each master presents to the slave it addresses, which is what
//! the PSRAM MSPI's own arbiter sees. `shell`'s `icm` command changes both at
//! runtime, which is how they were shown to be worth setting: over a fixed
//! workload the display's underrun rate drops from 20/20 to 14/20 while CPU
//! throughput is unchanged.
//!
//! The block also has a token-bucket rate regulator per master port, reached
//! through a command interface at `+0x400`. Throttling the cache master with
//! it never had any effect on this hardware, at any rate down to one
//! transaction per 4096 cycles -- see `docs/DISPLAY_BANDWIDTH.md`.

use crate::uart;

const AXI_ICM: usize = 0x500A_4000;
/// Force-on for the interconnect's own register clock. Without it the
/// registers below can read back as written but still be gated.
const CLK_EN: usize = AXI_ICM + 0x04;
/// Command-channel arbitration priority between interconnect masters.
const MST_ARB_PRIORITY0: usize = AXI_ICM + 0x1C;
/// AXI ARQOS each master presents to the slave it addresses (here PSRAM MSPI).
const MST_ARQOS0: usize = AXI_ICM + 0x28;
/// The write-side counterpart of `MST_ARQOS0`. Scanout only reads, so it never
/// mattered until the PPA started writing the framebuffer by DMA.
const MST_AWQOS0: usize = AXI_ICM + 0x30;

/// DW-GDMA's two AXI master ports occupy the same 4-bit field positions in
/// all three registers. The LCD channel sources from master port 1 (`SMS=1` in
/// its `CTL` word), but setting both keeps this correct if that ever changes.
const GDMA_MASTER1_SHIFT: u32 = 12;
const GDMA_MASTER2_SHIFT: u32 = 16;
/// The 2D-DMA master port, which is how PPA fills reach PSRAM.
const DMA2D_SHIFT: u32 = 8;
const FIELD: u32 = 0xF;
const HIGHEST: u32 = 0xF;
const LOWEST: u32 = 0;

/// Registers this module reports, for the `icm` shell command.
pub struct Status {
    pub clock_enable: u32,
    pub master_priority: u32,
    pub master_arqos: u32,
    pub master_awqos: u32,
}

/// Gives the DW-GDMA master ports the highest read priority on the system
/// interconnect, so DSI scanout reads outrank CPU and cache PSRAM traffic.
///
/// Safe to call more than once; the writes are idempotent.
pub fn prioritize_display_reads() {
    set_display_priority(HIGHEST, HIGHEST);
    pin_dma2d_below_display();
    let status = status();
    uart::log_hex(b"ICM: clk_en=", status.clock_enable);
    uart::log_hex(b"ICM: master priority=", status.master_priority);
    uart::log_hex(b"ICM: master arqos=", status.master_arqos);
    uart::log_hex(b"ICM: master awqos=", status.master_awqos);
}

/// Holds the 2D-DMA master port at the bottom of the arbitration order, on
/// both the read and the write side.
///
/// PPA fills contend with scanout for PSRAM as a separate interconnect master,
/// and a fill is never urgent: nothing waits on it but the CPU that started
/// it, whereas the DSI bridge losing its reads shows on the panel. The values
/// written are the power-on ones, so this changes no behaviour -- it states
/// the intent, so that the display keeping its lead is a decision rather than
/// something that happens to be true because nobody set the field.
///
/// The write side matters here in a way it never did for scanout: a fill is
/// 1.8 MiB of writes, and `MST_AWQOS` is the QoS value the PSRAM controller's
/// own arbiter sees for them.
pub fn pin_dma2d_below_display() {
    set_dma2d_priority(LOWEST, LOWEST);
}

/// Sets the 2D-DMA master port's interconnect arbitration priority and its AXI
/// QoS in both directions. All values are 4-bit; anything wider is truncated.
fn set_dma2d_priority(priority: u32, qos: u32) {
    let mask = FIELD << DMA2D_SHIFT;
    let priority = (priority & FIELD) << DMA2D_SHIFT;
    let qos = (qos & FIELD) << DMA2D_SHIFT;
    unsafe {
        modify(CLK_EN, 1, 1);
        modify(MST_ARB_PRIORITY0, mask, priority);
        modify(MST_ARQOS0, mask, qos);
        modify(MST_AWQOS0, mask, qos);
    }
}

/// Returns the DW-GDMA master ports to their power-on arbitration settings.
///
/// These registers survive `startup::reboot`, which resets only the HP CPU
/// core. Leaving the display DMA promoted across a reset would have it outrank
/// both the bootloader's and this firmware's own external-memory access while
/// PSRAM is being reset and re-tuned.
pub fn restore_default_priority() {
    set_display_priority(0, 0);
}

/// Sets the DW-GDMA master ports' interconnect arbitration priority and AXI
/// read QoS. Both values are 4-bit; anything wider is truncated.
pub fn set_display_priority(priority: u32, arqos: u32) {
    let mask = (FIELD << GDMA_MASTER1_SHIFT) | (FIELD << GDMA_MASTER2_SHIFT);
    let priority = priority & FIELD;
    let arqos = arqos & FIELD;
    unsafe {
        // The register clock has to be running before the fields below are
        // meaningful, and this bit resets to 0.
        modify(CLK_EN, 1, 1);
        modify(
            MST_ARB_PRIORITY0,
            mask,
            (priority << GDMA_MASTER1_SHIFT) | (priority << GDMA_MASTER2_SHIFT),
        );
        modify(
            MST_ARQOS0,
            mask,
            (arqos << GDMA_MASTER1_SHIFT) | (arqos << GDMA_MASTER2_SHIFT),
        );
    }
}

pub fn status() -> Status {
    unsafe {
        Status {
            clock_enable: read(CLK_EN),
            master_priority: read(MST_ARB_PRIORITY0),
            master_arqos: read(MST_ARQOS0),
            master_awqos: read(MST_AWQOS0),
        }
    }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn modify(address: usize, mask: u32, value: u32) {
    unsafe {
        let updated = (read(address) & !mask) | (value & mask);
        (address as *mut u32).write_volatile(updated);
    }
}
