//! `httpstream` -- the diagnostic that drives [`net::http::Transaction`]
//! directly, without a browser screen in the way.
//!
//! Stage 2 of `docs/WEB_BROWSER_PLAN.md` needs a way to prove that the
//! interruptible transaction reaches the right end for each of the fixture
//! server's responses, and to prove it a hundred times in a row without a
//! socket or an allocation going missing. A full-screen viewer is the wrong
//! instrument for that: it renders, it needs a pointer, and its output is
//! pixels. This is the same fetch reported as numbers.
//!
//! What it checks that a blocking `httpget` cannot:
//!
//! - the head is reported on its own, before any body byte, so a caller can
//!   stop on a redirect or a status it will not render
//! - `Content-Length`, `chunked` and close-delimited bodies each end where
//!   they should, and a body that stops early is [`Error::Truncated`]
//!   rather than a short page
//! - a cancelled or failed transfer gives its socket back, every time
//!
//! It takes a URL rather than a host and a path, which also puts
//! `browser::url` on the wire for the first time: the host that gets
//! connected to, the `Host:` header and the request target all come out of
//! one [`Url`], the same as they will in the viewer.
//!
//! `bt` -- [`walk`] -- is the other half, and the Stage 6 acceptance check.
//! It reads `/manifest.txt` off the fixture server, drives every endpoint
//! in it through the same `app::fetch::Fetch` the viewer uses, and compares
//! what came back with what the server says should have. Driving the
//! viewer's own fetch rather than a copy of it is the whole point: a walk
//! with its own redirect and status rules would be checking the wrong
//! code.

use alloc::string::String;
use alloc::vec::Vec;

use crate::browser::document::{Parser, Stats as DocumentStats};
use crate::browser::error;
use crate::browser::url::{self, Url};
use crate::console::Console;
use crate::framebuffer::Framebuffer;
use crate::net::http::{self, Error, Progress, Transaction};
use crate::net::tftp::Crc32;
use crate::{net, tick, uart, wifi};

use super::fetch::{Fetch, Network, Outcome as FetchOutcome};
use super::shell::{Line, resolve_target};

/// Body bytes one fetch will accept.
///
/// The browser's page bound, so `/limit/input` fails here for the same
/// reason and at the same size it will fail in the viewer.
const MAX_BODY: u64 = crate::browser::limits::MAX_DECODED_HTML_BYTES as u64;

/// How long one fetch may take in total, cancelled rather than left to the
/// per-read idle timeout.
///
/// `/slow` writes a byte every 2 ms and is a few hundred bytes, so it
/// finishes well inside this; a fixture that does not is a fixture that
/// hung, and a diagnostic that hangs with it is no use.
const FETCH_DEADLINE_MS: u64 = 30_000;

/// Repetitions the leak checks stop at, so a mistyped count cannot run for
/// an hour.
const MAX_REPEAT: u32 = 1000;

/// `hs <url> [r <n>|p [n]|c <n>]`
///
/// The address arrives already parsed: the shell supplies a missing scheme
/// and parses it before the link is brought up, so a typo is reported
/// without starting the C6.
pub fn run(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    url: &Url,
    rest: &[u8],
    rpc: &mut wifi::Rpc,
    stack: &mut net::Stack,
) {
    let (mode, count) = match parse_mode(rest) {
        Some(parsed) => parsed,
        None => {
            usage(console, framebuffer);
            return;
        }
    };

    // The address the URL says, resolved once: repeating the fetch is
    // meant to exercise sockets, not the resolver.
    let Some(address) = resolve_target(console, framebuffer, rpc, stack, url.host().as_bytes())
    else {
        return;
    };
    // Shown once, so that a short `hs /simple.html` still says what was
    // actually fetched -- the whole point of one `Url` producing both the
    // display text and the request.
    if let Ok(text) = url.to_text() {
        let mut line = Line::new();
        line.push_str("GET ");
        line.push_str(text.as_str());
        console.write_output_line(framebuffer, line.as_str());
    }

    let sockets_before = socket_count(stack);
    let heap_before = crate::heap_used();
    let mut failures = 0u32;

    for index in 0..count {
        let outcome = match mode {
            Mode::Fetch => fetch(stack, rpc, url, address, false),
            Mode::Parse => fetch(stack, rpc, url, address, true),
            Mode::Cancel => cancel_immediately(stack, rpc, url, address),
        };
        // Every round goes to the UART; only the first and the last reach
        // the console, because a hundred lines would scroll the summary --
        // which is the part worth reading -- straight off the screen.
        log_outcome(index, &outcome);
        if outcome.error.is_some() && mode != Mode::Cancel {
            failures += 1;
        }
        if index == 0 || index + 1 == count {
            write_outcome(console, framebuffer, &outcome);
        }
    }

    let sockets_after = socket_count(stack);
    let heap_after = crate::heap_used();
    let mut line = Line::new();
    line.push_str("sockets ");
    line.push_u32(sockets_before as u32);
    line.push_str(" -> ");
    line.push_u32(sockets_after as u32);
    line.push_str(", heap ");
    line.push_u32(heap_before as u32);
    line.push_str(" -> ");
    line.push_u32(heap_after as u32);
    console.write_output_line(framebuffer, line.as_str());

    if count > 1 {
        let mut line = Line::new();
        line.push_u32(count);
        line.push_str(" rounds, ");
        line.push_u32(failures);
        line.push_str(" failed");
        console.write_output_line(framebuffer, line.as_str());
    }
    // A socket left behind is the failure this command exists to catch, and
    // it is invisible until the set runs out -- so it is called out rather
    // than left as two numbers to compare.
    if sockets_after != sockets_before {
        console.write_output_line(
            framebuffer,
            "SOCKET LEAK: a transaction did not give its handle back",
        );
    }
}

