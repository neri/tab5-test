# DejaVu font provenance

- Upstream: https://dejavu-fonts.github.io/
- Version: 2.37
- Files: `DejaVuSans.ttf`, `DejaVuSansMono.ttf`
- Source packages: Ubuntu `fonts-dejavu-core` and `fonts-dejavu-mono` 2.37-8
- Core download: `https://archive.ubuntu.com/ubuntu/pool/main/f/fonts-dejavu/fonts-dejavu-core_2.37-8_all.deb`
- Core package SHA-256: `40049660c194f3b8a2541fc7369efebb10e9f94bdac836a2f38fafedd10fa73a`
- Mono download: `https://archive.ubuntu.com/ubuntu/pool/main/f/fonts-dejavu/fonts-dejavu-mono_2.37-8_all.deb`
- Mono package SHA-256: `8a599d6553307db7ecb795d2f0e5a301e03234afc75c7358b0ba43466454c89a`
- DejaVuSans.ttf SHA-256: `ae7b7855e115a5966d8b1b3f80f254ccc117ec86f9965e202ee2940453837280`
- DejaVuSansMono.ttf SHA-256: `c805f9436dbc268644c1d9584f01a601a653e028e08fd74b9b949f6cf8304d88`

The binary sources are ignored by Git. The root `Makefile` verifies both Ubuntu
package hashes and both extracted TTF hashes, so regeneration is independent of
the host's installed fonts. Redistribution is under `LICENSE.txt`.

## Noto Sans CJK

- Upstream: https://github.com/notofonts/noto-cjk
- Source package: Debian `fonts-noto-cjk` 1:20230817+repack1-3
- Collection face: Noto Sans CJK JP Regular, TTC index 0
- File: `NotoSansCJK-Regular.ttc`
- Download: `https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/Sans/OTC/NotoSansCJK-Regular.ttc`
- SHA-256: `b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a`
- Copyright: 2010-2012 Google Corporation
- License: SIL Open Font License 1.1; the standard license text is also
  present in `../../font/vendor/LICENSE-OFL-1.1.txt`

The 19,484,784 byte TTC exceeds this repository's source-data budget and is
therefore ignored by Git. The root `Makefile` downloads this immutable release
file and verifies the hash before generation. `import_noto.py` remains an
offline alternative for a host with the audited Debian package installed; it
copies only a file with the same hash.
