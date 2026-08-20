//! Minimal ESP32-P4 ECO2 Hex-DDR PSRAM bring-up.
//!
//! Maps the Tab5's full 32 MiB PSRAM at `0x4800_0000` (well within the chip's
//! 64 MiB PSRAM MMU window) and verifies the mapping before returning it to
//! the caller. The first framebuffer-sized slot backs the LCD module;
//! [`Psram::heap`] exposes everything after it for a global allocator.

use core::mem::transmute;

use crate::uart;

pub const WIDTH: usize = 720;
pub const HEIGHT: usize = 1280;
pub const BYTES_PER_PIXEL: usize = 2;
pub const FRAMEBUFFER_BYTES: usize = WIDTH * HEIGHT * BYTES_PER_PIXEL;

const PSRAM_VADDR: usize = 0x4800_0000;
pub const MAPPED_BYTES: usize = 32 * 1024 * 1024;
const PAGE_BYTES: usize = 64 * 1024;
const CACHE_LINE_BYTES: usize = 64;

const HP_SYS_CLKRST: usize = 0x500E_6000;
const HP_RST_EN0: usize = HP_SYS_CLKRST + 0xC0;
const MSPI2: usize = 0x5008_E000;
const MSPI3: usize = 0x5008_F000;
const MSPI_IOMUX: usize = 0x500E_1200;
const PMU: usize = 0x5011_5000;
const LP_CLKRST: usize = 0x5011_1000;
const LP_STORE15: usize = 0x5011_0068;
const LPPERI: usize = 0x5012_0000;
const I2C_ANA_MST: usize = 0x5012_4000;
const FALLBACK_TEST_MAGIC: u32 = 0x5053_4642;

const ROM_SPI_CMD_CONFIG: usize = 0x4FC0_0108;
const ROM_SPI_SET_OP_MODE: usize = 0x4FC0_0110;
const ROM_CACHE_PSRAM_MMU_SET: usize = 0x4FC0_0520;
const ROM_CACHE_INVALIDATE_ADDR: usize = 0x4FC0_03E4;
const ROM_CACHE_WRITEBACK_INVALIDATE_ADDR: usize = 0x4FC0_03FC;

const SYNC_READ: u32 = 0x0000;
const SYNC_WRITE: u32 = 0x8080;
const REG_READ: u32 = 0x4040;
const REG_WRITE: u32 = 0xC0C0;

#[derive(Clone, Copy)]
enum PsramClockSource {
    Spll480Mhz,
    Mpll400Mhz,
}

#[derive(Clone, Copy)]
#[repr(u32)]
enum InitStage {
    Clock = 1,
    ModeRegisterRead = 2,
    ModeRegisterWrite = 3,
    Mr8Read = 4,
    Mr8Write = 5,
    CommandPath = 6,
    Tuning = 7,
    DirectMemoryTest = 8,
    MmuMap = 9,
    MappedMemoryTest = 10,
}

/// Every value which changes with PSRAM operating frequency.
///
/// Keep this in DRAM because `init_critical` reads it while the shared MSPI
/// block is being reset and normal DROM/IROM access is unavailable. Keeping
/// both profiles here avoids scattering frequency-specific latency and
/// divider literals through the driver and makes 80 MHz fallback complete.
#[derive(Clone, Copy)]
struct PsramTiming {
    clock_source: PsramClockSource,
    frequency_mhz: u32,
    operating_bus_divider: u32,
    tuning_bus_divider: u32,
    read_dummy_bits: u32,
    write_dummy_bits: u32,
    register_read_dummy_bits: u32,
    read_latency_cycles: u32,
    write_latency_cycles: u32,
    mr0_read_latency_code: u8,
    mr4_write_latency_code: u8,
    use_preferred_dqs: bool,
    preferred_dqs_phase: u8,
    preferred_data_delay: u8,
    preferred_dqs_delay: u8,
}

#[used]
#[unsafe(link_section = ".dram.rodata.psram")]
static PSRAM_80_MHZ: PsramTiming = PsramTiming {
    clock_source: PsramClockSource::Spll480Mhz,
    frequency_mhz: 80,
    operating_bus_divider: 6,
    tuning_bus_divider: 24,
    read_dummy_bits: 18,
    write_dummy_bits: 8,
    register_read_dummy_bits: 8,
    read_latency_cycles: 10,
    write_latency_cycles: 5,
    mr0_read_latency_code: 2,
    mr4_write_latency_code: 2,
    use_preferred_dqs: true,
    preferred_dqs_phase: 0,
    preferred_data_delay: 0,
    preferred_dqs_delay: 0,
};

/// ESP-IDF v5.5.3's AP Memory Hex-PSRAM 200 MHz timing.
///
/// The PSRAM clock is MPLL 400 MHz / 2. Fixed-latency mode uses 14 read
/// cycles (MR0 code 4) and 7 write cycles (MR4 code 1), hence 26 read-dummy
/// bits and 12 register-read/write-dummy bits on the DDR command path.
#[used]
#[unsafe(link_section = ".dram.rodata.psram")]
static PSRAM_200_MHZ: PsramTiming = PsramTiming {
    clock_source: PsramClockSource::Mpll400Mhz,
    frequency_mhz: 200,
    operating_bus_divider: 2,
    tuning_bus_divider: 20,
    read_dummy_bits: 26,
    write_dummy_bits: 12,
    register_read_dummy_bits: 12,
    read_latency_cycles: 14,
    write_latency_cycles: 7,
    mr0_read_latency_code: 4,
    mr4_write_latency_code: 1,
    use_preferred_dqs: false,
    preferred_dqs_phase: 0,
    preferred_data_delay: 0,
    preferred_dqs_delay: 0,
};

const CACHE_MAP_L1_DCACHE: u32 = 1 << 4;
const CACHE_MAP_L2_CACHE: u32 = 1 << 5;

#[repr(C)]
struct RomSpiCommand {
    command: u16,
    command_bits: u16,
    address: *mut u32,
    address_bits: u32,
    tx_data: *mut u32,
    tx_bits: u32,
    rx_data: *mut u32,
    rx_bits: u32,
    dummy_bits: u32,
}

