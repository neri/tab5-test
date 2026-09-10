PYTHON ?= python3
CURL ?= curl

UNIFONT_JP := tools/font/vendor/unifont_jp-17.0.05.bdf.gz
UNIFONT_JP_URL := https://unifoundry.com/pub/unifont/unifont-17.0.05/font-builds/unifont_jp-17.0.05.bdf.gz
UNIFONT_JP_SHA256 := d3a4c98e41efcf38b49bd520a049230cc040d44433ab9c2cdcd9f1f481443976

DEJAVU_SANS := tools/ui-font/vendor/DejaVuSans.ttf
DEJAVU_SANS_SHA256 := ae7b7855e115a5966d8b1b3f80f254ccc117ec86f9965e202ee2940453837280
DEJAVU_MONO := tools/ui-font/vendor/DejaVuSansMono.ttf
DEJAVU_MONO_SHA256 := c805f9436dbc268644c1d9584f01a601a653e028e08fd74b9b949f6cf8304d88
DEJAVU_CORE_URL := https://archive.ubuntu.com/ubuntu/pool/main/f/fonts-dejavu/fonts-dejavu-core_2.37-8_all.deb
DEJAVU_CORE_SHA256 := 40049660c194f3b8a2541fc7369efebb10e9f94bdac836a2f38fafedd10fa73a
DEJAVU_MONO_URL := https://archive.ubuntu.com/ubuntu/pool/main/f/fonts-dejavu/fonts-dejavu-mono_2.37-8_all.deb
DEJAVU_MONO_PACKAGE_SHA256 := 8a599d6553307db7ecb795d2f0e5a301e03234afc75c7358b0ba43466454c89a

NOTO_CJK := tools/ui-font/vendor/NotoSansCJK-Regular.ttc
NOTO_CJK_URL := https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/Sans/OTC/NotoSansCJK-Regular.ttc
NOTO_CJK_SHA256 := b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a

.DEFAULT_GOAL := fonts

.PHONY: build run update clean fonts font-sources unifont-jp dejavu noto-cjk fonts-check

build: fonts
	cargo build --release

run: build
	cargo run --release

update:
	cargo update

test:
	cargo test -p tab5-browser -p tab5-font-codec -p tab5-font -p tab5-ui-font -p tab5-spki -p tab5-time -p tab5-system-ui --features tab5-font-codec/encoder --target x86_64-unknown-linux-gnu

# Download verified source fonts locally, then regenerate every firmware font
# artifact. Binary source fonts and the large UI blob are ignored by Git.
fonts: font-sources
	$(PYTHON) tools/font/generate.py
	$(PYTHON) tools/ui-font/generate.py

clean:
	-rm "$(UNIFONT_JP)" "$(DEJAVU_SANS)" "$(DEJAVU_MONO)" "$(NOTO_CJK)"
	-cargo clean

font-sources: unifont-jp dejavu noto-cjk

unifont-jp:
	@if [ -f "$(UNIFONT_JP)" ] && printf '%s  %s\n' "$(UNIFONT_JP_SHA256)" "$(UNIFONT_JP)" | sha256sum --check --status; then \
		echo "$(UNIFONT_JP): already downloaded and verified"; \
	else \
		set -eu; \
		tmp="$(UNIFONT_JP).tmp"; \
		trap 'rm -f "$$tmp"' EXIT HUP INT TERM; \
		mkdir -p "$(dir $(UNIFONT_JP))"; \
		$(CURL) --fail --location --retry 3 --output "$$tmp" "$(UNIFONT_JP_URL)"; \
		printf '%s  %s\n' "$(UNIFONT_JP_SHA256)" "$$tmp" | sha256sum --check --status; \
		mv "$$tmp" "$(UNIFONT_JP)"; \
		trap - EXIT HUP INT TERM; \
		echo "$(UNIFONT_JP): downloaded and verified"; \
	fi

