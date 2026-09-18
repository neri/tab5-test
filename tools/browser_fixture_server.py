#!/usr/bin/env python3
"""LAN HTTP fixture server for the Tab5 hypertext viewer.

Run it on a machine the board can reach and point `browser` at it:

    python3 tools/browser_fixture_server.py            # 0.0.0.0:8080
    python3 tools/browser_fixture_server.py --port 80

    browser http://<this machine>:8080/

What it serves is deliberately not a website. Half the endpoints are
responses no correct server would send -- headers that never end, a
`Content-Length` that disagrees with the body, a chunk size that is not
hexadecimal, a connection that dies in the middle of a paragraph. Those are
the cases the viewer has to fail *specifically* on rather than fail somehow,
and they cannot be provoked from a real server or from `http.server`, which
is why this speaks the protocol by hand over a raw socket.

Every limit the fixtures are built against is mirrored from
`browser/src/limits.rs`. The two have to agree: `/limit/...` endpoints exist
to sit just past a bound, and a bound that moved on one side only turns them
into ordinary pages that pass for the wrong reason. `--check-limits` prints
both tables side by side.

`/manifest.txt` lists every endpoint with the outcome it is supposed to
produce, which is what the on-device `browsertest` walk is driven from.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import html
import re
import socket
import socketserver
import ssl
import struct
import sys
import threading
import time
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

# --------------------------------------------------------------------------
# Limits, mirrored from browser/src/limits.rs.
# --------------------------------------------------------------------------

MAX_HEADER_BYTES = 16384
MAX_URL_BYTES = 2048
MAX_REDIRECTS = 5
MAX_HISTORY = 8
MAX_DECODED_HTML_BYTES = 2 * 1024 * 1024
MAX_TEXT_BYTES = 1024 * 1024
MAX_ITEMS = 16384
MAX_LINKS = 4096
MAX_LINK_URL_BYTES = 2 * 1024 * 1024
MAX_ANCHORS = 1024
MAX_ANCHOR_BYTES = 256 * 1024
MAX_NESTING_DEPTH = 32
MAX_ATTRIBUTES_PER_ELEMENT = 16
MAX_LAYOUT_LINES = 32768
MAX_TABLE_COLUMNS = 32
MAX_TABLE_SPAN = 32
MAX_TABLE_BORDER = 4
MAX_IMAGES = 64
MAX_IMAGE_COMPRESSED_BYTES = 1024 * 1024
MAX_IMAGE_WIDTH = 2048
MAX_IMAGE_HEIGHT = 2048
MAX_IMAGE_PIXELS = 2 * 1024 * 1024
MAX_IMAGE_DECODE_WORK_BYTES = 4 * 1024 * 1024
MAX_DECODED_IMAGE_SOFT_BYTES = 6 * 1024 * 1024
MAX_DECODED_IMAGE_HARD_BYTES = 8 * 1024 * 1024
MAX_FORMS = 32
MAX_FORM_CONTROLS = 256
MAX_INPUT_VALUE_BYTES = 4096
MAX_FORM_VALUE_BYTES = 32 * 1024
MAX_ENCODED_REQUEST_BYTES = 48 * 1024
MAX_SELECT_OPTIONS = 4096
MAX_HTTP_CACHE_ENTRY_BYTES = 1024 * 1024
DEFAULT_CACHE_FRESHNESS_SECS = 3600
MAX_CACHE_META_BYTES = 4096
MAX_RETAINED_POST_RESULTS = 2
MAX_RETAINED_POST_RESULT_BYTES = 320 * 1024
MAX_RETAINED_POST_REQUEST_BYTES = 2 * 48 * 1024
MAX_EXTENSION_OWNED_BYTES = 16 * 1024 * 1024

LIMITS = {
    "MAX_HEADER_BYTES": MAX_HEADER_BYTES,
    "MAX_URL_BYTES": MAX_URL_BYTES,
    "MAX_REDIRECTS": MAX_REDIRECTS,
    "MAX_HISTORY": MAX_HISTORY,
    "MAX_DECODED_HTML_BYTES": MAX_DECODED_HTML_BYTES,
    "MAX_TEXT_BYTES": MAX_TEXT_BYTES,
    "MAX_ITEMS": MAX_ITEMS,
    "MAX_LINKS": MAX_LINKS,
    "MAX_LINK_URL_BYTES": MAX_LINK_URL_BYTES,
    "MAX_ANCHORS": MAX_ANCHORS,
    "MAX_ANCHOR_BYTES": MAX_ANCHOR_BYTES,
    "MAX_NESTING_DEPTH": MAX_NESTING_DEPTH,
    "MAX_ATTRIBUTES_PER_ELEMENT": MAX_ATTRIBUTES_PER_ELEMENT,
    "MAX_LAYOUT_LINES": MAX_LAYOUT_LINES,
    "MAX_TABLE_COLUMNS": MAX_TABLE_COLUMNS,
    "MAX_TABLE_SPAN": MAX_TABLE_SPAN,
    "MAX_TABLE_BORDER": MAX_TABLE_BORDER,
    "MAX_IMAGES": MAX_IMAGES,
    "MAX_IMAGE_COMPRESSED_BYTES": MAX_IMAGE_COMPRESSED_BYTES,
    "MAX_IMAGE_WIDTH": MAX_IMAGE_WIDTH,
    "MAX_IMAGE_HEIGHT": MAX_IMAGE_HEIGHT,
    "MAX_IMAGE_PIXELS": MAX_IMAGE_PIXELS,
    "MAX_IMAGE_DECODE_WORK_BYTES": MAX_IMAGE_DECODE_WORK_BYTES,
    "MAX_DECODED_IMAGE_SOFT_BYTES": MAX_DECODED_IMAGE_SOFT_BYTES,
    "MAX_DECODED_IMAGE_HARD_BYTES": MAX_DECODED_IMAGE_HARD_BYTES,
    "MAX_FORMS": MAX_FORMS,
    "MAX_FORM_CONTROLS": MAX_FORM_CONTROLS,
    "MAX_INPUT_VALUE_BYTES": MAX_INPUT_VALUE_BYTES,
    "MAX_FORM_VALUE_BYTES": MAX_FORM_VALUE_BYTES,
    "MAX_ENCODED_REQUEST_BYTES": MAX_ENCODED_REQUEST_BYTES,
    "MAX_SELECT_OPTIONS": MAX_SELECT_OPTIONS,
    "MAX_HTTP_CACHE_ENTRY_BYTES": MAX_HTTP_CACHE_ENTRY_BYTES,
    "DEFAULT_CACHE_FRESHNESS_SECS": DEFAULT_CACHE_FRESHNESS_SECS,
    "MAX_CACHE_META_BYTES": MAX_CACHE_META_BYTES,
    "MAX_RETAINED_POST_RESULTS": MAX_RETAINED_POST_RESULTS,
    "MAX_RETAINED_POST_RESULT_BYTES": MAX_RETAINED_POST_RESULT_BYTES,
    "MAX_RETAINED_POST_REQUEST_BYTES": MAX_RETAINED_POST_REQUEST_BYTES,
    "MAX_EXTENSION_OWNED_BYTES": MAX_EXTENSION_OWNED_BYTES,
}

LIMITS_RS = Path(__file__).resolve().parent.parent / "browser" / "src" / "limits.rs"


def read_rust_limits() -> dict[str, int]:
    """Evaluates the `pub const` table in `limits.rs`.

    Only the arithmetic those constants actually use is accepted -- integer
    literals, `*` and underscore separators. Anything else is reported
    rather than guessed at, because a limit this cannot read is exactly the
    kind that drifts.
    """
    if not LIMITS_RS.exists():
        raise SystemExit(f"cannot find {LIMITS_RS}")
    pattern = re.compile(r"pub const (\w+): (?:usize|u32) = ([0-9_ *]+);")
    found: dict[str, int] = {}
    for name, expression in pattern.findall(LIMITS_RS.read_text(encoding="utf-8")):
        terms = [int(term.replace("_", "")) for term in expression.split("*")]
        value = 1
        for term in terms:
            value *= term
        found[name] = value
    return found


def check_limits() -> int:
    rust = read_rust_limits()
    names = sorted(set(LIMITS) | set(rust))
    width = max(len(name) for name in names)
    disagreements = 0
    for name in names:
        here = LIMITS.get(name)
        there = rust.get(name)
        mark = "ok" if here == there else "MISMATCH"
        if here != there:
            disagreements += 1
        print(f"{name:<{width}}  server={here!s:<10} limits.rs={there!s:<10} {mark}")
    if disagreements:
        print(f"\n{disagreements} limit(s) disagree; the fixtures no longer sit "
              "where they are supposed to")
    return 1 if disagreements else 0


# --------------------------------------------------------------------------
# HTML fixtures.
# --------------------------------------------------------------------------

def page(title: str, body: str) -> bytes:
    return (
        "<!DOCTYPE html>\n<html><head><title>"
        + title
        + "</title></head>\n<body>\n"
        + body
        + "\n</body></html>\n"
    ).encode("utf-8")


INDEX = page(
    "Tab5 fixture index",
    """<h1>Tab5 browser fixtures</h1>
<p>Every endpoint below is reachable from here with a relative link, which
is itself part of what is being tested.</p>
<h2>Documents that should render</h2>
<ul>
  <li><a href="simple.html">simple.html</a> - the shortest complete page</li>
  <li><a href="long.html">long.html</a> - many paragraphs, for scrolling</li>
  <li><a href="headings.html">headings.html</a> - h1 through h6</li>
  <li><a href="list.html">list.html</a> - nested ul and ol</li>
  <li><a href="pre.html">pre.html</a> - preformatted text</li>
  <li><a href="entity.html">entity.html</a> - character references</li>
  <li><a href="utf8.html">utf8.html</a> - non-ASCII text</li>
  <li><a href="inline.html">inline.html</a> - strong, em, code</li>
  <li><a href="image.html">image.html</a> - img alt handling</li>
  <li><a href="rule.html">rule.html</a> - hr</li>
  <li><a href="script.html">script.html</a> - script and style bodies</li>
  <li><a href="empty.html">empty.html</a> - a document with no text</li>
  <li><a href="links/">links/</a> - relative reference resolution</li>
</ul>
<h2>Character encodings</h2>
<ul>
  <li><a href="encoding/shift-jis">encoding/shift-jis</a> - declared by the header</li>
  <li><a href="encoding/shift-jis-meta">encoding/shift-jis-meta</a> - declared by meta</li>
  <li><a href="encoding/shift-jis-lying-meta">encoding/shift-jis-lying-meta</a> - header over meta</li>
  <li><a href="encoding/shift-jis-broken">encoding/shift-jis-broken</a> - one dangling lead byte</li>
  <li><a href="encoding/euc-jp">encoding/euc-jp</a> - declared by the header</li>
  <li><a href="encoding/euc-jp-meta">encoding/euc-jp-meta</a> - declared by meta</li>
  <li><a href="encoding/euc-jp-broken">encoding/euc-jp-broken</a> - one dangling lead byte</li>
  <li><a href="encoding/utf8-bom.html">encoding/utf8-bom.html</a> - a byte order mark</li>
</ul>
<h2>Media types</h2>
<ul>
  <li><a href="plain.txt">plain.txt</a> - text/plain, shown as itself</li>
  <li><a href="plain-sjis.txt">plain-sjis.txt</a> - text/plain in Shift_JIS</li>
  <li><a href="notatype.bin">notatype.bin</a> - neither HTML nor text, refused</li>
</ul>
<h2>Malformed markup that should still render</h2>
<ul>
  <li><a href="broken/unclosed.html">broken/unclosed.html</a></li>
  <li><a href="broken/deep-nest.html">broken/deep-nest.html</a></li>
  <li><a href="broken/huge-attribute.html">broken/huge-attribute.html</a></li>
  <li><a href="broken/bad-utf8.html">broken/bad-utf8.html</a></li>
</ul>
<h2>Forms</h2>
<ul>
  <li><a href="forms/post.html">forms/post.html</a> - direct POST</li>
  <li><a href="forms/inline-controls.html">forms/inline-controls.html</a> - inline controls and decorated/image buttons</li>
  <li><a href="forms/post-redirects.html">forms/post-redirects.html</a> - POST redirect rules</li>
  <li><a href="cache/index.html">cache/index.html</a> - HTTP cache revalidation</li>
  <li><a href="images/png-formats.html">images/png-formats.html</a> - every PNG colour type and bit depth up to 8 bits, and rejected variants</li>
  <li><a href="images/webp.html">images/webp.html</a> - static lossless/lossy-alpha WebP and rejected animation/damage</li>
  <li><a href="images/webp-lru.html">images/webp-lru.html</a> - cached WebP eviction and re-decode after large no-store images</li>
  <li><a href="images/lru.html">images/lru.html</a> - sixteen distinct large images that cross the decoded-image soft limit</li>
  <li><a href="images/limit-expanded.html">images/limit-expanded.html</a> - image between the old 512 KiB and new 1 MiB compressed limits</li>
  <li><a href="images/limit-dimensions.html">images/limit-dimensions.html</a> - width between the old 1280 and new 2048 pixel limits</li>
  <li><a href="images/limit-pixels.html">images/limit-pixels.html</a> - 1920x1080 inside and 2048x1025 just outside the new pixel limit</li>
  <li><a href="images/limit-work.html">images/limit-work.html</a> - RGBA expansion inside and just outside the 4 MiB decoder-work limit</li>
  <li><a href="images/pinned.html">images/pinned.html</a> - images from a TLS PINNED page (over the TLS listener)</li>
</ul>
<h2>Transfers</h2>
<ul>
  <li><a href="slow">slow</a> - one byte at a time</li>
  <li><a href="chunked">chunked</a> - chunk boundaries move every request</li>
  <li><a href="chunked-trailer">chunked-trailer</a> - extensions and a trailer</li>
  <li><a href="chunked-bad">chunked-bad</a> - a chunk size that is not hex</li>
  <li><a href="gzip">gzip</a> - Content-Encoding this cannot decode</li>
</ul>
<h2>Redirects</h2>
<ul>
  <li><a href="redirect/301">redirect/301</a></li>
  <li><a href="redirect/302">redirect/302</a></li>
  <li><a href="redirect/307">redirect/307</a></li>
  <li><a href="redirect/308">redirect/308</a></li>
  <li><a href="redirect/relative">redirect/relative</a></li>
  <li><a href="redirect/chain/5">redirect/chain/5</a> - inside the limit</li>
  <li><a href="redirect/chain/6">redirect/chain/6</a> - past the limit</li>
  <li><a href="redirect/loop">redirect/loop</a></li>
  <li><a href="redirect/https">redirect/https</a></li>
</ul>
<h2>Failures</h2>
<ul>
  <li><a href="fail/truncate-body">fail/truncate-body</a></li>
  <li><a href="fail/truncate-head">fail/truncate-head</a></li>
  <li><a href="fail/big-header">fail/big-header</a></li>
  <li><a href="fail/length-over">fail/length-over</a> - more body than promised</li>
  <li><a href="fail/length-under">fail/length-under</a> - less body than promised</li>
  <li><a href="fail/no-status">fail/no-status</a></li>
  <li><a href="status/404">status/404</a></li>
  <li><a href="status/500">status/500</a></li>
