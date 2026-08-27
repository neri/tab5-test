//! True randomness for TLS: the ESP32-P4's hardware RNG, and the CSPRNG it
//! seeds.
//!
//! ## Why the RNG needs help
//!
//! `LP_SYSTEM_REG_RNG_DATA` is not a noise source. It is a PRNG that an
//! analog entropy source stirs a couple of bits into per bus cycle, and on
//! Espressif's own parts that source is the radio -- which on Tab5 is a
//! *different chip*. The ESP32-C6 does the Wi-Fi (`docs/WIFI.md`), so the
//! P4's own RNG has nothing feeding it and reading it would return a
//! deterministic sequence that looks perfectly random.
//!
//! The other source the silicon offers is the SAR ADC sampling an unbonded
//! input with maximum attenuation, which is what ESP-IDF's
//! `bootloader_random_enable` sets up. That is what [`Source`] turns on. It
//! is not free -- it powers the SAR ADC, routes it to the digital
//! controller, and reconfigures the analog registers behind
//! [`crate::regi2c`] -- so it is held only for as long as seeding takes.
//!
//! What is deliberately *not* counted as entropy here: the cycle counter,
//! the MAC address, UART timing, uptime. `net::Stack` seeds TCP initial
//! sequence numbers from the cycle counter and the MAC, and that is fine for
//! what it does; a TLS key share made the same way would be guessable by
//! anyone who knows roughly when the board booted. When the hardware source
//! cannot be brought up, seeding fails and the connection never starts.
//!
//! ## The shape of one seeding
//!
//! 1. take the [`Source`] guard, which enables the ADC entropy source
//! 2. read [`SEED_BYTES`] from the RNG, no faster than the PRNG is stirred
//! 3. seed a ChaCha20 CSPRNG with them
//! 4. drop the guard, which powers the ADC back down whatever happened
//!
//! Nothing is kept across a reboot and nothing is written to flash: a stored
//! seed is a seed an attacker can read off the board.

use core::sync::atomic::{AtomicBool, Ordering};

use rand_chacha::ChaCha20Rng;
use rand_core::{CryptoRng, RngCore, SeedableRng};

use crate::{delay, regi2c};

/// How much hardware randomness one CSPRNG seeding takes.
///
/// ChaCha20's key is 32 bytes and there is nothing to gain from seeding it
/// with less; the plan's floor is 256 bits.
pub const SEED_BYTES: usize = 32;

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Error {
    /// Something else holds the ADC.
    Busy,
    /// A write to the SAR ADC's analog registers was not acknowledged, so
    /// the entropy source is not known to be running.
    AnalogBus,
    /// The RNG returned the same word every time. Either the entropy source
    /// never started or the register is not what this thinks it is; both
    /// mean the bytes are not random.
    Stuck,
}

impl Error {
    /// The one-word failure name, shared with the TLS layer: from the
    /// caller's side there is only one answer, "no true randomness".
    pub fn name(self) -> &'static str {
        "entropy"
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Busy => "the ADC entropy source is already in use",
            Self::AnalogBus => "the SAR ADC analog registers did not answer",
            Self::Stuck => "the hardware RNG returned a constant",
        }
    }
}

/// Makes the next [`Csprng::from_hardware`] fail, without touching the
/// hardware.
///
/// The point of the test hook is to check what the *callers* do: a TLS
/// connection that cannot be seeded must not put a single packet on the
/// wire, and that is only observable if the failure can be produced on
/// demand.
static FORCED_FAILURE: AtomicBool = AtomicBool::new(false);

pub fn force_failure(forced: bool) {
    FORCED_FAILURE.store(forced, Ordering::Relaxed);
}

pub fn failure_is_forced() -> bool {
    FORCED_FAILURE.load(Ordering::Relaxed)
}

/// Counts enabled/disabled transitions so a test can check they pair up.
static ENABLES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static DISABLES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

