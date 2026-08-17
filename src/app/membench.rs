//! CPU-side memory cost measurement for internal SRAM and PSRAM.
//!
//! Everything in `DESIGN.md`'s bandwidth discussion rests on how much a store
//! into PSRAM actually costs, and that had been estimated rather than measured:
//! a 64-byte line, 18 dummy cycles, CS setup and hold, and a command phase
//! whose length was a guess. This replaces the guess.
//!
//! The figures that matter are at the bottom of the report. `line write`
//! touches one `u16` per 64-byte line, which is the worst case a per-pixel
//! drawing loop produces and therefore the real price of write-allocate: the
//! line has to be fetched before the store can land.
//!
//! `line read` walks those same consecutive lines, so anything that prefetches
//! ahead will hide the latency in it -- which is honest for the drawing code,
//! since that also walks memory in order. `scatter read` exists to defeat that:
//! it strides 4 KiB at a time, past any plausible prefetch window and across
//! PSRAM page boundaries, while still covering every line. The gap between the
//! two is how much sequential access is buying.
//!
//! Scanout is running throughout, so the PSRAM numbers include the ~125 MB/s
//! the display is already taking. That is the condition the drawing code
//! actually runs in, and measuring it idle would flatter it.

use core::arch::asm;
use core::cell::UnsafeCell;

use crate::startup;

/// Internal-RAM buffer.
///
/// `memory.x` gives the whole firmware 256 KiB and the stack takes what is
/// left after `.bss`, so this trades directly against stack headroom. It has
/// to stay comfortably above the L1 data cache for the results to describe
/// SRAM rather than cache -- `l1_data_cache_bytes` reports what that actually
/// is, so the report can be read against it.
const SRAM_BYTES: usize = 48 * 1024;

/// PSRAM buffer. Sized past the largest L2 cache the bootloader can configure
/// (512 KiB) so a pass over it cannot be served from cache.
const PSRAM_BYTES: usize = 1024 * 1024;

const LINE_BYTES: usize = 64;

/// ESP32-P4 maps the same external-memory MMU pages into a second CPU-only
/// window which bypasses both cache levels.  Keep the cached pointer for cache
/// maintenance and add this offset only to the pointer used by the timed loop.
const NON_CACHEABLE_OFFSET: usize = 0x4000_0000;

