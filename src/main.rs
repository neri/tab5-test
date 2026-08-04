//! M5Stack Tab5 (ESP32-P4 ECO2) no_std startup diagnostic.
//! Linker layout revision: two XIP segments for the ESP-IDF bootloader.

#![no_std]
#![no_main]

use riscv_rt::entry;

mod framebuffer;
mod interrupts;
mod lcd;
mod psram;
mod startup;
mod uart;

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
    version: cstr("0.1.0"),
    project_name: cstr("tab5-hello-world"),
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

// Force a non-empty RAM-load segment between the two XIP segments. This is
// the layout expected by the ESP-IDF v5.5 bootloader on the Tab5.
#[used]
#[unsafe(link_section = ".data")]
static BOOT_LAYOUT_MARKER: u32 = 0xEC02_0001;

#[entry]
fn main() -> ! {
    startup::init();
    uart::hello_world();
    if let Some(psram) = psram::init() {
        lcd::run_framebuffer(psram);
        // Establish the diagnostic pattern if direct DMA setup returned.
        let _ = lcd::start_pattern();
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
    loop {
        core::hint::spin_loop();
    }
}
