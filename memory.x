/*
 * ESP32-P4 ECO2 memory map used by the Tab5 bootloader:
 *
 * - flash is mapped at 0x4000_0000; +0x20 is the ESP image header
 * - with the IDF default 256 KiB L2-cache split, usable L2 RAM begins at
 *   0x4ff4_0000.  This matches the addresses printed by the Tab5 boot log.
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
    RAM : ORIGIN = 0x4ff40000, LENGTH = 0x0006e000
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
