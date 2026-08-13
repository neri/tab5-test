//! Minimal ESP32-P4 ECO2 Hex-DDR PSRAM bring-up.
//!
//! Maps the Tab5's full 32 MiB PSRAM at `0x4800_0000` (well within the chip's
//! 64 MiB PSRAM MMU window) and verifies the mapping before returning it to
//! the caller. The first two framebuffer-sized slots back the LCD module;
//! [`Psram::heap`] exposes everything after them for a global allocator.

use core::mem::transmute;

use crate::uart;

pub const WIDTH: usize = 720;
pub const HEIGHT: usize = 1280;
pub const BYTES_PER_PIXEL: usize = 2;
pub const FRAMEBUFFER_BYTES: usize = WIDTH * HEIGHT * BYTES_PER_PIXEL;
pub const FRAMEBUFFER_COUNT: usize = 2;

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

const ROM_SPI_CMD_CONFIG: usize = 0x4FC0_0108;
const ROM_SPI_SET_OP_MODE: usize = 0x4FC0_0110;
const ROM_CACHE_PSRAM_MMU_SET: usize = 0x4FC0_0520;
const ROM_CACHE_INVALIDATE_ADDR: usize = 0x4FC0_03E4;
const ROM_CACHE_WRITEBACK_INVALIDATE_ADDR: usize = 0x4FC0_03FC;

const SYNC_READ: u32 = 0x0000;
const SYNC_WRITE: u32 = 0x8080;
const REG_READ: u32 = 0x4040;
const REG_WRITE: u32 = 0xC0C0;
const READ_DUMMY_BITS: u32 = 18;
const REG_READ_DUMMY_BITS: u32 = 8;
const WRITE_DUMMY_BITS: u32 = 8;

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

#[derive(Clone, Copy)]
pub struct Psram {
    base: usize,
    bytes: usize,
}

impl Psram {
    pub fn framebuffer(&self, index: usize) -> Option<*mut u16> {
        if index >= FRAMEBUFFER_COUNT {
            return None;
        }
        let offset = index * FRAMEBUFFER_BYTES;
        if offset + FRAMEBUFFER_BYTES > self.bytes {
            return None;
        }
        Some((self.base + offset) as *mut u16)
    }

    /// Returns the PSRAM span after the two framebuffers, for use as a heap.
    pub fn heap(&self) -> (*mut u8, usize) {
        let offset = FRAMEBUFFER_COUNT * FRAMEBUFFER_BYTES;
        ((self.base + offset) as *mut u8, self.bytes - offset)
    }