#[repr(align(4))]
struct AlignedBytes<const N: usize>([u8; N]);

macro_rules! dram_message {
    ($name:ident, $value:expr) => {
        #[used]
        #[unsafe(link_section = ".dram.rodata.psram")]
        static $name: [u8; ($value).len()] = *($value);
    };
}

dram_message!(
    MODE_REGISTER_TRANSACTION_FAILED,
    b"PSRAM: mode-register transaction failed\r\n"
);
dram_message!(
    MODE_REGISTER_WRITE_FAILED,
    b"PSRAM: mode-register write failed\r\n"
);
dram_message!(MR8_READ_FAILED, b"PSRAM: MR8 read failed\r\n");
dram_message!(MR8_WRITE_FAILED, b"PSRAM: MR8 write failed\r\n");
dram_message!(
    COMMAND_TRANSACTION_TIMEOUT,
    b"PSRAM: command-path transaction timed out\r\n"
);
dram_message!(COMMAND_READ_LABEL, b"PSRAM: command read=");
dram_message!(COMMAND_TEST_FAILED, b"PSRAM: command-path test failed\r\n");
dram_message!(MMU_ERROR_LABEL, b"PSRAM: MMU error=");
dram_message!(MAPPED_TEST_FAILED, b"PSRAM: mapped memory test failed\r\n");
dram_message!(READY_MESSAGE, b"PSRAM: ready (framebuffer + heap)\r\n");
dram_message!(
    TUNING_WRITE_TIMEOUT,
    b"PSRAM: tuning reference write timed out\r\n"
);
dram_message!(
    PREFERRED_DQS_FAILED,
    b"PSRAM: preferred DQS point failed; running full sweep\r\n"
);
dram_message!(NO_VALID_DQS_PHASE, b"PSRAM: no valid DQS phase\r\n");
dram_message!(
    NO_STABLE_DQS_WINDOW,
    b"PSRAM: no stable DQS delay window\r\n"
);
dram_message!(BAD_ADDRESS_LABEL, b"PSRAM: bad address=");
dram_message!(BAD_VALUE_LABEL, b"PSRAM: bad value=");
dram_message!(
    MSPI3_BUSY,
    b"PSRAM: MSPI3 busy before command configuration\r\n"
);
dram_message!(MSPI3_TIMEOUT, b"PSRAM: MSPI3 command timeout\r\n");
dram_message!(MSPI3_CMD_LABEL, b"PSRAM: MSPI3 CMD=");
dram_message!(MSPI3_CLOCK_LABEL, b"PSRAM: MSPI3 CLOCK=");
dram_message!(MSPI3_USER_LABEL, b"PSRAM: MSPI3 USER=");
dram_message!(MSPI3_MISC_LABEL, b"PSRAM: MSPI3 MISC=");
dram_message!(HP_CLK_LABEL, b"PSRAM: HP CLK=");
dram_message!(LDO2_LABEL, b"PSRAM: LDO2=");
dram_message!(LDO2_ANA_LABEL, b"PSRAM: LDO2 ANA=");
dram_message!(RF_PWC_LABEL, b"PSRAM: RF PWC=");
dram_message!(PROFILE_MHZ_LABEL, b"PSRAM: profile MHz=");
dram_message!(READ_LATENCY_LABEL, b"PSRAM: read latency cycles=");
dram_message!(WRITE_LATENCY_LABEL, b"PSRAM: write latency cycles=");
dram_message!(DQS_PHASE_LABEL, b"PSRAM: DQS phase=");
dram_message!(DQS_DATA_DELAY_LABEL, b"PSRAM: DQS data delay=");
dram_message!(DQS_DELAY_LABEL, b"PSRAM: DQS delay=");
dram_message!(DQS_WINDOW_START_LABEL, b"PSRAM: DQS window start=");
dram_message!(DQS_WINDOW_LENGTH_LABEL, b"PSRAM: DQS window length=");
dram_message!(FALLBACK_STAGE_LABEL, b"PSRAM: 200 MHz failed stage=");
dram_message!(FALLBACK_MESSAGE, b"PSRAM: falling back to 80 MHz\r\n");
dram_message!(MPLL_TIMEOUT, b"PSRAM: MPLL calibration timed out\r\n");
dram_message!(MPLL_READBACK_FAILED, b"PSRAM: MPLL readback failed\r\n");
dram_message!(DIRECT_TEST_FAILED, b"PSRAM: direct memory test failed\r\n");
dram_message!(
    FORCED_FALLBACK,
    b"PSRAM: diagnostic forced tuning failure\r\n"
);

#[used]
#[unsafe(link_section = ".dram.rodata.psram")]
static TUNING_REFERENCE_WORDS: [u32; 32] = [
    0x7f786655, 0xa5ff005a, 0x3f3c33aa, 0xa5ff5a00, 0x1f1e9955, 0xa5005aff, 0x0f0fccaa, 0xa55a00ff,
    0x07876655, 0xffa55a00, 0x03c333aa, 0xff00a55a, 0x01e19955, 0xff005aa5, 0x00f0ccaa, 0xff5a00a5,
    0x80786655, 0x00a5ff5a, 0xc03c33aa, 0x00a55aff, 0xe01e9355, 0x00ff5aa5, 0xf00fccaa, 0x005affa5,
    0xf8876655, 0x5aa5ff00, 0xfcc333aa, 0x5affa500, 0xfee19955, 0x5a00a5ff, 0x11f0ccaa, 0x5a00ffa5,
];

#[derive(Clone, Copy)]
pub struct Psram {
    base: usize,
    bytes: usize,
    frequency_mhz: u32,
}

impl Psram {
    /// Frequency of the profile that passed tuning and both memory tests.
    pub fn frequency_mhz(&self) -> u32 {
        self.frequency_mhz
    }

    pub fn framebuffer(&self) -> Option<*mut u16> {
        if FRAMEBUFFER_BYTES > self.bytes {
            return None;
        }
        Some(self.base as *mut u16)
    }