pub fn transition_counts() -> (u32, u32) {
    (
        ENABLES.load(Ordering::Relaxed),
        DISABLES.load(Ordering::Relaxed),
    )
}

/// Whether the ADC is currently held as an entropy source.
///
/// The exclusion this gives is against *this firmware* starting a second
/// user of the ADC, not against another core: everything that touches the
/// ADC runs on the main loop. What it buys is that a nested
/// [`Source::enable`] fails instead of the inner guard's `Drop` powering the
/// ADC down underneath the outer one.
static HELD: AtomicBool = AtomicBool::new(false);

/// The ADC entropy source, powered up for as long as this exists.
///
/// A guard rather than an enable/disable pair because every error path
/// between the two has to power the ADC back down, and the only way to get
/// that right once is to let `Drop` do it.
pub struct Source {
    _private: (),
}

impl Source {
    /// Powers up the SAR ADC as a noise source.
    ///
    /// Fails rather than continuing if the analog registers do not answer:
    /// a half-configured ADC still makes the RNG return *something*, and
    /// that something is what this refuses to hand out.
    pub fn enable() -> Result<Source, Error> {
        if HELD.swap(true, Ordering::Acquire) {
            return Err(Error::Busy);
        }
        ENABLES.fetch_add(1, Ordering::Relaxed);
        let source = Source { _private: () };
        match enable_adc_entropy() {
            // `source` is already live, so the failure path powers the ADC
            // down by dropping it rather than by a second call here.
            Err(error) => Err(error),
            Ok(()) => Ok(source),
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        disable_adc_entropy();
        DISABLES.fetch_add(1, Ordering::Relaxed);
        HELD.store(false, Ordering::Release);
    }
}

/// A ChaCha20 CSPRNG seeded from the hardware source.
///
/// One instance per TLS connection, seeded fresh: two connections never
/// share a stream, and there is no long-lived state for a later compromise
/// to unwind.
pub struct Csprng(ChaCha20Rng);

impl Csprng {
    /// Brings up the entropy source, takes [`SEED_BYTES`] from it and lets
    /// it go again.
    pub fn from_hardware() -> Result<Csprng, Error> {
        if failure_is_forced() {
            // Still nothing touched: the hook has to be indistinguishable
            // from "the hardware would not come up" at the caller.
            return Err(Error::Stuck);
        }
        let mut seed = [0u8; SEED_BYTES];
        {
            let _source = Source::enable()?;
            read_hardware_bytes(&mut seed)?;
        }
        Ok(Csprng(ChaCha20Rng::from_seed(seed)))
    }
}

impl RngCore for Csprng {
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        self.0.fill_bytes(destination);
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.0.try_fill_bytes(destination)
    }
}

impl CryptoRng for Csprng {}

/// Fills `destination` straight from the hardware RNG.
///
/// The caller must be holding a [`Source`]; without one this reads a PRNG
/// that nothing is stirring.
pub fn read_hardware_bytes(destination: &mut [u8]) -> Result<(), Error> {
    let mut first: Option<u32> = None;
    let mut varied = false;
    for chunk in destination.chunks_mut(4) {
        let word = read_random_word();
        match first {
            None => first = Some(word),
            Some(previous) => varied |= word != previous,
        }
        for (slot, byte) in chunk.iter_mut().zip(word.to_le_bytes()) {
            *slot = byte;
        }
    }
    // One repeated word is a coincidence worth 2^-32; every word repeated
    // over 32 bytes is a dead source.
    if destination.len() > 4 && !varied {
        return Err(Error::Stuck);
    }
    Ok(())
}

/// One 32-bit reading, no sooner than the PRNG behind the register has been
/// re-stirred.
///
/// The hardware mixes a couple of bits of analog noise into the register per
/// bus cycle, so reading it back to back returns a value with almost no new
/// entropy in it, so each of the four readings folded in here waits
/// [`READ_INTERVAL_US`] first -- including the first, so that a value left
/// in the register from before this seeding cannot be the whole answer.
fn read_random_word() -> u32 {
    let mut result = 0u32;
    for _ in 0..4 {
        delay::delay_us(READ_INTERVAL_US);
        result ^= unsafe { read(RNG_DATA) };
    }
    result
}