    /// Writes back a bounded byte range within one framebuffer.
    pub fn writeback_range(&self, index: usize, offset: usize, bytes: usize) -> bool {
        let Some(framebuffer) = self.framebuffer(index) else {
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
        let writeback_invalidate: unsafe extern "C" fn(u32, u32, u32) -> i32 =
            unsafe { transmute(ROM_CACHE_WRITEBACK_INVALIDATE_ADDR) };
        unsafe {
            // Push dirty L1 lines into L2 first, then push L2 into PSRAM.
            let l1 = writeback_invalidate(CACHE_MAP_L1_DCACHE, address as u32, length as u32);
            let l2 = writeback_invalidate(CACHE_MAP_L2_CACHE, address as u32, length as u32);
            l1 == 0 && l2 == 0
        }
    }
}

/// Powers, identifies, tunes and maps the Tab5's Hex-DDR PSRAM.
///
/// A failure is reported over USB serial and returns `None`; it never leaves a
/// partially verified framebuffer for DMA to consume.
pub fn init() -> Option<Psram> {
    enable_power_and_clock();
    configure_pins();
    configure_device_timing();

    let mut mr01 = AlignedBytes([0u8; 2]);
    let mut mr23 = AlignedBytes([0u8; 2]);
    let mut mr48 = AlignedBytes([0u8; 2]);
    if !common_read(REG_READ, 0, REG_READ_DUMMY_BITS, &mut mr01.0)
        || !common_read(REG_READ, 2, REG_READ_DUMMY_BITS, &mut mr23.0)
        || !common_read(REG_READ, 4, REG_READ_DUMMY_BITS, &mut mr48.0)
    {
        uart::log(b"PSRAM: mode-register transaction failed\r\n");
        return None;
    }
    // Preserve vendor-defined bits while selecting fixed latency, 10-cycle
    // reads, 5-cycle writes and the 2048-byte x16 linear burst used by IDF.
    mr01.0[0] = (mr01.0[0] & !0x3F) | (2 << 2) | (1 << 5);
    mr48.0[0] = (mr48.0[0] & 0x1F) | (2 << 5);
    if !common_write(REG_WRITE, 0, 0, &mr01.0) || !common_write(REG_WRITE, 4, 0, &mr48.0) {
        uart::log(b"PSRAM: mode-register write failed\r\n");
        return None;
    }

    let mut mr8 = AlignedBytes([0u8; 2]);
    if !common_read(REG_READ, 8, REG_READ_DUMMY_BITS, &mut mr8.0[..1]) {
        uart::log(b"PSRAM: MR8 read failed\r\n");
        return None;
    }
    mr8.0[0] = (mr8.0[0] & !0x4F) | 3 | (1 << 3) | (1 << 6);
    if !common_write(REG_WRITE, 8, 0, &mr8.0) {
        uart::log(b"PSRAM: MR8 write failed\r\n");
        return None;
    }

    let reference = AlignedBytes(0x5A6B_7C8Du32.to_le_bytes());
    let mut received = AlignedBytes([0u8; 4]);
    if !common_write(SYNC_WRITE, 0, WRITE_DUMMY_BITS, &reference.0)
        || !common_read(SYNC_READ, 0, READ_DUMMY_BITS, &mut received.0)
    {
        uart::log(b"PSRAM: command-path transaction timed out\r\n");
        return None;
    }
    if received.0 != reference.0 {
        uart::log_hex(b"PSRAM: command read=", u32::from_le_bytes(received.0));
        uart::log(b"PSRAM: command-path test failed\r\n");
        return None;
    }

    tune_dqs()?;

    configure_cache_access();
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
        uart::log_hex(b"PSRAM: MMU error=", result as u32);
        return None;
    }

    // A CPU-only reset leaves L1/L2 cache lines for the former PSRAM mapping
    // intact even though the MSPI controller was reset. Drop them before the
    // first access through the newly programmed MMU window.
    invalidate_mapped_cache();

    if !mapped_memory_test() {
        uart::log(b"PSRAM: mapped memory test failed\r\n");
        return None;
    }

    let psram = Psram {
        base: PSRAM_VADDR,
        bytes: MAPPED_BYTES,
    };
    uart::log(b"PSRAM: ready (2 framebuffers + heap)\r\n");
    Some(psram)
}

fn enable_power_and_clock() {
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

    unsafe {
        // Enable the PSRAM system clock before releasing its controller reset.
        modify(HP_SYS_CLKRST + 0x14, 1 << 31, 1 << 31);
    }

    // `startup::reboot` resets only the HP CPU core, so a prior direct MSPI
    // command can otherwise leave MSPI3 busy across a reboot. The ROM command
    // configuration helper waits for that busy bit without a timeout. Reset
    // both sides of the dual-MSPI block before programming it again.
    reset_mspi();

    unsafe {
        // Select the already-running 480 MHz SPLL, divide its core by one, then
        // divide both PSRAM buses by six for an 80 MHz DDR clock.
        modify(
            HP_SYS_CLKRST + 0x30,
            (3 << 12) | (1 << 14) | (1 << 15) | (0xFF << 16),
            (2 << 12) | (1 << 14) | (1 << 15),
        );
    }
    set_bus_divider(MSPI2 + 0x50, 6);
    set_bus_divider(MSPI3 + 0x14, 6);
    unsafe {
        modify(MSPI2 + 0x190, 1 << 5, 1 << 5);
        modify(MSPI2 + 0x180, 1 << 5, 1 << 5);
        modify(MSPI3 + 0x200, 1, 1);
    }
}

