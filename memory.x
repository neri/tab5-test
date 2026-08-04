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
    /* The P4 IDF bootloader expects two XIP segments: app-desc/rodata and
     * executable text. Keep them as separate, contiguous flash regions. */
    ROM_RODATA : ORIGIN = 0x40000020, LENGTH = 0x00000fe0
    ROM_TEXT : ORIGIN = 0x40001000, LENGTH = 0x003ff000
    RAM : ORIGIN = 0x4ff40000, LENGTH = 0x0006e000
}

REGION_ALIAS("REGION_TEXT", ROM_TEXT);
/* Generic riscv-rt emits a 16-byte empty .rodata section after .text. Put
 * that bookkeeping section in RAM so it cannot become a third XIP segment. */
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
        *(.srodata .srodata.*)
        *(.rodata .rodata.*)
    } > ROM_RODATA
}
INSERT BEFORE .text;