/// Microseconds between readings that are counted as fresh.
///
/// ESP-IDF waits `CPU_MHZ * 14` APB cycles per *byte* on this part. What
/// that is in microseconds depends on the APB divider the CPU happens to be
/// running with -- between about 56 and 126 us here -- so this takes the
/// slow end rather than working the divider out at run time. Four readings
/// per word puts one 32-byte seeding at roughly 4 ms, which is nothing once
/// per connection; being faster than the hardware is stirred is what would
/// actually cost something, and it would cost entropy.
const READ_INTERVAL_US: u32 = 128;

// -------------------------------------------------------------- registers

/// The hardware RNG's data register, in the always-on low-power domain.
const RNG_DATA: usize = 0x5011_01A4;

const HP_SYS_CLKRST: usize = 0x500E_6000;
const SOC_CLK_CTRL2: usize = HP_SYS_CLKRST + 0x1C;
const PERI_CLK_CTRL22: usize = HP_SYS_CLKRST + 0x9C;
const PERI_CLK_CTRL23: usize = HP_SYS_CLKRST + 0xA0;
const HP_RST_EN2: usize = HP_SYS_CLKRST + 0xC8;

const ADC_APB_CLK_EN: u32 = 1 << 5;
const ADC_CLK_EN: u32 = 1;
const RST_EN_ADC: u32 = 1 << 10;
const ADC_CLK_SRC_SEL: u32 = 0x3 << 30;
/// `div_num` [8:1], `numerator` [16:9] and `denominator` [24:17] together.
const ADC_CLK_DIV: u32 = 0x01FF_FFFE;
const ADC_CLK_DIV_NUM_SHIFT: u32 = 1;

const ADC: usize = 0x500D_E000;
const ADC_CTRL: usize = ADC;
const ADC_CTRL2: usize = ADC + 0x4;
const ADC_SAR1_PATT_TAB: usize = ADC + 0x18;
const ADC_SAR2_PATT_TAB: usize = ADC + 0x28;

const SAR_CLK_GATED: u32 = 1 << 5;
const SAR_CLK_DIV: u32 = 0xFF << 6;
const SAR_CLK_DIV_SHIFT: u32 = 6;
const SAR1_PATT_LEN: u32 = 0xF << 14;
const SAR1_PATT_LEN_SHIFT: u32 = 14;
const XPD_SAR1_FORCE: u32 = 0x3 << 26;
const XPD_SAR1_FORCE_SHIFT: u32 = 26;
/// `xpd_sar_force`: 0 leaves the power state to the FSM, 3 forces it on.
const XPD_SAR_FSM: u32 = 0;
const XPD_SAR_POWER_UP: u32 = 3;

const TIMER_SEL: u32 = 1 << 11;
const TIMER_TARGET: u32 = 0xFFF << 12;
const TIMER_TARGET_SHIFT: u32 = 12;
const TIMER_EN: u32 = 1 << 24;

const LP_ADC: usize = 0x5012_7000;
const LP_ADC_MEAS1_CTRL2: usize = LP_ADC + 0xC;
const LP_ADC_MEAS1_MUX: usize = LP_ADC + 0x10;
const MEAS1_START_FORCE: u32 = 1 << 18;
const SAR1_EN_PAD_FORCE: u32 = 1 << 31;
const SAR1_DIG_FORCE: u32 = 1 << 31;

const PMU: usize = 0x5011_5000;
const PMU_RF_PWC: usize = PMU + 0x15C;
const PERIF_I2C_RSTB: u32 = 1 << 26;
const XPD_PERIF_I2C: u32 = 1 << 27;