    /// Returns the PSRAM span after the framebuffer, for use as a heap.
    pub fn heap(&self) -> (*mut u8, usize) {
        (
            (self.base + FRAMEBUFFER_BYTES) as *mut u8,
            self.bytes - FRAMEBUFFER_BYTES,
        )
    }

    /// Writes back a bounded byte range within the framebuffer.
    pub fn writeback_range(&self, offset: usize, bytes: usize) -> bool {
        let Some(framebuffer) = self.framebuffer() else {
            return false;
        };
        let Some(end) = offset.checked_add(bytes) else {
            return false;
        };
        if bytes == 0 || end > FRAMEBUFFER_BYTES {
            return false;
        }

        // Both P4 cache levels use 64-byte lines. Expand the requested range
        // so neither level can retain a dirty edge line after the operation.
        let aligned_offset = offset & !(CACHE_LINE_BYTES - 1);
        let aligned_end = end
            .saturating_add(CACHE_LINE_BYTES - 1)
            .min(FRAMEBUFFER_BYTES)
            & !(CACHE_LINE_BYTES - 1);
        let aligned_end = if aligned_end < end {
            FRAMEBUFFER_BYTES
        } else {
            aligned_end
        };
        let address = framebuffer as usize + aligned_offset;
        let length = aligned_end - aligned_offset;
        iram_cache_writeback_invalidate(address as u32, length as u32)
    }
}

/// Writes back and invalidates an arbitrary span of the mapped PSRAM window.
///
/// Unlike `Psram::writeback_range`, which is bounded to the framebuffer, this
/// takes a raw address so a caller staging its own buffer -- `membench`
/// wanting a cold cache before each pass -- can drop it out of both levels.
/// The caller is responsible for the span lying inside the mapping.
pub fn writeback_invalidate(address: usize, bytes: usize) {
    let _ = iram_cache_writeback_invalidate(address as u32, bytes as u32);
}

/// Marks the next CPU-only reboot to reject an otherwise valid 200 MHz DQS
/// result. The marker is consumed before PSRAM initialization, exercising the
/// real post-200-MHz reset and 80 MHz mode-register recovery path once.
pub fn request_fallback_test() {
    unsafe { write(LP_STORE15, FALLBACK_TEST_MAGIC) };
}

/// Performs the ROM cache operation from IRAM so the call and return path do
/// not depend on IROM while the cache controller is maintaining L1/L2 state.
///
/// Machine interrupts deliberately remain enabled. The LCD ISR and its full
/// relocation closure are IRAM/DRAM-only, and a full framebuffer writeback is
/// far too long to mask frame-completion interrupts without causing underruns.
#[inline(never)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".iram.text.critical.cache")]
extern "C" fn iram_cache_writeback_invalidate(address: u32, bytes: u32) -> bool {
    let writeback_invalidate: unsafe extern "C" fn(u32, u32, u32) -> i32 =
        unsafe { transmute(ROM_CACHE_WRITEBACK_INVALIDATE_ADDR) };
    unsafe {
        // Push dirty L1 lines into L2 first, then push L2 into memory.
        let l1 = writeback_invalidate(CACHE_MAP_L1_DCACHE, address, bytes);
        let l2 = writeback_invalidate(CACHE_MAP_L2_CACHE, address, bytes);
        l1 == 0 && l2 == 0
    }
}

/// Powers, identifies, tunes and maps the Tab5's Hex-DDR PSRAM.
///
/// A failure is reported over USB serial and returns `None`; it never leaves a
/// partially verified framebuffer for DMA to consume.
#[inline(never)]
#[unsafe(export_name = "iram_psram_init")]
#[unsafe(link_section = ".iram.text.critical.psram")]
pub fn init() -> Option<Psram> {
    let interrupt_state = disable_machine_interrupts();
    let result = init_critical();
    restore_machine_interrupts(interrupt_state);
    result
}

#[inline(always)]
fn disable_machine_interrupts() -> usize {
    let previous: usize;
    unsafe {
        core::arch::asm!(
            "csrrc {previous}, mstatus, {mask}",
            previous = out(reg) previous,
            mask = in(reg) 1usize << 3,
            options(nomem, nostack),
        );
    }
    previous
}