</ul>
<h2>Limits</h2>
<ul>
  <li><a href="limit/input">limit/input</a> - past MAX_DECODED_HTML_BYTES</li>
  <li><a href="limit/input-nolength">limit/input-nolength</a> - the same without a length</li>
  <li><a href="limit/text">limit/text</a> - past MAX_TEXT_BYTES</li>
  <li><a href="limit/items">limit/items</a> - past MAX_ITEMS</li>
  <li><a href="limit/links">limit/links</a> - excess links become plain text</li>
  <li><a href="limit/url">limit/url</a> - a link past MAX_URL_BYTES</li>
  <li><a href="limit/longline">limit/longline</a> - one unbreakable line</li>
</ul>
<h2>Plain downloads</h2>
<ul>
  <li><a href="download/64k.bin">download/64k.bin</a></li>
  <li><a href="download/512k.bin">download/512k.bin</a> - the httpget regression</li>
</ul>
<p><a href="manifest.txt">manifest.txt</a> lists all of this with the outcome
each endpoint is supposed to produce, including the CRC-32 of the two
downloads above.</p>""",
)

SIMPLE = page(
    "simple",
    """<h1>Simple</h1>
<p>This page has a title, one heading, one paragraph and one link.</p>
<p>The link goes <a href="/links/target.html">to a target page</a>.</p>""",
)

LONG = page(
    "long",
    "\n".join(
        f"<h2>Section {section}</h2>\n"
        + "\n".join(
            "<p>"
            + " ".join(
                f"Section {section} paragraph {paragraph} word {word}."
                for word in range(12)
            )
            + "</p>"
            for paragraph in range(6)
        )
        for section in range(20)
    ),
)

HEADINGS = page(
    "headings",
    "\n".join(
        f"<h{level}>Heading level {level}</h{level}>\n"
        f"<p>Body text under the level {level} heading.</p>"
        for level in range(1, 7)
    ),
)

LIST = page(
    "list",
    """<h1>Lists</h1>
<ul>
  <li>first unordered item</li>
  <li>second unordered item
    <ul>
      <li>nested one</li>
      <li>nested two
        <ol>
          <li>deep ordered one</li>
          <li>deep ordered two</li>
        </ol>
      </li>
    </ul>
  </li>
  <li>third unordered item</li>
</ul>
<ol>
  <li>ordered one</li>
  <li>ordered two</li>
  <li>ordered three</li>
</ol>
<p>Text after the lists, at the outer level again.</p>""",
)

PRE = page(
    "pre",
    """<h1>Preformatted</h1>
<pre>
  column   one    two
  ------   ---    ---
  alpha      1      2
  beta       3      4

  a line with trailing spaces
  a very long preformatted line that has to wrap at the viewport because """
    + "x" * 200
    + """
</pre>
<p>Ordinary text after the pre block collapses    its    whitespace.</p>""",
)

ENTITY = page(
    "entity",
    """<h1>Character references</h1>
<p>Named: &amp; &lt; &gt; &quot; &apos; &nbsp;end</p>
<p>Decimal: &#65; &#8212; &#128169;</p>
<p>Hex: &#x41; &#x2014; &#x1F4A9;</p>
<p>Unterminated: &amp not-an-entity &#; &#x; &notareference;</p>
<p>In an attribute: <a href="/links/target.html?a=1&amp;b=2">a=1&amp;b=2</a></p>""",
)

UTF8 = page(
    "utf8",
    """<h1>Non-ASCII</h1>
<p>Two byte: é ü ñ αβγ</p>
<p>Three byte: 日本語 ひらがな カタカナ</p>
<p>Four byte: \U0001F600 \U0001F5FA \U0001F4A1</p>
<p>Mixed ASCII and not: Tab5 のブラウザ test.</p>""",
)

# --- Shift_JIS ------------------------------------------------------------
#
# What the board has to get right is not the table -- the crate's own tests
# cover that -- but the three ways a page says which encoding it is in, and
# what happens when it lies. The same sentence is used for all of them so
# that a wrong answer shows up as different text rather than as a different
# page.
#
# Written as Unicode here and encoded on the way out, so this file stays
# UTF-8 and the bytes on the wire are still real Shift_JIS.
#
# `cp932` and not `shift_jis`: the name variants and the circled numbers in
# the text below are the NEC and IBM extension rows, which plain JIS X 0208
# does not have and which every page labelled `Shift_JIS` in the wild
# assumes. The board's table is generated from the same codec.
SJIS_CODEC = "cp932"
SHIFT_JIS_TEXT = """<h1>日本語の表示</h1>
<p>このページはShift_JISで送られています。
半角カナ（ｶﾞｷﾞｸﾞﾀﾞ）も半角のまま出るはずです。</p>
<p>人名の異体字：髙 﨑。丸数字：①②③。</p>
<p><a href="/simple.html">simple.html</a></p>"""

SHIFT_JIS_META = (
    '<!DOCTYPE html>\n<html><head><meta charset="Shift_JIS">'
    "<title>日本語</title></head>\n<body>\n"
    + SHIFT_JIS_TEXT
    + "\n</body></html>\n"
).encode(SJIS_CODEC)

SHIFT_JIS_HEADER = (
    "<!DOCTYPE html>\n<html><head><title>日本語</title></head>\n<body>\n"
    + SHIFT_JIS_TEXT
    + "\n</body></html>\n"
).encode(SJIS_CODEC)

# One two-byte character replaced by a lead byte and an ASCII one, so the
# lead has no trail and everything after it is still aligned. What should
# come out is one replacement character, the full stop, and the rest of the
# page intact: the failure mode worth guarding against is the decoder that
# loses synchronisation and turns the remainder into rubble, which a reader
# cannot tell apart from a server that sent rubble.
#
# Corrupting one *byte* would not do. Shift_JIS is dense enough that almost
# any lead and almost any trail make some other perfectly good character,
# so a flipped byte gives a wrong page rather than a damaged one -- which is
# what the first attempt at this fixture quietly produced.
_BROKEN_AT = SHIFT_JIS_HEADER.index("表".encode(SJIS_CODEC))
SHIFT_JIS_BROKEN = (
    SHIFT_JIS_HEADER[:_BROKEN_AT] + b"\x93." + SHIFT_JIS_HEADER[_BROKEN_AT + 2 :]
)

# --- EUC-JP ---------------------------------------------------------------

EUC_JP_CODEC = "euc_jp"
EUC_JP_TEXT = """<h1>日本語の表示</h1>
<p>このページはEUC-JPで送られています。
半角カナ（ｶﾞｷﾞｸﾞﾀﾞ）も半角のまま出るはずです。</p>
<p>ひらがな：あいうえお。漢字：日本語。</p>
<p><a href="/simple.html">simple.html</a></p>"""

EUC_JP_META = (
    '<!DOCTYPE html>\n<html><head><meta charset="EUC-JP">'
    "<title>日本語</title></head>\n<body>\n"
    + EUC_JP_TEXT
    + "\n</body></html>\n"
).encode(EUC_JP_CODEC)

EUC_JP_HEADER = (
    "<!DOCTYPE html>\n<html><head><title>日本語</title></head>\n<body>\n"
    + EUC_JP_TEXT
    + "\n</body></html>\n"
).encode(EUC_JP_CODEC)

_EUC_BROKEN_AT = EUC_JP_HEADER.index("表".encode(EUC_JP_CODEC))
EUC_JP_BROKEN = (
    EUC_JP_HEADER[:_EUC_BROKEN_AT]
    + EUC_JP_HEADER[_EUC_BROKEN_AT : _EUC_BROKEN_AT + 1]
    + b"."
    + EUC_JP_HEADER[_EUC_BROKEN_AT + 2 :]
)

# Text that is not markup. What has to survive is its own spacing and line
# breaks, and the fact that the angle brackets in it are characters rather
# than the start of anything.
PLAIN = (
    "plain text, not markup\n"
    "======================\n"
    "\n"
    "  indented, and the   run of spaces before this stays\n"
    "a < b && c > d -- none of these open a tag or a reference\n"
    "&amp; is four characters here, not one\n"
    "\n"
    "\u65e5\u672c\u8a9e\u3082\u305d\u306e\u307e\u307e\u51fa\u307e\u3059\u3002\n"
).encode("utf-8")

UTF8_BOM = b"\xef\xbb\xbf" + page(
    "bom",
    "<h1>Byte order mark</h1>\n<p>The three bytes before the doctype are not text.</p>",
)


INLINE = page(
    "inline",
    """<h1>Inline runs</h1>
<p>Plain, <strong>strong</strong>, <b>bold</b>, <em>emphasis</em>,
<i>italic</i> and <code>code()</code> in one paragraph.</p>
<p><strong>A run that <em>nests</em> another</strong> and then stops.</p>
<p><code>&lt;tag attribute="value"&gt;</code> inside code.</p>""",
)

IMAGE = page(
    "image",
    """<h1>Images</h1>
<p>With alt: <img src="missing.png" alt="a red square"> after.</p>
<p>Without alt: <img src="missing.png"> after.</p>
<p>Empty alt: <img src="missing.png" alt=""> after.</p>
<p>Self closed: <img src="missing.png" alt="self closed"/> after.</p>""",
)

RULE = page(
    "rule",
    """<h1>Rules</h1>
<p>Above the rule.</p>
<hr>
<p>Between the rules.</p>
<hr/>
<p>Below the rule.</p>""",
)

SCRIPT = page(
    "script",
    """<h1>Raw text elements</h1>
<style>
  /* < and > and </ inside a style body */
  body > p { content: "</style is not the end"; }
</style>
<p>Between the style and the script.</p>
<script>
  // < and > and </ inside a script body
  if (a < b && c > d) { document.write("</scriptnot"); }
  var s = "</script is only ended by the real tag";
</script>
<p>After the script. Neither block above may appear as text.</p>""",
)

EMPTY = page("empty", "<!-- nothing but a comment -->")

LINKS_INDEX = page(
    "links",
    """<h1>Relative references</h1>
<ul>
  <li><a href="target.html">target.html</a> - same directory</li>
  <li><a href="./target.html">./target.html</a></li>
  <li><a href="deep/target.html">deep/target.html</a> - one down</li>
  <li><a href="deep/../target.html">deep/../target.html</a> - down and back</li>
  <li><a href="../simple.html">../simple.html</a> - one up</li>
  <li><a href="/simple.html">/simple.html</a> - root relative</li>
  <li><a href="?query=only">?query=only</a> - query only</li>
  <li><a href="#fragment">#fragment</a> - fragment only</li>
  <li><a href="target.html?a=1&amp;b=2#part">target.html?a=1&amp;b=2#part</a></li>
  <li><a href="http://example.invalid/absolute">absolute http</a></li>
  <li><a href="https://example.invalid/secure">absolute https</a></li>
  <li><a href="ftp://example.invalid/file">unsupported scheme</a></li>
  <li><a href="">empty href</a></li>
</ul>""",
)

LINKS_TARGET = page(
    "links target",
    """<h1>Target</h1>
<p>You arrived at the target page.</p>
<p><a href="../">back to the fixture index</a></p>""",
)

LINKS_DEEP = page(
    "links deep target",
    """<h1>Deep target</h1>
<p>One directory below the link index.</p>
<p><a href="../">back up one</a> and <a href="/">back to the root</a></p>""",
)

UNCLOSED = (
    b"<!DOCTYPE html>\n<html><head><title>unclosed</title>\n"
    b"<body>\n<h1>Unterminated markup\n"
    b"<p>A paragraph that is never closed.\n"
    b"<ul><li>one<li>two<li>three\n"
    b"<p>Another paragraph, still open, with an <a href=\"/simple.html\">"
    b"unclosed link\n"
    b"<div><div><span>and some unclosed containers\n"
)

DEEP_NEST = page(
    "deep nest",
    "<ul><li>" * (MAX_NESTING_DEPTH * 4)
    + "the innermost item, well past MAX_NESTING_DEPTH"
    + "</li></ul>" * (MAX_NESTING_DEPTH * 4),
)

HUGE_ATTRIBUTE = page(
    "huge attribute",
    "<p "
    + " ".join(
        f'data-{index}="{"v" * 64}"'
        for index in range(MAX_ATTRIBUTES_PER_ELEMENT * 4)
    )
    + f' title="{"t" * 8192}">Text under an element carrying more attributes '
    "than are examined, one of them enormous.</p>\n"
    '<p>Second paragraph, to show the parser recovered.</p>',
)

# Valid UTF-8 around a lone continuation byte and a truncated sequence. The
# viewer has to keep the text on both sides rather than give up on the
# document.
BAD_UTF8 = (
    b"<!DOCTYPE html>\n<html><head><title>bad utf8</title></head>\n<body>\n"
    b"<h1>Invalid encoding</h1>\n"
    b"<p>before \xff\xfe after</p>\n"
    b"<p>lone continuation \x80 done</p>\n"
    b"<p>truncated three byte \xe6\x97 done</p>\n"
    b"<p>overlong \xc0\xaf done</p>\n"
    b"<p>valid again: \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e</p>\n"
    b"</body></html>\n"
)

SLOW = page(
    "slow",
    """<h1>Slow</h1>
<p>Every byte of this response is written separately, so the viewer sees the
body arrive over several seconds and has to stay responsive throughout.</p>""",
)

CHUNKED = page(
    "chunked",
    """<h1>Chunked</h1>
