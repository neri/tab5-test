//! A monotonic millisecond clock built on SYSTIMER's periodic comparator.
//!
//! This is the firmware's general-purpose time source, not a networking
//! detail: `uptime` reads it, and smoltcp's retransmit and DHCP timers are
//! anchored to it.
//!
//! `delay.rs` stays as it is. Its `rdcycle` reads only the low 32 bits of
//! the cycle counter, which wraps every ~11.9 s at 360 MHz -- fine for the
//! short busy waits peripheral bring-up needs, useless for measuring
//! seconds and actively harmful as a protocol timer, because a stack that
//! sees time run backwards mis-schedules every retransmission.
//!
//! SYSTIMER on ESP32-P4 is two 52-bit counters and three comparators. Unit
//! 0 free-runs off XTAL (40 MHz) through the chip's fixed 2.5 divider, so
//! it counts at 16 MHz; comparator 0 is put in period mode at 16,000 ticks
//! and raises a level interrupt at 1 kHz. The ISR does nothing but bump a
//! counter and clear the interrupt, which is what keeps it clear of the
//! display's scan-out deadline.
//!
//! Registers follow ESP-IDF v5.5.3's
//! `components/soc/esp32p4/register/hw_ver1/soc/systimer_reg.h` and the
//! sequence in `components/hal/esp32p4/include/hal/systimer_ll.h`.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const SYSTIMER: usize = 0x500E_2000;
/// `SYSTIMER_CONF_REG` is the block's first register.
const CONF: usize = SYSTIMER;
const TARGET0_CONF: usize = SYSTIMER + 0x34;
const COMP0_LOAD: usize = SYSTIMER + 0x50;
const INT_ENA: usize = SYSTIMER + 0x64;
const INT_RAW: usize = SYSTIMER + 0x68;
const INT_CLR: usize = SYSTIMER + 0x6C;

/// `SYSTIMER_CONF_REG` bits.
const TARGET0_WORK_EN: u32 = 1 << 24;
const TIMER_UNIT0_WORK_EN: u32 = 1 << 30;
/// Register-file clock gating; the peripheral does not answer without it.
const REGISTER_CLK_EN: u32 = 1 << 31;

/// `SYSTIMER_TARGET0_CONF_REG` bits. The period field is 26 bits, which at
/// 16 MHz is a little over four seconds -- room to spare for 1 ms.
const TARGET0_PERIOD_MASK: u32 = 0x03FF_FFFF;
const TARGET0_PERIOD_MODE: u32 = 1 << 30;
const TARGET0_TIMER_UNIT_SEL: u32 = 1 << 31;

/// `SYSTIMER_COMP0_LOAD_REG`: applies the comparator's new period.
const TIMER_COMP0_LOAD: u32 = 1 << 0;

/// Comparator 0's bit in the interrupt registers.
const TARGET0_INT: u32 = 1 << 0;

/// The bus clock and clock source live in `HP_SYS_CLKRST`, not in SYSTIMER
/// itself -- the same split as PSRAM and SDMMC, and the same way to end up
/// with registers that accept writes but a peripheral that never counts.
const HP_SYS_CLKRST: usize = 0x500E_6000;
const SOC_CLK_CTRL2: usize = HP_SYS_CLKRST + 0x1C;
const PERI_CLK_CTRL21: usize = HP_SYS_CLKRST + 0x98;

const SYSTIMER_APB_CLK_EN: u32 = 1 << 23;
/// 0 selects XTAL, 1 selects RC_FAST. XTAL is the one with a known rate.
const SYSTIMER_CLK_SRC_SEL: u32 = 1 << 29;
const SYSTIMER_CLK_EN: u32 = 1 << 30;

/// XTAL 40 MHz through the fixed 2.5 divider (`SOC_SYSTIMER_FIXED_DIVIDER`).
const TICKS_PER_SECOND: u32 = 16_000_000;
const TICKS_PER_MILLISECOND: u32 = TICKS_PER_SECOND / 1000;

