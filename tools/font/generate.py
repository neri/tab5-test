#!/usr/bin/env python3
"""Turn Unifont-JP's BDF into the compact bitmap the firmware links into DROM.

The firmware never parses BDF: this runs on the host, and the checked-in
`font/data/tab5font16.bin` is what `include_bytes!` picks up. See
`docs/FONT_MIGRATION_PLAN.md` for why, and `README.md` next to this file for
the format.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import struct
import sys
import unicodedata
import zlib
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPOSITORY = HERE.parent.parent

SOURCE = HERE / "vendor" / "unifont_jp-17.0.05.bdf.gz"
# The hash of the *decompressed* BDF. Renaming or recompressing the archive
# changes the archive's hash but not this one.
SOURCE_SHA256 = "044463a47a5b320a1281dcd15fcb3010d6a4ec19603e4193bf28d12909cd009c"
MANIFEST = HERE / "ascii-manifest.txt"
OUTPUT = REPOSITORY / "font" / "data" / "tab5font16.bin"
REPORT = REPOSITORY / "font" / "data" / "tab5font16.txt"

MAGIC = b"T5F1"
FORMAT_VERSION = 1
HEADER_BYTES = 32
GLYPH_BYTES = 32
RANGE_BYTES = 8
CELL = 16
# The direct-ROM ASCII blob is intentionally bounded. Generation fails above
# this ceiling instead of quietly dropping characters to fit.
SIZE_LIMIT = 16 * 1024


class GenerationError(Exception):
    pass


@dataclass
class SourceGlyph:
    code_point: int
    device_width: int
    columns: list[int]


def read_source(path: Path) -> tuple[dict[int, SourceGlyph], dict[str, str]]:
    """Parse the BDF into 16x16 column-major glyphs keyed by code point."""
    raw = gzip.decompress(path.read_bytes())
    digest = hashlib.sha256(raw).hexdigest()
    if digest != SOURCE_SHA256:
        raise GenerationError(
            f"{path.name}: decompressed SHA-256 is {digest},\n"
            f"  expected {SOURCE_SHA256} (see vendor/PROVENANCE.md)"
        )
    text = raw.decode("latin-1")
    properties: dict[str, str] = {}
    glyphs: dict[int, SourceGlyph] = {}

    lines = text.splitlines()
    index = 0
    in_properties = False
    while index < len(lines):
        line = lines[index]
        index += 1
        if line.startswith("STARTPROPERTIES"):
            in_properties = True
            continue
        if line.startswith("ENDPROPERTIES"):
            in_properties = False
            continue
        if in_properties:
            key, _, value = line.partition(" ")
            properties[key] = value.strip().strip('"')
            continue
        if not line.startswith("STARTCHAR"):
            continue
        code_point = -1
        device_width = 0
        bounding_box = (0, 0, 0, 0)
        rows: list[int] = []
        while index < len(lines):
            entry = lines[index]
            index += 1
            if entry.startswith("ENCODING"):
                code_point = int(entry.split()[1])
            elif entry.startswith("DWIDTH"):
                device_width = int(entry.split()[1])
            elif entry.startswith("BBX"):
                bounding_box = tuple(int(value) for value in entry.split()[1:5])
            elif entry.startswith("BITMAP"):
                while index < len(lines) and not lines[index].startswith("ENDCHAR"):
                    rows.append(int(lines[index].strip() or "0", 16))
                    index += 1
            elif entry.startswith("ENDCHAR"):
                break
        if code_point < 0:
            continue
        glyphs[code_point] = SourceGlyph(
            code_point=code_point,
            device_width=device_width,
            columns=to_columns(code_point, bounding_box, rows, int(properties["FONT_ASCENT"])),
        )
    return glyphs, properties


def to_columns(
    code_point: int,
    bounding_box: tuple[int, int, int, int],
    rows: list[int],
    ascent: int,
) -> list[int]:
    """Normalise one BDF glyph onto a 16x16 canvas and transpose it.

    The framebuffer maps increasing logical X onto decreasing native address,
    so text is drawn one vertical run at a time and the data is stored column
    by column. Bit 0 of each column is its top pixel, matching the 5x7 font
    this replaces.
    """
    width, height, x_offset, y_offset = bounding_box
    top = ascent - (y_offset + height)
    if x_offset < 0 or top < 0 or x_offset + width > CELL or top + height > CELL:
        raise GenerationError(
            f"U+{code_point:04X}: BBX {bounding_box} does not fit the {CELL}x{CELL} canvas"
        )
    if len(rows) != height:
        raise GenerationError(
            f"U+{code_point:04X}: BITMAP has {len(rows)} rows, BBX says {height}"
        )
    # Each BDF row is padded to whole bytes, so its most significant bit is the
    # leftmost pixel of a `ceil(width / 8) * 8` wide field.
    padded = (width + 7) // 8 * 8
    columns = [0] * CELL
    for row_index, bits in enumerate(rows):
        canvas_row = top + row_index
        for column_index in range(width):
            if bits & (1 << (padded - 1 - column_index)):
                columns[x_offset + column_index] |= 1 << canvas_row
    return columns


def read_manifest(path: Path, seen: set[Path] | None = None) -> dict[int, str]:
    """Expand the manifest into code point -> the line that asked for it."""
    seen = seen if seen is not None else set()
    resolved = path.resolve()
    if resolved in seen:
        raise GenerationError(f"{path}: included more than once")
    seen.add(resolved)
    wanted: dict[int, str] = {}
    for number, raw in enumerate(path.read_text(encoding="ascii").splitlines(), start=1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        origin = f"{path.name}:{number}"
        if line.startswith("include "):
            included = (path.parent / line[len("include ") :].strip()).resolve()
            wanted.update(read_manifest(included, seen))
            continue
        first, _, last = line.partition("-")
        try:
            start = int(first, 16)
            end = int(last, 16) if last else start
        except ValueError as error:
            raise GenerationError(f"{origin}: cannot parse {line!r}") from error
        if end < start:
            raise GenerationError(f"{origin}: range {line!r} runs backwards")
        for code_point in range(start, end + 1):
            wanted.setdefault(code_point, origin)
    return wanted


def advance_of(code_point: int, glyph: SourceGlyph) -> int:
    """The pen movement for one glyph, in pixels.

    Combining marks take no advance of their own: they are painted over the
    glyph that precedes them. Everything else is the source font's own device
    width, which Unifont-JP keeps at exactly 8 or 16.
    """
    if unicodedata.category(chr(code_point)) in ("Mn", "Me"):
        return 0
    if glyph.device_width not in (8, 16):
        raise GenerationError(
            f"U+{code_point:04X}: DWIDTH {glyph.device_width} is neither 8 nor 16"
        )
    return glyph.device_width


def to_runs(code_points: list[int]) -> list[tuple[int, int, int]]:
    """(first code point, length, index of the first glyph) for each run."""
    runs: list[tuple[int, int, int]] = []
    start = 0
    for index in range(1, len(code_points) + 1):
        if index < len(code_points) and code_points[index] == code_points[index - 1] + 1:
            continue
        runs.append((code_points[start], index - start, start))
        start = index
    return runs


def build(
    wanted: dict[int, str], source: dict[int, SourceGlyph]
) -> tuple[bytes, list[int], list[int], list[tuple[int, int, int]], list[int]]:
    outside_bmp = sorted(code_point for code_point in wanted if code_point > 0xFFFF)
    missing = sorted(
        code_point
        for code_point in wanted
        if code_point <= 0xFFFF and code_point not in source
    )
    code_points = sorted(
        code_point
        for code_point in wanted
        if code_point <= 0xFFFF and code_point in source
    )
    if not code_points:
        raise GenerationError("the manifest selected no glyphs at all")
    if len(code_points) > 0xFFFF:
        raise GenerationError(
            f"{len(code_points)} glyphs exceeds the u16 glyph index in the range table"
        )

    advances = [advance_of(code_point, source[code_point]) for code_point in code_points]
    runs = to_runs(code_points)
    if len(runs) > 0xFFFFFFFF:
        raise GenerationError("too many ranges for the u32 range count")
    for _, length, _ in runs:
        if length > 0xFFFF:
            raise GenerationError("a range is longer than the u16 length field")

    ranges_offset = HEADER_BYTES
    bitmaps_offset = ranges_offset + RANGE_BYTES * len(runs)
    advances_offset = bitmaps_offset + GLYPH_BYTES * len(code_points)

    body = bytearray()
    for first, length, first_glyph in runs:
        body += struct.pack("<IHH", first, length, first_glyph)
    for code_point in code_points:
        body += struct.pack("<16H", *source[code_point].columns)
    body += bytes(advances)

    header = struct.pack(
        "<4sHHIIIIII",
        MAGIC,
        FORMAT_VERSION,
        HEADER_BYTES,
        len(code_points),
        len(runs),
        ranges_offset,
        bitmaps_offset,
        advances_offset,
        zlib.crc32(body),
    )
    if len(header) != HEADER_BYTES:
        raise GenerationError(f"header is {len(header)} bytes, expected {HEADER_BYTES}")
    return bytes(header) + bytes(body), missing, outside_bmp, runs, advances


def check_console_repertoire(
    code_points: list[int], advances: list[int]
) -> None:
    index = {code_point: position for position, code_point in enumerate(code_points)}
    for code_point in range(0x20, 0x7F):
        position = index.get(code_point)
        if position is None:
            raise GenerationError(
                f"U+{code_point:04X} is in printable ASCII but not in the subset"
            )
        if advances[position] != 8:
            raise GenerationError(
                f"U+{code_point:04X} is printable ASCII but is "
                f"{advances[position]} pixels wide, not 8"
            )


def write_report(
    path: Path,
    properties: dict[str, str],
    code_points: list[int],
    advances: list[int],
    runs: list[tuple[int, int, int]],
    missing: list[int],
    outside_bmp: list[int],
    payload: bytes,
) -> str:
    halfwidth = sum(1 for advance in advances if advance == 8)
    fullwidth = sum(1 for advance in advances if advance == 16)
    combining = sum(1 for advance in advances if advance == 0)
    lines = [
        "tab5font16.bin -- generated by tools/font/generate.py, do not hand-edit.",
        "",
        f"source           GNU Unifont Japanese {properties.get('FONT_VERSION', '?')}",
        f"source sha256    {SOURCE_SHA256}",
        f"unicodedata      {unicodedata.unidata_version}",
        f"format version   {FORMAT_VERSION}",
        "",
        f"glyphs           {len(code_points)}",
        f"  halfwidth      {halfwidth}",
        f"  fullwidth      {fullwidth}",
        f"  combining      {combining}",
        f"ranges           {len(runs)}",
        f"highest code     U+{code_points[-1]:04X}",
        f"bytes            {len(payload)} ({len(payload) / 1024:.1f} KiB "
        f"of the {SIZE_LIMIT // 1024} KiB ceiling)",
        f"crc32            {zlib.crc32(payload[HEADER_BYTES:]):08x}",
        "",
        f"requested but outside the BMP, dropped: {len(outside_bmp)}",
    ]
    lines += [f"  {' '.join(f'U+{c:04X}' for c in outside_bmp[i:i + 8])}" for i in range(0, len(outside_bmp), 8)]
    lines.append(f"requested but absent from the source font, dropped: {len(missing)}")
    lines += [f"  {' '.join(f'U+{c:04X}' for c in missing[i:i + 8])}" for i in range(0, len(missing), 8)]
    report = "\n".join(lines) + "\n"
    path.write_text(report, encoding="ascii")
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail instead of writing if the checked-in output is out of date",
    )
    arguments = parser.parse_args()

    try:
        source, properties = read_source(SOURCE)
        wanted = read_manifest(MANIFEST)
        payload, missing, outside_bmp, runs, advances = build(wanted, source)
        code_points = sorted(
            code_point
            for code_point in wanted
            if code_point <= 0xFFFF and code_point in source
        )
        check_console_repertoire(code_points, advances)
        if len(payload) > SIZE_LIMIT:
            raise GenerationError(
                f"{len(payload)} bytes exceeds the {SIZE_LIMIT} byte ceiling. "
                "Widen the DROM budget or narrow the manifest, and record the "
                "decision in docs/FONT_MIGRATION_PLAN.md."
            )
    except GenerationError as error:
        print(f"font generation failed: {error}", file=sys.stderr)
        return 1

    if arguments.check:
        if not OUTPUT.exists() or OUTPUT.read_bytes() != payload:
            print(f"{OUTPUT} is out of date; rerun tools/font/generate.py", file=sys.stderr)
            return 1
        print(f"{OUTPUT}: up to date ({len(payload)} bytes)")
        return 0

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_bytes(payload)
    print(write_report(REPORT, properties, code_points, advances, runs, missing, outside_bmp, payload))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
