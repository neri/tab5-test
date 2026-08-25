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
import re
import socket
import socketserver
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

# --------------------------------------------------------------------------
# Limits, mirrored from browser/src/limits.rs.
# --------------------------------------------------------------------------

MAX_HEADER_BYTES = 4096
MAX_URL_BYTES = 2048
MAX_REDIRECTS = 5
MAX_HISTORY = 8
MAX_DECODED_HTML_BYTES = 2 * 1024 * 1024
MAX_TEXT_BYTES = 1024 * 1024
MAX_ITEMS = 8192
MAX_LINKS = 1024
MAX_NESTING_DEPTH = 32
MAX_ATTRIBUTES_PER_ELEMENT = 16
MAX_LAYOUT_LINES = 32768
MAX_BROWSER_OWNED_BYTES = 4 * 1024 * 1024

LIMITS = {
    "MAX_HEADER_BYTES": MAX_HEADER_BYTES,
    "MAX_URL_BYTES": MAX_URL_BYTES,
    "MAX_REDIRECTS": MAX_REDIRECTS,
    "MAX_HISTORY": MAX_HISTORY,
    "MAX_DECODED_HTML_BYTES": MAX_DECODED_HTML_BYTES,
    "MAX_TEXT_BYTES": MAX_TEXT_BYTES,
    "MAX_ITEMS": MAX_ITEMS,
    "MAX_LINKS": MAX_LINKS,
    "MAX_NESTING_DEPTH": MAX_NESTING_DEPTH,
    "MAX_ATTRIBUTES_PER_ELEMENT": MAX_ATTRIBUTES_PER_ELEMENT,
    "MAX_LAYOUT_LINES": MAX_LAYOUT_LINES,
    "MAX_BROWSER_OWNED_BYTES": MAX_BROWSER_OWNED_BYTES,
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
    pattern = re.compile(r"pub const (\w+): usize = ([0-9_ *]+);")
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
<h2>Malformed markup that should still render</h2>
<ul>
  <li><a href="broken/unclosed.html">broken/unclosed.html</a></li>
  <li><a href="broken/deep-nest.html">broken/deep-nest.html</a></li>
  <li><a href="broken/huge-attribute.html">broken/huge-attribute.html</a></li>
  <li><a href="broken/bad-utf8.html">broken/bad-utf8.html</a></li>
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
  <li><a href="limit/links">limit/links</a> - past MAX_LINKS</li>
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


def html_response(body: bytes, status: int = 200) -> bytes:
    return (
        head(
            status,
            [
                ("Content-Type", "text/html; charset=utf-8"),
                ("Content-Length", str(len(body))),
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


@route("/redirect/https", "error:https")
def redirect_https(self: "FixtureHandler", request: Request) -> None:
    self.send_all(redirect("https://example.invalid/secure", 302))


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


@route("/limit/links", "error:link-limit")
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
    across runs of this server: `httpget` saves the file and the board's
    `fsverify` checksums it, and a target that moved every restart would
    make that comparison meaningless.
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


@route("/manifest.txt", "text")
def manifest(self: "FixtureHandler", request: Request) -> None:
    lines = "".join(f"{path}\t{outcome}\n" for path, outcome in sorted(set(MANIFEST)))
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

    def read_request(self) -> Request | None:
        buffer = b""
        while b"\r\n\r\n" not in buffer:
            data = self.request.recv(4096)
            if not data:
                return None
            buffer += data
            if len(buffer) > 64 * 1024:
                raise ValueError("request head too long")
        head_text = buffer.split(b"\r\n\r\n", 1)[0].decode("latin-1")
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
        return Request(method, target, path, query, headers)

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
        return 0

    if check_limits():
        return 1

    server = FixtureServer((arguments.host, arguments.port), FixtureHandler)
    print(f"serving fixtures on http://{arguments.host}:{arguments.port}/", flush=True)
    print(f"{len(set(MANIFEST))} endpoints; /manifest.txt lists them", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nstopping", flush=True)
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
