/*
 * ESP32-P4 ECO2 memory map used by the Tab5 bootloader:
 *
 * - flash is mapped at 0x4000_0000; +0x20 is the ESP image header
 * - on ECO2 (chip revision < v3) usable L2 RAM begins at 0x4ff4_0000: the ROM
 *   stack sits at 0x4ff3_cfc0 and ROM .data/.bss end exactly at 0x4ff4_0000.
 *   The 2nd-stage bootloader itself lives below that, at 0x4ff2_cbd0.
 * - on this revision the L2 cache is carved out of the *top* of L2MEM, so
 *   usable RAM ends at 0x4ffc_0000 minus the configured cache size.  See
 *   SRAM_HIGH_* in ESP-IDF's esp_system/ld/esp32p4/memory.ld.in and
 *   STARTUP_DATA_SIZE in heap/port/esp32p4/memory_layout.c, both under
 *   CONFIG_ESP32P4_SELECTS_REV_LESS_V3.  (0x4ffa_efc0 belongs to the rev >= v3
 *   layout, where the cache comes off the bottom instead; it does not apply.)
 *
 *   The cache size is chosen by the 2nd-stage bootloader, so it is only known
 *   at run time: ESP-IDF defaults to 128 KiB (top = 0x4ffa_0000) while the
 *   hardware reset value is 256 KiB (top = 0x4ff8_0000).  The intersection,
 *   0x00040000, is safe either way and is what this used to be sized for.
 *   `startup::log_ram_limit` prints the split that is actually in effect. XIP
 *   frees enough SRAM to use only the intersection safe for both cache sizes:
 *   0x4ff4_0000..0x4ff8_0000. The stack therefore never overlaps the cache,
 *   even when a different bootloader selects the larger 256 KiB cache.
 *
 * `riscv-rt` consumes the REGION_* aliases below.
 */
MEMORY
{
    /* The P4 IDF bootloader expects exactly two XIP segments.  Fill DROM up to
     * eight bytes before the next 64 KiB page boundary: the following segment
     * header then puts the IROM payload at image offset +0x20000, matching the
     * virtual address below without an extra espflash padding segment. */
    ROM_RODATA : ORIGIN = 0x40000020, LENGTH = 0x0001ffd8
    ROM_TEXT : ORIGIN = 0x40020000, LENGTH = 0x003e0000
    RAM : ORIGIN = 0x4ff40000, LENGTH = 0x00040000
}

REGION_ALIAS("REGION_TEXT", RAM);
REGION_ALIAS("REGION_RODATA", RAM);
REGION_ALIAS("REGION_DATA", RAM);
REGION_ALIAS("REGION_BSS", RAM);
REGION_ALIAS("REGION_HEAP", RAM);
REGION_ALIAS("REGION_STACK", RAM);

/* Keep reset/trap handling and the complete flash-critical call graph in
 * internal RAM.  This block is inserted before riscv-rt's .text.dummy so that
 * the latter can still use _stext as the start of ordinary application text. */
SECTIONS
{
    .iram.text : ALIGN(64)
    {
        __siram_text = .;
        KEEP(*(.init));
        KEEP(*(.init.rust));
        *(.iram.text.startup .iram.text.startup.*);

        . = ALIGN(64);
        __sflash_critical = .;
        *(.iram.text.critical .iram.text.critical.*);
        . = ALIGN(4);
        __eflash_critical = .;
        __eiram_text = .;
    } > RAM

    .dram.rodata : ALIGN(4)
    {
        __sdram_rodata = .;
        KEEP(*(.dram.rodata .dram.rodata.*));
        . = ALIGN(4);
        __edram_rodata = .;
    } > RAM

    /* The firmware installs its own CLIC-aware _start_trap in the critical
     * IRAM block above. Drop riscv-rt's unused generic trap implementation so
     * it cannot leave a second, flash-dependent trap path in RAM. */
    /DISCARD/ :
    {
        *(.trap.vector);
        *(.trap.start .trap.start.*);
        *(.trap.continue);
        *(.trap.rust);
        *(.trap .trap.*);
    }
}
INSERT BEFORE .text.dummy;

