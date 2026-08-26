#!/usr/bin/env python3
"""Regenerate `repertoire/jisx0213-plane1.txt` from Python's EUC-JIS-2004 codec.

The font subset is defined by Unicode ranges (`manifest.txt`), but JIS X 0213's
kanji are scattered across CJK Unified Ideographs rather than forming ranges of
their own.  Naming the standard in the manifest and expanding it at generation
time would make the generated font depend on the CPython version that happened
to run the tool, so the expansion is checked in instead and this script is what
reproduces it.

Plane 1 of JIS X 0213 is levels 1 to 3.  Plane 2 (level 4) is deliberately not
expanded: almost all of it lives outside the BMP, which the Unifont-JP source
does not cover -- see `docs/FONT_MIGRATION_PLAN.md`.

Characters that JIS X 0213 encodes as a base plus a combining mark decode to
more than one scalar; every scalar involved is included.
"""

from __future__ import annotations

import sys
import unicodedata
from pathlib import Path

OUTPUT = Path(__file__).with_name("repertoire") / "jisx0213-plane1.txt"
CODEC = "euc_jis_2004"


def plane1_scalars() -> set[int]:
    """Every Unicode scalar reachable through plane 1 of JIS X 0213.

    EUC-JIS-2004 encodes plane 1 as two bytes in 0xA1..0xFE; the 0x8F prefix
    (plane 2) and the 0x8E prefix (halfwidth katakana, which JIS X 0201 defines
    rather than JIS X 0213) are both left out.
    """
    scalars: set[int] = set()
    for first in range(0xA1, 0xFF):
        for second in range(0xA1, 0xFF):
            try:
                text = bytes((first, second)).decode(CODEC)
            except UnicodeDecodeError:
                continue
            scalars.update(ord(character) for character in text)
    return scalars


def runs(scalars: list[int]) -> list[tuple[int, int]]:
    result: list[tuple[int, int]] = []
    start = previous = scalars[0]
    for scalar in scalars[1:]:
        if scalar == previous + 1:
            previous = scalar
            continue
        result.append((start, previous))
        start = previous = scalar
    result.append((start, previous))
    return result


def main() -> int:
    scalars = sorted(plane1_scalars())
    if not scalars:
        print("no scalars decoded; is the euc_jis_2004 codec available?", file=sys.stderr)
        return 1
    outside_bmp = [scalar for scalar in scalars if scalar > 0xFFFF]
    lines = [
        "# JIS X 0213 plane 1 (levels 1-3), expanded from Python's",
        f"# {CODEC} codec by tools/font/derive_jisx0213.py. Do not hand-edit:",
        "# rerun the script instead, and review the diff.",
        f"# unicodedata {unicodedata.unidata_version}",
        f"# scalars {len(scalars)} runs {len(runs(scalars))} outside-bmp {len(outside_bmp)}",
    ]
    for first, last in runs(scalars):
        lines.append(f"{first:04X}-{last:04X}" if first != last else f"{first:04X}")
    OUTPUT.parent.mkdir(exist_ok=True)
    OUTPUT.write_text("\n".join(lines) + "\n", encoding="ascii")
    print(f"{OUTPUT}: {len(scalars)} scalars, {len(runs(scalars))} runs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
