//! M5Stack Tab5 (ESP32-P4 ECO2) multi-keyboard input echo console.
//! Linker layout: normal read-only data in DROM, normal code in IROM, and only
//! flash-critical code/data plus writable state in internal HP SRAM.

#![no_std]
#![no_main]

extern crate alloc;

use linked_list_allocator::LockedHeap;
use riscv_rt::entry;

mod app;
mod bmi270;
mod cardkb;
mod console;
mod delay;
mod dma2d;
mod framebuffer;
mod gpio;
mod i2c;
mod icm;
mod ina226;
mod input;
mod interrupts;
mod lcd;
mod net;
mod pma;
mod pmp;
mod power;
mod ppa;
mod psram;
mod rtc;
mod sdio;
mod sdmmc;
mod startup;
mod tab5_keyboard;
mod tick;
mod touch;
mod uart;
mod usb;
mod wifi;

// ESP-IDF 2nd-stage bootloaders and espflash require this descriptor. The
// linker rule in `memory.x` keeps it immediately after the 32-byte image
// header; no ESP32-P4 HAL or ROM implementation is linked in.
#[repr(C)]
struct EspAppDesc {
    magic_word: u32,
    secure_version: u32,
    reserv1: [u32; 2],
    version: [u8; 32],
    project_name: [u8; 32],
    time: [u8; 16],
    date: [u8; 16],
    idf_ver: [u8; 32],
    app_elf_sha256: [u8; 32],
    min_efuse_blk_rev_full: u16,
    max_efuse_blk_rev_full: u16,
    mmu_page_size: u8,
    reserv3: [u8; 3],
    reserv2: [u32; 18],
}

const fn cstr<const N: usize>(value: &str) -> [u8; N] {
    let bytes = value.as_bytes();
    let mut output = [0; N];
    let mut i = 0;
    while i < bytes.len() && i + 1 < N {
        output[i] = bytes[i];
        i += 1;
    }
    output
}

#[used]
#[unsafe(link_section = ".flash.appdesc")]
static APP_DESC: EspAppDesc = EspAppDesc {
    magic_word: 0xABCD_5432,
    secure_version: 0,
    reserv1: [0; 2],
    version: cstr("0.2.0"),
    project_name: cstr("tab5-cardkb-console"),
    time: cstr("00:00:00"),
    date: cstr("2026-08-03"),
    idf_ver: cstr("no_std-eco2"),
    app_elf_sha256: [0; 32],
    min_efuse_blk_rev_full: 0,
    max_efuse_blk_rev_full: u16::MAX,
    mmu_page_size: 16, // 64 KiB
    reserv3: [0; 3],
    reserv2: [0; 18],
};

// Leave recognizable non-zero words at the start of normal RAM data. Together
// with the BSS probe below these detect a broken bootloader/startup data layout
// before any peripheral driver relies on initialized globals.
#[used]
#[unsafe(link_section = ".data")]
static BOOT_LAYOUT_MARKER: [u32; 3] = [0xEC02_0001, 0x1357_9BDF, 0xA55A_C33C];

#[used]
#[unsafe(link_section = ".bss.boot_layout")]
static mut BOOT_BSS_MARKER: [u32; 3] = [0; 3];

// Backed by PSRAM once `psram::init` succeeds; unused until then.
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

const XIP_DROM_PRE_WORDS: [u32; 4] = [0x4452_4F4D, 0x5850_2101, 0xA55A_C33C, 0x1357_9BDF];
const XIP_DROM_POST_WORDS: [u32; 4] = [0x504F_5354, 0x434F_4C44, 0x5AA5_3CC3, 0x2468_ACE0];

const fn drom_probe_checksum(words: [u32; 4]) -> u32 {
    words[0] ^ words[1].rotate_left(3) ^ words[2].rotate_left(11) ^ words[3].rotate_left(19)
}

// Separate DROM cache lines prove that the post-PSRAM check is a new flash
// read, rather than a cache hit left by the pre-PSRAM check. Volatile reads in
// `run_xip_probe` prevent release LTO from replacing them with constants.
#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".eco2.rodata.probe.pre")]
static XIP_DROM_PROBE_PRE: [u32; 4] = XIP_DROM_PRE_WORDS;

#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".eco2.rodata.probe.post")]
static XIP_DROM_PROBE_POST: [u32; 4] = XIP_DROM_POST_WORDS;

#[used]
#[unsafe(link_section = ".dram.rodata.diagnostics")]
static PANIC_MESSAGE: [u8; 7] = *b"PANIC\r\n";

#[used]
#[unsafe(link_section = ".dram.rodata.diagnostics")]
static EXCEPTION_MCAUSE: [u8; 17] = *b"EXCEPTION mcause=";

#[used]
#[unsafe(link_section = ".dram.rodata.diagnostics")]
static EXCEPTION_MEPC: [u8; 15] = *b"EXCEPTION mepc=";

#[used]
#[unsafe(link_section = ".dram.rodata.diagnostics")]
static EXCEPTION_MTVAL: [u8; 16] = *b"EXCEPTION mtval=";

// ESP-IDF v5.5.3 on ESP32-P4 ECO2 asserts unless it finds exactly two
// external-memory-addressed image segments. This is the first real IROM code
// in the firmware: call it on both sides of `psram::init` before migrating the
// normal application code to XIP.
#[inline(never)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".eco2.xip_stub.pre")]
extern "C" fn xip_instruction_probe_pre(value: u32) -> u32 {
    value.rotate_left(9).wrapping_add(0x4952_4F4D) ^ 0xC33C_A55A
}