<p>This response is chunked, and the chunk boundaries move on every request
so that a tag, an entity or a multi-byte character straddles a boundary
sooner or later.</p>
<p>Second paragraph, entity &amp; here, and 日本語 here.</p>""",
)

GZIP = page("gzip", "<p>This body claims to be gzip but is not.</p>")


def repeated_items(count: int) -> bytes:
    return page(
        "items",
        "\n".join(f"<p>Item number {index}.</p>" for index in range(count)),
    )


def repeated_links(count: int) -> bytes:
    return page(
        "links",
        "\n".join(
            f'<p><a href="/links/target.html?n={index}">link {index}</a></p>'
            for index in range(count)
        ),
    )


LONG_URL = "/links/target.html?padding=" + "p" * (MAX_URL_BYTES + 64)
OVER_LONG_URL_PAGE = page(
    "url limit",
    f'<p>The link below is longer than MAX_URL_BYTES ({MAX_URL_BYTES}).</p>\n'
    f'<p><a href="{LONG_URL}">an over-long target</a></p>\n'
    '<p><a href="/simple.html">a normal target, after it</a></p>',
)

LONG_LINE = page(
    "long line",
    "<p>" + "w" * MAX_URL_BYTES + "</p>\n<p>A normal paragraph after it.</p>",
)


def wrap_body(title: bytes, filler: bytes, repeats: int) -> bytes:
    return (
        b"<!DOCTYPE html>\n<html><head><title>"
        + title
        + b"</title></head><body>\n"
        + filler * repeats
        + b"</body></html>\n"
    )


def oversize_body() -> bytes:
    """A document a little past `MAX_DECODED_HTML_BYTES`, and nothing else.

    Markup with no text in it, which took a device run to get right. The
    first version was repeated paragraphs of 512 characters, and its
    docstring claimed it would reach the input bound "not the item or text
    bound on the way there" -- which was simply false: a body that is mostly
    text reaches the one-megabyte *text* bound at about half the size, and
    `/limit/input-nolength` failed with `text-limit` on the board.

    Empty `div`s contribute no text and no blocks (an empty block is
    dropped), so the only bound left for this to reach is the one it is
    named after. The text bound has its own fixture below.
    """
    filler = b'<div class="a b c" data-x="yyyyyyyyyyyyyyyyyyyy"></div>\n'
    repeats = (MAX_DECODED_HTML_BYTES // len(filler)) + 256
    return wrap_body(b"oversize", filler, repeats)


def text_heavy_body() -> bytes:
    """A document past `MAX_TEXT_BYTES` but inside `MAX_DECODED_HTML_BYTES`.

    The other side of the pair: this one has to be refused for its text and
    not for its size, so it stays comfortably under the input bound.
    """
    paragraph = b"<p>" + b"o" * 512 + b"</p>\n"
    repeats = (MAX_TEXT_BYTES // 512) + 64
    body = wrap_body(b"text heavy", paragraph, repeats)
    assert len(body) < MAX_DECODED_HTML_BYTES, "would trip the input bound first"
    return body


# --------------------------------------------------------------------------
# Response construction.
# --------------------------------------------------------------------------

STATUS_TEXT = {
    200: "OK",
    301: "Moved Permanently",
    302: "Found",
    303: "See Other",
    307: "Temporary Redirect",
    308: "Permanent Redirect",
    404: "Not Found",
    500: "Internal Server Error",
}


def head(
    status: int,
    headers: list[tuple[str, str]],
) -> bytes:
    reason = STATUS_TEXT.get(status, "Unknown")
    lines = [f"HTTP/1.1 {status} {reason}"]
    lines += [f"{name}: {value}" for name, value in headers]
    return ("\r\n".join(lines) + "\r\n\r\n").encode("ascii")


def html_response(
    body: bytes, status: int = 200, extra: list[tuple[str, str]] | None = None
) -> bytes:
    return (
        head(
            status,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Content-Length", str(len(body))),
                *(extra or []),
                ("Connection", "close"),
            ],
        )
        + body
    )


def redirect(location: str, status: int) -> bytes:
    body = page(
        "redirect",
        f'<p>Redirecting to <a href="{location}">{location}</a>.</p>',
    )
    return (
        head(
            status,
            [
                ("Location", location),
                ("Content-Type", "text/html; charset=utf-8"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
        + body
    )


@dataclass
class Request:
    method: str
    target: str
    path: str
    query: str
    headers: dict[str, str]
    body: bytes


Handler = Callable[["FixtureHandler", Request], None]

ROUTES: dict[str, Handler] = {}
MANIFEST: list[tuple[str, str]] = []


def route(path: str, expectation: str):
    """Registers one endpoint and what it is supposed to produce.

    The expectation string is not decoration: `/manifest.txt` is what the
    on-device `browsertest` walk reads, so "ok" versus "error:body-limit" is
    the pass criterion for that endpoint rather than a comment about it.

    Four forms, and the walk acts on the first two only:

    - ``ok`` -- fetches and becomes a page
    - ``error:NAME`` -- fails with exactly that one-word reason, which is
      `app::fetch::Failure::name` on the device
    - ``download:crc32=...`` -- not HTML; for the `httpget` regression, and
      skipped by the walk
    - ``text`` -- the manifest itself, likewise skipped
    """

    def register(function: Handler) -> Handler:
        ROUTES[path] = function
        MANIFEST.append((path, expectation))
        return function

    return register


# --- documents ------------------------------------------------------------

def static(path: str, expectation: str, body: bytes) -> None:
    @route(path, expectation)
    def handler(self: "FixtureHandler", request: Request, body=body) -> None:
        self.send_all(html_response(body))


static("/", "ok", INDEX)
static("/index.html", "ok", INDEX)
static("/simple.html", "ok", SIMPLE)
static("/long.html", "ok", LONG)
static("/headings.html", "ok", HEADINGS)
static("/list.html", "ok", LIST)
static("/pre.html", "ok", PRE)
static("/entity.html", "ok", ENTITY)
static("/utf8.html", "ok", UTF8)
static("/inline.html", "ok", INLINE)

POST_FORM = page(
    "POST form",
    "<h1>POST form acceptance</h1>"
    "<p>The response must show one POST and the exact encoded body.</p>"
    "<form action='/forms/post/echo?kept=1' method='post'>"
    "<label for='post-q'>Query</label>"
    "<input id='post-q' name='q' value='two words'>"
    "<input type='hidden' name='q' value='hidden duplicate'>"
    "<input type='hidden' name='empty' value=''>"
    "<input name='off' value='disabled value' disabled>"
    "<button name='go' value='Send'>Send POST</button>"
    "</form>"
    "<h2>Checkboxes and radio buttons</h2>"
    "<form action='/forms/post/echo?kept=checkable' method='post'>"
    "<label for='post-cb-a'>Alpha (checked)</label>"
    "<input id='post-cb-a' type='checkbox' name='pick' value='alpha' checked>"
    "<label for='post-cb-b'>Beta</label>"
    "<input id='post-cb-b' type='checkbox' name='pick' value='beta'>"
    "<label for='post-cb-on'>No value attribute</label>"
    "<input id='post-cb-on' type='checkbox' name='flag'>"
    "<label for='post-r-1'>One</label>"
    "<input id='post-r-1' type='radio' name='choice' value='one' checked>"
    "<label for='post-r-2'>Two</label>"
    "<input id='post-r-2' type='radio' name='choice' value='two'>"
    "<button name='go' value='Checks'>Send checkable POST</button>"
    "</form>"
    "<h2>Textarea</h2>"
    "<form action='/forms/post/echo?kept=textarea' method='post'>"
    "<label for='post-text'>Text</label>"
    "<textarea id='post-text' name='text' rows='3'>line one\nline two</textarea>"
    "<button name='go' value='Text'>Send textarea POST</button>"
    "</form>"
    "<h2>Select</h2>"
    "<form action='/forms/post/echo?kept=select' method='post'>"
    "<label for='post-fruit'>Fruit</label>"
    "<select id='post-fruit' name='fruit'><option value='apple'>Apple</option>"
    "<option value='pear' selected>Pear</option><option value='plum'>Plum</option></select>"
    "<label for='post-many'>Many</label>"
    "<select id='post-many' name='many' multiple><option value='x' selected>X</option>"
    "<option value='y' selected>Y</option><option value='z'>Z</option></select>"
    "<button name='go' value='Select'>Send select POST</button>"
    "</form>"
    "<h2>Not kept for history</h2>"
    "<p>The result says Cache-Control: no-store. Back to it must ask before resending.</p>"
    "<form action='/forms/post/echo-no-store' method='post'>"
    "<input name='q' value='no store'>"
    "<button name='go' value='NoStore'>Send no-store POST</button>"
    "</form>"
    "<h2>Too large to keep</h2>"
    "<p>The result is larger than the retained POST result budget. Back to it must ask.</p>"
    "<form action='/forms/post/echo-large' method='post'>"
    "<input name='q' value='large'>"
    "<button name='go' value='Large'>Send large POST</button>"
    "</form>"
    "<p><a href='/'>index</a></p>",
)
static("/forms/post.html", "ok", POST_FORM)

POST_COUNTS: dict[str, int] = {}


# Paragraphs of filler whose document text alone exceeds the retained POST
# result budget, so the viewer must fall back to asking before resending.
LARGE_POST_FILLER = "".join(
    f"<p>filler {index:05d}: " + "x" * 96 + "</p>"
    for index in range(MAX_RETAINED_POST_RESULT_BYTES // 96 + 64)
)


def post_echo(
    self: "FixtureHandler",
    request: Request,
    no_store: bool = False,
    large: bool = False,
) -> None:
    if request.method == "POST":
        POST_COUNTS[request.path] = POST_COUNTS.get(request.path, 0) + 1
    count = POST_COUNTS.get(request.path, 0)
    body_text = request.body.decode("ascii", "backslashreplace")
    response = page(
        "POST result",
        "<h1>POST result</h1>"
        f"<p id='method'>method: {html.escape(request.method)}</p>"
        f"<p id='count'>POST count: {count}</p>"
        f"<p id='query'>target query: {html.escape(request.query)}</p>"
        f"<p id='type'>content-type: {html.escape(request.headers.get('content-type', 'missing'))}</p>"
        f"<p id='length'>content-length: {html.escape(request.headers.get('content-length', 'missing'))}</p>"
        f"<p id='body'>body: {html.escape(body_text)}</p>"
        f"<p id='kept'>cache-control no-store: {'yes' if no_store else 'no'}</p>"
        "<p><a href='/forms/post.html'>form again</a></p>"
        + (LARGE_POST_FILLER + "<p>end of large result</p>" if large else ""),
    )
    extra = [("Cache-Control", "no-store")] if no_store else None
    self.send_all(html_response(response, extra=extra))


def post_echo_no_store(self: "FixtureHandler", request: Request) -> None:
    post_echo(self, request, no_store=True)


def post_echo_large(self: "FixtureHandler", request: Request) -> None:
    post_echo(self, request, large=True)


# A POST target is intentionally absent from the GET walk manifest: walking
# it would change the counter whose whole purpose is detecting a duplicate
# submission.
ROUTES["/forms/post/echo"] = post_echo
ROUTES["/forms/post/echo-no-store"] = post_echo_no_store
ROUTES["/forms/post/echo-large"] = post_echo_large

POST_REDIRECT_FORM = page(
    "POST redirect forms",
    "<h1>POST redirect acceptance</h1>"
    "<p>Each result must show one source POST and one final request.</p>"
    + "".join(
        f"<h2>{code}</h2>"
        f"<form action='/forms/post/redirect/{code}?source={code}' method='post'>"
        f"<input name='q' value='redirect {code}'>"
        f"<button name='go' value='{code}'>Try {code}</button>"
        "</form>"
        for code in (301, 302, 303, 307, 308)
    )
    + "<h2>Cross-origin 307/308</h2>"
    "<p>From plaintext these redirect to the TLS listener, another origin. The viewer "
    "must ask before resending the body; cancel must leave both counts unchanged.</p>"
    + "".join(
        f"<form action='/forms/post/redirect/cross-{code}?source=cross-{code}' method='post'>"
        f"<input name='q' value='cross {code}'>"
        f"<button name='go' value='cross-{code}'>Try cross-origin {code}</button>"
        "</form>"
        for code in (307, 308)
    )
    + "<p><a href='/'>index</a></p>",
)
static("/forms/post-redirects.html", "ok", POST_REDIRECT_FORM)

POST_REDIRECT_SOURCE_COUNTS: dict[str, int] = {}
POST_REDIRECT_FINAL_COUNTS: dict[str, int] = {}


def post_redirect_source(
    self: "FixtureHandler", request: Request, code: str
) -> None:
    if request.method == "POST":
        POST_REDIRECT_SOURCE_COUNTS[code] = (
            POST_REDIRECT_SOURCE_COUNTS.get(code, 0) + 1
        )
    location = f"/forms/post/result/{code}?from={code}"
    if code.startswith("cross-"):
        if "tls" not in PORTS:
            self.send_all(html_response(page(
                "no TLS listener",
                "<h1>No TLS listener</h1><p>Start the server with a TLS port.</p>",
            ), 503))
            return
        location = self.other_scheme_origin(request) + location
    self.send_all(redirect(location, int(code.removeprefix("cross-"))))


def post_redirect_result(
    self: "FixtureHandler", request: Request, code: str
) -> None:
    POST_REDIRECT_FINAL_COUNTS[code] = (
        POST_REDIRECT_FINAL_COUNTS.get(code, 0) + 1
    )
    body_text = request.body.decode("ascii", "backslashreplace")
    response = page(
        f"POST redirect {code}",
        f"<h1>POST redirect {code}</h1>"
        f"<p id='method'>final method: {html.escape(request.method)}</p>"
        f"<p id='source-count'>source POST count: {POST_REDIRECT_SOURCE_COUNTS.get(code, 0)}</p>"
        f"<p id='final-count'>final request count: {POST_REDIRECT_FINAL_COUNTS[code]}</p>"
        f"<p id='query'>final query: {html.escape(request.query)}</p>"
        f"<p id='type'>content-type: {html.escape(request.headers.get('content-type', 'missing'))}</p>"
        f"<p id='length'>content-length: {html.escape(request.headers.get('content-length', 'missing'))}</p>"
        f"<p id='body'>body: {html.escape(body_text)}</p>"
        "<p><a href='/forms/post-redirects.html'>redirect forms</a></p>",
    )
    self.send_all(html_response(response))


def post_redirect_source_handler(code: str) -> Handler:
    def handler(self: "FixtureHandler", request: Request) -> None:
        post_redirect_source(self, request, code)

    return handler


def post_redirect_result_handler(code: str) -> Handler:
    def handler(self: "FixtureHandler", request: Request) -> None:
        post_redirect_result(self, request, code)

    return handler


# Source and result endpoints stay out of the GET manifest walk so it cannot
# change the counters used to detect an accidental duplicate submission.
for redirect_status in ("301", "302", "303", "307", "308", "cross-307", "cross-308"):
    ROUTES[f"/forms/post/redirect/{redirect_status}"] = (
        post_redirect_source_handler(redirect_status)
    )
    ROUTES[f"/forms/post/result/{redirect_status}"] = (
        post_redirect_result_handler(redirect_status)
    )


def png_chunk(kind: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + kind
        + data
        + struct.pack(">I", binascii.crc32(kind + data) & 0xFFFFFFFF)
    )


def checker_png() -> bytes:
    width, height = 192, 128
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
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
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(rows))
        + png_chunk(b"IEND", b"")
    )


def large_png() -> bytes:
    width, height = 640, 400
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            r = 20 + x * 200 // width
            g = 30 + y * 190 // height
            b = 220 - x * 120 // width
            if (x // 40 + y // 40) % 2:
                r = min(255, r + 25)
                b = max(0, b - 20)
            rows.extend((r, g, b, 255))
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(rows))
        + png_chunk(b"IEND", b"")
    )


IMAGE_PAGE = page(
    "network images",
    "<h1>Network PNG and JPEG</h1>"
    + "<p>before image</p>" * 12
    + "<img src='/images/checker.png' alt='network PNG failed' width='384' height='256'>"
    + "<img src='/images/checker.png' alt='duplicate PNG failed' width='192' height='128'>"
    + "<img src='/images/landscape.jpg' alt='JPEG failed' width='384' height='288'>"
    + "<img src='/images/bad-crc.png' alt='alt must not hide failure' width='320' height='96'>"
    + "<img src='/images/slow.png' alt='slow intrinsic image'>"
    + "<p>KEEP THIS LINE AT THE SAME SCREEN POSITION</p>"
    + "<img src='/images/large.png' alt='large image failed' width='640' height='400'>"
    + "<p>after image</p>" * 18,
)
static("/images/stage3.html", "ok", IMAGE_PAGE)

LRU_IMAGE_COUNT = 16
LRU_PAGE = page(
    "decoded image LRU",
    "<h1>Decoded image LRU</h1>"
    "<p>Each image is a distinct no-store URL with a 640x400 RGBA source "
    "(512,000 decoded RGB565 bytes). Scroll to the end and back. The viewer "
    "must stay responsive, preserve every box, and report evictions.</p>"
    + "".join(
        f"<h2>Image {index + 1} of {LRU_IMAGE_COUNT}</h2>"
        f"<img src='/images/lru.png?id={index}' width='384' height='240' "
        f"alt='LRU image {index + 1} failed'>"
        for index in range(LRU_IMAGE_COUNT)
    )
    + "<p>End of LRU fixture. Press i, then return to the top and press i again.</p>",
)
static("/images/lru.html", "ok", LRU_PAGE)

EXPANDED_LIMIT_PAGE = page(
    "expanded image input limit",
    "<h1>Expanded image input limit</h1>"
    "<p>The first valid RGB PNG is larger than the old 512 KiB compressed limit "
    "and no larger than the new 1 MiB limit. It must display and be cacheable.</p>"
    "<img src='/images/expanded-limit.png' width='384' height='240' alt='new limit failed'>"
    "<p>The second response is exactly one byte over the new limit. Only its image "
    "box must fail with image too large.</p>"
    "<img src='/images/over-expanded-limit.png' width='384' height='120' alt='over limit'>"
    "<p>The page and first image must remain after the local failure.</p>",
)
static("/images/limit-expanded.html", "ok", EXPANDED_LIMIT_PAGE)

DIMENSION_LIMIT_PAGE = page(
    "expanded image dimensions",
    "<h1>Expanded image dimensions</h1>"
    "<p>The first 1600x400 one-bit PNG exceeds the old 1280-pixel side limit "
    "while staying below the unchanged pixel and decoder-work limits.</p>"
    "<img src='/images/wide-limit.png' width='768' height='192' alt='wide image failed'>"
    "<p>The second image is 2049x1, one pixel over the new side limit. Only its "
    "box must fail with image too large.</p>"
    "<img src='/images/over-wide-limit.png' width='384' height='64' alt='over width'>"
    "<p>The page and first image must remain after the local failure.</p>",
)
static("/images/limit-dimensions.html", "ok", DIMENSION_LIMIT_PAGE)

PIXEL_LIMIT_PAGE = page(
    "expanded image pixel count",
    "<h1>Expanded image pixel count</h1>"
    "<p>The first 1920x1080 one-bit PNG is inside the 2,097,152-pixel limit. "
    "Its decoded RGB565 allocation is 4,147,200 bytes.</p>"
    "<img src='/images/full-hd-limit.png' width='768' height='432' alt='full HD failed'>"
    "<p>The second image is 2048x1025: both sides are legal, but its 2,099,200 "
    "pixels exceed the total by 2,048. Only its box must fail with image too large.</p>"
    "<img src='/images/over-pixel-limit.png' width='384' height='192' alt='over pixels'>"
    "<p>The page and first image must remain after the local failure.</p>",
)
static("/images/limit-pixels.html", "ok", PIXEL_LIMIT_PAGE)

WORK_LIMIT_PAGE = page(
    "expanded decoder work limit",
    "<h1>Expanded decoder work limit</h1>"
    "<p>The first 1024x768 RGBA PNG expands to about 3 MiB before RGB565 "
    "conversion and must display.</p>"
    "<img src='/images/rgba-work-limit.png' width='512' height='384' alt='RGBA work failed'>"
    "<p>The second 1024x1024 RGBA PNG needs 4,195,328 bytes including one PNG "
    "filter byte per row: 1,024 bytes over the 4 MiB work limit. Only its box "
    "must fail with image too large.</p>"
    "<img src='/images/over-work-limit.png' width='384' height='384' alt='over work'>"
    "<p>The page and first image must remain after the local failure.</p>",
)
static("/images/limit-work.html", "ok", WORK_LIMIT_PAGE)


def one_bit_grey_png(width: int, height: int) -> bytes:
    row_bytes = (width + 7) // 8
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        rows.extend((0xAA if (byte + y // 16) % 2 == 0 else 0x55) for byte in range(row_bytes))
    ihdr = struct.pack(">IIBBBBB", width, height, 1, 0, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(rows, 6))
        + png_chunk(b"IEND", b"")
    )


def rgba_work_png(width: int, height: int) -> bytes:
    row = bytearray()
    for x in range(width):
        row.extend((x & 0xFF, (x * 3) & 0xFF, 160, (x * 5) & 0xFF))
    rows = (b"\x00" + bytes(row)) * height
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(rows, 6))
        + png_chunk(b"IEND", b"")
    )

_EXPANDED_LIMIT_PNG: bytes | None = None


def expanded_limit_png() -> bytes:
    """A deterministic 640x400 RGB PNG whose compressed body is 512 KiB..1 MiB."""
    global _EXPANDED_LIMIT_PNG
    if _EXPANDED_LIMIT_PNG is not None:
        return _EXPANDED_LIMIT_PNG
    width, height = 640, 400
    state = 0x5A17_2026
    rows = bytearray()
    for _y in range(height):
        rows.append(0)
        for _x in range(width * 3):
            state ^= state << 13
            state ^= state >> 17
            state ^= state << 5
            state &= 0xFFFF_FFFF
            rows.append(state & 0xFF)
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    body = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(rows, 6))
        + png_chunk(b"IEND", b"")
    )
    assert 512 * 1024 < len(body) <= MAX_IMAGE_COMPRESSED_BYTES
    _EXPANDED_LIMIT_PNG = body
    return body

INLINE_CONTROL_PAGE = page(
    "inline controls and buttons",
    "<h1>Inline controls and buttons</h1>"
    "<form action='/forms/post/echo' method='post'>"
    "<p><label for=q>Query</label> <input id=q name=q value=initial> after "
    "<select name=choice><option selected>one</option><option>two</option></select> tail "
    "<button name=plain value=yes>Plain</button>.</p>"
    "<p><button name=styled value=yes><strong>Bold</strong> <em>italic</em> "
    "<code>code</code> <div>block flattened</div> <a href=/bad>link text</a>"
    "<input name=nested> end</button></p>"
    "<p><button name=image value=yes>before <img src='/images/checker.png' width=80 height=40 alt=checker> after</button> "
    "<button name=slow value=yes><img src='/images/slow.png' alt=slow> slow</button> "
    "<button name=broken value=yes><img src='/images/bad-crc.png' alt=broken> broken</button></p>"
    "<table border=1><tr><th>wide inline cell</th><th>peer</th></tr>"
    "<tr><td><label for=cell>Cell</label> <input id=cell name=cell value=value> after "
    "<button name=cell-go value=yes><strong>Go</strong><img src='/images/checker.png' width=40 height=40></button></td>"
    "<td rowspan=3>rowspan</td></tr><tr><td>narrow <input name=narrow> tail</td></tr>"
    "<tr><td>before <input type=checkbox name=before-image> inline"
    "<img src='/images/checker.png' width=80 height=40 alt=standalone>"
    "after <input type=radio name=after-image> inline</td></tr></table>"
    "</form><p><a href='/'>index</a></p>",
)
static("/forms/inline-controls.html", "ok", INLINE_CONTROL_PAGE)

BASELINE_JPEG = base64.b64decode(
    "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAYEBQYFBAYGBQYHBwYIChAKCgkJChQODwwQFxQYGBcUFhYaHSUfGhsjHBYWICwgIyYnKSopGR8tMC0oMCUoKSj/2wBDAQcHBwoIChMKChMoGhYaKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCj/wAARCAAMABADASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDyvRfCP3f3f6V32ieEPu/u/wBK7XRNMtvl+Su+0XTLX5fkpYbEs5+GeJa2h//Z"
)

# Generated with Pillow 10.2.0 from a 16x16 RGBA gradient. Keeping the encoded
# bytes here makes the fixture server independent of Pillow and libwebp.
WEBP_LOSSLESS = base64.b64decode(
    "UklGRjAAAABXRUJQVlA4TCQAAAAvD8ADEJmM6H9sIgre/wCRtk0tzL/gw9OJGMBFmABwHbuu9R4="
)
WEBP_LOSSY_ALPHA = base64.b64decode(
    "UklGRoYAAABXRUJQVlA4WAoAAAAQAAAADwAADwAAQUxQSBAAAAAFDzAIERHi/v8R/Q////9/"
    "VlA4IFAAAABwAgCdASoQABAAAUAmJbACdHMBMAH6AAXeScYAAP79pt/9JYkuNU1fVraR/qJ3g"
    "iHrmN7KAv/4jw1/u6VVwBEXsA//86CwD//z8a/ZK7AAAA=="
)
# Two opaque 16x16 frames, red then blue. Animation must be rejected rather
# than silently showing its first frame.
WEBP_ANIMATED = base64.b64decode(
    "UklGRsQAAABXRUJQVlA4WAoAAAACAAAADwAADwAAQU5JTQYAAAAAAAAAAABBTk1GSgAAAAAAAAAA"
    "AA8AAA8AAGQAAAJWUDggMgAAADABAJ0BKhAAEAABQCYloAADcAD+8ut///mwP/bz/wR6Af//0u"
    "D//pcH//S4P/SkAAAAQU5NRkYAAAAAAAAAAAAPAAAPAABkAAAAVlA4IC4AAAA0AQCdASoQABAAAA"
    "AmJaAAA3AA/vtV4///S4P/+lwf/9Lg/9Lg//rV5Vesq6AA"
)

WEBP_PAGE = page(
    "static WebP",
    "<h1>Static WebP</h1>"
    "<p>Lossless RGBA gradient: expected image.</p>"
    "<img src='/images/webp-lossless.webp' width='256' height='256' alt='lossless failed'>"
    "<p>Lossy VP8 plus ALPH: expected image with white alpha compositing.</p>"
    "<img src='/images/webp-lossy-alpha.webp' width='256' height='256' alt='lossy alpha failed'>"
    "<p>Animated: expected unsupported image in this box only.</p>"
    "<img src='/images/webp-animated.webp' width='256' height='128' alt='animation'>"
    "<p>Truncated: expected malformed image in this box only.</p>"
    "<img src='/images/webp-broken.webp' width='256' height='128' alt='broken'>"
    "<p>The page and both static images must remain usable after the two failures.</p>",
)
static("/images/webp.html", "ok", WEBP_PAGE)

WEBP_LRU_PAGE = page(
    "WebP eviction and re-decode",
    "<h1>WebP eviction and re-decode</h1>"
    "<p>The cached lossless WebP below must display. Record the i diagnostic, "
    "then scroll through every large no-store PNG to force the old WebP out of "
    "the decoded-image LRU. Return here: the WebP must reappear from the HTTP "
    "cache without changing this box.</p>"
    "<img src='/images/webp-lossless.webp' width='256' height='256' alt='WebP re-decode failed'>"
    + "".join(
        f"<h2>Pressure image {index + 1} of {LRU_IMAGE_COUNT}</h2>"
        f"<img src='/images/lru.png?webp={index}' width='384' height='240' "
        f"alt='pressure image {index + 1} failed'>"
        for index in range(LRU_IMAGE_COUNT)
    )
    + "<p>End. Record i, return to the top, wait for the WebP, and record i again.</p>",
)
static("/images/webp-lru.html", "ok", WEBP_LRU_PAGE)


def send_webp(self: "FixtureHandler", body: bytes) -> None:
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/webp"),
                ("Content-Length", str(len(body))),
                ("Cache-Control", "max-age=3600"),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/webp-lossless.webp", "download:webp-lossless")
def webp_lossless(self: "FixtureHandler", request: Request) -> None:
    send_webp(self, WEBP_LOSSLESS)


@route("/images/webp-lossy-alpha.webp", "download:webp-lossy-alpha")
def webp_lossy_alpha(self: "FixtureHandler", request: Request) -> None:
    send_webp(self, WEBP_LOSSY_ALPHA)


@route("/images/webp-animated.webp", "download:webp-animated")
def webp_animated(self: "FixtureHandler", request: Request) -> None:
    send_webp(self, WEBP_ANIMATED)


@route("/images/webp-broken.webp", "download:webp-broken")
def webp_broken(self: "FixtureHandler", request: Request) -> None:
    send_webp(self, WEBP_LOSSLESS[:-8])


@route("/images/pinned.html", "ok")
def pinned_images_page(self: "FixtureHandler", request: Request) -> None:
    """Images from a pinned page that must not lower its identity.

    Opened over the TLS listener on a board built with `tls-fixture-pins`
    (and this machine's address in `tools/pins/fixture_pins.txt`), the page
    itself is TLS PINNED. Every image then has to be HTTPS to a pinned host:
    the plaintext one and the unpinned host are refused before any request,
    and the Ed25519 listener is a pinned host whose key does not match, so it
    fails at the pin check. `no-store`, so the page is always really fetched
    and never shown from the board's cache.
    """
    if "tls" not in PORTS:
        body = page(
            "no TLS listener",
            "<h1>No TLS listener</h1><p>Start the server with a TLS port.</p>",
        )
        self.send_all(html_response(body, 503))
        return
    tls = self.origin(request, PORTS["tls"], secure=True)
    plain = self.origin(request, PORTS["http"], secure=False)
    ed25519 = self.origin(request, PORTS["tls"] + TLS_ED25519_OFFSET, secure=True)
    rows = [
        ("same pinned host over HTTPS", f"{tls}/images/checker.png", "shown"),
        ("same host over plaintext", f"{plain}/images/checker.png", "HTTPS downgrade refused"),
        (
            "host with no pin",
            "https://tls-unpinned.invalid/images/checker.png",
            "TLS identity downgrade refused (a board without pins: a DNS failure)",
        ),
        (
            "pinned host, Ed25519 key that is not the pin",
            f"{ed25519}/images/checker.png",
            "a connection failure, never the image",
        ),
    ]
    body = page(
        "pinned images",
        "<h1>Images from a pinned page</h1>"
        "<p>Tap the lock first: it must say TLS PINNED. Opened over plaintext or on "
        "a board without pins this page proves nothing.</p>"
        + "".join(
            f"<h2>{index}. {html.escape(label)}</h2>"
            f"<p>expected: {html.escape(expected)}</p>"
            f"<p><code>{html.escape(url)}</code></p>"
            f"<img src='{html.escape(url)}' width='192' height='128' alt='{html.escape(label)}'>"
            for index, (label, url, expected) in enumerate(rows, 1)
        )
        + "<p><a href='/'>index</a></p>",
    )
    self.send_all(html_response(body, extra=[("Cache-Control", "no-store")]))


@route("/images/checker.png", "download:png")
def network_checker_png(self: "FixtureHandler", request: Request) -> None:
    body = checker_png()
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/bad-crc.png", "download:png-bad-crc")
def network_bad_crc_png(self: "FixtureHandler", request: Request) -> None:
    body = bytearray(checker_png())
    body[29] ^= 1
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/slow.png", "download:png-slow")
def network_slow_png(self: "FixtureHandler", request: Request) -> None:
    body = checker_png()
    chunk = max(1, (len(body) + 4) // 5)
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
        + body[:chunk]
    )
    offset = chunk
    while offset < len(body):
        time.sleep(2)
        end = min(offset + chunk, len(body))
        self.send_all(body[offset:end])
        offset = end


@route("/images/large.png", "download:png-large")
def network_large_png(self: "FixtureHandler", request: Request) -> None:
    body = large_png()
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/lru.png", "download:png-lru-no-store")
def network_lru_png(self: "FixtureHandler", request: Request) -> None:
    body = large_png()
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Cache-Control", "no-store"),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/expanded-limit.png", "download:png-expanded-limit")
def network_expanded_limit_png(self: "FixtureHandler", request: Request) -> None:
    body = expanded_limit_png()
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Cache-Control", "max-age=3600"),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/over-expanded-limit.png", "download:png-over-expanded-limit")
def network_over_expanded_limit_png(self: "FixtureHandler", request: Request) -> None:
    valid = expanded_limit_png()
    body = valid + bytes(MAX_IMAGE_COMPRESSED_BYTES + 1 - len(valid))
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Cache-Control", "no-store"),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/wide-limit.png", "download:png-wide-limit")
def network_wide_limit_png(self: "FixtureHandler", request: Request) -> None:
    body = one_bit_grey_png(1600, 400)
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Cache-Control", "no-store"),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/over-wide-limit.png", "download:png-over-wide-limit")
def network_over_wide_limit_png(self: "FixtureHandler", request: Request) -> None:
    body = one_bit_grey_png(2049, 1)
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Cache-Control", "no-store"),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/full-hd-limit.png", "download:png-full-hd-limit")
def network_full_hd_limit_png(self: "FixtureHandler", request: Request) -> None:
    body = one_bit_grey_png(1920, 1080)
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Cache-Control", "no-store"),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/over-pixel-limit.png", "download:png-over-pixel-limit")
def network_over_pixel_limit_png(self: "FixtureHandler", request: Request) -> None:
    body = one_bit_grey_png(2048, 1025)
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Cache-Control", "no-store"),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/rgba-work-limit.png", "download:png-rgba-work-limit")
def network_rgba_work_limit_png(self: "FixtureHandler", request: Request) -> None:
    body = rgba_work_png(1024, 768)
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Cache-Control", "no-store"),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/images/over-work-limit.png", "download:png-over-work-limit")
def network_over_work_limit_png(self: "FixtureHandler", request: Request) -> None:
    body = rgba_work_png(1024, 1024)
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/png"),
                ("Content-Length", str(len(body))),
                ("Cache-Control", "no-store"),
                ("Connection", "close"),
            ],
        )
        + body
    )


# --- PNG colour types and bit depths --------------------------------------
#
# One image per format the decoder accepts, and a few it must refuse. Every
# row cycles through filter types 0-4 so the one-byte look-back used below
# eight bits per pixel is exercised, and the width is odd so packed rows end
# in padding bits. Those padding bits are set to one: a decoder that reads
# them paints a stray column at the right edge.

PNG_FORMAT_WIDTH, PNG_FORMAT_HEIGHT = 99, 40
ADAM7 = ((0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2))


def pack_png_row(samples: list[int], bit_depth: int) -> bytes:
    if bit_depth == 16:
        return b"".join(struct.pack(">H", sample) for sample in samples)
    if bit_depth == 8:
        return bytes(samples)
    out = bytearray()
    accumulator = bits = 0
    for sample in samples:
        accumulator = (accumulator << bit_depth) | sample
        bits += bit_depth
        while bits >= 8:
            bits -= 8
            out.append((accumulator >> bits) & 0xFF)
    if bits:
        out.append(((accumulator << (8 - bits)) | ((1 << (8 - bits)) - 1)) & 0xFF)
    return bytes(out)


def png_paeth(left: int, up: int, upper_left: int) -> int:
    estimate = left + up - upper_left
    distances = (abs(estimate - left), abs(estimate - up), abs(estimate - upper_left))
    if distances[0] <= distances[1] and distances[0] <= distances[2]:
        return left
    return up if distances[1] <= distances[2] else upper_left


def filter_png_rows(rows: list[bytes], bits_per_pixel: int) -> bytes:
    offset = max(1, bits_per_pixel // 8)
    out = bytearray()
    previous = bytes(len(rows[0]))
    for y, row in enumerate(rows):
        kind = y % 5
        out.append(kind)
        for index, value in enumerate(row):
            left = row[index - offset] if index >= offset else 0
            up = previous[index]
            upper_left = previous[index - offset] if index >= offset else 0
            predictor = (0, left, up, (left + up) // 2, png_paeth(left, up, upper_left))[kind]
            out.append((value - predictor) & 0xFF)
        previous = row
    return bytes(out)


def format_png(
    color_type: int,
    bit_depth: int,
    pixel: Callable[[int, int], tuple[int, ...]],
    extra: tuple[tuple[bytes, bytes], ...] = (),
    interlace: bool = False,
) -> bytes:
    width, height = PNG_FORMAT_WIDTH, PNG_FORMAT_HEIGHT
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[color_type]

    def scanlines(xs: range, ys: range) -> bytes:
        rows = [pack_png_row([s for x in xs for s in pixel(x, y)], bit_depth) for y in ys]
        return filter_png_rows(rows, channels * bit_depth)

    if interlace:
        data = b"".join(
            scanlines(range(x0, width, dx), range(y0, height, dy))
            for x0, y0, dx, dy in ADAM7
            if x0 < width and y0 < height
        )
    else:
        data = scanlines(range(width), range(height))
    ihdr = struct.pack(">IIBBBBB", width, height, bit_depth, color_type, 0, 0, int(interlace))
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + b"".join(png_chunk(kind, body) for kind, body in extra)
        + png_chunk(b"IDAT", zlib.compress(data))
        + png_chunk(b"IEND", b"")
    )


def png_step(x: int, levels: int) -> int:
    return x * levels // PNG_FORMAT_WIDTH


def png_hue_palette(count: int) -> bytes:
    out = bytearray()
    for index in range(count):
        sector, fraction = divmod(index * 6 * 255 // count, 255)
        rise, fall = fraction, 255 - fraction
        out.extend(((255, rise, 0), (fall, 255, 0), (0, 255, rise), (0, fall, 255), (rise, 0, 255), (255, 0, fall))[sector])
    return bytes(out)


def png_grey_bands(bit_depth: int) -> Callable[[int, int], tuple[int, ...]]:
    levels = 1 << bit_depth
    return lambda x, y: (png_step(x if y < PNG_FORMAT_HEIGHT // 2 else PNG_FORMAT_WIDTH - 1 - x, levels),)


def png_palette_bands(bit_depth: int) -> Callable[[int, int], tuple[int, ...]]:
    count = 1 << bit_depth
    return lambda x, y: ((png_step(x, count) + y * count // (2 * PNG_FORMAT_HEIGHT)) % count,)


# (name, format, what the screen must show, color type, bit depth, pixel, extra chunks, interlace)
PNG_FORMATS = (
    ("grey1", "greyscale 1-bit", "black and white halves, swapping at mid height", 0, 1, png_grey_bands(1), (), False),
    ("grey2", "greyscale 2-bit", "4 grey bands, dark to light on top, reversed below", 0, 2, png_grey_bands(2), (), False),
    ("grey4", "greyscale 4-bit", "16 grey bands, reversed below", 0, 4, png_grey_bands(4), (), False),
    ("grey8", "greyscale 8-bit", "smooth grey ramp, reversed below", 0, 8, png_grey_bands(8), (), False),
    (
        "grey2-trns", "greyscale 2-bit + tRNS 1", "4 bands; the second is white, not dark grey",
        0, 2, lambda x, y: (png_step(x, 4),), ((b"tRNS", b"\x00\x01"),), False,
    ),
    (
        "grey8-trns", "greyscale 8-bit + tRNS 0", "light ramp with white stripes, no black",
        0, 8, lambda x, y: (0 if x % 11 < 4 else 64 + x * 191 // PNG_FORMAT_WIDTH,), ((b"tRNS", b"\x00\x00"),), False,
    ),
    (
        "palette1", "palette 1-bit", "navy and yellow 8-pixel checkerboard, no stray right column",
        3, 1, lambda x, y: ((x // 8 + y // 8) % 2,), ((b"PLTE", bytes((20, 40, 140, 250, 210, 40))),), False,
    ),
    ("palette2", "palette 2-bit", "4 hue bands, shifting across two rows", 3, 2, png_palette_bands(2), ((b"PLTE", png_hue_palette(4)),), False),
    ("palette4", "palette 4-bit", "16 hue bands, shifting down the image", 3, 4, png_palette_bands(4), ((b"PLTE", png_hue_palette(16)),), False),
    ("palette8", "palette 8-bit", "full hue ramp, shifting down the image", 3, 8, png_palette_bands(8), ((b"PLTE", png_hue_palette(256)),), False),
    (
        "palette4-trns", "palette 4-bit + tRNS", "white fading to blue left to right, 16 steps",
        3, 4, lambda x, y: (png_step(x, 16),),
        ((b"PLTE", bytes((0, 60, 200)) * 16), (b"tRNS", bytes(index * 17 for index in range(16)))), False,
    ),
    (
        "rgb8", "RGB 8-bit", "red rising left to right, green rising downwards",
        2, 8, lambda x, y: (x * 255 // (PNG_FORMAT_WIDTH - 1), y * 255 // (PNG_FORMAT_HEIGHT - 1), 128), (), False,
    ),
    (
        "rgb8-trns", "RGB 8-bit + tRNS red", "grey; left square white, right square red",
        2, 8, lambda x, y: ((255, 0, 0) if 10 <= x < 40 and 10 <= y < 30 else (254, 0, 0) if 60 <= x < 90 and 10 <= y < 30 else (90, 90, 90)),
        ((b"tRNS", struct.pack(">HHH", 255, 0, 0)),), False,
    ),
    (
        "grey-alpha8", "greyscale + alpha 8-bit", "white fading to black left to right",
        4, 8, lambda x, y: (0, x * 255 // (PNG_FORMAT_WIDTH - 1)), (), False,
    ),
    (
        "rgba8", "RGBA 8-bit", "white fading to green left to right",
        6, 8, lambda x, y: (0, 150, 60, x * 255 // (PNG_FORMAT_WIDTH - 1)), (), False,
    ),
    ("grey16", "greyscale 16-bit", "unsupported image", 0, 16, lambda x, y: (x * 65535 // PNG_FORMAT_WIDTH,), (), False),
    (
        "palette1-adam7", "palette 1-bit, Adam7", "unsupported image",
        3, 1, lambda x, y: ((x // 8 + y // 8) % 2,), ((b"PLTE", bytes((20, 40, 140, 250, 210, 40))),), True,
    ),
    ("rgb4", "RGB 4-bit (undefined)", "broken image", 2, 4, lambda x, y: (1, 2, 3), (), False),
    ("palette2-no-plte", "palette 2-bit without PLTE", "broken image", 3, 2, png_palette_bands(2), (), False),
    (
        "palette2-index", "palette 2-bit, index past PLTE", "broken image",
        3, 2, png_palette_bands(2), ((b"PLTE", png_hue_palette(2)),), False,
    ),
)

PNG_FORMAT_BODIES = {
    name: format_png(color_type, bit_depth, pixel, extra, interlace)
    for name, _, _, color_type, bit_depth, pixel, extra, interlace in PNG_FORMATS
}


def png_format_handler(body: bytes) -> Handler:
    def handler(self: "FixtureHandler", request: Request) -> None:
        self.send_all(
            head(
                200,
                [
                    ("Content-Type", "image/png"),
                    ("Content-Length", str(len(body))),
                    ("Connection", "close"),
                ],
            )
            + body
        )

    return handler


for _name, _body in PNG_FORMAT_BODIES.items():
    route(f"/images/formats/{_name}.png", f"download:png-{_name}")(png_format_handler(_body))

static(
    "/images/png-formats.html",
    "ok",
    page(
        "PNG formats",
        "<h1>PNG colour types and bit depths</h1>"
        "<p>Each image is 99x40, drawn at 198x80. Transparency composites over white.</p>"
        "<table border='1'><tr><th>Format</th><th>Expected</th><th>Image</th></tr>"
        + "".join(
            f"<tr><td>{html.escape(label)}</td><td>{html.escape(expected)}</td>"
            f"<td><img src='/images/formats/{name}.png' alt='{html.escape(name)}' width='198' height='80'></td></tr>"
            for name, label, expected, *_ in PNG_FORMATS
        )
        + "</table><p><a href='/'>index</a></p>",
    ),
)


@route("/images/landscape.jpg", "download:jpeg")
def network_baseline_jpeg(self: "FixtureHandler", request: Request) -> None:
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "image/jpeg"),
                ("Content-Length", str(len(BASELINE_JPEG))),
                ("Connection", "close"),
            ],
        )
        + BASELINE_JPEG
    )
static("/image.html", "ok", IMAGE)
static("/rule.html", "ok", RULE)
static("/script.html", "ok", SCRIPT)
static("/empty.html", "ok", EMPTY)
static("/links/", "ok", LINKS_INDEX)
static("/links/index.html", "ok", LINKS_INDEX)
static("/links/target.html", "ok", LINKS_TARGET)
static("/links/deep/target.html", "ok", LINKS_DEEP)
static("/broken/unclosed.html", "ok", UNCLOSED)
static("/broken/deep-nest.html", "ok", DEEP_NEST)
static("/broken/huge-attribute.html", "ok", HUGE_ATTRIBUTE)
static("/broken/bad-utf8.html", "ok", BAD_UTF8)
static("/limit/url", "ok", OVER_LONG_URL_PAGE)
static("/limit/longline", "ok", LONG_LINE)


@route("/encoding/shift-jis", "ok")
def shift_jis_header(self: "FixtureHandler", request: Request) -> None:
    """Shift_JIS declared in the header and nowhere else."""
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=Shift_JIS"),
                ("Content-Length", str(len(SHIFT_JIS_HEADER))),
                ("Connection", "close"),
            ],
        )
        + SHIFT_JIS_HEADER
    )


@route("/encoding/shift-jis-meta", "ok")
def shift_jis_meta(self: "FixtureHandler", request: Request) -> None:
    """Shift_JIS declared only by `<meta>`, with no `charset` in the header.

    The common shape by a wide margin: a server that serves every file as
    `text/html` with no parameter, and pages that say it themselves.
    """
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html"),
                ("Content-Length", str(len(SHIFT_JIS_META))),
                ("Connection", "close"),
            ],
        )
        + SHIFT_JIS_META
    )


@route("/encoding/shift-jis-lying-meta", "ok")
def shift_jis_lying_meta(self: "FixtureHandler", request: Request) -> None:
    """A header that says Shift_JIS over a `<meta>` that says UTF-8.

    The header wins. This is the one case where the two sources disagree,
    and it happens for real whenever a page is converted and its `<meta>`
    is not.
    """
    body = SHIFT_JIS_META.replace(b"Shift_JIS", b"utf-8xxxx")
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=shift_jis"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/encoding/shift-jis-broken", "ok")
def shift_jis_broken(self: "FixtureHandler", request: Request) -> None:
    """Shift_JIS with one lead byte whose trail byte is missing."""
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=shift_jis"),
                ("Content-Length", str(len(SHIFT_JIS_BROKEN))),
                ("Connection", "close"),
            ],
        )
        + SHIFT_JIS_BROKEN
    )


@route("/encoding/euc-jp", "ok")
def euc_jp_header(self: "FixtureHandler", request: Request) -> None:
    """EUC-JP declared in the header and nowhere else."""
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=EUC-JP"),
                ("Content-Length", str(len(EUC_JP_HEADER))),
                ("Connection", "close"),
            ],
        )
        + EUC_JP_HEADER
    )


@route("/encoding/euc-jp-meta", "ok")
def euc_jp_meta(self: "FixtureHandler", request: Request) -> None:
    """EUC-JP declared only by `<meta>`."""
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html"),
                ("Content-Length", str(len(EUC_JP_META))),
                ("Connection", "close"),
            ],
        )
        + EUC_JP_META
    )


@route("/encoding/euc-jp-broken", "ok")
def euc_jp_broken(self: "FixtureHandler", request: Request) -> None:
    """EUC-JP with one lead byte whose trail byte is missing."""
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=euc-jp"),
                ("Content-Length", str(len(EUC_JP_BROKEN))),
                ("Connection", "close"),
            ],
        )
        + EUC_JP_BROKEN
    )


static("/encoding/utf8-bom.html", "ok", UTF8_BOM)


@route("/plain.txt", "ok")
def plain_text(self: "FixtureHandler", request: Request) -> None:
    """`text/plain`, which the viewer shows as itself rather than refusing."""
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/plain; charset=utf-8"),
                ("Content-Length", str(len(PLAIN))),
                ("Connection", "close"),
            ],
        )
        + PLAIN
    )


@route("/plain-sjis.txt", "ok")
def plain_text_shift_jis(self: "FixtureHandler", request: Request) -> None:
    """Plain text in Shift_JIS: the two decisions are independent."""
    body = PLAIN.decode("utf-8").encode(SJIS_CODEC)
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/plain; charset=shift_jis"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/notatype.bin", "error:not-html")
def not_a_type(self: "FixtureHandler", request: Request) -> None:
    """A media type that is neither HTML nor text, which is still refused."""
    body = bytes(range(256)) * 4
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "application/octet-stream"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
        + body
    )


@route("/status/404", "error:status-404")
def status_404(self: "FixtureHandler", request: Request) -> None:
    body = page("404", "<h1>404</h1><p>No such fixture.</p>")
    self.send_all(html_response(body, 404))


@route("/status/500", "error:status-500")
def status_500(self: "FixtureHandler", request: Request) -> None:
    body = page("500", "<h1>500</h1><p>The fixture server failed on purpose.</p>")
    self.send_all(html_response(body, 500))


# --- transfers ------------------------------------------------------------

@route("/slow", "ok")
def slow(self: "FixtureHandler", request: Request) -> None:
    """One byte per write, headers included.

    The delay is per byte rather than one long pause at the start because
    the thing being tested is that the viewer stays responsive *while*
    receiving, not that it survives a stall.
    """
    response = html_response(SLOW)
    for index in range(len(response)):
        self.send_all(response[index : index + 1])
        time.sleep(0.002)


@route("/chunked", "ok")
def chunked(self: "FixtureHandler", request: Request) -> None:
    """Chunked, with boundaries that advance on every request.

    The step is derived from a counter rather than randomly so that a run
    is reproducible: request N always splits at the same offsets, and a
    failure can be repeated by asking N times again.
    """
    step = next_chunk_step()
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Transfer-Encoding", "chunked"),
                ("Connection", "close"),
            ],
        )
    )
    body = CHUNKED
    offset = 0
    size = step
    while offset < len(body):
        piece = body[offset : offset + size]
        self.send_all(f"{len(piece):x}\r\n".encode("ascii") + piece + b"\r\n")
        offset += len(piece)
        # Vary within the response too, so one response covers several
        # boundary positions rather than a single stride.
        size = size % 37 + 1
    self.send_all(b"0\r\n\r\n")


@route("/chunked-trailer", "ok")
def chunked_trailer(self: "FixtureHandler", request: Request) -> None:
    """Chunk extensions and a trailer section, both of which are skipped."""
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Transfer-Encoding", "chunked"),
                ("Trailer", "X-Fixture"),
                ("Connection", "close"),
            ],
        )
    )
    body = CHUNKED
    offset = 0
    while offset < len(body):
        piece = body[offset : offset + 64]
        self.send_all(
            f"{len(piece):x};name=value;bare\r\n".encode("ascii") + piece + b"\r\n"
        )
        offset += len(piece)
    self.send_all(b"0;last\r\nX-Fixture: done\r\nX-Other: also\r\n\r\n")


@route("/chunked-bad", "error:chunk")
def chunked_bad(self: "FixtureHandler", request: Request) -> None:
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Transfer-Encoding", "chunked"),
                ("Connection", "close"),
            ],
        )
    )
    self.send_all(b"20\r\n" + b"a" * 0x20 + b"\r\n")
    self.send_all(b"not-a-hex-size\r\nwhatever\r\n")


@route("/gzip", "error:encoding")
def gzip_encoded(self: "FixtureHandler", request: Request) -> None:
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Content-Encoding", "gzip"),
                ("Content-Length", str(len(GZIP))),
                ("Connection", "close"),
            ],
        )
        + GZIP
    )


# --- redirects ------------------------------------------------------------

for code in (301, 302, 307, 308):
    @route(f"/redirect/{code}", "ok")
    def redirect_code(self: "FixtureHandler", request: Request, code=code) -> None:
        self.send_all(redirect("/simple.html", code))


@route("/redirect/relative", "ok")
def redirect_relative(self: "FixtureHandler", request: Request) -> None:
    """A `Location` that is a relative reference, which RFC 7231 allows.

    Resolving it against the request URL rather than treating it as a path
    is the difference between landing on `/links/target.html` and asking
    for `/target.html`.
    """
    self.send_all(redirect("../links/target.html", 302))


@route("/redirect/loop", "error:redirect-limit")
def redirect_loop(self: "FixtureHandler", request: Request) -> None:
    self.send_all(redirect("/redirect/loop", 302))


@route("/redirect/https", "ok")
def redirect_https(self: "FixtureHandler", request: Request) -> None:
    """Both halves of the no-downgrade rule, from whichever side is asking.

    Over plaintext this redirects *up* to the TLS listener, which the viewer
    is supposed to follow: the page it lands on is this same index, served
    over TLS, so the expectation is `ok`.

    Over TLS it redirects *down* to the plaintext listener, which the viewer
    is supposed to refuse with `https-downgrade` -- and refusing it is the
    whole rule. That direction cannot be produced from a plaintext-only
    server, which is why the TLS listener exists.
    """
    self.send_all(redirect(self.other_scheme_origin(request) + "/", 302))


@route("/redirect/unpinned", "error:dns")
def redirect_unpinned(self: "FixtureHandler", request: Request) -> None:
    """A redirect to an `https` host this firmware has no pin for.

    From a *pinned* connection that is `tls-auth-downgrade`: the identity
    that was established is being traded for one that cannot be. From an
    unpinned one there was no identity to lose, so the viewer follows it and
    fails at the name, which does not resolve.

    Which of the two the manifest asks for therefore depends on the build as
    well as the connection -- see `tls_manifest_entries`.
    """
    self.send_all(redirect("https://tls-unpinned.invalid/", 302))


def redirect_chain(self: "FixtureHandler", request: Request) -> None:
    """`/redirect/chain/N` redirects exactly N times, then serves a page.

    `N == MAX_REDIRECTS` has to succeed and `N == MAX_REDIRECTS + 1` has to
    fail; a limit that is off by one shows up here and nowhere else -- so
    the count here has to be exact, and it was not at first. The original
    also redirected at zero, which made `/redirect/chain/N` take N+1 hops
    and turned `chain/5` into a case that correctly failed against a limit
    of five while claiming to be inside it.

    The last hop goes to `/simple.html` rather than to `chain/0`, so a
    successful chain ends somewhere visibly different from where it
    started; `chain/0` itself serves the page directly, with no redirect at
    all.
    """
    remaining = request.path.rsplit("/", 1)[-1]
    try:
        count = int(remaining)
    except ValueError:
        self.send_all(html_response(page("bad", "<p>chain needs a number</p>"), 404))
        return
    if count <= 0:
        self.send_all(html_response(SIMPLE))
        return
    if count == 1:
        self.send_all(redirect("/simple.html", 302))
        return
    self.send_all(redirect(f"/redirect/chain/{count - 1}", 302))


ROUTES["/redirect/chain/"] = redirect_chain
MANIFEST.append((f"/redirect/chain/{MAX_REDIRECTS}", "ok"))
MANIFEST.append((f"/redirect/chain/{MAX_REDIRECTS + 1}", "error:redirect-limit"))


# --- failures -------------------------------------------------------------

@route("/fail/truncate-body", "error:truncated")
def truncate_body(self: "FixtureHandler", request: Request) -> None:
    body = page("truncated", "<h1>Truncated</h1><p>" + "t" * 4096 + "</p>")
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
        + body[: len(body) // 2]
    )
    self.hard_close()


@route("/fail/truncate-head", "error:truncated")
def truncate_head(self: "FixtureHandler", request: Request) -> None:
    self.send_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Len")
    self.hard_close()


@route("/fail/big-header", "error:header-limit")
def big_header(self: "FixtureHandler", request: Request) -> None:
    """A header block past `MAX_HEADER_BYTES` with no blank line in reach."""
    self.send_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n")
    written = 0
    while written < MAX_HEADER_BYTES * 2:
        line = f"X-Filler-{written}: {'f' * 64}\r\n".encode("ascii")
        self.send_all(line)
        written += len(line)
    self.send_all(b"\r\n")
    self.send_all(SIMPLE)


@route("/fail/length-over", "error:truncated")
def length_over(self: "FixtureHandler", request: Request) -> None:
    """Promises more than it sends, then closes."""
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Content-Length", str(len(SIMPLE) + 4096)),
                ("Connection", "close"),
            ],
        )
        + SIMPLE
    )
    self.hard_close()


@route("/fail/length-under", "ok")
def length_under(self: "FixtureHandler", request: Request) -> None:
    """Sends more than it promised.

    The extra is not an error: `Content-Length` is where the body ends, so
    the trailing bytes belong to nothing and are dropped. What must not
    happen is the surplus reaching the document.
    """
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Content-Length", str(len(SIMPLE))),
                ("Connection", "close"),
            ],
        )
        + SIMPLE
        + page("surplus", "<h1>SURPLUS</h1><p>This must not be displayed.</p>")
    )


@route("/fail/no-status", "error:not-http")
def no_status(self: "FixtureHandler", request: Request) -> None:
    self.send_all(b"this is not a status line\r\n\r\n<html><body>hi</body></html>")


# --- limits ---------------------------------------------------------------

@route("/limit/input", "error:body-limit")
def limit_input(self: "FixtureHandler", request: Request) -> None:
    """Announces a length past the input bound.

    The point is that it is refused *before* the body is read: the response
    below would take megabytes to prove otherwise.
    """
    body = oversize_body()
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
    )
    self.send_all(body)


@route("/limit/input-nolength", "error:body-limit")
def limit_input_nolength(self: "FixtureHandler", request: Request) -> None:
    """The same body with no length at all, so the bound has to apply to
    what has arrived rather than to what was announced."""
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Connection", "close"),
            ],
        )
    )
    self.send_all(oversize_body())


@route("/limit/text", "error:text-limit")
def limit_text(self: "FixtureHandler", request: Request) -> None:
    self.send_all(html_response(text_heavy_body()))


@route("/limit/items", "error:item-limit")
def limit_items(self: "FixtureHandler", request: Request) -> None:
    self.send_all(html_response(repeated_items(MAX_ITEMS + 64)))


@route("/limit/links", "excess links ignored")
def limit_links(self: "FixtureHandler", request: Request) -> None:
    self.send_all(html_response(repeated_links(MAX_LINKS + 64)))


# The documents worth checking a parser against, by the name they are
# written out under. `--dump` writes these so that the Rust crate's tests
# can parse the very bytes the board will be served, rather than a second
# copy of them that drifts.
#
# The oversize and repeated-element fixtures are deliberately absent: they
# are megabytes, they exist to trip a limit rather than to be parsed, and
# the limits already have unit tests that do not need a file.
DUMPABLE = {
    "index.html": INDEX,
    "simple.html": SIMPLE,
    "long.html": LONG,
    "headings.html": HEADINGS,
    "list.html": LIST,
    "pre.html": PRE,
    "entity.html": ENTITY,
    "utf8.html": UTF8,
    # Not UTF-8, and that is the point: the crate's fixture test decodes
    # these through the same `Parser` the board does, so the bytes checked
    # on the host are the bytes served to the board.
    "shift-jis.sjis.html": SHIFT_JIS_HEADER,
    "shift-jis-meta.sjis.html": SHIFT_JIS_META,
    "shift-jis-broken.sjis.html": SHIFT_JIS_BROKEN,
    "utf8-bom.html": UTF8_BOM,
    "plain.txt": PLAIN,
    "inline.html": INLINE,
    "image.html": IMAGE,
    "rule.html": RULE,
    "script.html": SCRIPT,
    "empty.html": EMPTY,
    "links-index.html": LINKS_INDEX,
    "links-target.html": LINKS_TARGET,
    "links-deep.html": LINKS_DEEP,
    "broken-unclosed.html": UNCLOSED,
    "broken-deep-nest.html": DEEP_NEST,
    "broken-huge-attribute.html": HUGE_ATTRIBUTE,
    "broken-bad-utf8.html": BAD_UTF8,
    "slow.html": SLOW,
    "chunked.html": CHUNKED,
    "limit-url.html": OVER_LONG_URL_PAGE,
    "limit-longline.html": LONG_LINE,
}


def dump(directory: str) -> int:
    """Writes the parsable fixtures out for the Rust crate's tests."""
    target = Path(directory)
    target.mkdir(parents=True, exist_ok=True)
    changed = 0
    for name, content in sorted(DUMPABLE.items()):
        path = target / name
        if not path.exists() or path.read_bytes() != content:
            path.write_bytes(content)
            changed += 1
            print(f"wrote {path} ({len(content)} bytes)")
    print(f"{len(DUMPABLE)} fixtures, {changed} changed")
    return 0


# --- plain downloads, for the httpget regression --------------------------

def filler(size: int) -> bytes:
    """A deterministic byte pattern of `size` bytes.

    Derived from the index rather than random so the CRC-32 below is stable
    across runs of this server: the board's `hs` command CRC-32s the body it
    decoded, and a target that moved every restart would make that
    comparison meaningless. (`httpget` saves the same endpoint, but nothing
    on the board checksums a saved file -- `fsverify` re-checks mounts
    against their media, which is a different question.)
    """
    return bytes((index * 31 + (index >> 8) * 17) & 0xFF for index in range(size))


DOWNLOADS = {
    "/download/64k.bin": filler(64 * 1024),
    "/download/512k.bin": filler(512 * 1024),
}


def crc32(data: bytes) -> int:
    import zlib

    return zlib.crc32(data) & 0xFFFFFFFF


for path, content in DOWNLOADS.items():
    @route(path, f"download:crc32={crc32(content):08x}")
    def download(self: "FixtureHandler", request: Request, content=content) -> None:
        self.send_all(
            head(
                200,
                [
                    ("Content-Type", "application/octet-stream"),
                    ("Content-Length", str(len(content))),
                    ("Connection", "close"),
                ],
            )
            + content
        )


@route("/require-user-agent", "ok")
def require_user_agent(self: "FixtureHandler", request: Request) -> None:
    """403 unless the request carried a `User-Agent`, like much of the web.

    Measured on 2026-08-28: `en.wikipedia.org` and `stackoverflow.com` both
    answer 403 to this firmware's request with the header removed. That was
    a real bug -- pages that a PC could open and the board could not -- and
    it was invisible from here until this fixture existed, because nothing
    else on this server cares what asked.

    So it is a fixture rather than a note: if the header is ever dropped
    again this comes back as `status` instead of `ok`.
    """
    agent = request.headers.get("user-agent", "")
    if not agent:
        body = page("no user agent", "<h1>403</h1><p>Send a User-Agent.</p>")
        self.send_all(html_response(body, 403))
        return
    body = page(
        "user agent",
        f"<h1>User-Agent</h1><p>You said: {agent}</p>",
    )
    self.send_all(html_response(body, 200))


# --- HTTP cache --------------------------------------------------------------
#
# Every endpoint counts what it received, and `/cache/stats.html` (no-store,
# no validator) shows the counters. None of them is in the GET manifest: the
# walk would change the counts the acceptance check reads.

STATUS_TEXT.setdefault(304, "Not Modified")
CACHE_COUNTS: dict[str, dict[str, int]] = {}
CACHE_STATE = {"version": 1}
CACHE_PATHS = (
    "/cache/counter.html",
    "/cache/max-age.html",
    "/cache/expires.html",
    "/cache/plain.html",
    "/cache/no-cache.html",
    "/cache/expired.html",
    "/cache/etag.html",
    "/cache/no-store.html",
    "/cache/vary.html",
    "/cache/vary-encoding.html",
    "/cache/large.html",
    "/cache/image.png",
)


def send_conditional(
    self: "FixtureHandler",
    request: Request,
    body: bytes,
    content_type: str,
    etag: str,
    extra: tuple[tuple[str, str], ...] = (),
) -> None:
    from email.utils import formatdate

    counter = request.target if request.path == "/cache/sized.html" else request.path
    counts = CACHE_COUNTS.setdefault(
        counter, {"requests": 0, "conditional": 0, "200": 0, "304": 0}
    )
    extra = (("Date", formatdate(usegmt=True)), *extra)
    counts["requests"] += 1
    offered = request.headers.get("if-none-match")
    if offered is not None:
        counts["conditional"] += 1
    if offered == etag:
        counts["304"] += 1
        self.send_all(head(304, [("ETag", etag), *extra, ("Connection", "close")]))
        return
    counts["200"] += 1
    self.send_all(
        head(
            200,
            [
                ("Content-Type", content_type),
                ("Content-Length", str(len(body))),
                ("ETag", etag),
                *extra,
                ("Connection", "close"),
            ],
        )
        + body
    )


def cache_page(title: str, text: str, filler: str = "") -> bytes:
    return page(
        title,
        f"<h1>{html.escape(title)}</h1>{text}"
        f"<p id='served'>served at {time.strftime('%H:%M:%S')}</p>"
        "<p><a href='/cache/stats.html'>counters</a> | "
        "<a href='/cache/index.html'>cache fixtures</a></p>" + filler,
    )


CACHE_INDEX = page(
    "HTTP cache fixtures",
    "<h1>HTTP cache fixtures</h1>"
    "<p>Open a page, go to the counters and come back, or reload with r (revalidate) "
    "or R (forced). A page shown from the cache keeps its served-at time; a fresh one "
    "is not requested at all.</p><ul>"
    "<li><a href='/cache/counter.html'>counter.html</a> - cached for an hour; a POST to "
    "it, or a POST answered with a 303 to it, must make the next visit fetch it again</li>"
    "<li><a href='/cache/max-age.html'>max-age.html</a> - max-age=60: no request for "
    "60 s, then purged and fetched again</li>"
    "<li><a href='/cache/expires.html'>expires.html</a> - Expires 30 s after Date</li>"
    "<li><a href='/cache/plain.html'>plain.html</a> - no cache headers: fresh for the "
    "default hour</li>"
    "<li><a href='/cache/no-cache.html'>no-cache.html</a> - every use is a 304</li>"
    "<li><a href='/cache/expired.html'>expired.html</a> - max-age=0: never kept</li>"
    "<li><a href='/cache/etag.html'>etag.html</a> - default hour; r: 304, R: 200</li>"
    "<li>sized.html - distinct 400 KiB pages, fresh for an hour; open them one after "
    "another to fill /tmp and force a purge: "
    + " ".join(
        f"<a href='/cache/sized.html?kib=400&amp;id={ident}'>{ident}</a>"
        for ident in range(1, 21)
    )
    + " | 100 KiB: "
    + " ".join(
        f"<a href='/cache/sized.html?kib=100&amp;id={ident}'>{ident}</a>"
        for ident in range(1, 11)
    )
    + "</li>"
    "<li><a href='/cache/bump'>bump</a> - change etag.html's version</li>"
    "<li><a href='/cache/no-store.html'>no-store.html</a> - never conditional</li>"
    "<li><a href='/cache/vary.html'>vary.html</a> - Vary: User-Agent, never conditional</li>"
    "<li><a href='/cache/vary-encoding.html'>vary-encoding.html</a> - "
    "Vary: Accept-Encoding, conditional</li>"
    "<li><a href='/cache/large.html'>large.html</a> - over one entry, never conditional</li>"
    "<li><a href='/cache/image.html'>image.html</a> - the image is revalidated</li>"
    "<li><a href='/cache/stats.html'>stats.html</a> - counters</li>"
    "<li><a href='/cache/reset'>reset</a> - clear the counters</li>"
    "</ul><p><a href='/'>index</a></p>",
)


def cache_index(self: "FixtureHandler", request: Request) -> None:
    self.send_all(html_response(CACHE_INDEX))


def cache_etag(self: "FixtureHandler", request: Request) -> None:
    version = CACHE_STATE["version"]
    send_conditional(
        self,
        request,
        cache_page(f"ETag version {version}", "<p>Revalidated with If-None-Match.</p>"),
        "text/html; charset=utf-8",
        f'"etag-v{version}"',
    )


def cache_max_age(self: "FixtureHandler", request: Request) -> None:
    send_conditional(
        self,
        request,
        cache_page("max-age=60", "<p>Fresh for 60 seconds.</p>"),
        "text/html; charset=utf-8",
        '"max-age"',
        (("Cache-Control", "max-age=60"),),
    )


def cache_expires(self: "FixtureHandler", request: Request) -> None:
    from email.utils import formatdate

    send_conditional(
        self,
        request,
        cache_page("Expires", "<p>Expires 30 seconds after its Date.</p>"),
        "text/html; charset=utf-8",
        '"expires"',
        (("Expires", formatdate(time.time() + 30, usegmt=True)),),
    )


def cache_plain(self: "FixtureHandler", request: Request) -> None:
    body = cache_page("no cache headers", "<p>No ETag, no Cache-Control.</p>")
    counts = CACHE_COUNTS.setdefault(
        request.path, {"requests": 0, "conditional": 0, "200": 0, "304": 0}
    )
    counts["requests"] += 1
    counts["200"] += 1
    if request.headers.get("if-none-match") is not None:
        counts["conditional"] += 1
    self.send_all(html_response(body))


def cache_no_cache(self: "FixtureHandler", request: Request) -> None:
    send_conditional(
        self,
        request,
        cache_page("no-cache", "<p>Stored, but every use must be confirmed.</p>"),
        "text/html; charset=utf-8",
        '"no-cache"',
        (("Cache-Control", "no-cache"),),
    )


def cache_expired(self: "FixtureHandler", request: Request) -> None:
    send_conditional(
        self,
        request,
        cache_page("max-age=0", "<p>Stale on arrival.</p>"),
        "text/html; charset=utf-8",
        '"expired"',
        (("Cache-Control", "max-age=0"),),
    )


def cache_sized(self: "FixtureHandler", request: Request) -> None:
    from urllib.parse import parse_qs

    query = parse_qs(request.query)
    try:
        kib = max(1, min(int(query.get("kib", ["400"])[0]), 500))
    except ValueError:
        kib = 400
    ident = html.escape(query.get("id", ["1"])[0])
    filler = "<p>" + "z" * (kib * 1024) + "</p>"
    send_conditional(
        self,
        request,
        cache_page(f"sized {kib} KiB id {ident}", "<p>Fresh for an hour.</p>", filler),
        "text/html; charset=utf-8",
        f'"sized-{kib}-{ident}"',
        (("Cache-Control", "max-age=3600"),),
    )


CACHE_COUNTER = {"value": 0, "posts": 0}


def cache_counter(self: "FixtureHandler", request: Request) -> None:
    if request.method == "POST":
        CACHE_COUNTER["value"] += 1
        CACHE_COUNTER["posts"] += 1
        body = page(
            "counter updated",
            f"<h1>Counter updated to {CACHE_COUNTER['value']}</h1>"
            "<p>This POST went to counter.html itself.</p>"
            "<p><a href='/cache/counter.html'>back to the counter</a> | "
            "<a href='/cache/stats.html'>counters</a></p>",
        )
        self.send_all(html_response(body))
        return
    value = CACHE_COUNTER["value"]
    send_conditional(
        self,
        request,
        cache_page(
            f"counter {value}",
            f"<p id='value'>counter value: {value} (POSTs so far: {CACHE_COUNTER['posts']})</p>"
            "<form action='/cache/counter.html' method='post'>"
            "<button name='go' value='same'>POST to this URL</button></form>"
            "<form action='/cache/counter-prg' method='post'>"
            "<button name='go' value='prg'>POST, then 303 back here</button></form>"
            "<p><a href='/cache/plain.html'>another page</a> (then Backspace back here)</p>",
        ),
        "text/html; charset=utf-8",
        f'"counter-{value}"',
        (("Cache-Control", "max-age=3600"),),
    )


def cache_counter_prg(self: "FixtureHandler", request: Request) -> None:
    if request.method == "POST":
        CACHE_COUNTER["value"] += 1
        CACHE_COUNTER["posts"] += 1
    self.send_all(redirect("/cache/counter.html", 303))


def cache_bump(self: "FixtureHandler", request: Request) -> None:
    CACHE_STATE["version"] += 1
    self.send_all(redirect("/cache/stats.html", 303))


def cache_no_store(self: "FixtureHandler", request: Request) -> None:
    send_conditional(
        self,
        request,
        cache_page("no-store", "<p>Cache-Control: no-store with an ETag.</p>"),
        "text/html; charset=utf-8",
        '"no-store"',
        (("Cache-Control", "no-store"),),
    )


def cache_vary(self: "FixtureHandler", request: Request) -> None:
    send_conditional(
        self,
        request,
        cache_page("Vary: User-Agent", "<p>Cannot be matched to a later request.</p>"),
        "text/html; charset=utf-8",
        '"vary"',
        (("Vary", "User-Agent"),),
    )


def cache_vary_encoding(self: "FixtureHandler", request: Request) -> None:
    send_conditional(
        self,
        request,
        cache_page("Vary: Accept-Encoding", "<p>The client sends a fixed value.</p>"),
        "text/html; charset=utf-8",
        '"vary-encoding"',
        (("Vary", "Accept-Encoding"),),
    )


LARGE_CACHE_FILLER = "".join(
    f"<p>cache filler {index:05d}: " + "y" * 96 + "</p>" for index in range(6000)
)


def cache_large(self: "FixtureHandler", request: Request) -> None:
    send_conditional(
        self,
        request,
        cache_page("large", "<p>Larger than one cache entry.</p>", LARGE_CACHE_FILLER),
        "text/html; charset=utf-8",
        '"large"',
    )


def make_cache_png() -> bytes:
    width = height = 32
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            rows += bytes((x * 8, y * 8, 160))
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + png_chunk(b"IDAT", zlib.compress(bytes(rows)))
        + png_chunk(b"IEND", b"")
    )