/// Resets the shared AXI and APB portions of the dual-MSPI controller.
///
/// The ordering matches ESP-IDF's PSRAM controller reset: assert AXI, assert
/// APB, then release APB before AXI.
fn reset_mspi() {
    const RST_EN_MSPI_AXI: u32 = 1 << 22;
    const RST_EN_MSPI_APB: u32 = 1 << 24;

    unsafe {
        modify(HP_RST_EN0, RST_EN_MSPI_AXI, RST_EN_MSPI_AXI);
        modify(HP_RST_EN0, RST_EN_MSPI_APB, RST_EN_MSPI_APB);
        modify(HP_RST_EN0, RST_EN_MSPI_APB, 0);
        modify(HP_RST_EN0, RST_EN_MSPI_AXI, 0);
    }
}

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

fn configure_cache_access() {
    let cache_sctrl = 1
        | (1 << 3)
        | (1 << 4)
        | (1 << 5)
        | ((READ_DUMMY_BITS - 1) << 6)
        | (31 << 14)
        | (1 << 20)
        | (1 << 21)
        | ((WRITE_DUMMY_BITS - 1) << 22);
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

fn tune_dqs() -> Option<(u8, u8, u8)> {
    // The tested Tab5 repeatedly selected the centre point below. Validate it
    // with the same 100 reads used for every full-sweep candidate and avoid
    // scanning all 31 delay points when it is still stable. Any failure falls
    // through to the complete ESP-IDF-style tuning sweep.
    const PREFERRED_PHASE: u8 = 0;
    const PREFERRED_DATA_DELAY: u8 = 0;
    const PREFERRED_DQS_DELAY: u8 = 0;

    const REFERENCE_WORDS: [u32; 32] = [
        0x7f786655, 0xa5ff005a, 0x3f3c33aa, 0xa5ff5a00, 0x1f1e9955, 0xa5005aff, 0x0f0fccaa,
        0xa55a00ff, 0x07876655, 0xffa55a00, 0x03c333aa, 0xff00a55a, 0x01e19955, 0xff005aa5,
        0x00f0ccaa, 0xff5a00a5, 0x80786655, 0x00a5ff5a, 0xc03c33aa, 0x00a55aff, 0xe01e9355,
        0x00ff5aa5, 0xf00fccaa, 0x005affa5, 0xf8876655, 0x5aa5ff00, 0xfcc333aa, 0x5affa500,
        0xfee19955, 0x5a00a5ff, 0x11f0ccaa, 0x5a00ffa5,
    ];
    let reference =
        unsafe { core::slice::from_raw_parts(REFERENCE_WORDS.as_ptr() as *const u8, 128) };

    // Write the reference at 20 MHz with all tuning delays cleared.
    set_bus_divider(MSPI2 + 0x50, 24);
    set_bus_divider(MSPI3 + 0x14, 24);
    clear_tuning();
    for chunk in 0..2 {
        if !common_write(
            SYNC_WRITE,
            0x80 + chunk * 64,
            WRITE_DUMMY_BITS,
            &reference[chunk as usize * 64..chunk as usize * 64 + 64],
        ) {
            uart::log(b"PSRAM: tuning reference write timed out\r\n");
            return None;
        }
    }
    set_bus_divider(MSPI2 + 0x50, 6);
    set_bus_divider(MSPI3 + 0x14, 6);
    unsafe {
        modify(MSPI3 + 0xD4, 1 << 1, 0);
    }

    set_dqs_phase(PREFERRED_PHASE);
    set_all_delays(PREFERRED_DATA_DELAY, PREFERRED_DQS_DELAY);
    if reference_matches(reference, 100) {
        unsafe {
            modify(MSPI3 + 0xD4, 1 << 1, 1 << 1);
        }
        return Some((PREFERRED_PHASE, PREFERRED_DATA_DELAY, PREFERRED_DQS_DELAY));
    }
    uart::log(b"PSRAM: preferred DQS point failed; running full sweep\r\n");

    let mut phase_good = [false; 4];
    for phase in 0..4u8 {
        set_dqs_phase(phase);
        phase_good[phase as usize] = reference_matches(reference, 1);
    }
    let (phase_len, phase_end) = longest_run(&phase_good);
    if phase_len == 0 {
        uart::log(b"PSRAM: no valid DQS phase\r\n");
        return None;
    }
    let best_phase = (phase_end + 1 - phase_len) as u8;

    let mut delay_good = [false; 31];
    for index in 0..31u8 {
        let (data, dqs) = delay_pair(index);
        set_dqs_phase(best_phase);
        set_all_delays(data, dqs);
        delay_good[index as usize] = reference_matches(reference, 100);
    }
    let (delay_len, delay_end) = longest_run(&delay_good);
    if delay_len <= 1 {
        uart::log(b"PSRAM: no stable DQS delay window\r\n");
        return None;
    }
    let best_index = delay_end - delay_len / 2;
    let (best_data, best_dqs) = delay_pair(best_index as u8);
    set_dqs_phase(best_phase);
    set_all_delays(best_data, best_dqs);
    unsafe {
        modify(MSPI3 + 0xD4, 1 << 1, 1 << 1);
    }
    Some((best_phase, best_data, best_dqs))
}

fn reference_matches(reference: &[u8], attempts: usize) -> bool {
    let mut received = AlignedBytes([0u8; 128]);
    for _ in 0..attempts {
        for chunk in 0..2 {
            if !common_read(
                SYNC_READ,
                0x80 + chunk * 64,
                READ_DUMMY_BITS,
                &mut received.0[chunk as usize * 64..chunk as usize * 64 + 64],
            ) {
                return false;
            }
        }
        if received.0 != reference {
            return false;
        }
    }
    true
}

fn delay_pair(index: u8) -> (u8, u8) {
    if index < 16 {
        (0, 15 - index)
    } else {
        (index - 15, 0)
    }
}

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

fn clear_tuning() {
    set_dqs_phase(0);
    set_all_delays(0, 0);
}

fn set_dqs_phase(phase: u8) {
    unsafe {
        for offset in [0x3C, 0x68] {
            modify(MSPI_IOMUX + offset, 3 << 1, (phase as u32) << 1);
        }
    }
}

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

fn mapped_memory_test() -> bool {
    let locations = [
        PSRAM_VADDR,
        PSRAM_VADDR + PAGE_BYTES - 4,
        PSRAM_VADDR + FRAMEBUFFER_BYTES - 4,
        PSRAM_VADDR + FRAMEBUFFER_BYTES,
        PSRAM_VADDR + MAPPED_BYTES - 4,
    ];
    let patterns = [
        0x0123_4567u32,
        0x89AB_CDEF,
        0x55AA_33CC,
        0xA5A5_5A5A,
        0xC001_CAFE,
    ];
    for index in 0..locations.len() {
        unsafe { (locations[index] as *mut u32).write_volatile(patterns[index]) };
    }
    for index in 0..locations.len() {
        let value = unsafe { (locations[index] as *const u32).read_volatile() };
        if value != patterns[index] {
            uart::log_hex(b"PSRAM: bad address=", locations[index] as u32);
            uart::log_hex(b"PSRAM: bad value=", value);
            return false;
        }
    }
    true
}

fn common_read(command: u32, address: u32, dummy_bits: u32, output: &mut [u8]) -> bool {
    transaction(command, address, dummy_bits, None, Some(output))
}

fn common_write(command: u32, address: u32, dummy_bits: u32, input: &[u8]) -> bool {
    transaction(command, address, dummy_bits, Some(input), None)
}

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
            uart::log(b"PSRAM: MSPI3 busy before command configuration\r\n");
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
                uart::log(b"PSRAM: MSPI3 command timeout\r\n");
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
unsafe fn log_mspi3_state() {
    unsafe {
        uart::log_hex(b"PSRAM: MSPI3 CMD=", read(MSPI3));
        uart::log_hex(b"PSRAM: MSPI3 CLOCK=", read(MSPI3 + 0x14));
        uart::log_hex(b"PSRAM: MSPI3 USER=", read(MSPI3 + 0x18));
        uart::log_hex(b"PSRAM: MSPI3 MISC=", read(MSPI3 + 0x34));
        uart::log_hex(b"PSRAM: HP CLK=", read(HP_SYS_CLKRST + 0x30));
        uart::log_hex(b"PSRAM: LDO2=", read(PMU + 0x1D0));
        uart::log_hex(b"PSRAM: LDO2 ANA=", read(PMU + 0x1D4));
        uart::log_hex(b"PSRAM: RF PWC=", read(PMU + 0x15C));
    }
}

fn set_bus_divider(register: usize, divider: u32) {
    let value = ((divider - 1) << 16) | ((divider / 2 - 1) << 8) | (divider - 1);
    unsafe {
        write(register, value);
    }
}

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