/// Milliseconds since [`init`], split in two because RV32 has no 64-bit
/// atomic. The ISR is the only writer, so the halves only ever need to be
/// read consistently, never updated concurrently.
static MILLISECONDS_LOW: AtomicU32 = AtomicU32::new(0);
static MILLISECONDS_HIGH: AtomicU32 = AtomicU32::new(0);
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Starts the 1 kHz tick. Must be called after `interrupts::install` has
/// put the trap entry in `mtvec` and enabled machine interrupts.
pub fn init() {
    if RUNNING.load(Ordering::Acquire) {
        return;
    }

    MILLISECONDS_LOW.store(0, Ordering::Relaxed);
    MILLISECONDS_HIGH.store(0, Ordering::Relaxed);

    unsafe {
        // Bus clock, clock source and the peripheral's own register clock.
        // All three default to the values wanted here, but the cost of
        // saying so is one write each and it removes a dependency on what
        // the bootloader left behind.
        modify(SOC_CLK_CTRL2, SYSTIMER_APB_CLK_EN, SYSTIMER_APB_CLK_EN);
        modify(
            PERI_CLK_CTRL21,
            SYSTIMER_CLK_SRC_SEL | SYSTIMER_CLK_EN,
            SYSTIMER_CLK_EN,
        );
        modify(
            CONF,
            REGISTER_CLK_EN | TIMER_UNIT0_WORK_EN,
            REGISTER_CLK_EN | TIMER_UNIT0_WORK_EN,
        );

        // Comparator 0 against unit 0, period mode, one millisecond.
        // Disable, configure, apply, re-enable -- the order ESP-IDF's
        // `systimer_hal_set_alarm_period` uses, and the reason the period
        // only reaches the comparator through `COMP0_LOAD`.
        modify(CONF, TARGET0_WORK_EN, 0);
        write(
            TARGET0_CONF,
            (TARGET0_PERIOD_MODE | (TICKS_PER_MILLISECOND & TARGET0_PERIOD_MASK))
                & !TARGET0_TIMER_UNIT_SEL,
        );
        write(COMP0_LOAD, TIMER_COMP0_LOAD);
        modify(CONF, TARGET0_WORK_EN, TARGET0_WORK_EN);

        // Start from a clean slate: this is a level interrupt, so a stale
        // raw bit would fire the moment the CLIC line is enabled.
        write(INT_CLR, TARGET0_INT);
        write(INT_ENA, TARGET0_INT);
    }

    crate::interrupts::install_tick();
    RUNNING.store(true, Ordering::Release);
}

/// Milliseconds since [`init`], or 0 before it. Monotonic: the only writer
/// is the ISR, and it only counts up.
pub fn now_ms() -> u64 {
    // The halves cannot be read atomically together, so read the high half
    // either side of the low one. The ISR bumps the high half *before*
    // storing the wrapped low half, so an unchanged high half means the
    // pair belongs to the same instant.
    loop {
        let high = MILLISECONDS_HIGH.load(Ordering::Acquire);
        let low = MILLISECONDS_LOW.load(Ordering::Acquire);
        if high == MILLISECONDS_HIGH.load(Ordering::Acquire) {
            return ((high as u64) << 32) | low as u64;
        }
    }
}

/// Whether the tick is running. Callers that need real time -- rather than
/// a busy wait -- have nothing to fall back on if it is not.
pub fn is_running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

/// Comparator 0's raw interrupt bit, for the diagnostic that asks whether
/// ticks are being missed rather than merely delayed.
pub fn interrupt_pending() -> bool {
    unsafe { read(INT_RAW) & TARGET0_INT != 0 }
}

/// Called from the CLIC dispatcher. Level interrupt: clearing the source is
/// what stops it from re-entering immediately after `mret`.
#[inline]
pub(crate) fn handle_interrupt() {
    unsafe { write(INT_CLR, TARGET0_INT) };

    let low = MILLISECONDS_LOW.load(Ordering::Relaxed).wrapping_add(1);
    // Carry first: a reader that sees the new high half with the old low
    // half retries, while the reverse order would hand out a time 2^32 ms
    // in the past.
    if low == 0 {
        MILLISECONDS_HIGH.fetch_add(1, Ordering::Release);
    }
    MILLISECONDS_LOW.store(low, Ordering::Release);
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