CACHE_PNG = make_cache_png()
CACHE_IMAGE_PAGE = page(
    "cache image",
    "<h1>Cached image</h1><p><img src='/cache/image.png' width='128' height='128' "
    "alt='cache image'></p><p><a href='/cache/stats.html'>counters</a> | "
    "<a href='/cache/index.html'>cache fixtures</a></p>",
)


def cache_image_page(self: "FixtureHandler", request: Request) -> None:
    self.send_all(html_response(CACHE_IMAGE_PAGE, extra=[("Cache-Control", "no-store")]))


def cache_image(self: "FixtureHandler", request: Request) -> None:
    send_conditional(self, request, CACHE_PNG, "image/png", '"png-v1"')


def cache_stats(self: "FixtureHandler", request: Request) -> None:
    rows = "".join(
        f"<tr><td>{path}</td>"
        + "".join(
            f"<td>{CACHE_COUNTS.get(path, {}).get(key, 0)}</td>"
            for key in ("requests", "conditional", "200", "304")
        )
        + "</tr>"
        for path in (*CACHE_PATHS, *sorted(key for key in CACHE_COUNTS if key.startswith("/cache/sized")))
    )
    body = page(
        "cache counters",
        "<h1>Cache counters</h1>"
        f"<p>etag.html version: {CACHE_STATE['version']}</p>"
        "<table border='1'><tr><th>path</th><th>requests</th><th>If-None-Match</th>"
        f"<th>200</th><th>304</th></tr>{rows}</table>"
        "<p><a href='/cache/index.html'>cache fixtures</a> | "
        "<a href='/cache/reset'>reset</a></p>",
    )
    self.send_all(html_response(body, extra=[("Cache-Control", "no-store")]))