fn usage(console: &mut Console, framebuffer: &mut Framebuffer) {
    console.write_output_line(framebuffer, "usage: hs <url> [r <n>|p [n]|c <n>]");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Fetch to the end.
    Fetch,
    /// Fetch, and hand the body straight to the document parser as it
    /// arrives -- the whole chain from socket to document, without a
    /// screen. What Stage 5 will do, reported as numbers.
    Parse,
    /// Start, poll once, and cancel -- the socket-return check.
    Cancel,
}

fn parse_mode(rest: &[u8]) -> Option<(Mode, u32)> {
    let (word, tail) = split_word(trim(rest));
    // One-letter forms first, because they are what gets typed: the long
    // spellings are kept so an old note or a log line still runs.
    match word {
        b"" => Some((Mode::Fetch, 1)),
        b"r" | b"repeat" => parse_count(tail).map(|count| (Mode::Fetch, count)),
        b"c" | b"cancel" => parse_count(tail).map(|count| (Mode::Cancel, count)),
        b"p" | b"parse" => match trim(tail) {
            b"" => Some((Mode::Parse, 1)),
            tail => parse_count(tail).map(|count| (Mode::Parse, count)),
        },
        _ => None,
    }
}

fn parse_count(text: &[u8]) -> Option<u32> {
    let text = trim(text);
    if text.is_empty() || !text.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut value: u32 = 0;
    for &byte in text {
        value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
    }
    if value == 0 || value > MAX_REPEAT {
        return None;
    }
    Some(value)
}

/// What one round came to.
struct Outcome {
    status: Option<u16>,
    framing: &'static str,
    location: Option<Vec<u8>>,
    body_bytes: u64,
    received: usize,
    crc: u32,
    elapsed_ms: u64,
    polls: u32,
    /// Polls that reported [`Progress::Idle`] -- the transfer waiting on
    /// the network, which is exactly the time a browser screen would have
    /// spent on its own input.
    idle_polls: u32,
    /// Bytes received by the time the head was complete, which is how much
    /// of the transfer happened before a caller had its first chance to
    /// stop. A response whose whole body arrived inside the same read still
    /// reports the head on its own, so this can be larger than the head.
    head_bytes: usize,
    /// What the viewer would do with this response, decided from the head
    /// alone: render it, follow it, or refuse it.
    disposition: &'static str,
    error: Option<Error>,
    /// Set in `parse` mode: what the body became.
    document: Option<DocumentOutcome>,
}

/// What the document parser made of the body.
struct DocumentOutcome {
    title: [u8; 48],
    title_len: usize,
    blocks: usize,
    runs: usize,
    stats: DocumentStats,
    /// The first characters of the page's text, so a glance at the console
    /// says whether the right thing was parsed.
    head: [u8; 64],
    head_len: usize,
    error: Option<error::Error>,
}

/// The transport a URL's scheme calls for.
///
/// Taken from the `Url` rather than from a flag, at every site that opens a
/// connection: an `https://` address handed to the plaintext path would
/// connect in the clear to port 443, which is the downgrade this whole
/// layering exists to make unwriteable. There is deliberately no way to ask
/// for one with the other's scheme.
fn security_for(url: &Url) -> net::transport::Security<'_> {
    match url.scheme() {
        crate::browser::url::Scheme::Http => net::transport::Security::Plain,
        crate::browser::url::Scheme::Https => net::transport::Security::Tls {
            server_name: url.host(),
            policy: net::pins::policy_for(url.host()),
        },
    }
}

