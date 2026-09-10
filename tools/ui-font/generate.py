#!/usr/bin/env python3
"""Generate the deterministic T5A4 UI-font blob (Pillow 10.2.0)."""

from pathlib import Path
import argparse, hashlib, runpy, struct, unicodedata, zlib
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
VENDOR = Path(__file__).parent / "vendor"
OUT = ROOT / "ui-font" / "data" / "tab5-ui-fonts.bin"
REPORT = OUT.with_suffix(".txt")
FACES = ((0, "DejaVu Sans", "DejaVuSans.ttf"), (1, "DejaVu Sans Mono", "DejaVuSansMono.ttf"))
JAPANESE_FACE = (2, "Noto Sans CJK JP", "NotoSansCJK-Regular.ttc")
SIZES = ((16, 13, 12), (24, 20, 19), (32, 27, 26))  # box, raster em, baseline
JAPANESE_SIZE = (16, 13, 12)
JAPANESE_MANIFEST = Path(__file__).parent / "japanese-manifest.txt"
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
    "NotoSansCJK-Regular.ttc": "b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a",
}

def raster(font, cp):
    char = chr(cp)
    if cp in (0x20, 0xA0):
        return 0, 0, 0, 0, b""
    left, top, right, bottom = font.getbbox(char, anchor="ls")
    width, height = max(0, right-left), max(0, bottom-top)
    image = Image.new("L", (max(1, width), max(1, height)), 0)
    ImageDraw.Draw(image).text((-left, -top), char, font=font, fill=255, anchor="ls")
    pixels = image.load()
    nibbles = [round(pixels[x, row] * 15 / 255) for x in range(width) for row in range(height)]
    packed = bytearray((len(nibbles)+1)//2)
    for i, value in enumerate(nibbles):
        packed[i//2] |= value << (4 if i % 2 == 0 else 0)
    return left, top, width, height, bytes(packed)

def render(font, cp, box, baseline, forced_advance=None):
    left, top, width, height, bitmap = raster(font, cp)
    y = baseline + top
    if y < 0 or y + height > box:
        raise ValueError(f"U+{cp:04X} exceeds {box}px box: y={y}, h={height}")
    advance = forced_advance if forced_advance is not None else max(1, round(font.getlength(chr(cp))))
    return left, y, width, height, advance, bitmap

def unifont_fallback(glyph, advance):
    ink = [(x, y) for x, column in enumerate(glyph.columns) for y in range(16) if column & (1 << y)]
    if not ink:
        return 0, 0, 0, 0, advance, b""
    left, right = min(x for x, _ in ink), max(x for x, _ in ink) + 1
    top, bottom = min(y for _, y in ink), max(y for _, y in ink) + 1
    nibbles = [15 if glyph.columns[x] & (1 << y) else 0 for x in range(left, right) for y in range(top, bottom)]
    packed = bytearray((len(nibbles) + 1) // 2)
    for index, value in enumerate(nibbles):
        packed[index // 2] |= value << (4 if index % 2 == 0 else 0)
    return left, top, right-left, bottom-top, advance, bytes(packed)

def japanese_source():
    module_path = ROOT / "tools" / "font" / "generate.py"
    module = runpy.run_path(str(module_path), run_name="tab5_unifont_generator")
    source, _ = module["read_source"](module["SOURCE"])
    wanted = module["read_manifest"](JAPANESE_MANIFEST)
    return module, source, sorted(wanted)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail instead of writing if the generated outputs are out of date",
    )
    arguments = parser.parse_args()
    assert len(CODEPOINTS) == 210 and 0xAD not in CODEPOINTS
    strikes, glyphs, bitmaps, lines = [], [], bytearray(), []
    intermediate = {0: False, 1: False, 2: False}
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
    module, source, japanese = japanese_source()
    face_id, name, filename = JAPANESE_FACE
    font_path = VENDOR / filename
    digest = hashlib.sha256(font_path.read_bytes()).hexdigest()
    if digest != SOURCE_HASHES[filename]:
        raise ValueError(f"source hash differs for {filename}: {digest}")
    box, em, baseline = JAPANESE_SIZE
    font = ImageFont.truetype(font_path, em, index=0)
    missing_raster = raster(font, 0x10ffff)
    first = len(glyphs)
    bitmap_start = len(bitmaps)
    outline_count = fallback_count = 0
    dropped = []
    for cp in japanese:
        source_glyph = source.get(cp)
        advance = (
            module["advance_of"](cp, source_glyph)
            if source_glyph is not None
            else (0 if unicodedata.combining(chr(cp)) else 16)
        )
        if advance == 0 and source_glyph is not None:
            # Standalone outline rendering gives combining marks bearings for
            # an implicit base (U+0301 becomes wider than a Japanese cell in
            # Noto). Preserve Unifont's explicit overlay-cell placement.
            left, y, width, height, advance, bitmap = unifont_fallback(source_glyph, advance)
            fallback_count += 1
        else:
            try:
                candidate = raster(font, cp)
                if candidate == missing_raster:
                    raise ValueError("not present in Noto")
                left, y, width, height, advance, bitmap = render(font, cp, box, baseline, advance)
                outline_count += 1
                intermediate[face_id] |= any((byte & 15) not in (0, 15) or (byte >> 4) not in (0, 15) for byte in bitmap)
            except ValueError:
                if source_glyph is None:
                    dropped.append(cp)
                    continue
                left, y, width, height, advance, bitmap = unifont_fallback(source_glyph, advance)
                fallback_count += 1
        if not (-128 <= left <= 127 and -128 <= y <= 127 and width <= 255 and height <= 255 and advance <= 255):
            raise ValueError(f"Japanese metrics do not fit U+{cp:04X}")
        offset = len(bitmaps)
        bitmaps.extend(bitmap)
        glyphs.append((cp, left, y, width, height, advance, offset, len(bitmap)))
    japanese_count = len(glyphs) - first
    strikes.append((face_id, box, baseline, box-baseline, first, japanese_count))
    lines.append(
        f"{name} {box}px: requested={len(japanese)} glyphs={japanese_count} "
        f"bitmap={len(bitmaps)-bitmap_start} bytes outline={outline_count} "
        f"unifont-fallback={fallback_count} dropped={len(dropped)}"
    )
    if dropped:
        lines.append("Japanese requested but unavailable: " + " ".join(f"U+{cp:04X}" for cp in dropped))
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
    blob = bytes(header + payload)
    digest = hashlib.sha256(blob).hexdigest()
    report = "\n".join([
        "tab5 UI fonts", "format: T5A4 v1, column-major A4", "generator: tools/ui-font/generate.py; Pillow 10.2.0",
        "faces: DejaVu Sans 2.37; DejaVu Sans Mono 2.37; Noto Sans CJK JP 20230817", "Latin code points: 210 per strike", *lines,
        f"metadata={bitmaps_offset} bytes bitmap={len(bitmaps)} bytes total={len(blob)} bytes",
        f"crc32={crc:08x}", f"sha256={digest}", "intermediate coverage: yes (all faces)", ""
    ])
    if arguments.check:
        if not OUT.exists() or OUT.read_bytes() != blob or not REPORT.exists() or REPORT.read_text(encoding="utf-8") != report:
            raise SystemExit(f"{OUT} or {REPORT} is out of date; run make fonts")
        print(f"{OUT}: up to date ({len(blob)} bytes)")
        return
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_bytes(blob)
    REPORT.write_text(report, encoding="utf-8")
    print(report, end="")

if __name__ == "__main__": main()
