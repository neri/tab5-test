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
 *   hardware reset value is 256 KiB (top = 0x4ff8_0000).  The window below is
 *   the intersection and is therefore safe either way.  `startup::log_ram_limit`
 *   prints the split that is actually in effect; once a 128 KiB split is
 *   confirmed on the target device this can be raised to 0x00060000.
 *
 * `riscv-rt` consumes the REGION_* aliases below.
 */
MEMORY
{
    /* The P4 IDF bootloader expects exactly two XIP segments. Keep an 8-byte
     * virtual gap between the descriptor segment and compatibility segment:
     * espflash then preserves two headers without adding a padding segment. */
    /* The 0x1018-byte first segment makes the following image segment's
     * physical payload start at +0x1040, matching ROM_TEXT's page offset. */
    ROM_RODATA : ORIGIN = 0x40000020, LENGTH = 0x00001018
    /* A four-byte compatibility segment is enough to satisfy the ECO2
     * bootloader's exactly-two-XIP-segments invariant. Application code is
     * loaded into HP SRAM instead of executing through the flash cache. */
    ROM_TEXT : ORIGIN = 0x40001040, LENGTH = 0x00000004
    RAM : ORIGIN = 0x4ff40000, LENGTH = 0x00040000
}

REGION_ALIAS("REGION_TEXT", RAM);
REGION_ALIAS("REGION_RODATA", RAM);
REGION_ALIAS("REGION_DATA", RAM);
REGION_ALIAS("REGION_BSS", RAM);
REGION_ALIAS("REGION_HEAP", RAM);
REGION_ALIAS("REGION_STACK", RAM);

/* ESP-IDF reads the descriptor immediately after the 32-byte image header.
 * Collect all read-only data in the same first XIP segment. */
SECTIONS
{
    .flash.appdesc : ALIGN(4)
    {
        KEEP(*(.flash.appdesc));
    } > ROM_RODATA

    .eco2.rodata : ALIGN(4)
    {
        KEEP(*(.eco2.pad));
    } > ROM_RODATA

    .eco2.xip_stub : ALIGN(4)
    {
        KEEP(*(.eco2.xip_stub));
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
ASSERT(ADDR(.eco2.rodata) + SIZEOF(.eco2.rodata) == ORIGIN(ROM_RODATA) + LENGTH(ROM_RODATA),
       "XIP_SEGMENT_PAD no longer fills ROM_RODATA exactly");