/// One fetch, polled to the end with a bounded budget each time.
///
/// The loop here stands in for the browser's frame loop: it is what proves
/// the transaction actually returns between reads rather than blocking
/// inside one. The `polls` and `idle_polls` counters are how that shows up
/// in the output -- a transfer that blocked would report one poll.
fn fetch(
    stack: &mut net::Stack,
    rpc: &mut wifi::Rpc,
    url: &Url,
    address: smoltcp::wire::Ipv4Address,
    build_document: bool,
) -> Outcome {
    let (Ok(target), Ok(host)) = (url.request_target(), url.host_header()) else {
        return failed(Error::OutOfMemory);
    };
    let mut transaction = match Transaction::start(
        stack,
        address,
        url.port(),
        host.as_bytes(),
        target.as_bytes(),
        MAX_BODY,
        security_for(url),
    ) {
        Ok(transaction) => transaction,
        Err(error) => return failed(error),
    };

    let mut parser = if build_document {
        // The page's own URL is the base every relative link resolves
        // against, so the parser is given the same `Url` the request was
        // built from rather than a second one parsed from the same text.
        Parser::new(url.clone()).ok()
    } else {
        None
    };
    let mut crc = Crc32::new();
    let mut document_error = None;
    let mut sink = |bytes: &[u8]| {
        crc.update(bytes);
        if let Some(parser) = parser.as_mut()
            && let Err(failure) = parser.feed(bytes)
        {
            // Refusing the body here is what stops the transfer: a page
            // past a limit is not read to the end and then discarded.
            document_error = Some(failure);
            return false;
        }
        true
    };
    let mut status = None;
    let mut framing = "?";
    let mut location = None;
    let mut error = None;
    let mut idle_polls = 0u32;
    let mut head_bytes = 0usize;
    let mut disposition = "-";
    let deadline = tick::now_ms() + FETCH_DEADLINE_MS;
    loop {
        match transaction.poll(stack, rpc, http::DEFAULT_POLL_BUDGET, &mut sink) {
            Progress::HeadReady => {
                if let Some(head) = transaction.head() {
                    status = head.status;
                    framing = if head.chunked {
                        "chunked"
                    } else if head.content_length.is_some() {
                        "length"
                    } else {
                        "close"
                    };
                    location = head.location.clone();
                    disposition = disposition_of(head);
                }
                head_bytes = transaction.stats().received;
            }
            Progress::Idle => idle_polls += 1,
            Progress::Complete => break,
            Progress::Failed(failure) => {
                error = Some(failure);
                break;
            }
            Progress::Connecting | Progress::Body => {}
        }
        if tick::now_ms() > deadline {
            transaction.cancel();
            error = Some(Error::TimedOut);
            break;
        }
    }

    // A response refused for its framing -- compressed, or longer than the
    // limit -- never reported `HeadReady`, but its head was parsed and is
    // worth showing: "status 200, encoding" says what happened, where "no
    // status, encoding" says almost nothing.
    if status.is_none()
        && let Some(head) = transaction.head()
    {
        status = head.status;
        location = head.location.clone();
        disposition = disposition_of(head);
    }
    let stats = transaction.close(stack, rpc);
    let document = parser.map(|parser| finish_document(parser, document_error));
    Outcome {
        status,
        framing,
        location,
        body_bytes: stats.body_bytes,
        received: stats.received,
        crc: crc.finish(),
        elapsed_ms: stats.elapsed_ms,
        polls: stats.polls,
        idle_polls,
        head_bytes,
        disposition,
        error,
        document,
    }
}

/// Turns the parser into something reportable.
///
/// A parse that hit a limit has no document at all -- `finish` is the only
/// way to get one and the error path never reaches it -- so the counts come
/// back as zero with the reason beside them. That is the behaviour being
/// checked: a page past a bound must not arrive as a shorter page.
fn finish_document(parser: Parser, failure: Option<error::Error>) -> DocumentOutcome {
    let mut outcome = DocumentOutcome {
        title: [0; 48],
        title_len: 0,
        blocks: 0,
        runs: 0,
        stats: DocumentStats::default(),
        head: [0; 64],
        head_len: 0,
        error: failure,
    };
    if failure.is_some() {
        return outcome;
    }
    match parser.finish() {
        Ok(document) => {
            outcome.title_len = copy_ascii(&mut outcome.title, document.title());
            outcome.head_len = copy_ascii(&mut outcome.head, document.text());
            outcome.blocks = document.blocks().len();
            outcome.runs = document.runs().len();
            outcome.stats = document.stats();
        }
        Err(failure) => outcome.error = Some(failure),
    }
    outcome
}

/// Copies the start of `text` into a fixed buffer, one byte per character
/// and `.` for anything outside printable ASCII.
///
/// Non-ASCII collapses to a single `.` rather than to its UTF-8 bytes, so
/// the count on screen matches the characters rather than the encoding.
fn copy_ascii(target: &mut [u8], text: &str) -> usize {
    let mut length = 0;
    for character in text.chars() {
        if length >= target.len() {
            break;
        }
        target[length] = match character {
            ' '..='~' => character as u8,
            _ => b'.',
        };
        length += 1;
    }
    length
}

