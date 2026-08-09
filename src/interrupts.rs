//! ESP32-P4 ECO2 CLIC interrupt glue for the display DMA.
//!
//! ECO2 does not expose the DSI Bridge VSYNC event. ESP-IDF therefore treats
//! the full-frame DW-GDMA completion as a synthetic VSYNC and immediately
//! rearms the next framebuffer. This module implements the same policy without
//! an RTOS or PAC.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

const DW_GDMA: usize = 0x5008_1000;
const CHANNEL: usize = DW_GDMA + 0x100;

const INTERRUPT_CORE0: usize = 0x500D_6000;
const DW_GDMA_SOURCE: usize = 24;

const CLIC_CONFIG: usize = 0x2080_0000;
const CLIC_THRESHOLD: usize = 0x2080_0008;
const CLIC_CTRL: usize = 0x2080_1000;

// Peripheral interrupt line 1 maps to CLIC interrupt 17. Interrupt line 6 is
// reserved by ESP-IDF; line 1 is a normal external level interrupt.
const CPU_INTERRUPT_LINE: u32 = 1;
const CLIC_INTERRUPT: u32 = CPU_INTERRUPT_LINE + 16;

const DMA_FULL_DONE: u32 = 1 << 1;
const DMA_ERROR_MASK: u32 = 0x0000_3FE0;

static FRAMEBUFFER_0: AtomicU32 = AtomicU32::new(0);
static FRAMEBUFFER_1: AtomicU32 = AtomicU32::new(0);
static REQUESTED: AtomicUsize = AtomicUsize::new(0);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static FRAME_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static DMA_ERROR: AtomicU32 = AtomicU32::new(0);

// riscv-rt's generic trap entry does not preserve CLIC's extended mcause
// fields. ESP32-P4 requires mcause to be restored before mret because it also
// carries the previous interrupt level. Keep this small entry independent of
// ESP-IDF and save all integer registers that Rust may use.
global_asm!(
    r#"
    .section .trap.start, "ax"
    .balign 64
    .global _start_trap
    .type _start_trap, @function
_start_trap:
    addi sp, sp, -144
    sw ra,   0(sp)
    sw gp,   4(sp)
    sw tp,   8(sp)
    sw t0,  12(sp)
    sw t1,  16(sp)
    sw t2,  20(sp)
    sw s0,  24(sp)
    sw s1,  28(sp)
    sw a0,  32(sp)
    sw a1,  36(sp)
    sw a2,  40(sp)
    sw a3,  44(sp)
    sw a4,  48(sp)
    sw a5,  52(sp)
    sw a6,  56(sp)
    sw a7,  60(sp)
    sw s2,  64(sp)
    sw s3,  68(sp)
    sw s4,  72(sp)
    sw s5,  76(sp)
    sw s6,  80(sp)
    sw s7,  84(sp)
    sw s8,  88(sp)
    sw s9,  92(sp)
    sw s10, 96(sp)
    sw s11,100(sp)
    sw t3, 104(sp)
    sw t4, 108(sp)
    sw t5, 112(sp)
    sw t6, 116(sp)
    csrr t0, mcause
    sw t0, 120(sp)
    csrr t0, mstatus
    sw t0, 124(sp)
    csrr t0, mepc
    sw t0, 128(sp)
    csrr a0, mcause
    call esp32p4_interrupt
    lw t0, 120(sp)
    csrw mcause, t0
    lw t0, 124(sp)
    csrw mstatus, t0
    lw t0, 128(sp)
    csrw mepc, t0
    lw ra,   0(sp)
    lw gp,   4(sp)
    lw tp,   8(sp)
    lw t0,  12(sp)
    lw t1,  16(sp)
    lw t2,  20(sp)
    lw s0,  24(sp)
    lw s1,  28(sp)
    lw a0,  32(sp)
    lw a1,  36(sp)
    lw a2,  40(sp)
    lw a3,  44(sp)
    lw a4,  48(sp)
    lw a5,  52(sp)
    lw a6,  56(sp)
    lw a7,  60(sp)
    lw s2,  64(sp)
    lw s3,  68(sp)
    lw s4,  72(sp)
    lw s5,  76(sp)
    lw s6,  80(sp)
    lw s7,  84(sp)
    lw s8,  88(sp)
    lw s9,  92(sp)
    lw s10, 96(sp)
    lw s11,100(sp)
    lw t3, 104(sp)
    lw t4, 108(sp)
    lw t5, 112(sp)
    lw t6, 116(sp)
    addi sp, sp, 144
    mret
    .size _start_trap, .-_start_trap
"#
);

