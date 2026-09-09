#!/usr/bin/env python3
"""Generate the deterministic T5A4 UI-font blob (Pillow 10.2.0)."""

from pathlib import Path
import hashlib, struct, zlib
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
VENDOR = Path(__file__).parent / "vendor"
OUT = ROOT / "ui-font" / "data" / "tab5-ui-fonts.bin"
REPORT = OUT.with_suffix(".txt")
FACES = ((0, "DejaVu Sans", "DejaVuSans.ttf"), (1, "DejaVu Sans Mono", "DejaVuSansMono.ttf"))
SIZES = ((16, 13, 12), (24, 20, 19), (32, 27, 26))  # box, raster em, baseline
CODEPOINTS = tuple(
    list(range(0x20, 0x7F)) + list(range(0xA0, 0xAD)) + list(range(0xAE, 0x100))
    + list(range(0x2010, 0x2016)) + list(range(0x2018, 0x2020))
    + [0x2022, 0x2026, 0x2032, 0x2033, 0x20AC, 0x2122]
)
HEADER = 32
STRIKE_BYTES = 16
GLYPH_BYTES = 16
SOURCE_HASHES = {
    "DejaVuSans.ttf": "ae7b7855e115a5966d8b1b3f80f254ccc117ec86f9965e202ee2940453837280",
    "DejaVuSansMono.ttf": "c805f9436dbc268644c1d9584f01a601a653e028e08fd74b9b949f6cf8304d88",
}

def render(font, cp, box, baseline):
    char = chr(cp)
    if cp in (0x20, 0xA0):
        return 0, 0, 0, 0, max(1, round(font.getlength(" "))), b""
    left, top, right, bottom = font.getbbox(char, anchor="ls")
    width, height = max(0, right-left), max(0, bottom-top)
    image = Image.new("L", (max(1, width), max(1, height)), 0)
    ImageDraw.Draw(image).text((-left, -top), char, font=font, fill=255, anchor="ls")
    # Pillow's top/bottom are relative to the baseline; retain bearings. The
    # chosen raster ems make all required glyphs fit the nominal line box.
    y = baseline + top
    if y < 0 or y + height > box:
        raise ValueError(f"U+{cp:04X} exceeds {box}px box: y={y}, h={height}")
    pixels = image.load()
    nibbles = [round(pixels[x, row] * 15 / 255) for x in range(width) for row in range(height)]
    packed = bytearray((len(nibbles)+1)//2)
    for i, value in enumerate(nibbles):
        packed[i//2] |= value << (4 if i % 2 == 0 else 0)
    return left, y, width, height, max(1, round(font.getlength(char))), bytes(packed)

def main():
    assert len(CODEPOINTS) == 210 and 0xAD not in CODEPOINTS
    strikes, glyphs, bitmaps, lines = [], [], bytearray(), []
    intermediate = {0: False, 1: False}
    for face_id, name, filename in FACES:
        source = VENDOR / filename
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        if digest != SOURCE_HASHES[filename]:
            raise ValueError(f"source hash differs for {filename}: {digest}")
        for box, em, baseline in SIZES:
            font = ImageFont.truetype(source, em)
            first = len(glyphs)
            advances = set()
            bitmap_start = len(bitmaps)
            for cp in CODEPOINTS:
                left, y, width, height, advance, bitmap = render(font, cp, box, baseline)
                if not (-128 <= left <= 127 and width <= 255 and height <= 255 and advance <= 255):
                    raise ValueError(f"metrics do not fit U+{cp:04X}")
                offset = len(bitmaps)
                bitmaps.extend(bitmap)
                glyphs.append((cp, left, y, width, height, advance, offset, len(bitmap)))
                advances.add(advance)
                intermediate[face_id] |= any((byte & 15) not in (0, 15) or (byte >> 4) not in (0, 15) for byte in bitmap)
            if face_id == 1 and len(advances) != 1:
                raise ValueError(f"monospace {box}px advances differ: {advances}")
            strikes.append((face_id, box, baseline, box-baseline, first, len(CODEPOINTS)))
            lines.append(f"{name} {box}px: glyphs=210 bitmap={len(bitmaps)-bitmap_start} bytes advance={min(advances)}..{max(advances)}")
    if not all(intermediate.values()): raise ValueError("face has no intermediate A4 coverage")
    strikes_offset = HEADER
    glyphs_offset = strikes_offset + len(strikes)*STRIKE_BYTES
    bitmaps_offset = glyphs_offset + len(glyphs)*GLYPH_BYTES
    payload = bytearray()
    for face, size, ascent, descent, first, count in strikes:
        payload += struct.pack("<BBBBIHHI", face,size,ascent,descent,first,count,0,0)
    for cp,left,y,w,h,advance,offset,length in glyphs:
        payload += struct.pack("<IbbBBBBIH", cp,left,y,w,h,advance,0,offset,length)
    payload += bitmaps
    crc = zlib.crc32(payload)
    header = struct.pack("<4sHHHHIIIII", b"T5A4",1,HEADER,len(strikes),len(glyphs),strikes_offset,glyphs_offset,bitmaps_offset,HEADER+len(payload),crc)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_bytes(header+payload)
    digest = hashlib.sha256(header+payload).hexdigest()
    REPORT.write_text("\n".join([
        "tab5 UI fonts", "format: T5A4 v1, column-major A4", "generator: tools/ui-font/generate.py; Pillow 10.2.0",
        "faces: DejaVu Sans 2.37; DejaVu Sans Mono 2.37", "code points: 210 per strike", *lines,
        f"metadata={bitmaps_offset} bytes bitmap={len(bitmaps)} bytes total={len(header)+len(payload)} bytes",
        f"crc32={crc:08x}", f"sha256={digest}", "intermediate coverage: yes (both faces)", ""
    ]), encoding="utf-8")
    print(REPORT.read_text(), end="")

if __name__ == "__main__": main()
