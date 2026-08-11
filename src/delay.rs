//! Busy-wait delays timed off the HP core's cycle counter.
//!
//! `startup::raise_cpu_clock` moves the HP core from the bootloader's 90 MHz
//! boot tap to the full 360 MHz CPLL/1, so a cycle-counter delay is stable
//! enough for peripheral reset/timing requirements.

const CPU_CYCLES_PER_US: u32 = 360;
const CPU_CYCLES_PER_MS: u32 = CPU_CYCLES_PER_US * 1000;

pub fn delay_ms(milliseconds: u32) {
    let start = cycle_count();
    let cycles = milliseconds.saturating_mul(CPU_CYCLES_PER_MS);
    while cycle_count().wrapping_sub(start) < cycles {
        core::hint::spin_loop();
    }
}

pub fn delay_us(microseconds: u32) {
    let start = cycle_count();
    let cycles = microseconds.saturating_mul(CPU_CYCLES_PER_US);
    while cycle_count().wrapping_sub(start) < cycles {
        core::hint::spin_loop();
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