#[inline(never)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".eco2.xip_stub.post")]
extern "C" fn xip_instruction_probe_post(value: u32) -> u32 {
    value.rotate_right(7).wrapping_mul(0x0101_0101) ^ 0x5AA5_3CC3
}

fn run_xip_probe(
    phase: &[u8],
    drom_probe: *const u32,
    expected_drom: u32,
    instruction_probe: extern "C" fn(u32) -> u32,
    input: u32,
    expected_instruction: u32,
) {
    uart::log(b"XIP: ");
    uart::log(phase);
    uart::log(b" DROM probe start\r\n");

    let actual_drom = unsafe {
        drom_probe.add(0).read_volatile()
            ^ drom_probe.add(1).read_volatile().rotate_left(3)
            ^ drom_probe.add(2).read_volatile().rotate_left(11)
            ^ drom_probe.add(3).read_volatile().rotate_left(19)
    };
    if actual_drom != expected_drom {
        uart::log(b"XIP: DROM probe failed\r\n");
        halt();
    }

    uart::log(b"XIP: ");
    uart::log(phase);
    uart::log(b" IROM probe start\r\n");

    // `black_box` keeps this as an actual indirect call to the IROM address
    // under fat LTO instead of letting the optimizer fold the probe locally.
    let probe_fn = core::hint::black_box(instruction_probe);
    let actual = probe_fn(core::hint::black_box(input));
    if actual != expected_instruction {
        uart::log_hex(b"XIP: IROM probe actual=", actual);
        uart::log_hex(b"XIP: IROM probe expected=", expected_instruction);
        halt();
    }

    uart::log(b"XIP: ");
    uart::log(phase);
    uart::log(b" DROM+IROM ok\r\n");
}

fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn validate_boot_layout() {
    let data = &raw const BOOT_LAYOUT_MARKER as *const u32;
    let bss = &raw const BOOT_BSS_MARKER as *const u32;
    let data_ok = unsafe {
        data.add(0).read_volatile() == 0xEC02_0001
            && data.add(1).read_volatile() == 0x1357_9BDF
            && data.add(2).read_volatile() == 0xA55A_C33C
    };
    let bss_ok = unsafe {
        bss.add(0).read_volatile() == 0
            && bss.add(1).read_volatile() == 0
            && bss.add(2).read_volatile() == 0
    };
    if !data_ok || !bss_ok {
        uart::log(b"MEM: .data/.bss initialization failed\r\n");
        halt();
    }
}

#[entry]
#[unsafe(link_section = ".iram.text.startup")]
fn main() -> ! {
    startup::init();
    validate_boot_layout();
    if !startup::raise_cpu_clock() {
        uart::log(b"CPU: unexpected boot clock source, staying at 90 MHz\r\n");
    }
    uart::hello_world();
    startup::log_ram_limit();
    if i2c::initialize_board_bus().is_err() {
        uart::log(b"I2C: board-bus recovery failed\r\n");
    }
    // A CPU-only reset leaves the previous boot's scanout DMA reading PSRAM.
    // It has to stop before `psram::init` resets and re-tunes that controller
    // underneath it.
    lcd::quiesce_dma();
    // Same reasoning for the other master that can be left addressing PSRAM
    // across a CPU-only reset, and the point at which its registers become
    // readable at all.
    if !ppa::init() {
        uart::log(b"PPA: unavailable, fills stay on the CPU\r\n");
    }
    let pre_input = 0x1020_3040u32;
    run_xip_probe(
        b"pre-PSRAM",
        &raw const XIP_DROM_PROBE_PRE as *const u32,
        drom_probe_checksum(XIP_DROM_PRE_WORDS),
        xip_instruction_probe_pre,
        pre_input,
        pre_input.rotate_left(9).wrapping_add(0x4952_4F4D) ^ 0xC33C_A55A,
    );
    if let Some(psram) = psram::init() {
        let post_input = 0x89AB_CDEFu32;
        run_xip_probe(
            b"post-PSRAM",
            &raw const XIP_DROM_PROBE_POST as *const u32,
            drom_probe_checksum(XIP_DROM_POST_WORDS),
            xip_instruction_probe_post,
            post_input,
            post_input.rotate_right(7).wrapping_mul(0x0101_0101) ^ 0x5AA5_3CC3,
        );
        let (heap_start, heap_size) = psram.heap();
        unsafe {
            ALLOCATOR.lock().init(heap_start, heap_size);
        }
        app::run(psram);
    }

    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
#[unsafe(link_section = ".iram.text.critical.panic")]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    uart::log(&PANIC_MESSAGE);
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(export_name = "ExceptionHandler")]
#[unsafe(link_section = ".iram.text.critical.exception")]
fn exception_handler(_: &riscv_rt::TrapFrame) -> ! {
    let mcause: u32;
    let mepc: u32;
    let mtval: u32;
    unsafe {
        core::arch::asm!("csrr {0}, mcause", out(reg) mcause);
        core::arch::asm!("csrr {0}, mepc", out(reg) mepc);
        core::arch::asm!("csrr {0}, mtval", out(reg) mtval);
    }
    uart::log_hex(&EXCEPTION_MCAUSE, mcause);
    uart::log_hex(&EXCEPTION_MEPC, mepc);
    uart::log_hex(&EXCEPTION_MTVAL, mtval);
    loop {
        core::hint::spin_loop();
    }
}
