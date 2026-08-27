//! The analog configuration bus behind the ESP32-P4's analog blocks.
//!
//! Several analog blocks -- the MSPI/PSRAM PLL, the SAR ADC, the bias
//! generator -- have registers that are not memory-mapped at all. They sit
//! behind a small hardware I2C master (`I2C_ANA_MST`) that the CPU drives one
//! byte at a time: write a block address, a register address and a direction
//! into one control register, wait for the busy bit to clear, and read the
//! answer back out of the same register.
//!
//! Which analog block the master is talking to is not part of that control
//! word. It is a separate routing choice made in `ANA_CONF1`/`ANA_CONF2`, one
//! bit per block, and the bit number has nothing to do with the block address
//! -- so [`select`] carries both, and every access sets the routing first.
//! Getting that wrong reads a plausible byte from the wrong block, which is
//! the failure this module exists to make impossible to write by accident.
//!
//! ## Why this lives in IRAM
//!
//! [`crate::psram`] calls this before PSRAM works and while the flash cache
//! is being reconfigured, so the code cannot be in flash. It is therefore in
//! the same `.iram.text.critical.psram` section as its first caller, and
//! `tools/check_elf_layout.py` checks that the whole closure stays out of
//! flash. [`crate::entropy`] calls it long after boot, where any section
//! would do; sharing the IRAM copy costs a few hundred bytes and keeps one
//! implementation of the bus protocol rather than two.

/// The analog master's registers.
const I2C_ANA_MST: usize = 0x5012_4000;
/// Low-power peripheral clock gating, which holds the master's clock enable.
const LPPERI: usize = 0x5012_0000;

const CTRL0: usize = I2C_ANA_MST;
const ANA_CONF1: usize = I2C_ANA_MST + 0x1C;
const ANA_CONF2: usize = I2C_ANA_MST + 0x20;
const CLK160M: usize = I2C_ANA_MST + 0x34;

const CONF_MASK: u32 = 0x00FF_FFFF;
const BUSY: u32 = 1 << 25;
const WRITE_ENABLE: u32 = 1 << 24;
const DATA_SHIFT: u32 = 16;
const REGISTER_SHIFT: u32 = 8;

const LPPERI_CK_EN_LP_I2CMST: u32 = 1 << 27;

/// How long a transfer may hold the bus busy before it is called dead.
///
/// A count rather than a duration: this runs before the tick timer exists.
const BUSY_SPINS: u32 = 1_000_000;

/// One analog block: the address that goes in the control word, and the
/// routing bit that decides whether the master is talking to it at all.
///
/// The two numbers are unrelated, which is why they travel together.
#[derive(Clone, Copy)]
pub struct Block {
    address: u8,
    select: u32,
}

/// The MSPI/PSRAM PLL, block 0x63 routed through bit 9.
pub const MSPI_XTAL: Block = Block {
    address: 0x63,
    select: 1 << 9,
};

/// The SAR ADC's analog registers, block 0x69 routed through bit 7. What
/// [`crate::entropy`] needs to bring the ADC up as an entropy source.
pub const SAR_ADC: Block = Block {
    address: 0x69,
    select: 1 << 7,
};

/// Turns on the master's clock.
///
/// The bootloader normally leaves it enabled. Forcing it on here means a
/// CPU-only reboot, or a future bootloader that does not, cannot leave the
/// bus dead in a way that only shows up as a wrong readback.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
pub fn enable_clock() {
    unsafe {
        modify(LPPERI, LPPERI_CK_EN_LP_I2CMST, LPPERI_CK_EN_LP_I2CMST);
        modify(CLK160M, 1, 1);
    }
}

/// Routes the master to one block, and only that one.
///
/// Both configuration registers are cleared first: leaving another block's
/// bit set is what makes a read return someone else's byte.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn select(block: Block) {
    unsafe {
        modify(ANA_CONF1, CONF_MASK, 0);
        modify(ANA_CONF2, CONF_MASK, block.select);
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn wait_idle() -> bool {
    let mut timeout = BUSY_SPINS;
    while unsafe { read(CTRL0) } & BUSY != 0 {
        if timeout == 0 {
            return false;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
    true
}

/// Reads one 8-bit analog register, or `None` if the bus never went idle.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
pub fn read_register(block: Block, register: u8) -> Option<u8> {
    select(block);
    if !wait_idle() {
        return None;
    }
    unsafe {
        write(
            CTRL0,
            u32::from(block.address) | (u32::from(register) << REGISTER_SHIFT),
        );
    }
    if !wait_idle() {
        return None;
    }
    Some((unsafe { read(CTRL0) } >> DATA_SHIFT) as u8)
}

/// Writes one 8-bit analog register. Returns whether the bus went idle
/// afterwards, which is the only acknowledgement this bus offers.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
pub fn write_register(block: Block, register: u8, value: u8) -> bool {
    select(block);
    if !wait_idle() {
        return false;
    }
    unsafe {
        write(
            CTRL0,
            u32::from(block.address)
                | (u32::from(register) << REGISTER_SHIFT)
                | (u32::from(value) << DATA_SHIFT)
                | WRITE_ENABLE,
        );
    }
    wait_idle()
}

/// Writes `bits` into `register`'s `lsb..=msb` field, leaving the rest of
/// the byte as it was.
///
/// A read-modify-write, because these bytes pack unrelated settings: the SAR
/// ADC's `DTEST` and `ENT` controls share register 0x9, and writing one as a
/// whole byte would clear the other.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
pub fn write_field(block: Block, register: u8, msb: u8, lsb: u8, bits: u8) -> bool {
    let Some(current) = read_register(block, register) else {
        return false;
    };
    let width = msb - lsb + 1;
    let mask = (((1u16 << width) - 1) as u8) << lsb;
    write_register(block, register, (current & !mask) | ((bits << lsb) & mask))
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