def cache_reset(self: "FixtureHandler", request: Request) -> None:
    CACHE_COUNTS.clear()
    CACHE_COUNTER.update(value=0, posts=0)
    self.send_all(redirect("/cache/stats.html", 303))


ROUTES["/cache/index.html"] = cache_index
ROUTES["/cache/etag.html"] = cache_etag
ROUTES["/cache/counter.html"] = cache_counter
ROUTES["/cache/counter-prg"] = cache_counter_prg
ROUTES["/cache/max-age.html"] = cache_max_age
ROUTES["/cache/expires.html"] = cache_expires
ROUTES["/cache/plain.html"] = cache_plain
ROUTES["/cache/no-cache.html"] = cache_no_cache
ROUTES["/cache/expired.html"] = cache_expired
ROUTES["/cache/sized.html"] = cache_sized
ROUTES["/cache/bump"] = cache_bump
ROUTES["/cache/no-store.html"] = cache_no_store
ROUTES["/cache/vary.html"] = cache_vary
ROUTES["/cache/vary-encoding.html"] = cache_vary_encoding
ROUTES["/cache/large.html"] = cache_large
ROUTES["/cache/image.html"] = cache_image_page
ROUTES["/cache/image.png"] = cache_image
ROUTES["/cache/stats.html"] = cache_stats
ROUTES["/cache/reset"] = cache_reset