#[inline(always)]
fn restore_machine_interrupts(previous: usize) {
    if previous & (1 << 3) != 0 {
        unsafe {
            core::arch::asm!(
                "csrs mstatus, {mask}",
                mask = in(reg) 1usize << 3,
                options(nomem, nostack),
            );
        }
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn zero_memory(bytes: *mut u8, length: usize) {
    for index in 0..length {
        unsafe { bytes.add(index).write_volatile(0) };
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn bytes_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    for index in 0..left.len() {
        let left_byte = unsafe { left.as_ptr().add(index).read_volatile() };
        let right_byte = unsafe { right.as_ptr().add(index).read_volatile() };
        if left_byte != right_byte {
            return false;
        }
    }
    true
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn init_critical() -> Option<Psram> {
    let force_fallback = take_fallback_test_request();
    match init_profile(&PSRAM_200_MHZ, force_fallback) {
        Ok((psram, phase, data_delay, dqs_delay)) => {
            log_ready(&PSRAM_200_MHZ, phase, data_delay, dqs_delay);
            Some(psram)
        }
        Err(stage) => {
            uart::log_hex(&FALLBACK_STAGE_LABEL, stage as u32);
            uart::log(&FALLBACK_MESSAGE);
            match init_profile(&PSRAM_80_MHZ, false) {
                Ok((psram, phase, data_delay, dqs_delay)) => {
                    log_ready(&PSRAM_80_MHZ, phase, data_delay, dqs_delay);
                    Some(psram)
                }
                Err(_) => None,
            }
        }
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn init_profile(
    timing: &PsramTiming,
    force_tuning_failure: bool,
) -> Result<(Psram, u8, u8, u8), InitStage> {
    enable_power_and_clock(timing)?;
    configure_pins();
    configure_device_timing();

    let mut mr01 = AlignedBytes([0u8; 2]);
    let mut mr23 = AlignedBytes([0u8; 2]);
    let mut mr48 = AlignedBytes([0u8; 2]);
    if !common_read(REG_READ, 0, timing.register_read_dummy_bits, &mut mr01.0)
        || !common_read(REG_READ, 2, timing.register_read_dummy_bits, &mut mr23.0)
        || !common_read(REG_READ, 4, timing.register_read_dummy_bits, &mut mr48.0)
    {
        uart::log(&MODE_REGISTER_TRANSACTION_FAILED);
        return Err(InitStage::ModeRegisterRead);
    }
    // Preserve vendor-defined bits while selecting the profile's fixed read
    // and write latency plus the 2048-byte x16 linear burst used by IDF.
    mr01.0[0] = (mr01.0[0] & !0x3F) | (timing.mr0_read_latency_code << 2) | (1 << 5);
    mr48.0[0] = (mr48.0[0] & 0x1F) | (timing.mr4_write_latency_code << 5);
    if !common_write(REG_WRITE, 0, 0, &mr01.0) || !common_write(REG_WRITE, 4, 0, &mr48.0) {
        uart::log(&MODE_REGISTER_WRITE_FAILED);
        return Err(InitStage::ModeRegisterWrite);
    }

    let mut mr8 = AlignedBytes([0u8; 2]);
    if !common_read(
        REG_READ,
        8,
        timing.register_read_dummy_bits,
        &mut mr8.0[..1],
    ) {
        uart::log(&MR8_READ_FAILED);
        return Err(InitStage::Mr8Read);
    }
    mr8.0[0] = (mr8.0[0] & !0x4F) | 3 | (1 << 3) | (1 << 6);
    if !common_write(REG_WRITE, 8, 0, &mr8.0) {
        uart::log(&MR8_WRITE_FAILED);
        return Err(InitStage::Mr8Write);
    }

    let reference = AlignedBytes(0x5A6B_7C8Du32.to_le_bytes());
    let mut received = AlignedBytes([0u8; 4]);
    if !common_write(SYNC_WRITE, 0, timing.write_dummy_bits, &reference.0)
        || !common_read(SYNC_READ, 0, timing.read_dummy_bits, &mut received.0)
    {
        uart::log(&COMMAND_TRANSACTION_TIMEOUT);
        return Err(InitStage::CommandPath);
    }
    if received.0 != reference.0 {
        uart::log_hex(&COMMAND_READ_LABEL, u32::from_le_bytes(received.0));
        uart::log(&COMMAND_TEST_FAILED);
        return Err(InitStage::CommandPath);
    }

    let (phase, data_delay, dqs_delay) = match tune_dqs(timing) {
        Some(point) => point,
        None => return Err(InitStage::Tuning),
    };
    if force_tuning_failure {
        uart::log(&FORCED_FALLBACK);
        return Err(InitStage::Tuning);
    }
    if !direct_memory_test(timing) {
        uart::log(&DIRECT_TEST_FAILED);
        return Err(InitStage::DirectMemoryTest);
    }

    configure_cache_access(timing);
    let map: unsafe extern "C" fn(u32, u32, u32, u32, u32, u32) -> i32 =
        unsafe { transmute(ROM_CACHE_PSRAM_MMU_SET) };
    let result = unsafe {
        map(
            0,
            PSRAM_VADDR as u32,
            0,
            (PAGE_BYTES / 1024) as u32,
            (MAPPED_BYTES / PAGE_BYTES) as u32,
            0,
        )
    };
    if result != 0 {
        uart::log_hex(&MMU_ERROR_LABEL, result as u32);
        return Err(InitStage::MmuMap);
    }

    // A CPU-only reset leaves L1/L2 cache lines for the former PSRAM mapping
    // intact even though the MSPI controller was reset. Drop them before the
    // first access through the newly programmed MMU window.
    invalidate_mapped_cache();

    if !mapped_memory_test() {
        uart::log(&MAPPED_TEST_FAILED);
        return Err(InitStage::MappedMemoryTest);
    }

    let psram = Psram {
        base: PSRAM_VADDR,
        bytes: MAPPED_BYTES,
        frequency_mhz: timing.frequency_mhz,
    };
    Ok((psram, phase, data_delay, dqs_delay))
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn take_fallback_test_request() -> bool {
    let requested = unsafe { read(LP_STORE15) } == FALLBACK_TEST_MAGIC;
    if requested {
        unsafe { write(LP_STORE15, 0) };
    }
    requested
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn log_ready(timing: &PsramTiming, phase: u8, data_delay: u8, dqs_delay: u8) {
    uart::log_hex(&PROFILE_MHZ_LABEL, timing.frequency_mhz);
    uart::log_hex(&READ_LATENCY_LABEL, timing.read_latency_cycles);
    uart::log_hex(&WRITE_LATENCY_LABEL, timing.write_latency_cycles);
    uart::log_hex(&DQS_PHASE_LABEL, phase as u32);
    uart::log_hex(&DQS_DATA_DELAY_LABEL, data_delay as u32);
    uart::log_hex(&DQS_DELAY_LABEL, dqs_delay as u32);
    uart::log(&READY_MESSAGE);
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn enable_power_and_clock(timing: &PsramTiming) -> Result<(), InitStage> {
    unsafe {
        // Tab5 connects PSRAM VDD and the optional MPLL domain to LDO channel 2.
        // Channel 2 is unit 1 / ext_ldo[3].  Nominal ECO2 1.8 V is dref=6,mul=5.
        write(PMU + 0x1D0, 0x4020_0180);
        write(PMU + 0x1D4, (6 << 28) | (5 << 23));
        // The dedicated MSPI PHY is powered separately from the digital MSPI
        // registers. IDF enables it as part of rtc_clk_mpll_enable(), even before
        // it programs or selects MPLL as the PSRAM clock source.
        modify(PMU + 0x15C, 1 << 24, 1 << 24);
        modify(LP_CLKRST + 0x40, 1 << 28, 1 << 28);
    }
    delay();

    if matches!(timing.clock_source, PsramClockSource::Mpll400Mhz) && !configure_mpll_400mhz() {
        return Err(InitStage::Clock);
    }

    unsafe {
        // Enable the PSRAM system clock before releasing its controller reset.
        modify(HP_SYS_CLKRST + 0x14, 1 << 31, 1 << 31);
    }

    // `startup::reboot` resets only the HP CPU core, so a prior direct MSPI
    // command can otherwise leave MSPI3 busy across a reboot. The ROM command
    // configuration helper waits for that busy bit without a timeout. Reset
    // both sides of the dual-MSPI block before programming it again.
    reset_mspi();

    match timing.clock_source {
        PsramClockSource::Spll480Mhz => unsafe {
            // Select the already-running 480 MHz SPLL and divide its core by
            // one. The two bus dividers below produce the profile frequency.
            modify(
                HP_SYS_CLKRST + 0x30,
                (3 << 12) | (1 << 14) | (1 << 15) | (0xFF << 16),
                (2 << 12) | (1 << 14) | (1 << 15),
            );
        },
        PsramClockSource::Mpll400Mhz => unsafe {
            // ESP-IDF selects MPLL with source value 1 after calibrating it
            // to 400 MHz. Keep both the PLL and PSRAM core clocks enabled.
            modify(
                HP_SYS_CLKRST + 0x30,
                (3 << 12) | (1 << 14) | (1 << 15) | (0xFF << 16),
                (1 << 12) | (1 << 14) | (1 << 15),
            );
        },
    }
    set_bus_divider(MSPI2 + 0x50, timing.operating_bus_divider);
    set_bus_divider(MSPI3 + 0x14, timing.operating_bus_divider);
    unsafe {
        modify(MSPI2 + 0x190, 1 << 5, 1 << 5);
        modify(MSPI2 + 0x180, 1 << 5, 1 << 5);
        modify(MSPI3 + 0x200, 1, 1);
    }
    Ok(())
}

/// Configures the ESP32-P4 media PLL exactly as ESP-IDF v5.5.3 does for
/// 200 MHz Hex PSRAM: 40 MHz XTAL * (19 + 1) / (1 + 1) = 400 MHz.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn configure_mpll_400mhz() -> bool {
    const MPLL_BLOCK: u8 = 0x63;
    const MPLL_CAL_RSTB_REG: u8 = 1;
    const MPLL_DIV_REG: u8 = 2;
    const MPLL_DHREF_REG: u8 = 3;
    const MPLL_CAL_RSTB: u8 = 1 << 5;
    const MPLL_DIV_400MHZ: u8 = (19 << 3) | 1;
    const MPLL_CAL_END: u32 = 1 << 8;
    const MPLL_CAL_STOP: u32 = 1 << 9;

    unsafe {
        // The bootloader normally leaves this clock enabled. Force it on so
        // CPU-only reboot and future bootloader changes cannot break regi2c.
        modify(LPPERI, 1 << 27, 1 << 27);
        modify(I2C_ANA_MST + 0x34, 1, 1);

        // Clearing CAL_STOP starts MPLL self-calibration.
        modify(HP_SYS_CLKRST + 0xBC, MPLL_CAL_STOP, 0);
    }

    let Some(dhref) = regi2c_read(MPLL_BLOCK, MPLL_DHREF_REG) else {
        uart::log(&MPLL_READBACK_FAILED);
        return false;
    };
    if !regi2c_write(MPLL_BLOCK, MPLL_DHREF_REG, dhref | (3 << 4)) {
        uart::log(&MPLL_READBACK_FAILED);
        return false;
    }
    let Some(cal_rstb) = regi2c_read(MPLL_BLOCK, MPLL_CAL_RSTB_REG) else {
        uart::log(&MPLL_READBACK_FAILED);
        return false;
    };
    if !regi2c_write(MPLL_BLOCK, MPLL_CAL_RSTB_REG, cal_rstb & !MPLL_CAL_RSTB)
        || !regi2c_write(MPLL_BLOCK, MPLL_CAL_RSTB_REG, cal_rstb | MPLL_CAL_RSTB)
        || !regi2c_write(MPLL_BLOCK, MPLL_DIV_REG, MPLL_DIV_400MHZ)
    {
        uart::log(&MPLL_READBACK_FAILED);
        return false;
    }

    let mut timeout = 5_000_000u32;
    while unsafe { read(HP_SYS_CLKRST + 0xBC) } & MPLL_CAL_END == 0 {
        if timeout == 0 {
            unsafe { modify(HP_SYS_CLKRST + 0xBC, MPLL_CAL_STOP, MPLL_CAL_STOP) };
            uart::log(&MPLL_TIMEOUT);
            return false;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
    unsafe { modify(HP_SYS_CLKRST + 0xBC, MPLL_CAL_STOP, MPLL_CAL_STOP) };

    if regi2c_read(MPLL_BLOCK, MPLL_DIV_REG) != Some(MPLL_DIV_400MHZ) {
        uart::log(&MPLL_READBACK_FAILED);
        return false;
    }
    true
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn regi2c_select_mpll() {
    unsafe {
        // ESP32-P4's analog master routes MPLL block 0x63 through bit 9.
        modify(I2C_ANA_MST + 0x1C, 0x00FF_FFFF, 0);
        modify(I2C_ANA_MST + 0x20, 0x00FF_FFFF, 1 << 9);
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn regi2c_wait_idle() -> bool {
    let mut timeout = 1_000_000u32;
    while unsafe { read(I2C_ANA_MST) } & (1 << 25) != 0 {
        if timeout == 0 {
            return false;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
    true
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn regi2c_read(block: u8, register: u8) -> Option<u8> {
    regi2c_select_mpll();
    if !regi2c_wait_idle() {
        return None;
    }
    unsafe {
        write(I2C_ANA_MST, (block as u32) | ((register as u32) << 8));
    }
    if !regi2c_wait_idle() {
        return None;
    }
    Some((unsafe { read(I2C_ANA_MST) } >> 16) as u8)
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn regi2c_write(block: u8, register: u8, value: u8) -> bool {
    regi2c_select_mpll();
    if !regi2c_wait_idle() {
        return false;
    }
    unsafe {
        write(
            I2C_ANA_MST,
            (block as u32) | ((register as u32) << 8) | ((value as u32) << 16) | (1 << 24),
        );
    }
    regi2c_wait_idle()
}

/// Resets the shared AXI and APB portions of the dual-MSPI controller.
///
/// The ordering matches ESP-IDF's PSRAM controller reset: assert AXI, assert
/// APB, then release APB before AXI.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn reset_mspi() {
    // ESP32-P4 has separate reset bits for the flash MSPI (22/24) and the
    // dual-MSPI PSRAM controller (23/25). Reset only the latter: resetting
    // 22/24 destroys the bootloader's flash setup and makes the next uncached
    // DROM/IROM access stall even though already-cached probe lines still work.
    const RST_EN_DUAL_MSPI_AXI: u32 = 1 << 23;
    const RST_EN_DUAL_MSPI_APB: u32 = 1 << 25;

    unsafe {
        modify(HP_RST_EN0, RST_EN_DUAL_MSPI_AXI, RST_EN_DUAL_MSPI_AXI);
        modify(HP_RST_EN0, RST_EN_DUAL_MSPI_APB, RST_EN_DUAL_MSPI_APB);
        modify(HP_RST_EN0, RST_EN_DUAL_MSPI_APB, 0);
        modify(HP_RST_EN0, RST_EN_DUAL_MSPI_AXI, 0);
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn invalidate_mapped_cache() {
    let invalidate: unsafe extern "C" fn(u32, u32, u32) =
        unsafe { transmute(ROM_CACHE_INVALIDATE_ADDR) };
    unsafe {
        // The PSRAM mapping is external to the CPU, so both cache levels must
        // be invalidated together. This matches ESP-IDF's cache_invalidate_addr.
        invalidate(
            CACHE_MAP_L1_DCACHE | CACHE_MAP_L2_CACHE,
            PSRAM_VADDR as u32,
            MAPPED_BYTES as u32,
        );
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn configure_pins() {
    unsafe {
        // D0..D7, CLK, CS and D8..D15 have the same two-bit drive field.
        for offset in (0x1C..=0x38).step_by(4) {
            modify(MSPI_IOMUX + offset, 3 << 12, 2 << 12);
        }
        for offset in (0x40..=0x64).step_by(4) {
            modify(MSPI_IOMUX + offset, 3 << 12, 2 << 12);
        }
        for offset in [0x3C, 0x68] {
            modify(MSPI_IOMUX + offset, (3 << 15) | 1, (2 << 15) | 1);
        }
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn configure_device_timing() {
    unsafe {
        // CS setup=4, hold=4, hold-delay=3, split transfers, 2048-byte pages.
        modify(
            MSPI2 + 0x1A0,
            0x7FF | (0x3F << 25) | (1 << 31),
            1 | (1 << 1) | (3 << 2) | (3 << 7) | (2 << 25) | (1 << 31),
        );
        modify(MSPI2 + 0x174, 3 << 18, 3 << 18);
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn configure_cache_access(timing: &PsramTiming) {
    let cache_sctrl = 1
        | (1 << 3)
        | (1 << 4)
        | (1 << 5)
        | ((timing.read_dummy_bits - 1) << 6)
        | (31 << 14)
        | (1 << 20)
        | (1 << 21)
        | ((timing.write_dummy_bits - 1) << 22);
    unsafe {
        write(MSPI2 + 0x40, cache_sctrl);
        modify(
            MSPI2 + 0x44,
            (0x3F << 18) | (3 << 26),
            // OPI command/address, HEX data and write-dummy output. Bit 22
            // (`sdummy_rin`) must remain clear for the DDR PSRAM path.
            0b1100_1011_1100 << 16,
        );
        write(MSPI2 + 0x48, (15 << 28) | SYNC_READ);
        write(MSPI2 + 0x4C, (15 << 28) | SYNC_WRITE);
        modify(MSPI2 + 0xD8, 0xF, 3);
        // MEM_CTRL1: keep AXI read/write bursts spliced across PSRAM pages.
        modify(MSPI2 + 0x70, (1 << 25) | (1 << 26), (1 << 25) | (1 << 26));
        modify(MSPI2 + 0x3C, (1 << 31) | 1, 1);
        // Cache and command engines both use fixed-latency mode after tuning.
        modify(MSPI3 + 0xD4, 1 << 1, 1 << 1);
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn tune_dqs(timing: &PsramTiming) -> Option<(u8, u8, u8)> {
    // The tested Tab5 repeatedly selected the centre point below. Validate it
    // with the same 100 reads used for every full-sweep candidate and avoid
    // scanning all 31 delay points when it is still stable. Any failure falls
    // through to the complete ESP-IDF-style tuning sweep.
    let reference =
        unsafe { core::slice::from_raw_parts(TUNING_REFERENCE_WORDS.as_ptr() as *const u8, 128) };

    // Write the reference at the profile's conservative tuning clock with all
    // delays cleared before switching back to the operating divider.
    set_bus_divider(MSPI2 + 0x50, timing.tuning_bus_divider);
    set_bus_divider(MSPI3 + 0x14, timing.tuning_bus_divider);
    clear_tuning();
    for chunk in 0..2 {
        if !common_write(
            SYNC_WRITE,
            0x80 + chunk * 64,
            timing.write_dummy_bits,
            &reference[chunk as usize * 64..chunk as usize * 64 + 64],
        ) {
            uart::log(&TUNING_WRITE_TIMEOUT);
            return None;
        }
    }
    set_bus_divider(MSPI2 + 0x50, timing.operating_bus_divider);
    set_bus_divider(MSPI3 + 0x14, timing.operating_bus_divider);
    unsafe {
        modify(MSPI3 + 0xD4, 1 << 1, 0);
    }

    if timing.use_preferred_dqs {
        set_dqs_phase(timing.preferred_dqs_phase);
        set_all_delays(timing.preferred_data_delay, timing.preferred_dqs_delay);
        if reference_matches(timing, reference, 100) {
            unsafe {
                modify(MSPI3 + 0xD4, 1 << 1, 1 << 1);
            }
            return Some((
                timing.preferred_dqs_phase,
                timing.preferred_data_delay,
                timing.preferred_dqs_delay,
            ));
        }
        uart::log(&PREFERRED_DQS_FAILED);
    }

    let mut phase_storage = core::mem::MaybeUninit::<[bool; 4]>::uninit();
    zero_memory(phase_storage.as_mut_ptr().cast::<u8>(), 4);
    let phase_good = unsafe { phase_storage.assume_init_mut() };
    for phase in 0..4u8 {
        set_dqs_phase(phase);
        phase_good[phase as usize] = reference_matches(timing, reference, 1);
    }
    let (phase_len, phase_end) = longest_run(phase_good);
    if phase_len == 0 {
        uart::log(&NO_VALID_DQS_PHASE);
        return None;
    }
    let best_phase = (phase_end + 1 - phase_len) as u8;

    let mut delay_storage = core::mem::MaybeUninit::<[bool; 31]>::uninit();
    zero_memory(delay_storage.as_mut_ptr().cast::<u8>(), 31);
    let delay_good = unsafe { delay_storage.assume_init_mut() };
    for index in 0..31u8 {
        let (data, dqs) = delay_pair(index);
        set_dqs_phase(best_phase);
        set_all_delays(data, dqs);
        delay_good[index as usize] = reference_matches(timing, reference, 100);
    }
    let (delay_len, delay_end) = longest_run(delay_good);
    if delay_len <= 1 {
        uart::log(&NO_STABLE_DQS_WINDOW);
        return None;
    }
    let best_index = delay_end - delay_len / 2;
    let (best_data, best_dqs) = delay_pair(best_index as u8);
    set_dqs_phase(best_phase);
    set_all_delays(best_data, best_dqs);
    unsafe {
        modify(MSPI3 + 0xD4, 1 << 1, 1 << 1);
    }
    uart::log_hex(&DQS_WINDOW_START_LABEL, (delay_end + 1 - delay_len) as u32);
    uart::log_hex(&DQS_WINDOW_LENGTH_LABEL, delay_len as u32);
    Some((best_phase, best_data, best_dqs))
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn reference_matches(timing: &PsramTiming, reference: &[u8], attempts: usize) -> bool {
    let mut received_storage = core::mem::MaybeUninit::<AlignedBytes<128>>::uninit();
    zero_memory(received_storage.as_mut_ptr().cast::<u8>(), 128);
    let received = unsafe { received_storage.assume_init_mut() };
    for _ in 0..attempts {
        for chunk in 0..2 {
            if !common_read(
                SYNC_READ,
                0x80 + chunk * 64,
                timing.read_dummy_bits,
                &mut received.0[chunk as usize * 64..chunk as usize * 64 + 64],
            ) {
                return false;
            }
        }
        if !bytes_equal(&received.0, reference) {
            return false;
        }
    }
    true
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn delay_pair(index: u8) -> (u8, u8) {
    if index < 16 {
        (0, 15 - index)
    } else {
        (index - 15, 0)
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn longest_run(values: &[bool]) -> (usize, usize) {
    let mut best_len = 0;
    let mut best_end = 0;
    let mut current = 0;
    for (index, &good) in values.iter().enumerate() {
        if good {
            current += 1;
            if current > best_len {
                best_len = current;
                best_end = index;
            }
        } else {
            current = 0;
        }
    }
    (best_len, best_end)
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn clear_tuning() {
    set_dqs_phase(0);
    set_all_delays(0, 0);
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn set_dqs_phase(phase: u8) {
    unsafe {
        for offset in [0x3C, 0x68] {
            modify(MSPI_IOMUX + offset, 3 << 1, (phase as u32) << 1);
        }
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn set_all_delays(data: u8, dqs: u8) {
    unsafe {
        for offset in (0x1C..=0x38).step_by(4) {
            modify(MSPI_IOMUX + offset, 0xF << 4, (data as u32) << 4);
        }
        for offset in (0x40..=0x64).step_by(4) {
            modify(MSPI_IOMUX + offset, 0xF << 4, (data as u32) << 4);
        }
        for offset in [0x3C, 0x68] {
            modify(
                MSPI_IOMUX + offset,
                (0xF << 7) | (0xF << 17),
                ((dqs as u32) << 7) | ((dqs as u32) << 17),
            );
        }
    }
}

/// Exercises the tuned direct-command path at both ends and across the two
/// framebuffer/heap regions before the cache MMU is enabled.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn direct_memory_test(timing: &PsramTiming) -> bool {
    let locations = [
        0u32,
        PAGE_BYTES as u32 - 4,
        FRAMEBUFFER_BYTES as u32 - 4,
        FRAMEBUFFER_BYTES as u32,
        (MAPPED_BYTES / 2) as u32,
        MAPPED_BYTES as u32 - 4,
    ];
    for round in 0..16u32 {
        let walking = 1u32 << ((round * 2) & 31);
        let pattern = if round & 1 == 0 { walking } else { !walking };
        for (index, &address) in locations.iter().enumerate() {
            let expected = pattern ^ address.rotate_left(index as u32);
            let bytes = AlignedBytes(expected.to_le_bytes());
            if !common_write(SYNC_WRITE, address, timing.write_dummy_bits, &bytes.0) {
                return false;
            }
        }
        for (index, &address) in locations.iter().enumerate() {
            let expected = pattern ^ address.rotate_left(index as u32);
            let mut bytes = AlignedBytes([0u8; 4]);
            if !common_read(SYNC_READ, address, timing.read_dummy_bits, &mut bytes.0)
                || u32::from_le_bytes(bytes.0) != expected
            {
                uart::log_hex(&BAD_ADDRESS_LABEL, address);
                uart::log_hex(&BAD_VALUE_LABEL, u32::from_le_bytes(bytes.0));
                return false;
            }
        }
    }
    true
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn mapped_memory_test() -> bool {
    let locations = [
        PSRAM_VADDR,
        PSRAM_VADDR + PAGE_BYTES - 4,
        PSRAM_VADDR + FRAMEBUFFER_BYTES - 4,
        PSRAM_VADDR + FRAMEBUFFER_BYTES,
        PSRAM_VADDR + MAPPED_BYTES / 2,
        PSRAM_VADDR + MAPPED_BYTES - 4,
    ];
    for round in 0..8u32 {
        let walking = 1u32 << ((round * 4) & 31);
        let pattern = if round & 1 == 0 { walking } else { !walking };
        for (index, &location) in locations.iter().enumerate() {
            let expected = pattern ^ (location as u32).rotate_left(index as u32);
            unsafe { (location as *mut u32).write_volatile(expected) };
            let line = location & !(CACHE_LINE_BYTES - 1);
            if !iram_cache_writeback_invalidate(line as u32, CACHE_LINE_BYTES as u32) {
                return false;
            }
        }
        for (index, &location) in locations.iter().enumerate() {
            let expected = pattern ^ (location as u32).rotate_left(index as u32);
            let value = unsafe { (location as *const u32).read_volatile() };
            if value != expected {
                uart::log_hex(&BAD_ADDRESS_LABEL, location as u32);
                uart::log_hex(&BAD_VALUE_LABEL, value);
                return false;
            }
        }
    }
    true
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn common_read(command: u32, address: u32, dummy_bits: u32, output: &mut [u8]) -> bool {
    transaction(command, address, dummy_bits, None, Some(output))
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn common_write(command: u32, address: u32, dummy_bits: u32, input: &[u8]) -> bool {
    transaction(command, address, dummy_bits, Some(input), None)
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn transaction(
    command: u32,
    mut address: u32,
    dummy_bits: u32,
    input: Option<&[u8]>,
    output: Option<&mut [u8]>,
) -> bool {
    let (tx_data, tx_bits) = match input {
        Some(bytes) => (bytes.as_ptr() as *mut u32, (bytes.len() * 8) as u32),
        None => (core::ptr::null_mut(), 0),
    };
    let (rx_data, rx_bits, rx_bytes, rx_len) = match output {
        Some(bytes) => (
            bytes.as_mut_ptr() as *mut u32,
            (bytes.len() * 8) as u32,
            bytes.as_mut_ptr(),
            bytes.len(),
        ),
        None => (core::ptr::null_mut(), 0, core::ptr::null_mut(), 0),
    };
    let mut config = RomSpiCommand {
        command: command as u16,
        command_bits: 16,
        address: &mut address,
        address_bits: 32,
        tx_data,
        tx_bits,
        rx_data,
        rx_bits,
        dummy_bits,
    };
    let set_mode: unsafe extern "C" fn(i32, i32) = unsafe { transmute(ROM_SPI_SET_OP_MODE) };
    let configure: unsafe extern "C" fn(i32, *mut RomSpiCommand) =
        unsafe { transmute(ROM_SPI_CMD_CONFIG) };

    unsafe {
        // `esp_rom_spi_cmd_config` has its own unbounded `while (CMD != 0)`
        // loop. Check the identical condition first so a residual command
        // after a CPU-only restart degrades to the PSRAM fallback instead of
        // permanently wedging the boot.
        if !wait_for_mspi3_idle() {
            uart::log(&MSPI3_BUSY);
            log_mspi3_state();
            return false;
        }

        // ESP_ROM_SPIFLASH_OPI_DTR_MODE = 7; PSRAM is on CS1.
        set_mode(3, 7);
        configure(3, &mut config);

        // The ROM start helper waits forever if the command engine has no clock.
        // Select CS1 and perform the same trigger with a bounded poll instead.
        modify(MSPI3 + 0x34, 3, 1);
        modify(MSPI3, 1 << 18, 1 << 18);
        let mut timeout = 5_000_000u32;
        while read(MSPI3) & (1 << 18) != 0 {
            if timeout == 0 {
                uart::log(&MSPI3_TIMEOUT);
                log_mspi3_state();
                return false;
            }
            timeout -= 1;
        }

        if rx_len != 0 {
            for index in 0..rx_len {
                let word = read(MSPI3 + 0x58 + (index / 4) * 4);
                rx_bytes.add(index).write((word >> ((index % 4) * 8)) as u8);
            }
        }
    }
    true
}

/// Checks the exact idle condition that `esp_rom_spi_cmd_config` waits for,
/// but with a finite timeout.
///
/// # Safety
/// MSPI3 must be a valid MMIO block clocked for CPU access.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
unsafe fn wait_for_mspi3_idle() -> bool {
    let mut timeout = 5_000_000u32;
    while unsafe { read(MSPI3) } != 0 {
        if timeout == 0 {
            return false;
        }
        timeout -= 1;
        core::hint::spin_loop();
    }
    true
}

/// # Safety
/// MSPI3, the clock-control block and PMU must be valid MMIO blocks.
#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
unsafe fn log_mspi3_state() {
    unsafe {
        uart::log_hex(&MSPI3_CMD_LABEL, read(MSPI3));
        uart::log_hex(&MSPI3_CLOCK_LABEL, read(MSPI3 + 0x14));
        uart::log_hex(&MSPI3_USER_LABEL, read(MSPI3 + 0x18));
        uart::log_hex(&MSPI3_MISC_LABEL, read(MSPI3 + 0x34));
        uart::log_hex(&HP_CLK_LABEL, read(HP_SYS_CLKRST + 0x30));
        uart::log_hex(&LDO2_LABEL, read(PMU + 0x1D0));
        uart::log_hex(&LDO2_ANA_LABEL, read(PMU + 0x1D4));
        uart::log_hex(&RF_PWC_LABEL, read(PMU + 0x15C));
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn set_bus_divider(register: usize, divider: u32) {
    let value = ((divider - 1) << 16) | ((divider / 2 - 1) << 8) | (divider - 1);
    unsafe {
        write(register, value);
    }
}

#[inline(never)]
#[unsafe(link_section = ".iram.text.critical.psram")]
fn delay() {
    for _ in 0..200_000 {
        core::hint::spin_loop();
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
