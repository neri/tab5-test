#!/usr/bin/env python3
"""Validate XIP/IRAM/DRAM placement and the flash-critical relocation closure."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


SECTION_RE = re.compile(
    r"^\s*\[\s*(\d+)\]\s+(\S+)\s+\S+\s+([0-9a-fA-F]+)\s+"
    r"[0-9a-fA-F]+\s+([0-9a-fA-F]+)"
)
ENTRY_RE = re.compile(r"Entry point address:\s+0x([0-9a-fA-F]+)")
ROM_START = 0x4FC00000
ROM_END = 0x4FD00000
XIP_START = 0x40000000
XIP_END = 0x40400000
RAM_START = 0x4FF40000
RAM_END = 0x4FF80000
MIN_STACK_BYTES = 0x20000


@dataclass(frozen=True)
class Section:
    index: int
    name: str
    address: int
    size: int

    def contains(self, address: int) -> bool:
        return self.address <= address < self.address + self.size


@dataclass(frozen=True)
class Symbol:
    name: str
    value: int
    section_index: str


@dataclass(frozen=True)
class Relocation:
    offset: int
    kind: str
    target_value: int
    target_name: str


def run_readelf(readelf: str, *arguments: str) -> str:
    result = subprocess.run(
        [readelf, *arguments],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or f"{readelf} failed")
    return result.stdout


def parse_sections(output: str) -> dict[str, Section]:
    sections: dict[str, Section] = {}
    for line in output.splitlines():
        match = SECTION_RE.match(line)
        if match is None:
            continue
        index, name, address, size = match.groups()
        sections[name] = Section(int(index), name, int(address, 16), int(size, 16))
    return sections


def parse_symbols(output: str) -> dict[str, Symbol]:
    symbols: dict[str, Symbol] = {}
    for line in output.splitlines():
        parts = line.split()
        if len(parts) < 8 or not parts[0].endswith(":"):
            continue
        try:
            value = int(parts[1], 16)
        except ValueError:
            continue
        name = parts[7]
        symbols[name] = Symbol(name, value, parts[6])
    return symbols


def parse_relocations(output: str, section_name: str) -> list[Relocation]:
    relocations: list[Relocation] = []
    active = False
    heading = f"Relocation section '{section_name}'"
    for line in output.splitlines():
        if line.startswith("Relocation section '"):
            active = line.startswith(heading)
            continue
        if not active:
            continue
        parts = line.split()
        if len(parts) < 5:
            continue
        try:
            offset = int(parts[0], 16)
            target_value = int(parts[3], 16)
        except ValueError:
            continue
        relocations.append(Relocation(offset, parts[2], target_value, parts[4]))
    return relocations


def symbol_value(symbols: dict[str, Symbol], name: str) -> int:
    try:
        return symbols[name].value
    except KeyError as error:
        raise ValueError(f"required linker symbol {name} is missing") from error


def containing_section(sections: dict[str, Section], address: int) -> Section | None:
    for section in sections.values():
        if section.contains(address):
            return section
    return None


def validate(elf: Path, readelf: str) -> None:
    section_output = run_readelf(readelf, "-SW", str(elf))
    symbol_output = run_readelf(readelf, "-sW", str(elf))
    relocation_output = run_readelf(readelf, "-rW", str(elf))
    header_output = run_readelf(readelf, "-hW", str(elf))

    sections = parse_sections(section_output)
    symbols = parse_symbols(symbol_output)
    errors: list[str] = []

    try:
        iram = sections[".iram.text"]
        dram = sections[".dram.rodata"]
        flash_rodata = sections[".flash.rodata"]
        flash_text = sections[".flash.text"]
        stack = sections[".stack"]
    except KeyError as error:
        raise ValueError(f"required output section {error.args[0]} is missing") from error

    if not (
        XIP_START <= flash_rodata.address
        and flash_rodata.address + flash_rodata.size <= XIP_END
    ):
        errors.append(
            f".flash.rodata 0x{flash_rodata.address:08x}.."
            f"0x{flash_rodata.address + flash_rodata.size:08x} is outside DROM"
        )
    for name in (".rodata", ".eh_frame"):
        section = sections.get(name)
        if section is not None and section.size != 0:
            errors.append(
                f"ordinary {name} remains outside .flash.rodata "
                f"({section.size} byte at 0x{section.address:08x})"
            )

    if not (
        XIP_START <= flash_text.address
        and flash_text.address + flash_text.size <= XIP_END
    ):
        errors.append(
            f".flash.text 0x{flash_text.address:08x}.."
            f"0x{flash_text.address + flash_text.size:08x} is outside IROM"
        )
    ordinary_text = sections.get(".text")
    if ordinary_text is not None and ordinary_text.size != 0:
        errors.append(
            f"ordinary .text remains outside .flash.text ({ordinary_text.size} byte at "
            f"0x{ordinary_text.address:08x})"
        )

    data = sections.get(".data")
    if data is None:
        errors.append("required output section .data is missing")
    elif not (RAM_START <= data.address < RAM_END):
        errors.append(f".data is not in internal RAM (0x{data.address:08x})")

    for section in (iram, dram, data, sections.get(".bss"), stack):
        if section is None:
            continue
        if not (
            RAM_START <= section.address
            and section.address + section.size <= RAM_END
        ):
            errors.append(
                f"{section.name} 0x{section.address:08x}.."
                f"0x{section.address + section.size:08x} is outside safe RAM"
            )
    if stack.size < MIN_STACK_BYTES:
        errors.append(
            f"stack is {stack.size} byte, below the {MIN_STACK_BYTES}-byte minimum"
        )

    try:
        data_start = symbol_value(symbols, "__sdata")
        data_load = symbol_value(symbols, "__sidata")
    except ValueError as error:
        errors.append(str(error))
    else:
        if data_load != data_start:
            errors.append(
                f".data load address 0x{data_load:08x} differs from its direct-load "
                f"RAM address 0x{data_start:08x}"
            )

    entry_match = ENTRY_RE.search(header_output)
    if entry_match is None:
        errors.append("ELF entry point was not found")
        entry = 0
    else:
        entry = int(entry_match.group(1), 16)
        if not iram.contains(entry):
            errors.append(f"ELF entry 0x{entry:08x} is outside .iram.text")

    required_exact = [
        "_start",
        "_start_rust",
        "hal_main",
        "_start_trap",
        "ExceptionHandler",
        "iram_psram_init",
        "iram_cache_writeback_invalidate",
        "esp32p4_interrupt",
    ]
    for name in required_exact:
        symbol = symbols.get(name)
        if symbol is None:
            errors.append(f"required IRAM symbol {name} is missing")
        elif not iram.contains(symbol.value):
            errors.append(f"{name} is at 0x{symbol.value:08x}, outside .iram.text")

    for name in (
        "XIP_DROM_PROBE_PRE",
        "XIP_DROM_PROBE_POST",
        "xip_instruction_probe_pre",
        "xip_instruction_probe_post",
    ):
        symbol = symbols.get(name)
        if symbol is None:
            errors.append(f"required XIP symbol {name} is missing")
        elif not (XIP_START <= symbol.value < XIP_END):
            errors.append(f"{name} is at 0x{symbol.value:08x}, outside XIP")

    pre_drom = symbols.get("XIP_DROM_PROBE_PRE")
    post_drom = symbols.get("XIP_DROM_PROBE_POST")
    if pre_drom is not None and post_drom is not None:
        if post_drom.value - pre_drom.value < 64:
            errors.append("pre/post DROM probes share a 64-byte cache line")
    pre_irom = symbols.get("xip_instruction_probe_pre")
    post_irom = symbols.get("xip_instruction_probe_post")
    if pre_irom is not None and post_irom is not None:
        if post_irom.value - pre_irom.value < 64:
            errors.append("pre/post IROM probes share a 64-byte cache line")

    unwind = [symbol for name, symbol in symbols.items() if name.endswith("rust_begin_unwind")]
    if len(unwind) != 1:
        errors.append(f"expected one rust_begin_unwind symbol, found {len(unwind)}")
    elif not iram.contains(unwind[0].value):
        errors.append("rust_begin_unwind is outside .iram.text")

    try:
        critical_start = symbol_value(symbols, "__sflash_critical")
        critical_end = symbol_value(symbols, "__eflash_critical")
    except ValueError as error:
        errors.append(str(error))
        critical_start = critical_end = 0

    if not (
        iram.address <= critical_start < critical_end <= iram.address + iram.size
    ):
        errors.append(
            f"flash-critical range 0x{critical_start:08x}..0x{critical_end:08x} "
            "is outside .iram.text"
        )

    allowed_ram_sections = {".iram.text", ".dram.rodata", ".data", ".bss", ".uninit", ".stack"}
    relocations = parse_relocations(relocation_output, ".rela.iram.text")
    critical_relocations = [
        relocation
        for relocation in relocations
        if critical_start <= relocation.offset < critical_end
    ]
    for relocation in critical_relocations:
        target_section = containing_section(sections, relocation.target_value)
        if target_section is not None and target_section.name in allowed_ram_sections:
            continue
        if ROM_START <= relocation.target_value < ROM_END:
            continue
        if relocation.target_value == 0 and relocation.target_name.startswith("_"):
            continue
        location = target_section.name if target_section is not None else "no alloc section"
        errors.append(
            f"critical relocation at 0x{relocation.offset:08x} targets "
            f"{relocation.target_name} (0x{relocation.target_value:08x}, {location})"
        )

    print(
        f"ELF layout: entry=0x{entry:08x} "
        f"IRAM={iram.size} DRAM-rodata={dram.size} "
        f"DROM={flash_rodata.size} IROM={flash_text.size} stack={stack.size}"
    )
    print(
        f"  flash-critical=0x{critical_start:08x}..0x{critical_end:08x} "
        f"relocations={len(critical_relocations)}"
    )

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        raise ValueError(f"ELF layout has {len(errors)} error(s)")

    print(
        "ELF layout: ok (ordinary text/rodata in IROM/DROM; "
        "entry/trap/PSRAM in IRAM; critical closure is RAM/ROM-only)"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("elf", type=Path)
    parser.add_argument("--readelf", default="llvm-readelf")
    args = parser.parse_args()
    try:
        validate(args.elf, args.readelf)
    except (OSError, RuntimeError, ValueError) as error:
        print(f"check_elf_layout: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
