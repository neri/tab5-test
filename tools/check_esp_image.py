#!/usr/bin/env python3
"""Validate the ESP32-P4 application image layout used by this project."""

from __future__ import annotations

import argparse
import struct
import sys
from dataclasses import dataclass
from pathlib import Path


APP_MAGIC = 0xE9
APP_DESC_MAGIC = 0xABCD5432
APP_HEADER_BYTES = 24
SEGMENT_HEADER_BYTES = 8
DEFAULT_APP_OFFSET = 0x10000
XIP_START = 0x40000000
XIP_END = 0x40400000


@dataclass(frozen=True)
class Segment:
    index: int
    load_address: int
    data_length: int
    payload_offset: int

    @property
    def is_xip(self) -> bool:
        return XIP_START <= self.load_address < XIP_END


def integer(value: str) -> int:
    return int(value, 0)


def parse_segments(image: bytes, app_offset: int) -> tuple[int, list[Segment]]:
    if app_offset < 0 or app_offset + APP_HEADER_BYTES > len(image):
        raise ValueError(f"application offset 0x{app_offset:x} is outside the image")

    header = image[app_offset : app_offset + APP_HEADER_BYTES]
    if header[0] != APP_MAGIC:
        raise ValueError(
            f"application magic at 0x{app_offset:x} is 0x{header[0]:02x}, expected 0xe9"
        )

    segment_count = header[1]
    cursor = app_offset + APP_HEADER_BYTES
    segments: list[Segment] = []
    for index in range(segment_count):
        if cursor + SEGMENT_HEADER_BYTES > len(image):
            raise ValueError(f"segment {index} header extends past end of image")
        load_address, data_length = struct.unpack_from("<II", image, cursor)
        payload_offset = cursor + SEGMENT_HEADER_BYTES
        payload_end = payload_offset + data_length
        if payload_end > len(image):
            raise ValueError(f"segment {index} payload extends past end of image")
        segments.append(Segment(index, load_address, data_length, payload_offset))
        cursor = payload_end

    return segment_count, segments


def validate(image: bytes, app_offset: int) -> list[Segment]:
    segment_count, segments = parse_segments(image, app_offset)
    errors: list[str] = []
    xip_segments = [segment for segment in segments if segment.is_xip]

    if len(xip_segments) != 2:
        errors.append(f"found {len(xip_segments)} XIP segments, expected exactly 2")

    if not xip_segments:
        errors.append("application descriptor segment is missing")
    else:
        first = xip_segments[0]
        if first.load_address != XIP_START + 0x20:
            errors.append(
                f"first XIP segment loads at 0x{first.load_address:08x}, expected 0x40000020"
            )
        if first.data_length < 4:
            errors.append("first XIP segment is too short to contain the app descriptor")
        else:
            descriptor_magic = struct.unpack_from("<I", image, first.payload_offset)[0]
            if descriptor_magic != APP_DESC_MAGIC:
                errors.append(
                    f"app descriptor magic is 0x{descriptor_magic:08x}, "
                    f"expected 0x{APP_DESC_MAGIC:08x}"
                )

    for segment in xip_segments:
        physical_page_offset = (segment.payload_offset - app_offset) & 0xFFFF
        virtual_page_offset = segment.load_address & 0xFFFF
        if physical_page_offset != virtual_page_offset:
            errors.append(
                f"segment {segment.index} page offset mismatch: physical "
                f"0x{physical_page_offset:04x}, virtual 0x{virtual_page_offset:04x}"
            )

    print(f"ESP image: app_offset=0x{app_offset:x} segments={segment_count}")
    for segment in segments:
        kind = "XIP" if segment.is_xip else "RAM"
        relative_payload = segment.payload_offset - app_offset
        print(
            f"  [{segment.index}] {kind} load=0x{segment.load_address:08x} "
            f"size=0x{segment.data_length:x} payload=+0x{relative_payload:x}"
        )

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        raise ValueError(f"image layout has {len(errors)} error(s)")

    print("ESP image layout: ok (exactly two page-aligned XIP segments)")
    return segments


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", type=Path, help="merged image produced by espflash save-image")
    parser.add_argument(
        "--app-offset",
        type=integer,
        default=DEFAULT_APP_OFFSET,
        help="application image offset (default: 0x10000)",
    )
    args = parser.parse_args()

    try:
        validate(args.image.read_bytes(), args.app_offset)
    except (OSError, ValueError) as error:
        print(f"check_esp_image: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
