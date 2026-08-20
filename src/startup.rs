//! ESP32-P4 ECO2 startup operations which must happen before application code.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::uart;

const HP_SYS_CLKRST: usize = 0x500E_6000;
const ROOT_CLK_CTRL0: usize = HP_SYS_CLKRST + 0x04;
const ROOT_CLK_CTRL1: usize = HP_SYS_CLKRST + 0x08;
const ROOT_CLK_CTRL2: usize = HP_SYS_CLKRST + 0x0C;
const LP_CLKRST: usize = 0x5011_1000;
const HP_CLK_CTRL: usize = LP_CLKRST + 0x40;

// LP_SYS STORE13/14 survive the HP-core-only reset used by `reboot()`.  They
// therefore carry the bounded reboot diagnostic across boots without putting
// state in PSRAM, whose controller is deliberately reset on every boot.
const LP_STORE13: usize = 0x5011_0060;
const LP_STORE14: usize = 0x5011_0064;
const REBOOT_TEST_MAGIC: u32 = 0x5254_5354;
const REBOOT_TEST_MAX: u32 = 100;

/// Clock the CPU is actually running at. The bootloader hands over at 90 MHz
/// and `raise_cpu_clock` moves to 360 MHz only if it recognises that state, so
/// anything converting cycles to time has to ask rather than assume.
static CPU_HZ: AtomicU32 = AtomicU32::new(90_000_000);

/// Current CPU clock in hertz.
pub fn cpu_hz() -> u32 {
    CPU_HZ.load(Ordering::Relaxed)
}

/// Result of consuming one successful application boot in the reboot test.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum RebootTestBoot {
    Inactive,
    Reboot { completed: u32, remaining: u32 },
    Complete { total: u32 },
    Failed { completed: u32, total: u32 },
}

/// Arms a bounded reboot test. The caller performs the first reboot.
pub fn request_reboot_test(count: u32) -> bool {
    if count == 0 || count > REBOOT_TEST_MAX {
        return false;
    }
    unsafe {
        // Write the payload first so a reset between these writes cannot make
        // a stale count look armed.
        write(LP_STORE14, (count << 16) | count);
        write(LP_STORE13, REBOOT_TEST_MAGIC);
    }
    true
}

/// Counts a boot only after PSRAM, the post-PSRAM XIP probe, heap setup and
/// display scanout have all succeeded. Intermediate successes request another
/// reset; the final success clears both scratch registers.
pub fn complete_reboot_test_boot(psram_200mhz: bool) -> RebootTestBoot {
    unsafe {
        if read(LP_STORE13) != REBOOT_TEST_MAGIC {
            return RebootTestBoot::Inactive;
        }

        let state = read(LP_STORE14);
        let total = state >> 16;
        let remaining = state & 0xFFFF;
        if total == 0 || total > REBOOT_TEST_MAX || remaining == 0 || remaining > total {
            write(LP_STORE13, 0);
            write(LP_STORE14, 0);
            return RebootTestBoot::Inactive;
        }

        let completed_before_this_boot = total - remaining;
        if !psram_200mhz {
            write(LP_STORE13, 0);
            write(LP_STORE14, 0);
            return RebootTestBoot::Failed {
                completed: completed_before_this_boot,
                total,
            };
        }

        let completed = completed_before_this_boot + 1;
        if remaining == 1 {
            write(LP_STORE13, 0);
            write(LP_STORE14, 0);
            RebootTestBoot::Complete { total }
        } else {
            let next_remaining = remaining - 1;
            write(LP_STORE14, (total << 16) | next_remaining);
            RebootTestBoot::Reboot {
                completed,
                remaining: next_remaining,
            }
        }
    }
}

/// Stops the RTC watchdog inherited from the ESP-IDF bootloader.
pub fn init() {
    const LP_WDT_BASE: usize = 0x5011_6000;
    const CONFIG0: *mut u32 = LP_WDT_BASE as *mut u32;
    const WPROTECT: *mut u32 = (LP_WDT_BASE + 0x18) as *mut u32;
    const WKEY: u32 = 0x50D8_3AA1;
    const WDT_EN: u32 = 1 << 31;

    // ESP-IDF v5.4: wdt_hal_write_protect_disable(); wdt_hal_disable();
    // wdt_hal_write_protect_enable().
    unsafe {
        WPROTECT.write_volatile(WKEY);
        CONFIG0.write_volatile(CONFIG0.read_volatile() & !WDT_EN);
        WPROTECT.write_volatile(0);
    }
}