/// What the viewer would do with a response, from its head alone.
///
/// This is the decision `Progress::HeadReady` exists for, made here so the
/// diagnostic reports the same judgement the viewer will: a redirect is
/// followed without its body ever being read, and a status outside 2xx
/// becomes the browser's own error page rather than whatever the server
/// sent to fill the screen.
fn disposition_of(head: &http::Head) -> &'static str {
    if head.is_redirect() {
        "redirect"
    } else if !head.is_success() {
        "error-page"
    } else if head.is_html() {
        "render"
    } else {
        "not-html"
    }
}

/// Starts a transfer and abandons it, which is what Escape does.
///
/// One poll first, so the socket has actually been given to the stack and
/// the connect is under way: cancelling before that would test nothing but
/// the constructor.
fn cancel_immediately(
    stack: &mut net::Stack,
    rpc: &mut wifi::Rpc,
    url: &Url,
    address: smoltcp::wire::Ipv4Address,
) -> Outcome {
    let (Ok(target), Ok(host)) = (url.request_target(), url.host_header()) else {
        return failed(Error::OutOfMemory);
    };
    let mut transaction = match Transaction::start(
        stack,
        address,
        url.port(),
        host.as_bytes(),
        target.as_bytes(),
        MAX_BODY,
        security_for(url),
    ) {
        Ok(transaction) => transaction,
        Err(error) => return failed(error),
    };
    let mut discard = |_: &[u8]| true;
    let _ = transaction.poll(stack, rpc, http::DEFAULT_POLL_BUDGET, &mut discard);
    transaction.cancel();
    let error = transaction.error();
    let stats = transaction.close(stack, rpc);
    Outcome {
        status: None,
        framing: "cancelled",
        location: None,
        body_bytes: stats.body_bytes,
        received: stats.received,
        crc: 0,
        elapsed_ms: stats.elapsed_ms,
        polls: stats.polls,
        idle_polls: 0,
        head_bytes: 0,
        disposition: "-",
        error,
        document: None,
    }
}

fn failed(error: Error) -> Outcome {
    Outcome {
        status: None,
        framing: "-",
        location: None,
        body_bytes: 0,
        received: 0,
        crc: 0,
        elapsed_ms: 0,
        polls: 0,
        idle_polls: 0,
        head_bytes: 0,
        disposition: "-",
        error: Some(error),
        document: None,
    }
}

fn write_outcome(console: &mut Console, framebuffer: &mut Framebuffer, outcome: &Outcome) {
    let mut line = Line::new();
    match outcome.status {
        Some(status) => {
            line.push_str("status ");
            line.push_u32(status as u32);
        }
        None => line.push_str("no status"),
    }
    line.push_str(" ");
    line.push_str(outcome.framing);
    line.push_str(" ");
    line.push_str(outcome.disposition);
    line.push_str(" body=");
    line.push_u64(outcome.body_bytes);
    line.push_str(" crc=");
    line.push_hex(outcome.crc, 8);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("recv=");
    line.push_u32(outcome.received as u32);
    line.push_str(" ");
    line.push_u32(outcome.elapsed_ms as u32);
    line.push_str(" ms polls=");
    line.push_u32(outcome.polls);
    line.push_str(" idle=");
    line.push_u32(outcome.idle_polls);
    line.push_str(" head@");
    line.push_u32(outcome.head_bytes as u32);
    console.write_output_line(framebuffer, line.as_str());

    if let Some(location) = &outcome.location {
        let mut line = Line::new();
        line.push_str("location: ");
        line.push_ascii(location);
        console.write_output_line(framebuffer, line.as_str());
    }
    if let Some(error) = outcome.error {
        let mut line = Line::new();
        line.push_str("failed: ");
        line.push_str(http::error_text(error));
        console.write_output_line(framebuffer, line.as_str());
    }
    if let Some(document) = &outcome.document {
        write_document(console, framebuffer, document);
    }
}

fn write_document(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    document: &DocumentOutcome,
) {
    if let Some(error) = document.error {
        let mut line = Line::new();
        line.push_str("parse failed: ");
        line.push_str(error::error_text(error));
        console.write_output_line(framebuffer, line.as_str());
        return;
    }
    let mut line = Line::new();
    line.push_str("doc \"");
    line.push_ascii(&document.title[..document.title_len]);
    line.push_str("\" blocks=");
    line.push_u32(document.blocks as u32);
    line.push_str(" runs=");
    line.push_u32(document.runs as u32);
    line.push_str(" links=");
    line.push_u32(document.stats.links as u32);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("text=");
    line.push_u32(document.stats.text_bytes as u32);
    line.push_str(" items=");
    line.push_u32(document.stats.items as u32);
    line.push_str(" owned=");
    line.push_u32(document.stats.owned_bytes as u32);
    line.push_str(" token=");
    line.push_u32(document.stats.longest_token as u32);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("first: ");
    line.push_ascii(&document.head[..document.head_len]);
    console.write_output_line(framebuffer, line.as_str());
}

