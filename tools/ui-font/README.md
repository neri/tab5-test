# UI font generator

`generate.py` converts the vendored DejaVu Sans and DejaVu Sans Mono sources to
the firmware's checked-in `ui-font/data/tab5-ui-fonts.bin`. Pillow/FreeType is a
generator dependency only; an ordinary Cargo build parses no outline font.

Regenerate with `python3 tools/ui-font/generate.py`. The script writes the blob
and `ui-font/data/tab5-ui-fonts.txt`, then validates the complete English Latin
set, monospace advances, intermediate A4 coverage, offsets and CRC.

The sources are DejaVu Fonts 2.37. See `vendor/LICENSE.txt` and
`vendor/PROVENANCE.md`.