// Reached through the `UnsafeCell`'s pointer rather than by field access, so
// the field genuinely is never read by name.
#[repr(align(64))]
struct AlignedBuffer(#[allow(dead_code)] [u32; SRAM_BYTES / 4]);

struct SramStorage(UnsafeCell<AlignedBuffer>);

// Touched only by the foreground shell command, from one hart.
unsafe impl Sync for SramStorage {}

static SRAM_BUFFER: SramStorage = SramStorage(UnsafeCell::new(AlignedBuffer([0; SRAM_BYTES / 4])));

/// One target's results. Throughputs are MB/s; line figures are nanoseconds
/// per 64-byte line.
pub struct Measurements {
    pub sequential_write_u32: u32,
    pub sequential_write_u16: u32,
    pub sequential_read_u32: u32,
    pub line_write_ns: u32,
    pub line_read_ns: u32,
    pub scatter_read_ns: u32,
}

pub struct Report {
    pub cpu_hz: u32,
    pub l1_data_cache_bytes: u32,
    pub sram: Measurements,
    pub psram: Option<Measurements>,
    pub psram_direct: Option<Measurements>,
}

/// Runs the whole suite. `psram_buffer` is a caller-owned span in mapped
/// PSRAM; passing `None` reports internal RAM only.
pub fn run(psram_buffer: Option<(*mut u32, usize)>) -> Report {
    let cpu_hz = startup::cpu_hz();
    let sram = unsafe {
        let base = SRAM_BUFFER.0.get() as *mut u32;
        measure(base, SRAM_BYTES, cpu_hz, MemoryPath::Internal)
    };
    let psram = psram_buffer.map(|(base, bytes)| unsafe {
        measure(base, bytes, cpu_hz, MemoryPath::CachedPsram { base })
    });
    let psram_direct = psram_buffer.map(|(cached_base, bytes)| unsafe {
        // The direct and cached virtual windows share the PSRAM MMU table.
        // Flush the cached view left by the preceding measurement before the
        // same physical bytes are touched through the direct view.
        crate::psram::writeback_invalidate(cached_base as usize, bytes);
        let direct_base = cached_base.map_addr(|address| address + NON_CACHEABLE_OFFSET);
        measure(
            direct_base,
            bytes,
            cpu_hz,
            MemoryPath::DirectPsram { cached_base },
        )
    });
    Report {
        cpu_hz,
        l1_data_cache_bytes: l1_data_cache_bytes(),
        sram,
        psram,
        psram_direct,
    }
}

#[derive(Clone, Copy)]
enum MemoryPath {
    Internal,
    CachedPsram { base: *mut u32 },
    DirectPsram { cached_base: *mut u32 },
}

/// Configured L1 data cache size, from the one-hot size register in the same
/// block `startup::log_ram_limit` reads the L2 split from.
///
/// The SRAM figures only describe SRAM if the buffer is larger than this.
fn l1_data_cache_bytes() -> u32 {
    const L1_DCACHE_CACHESIZE_CONF: usize = 0x3FF1_0018;
    let configured = unsafe { (L1_DCACHE_CACHESIZE_CONF as *const u32).read_volatile() } & 0x3FF;
    if configured == 0 {
        return 0;
    }
    // Bit 0 means 256 bytes and each further bit doubles it.
    256u32 << configured.trailing_zeros()
}

/// # Safety
/// `base` must be writable for `bytes` and 64-byte aligned.
unsafe fn measure(base: *mut u32, bytes: usize, cpu_hz: u32, path: MemoryPath) -> Measurements {
    let lines = bytes / LINE_BYTES;

    // Each measurement starts from a cold cache, otherwise the line figures
    // report a cache hit rather than the memory behind it.
    let cycles_write_u32 = unsafe {
        cold(bytes, path);
        sequential_write_u32(base, bytes / 4)
    };
    let cycles_write_u16 = unsafe {
        cold(bytes, path);
        sequential_write_u16(base as *mut u16, bytes / 2)
    };
    let cycles_read_u32 = unsafe {
        cold(bytes, path);
        sequential_read_u32(base, bytes / 4)
    };
    let cycles_line_write = unsafe {
        cold(bytes, path);
        line_write_u16(base as *mut u16, lines)
    };
    let cycles_line_read = unsafe {
        cold(bytes, path);
        line_read_u32(base, lines)
    };
    let cycles_scatter_read = unsafe {
        cold(bytes, path);
        scatter_read_u32(base, bytes)
    };

    Measurements {
        sequential_write_u32: throughput(bytes, cycles_write_u32, cpu_hz),
        sequential_write_u16: throughput(bytes, cycles_write_u16, cpu_hz),
        sequential_read_u32: throughput(bytes, cycles_read_u32, cpu_hz),
        line_write_ns: nanoseconds_each(cycles_line_write, lines, cpu_hz),
        line_read_ns: nanoseconds_each(cycles_line_read, lines, cpu_hz),
        scatter_read_ns: nanoseconds_each(cycles_scatter_read, lines, cpu_hz),
    }
}

/// Drops the buffer out of both cache levels so the next pass misses.
///
/// # Safety
/// The cached alias in `path` must span `bytes` when it is present.
unsafe fn cold(bytes: usize, path: MemoryPath) {
    match path {
        MemoryPath::Internal => {}
        MemoryPath::CachedPsram { base } => {
            crate::psram::writeback_invalidate(base as usize, bytes);
        }
        MemoryPath::DirectPsram { cached_base } => {
            // Cache maintenance APIs take addresses from the cached window,
            // not the CPU-only 0x8xxx_xxxx direct window.  Evicting that alias
            // prevents a stale dirty line from later overwriting direct stores.
            crate::psram::writeback_invalidate(cached_base as usize, bytes);
        }
    }
    // Internal RAM has no such operation; the buffer is sized to push itself
    // out of L1 instead, which is why its results are labelled as including
    // whatever cache still helps.
}

fn throughput(bytes: usize, cycles: u32, cpu_hz: u32) -> u32 {
    if cycles == 0 {
        return 0;
    }
    ((bytes as u64 * cpu_hz as u64) / (cycles as u64 * 1_000_000)) as u32
}

fn nanoseconds_each(cycles: u32, count: usize, cpu_hz: u32) -> u32 {
    if count == 0 || cpu_hz == 0 {
        return 0;
    }
    ((cycles as u64 * 1_000_000_000) / (count as u64 * cpu_hz as u64)) as u32
}

/// Reads the RISC-V cycle counter. ESP-IDF saves and restores `mcycle` across
/// sleep on this target, so it runs without any counter-inhibit handling.
pub fn cycles() -> u32 {
    let value: u32;
    unsafe {
        asm!("csrr {0}, mcycle", out(reg) value, options(nomem, nostack));
    }
    value
}

/// Orders all explicit memory accesses before the following cycle-counter
/// read.  This matters for the direct alias: without a fence the timed loop
/// could stop while stores were still queued in the CPU write buffer.
#[inline(always)]
fn finish_memory_accesses() {
    unsafe {
        asm!("fence rw, rw", options(nostack));
    }
}

/// # Safety
/// `base` must be writable for `words` 32-bit words.
unsafe fn sequential_write_u32(base: *mut u32, words: usize) -> u32 {
    let start = cycles();
    for index in 0..words {
        unsafe { base.add(index).write_volatile(0x5A5A_5A5A) };
    }
    finish_memory_accesses();
    cycles().wrapping_sub(start)
}

/// # Safety
/// `base` must be writable for `halfwords` 16-bit halfwords.
unsafe fn sequential_write_u16(base: *mut u16, halfwords: usize) -> u32 {
    let start = cycles();
    for index in 0..halfwords {
        unsafe { base.add(index).write_volatile(0x5A5A) };
    }
    finish_memory_accesses();
    cycles().wrapping_sub(start)
}

/// # Safety
/// `base` must be readable for `words` 32-bit words.
unsafe fn sequential_read_u32(base: *const u32, words: usize) -> u32 {
    let start = cycles();
    for index in 0..words {
        // Volatile so the loop cannot be discarded as having no effect.
        unsafe { core::ptr::read_volatile(base.add(index)) };
    }
    finish_memory_accesses();
    cycles().wrapping_sub(start)
}

/// One 16-bit store per 64-byte line: the shape a per-pixel drawing loop
/// produces, and the case where write-allocate costs a full line fetch for two
/// bytes of payload.
///
/// # Safety
/// `base` must be writable for `lines` 64-byte lines.
unsafe fn line_write_u16(base: *mut u16, lines: usize) -> u32 {
    let stride = LINE_BYTES / core::mem::size_of::<u16>();
    let start = cycles();
    for index in 0..lines {
        unsafe { base.add(index * stride).write_volatile(0x5A5A) };
    }
    finish_memory_accesses();
    cycles().wrapping_sub(start)
}

/// One 32-bit load per 64-byte line: plain miss latency.
///
/// # Safety
/// `base` must be readable for `lines` 64-byte lines.
unsafe fn line_read_u32(base: *const u32, lines: usize) -> u32 {
    let stride = LINE_BYTES / core::mem::size_of::<u32>();
    let start = cycles();
    for index in 0..lines {
        unsafe { core::ptr::read_volatile(base.add(index * stride)) };
    }
    finish_memory_accesses();
    cycles().wrapping_sub(start)
}

/// One 32-bit load per 64-byte line, but ordered so consecutive accesses are
/// 4 KiB apart instead of adjacent.
///
/// Every line is still visited exactly once -- the outer loop shifts the start
/// by one line each pass -- so this is directly comparable with `line_read_u32`
/// and the difference between them is what prefetching and page locality are
/// worth.
///
/// # Safety
/// `base` must be readable for `bytes`.
unsafe fn scatter_read_u32(base: *const u32, bytes: usize) -> u32 {
    const STRIDE_BYTES: usize = 4096;
    let words_per_stride = STRIDE_BYTES / core::mem::size_of::<u32>();
    let words_per_line = LINE_BYTES / core::mem::size_of::<u32>();
    let strides = bytes / STRIDE_BYTES;
    let passes = STRIDE_BYTES / LINE_BYTES;
    let start = cycles();
    for pass in 0..passes {
        for stride in 0..strides {
            let word = pass * words_per_line + stride * words_per_stride;
            unsafe { core::ptr::read_volatile(base.add(word)) };
        }
    }
    finish_memory_accesses();
    cycles().wrapping_sub(start)
}

pub const fn sram_bytes() -> usize {
    SRAM_BYTES
}

pub const fn psram_bytes() -> usize {
    PSRAM_BYTES
}