/// One line per round on the UART, in the shape a log can be scanned or
/// diffed: the fields are always in the same order and always present.
///
/// Built into one buffer and written once, rather than through
/// `uart::log_hex` per field -- that helper ends every value with a
/// newline, which would turn each round into a dozen lines and lose the
/// property this is for.
fn log_outcome(index: u32, outcome: &Outcome) {
    let mut line = LogLine::new();
    line.text("HTTPSTREAM round=");
    line.decimal(index);
    line.text(" status=");
    match outcome.status {
        Some(status) => line.decimal(status as u32),
        None => line.text("none"),
    }
    line.text(" framing=");
    line.text(outcome.framing);
    line.text(" disposition=");
    line.text(outcome.disposition);
    line.text(" body=");
    line.decimal(outcome.body_bytes as u32);
    line.text(" recv=");
    line.decimal(outcome.received as u32);
    line.text(" head@");
    line.decimal(outcome.head_bytes as u32);
    line.text(" crc=");
    line.hex(outcome.crc);
    line.text(" ms=");
    line.decimal(outcome.elapsed_ms as u32);
    line.text(" polls=");
    line.decimal(outcome.polls);
    line.text(" idle=");
    line.decimal(outcome.idle_polls);
    line.text(" error=");
    match outcome.error {
        Some(error) => line.text(http::error_name(error)),
        None => line.text("none"),
    }
    if let Some(document) = &outcome.document {
        line.text(" blocks=");
        line.decimal(document.blocks as u32);
        line.text(" runs=");
        line.decimal(document.runs as u32);
        line.text(" links=");
        line.decimal(document.stats.links as u32);
        line.text(" text=");
        line.decimal(document.stats.text_bytes as u32);
        line.text(" owned=");
        line.decimal(document.stats.owned_bytes as u32);
        line.text(" parse=");
        match document.error {
            Some(failure) => line.text(error::error_name(failure)),
            None => line.text("none"),
        }
    }
    line.finish();
}

/// A fixed-size line assembled for one `uart::log` call.
///
/// `shell::Line` is the same idea for the console, but its 80 bytes are the
/// console's width and one of these rounds does not fit in them. Anything
/// past the buffer is dropped rather than wrapped: a truncated diagnostic
/// line is obvious, a wrapped one silently stops being one line per round.
struct LogLine {
    buffer: [u8; 192],
    len: usize,
}

impl LogLine {
    fn new() -> Self {
        Self {
            buffer: [0; 192],
            len: 0,
        }
    }

    fn byte(&mut self, value: u8) {
        if self.len < self.buffer.len() {
            self.buffer[self.len] = value;
            self.len += 1;
        }
    }

    fn text(&mut self, value: &str) {
        for &byte in value.as_bytes() {
            self.byte(byte);
        }
    }

    fn decimal(&mut self, value: u32) {
        let mut digits = [0u8; 10];
        let mut count = 0;
        let mut remaining = value;
        loop {
            digits[count] = b'0' + (remaining % 10) as u8;
            count += 1;
            remaining /= 10;
            if remaining == 0 {
                break;
            }
        }
        for index in (0..count).rev() {
            self.byte(digits[index]);
        }
    }

    fn hex(&mut self, value: u32) {
        // Upper case, so a CRC read off the UART and one read off the
        // console are the same string.
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for shift in (0..8).rev() {
            self.byte(HEX[((value >> (shift * 4)) & 0xF) as usize]);
        }
    }

    fn finish(&mut self) {
        self.text("\r\n");
        uart::log(&self.buffer[..self.len]);
    }
}

fn socket_count(stack: &mut net::Stack) -> usize {
    stack.sockets_mut().iter().count()
}