/// Installs the full-frame DMA interrupt and enables machine interrupts.
///
/// Call only after the GDMA register clock and channel configuration exist.
pub fn install(framebuffer_0: u32, framebuffer_1: u32) {
    FRAMEBUFFER_0.store(framebuffer_0, Ordering::Relaxed);
    FRAMEBUFFER_1.store(framebuffer_1, Ordering::Relaxed);
    REQUESTED.store(0, Ordering::Relaxed);
    ACTIVE.store(0, Ordering::Relaxed);
    FRAME_SEQUENCE.store(0, Ordering::Relaxed);
    DMA_ERROR.store(0, Ordering::Relaxed);

    unsafe extern "C" {
        fn _start_trap();
    }

    unsafe {
        // Generate and propagate full-transfer and error events from channel 0.
        write(CHANNEL + 0x98, u32::MAX);
        write(CHANNEL + 0x80, DMA_FULL_DONE | DMA_ERROR_MASK);
        write(CHANNEL + 0x90, DMA_FULL_DONE | DMA_ERROR_MASK);

        // Route peripheral source 24 to external CPU line 1 (CLIC interrupt 17).
        write(INTERRUPT_CORE0 + DW_GDMA_SOURCE * 4, CLIC_INTERRUPT);

        // Configure three interrupt-level bits, then disable any stale external
        // enables inherited from the bootloader before enabling our level IRQ.
        modify(CLIC_CONFIG, 0xF << 1, 3 << 1);
        for interrupt in 16..48 {
            modify(CLIC_CTRL + interrupt * 4, 1 << 8, 0);
        }
        modify(
            CLIC_CTRL + CLIC_INTERRUPT as usize * 4,
            (0xFF << 24) | (0x3 << 17) | (1 << 16) | (1 << 8),
            (0x3F << 24) | (1 << 8),
        );
        // Priority 0 is masked; priority 1 and above can enter.
        write(CLIC_THRESHOLD, 0x1F00_0000);

        let trap = _start_trap as *const () as usize | 3; // ESP32-P4 CLIC mode
        asm!("csrw mtvec, {trap}", trap = in(reg) trap, options(nostack));
        asm!("fence iorw, iorw", options(nostack));
        asm!("csrsi mstatus, 8", options(nostack));
    }
}

#[allow(dead_code)]
pub fn request_framebuffer(index: usize) {
    if index < 2 {
        REQUESTED.store(index, Ordering::Release);
    }
}

pub fn active_framebuffer() -> usize {
    ACTIVE.load(Ordering::Acquire)
}

pub fn frame_sequence() -> u32 {
    FRAME_SEQUENCE.load(Ordering::Acquire)
}

pub fn dma_error() -> u32 {
    DMA_ERROR.load(Ordering::Acquire)
}

pub fn wait_for_interrupt() {
    unsafe { asm!("wfi", options(nomem, nostack)) };
}

#[unsafe(no_mangle)]
extern "C" fn esp32p4_interrupt(cause: u32) {
    unsafe {
        if cause & 0x8000_0000 == 0 || cause & 0x0FFF != CLIC_INTERRUPT {
            loop {
                core::hint::spin_loop();
            }
        }

        let status = read(CHANNEL + 0x88);
        write(CHANNEL + 0x98, status);
        let errors = status & DMA_ERROR_MASK;
        if errors != 0 {
            DMA_ERROR.store(errors, Ordering::Release);
            return;
        }

        if status & DMA_FULL_DONE != 0 {
            let next = REQUESTED.load(Ordering::Acquire);
            let address = if next == 0 {
                FRAMEBUFFER_0.load(Ordering::Relaxed)
            } else {
                FRAMEBUFFER_1.load(Ordering::Relaxed)
            };
            write(CHANNEL, address);
            write(DW_GDMA + 0x18, 0x101);
            ACTIVE.store(next, Ordering::Release);
            FRAME_SEQUENCE.fetch_add(1, Ordering::Release);
        }
    }
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