/// SAR ADC analog register 0x9 packs both the test-output select
/// (`DTEST`, bits 3:0) and the entropy-source enable (`ENT`, bit 4), which
/// is why they are written as fields rather than as a byte.
const SAR_ADC_REGISTER_TEST: u8 = 0x9;
/// Register 0x0/0x1 hold the SAR1 initial calibration code, low then high.
const SAR_ADC_REGISTER_CODE_LOW: u8 = 0x0;
const SAR_ADC_REGISTER_CODE_HIGH: u8 = 0x1;
/// The initial code ESP-IDF writes before using the ADC as a noise source.
const SAR_ADC_INITIAL_CODE: u16 = 2166;

/// Channel 10 at maximum attenuation, the input ESP-IDF samples for noise.
const ENTROPY_ATTENUATION: u32 = 3;
const ENTROPY_CHANNEL: u32 = 10;

/// Powers the SAR ADC up and points the digital controller at the noise
/// channel.
///
/// The order is ESP-IDF's `bootloader_random_enable`, and it matters: the
/// analog registers cannot be written before the analog bus is powered
/// (`PMU_RF_PWC`) and clocked, and the trigger must not start before the
/// pattern table says what to sample.
fn enable_adc_entropy() -> Result<(), Error> {
    unsafe {
        // Reset the digital controller, then clock it.
        modify(HP_RST_EN2, RST_EN_ADC, RST_EN_ADC);
        modify(HP_RST_EN2, RST_EN_ADC, 0);
        modify(SOC_CLK_CTRL2, ADC_APB_CLK_EN, ADC_APB_CLK_EN);
        modify(PERI_CLK_CTRL23, ADC_CLK_EN, ADC_CLK_EN);

        // XTAL, undivided: a noise source clocked off a PLL would follow
        // whatever the PLL is doing.
        modify(PERI_CLK_CTRL22, ADC_CLK_SRC_SEL, 0);
        modify(PERI_CLK_CTRL23, ADC_CLK_DIV, 0);
        modify(ADC_CTRL, SAR_CLK_GATED, SAR_CLK_GATED);

        // Power the analog bus's peripheral group, held in reset across the
        // change so the block comes up from a known state.
        modify(PMU_RF_PWC, PERIF_I2C_RSTB, 0);
        modify(PMU_RF_PWC, XPD_PERIF_I2C, XPD_PERIF_I2C);
        modify(PMU_RF_PWC, PERIF_I2C_RSTB, PERIF_I2C_RSTB);
    }
    regi2c::enable_clock();

    // `DTEST` off, `ENT` on: the ADC's analog output is routed to the
    // entropy path rather than to the test pin.
    if !regi2c::write_field(regi2c::SAR_ADC, SAR_ADC_REGISTER_TEST, 3, 0, 0)
        || !regi2c::write_field(regi2c::SAR_ADC, SAR_ADC_REGISTER_TEST, 4, 4, 1)
        || !regi2c::write_field(
            regi2c::SAR_ADC,
            SAR_ADC_REGISTER_CODE_HIGH,
            3,
            0,
            (SAR_ADC_INITIAL_CODE >> 8) as u8,
        )
        || !regi2c::write_field(
            regi2c::SAR_ADC,
            SAR_ADC_REGISTER_CODE_LOW,
            7,
            0,
            SAR_ADC_INITIAL_CODE as u8,
        )
    {
        return Err(Error::AnalogBus);
    }

    unsafe {
        // Every one of the four pattern slots samples the same channel, and
        // the length is then set to one, so whichever slot the controller
        // starts from it samples the noise input.
        for slot in 0..4 {
            write_pattern_entry(slot, ENTROPY_ATTENUATION | (ENTROPY_CHANNEL << 2));
        }
        modify(ADC_CTRL, SAR1_PATT_LEN, 0 << SAR1_PATT_LEN_SHIFT);

        // Hand SAR1 to the digital controller rather than to the ULP.
        modify(LP_ADC_MEAS1_MUX, SAR1_DIG_FORCE, SAR1_DIG_FORCE);
        modify(
            LP_ADC_MEAS1_CTRL2,
            MEAS1_START_FORCE | SAR1_EN_PAD_FORCE,
            MEAS1_START_FORCE | SAR1_EN_PAD_FORCE,
        );

        // Force it powered rather than leaving it to the FSM, which would
        // power it down between conversions.
        modify(ADC_CTRL, SAR_CLK_GATED, SAR_CLK_GATED);
        modify(
            ADC_CTRL,
            XPD_SAR1_FORCE,
            XPD_SAR_POWER_UP << XPD_SAR1_FORCE_SHIFT,
        );

        modify(ADC_CTRL, SAR_CLK_DIV, 15 << SAR_CLK_DIV_SHIFT);
        modify(ADC_CTRL2, TIMER_TARGET, 100 << TIMER_TARGET_SHIFT);
        modify(ADC_CTRL2, TIMER_SEL | TIMER_EN, TIMER_SEL | TIMER_EN);
    }
    Ok(())
}