fn trim(text: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = text.len();
    while start < end && text[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && text[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &text[start..end]
}

fn split_word(text: &[u8]) -> (&[u8], &[u8]) {
    match text.iter().position(|byte| byte.is_ascii_whitespace()) {
        Some(index) => (&text[..index], &text[index + 1..]),
        None => (text, b""),
    }
}

// ---------------------------------------------------------------------------
// `bt` -- the fixture walk.
// ---------------------------------------------------------------------------

/// How long one endpoint may take before the walk gives up on it.
///
/// `/limit/input` is two megabytes over Wi-Fi and `/slow` writes a byte
/// every two milliseconds, so this is generous. An endpoint that reaches it
/// has hung, and a walk that hangs with it tells nobody anything.
const ENDPOINT_DEADLINE_MS: u64 = 60_000;

/// The manifest is small and this bounds it anyway, because it arrives off
/// the network like everything else.
const MAX_MANIFEST_BYTES: usize = 16 * 1024;

/// Walks every endpoint the fixture server lists.
///
/// `base` is the manifest's own address -- the one word `bt` is given --
/// and every path in the manifest is resolved against it, so the whole walk
/// follows from that single address.
pub fn walk(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    base: &Url,
    rounds: u32,
    rpc: &mut wifi::Rpc,
    stack: &mut net::Stack,
) {
    let Ok(manifest_url) = base.resolve("/manifest.txt") else {
        console.write_output_line(framebuffer, "bt: cannot build the manifest address");
        return;
    };
    let Some(address) = resolve_target(
        console,
        framebuffer,
        rpc,
        stack,
        manifest_url.host().as_bytes(),
    ) else {
        return;
    };

    let manifest = match load_manifest(&manifest_url, address, rpc, stack) {
        Ok(manifest) => manifest,
        Err(reason) => {
            let mut line = Line::new();
            line.push_str("bt: cannot read the manifest: ");
            line.push_str(reason);
            console.write_output_line(framebuffer, line.as_str());
            return;
        }
    };

    let entries = url::split_manifest(&manifest);
    let mut line = Line::new();
    line.push_str("bt: ");
    line.push_u32(entries.len() as u32);
    line.push_str(" endpoints, ");
    line.push_u32(rounds);
    line.push_str(" round(s)");
    console.write_output_line(framebuffer, line.as_str());

    let sockets_before = socket_count(stack);
    let heap_before = crate::heap_used();
    let dropped_before = rpc.dropped_data_frames();
    let deconfigured_before = stack.deconfigured_count();
    let _ = crate::lcd::take_underrun();
    let underruns_before = crate::lcd::underrun_count();

    let mut totals = Totals::default();
    for round in 0..rounds {
        for entry in &entries {
            let Some((path, expectation)) = split_entry(entry) else {
                continue;
            };
            let Some(expected) = Expectation::parse(expectation) else {
                // `download:` and `text` endpoints are not pages; they are
                // the `httpget` regression's business.
                totals.skipped += 1;
                continue;
            };
            let Ok(target) = base.resolve(path) else {
                totals.failed += 1;
                report_bad_address(console, framebuffer, path);
                continue;
            };
            let result = visit(&target, expected, rpc, stack);
            log_visit(round, path, expectation, &result);
            totals.record(&result);
            if !result.matched {
                report_mismatch(console, framebuffer, path, expectation, &result);
            }
        }
    }

    let sockets_after = socket_count(stack);
    let heap_after = crate::heap_used();
    write_totals(console, framebuffer, &totals);

    let mut line = Line::new();
    line.push_str("sockets ");
    line.push_u32(sockets_before as u32);
    line.push_str(" -> ");
    line.push_u32(sockets_after as u32);
    line.push_str(", heap ");
    line.push_u32(heap_before as u32);
    line.push_str(" -> ");
    line.push_u32(heap_after as u32);
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("dropped +");
    line.push_u32(rpc.dropped_data_frames().wrapping_sub(dropped_before));
    line.push_str(", deconfigured +");
    line.push_u32(stack.deconfigured_count().wrapping_sub(deconfigured_before));
    line.push_str(", underruns +");
    line.push_u32(crate::lcd::underrun_count().wrapping_sub(underruns_before));
    console.write_output_line(framebuffer, line.as_str());

    if sockets_after != sockets_before {
        console.write_output_line(
            framebuffer,
            "SOCKET LEAK: a fetch did not give its handle back",
        );
    }
}

/// What the manifest says an endpoint should do.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expectation {
    /// Becomes a page.
    Page,
    /// Fails with exactly this reason.
    Failure(ManifestName),
}

/// A one-word failure name copied out of the manifest.
///
/// Fixed-size rather than a `String`: the walk compares thousands of these
/// across a hundred rounds, and none of them is longer than this.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ManifestName {
    bytes: [u8; 24],
    length: usize,
}

impl ManifestName {
    fn new(text: &str) -> ManifestName {
        let mut name = ManifestName {
            bytes: [0; 24],
            length: 0,
        };
        for &byte in text.as_bytes() {
            if name.length < name.bytes.len() {
                name.bytes[name.length] = byte;
                name.length += 1;
            }
        }
        name
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("")
    }
}

impl Expectation {
    fn parse(text: &str) -> Option<Expectation> {
        if text == "ok" {
            return Some(Expectation::Page);
        }
        text.strip_prefix("error:")
            .map(|name| Expectation::Failure(ManifestName::new(name)))
    }
}

