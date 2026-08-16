//! M5Stack Tab5 (ESP32-P4 ECO2) multi-keyboard input echo console.
//! Linker layout: two minimal XIP compatibility segments and an HP-SRAM body.

#![no_std]
#![no_main]

extern crate alloc;

use linked_list_allocator::LockedHeap;
use riscv_rt::entry;

mod app;
mod axis_test;
mod battery;
mod bmi270;
mod cardkb;
mod console;
mod delay;
mod framebuffer;
mod gpio;
mod i2c;
mod input;
mod ina226;
mod interrupts;
mod lcd;
mod mbr;
mod paint;
mod power;
mod psram;
mod sdmmc;
mod shell;
mod startup;
mod touch;
mod touch_test;
mod uart;
mod usb;

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

// Leave a recognizable word at the start of normal RAM data.
#[used]
#[unsafe(link_section = ".data")]
static BOOT_LAYOUT_MARKER: u32 = 0xEC02_0001;

// Backed by PSRAM once `psram::init` succeeds; unused until then.
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

// Make the second XIP segment's image payload start at offset 0x1040. Its
// virtual address has the same 64 KiB-page offset, so espflash does not create
// an extra padding segment. Only the app descriptor and this padding are
// flash-mapped; all regular Rust code and constants are loaded into HP SRAM.
#[used]
#[unsafe(link_section = ".eco2.pad")]
static XIP_SEGMENT_PAD: [u8; 3864] = [0; 3864];

// ESP-IDF v5.5.3 on ESP32-P4 ECO2 asserts unless it finds exactly two
// external-memory-addressed image segments. This segment is never executed.
#[used]
#[unsafe(link_section = ".eco2.xip_stub")]
static XIP_COMPATIBILITY_STUB: [u8; 4] = [0x01, 0x00, 0x01, 0x00];

#[entry]
fn main() -> ! {
    startup::init();
    if !startup::raise_cpu_clock() {
        uart::log(b"CPU: unexpected boot clock source, staying at 90 MHz\r\n");
    }
    uart::hello_world();
    startup::log_ram_limit();
    if i2c::initialize_board_bus().is_err() {
        uart::log(b"I2C: board-bus recovery failed\r\n");
    }
    if let Some(psram) = psram::init() {
        let (heap_start, heap_size) = psram.heap();
        unsafe {
            ALLOCATOR.lock().init(heap_start, heap_size);
        }
        app::run(psram);
    } else {
        // PSRAM failed, so keep the independent DSI VPG useful as a visible
        // diagnostic rather than attempting the framebuffer path.
        let _ = lcd::start_pattern();
    }

    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    uart::log(b"PANIC\r\n");
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(export_name = "ExceptionHandler")]
fn exception_handler(_: &riscv_rt::TrapFrame) -> ! {
    let mcause: u32;
    let mepc: u32;
    let mtval: u32;
    unsafe {
        core::arch::asm!("csrr {0}, mcause", out(reg) mcause);
        core::arch::asm!("csrr {0}, mepc", out(reg) mepc);
        core::arch::asm!("csrr {0}, mtval", out(reg) mtval);
    }
    uart::log_hex(b"EXCEPTION mcause=", mcause);
    uart::log_hex(b"EXCEPTION mepc=", mepc);
    uart::log_hex(b"EXCEPTION mtval=", mtval);
    loop {
        core::hint::spin_loop();
    }
}
