#!/usr/bin/env python3
"""Import the audited Debian Noto CJK source into the reproducible vendor set."""

from pathlib import Path
import hashlib
import shutil

SOURCE = Path("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc")
DESTINATION = Path(__file__).parent / "vendor" / "NotoSansCJK-Regular.ttc"
EXPECTED_SHA256 = "b76b0433203017ca80401b2ee0dd69350349871c4b19d504c34dbdd80541690a"


def main() -> None:
    data = SOURCE.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != EXPECTED_SHA256:
        raise SystemExit(f"unexpected {SOURCE} SHA-256: {digest}")
    DESTINATION.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(SOURCE, DESTINATION)
    print(f"{DESTINATION}: {len(data)} bytes, SHA-256 {digest}")


if __name__ == "__main__":
    main()