/// What one endpoint actually did.
struct Visit {
    matched: bool,
    /// The one-word reason, or `ok`.
    got: ManifestName,
    crc: u32,
    blocks: usize,
    links: usize,
    text_bytes: usize,
    peak_owned: usize,
    received: usize,
    redirects: usize,
    elapsed_ms: u64,
}

#[derive(Default)]
struct Totals {
    passed: u32,
    failed: u32,
    skipped: u32,
    peak_owned: usize,
    slowest_ms: u64,
}

impl Totals {
    fn record(&mut self, visit: &Visit) {
        if visit.matched {
            self.passed += 1;
        } else {
            self.failed += 1;
        }
        self.peak_owned = self.peak_owned.max(visit.peak_owned);
        self.slowest_ms = self.slowest_ms.max(visit.elapsed_ms);
    }
}

/// Fetches one endpoint and judges it.
fn visit(
    target: &Url,
    expected: Expectation,
    rpc: &mut wifi::Rpc,
    stack: &mut net::Stack,
) -> Visit {
    let mut network = Network { rpc, stack };
    let started = tick::now_ms();
    let mut fetch = match Fetch::start(target.clone(), &mut network) {
        Ok(fetch) => fetch,
        Err(failure) => {
            return judge(
                expected,
                ManifestName::new(failure.name),
                failure.status,
                None,
                0,
                0,
                0,
                tick::now_ms().saturating_sub(started),
            );
        }
    };
    let deadline = started + ENDPOINT_DEADLINE_MS;
    loop {
        match fetch.step(&mut network) {
            FetchOutcome::Working => {
                if tick::now_ms() > deadline {
                    let received = fetch.received();
                    let peak = fetch.peak_owned();
                    let redirects = fetch.redirects();
                    fetch.close(&mut network);
                    return judge(
                        expected,
                        ManifestName::new("hung"),
                        None,
                        None,
                        peak,
                        received,
                        redirects,
                        tick::now_ms().saturating_sub(started),
                    );
                }
            }
            FetchOutcome::Page(document) => {
                let received = fetch.received();
                let peak = fetch.peak_owned();
                let redirects = fetch.redirects();
                fetch.close(&mut network);
                return judge(
                    expected,
                    ManifestName::new("ok"),
                    None,
                    Some(document),
                    peak,
                    received,
                    redirects,
                    tick::now_ms().saturating_sub(started),
                );
            }
            FetchOutcome::Failed(failure) => {
                let received = fetch.received();
                let peak = fetch.peak_owned();
                let redirects = fetch.redirects();
                fetch.close(&mut network);
                return judge(
                    expected,
                    ManifestName::new(failure.name),
                    failure.status,
                    None,
                    peak,
                    received,
                    redirects,
                    tick::now_ms().saturating_sub(started),
                );
            }
        }
    }
}

/// Compares what happened with what was expected.
///
/// A refused status carries its number into the name -- `status-404` -- so
/// the manifest can say which refusal it meant rather than only that there
/// was one.
#[allow(clippy::too_many_arguments)]
fn judge(
    expected: Expectation,
    name: ManifestName,
    status: Option<u16>,
    document: Option<crate::browser::document::Document>,
    peak_owned: usize,
    received: usize,
    redirects: usize,
    elapsed_ms: u64,
) -> Visit {
    let got = match (name.as_str(), status) {
        ("status", Some(code)) => {
            let mut text = Line::new();
            text.push_str("status-");
            text.push_u32(code as u32);
            ManifestName::new(text.as_str())
        }
        _ => name,
    };
    let matched = match expected {
        Expectation::Page => got.as_str() == "ok",
        Expectation::Failure(wanted) => got == wanted,
    };
    let (crc, blocks, links, text_bytes) = match &document {
        Some(document) => {
            let mut crc = Crc32::new();
            crc.update(document.text().as_bytes());
            (
                crc.finish(),
                document.blocks().len(),
                document.links().len(),
                document.stats().text_bytes,
            )
        }
        None => (0, 0, 0, 0),
    };
    Visit {
        matched,
        got,
        crc,
        blocks,
        links,
        text_bytes,
        peak_owned,
        received,
        redirects,
        elapsed_ms,
    }
}