/* riscv-rt requires its dummy/default .text output to live in REGION_TEXT.
 * Ordinary text inputs are consumed by .flash.text below, so park that empty
 * output after the internal-only RAM sections. */
_stext = ALIGN(ADDR(.dram.rodata) + SIZEOF(.dram.rodata), 64);

/* ESP-IDF reads the descriptor immediately after the 32-byte image header.
 * Collect ordinary read-only data after it in the same DROM segment.  The
 * final location-counter assignment deliberately pads the segment to the
 * physical/virtual page boundary required by the next IROM segment.
 *
 * REGION_RODATA remains RAM because riscv-rt also uses it as .data's load
 * region.  The bootloader already loads .data directly to its RAM VMA, so
 * keeping that behavior avoids a second startup copy without consuming any
 * additional run-time RAM.  These input sections are consumed here before
 * riscv-rt's otherwise-empty default .rodata/.eh_frame outputs. */
SECTIONS
{
    .flash.appdesc : ALIGN(4)
    {
        KEEP(*(.flash.appdesc));
    } > ROM_RODATA

    .flash.rodata : ALIGN(4)
    {
        KEEP(*(.eco2.rodata.probe.pre));
        /* Keep the post-reset probe out of the pre-reset cache line. */
        . = ALIGN(256);
        KEEP(*(.eco2.rodata.probe.post));
        *(.srodata .srodata.*);
        *(.rodata .rodata.*);
        KEEP(*(.eh_frame .eh_frame.*));
        . = ORIGIN(ROM_RODATA) + LENGTH(ROM_RODATA) - 1;
        BYTE(0);
    } > ROM_RODATA

    .flash.text : ALIGN(4)
    {
        KEEP(*(.eco2.xip_stub.pre));
        /* A distinct cache line makes the post-reset call a real XIP miss. */
        . = ALIGN(256);
        KEEP(*(.eco2.xip_stub.post));
        /* The IRAM blocks inserted before .text.dummy have already consumed
         * startup, trap/ISR and PSRAM-critical functions. */
        *(.text.abort);
        *(.text .text.*);
    } > ROM_TEXT
}
INSERT BEFORE .text;

/* The two flash-mapped segments must keep matching physical and virtual 64 KiB
 * page offsets, otherwise espflash inserts a padding segment and the ECO2
 * bootloader sees three flash-mapped segments and asserts.  The image header is
 * 24 bytes and every segment header is 8 bytes, so the second segment's payload
 * starts at 0x20 + LENGTH(ROM_RODATA) + 8, which has to equal ROM_TEXT's page
 * offset.  Both checks fail at link time instead of failing to boot. */
ASSERT(ORIGIN(ROM_TEXT) - ORIGIN(ROM_RODATA) == LENGTH(ROM_RODATA) + 8,
       "ROM_TEXT must start one segment header after the end of ROM_RODATA");
ASSERT(ADDR(.flash.rodata) + SIZEOF(.flash.rodata) == ORIGIN(ROM_RODATA) + LENGTH(ROM_RODATA),
       "DROM segment no longer fills ROM_RODATA exactly");
ASSERT(ADDR(.flash.text) >= ORIGIN(ROM_TEXT) &&
       ADDR(.flash.text) + SIZEOF(.flash.text) <= ORIGIN(ROM_TEXT) + LENGTH(ROM_TEXT),
       "IROM text is outside ROM_TEXT");
ASSERT(__siram_text >= ORIGIN(RAM) && __eiram_text <= ORIGIN(RAM) + LENGTH(RAM),
       "IRAM text is outside RAM");
ASSERT(__sdram_rodata >= ORIGIN(RAM) && __edram_rodata <= ORIGIN(RAM) + LENGTH(RAM),
       "DRAM rodata is outside RAM");
ASSERT(SIZEOF(.text) == 0, "ordinary text leaked back into RAM");
ASSERT(SIZEOF(.rodata) == 0, "ordinary rodata leaked back into RAM");
ASSERT(SIZEOF(.stack) >= 0x20000, "less than 128 KiB remains for the stack");