# Ubuntu 2.37-8 contains the exact TTFs used for the accepted raster output.
# Verify both packages and extracted files; package metadata alone is not used
# as proof that a particular font file was extracted.
dejavu:
	@if [ -f "$(DEJAVU_SANS)" ] && [ -f "$(DEJAVU_MONO)" ] \
		&& printf '%s  %s\n' "$(DEJAVU_SANS_SHA256)" "$(DEJAVU_SANS)" | sha256sum --check --status \
		&& printf '%s  %s\n' "$(DEJAVU_MONO_SHA256)" "$(DEJAVU_MONO)" | sha256sum --check --status; then \
		echo "DejaVu Sans/Mono: already downloaded and verified"; \
	else \
		set -eu; \
		core_deb="$$(mktemp)"; mono_deb="$$(mktemp)"; \
		sans_tmp="$(DEJAVU_SANS).tmp"; mono_tmp="$(DEJAVU_MONO).tmp"; \
		trap 'rm -f "$$core_deb" "$$mono_deb" "$$sans_tmp" "$$mono_tmp"' EXIT HUP INT TERM; \
		mkdir -p "$(dir $(DEJAVU_SANS))"; \
		$(CURL) --fail --location --retry 3 --output "$$core_deb" "$(DEJAVU_CORE_URL)"; \
		$(CURL) --fail --location --retry 3 --output "$$mono_deb" "$(DEJAVU_MONO_URL)"; \
		printf '%s  %s\n' "$(DEJAVU_CORE_SHA256)" "$$core_deb" | sha256sum --check --status; \
		printf '%s  %s\n' "$(DEJAVU_MONO_PACKAGE_SHA256)" "$$mono_deb" | sha256sum --check --status; \
		dpkg-deb --fsys-tarfile "$$core_deb" | tar -xOf - ./usr/share/fonts/truetype/dejavu/DejaVuSans.ttf > "$$sans_tmp"; \
		dpkg-deb --fsys-tarfile "$$mono_deb" | tar -xOf - ./usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf > "$$mono_tmp"; \
		printf '%s  %s\n' "$(DEJAVU_SANS_SHA256)" "$$sans_tmp" | sha256sum --check --status; \
		printf '%s  %s\n' "$(DEJAVU_MONO_SHA256)" "$$mono_tmp" | sha256sum --check --status; \
		mv "$$sans_tmp" "$(DEJAVU_SANS)"; \
		mv "$$mono_tmp" "$(DEJAVU_MONO)"; \
		trap - EXIT HUP INT TERM; \
		rm -f "$$core_deb" "$$mono_deb"; \
		echo "DejaVu Sans/Mono: downloaded, extracted, and verified"; \
	fi

# Keep the URL on an immutable upstream release tag and verify before moving
# the download into place. A failed download never replaces a valid file.
noto-cjk:
	@if [ -f "$(NOTO_CJK)" ] && printf '%s  %s\n' "$(NOTO_CJK_SHA256)" "$(NOTO_CJK)" | sha256sum --check --status; then \
		echo "$(NOTO_CJK): already downloaded and verified"; \
	else \
		set -eu; \
		tmp="$(NOTO_CJK).tmp"; \
		trap 'rm -f "$$tmp"' EXIT HUP INT TERM; \
		mkdir -p "$(dir $(NOTO_CJK))"; \
		$(CURL) --fail --location --retry 3 --output "$$tmp" "$(NOTO_CJK_URL)"; \
		printf '%s  %s\n' "$(NOTO_CJK_SHA256)" "$$tmp" | sha256sum --check --status; \
		mv "$$tmp" "$(NOTO_CJK)"; \
		trap - EXIT HUP INT TERM; \
		echo "$(NOTO_CJK): downloaded and verified"; \
	fi

# Verify both deterministic generated blobs without rewriting the worktree.
fonts-check: font-sources
	$(PYTHON) tools/font/generate.py --check
	$(PYTHON) tools/ui-font/generate.py --check