@route("/manifest.txt", "text")
def manifest(self: "FixtureHandler", request: Request) -> None:
    """The walk's contract, written for the connection that asked for it.

    Two endpoints mean different things depending on whether this request
    arrived over TLS -- an upgrade is a success and a downgrade is a refusal,
    and which one `/redirect/https` performs depends on which way round it
    already is. Serving one static list would make one of the two wrong, so
    the list is built per connection by the server that will also answer it.

    The absolute-URL entries are the TLS failure fixtures, which live on
    their own ports. They are absolute so that a plaintext walk reaches them
    too: whether the board can be talked out of a TLS failure has nothing to
    do with how it read the manifest.
    """
    entries = dict(MANIFEST)
    entries.update(self.tls_manifest_entries(request))
    lines = "".join(f"{path}\t{outcome}\n" for path, outcome in sorted(entries.items()))
    body = lines.encode("utf-8")
    self.send_all(
        head(
            200,
            [
                ("Content-Type", "text/plain; charset=utf-8"),
                ("Content-Length", str(len(body))),
                ("Connection", "close"),
            ],
        )
        + body
    )


# --------------------------------------------------------------------------
# Server.
# --------------------------------------------------------------------------

_chunk_step_lock = threading.Lock()
_chunk_step = 0


def next_chunk_step() -> int:
    """The chunk size `/chunked` starts from, advancing per request."""
    global _chunk_step
    with _chunk_step_lock:
        _chunk_step += 1
        return (_chunk_step % 97) + 1


class FixtureServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


# Which extra listeners exist, and what each is for. The offsets are from
# `--tls-port` so that one number moves them all.
TLS_ALERT_OFFSET = 1
TLS_ED25519_OFFSET = 2
TLS_RSA_OFFSET = 3
TLS_12_OFFSET = 4


class AlertHandler(socketserver.BaseRequestHandler):
    """Answers a ClientHello with a fatal TLS alert and nothing else.

    Hand-written rather than produced by refusing something in OpenSSL,
    because what is being tested is that the board reports `tls-alert` --
    a peer that said no -- rather than folding it into a timeout or a
    connection error. Seven bytes is the whole of it: a TLS record of type
    alert (0x15), version TLS 1.2 as the record layer always claims,
    length 2, then fatal (2) and the description.

    `access_denied` and not `handshake_failure`: the firmware reads
    `handshake_failure`, `protocol_version` and `insufficient_security` as
    "nothing in common to speak" and reports those as `tls-version`, which
    is the more useful answer. This fixture is for the other kind of no --
    a server that could have talked and would not.
    """

    RECORD = bytes([0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 49])

    def handle(self) -> None:
        try:
            # Wait until the ClientHello is actually on the wire, so the
            # alert is an answer rather than a race with the connect.
            self.request.recv(4096)
            self.request.sendall(self.RECORD)
        except OSError:
            pass


class AlertServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


class TlsFixtureServer(FixtureServer):
    """The same fixtures, over TLS 1.3.

    A second listener rather than a mode, so that one run of this serves
    both and a walk can compare them. What it is for is the pinning
    fixtures: the board decides whether a connection is authenticated by
    hashing the certificate's public key, so which key this presents is the
    whole experiment -- `tools/tls/fixture-current` is the pinned one,
    `fixture-next` is the rotation pin, and `fixture-other` is a key that is
    not pinned and must be refused.

    TLS 1.3 only, matching what the board speaks. Nothing here checks a
    client certificate: the board does not send one.
    """

    def __init__(
        self,
        address,
        handler,
        certificate: Path,
        key: Path,
        version: ssl.TLSVersion = ssl.TLSVersion.TLSv1_3,
    ) -> None:
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        # Pinned to one version in both directions. `TLSv1_2` here is a
        # server the board cannot talk to at all: it offers TLS 1.3 and
        # nothing else, so there is no overlap and this answers with a
        # `protocol_version` alert -- which is what a real TLS 1.2-only
        # server does, and what the board has to report as `tls-version`
        # rather than as a bare refusal.
        self.context.minimum_version = version
        self.context.maximum_version = version
        self.context.load_cert_chain(certfile=str(certificate), keyfile=str(key))
        super().__init__(address, handler)

    def get_request(self):
        connection, address = super().get_request()
        # Wrapping here rather than in the handler keeps the handler
        # identical for both listeners -- the fixtures are the same
        # fixtures, and a second copy of them written against a TLS socket
        # would be a second set of answers to compare against.
        return self.context.wrap_socket(connection, server_side=True), address

    def handle_error(self, request, client_address) -> None:
        # A client that hangs up mid-handshake is an ordinary outcome here:
        # the pin-mismatch fixture is *supposed* to be refused by the board.
        # A traceback for it would drown the log the walk is read from.
        print(f"  (tls handshake failed from {client_address[0]})", flush=True)


# Ports the running server is listening on, for the manifest to name. Filled
# in by `main`; the handler cannot work them out from a connection because
# the extra listeners are not the one it arrived on.
PORTS: dict[str, int] = {}

# Whether the board being walked has a pin for the address it is reaching
# this server on -- that is, whether it was built with `tls-fixture-pins`
# *and* `tools/pins/fixture_pins.txt` names this machine's address.
#
# It changes three expectations, and no server can work it out from a
# connection: pinning is entirely the client's decision and leaves no trace
# on the wire when it succeeds. So it is a flag rather than a guess.
#
# False by default, because that is what an ordinary board is.
# `tls-fixture-pins` is not a default feature, so `cargo build --release` --
# the build `CLAUDE.md` documents and the one anybody runs -- carries no
# pins at all. Defaulting to True meant the ordinary walk reported two
# failures on the Ed25519 and RSA listeners, and the person running it had
# to know to describe their normal build with a flag.
BOARD_HAS_FIXTURE_PINS = False


class FixtureHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        try:
            request = self.read_request()
        except (OSError, ValueError):
            return
        if request is None:
            return
        print(f"{self.client_address[0]} {request.method} {request.target}", flush=True)
        handler = ROUTES.get(request.path)
        if handler is None and request.path.startswith("/redirect/chain/"):
            handler = redirect_chain
        try:
            if handler is None:
                body = page(
                    "not found",
                    f"<h1>404</h1><p>No fixture at {request.path}</p>"
                    '<p><a href="/">index</a></p>',
                )
                self.send_all(html_response(body, 404))
            else:
                handler(self, request)
        except (BrokenPipeError, ConnectionResetError):
            # The board cancelling a load is a normal outcome here, not a
            # server fault: `Escape` during a `/slow` fetch aborts the
            # socket, and the write in flight fails.
            print(f"  (client went away on {request.path})", flush=True)
        finally:
            try:
                self.request.shutdown(socket.SHUT_WR)
            except OSError:
                pass

    def is_tls(self) -> bool:
        return isinstance(self.request, ssl.SSLSocket)

    def origin(self, request: Request, port: int, secure: bool) -> str:
        """`scheme://host:port` for one of this server's own listeners.

        The host comes from the request's `Host` header, so it is the
        address the board actually reached this machine on rather than
        whatever this machine thinks it is called.
        """
        # Everything before the first colon. The browser has no IPv6
        # literals, so a host is a name or a dotted quad and a colon can
        # only be the port -- and taking the *first* one keeps this right
        # even for a client that sent a doubled port.
        host = request.headers.get("host", "").partition(":")[0]
        scheme = "https" if secure else "http"
        return f"{scheme}://{host}:{port}"

    def other_scheme_origin(self, request: Request) -> str:
        """The other listener: the TLS one from plaintext, and back."""
        if self.is_tls():
            return self.origin(request, PORTS["http"], secure=False)
        return self.origin(request, PORTS["tls"], secure=True)

    def tls_manifest_entries(self, request: Request) -> dict[str, str]:
        """The manifest lines that depend on the connection or the build.

        `/redirect/https` swaps direction with the scheme. `/redirect/unpinned`
        is a downgrade only from a connection that had an identity to lose,
        which is a property of the *board's* build as well as of the
        connection -- see `BOARD_HAS_FIXTURE_PINS`.
        """
        entries: dict[str, str] = {}
        if self.is_tls():
            entries["/redirect/https"] = "error:https-downgrade"
            entries["/redirect/unpinned"] = (
                "error:tls-auth-downgrade" if BOARD_HAS_FIXTURE_PINS else "error:dns"
            )
        else:
            entries["/redirect/https"] = "ok"
            entries["/redirect/unpinned"] = "error:dns"
        if "tls" not in PORTS:
            return entries
        alert = self.origin(request, PORTS["tls"] + TLS_ALERT_OFFSET, secure=True)
        ed25519 = self.origin(request, PORTS["tls"] + TLS_ED25519_OFFSET, secure=True)
        rsa = self.origin(request, PORTS["tls"] + TLS_RSA_OFFSET, secure=True)
        tls12 = self.origin(request, PORTS["tls"] + TLS_12_OFFSET, secure=True)
        # Neither of these gets as far as a certificate, so nothing about
        # pinning applies and they read the same from either build.
        entries[f"{alert}/"] = "error:tls-alert"
        entries[f"{tls12}/"] = "error:tls-version"

        if BOARD_HAS_FIXTURE_PINS:
            # A pin belongs to a *host*, not to a host and port. These
            # listeners are on the same address as the pinned one, so a
            # board carrying its pins checks them here too -- and neither of
            # these keys is pinned, so both are refused before their
            # signature algorithm is ever reached.
            #
            # That is worth asserting rather than working around: it is the
            # property that makes a pin worth having. A key that got in by
            # arriving on another port would be a pin that protects one port.
            entries[f"{ed25519}/"] = "error:tls-pin"
            entries[f"{rsa}/"] = "error:tls-pin"
            return entries

        # Without pins the connection gets as far as the signature, which is
        # where these two earn their keep.
        #
        # Ed25519 is in the ClientHello because the library puts it there and
        # will not be talked out of it, and the firmware's verifier does not
        # implement it. What has to happen is an explicit refusal rather than
        # an acceptance -- an unimplemented algorithm that passed would be
        # the worst outcome available.
        entries[f"{ed25519}/"] = "error:tls-cert"
        # RSA-PSS against a server whose key is known, next to the ECDSA
        # P-256 one the main listener uses: both signature schemes the
        # firmware does implement, exercised from the walk rather than only
        # against whatever the public internet happens to serve today.
        entries[f"{rsa}/"] = "ok"
        return entries

    def read_request(self) -> Request | None:
        buffer = b""
        while b"\r\n\r\n" not in buffer:
            data = self.request.recv(4096)
            if not data:
                return None
            buffer += data
            # A TLS ClientHello starts with 0x16, and no HTTP method starts
            # with anything but an uppercase letter. Hanging up on it is
            # what a plaintext server does, and it is what makes
            # `/redirect/https` fail quickly and identically every time
            # instead of waiting for the board's handshake timeout.
            if buffer[:1] and not (b"A" <= buffer[:1] <= b"Z"):
                raise ValueError("not an HTTP request")
            if len(buffer) > 64 * 1024:
                raise ValueError("request head too long")
        head_bytes, body = buffer.split(b"\r\n\r\n", 1)
        head_text = head_bytes.decode("latin-1")
        lines = head_text.split("\r\n")
        fields = lines[0].split(" ")
        if len(fields) < 2:
            raise ValueError("not a request line")
        method, target = fields[0], fields[1]
        path, _, query = target.partition("?")
        headers = {}
        for line in lines[1:]:
            name, separator, value = line.partition(":")
            if separator:
                headers[name.strip().lower()] = value.strip()
        length_text = headers.get("content-length", "0")
        if not length_text.isascii() or not length_text.isdigit():
            raise ValueError("invalid content length")
        content_length = int(length_text)
        if content_length > MAX_ENCODED_REQUEST_BYTES:
            raise ValueError("request body too long")
        while len(body) < content_length:
            data = self.request.recv(min(4096, content_length - len(body)))
            if not data:
                raise ValueError("truncated request body")
            body += data
        return Request(method, target, path, query, headers, body[:content_length])

    def send_all(self, data: bytes) -> None:
        self.request.sendall(data)

    def hard_close(self) -> None:
        """Closes without a FIN, the way a dropped link looks.

        `SO_LINGER` with a zero timeout makes the close send RST. A clean
        FIN would be indistinguishable from a close-delimited response that
        simply ended, which is the opposite of what these fixtures are for.
        """
        self.request.setsockopt(
            socket.SOL_SOCKET, socket.SO_LINGER, b"\x01\x00\x00\x00\x00\x00\x00\x00"
        )
        self.request.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8080)
    parser.add_argument(
        "--tls-port",
        type=int,
        default=8443,
        help="port for the TLS listener, or 0 to serve plaintext only",
    )
    parser.add_argument(
        "--pinned-board",
        action="store_true",
        help="the board being walked was built with `tls-fixture-pins` and "
        "this machine's address is in tools/pins/fixture_pins.txt. changes "
        "what /manifest.txt expects of /redirect/unpinned and of the Ed25519 "
        "and RSA listeners, which such a board refuses on the pin before it "
        "reaches their signatures. off by default: `tls-fixture-pins` is not "
        "a default feature, so an ordinary build has no pins",
    )
    parser.add_argument(
        "--tls-key-name",
        default="current",
        choices=["current", "next", "other"],
        help="which key in tools/tls/ the TLS listener presents. `current` "
        "and `next` are both pinned by tools/pins/fixture_pins.txt; `other` "
        "is not, and a board built with those pins must refuse it",
    )
    parser.add_argument(
        "--check-limits",
        action="store_true",
        help="compare this file's limits against browser/src/limits.rs and exit",
    )
    parser.add_argument(
        "--dump",
        metavar="DIR",
        help="write the parsable fixtures into DIR and exit "
        "(browser/tests/fixtures)",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="print the endpoint manifest and exit",
    )
    arguments = parser.parse_args()

    if arguments.check_limits:
        return check_limits()
    if arguments.dump:
        return dump(arguments.dump)
    if arguments.list:
        for path, outcome in sorted(set(MANIFEST)):
            print(f"{path}\t{outcome}")
        print(
            "\n(as served over plaintext. /redirect/https and /redirect/unpinned\n"
            " change over TLS, and the TLS failure fixtures are added as absolute\n"
            " URLs once the listeners have ports -- see tls_manifest_entries)"
        )
        return 0

    if check_limits():
        return 1

    server = FixtureServer((arguments.host, arguments.port), FixtureHandler)
    print(f"serving fixtures on http://{arguments.host}:{arguments.port}/", flush=True)
    print(f"{len(set(MANIFEST))} endpoints; /manifest.txt lists them", flush=True)

    global BOARD_HAS_FIXTURE_PINS
    BOARD_HAS_FIXTURE_PINS = arguments.pinned_board
    # Said out loud, because it is an assumption about the *other* machine
    # that nothing here can check. A walk that fails only on the Ed25519
    # and RSA listeners is a walk against a board whose pins are not what
    # this line says.
    print(
        "board assumed to be built "
        + (
            "WITH tls-fixture-pins (--pinned-board)"
            if BOARD_HAS_FIXTURE_PINS
            else "without tls-fixture-pins; pass --pinned-board if it has them"
        ),
        flush=True,
    )
    PORTS["http"] = arguments.port
    extra: list[socketserver.BaseServer] = []
    if arguments.tls_port:
        PORTS["tls"] = arguments.tls_port
        keys = Path(__file__).resolve().parent / "tls"

        def start(server: socketserver.BaseServer, description: str) -> None:
            threading.Thread(target=server.serve_forever, daemon=True).start()
            extra.append(server)
            print(f"  {description}", flush=True)

        def tls_listener(
            port: int,
            name: str,
            description: str,
            version: ssl.TLSVersion = ssl.TLSVersion.TLSv1_3,
        ) -> bool:
            certificate = keys / f"fixture-{name}.crt"
            key = keys / f"fixture-{name}.key"
            if not certificate.exists():
                print(f"no such fixture key: {certificate}", file=sys.stderr)
                return False
            start(
                TlsFixtureServer(
                    (arguments.host, port), FixtureHandler, certificate, key, version
                ),
                f"https://{arguments.host}:{port}/  {description}",
            )
            return True

        print("TLS listeners:", flush=True)
        if not tls_listener(
            arguments.tls_port,
            arguments.tls_key_name,
            f"the fixtures, ECDSA P-256, `{arguments.tls_key_name}` key",
        ):
            return 1
        start(
            AlertServer(
                (arguments.host, arguments.tls_port + TLS_ALERT_OFFSET), AlertHandler
            ),
            f"https://{arguments.host}:{arguments.tls_port + TLS_ALERT_OFFSET}/"
            "  a fatal access_denied alert",
        )
        if not tls_listener(
            arguments.tls_port + TLS_ED25519_OFFSET,
            "ed25519",
            "an Ed25519 certificate, which the firmware does not verify",
        ):
            return 1
        if not tls_listener(
            arguments.tls_port + TLS_RSA_OFFSET, "rsa", "an RSA-PSS certificate"
        ):
            return 1
        if not tls_listener(
            arguments.tls_port + TLS_12_OFFSET,
            arguments.tls_key_name,
            "TLS 1.2 only, which the board cannot speak",
            ssl.TLSVersion.TLSv1_2,
        ):
            return 1

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nstopping", flush=True)
    finally:
        server.server_close()
        for other in extra:
            other.shutdown()
            other.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
