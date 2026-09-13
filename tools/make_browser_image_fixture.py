#!/usr/bin/env python3
"""Create the Stage 3 local PNG/HTML fixture in a mounted volume."""

import argparse
import binascii
import struct
import zlib
from pathlib import Path


def chunk(kind: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + kind + data + struct.pack(
        ">I", binascii.crc32(kind + data) & 0xFFFFFFFF
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    args.directory.mkdir(parents=True, exist_ok=True)

    width, height = 192, 128
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            # A small test card: gradient sky, translucent sun, two mountain
            # silhouettes, one-pixel grid lines and bottom colour bars.
            r, g, b, a = 24 + x * 72 // width, 80 + y * 80 // height, 190 + x * 50 // width, 255
            if (x - 148) ** 2 + (y - 28) ** 2 < 18**2:
                r, g, b, a = 255, 210, 32, 176
            ridge = 78 - abs(x - 62) * 3 // 5
            ridge2 = 92 - abs(x - 132) * 2 // 5
            if y > min(ridge, ridge2):
                r, g, b, a = (35, 92, 64, 255) if y < 104 else (20, 48, 34, 255)
            if y >= 112:
                bars = ((230, 40, 50), (250, 190, 30), (40, 190, 90), (35, 110, 230), (170, 55, 210), (245, 245, 245))
                r, g, b = bars[min(x * len(bars) // width, len(bars) - 1)]
            if x % 32 == 0 or y % 32 == 0:
                r, g, b, a = 255, 255, 255, 128
            rows.extend((r, g, b, a))
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(rows))
        + chunk(b"IEND", b"")
    )
    (args.directory / "stage3.png").write_bytes(png)
    html = "<title>local PNG</title><h1>Local PNG</h1>"
    html += "<p>Scroll checkpoint before image.</p>" * 12
    html += "<img src='stage3.png' alt='local PNG failed' width='384' height='256'>"
    html += "<p>The test card above is decoded from the adjacent file.</p>"
    html += "<p>Scroll checkpoint after image.</p>" * 18
    (args.directory / "stage3.html").write_text(html, encoding="utf-8")


if __name__ == "__main__":
    main()
