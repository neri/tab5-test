# UI font generator

`generate.py` converts DejaVu Sans, DejaVu Sans Mono and Noto Sans CJK JP to the
firmware's local `ui-font/data/tab5-ui-fonts.bin`.
Latin has 16/24/32 pixel strikes. Japanese has one 16 pixel A4 strike; the
firmware integer-scales it for 32 pixel text. Pillow/FreeType is a generator
dependency only; an ordinary Cargo build parses no outline font.

Regenerate with `python3 tools/ui-font/generate.py`. The script writes the blob
and `ui-font/data/tab5-ui-fonts.txt`, then validates the complete English Latin
set, the Japanese manifest, monospace advances, intermediate A4 coverage,
offsets and CRC. Japanese glyphs absent from Noto or outside its line box use
the same Unifont source as the ASCII generator, stored as binary A4 values.

The sources are DejaVu Fonts 2.37, Noto Sans CJK 20230817 and GNU Unifont
Japanese 17.0.05. All binary source fonts and the generated 1 MiB A4 blob are
ignored by Git. From the repository root, `make fonts` downloads version-pinned
distributions, verifies the package and source-file SHA-256 values, and
regenerates the ASCII and A4 blobs. Already verified sources are reused. Run
this once after a new checkout, before Cargo build. DejaVu extraction requires
`dpkg-deb` and `tar`. `make fonts-check` regenerates in memory and rejects stale
local artifacts. See `vendor/PROVENANCE.md` and the referenced license files.