/// Forces a full chip reset by triggering HP CPU core 0's own software-reset
/// bit -- `LP_CLKRST_HPCPU_RESET_CTRL0_REG`, bit 13 (`HPCORE0_SW_RESET`,
/// write-1-to-trigger, self-clearing). This is not a guess: it is exactly
/// `cpu_utility_ll_reset_cpu(0)`, the primitive ESP-IDF's own
/// `esp_restart_noos` uses as its actual reset trigger on ESP32-P4, read
/// from the real ESP32-P4 SoC register headers
/// (`components/soc/esp32p4/register/soc/lp_clkrst_reg.h` in an ESP-IDF
/// checkout), not inferred from another chip family.
///
/// An earlier version of this function instead armed the LP watchdog
/// (`0x5011_6000`) and waited for it to fire, which hung on real hardware
/// instead of resetting. Two things were wrong with it: the direct
/// `CONFIG0` overwrite zeroed `WDT_SYS_RESET_LENGTH` (the reset pulse
/// width) from its non-zero default down to the shortest setting, and more
/// fundamentally, ESP-IDF itself only arms that same watchdog as a
/// multi-second safety net *behind* this CPU-reset call -- never as the
/// primary mechanism.
pub fn reboot() -> ! {
    const HPCPU_RESET_CTRL0: *mut u32 = (LP_CLKRST + 0x14) as *mut u32;
    const HPCORE0_SW_RESET: u32 = 1 << 13;

    unsafe {
        let value = HPCPU_RESET_CTRL0.read_volatile();
        HPCPU_RESET_CTRL0.write_volatile(value | HPCORE0_SW_RESET);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Raises the CPU clock from the bootloader's 90 MHz tap of CPLL (div 4) to
/// the full 360 MHz (div 1).
///
/// ESP-IDF's own 2nd-stage bootloader already enables and calibrates CPLL to
/// 360 MHz to derive its 90 MHz boot clock on ECO2 (`CPU_CLK_FREQ_MHZ_BTLD`
/// combined with the `CONFIG_ESP32P4_SELECTS_REV_LESS_V3` branch of
/// `rtc_clk_cpu_freq_mhz_to_config`), so only the CPU/MEM/APB dividers need
/// to change here -- no PLL power-up or regi2c calibration is required. An
/// ESP-IDF application's own startup performs the same divider change before
/// running any application code; this bare-metal firmware had no such step,
/// so it previously ran its entire boot sequence -- and every
/// `delay_ms`/`delay_us` call -- at 90 MHz, about 4x slower than intended.
///
/// Order matters: MEM_CLK must stay at or below 200 MHz and APB_CLK at or
/// below 100 MHz at every intermediate step, so dividers are widened from
/// APB up to CPU before the CPU divider itself is narrowed (mirrors
/// `rtc_clk_cpu_freq_to_cpll_mhz`'s upscale path).
///
/// Returns `false` without changing anything if the CPU is not already
/// running from CPLL, since that means the bootloader did not take the
/// expected 90 MHz path and the current dividers are unknown.
pub fn raise_cpu_clock() -> bool {
    const HP_ROOT_CLK_SRC_SEL: u32 = 0x3;
    const SRC_CPLL: u32 = 1;

    unsafe {
        if read(HP_CLK_CTRL) & HP_ROOT_CLK_SRC_SEL != SRC_CPLL {
            return false;
        }

        const CPU_CLK_DIV_NUM: u32 = 0xFF << 5;
        const SYS_CLK_DIV_NUM: u32 = 0xFF << 24;
        const MEM_CLK_DIV_NUM: u32 = 0xFF;
        const APB_CLK_DIV_NUM: u32 = 0xFF << 16;

        modify(ROOT_CLK_CTRL2, APB_CLK_DIV_NUM, 1 << 16); // APB div 2 (was 1)
        latch_dividers();
        modify(ROOT_CLK_CTRL1, SYS_CLK_DIV_NUM, 0 << 24); // SYS div 1 (unchanged)
        latch_dividers();
        modify(ROOT_CLK_CTRL1, MEM_CLK_DIV_NUM, 1); // MEM div 2 (was 1)
        latch_dividers();
        modify(ROOT_CLK_CTRL0, CPU_CLK_DIV_NUM, 0 << 5); // CPU div 1 (was 4)
        latch_dividers();
    }

    CPU_HZ.store(360_000_000, Ordering::Relaxed);
    true
}

fn latch_dividers() {
    const SOC_CLK_DIV_UPDATE: u32 = 1 << 4;
    unsafe {
        modify(ROOT_CLK_CTRL0, SOC_CLK_DIV_UPDATE, SOC_CLK_DIV_UPDATE);
        while read(ROOT_CLK_CTRL0) & SOC_CLK_DIV_UPDATE != 0 {}
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
        write(address, (read(address) & !mask) | (value & mask));
    }
}

/// Reports where usable L2 RAM actually ends on this device.
///
/// ECO2 takes the L2 cache out of the top of L2MEM, and the 2nd-stage
/// bootloader picks the size, so the limit is not a link-time constant. The
/// `RAM` region in `memory.x` is sized for the smallest supported window; this
/// prints the real one so it can be checked, and shouts if the stack ever ends
/// up inside the cache area.
pub fn log_ram_limit() {
    const CACHESIZE_CONF: *const u32 = 0x3FF1_0278 as *const u32;
    const L2MEM_END: u32 = 0x4FFC_0000;

    unsafe extern "C" {
        static _stack_start: u8;
    }

    // CACHE_L2_CACHE_CACHESIZE_CONF_REG is one-hot; only these three sizes are
    // selectable on ESP32-P4.
    let conf = unsafe { CACHESIZE_CONF.read_volatile() };
    let cache_bytes = if conf & (1 << 9) != 0 {
        128 * 1024
    } else if conf & (1 << 10) != 0 {
        256 * 1024
    } else if conf & (1 << 11) != 0 {
        512 * 1024
    } else {
        0
    };
    let stack_top = &raw const _stack_start as u32;

    uart::log_hex(b"RAM: L2 cache bytes=", cache_bytes);
    uart::log_hex(b"RAM: usable top=", L2MEM_END - cache_bytes);
    uart::log_hex(b"RAM: stack top=", stack_top);
    if cache_bytes == 0 || stack_top > L2MEM_END - cache_bytes {
        uart::log(b"RAM: stack top is inside the L2 cache area\r\n");
    }
}