/// Reads the manifest with the blocking client.
///
/// `net::http::get` rather than a `Fetch`: the manifest is `text/plain`,
/// and `Fetch` exists to produce documents. A diagnostic that owns the
/// console may as well block for the one request that is not a page.
fn load_manifest(
    url: &Url,
    address: smoltcp::wire::Ipv4Address,
    rpc: &mut wifi::Rpc,
    stack: &mut net::Stack,
) -> Result<String, &'static str> {
    // `host()` and not `host_header()`: `net::http::get` builds the header
    // from a bare host and the port, where `Transaction::start` takes the
    // finished value. Handing it the finished one asked the server for
    // `Host: 192.168.0.159:8080:8080`, which no fixture had ever looked at
    // until the manifest started building URLs out of it.
    let Ok(target) = url.request_target() else {
        return Err("out of memory");
    };
    let host = url.host();
    let mut body = String::new();
    let mut overflowed = false;
    let outcome = {
        let mut sink = |bytes: &[u8]| {
            if body.len() + bytes.len() > MAX_MANIFEST_BYTES {
                overflowed = true;
                return false;
            }
            match core::str::from_utf8(bytes) {
                Ok(text) => crate::browser::memory::push_str(&mut body, text).is_ok(),
                // The manifest is ASCII; a chunk boundary cannot split a
                // character in it, so anything that fails here is not the
                // file this expects.
                Err(_) => false,
            }
        };
        net::http::get(
            stack,
            rpc,
            address,
            url.port(),
            host.as_bytes(),
            target.as_bytes(),
            security_for(url),
            &mut sink,
        )
    };
    if overflowed {
        return Err("the manifest is larger than expected");
    }
    match outcome {
        Ok(response) if matches!(response.status, Some(200)) => Ok(body),
        Ok(_) => Err("the server did not return the manifest"),
        Err(error) => Err(net::http::error_text(error)),
    }
}

/// Splits one `path<TAB>expectation` line.
fn split_entry(line: &str) -> Option<(&str, &str)> {
    let mut fields = line.split('\t');
    let path = fields.next()?.trim();
    let expectation = fields.next()?.trim();
    if path.is_empty() || expectation.is_empty() {
        return None;
    }
    Some((path, expectation))
}

fn report_bad_address(console: &mut Console, framebuffer: &mut Framebuffer, path: &str) {
    let mut line = Line::new();
    line.push_str("FAIL ");
    line.push_str(path);
    line.push_str(": not an address");
    console.write_output_line(framebuffer, line.as_str());
}

fn report_mismatch(
    console: &mut Console,
    framebuffer: &mut Framebuffer,
    path: &str,
    expectation: &str,
    result: &Visit,
) {
    let mut line = Line::new();
    line.push_str("FAIL ");
    line.push_str(path);
    console.write_output_line(framebuffer, line.as_str());
    let mut line = Line::new();
    line.push_str("  expected ");
    line.push_str(expectation);
    line.push_str(", got ");
    line.push_str(result.got.as_str());
    console.write_output_line(framebuffer, line.as_str());
}

fn write_totals(console: &mut Console, framebuffer: &mut Framebuffer, totals: &Totals) {
    let mut line = Line::new();
    line.push_u32(totals.passed);
    line.push_str(" passed, ");
    line.push_u32(totals.failed);
    line.push_str(" failed, ");
    line.push_u32(totals.skipped);
    line.push_str(" skipped");
    console.write_output_line(framebuffer, line.as_str());

    let mut line = Line::new();
    line.push_str("peak owned ");
    line.push_u32(totals.peak_owned as u32);
    line.push_str(" of ");
    line.push_u32(crate::browser::limits::MAX_BROWSER_OWNED_BYTES as u32);
    line.push_str(", slowest ");
    line.push_u32(totals.slowest_ms as u32);
    line.push_str(" ms");
    console.write_output_line(framebuffer, line.as_str());

    if totals.peak_owned > crate::browser::limits::MAX_BROWSER_OWNED_BYTES {
        console.write_output_line(framebuffer, "OVER BUDGET: a page owned more than 4 MiB");
    }
}

/// One line per endpoint on the UART, in a shape two runs can be diffed.
fn log_visit(round: u32, path: &str, expectation: &str, result: &Visit) {
    let mut line = LogLine::new();
    line.text(if result.matched {
        "BT  ok  "
    } else {
        "BT FAIL "
    });
    line.text("round=");
    line.decimal(round);
    line.text(" path=");
    line.text(path);
    line.text(" expect=");
    line.text(expectation);
    line.text(" got=");
    line.text(result.got.as_str());
    line.text(" crc=");
    line.hex(result.crc);
    line.text(" blocks=");
    line.decimal(result.blocks as u32);
    line.text(" links=");
    line.decimal(result.links as u32);
    line.text(" text=");
    line.decimal(result.text_bytes as u32);
    line.text(" recv=");
    line.decimal(result.received as u32);
    line.text(" hops=");
    line.decimal(result.redirects as u32);
    line.text(" peak=");
    line.decimal(result.peak_owned as u32);
    line.text(" ms=");
    line.decimal(result.elapsed_ms as u32);
    line.finish();
}