/// Undoes [`enable_adc_entropy`], in the reverse order and without a way to
/// fail.
///
/// This runs from `Drop`, including on the path where the enable itself gave
/// up part-way, so every step has to be safe to run against hardware that
/// never got as far as being configured. Nothing here reads back: there is
/// no useful answer to "the power-down did not take".
///
/// The analog master's clock is left on. `crate::psram` needs it during
/// boot and ESP-IDF reference-counts it for exactly that reason; turning it
/// off here would be this module deciding on behalf of a subsystem it does
/// not own.
fn disable_adc_entropy() {
    unsafe {
        modify(ADC_CTRL2, TIMER_EN, 0);
        for slot in 0..4 {
            write(ADC_SAR1_PATT_TAB + slot * 4, 0x00FF_FFFF);
            write(ADC_SAR2_PATT_TAB + slot * 4, 0x00FF_FFFF);
        }
    }

    // Clearing the calibration code and the entropy enable is what actually
    // takes the analog block out of noise mode; a failure here leaves the
    // ADC powered but harmless, and there is nothing better to do about it.
    let _ = regi2c::write_field(regi2c::SAR_ADC, SAR_ADC_REGISTER_CODE_HIGH, 3, 0, 0);
    let _ = regi2c::write_field(regi2c::SAR_ADC, SAR_ADC_REGISTER_CODE_LOW, 7, 0, 0);
    let _ = regi2c::write_field(regi2c::SAR_ADC, SAR_ADC_REGISTER_TEST, 3, 0, 0);
    let _ = regi2c::write_field(regi2c::SAR_ADC, SAR_ADC_REGISTER_TEST, 4, 4, 0);

    unsafe {
        modify(PMU_RF_PWC, XPD_PERIF_I2C, 0);

        // Back to the reset divider, and SAR1 back to the ULP controller,
        // so that a later ADC user finds the block as it was.
        modify(PERI_CLK_CTRL23, ADC_CLK_DIV, 4 << ADC_CLK_DIV_NUM_SHIFT);
        modify(PERI_CLK_CTRL22, ADC_CLK_SRC_SEL, 0);
        modify(ADC_CTRL, XPD_SAR1_FORCE, XPD_SAR_FSM << XPD_SAR1_FORCE_SHIFT);
        modify(LP_ADC_MEAS1_MUX, SAR1_DIG_FORCE, 0);
    }
}

/// Writes one six-bit pattern-table slot.
///
/// The four slots of one register are packed from the top down -- slot 0 in
/// bits 23:18, slot 3 in bits 5:0 -- which is why the shift runs backwards.
///
/// # Safety
/// The ADC's registers must be clocked.
unsafe fn write_pattern_entry(slot: usize, pattern: u32) {
    let register = ADC_SAR1_PATT_TAB + (slot / 4) * 4;
    let offset = ((slot % 4) * 6) as u32;
    let mask = 0x00FC_0000 >> offset;
    unsafe { modify(register, mask, ((pattern & 0x3F) << 18) >> offset) };
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
