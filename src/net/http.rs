//! HTTP/1.0 GET, in two shapes over one implementation.
//!
//! [`Transaction`] is the real one: a state machine that owns a socket in
//! the stack's `SocketSet` and makes a bounded amount of progress each time
//! it is polled. That is what the browser needs, because a screen that
//! cannot come back to its input loop until a page has finished arriving is
//! a screen that cannot be cancelled, cannot service USB and cannot redraw
//! -- and "the page finishes arriving" is not something the other end has
//! agreed to do.
//!
//! [`get`] is the old blocking call, kept because `httpget` and its saved
//! downloads are a working regression test for the TCP path. It is now a
//! loop around [`Transaction`] rather than a second implementation, so
//! header parsing, body framing and socket teardown exist once.
//!
//! What this understands of HTTP is deliberately small and listed in
//! [`Head`]: a status, a length or a chunked encoding, a content type, a
//! `Location`, and a refusal to pretend it can decode a `Content-Encoding`
//! it cannot. No cookies, no authentication, no keep-alive.
//!
//! Nothing here touches a socket. The bytes come from a
//! [`Transport`](crate::net::transport::Transport), which is either a TCP
//! stream or a TLS session over one, and everything below -- the status
//! line, the header block, the chunk decoder, where a body ends -- is the
//! same code either way. That is the point: two copies of "where does the
//! body end", one for `http` and one for `https`, is two chances to get it
//! wrong and one page shown as complete when it is half of one.
//!
//! Names are resolved before they get here: the caller passes both the
//! address to connect to and the text to put in `Host:`.

use alloc::vec::Vec;

use smoltcp::wire::Ipv4Address;

use crate::browser::memory::{self, OutOfMemory};
use crate::browser::request::{self, Method};
use crate::net::Stack;
use crate::net::tls;
use crate::net::transport::{self, Security, Transport};
use crate::tick;
use crate::wifi::Rpc;

/// How long the transfer may stall before it is called dead.
const IDLE_TIMEOUT_MS: u64 = 5000;

/// How much of the header block is kept. Enough for a status line and the
/// headers worth looking at; a server that has not finished its headers by
/// here is not one this can work with.
///
/// The same number the browser's limit table records, taken from there
/// rather than restated: two copies of a bound are two bounds.
pub const MAX_HEADER_BYTES: usize = crate::browser::limits::MAX_HEADER_BYTES;

/// Bytes moved off the socket in one [`Transaction::poll`] when the caller
/// does not ask for a different budget.
///
/// This is what keeps a fast server from starving the frame loop: at some
/// point the polling has to stop and let the caller check its input, and
/// 4 KiB is roughly one TCP window's worth -- enough that the round trip
/// through the caller is not the bottleneck, small enough that handing it
/// to the tokenizer is not a visible pause.
pub const DEFAULT_POLL_BUDGET: usize = 4096;

/// Read granularity inside one poll. On the stack, so it is bounded by the
/// 128 KiB the linker guarantees rather than by the heap.
const READ_CHUNK: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The byte stream underneath failed: the link, the connection, or --
    /// for `https` -- the handshake, the certificate or a pin.
    ///
    /// Carried rather than flattened so that a TLS failure keeps its own
    /// name all the way to the screen: "the certificate's signature is
    /// wrong" and "the connection timed out" are not the same thing to
    /// report, and a browser fixture matching on one must not pass on the
    /// other.
    Transport(transport::Error),
    /// Connected, but the peer stopped talking mid-response.
    TimedOut,
    /// No blank line ended the headers within [`MAX_HEADER_BYTES`], so where
    /// the body starts is unknown. Everything after that would be a guess.
    HeadersTooLong,
    /// The peer closed before the body it promised had arrived -- a
    /// `Content-Length` unmet, or a chunked body with no terminating chunk.
    ///
    /// Distinct from a clean close-delimited end on purpose: one is a
    /// complete document and the other is half of one, and showing half a
    /// page as though it were the page is the failure this whole layer is
    /// arranged to avoid.
    Truncated,
    /// The chunked framing did not parse: a size that is not hexadecimal, a
    /// missing CRLF after a chunk, a size past what a body may be.
    ChunkFraming,
    /// The body went past the caller's limit.
    BodyTooLong,
    /// A `Content-Encoding` other than `identity`. The request asks for
    /// `identity` only, so this is a server ignoring what it was asked;
    /// the body is compressed and there is no decompressor here.
    UnsupportedEncoding,
    /// The sink would not take the body, so the transfer was abandoned.
    SinkRefused,
    /// The caller stopped it.
    Cancelled,
    OutOfMemory,
    /// A request value could have introduced another request or header.
    InvalidRequest,
    Local,
}

impl From<OutOfMemory> for Error {
    fn from(_: OutOfMemory) -> Self {
        Error::OutOfMemory
    }
}

impl From<transport::Error> for Error {
    fn from(error: transport::Error) -> Self {
        match error {
            // The ones the HTTP layer already had names for keep them, so
            // that splitting the transport out renamed no failure.
            transport::Error::Cancelled => Error::Cancelled,
            transport::Error::OutOfMemory => Error::OutOfMemory,
            transport::Error::Local => Error::Local,
            error => Error::Transport(error),
        }
    }
}

/// A short ASCII sentence, for the console and the browser's status line.
pub fn error_text(error: Error) -> &'static str {
    match error {
        Error::Transport(error) => error.message(),
        Error::TimedOut => "the server stopped responding",
        Error::HeadersTooLong => "the response headers never ended",
        Error::Truncated => "the connection closed before the page finished",
        Error::ChunkFraming => "the chunked response was malformed",
        Error::BodyTooLong => "the page is larger than this can hold",
        Error::UnsupportedEncoding => "the response is compressed, which is not supported",
        Error::SinkRefused => "the transfer was abandoned",
        Error::Cancelled => "cancelled",
        Error::OutOfMemory => "out of memory during the transfer",
        Error::InvalidRequest => "the request contained an unsafe header value",
        Error::Local => "a local socket operation failed",
    }
}

/// A short name, for one-line UART statistics.
pub fn error_name(error: Error) -> &'static str {
    match error {
        Error::Transport(error) => error.name(),
        Error::TimedOut => "timed-out",
        Error::HeadersTooLong => "header-limit",
        Error::Truncated => "truncated",
        Error::ChunkFraming => "chunk",
        Error::BodyTooLong => "body-limit",
        Error::UnsupportedEncoding => "encoding",
        Error::SinkRefused => "sink-refused",
        Error::Cancelled => "cancelled",
        Error::OutOfMemory => "out-of-memory",
        Error::InvalidRequest => "invalid-request",
        Error::Local => "local",
    }
}

/// Everything read out of the response head.
///
/// The raw block is kept too, because `httpget` prints it and because a
/// response this does not understand is more useful shown than summarised.
pub struct Head {
    /// The status line's code, or `None` when the first line was not one.
    ///
    /// `None` rather than an error: this layer reports what arrived and
    /// lets the caller decide. `httpget` prints it and moves on; the
    /// browser treats it as "not an HTTP response" and stops.
    pub status: Option<u16>,
    pub content_length: Option<u64>,
    pub chunked: bool,
    /// Whether the body is stored as sent. False when `Content-Encoding`
    /// named anything other than `identity`.
    pub identity_encoding: bool,
    /// `Location`, as sent. Resolving it against the request URL is the
    /// caller's job -- this layer has no URL, only a host and a target.
    pub location: Option<Vec<u8>>,
    /// The media type from `Content-Type`, lowercased, without parameters.
    pub media_type: Option<Vec<u8>>,
    /// The `charset` parameter, lowercased.
    pub charset: Option<Vec<u8>>,
    /// `Cache-Control` carried a `no-store` directive. The response may be
    /// shown but no copy of it may be kept for later.
    pub no_store: bool,
    /// `ETag`, when it was present, printable and at most 256 bytes.
    pub etag: Option<Vec<u8>>,
    /// `Vary` named something other than `Accept-Encoding`, which this
    /// client sends as a fixed value. A response that varies on anything
    /// else, or on `*`, cannot be matched to a later request and is not
    /// stored.
    pub vary_blocks_cache: bool,
    /// `Cache-Control: no-cache`: a stored copy may only be reused after the
    /// server confirms it.
    pub no_cache: bool,
    /// `Cache-Control: max-age` in seconds, the smallest when repeated. An
    /// unreadable value counts as zero, which makes the response stale.
    pub max_age: Option<u64>,
    /// `Age` in seconds.
    pub age: Option<u64>,
    /// `Date` as sent, when it is short enough to be a date.
    pub date: Option<Vec<u8>>,
    /// `Expires` as sent. Present but too long to be a date is kept empty,
    /// which the cache reads as already expired.
    pub expires: Option<Vec<u8>>,
    /// The status line and headers, without the blank line that ends them.
    pub raw: Vec<u8>,
}

impl Head {
    pub fn is_html(&self) -> bool {
        match &self.media_type {
            Some(media) => media == b"text/html" || media == b"application/xhtml+xml",
            // A response with no `Content-Type` at all is treated as HTML:
            // it is what the fixture server's hand-written responses do and
            // what a great many small servers do, and the tokenizer is
            // safe on arbitrary bytes anyway.
            None => true,
        }
    }

    /// Whether the body is text of some kind that is not HTML.
    ///
    /// Every `text/*` subtype except `text/html`, which [`Self::is_html`]
    /// has already claimed. The browser shows these as themselves: a
    /// `.txt`, a `.md`, a `.csv`, a server's `manifest.txt`. A response
    /// with no `Content-Type` is not one of these -- it is treated as HTML,
    /// see above -- so this only ever answers for a server that said what
    /// it was sending.
    pub fn is_text(&self) -> bool {
        match &self.media_type {
            Some(media) => media.starts_with(b"text/") && !self.is_html(),
            None => false,
        }
    }

    pub fn is_redirect(&self) -> bool {
        matches!(self.status, Some(301 | 302 | 303 | 307 | 308))
    }

    pub fn is_success(&self) -> bool {
        matches!(self.status, Some(status) if (200..300).contains(&status))
    }
}

/// What one [`Transaction::poll`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Progress {
    /// The connection is not open yet.
    Connecting,
    /// The head finished during this poll; [`Transaction::head`] now
    /// answers. The caller decides here whether to keep going -- a
    /// redirect, an unsupported encoding or a status it will not render
    /// are all known before a byte of body has been handed over.
    HeadReady,
    /// Body bytes reached the sink during this poll.
    Body,
    /// Nothing arrived. Not an error: the peer is thinking, or the budget
    /// was spent on a head that is still incomplete.
    Idle,
    /// The body ended, cleanly and completely.
    Complete,
    Failed(Error),
}

/// How the body's end is decided.
enum Framing {
    /// `Content-Length`.
    Length { remaining: u64 },
    /// `Transfer-Encoding: chunked`.
    Chunked(Chunked),
    /// Neither: the body ends when the peer closes. HTTP/1.0's default and
    /// what almost every response to this client actually uses.
    UntilClose,
}

/// Where the chunked decoder is between two bytes.
///
/// Written as a byte-at-a-time state machine rather than by finding CRLFs
/// in a buffer because chunk boundaries arrive split: the fixture server
/// moves them on every request precisely so that a size line, a CRLF or a
/// trailer gets cut in half sooner or later.
enum Chunked {
    /// Reading hexadecimal size digits.
    Size { value: u64, digits: usize },
    /// Past a `;` in the size line: skipping the extension to the CR. The
    /// size has already been read and is carried across.
    Extension { size: u64 },
    /// Just saw the CR that ends a size line; the LF has to follow.
    SizeLf { size: u64 },
    /// Copying chunk data out.
    Data { remaining: u64 },
    /// The CR of the CRLF that follows chunk data.
    DataCr,
    /// The LF of that CRLF.
    DataLf,
    /// After the terminal zero chunk: skipping trailer lines until a blank
    /// one. `blank` tracks how much of the ending CRLFCRLF has been seen.
    Trailer { at_line_start: bool, seen_cr: bool },
    /// The body is over.
    Done,
}

/// The largest chunk size that will be accepted, as a guard on the hex
/// parser rather than on the body: `ffffffffffffffff` is a legal-looking
/// size line, and multiplying by sixteen forever is how a parser is made
/// to spin.
const MAX_CHUNK_SIZE: u64 = 1 << 40;

/// The maximum number of hexadecimal digits in a chunk size line.
const MAX_CHUNK_SIZE_DIGITS: usize = 16;

enum State {
    Connecting,
    /// Connected; the request is going out.
    Sending,
    /// Reading the head.
    Head,
    /// Reading the body.
    Body(Framing),
    Complete,
    Failed(Error),
}

/// Counters worth reporting for one fetch.
#[derive(Clone, Copy, Default)]
pub struct Stats {
    /// Everything read off the socket, head included.
    pub received: usize,
    /// Body bytes handed to the sink, after chunk decoding.
    pub body_bytes: u64,
    pub elapsed_ms: u64,
    /// How many times `poll` was called.
    pub polls: u32,
}

/// One HTTP GET in flight.
///
/// Owns a [`Transport`] -- which owns a socket handle in the caller's
/// `SocketSet` -- and nothing else. Specifically not the [`Stack`] or the
/// [`Rpc`], which are borrowed for the length of a single
/// [`Transaction::poll`] and given back. That is what lets the browser's
/// frame loop keep the stack for its own polling while a fetch is running.
///
/// It must be handed to [`Transaction::close`]. Dropping it instead leaks
/// the socket out of the set, and since the set is `'static` that socket
/// is gone for the run; the `Drop` below says so on the UART rather than
/// letting it be silent.
#[must_use = "a Transaction owns a socket and has to be closed"]
pub struct Transaction {
    transport: Option<Transport>,
    state: State,
    /// The request, and how much of it has been handed to the transport.
    /// Sending can be partial: the plaintext send buffer is 8 KiB but a
    /// request with a long target might not fit in one call.
    request: Vec<u8>,
    sent: usize,
    /// The head block as it accumulates, replaced by [`Head`] once the
    /// blank line turns up.
    head_buffer: Vec<u8>,
    head: Option<Head>,
    max_body: u64,
    stats: Stats,
    started_ms: u64,
    last_progress_ms: u64,
    /// Set by [`Transaction::close`]; checked by `Drop`.
    closed: bool,
}

impl Transaction {
    /// Opens a connection and queues `GET target`.
    ///
    /// `host` is the finished `Host:` header value -- the port included when
    /// it is not the scheme's default, which is what
    /// [`Url::host_header`](crate::browser::url::Url::host_header) produces.
    /// Nothing is appended to it here. It is not always `address` written
    /// out: when the destination was given as a name, the name is what the
    /// server needs to pick a virtual host, and the address it resolved to
    /// tells it nothing.
    ///
    /// [`get`] takes the *bare* host instead and builds this from it. The
    /// two are different on purpose and were once the same word, which is
    /// how a caller came to hand a header value to the one that appends a
    /// port to it.
    ///
    /// `max_body` bounds the decoded body. A transfer that reaches it fails
    /// with [`Error::BodyTooLong`] rather than delivering a prefix -- and a
    /// `Content-Length` past it fails before the body is read at all.
    ///
    /// `security` decides whether this is `http` or `https`, and is the
    /// only place the difference is made. A failure to open a TLS
    /// connection at all -- no hardware randomness, for instance -- is
    /// reported here rather than as state, because there is no socket to
    /// clean up when it happens.
    pub fn start(
        stack: &mut Stack,
        address: Ipv4Address,
        port: u16,
        host: &[u8],
        target: &[u8],
        max_body: u64,
        security: Security<'_>,
    ) -> Result<Transaction, Error> {
        Self::start_request(
            stack,
            address,
            port,
            host,
            target,
            Method::Get,
            &[],
            None,
            max_body,
            security,
        )
    }

    /// Opens a connection and queues a bounded GET or urlencoded POST.
    #[allow(clippy::too_many_arguments)]
    pub fn start_request(
        stack: &mut Stack,
        address: Ipv4Address,
        port: u16,
        host: &[u8],
        target: &[u8],
        method: Method,
        body: &[u8],
        if_none_match: Option<&[u8]>,
        max_body: u64,
        security: Security<'_>,
    ) -> Result<Transaction, Error> {
        let request = build_request(method, body, host, target, if_none_match)?;
        let transport = Transport::connect(stack, address, port, security)?;
        let now = tick::now_ms();
        Ok(Transaction {
            transport: Some(transport),
            state: State::Connecting,
            request,
            sent: 0,
            head_buffer: Vec::new(),
            head: None,
            max_body,
            stats: Stats::default(),
            started_ms: now,
            last_progress_ms: now,
            closed: false,
        })
    }

    /// What the connection proved about who is on the other end, or `None`
    /// for plaintext HTTP.
    pub fn authentication(&self) -> Option<tls::Authentication> {
        self.transport.as_ref().and_then(Transport::authentication)
    }

    /// Whether a TLS stream ended with a TCP close rather than a
    /// `close_notify`. Always false for plaintext.
    pub fn closed_without_notify(&self) -> bool {
        self.transport
            .as_ref()
            .is_some_and(Transport::closed_without_notify)
    }

    /// Whether any request byte has been handed to the transport.
    pub fn request_started(&self) -> bool {
        self.sent != 0
    }

    /// The TLS counters, for a caller reporting them. `None` for plaintext.
    pub fn tls_stats(&self) -> Option<tls::Stats> {
        self.transport.as_ref().and_then(Transport::tls_stats)
    }

    /// Makes up to `budget` bytes of progress, then returns.
    ///
    /// Returning is the point. Every path through this either moves bytes
    /// or notices it cannot, and none of them waits: the caller polls again
    /// when it has finished looking at its own input.
    pub fn poll(
        &mut self,
        stack: &mut Stack,
        rpc: &mut Rpc,
        budget: usize,
        sink: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Progress {
        self.stats.polls = self.stats.polls.saturating_add(1);
        match self.state {
            State::Complete => return Progress::Complete,
            State::Failed(error) => return Progress::Failed(error),
            _ => {}
        }

        // The transport is stepped first and unconditionally: for TLS this
        // is what runs the handshake and turns records into plaintext, and
        // there is nothing for the parser to read until it has.
        let Some(transport) = self.transport.as_mut() else {
            return self.fail(Error::Local);
        };
        match transport.poll(stack, rpc, budget) {
            transport::Progress::Failed(error) => return self.fail(error.into()),
            transport::Progress::Connecting => {
                if matches!(self.state, State::Connecting) {
                    return Progress::Connecting;
                }
                // Past the point where the request went out, a transport
                // that says it is not ready has gone backwards, which only
                // a broken one does.
                return self.fail(Error::Transport(transport::Error::NotConnected));
            }
            transport::Progress::Ready => {
                if matches!(self.state, State::Connecting) {
                    self.state = State::Sending;
                    self.last_progress_ms = tick::now_ms();
                }
            }
        }

        match self.state {
            State::Sending => self.poll_sending(stack),
            State::Head | State::Body(_) => self.poll_receiving(stack, budget, sink),
            State::Complete => Progress::Complete,
            State::Failed(error) => Progress::Failed(error),
            State::Connecting => Progress::Connecting,
        }
    }

    fn poll_sending(&mut self, stack: &mut Stack) -> Progress {
        let Some(transport) = self.transport.as_mut() else {
            return self.fail(Error::Local);
        };
        match transport.write(stack, &self.request[self.sent..]) {
            Ok(count) => {
                self.sent += count;
                if count > 0 {
                    self.last_progress_ms = tick::now_ms();
                }
            }
            Err(error) => return self.fail(error.into()),
        }
        if self.sent >= self.request.len() {
            // The encoded request includes any POST body, so reaching its
            // end means both head and body have been handed over. The TLS
            // engine needs telling; the plaintext one does not care.
            transport.finish_request();
            self.state = State::Head;
        } else if tick::now_ms().saturating_sub(self.last_progress_ms) > transport::WRITE_STALL_MS {
            return self.fail(Error::TimedOut);
        }
        Progress::Connecting
    }

    fn poll_receiving(
        &mut self,
        stack: &mut Stack,
        budget: usize,
        sink: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Progress {
        let mut head_ready = false;
        let mut delivered = false;

        // Body bytes held back by the read that finished the head. Flushed
        // before anything else, and before the socket is read again: if the
        // whole response arrived in one read, these are the entire body and
        // there will never be another read to carry them.
        if matches!(self.state, State::Body(_)) && !self.head_buffer.is_empty() {
            let carried = core::mem::take(&mut self.head_buffer);
            match self.consume_body(&carried, sink) {
                Ok((delivered_now, finished)) => {
                    delivered |= delivered_now;
                    if finished {
                        return self.complete();
                    }
                }
                Err(error) => return self.fail(error),
            }
        }
        // `Content-Length: 0` -- a body that is over before it starts, and
        // the one case no read can ever finish.
        if matches!(self.state, State::Body(Framing::Length { remaining: 0 })) {
            return self.complete();
        }

        let mut spent = 0usize;
        while spent < budget {
            let mut chunk = [0u8; READ_CHUNK];
            let want = READ_CHUNK.min(budget - spent);
            let Some(transport) = self.transport.as_mut() else {
                return self.fail(Error::Local);
            };
            let count = transport.read(stack, &mut chunk[..want]);
            if count == 0 {
                break;
            }
            spent += count;
            self.stats.received += count;
            self.last_progress_ms = tick::now_ms();
            match self.consume(&chunk[..count], sink) {
                Ok(outcome) => {
                    head_ready |= outcome.head_ready;
                    delivered |= outcome.delivered;
                    if outcome.finished {
                        return self.complete();
                    }
                }
                Err(error) => return self.fail(error),
            }
            // A head that just completed is worth returning on even with
            // budget left: the caller may be about to stop -- a redirect, a
            // status it will not render, an encoding it cannot read -- and
            // reading a body it is going to throw away is exactly the work
            // this layer exists to avoid.
            if head_ready {
                break;
            }
        }

        if head_ready {
            return Progress::HeadReady;
        }
        if delivered {
            return Progress::Body;
        }

        // Nothing arrived. Either the peer has finished, or it is quiet.
        let at_end = self
            .transport
            .as_ref()
            .is_some_and(|transport| transport.at_end(stack));
        if at_end {
            return self.finish_at_close();
        }
        if tick::now_ms().saturating_sub(self.last_progress_ms) > IDLE_TIMEOUT_MS {
            return self.fail(Error::TimedOut);
        }
        Progress::Idle
    }

    /// What a clean close means depends on the framing that was announced.
    ///
    /// Only [`Framing::UntilClose`] treats it as the end of the document.
    /// A `Content-Length` that has not been met and a chunked body with no
    /// terminating chunk are both half a page, and half a page shown as
    /// though it were the page is the outcome this whole layer is arranged
    /// to avoid.
    fn finish_at_close(&mut self) -> Progress {
        match &self.state {
            // The head never ended -- the peer simply stopped. Where the
            // body starts is unknown, which is the same dead end as a head
            // that was too long, but it happened for a different reason and
            // is reported as one.
            State::Head => self.fail(Error::Truncated),
            State::Body(Framing::UntilClose) => self.complete(),
            State::Body(Framing::Length { remaining }) if *remaining == 0 => self.complete(),
            State::Body(_) => self.fail(Error::Truncated),
            State::Complete => Progress::Complete,
            _ => self.fail(Error::Transport(transport::Error::NotConnected)),
        }
    }

    fn complete(&mut self) -> Progress {
        self.state = State::Complete;
        self.stats.elapsed_ms = tick::now_ms().saturating_sub(self.started_ms);
        Progress::Complete
    }

    /// Feeds one read into the head parser or the body framing.
    ///
    /// Body bytes that arrive in the same read as the end of the head are
    /// *not* delivered here. They are held in `head_buffer` and flushed at
    /// the start of the next poll, so that [`Progress::HeadReady`] is
    /// always reported on its own -- which is what lets a caller stop on a
    /// redirect or an unwanted status without having been handed a body
    /// first.
    fn consume(
        &mut self,
        bytes: &[u8],
        sink: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<Consumed, Error> {
        let mut outcome = Consumed::default();
        match self.state {
            State::Head => {
                let searched_from = self.head_buffer.len().saturating_sub(3);
                memory::extend_from_slice(&mut self.head_buffer, bytes)?;
                match find_header_end(&self.head_buffer, searched_from) {
                    Some(end) => {
                        let body_start = end + HEADER_TERMINATOR.len();
                        let carried = self.head_buffer.split_off(body_start);
                        self.head_buffer.truncate(end);
                        let head = parse_head(core::mem::take(&mut self.head_buffer))?;
                        // Recorded before the framing is chosen, so that a
                        // response refused for its framing -- a compressed
                        // body, a length past the limit -- can still be
                        // reported with its status and headers. Refusing
                        // one without being able to say what it was is a
                        // bad diagnostic and a worse error page.
                        self.head = Some(head);
                        let framing = framing_for(self.max_body, self.head.as_ref())?;
                        self.state = State::Body(framing);
                        self.head_buffer = carried;
                        outcome.head_ready = true;
                    }
                    None => {
                        if self.head_buffer.len() > MAX_HEADER_BYTES {
                            return Err(Error::HeadersTooLong);
                        }
                    }
                }
            }
            State::Body(_) => {
                let (delivered, finished) = self.consume_body(bytes, sink)?;
                outcome.delivered = delivered;
                outcome.finished = finished;
            }
            _ => {}
        }
        Ok(outcome)
    }

    /// Applies the framing to a run of body bytes.
    ///
    /// Returns whether anything reached the sink and whether the body ended.
    fn consume_body(
        &mut self,
        bytes: &[u8],
        sink: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<(bool, bool), Error> {
        let State::Body(framing) = &mut self.state else {
            return Ok((false, false));
        };
        match framing {
            Framing::UntilClose => {
                let count = bytes.len() as u64;
                if self.stats.body_bytes + count > self.max_body {
                    return Err(Error::BodyTooLong);
                }
                self.stats.body_bytes += count;
                if !sink(bytes) {
                    return Err(Error::SinkRefused);
                }
                Ok((!bytes.is_empty(), false))
            }
            Framing::Length { remaining } => {
                // Anything past the announced length is not part of the
                // body. It is dropped rather than delivered: the server
                // said where the body ends, and a document that includes
                // whatever followed is not the document it sent.
                let take = (*remaining).min(bytes.len() as u64) as usize;
                *remaining -= take as u64;
                let finished = *remaining == 0;
                if take > 0 {
                    if self.stats.body_bytes + take as u64 > self.max_body {
                        return Err(Error::BodyTooLong);
                    }
                    self.stats.body_bytes += take as u64;
                    if !sink(&bytes[..take]) {
                        return Err(Error::SinkRefused);
                    }
                }
                Ok((take > 0, finished))
            }
            Framing::Chunked(_) => self.consume_chunked(bytes, sink),
        }
    }

    fn consume_chunked(
        &mut self,
        bytes: &[u8],
        sink: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<(bool, bool), Error> {
        let mut delivered = false;
        let mut index = 0usize;
        while index < bytes.len() {
            let State::Body(Framing::Chunked(chunked)) = &mut self.state else {
                break;
            };
            match chunked {
                Chunked::Done => break,
                Chunked::Data { remaining } => {
                    let take = (*remaining).min((bytes.len() - index) as u64) as usize;
                    if self.stats.body_bytes + take as u64 > self.max_body {
                        return Err(Error::BodyTooLong);
                    }
                    *remaining -= take as u64;
                    let ended = *remaining == 0;
                    if ended {
                        *chunked = Chunked::DataCr;
                    }
                    self.stats.body_bytes += take as u64;
                    if take > 0 {
                        if !sink(&bytes[index..index + take]) {
                            return Err(Error::SinkRefused);
                        }
                        delivered = true;
                    }
                    index += take;
                }
                _ => {
                    let byte = bytes[index];
                    index += 1;
                    step_chunked(chunked, byte)?;
                }
            }
        }
        let done = matches!(&self.state, State::Body(Framing::Chunked(Chunked::Done)));
        Ok((delivered, done))
    }

    fn fail(&mut self, error: Error) -> Progress {
        self.state = State::Failed(error);
        self.stats.elapsed_ms = tick::now_ms().saturating_sub(self.started_ms);
        Progress::Failed(error)
    }

    /// The response head, once [`Progress::HeadReady`] has been reported.
    pub fn head(&self) -> Option<&Head> {
        self.head.as_ref()
    }

    /// Takes the head out, for a caller that keeps it past the transaction.
    pub fn take_head(&mut self) -> Option<Head> {
        self.head.take()
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Complete | State::Failed(_))
    }

    pub fn error(&self) -> Option<Error> {
        match self.state {
            State::Failed(error) => Some(error),
            _ => None,
        }
    }

    /// Stops the transfer. The socket still has to be [`Transaction::close`]d.
    pub fn cancel(&mut self) {
        if !self.is_finished() {
            if let Some(transport) = self.transport.as_mut() {
                transport.cancel();
            }
            self.state = State::Failed(Error::Cancelled);
            self.stats.elapsed_ms = tick::now_ms().saturating_sub(self.started_ms);
        }
    }

    /// Ends the connection and takes the socket out of the set.
    pub fn close(mut self, stack: &mut Stack, rpc: &mut Rpc) -> Stats {
        if let Some(transport) = self.transport.take() {
            transport.close(stack, rpc);
        }
        self.closed = true;
        if self.stats.elapsed_ms == 0 {
            self.stats.elapsed_ms = tick::now_ms().saturating_sub(self.started_ms);
        }
        self.stats
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.closed {
            transport::report_unclosed(b"HTTP");
        }
    }
}

/// What one read did, so `poll` can report a single [`Progress`].
#[derive(Default)]
struct Consumed {
    head_ready: bool,
    delivered: bool,
    finished: bool,
}

/// Chooses the body framing from the head, refusing what cannot be read.
fn framing_for(max_body: u64, head: Option<&Head>) -> Result<Framing, Error> {
    let Some(head) = head else {
        return Err(Error::Local);
    };
    if !head.identity_encoding {
        return Err(Error::UnsupportedEncoding);
    }
    if head.chunked {
        return Ok(Framing::Chunked(Chunked::Size {
            value: 0,
            digits: 0,
        }));
    }
    match head.content_length {
        Some(length) => {
            // Refused here, before a byte of it is read. A megabyte that is
            // going to be rejected anyway is a megabyte of somebody's
            // bandwidth and several seconds of the user's time.
            if length > max_body {
                return Err(Error::BodyTooLong);
            }
            Ok(Framing::Length { remaining: length })
        }
        None => Ok(Framing::UntilClose),
    }
}

/// Advances the chunked decoder by one framing byte.
fn step_chunked(state: &mut Chunked, byte: u8) -> Result<(), Error> {
    match state {
        Chunked::Size { value, digits } => match byte {
            b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' => {
                if *digits >= MAX_CHUNK_SIZE_DIGITS {
                    return Err(Error::ChunkFraming);
                }
                let digit = (byte as char).to_digit(16).unwrap_or(0) as u64;
                *value = value.saturating_mul(16).saturating_add(digit);
                if *value > MAX_CHUNK_SIZE {
                    return Err(Error::ChunkFraming);
                }
                *digits += 1;
                Ok(())
            }
            b';' => {
                if *digits == 0 {
                    return Err(Error::ChunkFraming);
                }
                *state = Chunked::Extension { size: *value };
                Ok(())
            }
            b'\r' => {
                if *digits == 0 {
                    return Err(Error::ChunkFraming);
                }
                let size = *value;
                *state = Chunked::SizeLf { size };
                Ok(())
            }
            _ => Err(Error::ChunkFraming),
        },
        Chunked::Extension { size } => {
            // Extensions are skipped rather than read: no extension this
            // has any use for exists, and the CR is the only byte in here
            // that changes what happens next.
            if byte == b'\r' {
                *state = Chunked::SizeLf { size: *size };
            }
            Ok(())
        }
        Chunked::SizeLf { size } => {
            if byte != b'\n' {
                return Err(Error::ChunkFraming);
            }
            *state = if *size == 0 {
                Chunked::Trailer {
                    at_line_start: true,
                    seen_cr: false,
                }
            } else {
                Chunked::Data { remaining: *size }
            };
            Ok(())
        }
        Chunked::Data { .. } => Ok(()),
        Chunked::DataCr => {
            if byte != b'\r' {
                return Err(Error::ChunkFraming);
            }
            *state = Chunked::DataLf;
            Ok(())
        }
        Chunked::DataLf => {
            if byte != b'\n' {
                return Err(Error::ChunkFraming);
            }
            *state = Chunked::Size {
                value: 0,
                digits: 0,
            };
            Ok(())
        }
        Chunked::Trailer {
            at_line_start,
            seen_cr,
        } => {
            match byte {
                b'\r' => *seen_cr = true,
                b'\n' => {
                    if *at_line_start && *seen_cr {
                        // A blank line: the trailer section, and the body,
                        // are over.
                        *state = Chunked::Done;
                        return Ok(());
                    }
                    *at_line_start = true;
                    *seen_cr = false;
                }
                _ => {
                    *at_line_start = false;
                    *seen_cr = false;
                }
            }
            Ok(())
        }
        Chunked::Done => Ok(()),
    }
}

/// The blank line between the headers and the body.
const HEADER_TERMINATOR: &[u8] = b"\r\n\r\n";

/// Where the header block ends, searching from `from` so that a terminator
/// spanning two reads is still found without rescanning everything.
fn find_header_end(buffer: &[u8], from: usize) -> Option<usize> {
    buffer
        .get(from..)?
        .windows(HEADER_TERMINATOR.len())
        .position(|window| window == HEADER_TERMINATOR)
        .map(|offset| from + offset)
}

/// Reads the headers this understands out of the raw block.
///
/// Unknown headers are skipped without complaint, and a header whose value
/// does not parse is treated as absent rather than as an error: the
/// alternative is refusing a page over a malformed `Content-Type`, which
/// helps nobody.
fn parse_head(raw: Vec<u8>) -> Result<Head, Error> {
    let mut head = Head {
        status: parse_status(&raw),
        content_length: None,
        chunked: false,
        identity_encoding: true,
        location: None,
        media_type: None,
        charset: None,
        no_store: false,
        etag: None,
        vary_blocks_cache: false,
        no_cache: false,
        max_age: None,
        age: None,
        date: None,
        expires: None,
        raw: Vec::new(),
    };
    for line in raw.split(|&byte| byte == b'\n').skip(1) {
        let line = trim_ascii(line);
        let Some(colon) = line.iter().position(|&byte| byte == b':') else {
            continue;
        };
        let name = trim_ascii(&line[..colon]);
        let value = trim_ascii(&line[colon + 1..]);
        if name.eq_ignore_ascii_case(b"content-length") {
            head.content_length = parse_u64(value);
        } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
            // Any transfer coding that is not `identity` means chunked here:
            // `chunked` is the only one HTTP/1.1 requires, and a coding this
            // does not know would be a body it cannot frame either way.
            head.chunked = !value.eq_ignore_ascii_case(b"identity");
        } else if name.eq_ignore_ascii_case(b"content-encoding") {
            head.identity_encoding = value.is_empty() || value.eq_ignore_ascii_case(b"identity");
        } else if name.eq_ignore_ascii_case(b"location") {
            head.location = Some(owned(value)?);
        } else if name.eq_ignore_ascii_case(b"etag") {
            head.etag = (!value.is_empty()
                && value.len() <= 256
                && value.iter().all(|byte| (0x21..=0x7e).contains(byte)))
            .then(|| owned(value))
            .transpose()?;
        } else if name.eq_ignore_ascii_case(b"vary") {
            head.vary_blocks_cache |= value.split(|&byte| byte == b',').any(|field| {
                let field = trim_ascii(field);
                !field.is_empty() && !field.eq_ignore_ascii_case(b"accept-encoding")
            });
        } else if name.eq_ignore_ascii_case(b"cache-control") {
            // Repeated headers accumulate: any `no-store` or `no-cache` wins,
            // and the smallest `max-age` is the one that holds.
            for directive in value.split(|&byte| byte == b',') {
                let directive = trim_ascii(directive);
                let (name, argument) = match directive.iter().position(|&byte| byte == b'=') {
                    Some(equals) => (
                        trim_ascii(&directive[..equals]),
                        Some(trim_ascii(&directive[equals + 1..])),
                    ),
                    None => (directive, None),
                };
                if name.eq_ignore_ascii_case(b"no-store") {
                    head.no_store = true;
                } else if name.eq_ignore_ascii_case(b"no-cache") {
                    head.no_cache = true;
                } else if name.eq_ignore_ascii_case(b"max-age") {
                    let argument = argument.map(|argument| {
                        argument
                            .strip_prefix(b"\"")
                            .and_then(|inner| inner.strip_suffix(b"\""))
                            .unwrap_or(argument)
                    });
                    let seconds = argument.and_then(parse_u64).unwrap_or(0);
                    head.max_age = Some(head.max_age.map_or(seconds, |old| old.min(seconds)));
                }
            }
        } else if name.eq_ignore_ascii_case(b"age") {
            head.age = parse_u64(value);
        } else if name.eq_ignore_ascii_case(b"date") {
            head.date = (value.len() <= 64).then(|| owned(value)).transpose()?;
        } else if name.eq_ignore_ascii_case(b"expires") {
            head.expires = Some(if value.len() <= 64 {
                owned(value)?
            } else {
                Vec::new()
            });
        } else if name.eq_ignore_ascii_case(b"content-type") {
            let (media, charset) = split_content_type(value);
            head.media_type = Some(lowercased(media)?);
            head.charset = match charset {
                Some(charset) => Some(lowercased(charset)?),
                None => None,
            };
        }
    }
    head.raw = raw;
    Ok(head)
}

fn split_content_type(value: &[u8]) -> (&[u8], Option<&[u8]>) {
    let mut parts = value.split(|&byte| byte == b';');
    let media = trim_ascii(parts.next().unwrap_or(b""));
    for parameter in parts {
        let parameter = trim_ascii(parameter);
        let Some(equals) = parameter.iter().position(|&byte| byte == b'=') else {
            continue;
        };
        if trim_ascii(&parameter[..equals]).eq_ignore_ascii_case(b"charset") {
            let mut charset = trim_ascii(&parameter[equals + 1..]);
            if charset.len() >= 2 && charset[0] == b'"' && charset[charset.len() - 1] == b'"' {
                charset = &charset[1..charset.len() - 1];
            }
            return (media, Some(charset));
        }
    }
    (media, None)
}

fn trim_ascii(value: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = value.len();
    while start < end && (value[start] == b' ' || value[start] == b'\t' || value[start] == b'\r') {
        start += 1;
    }
    while end > start
        && (value[end - 1] == b' ' || value[end - 1] == b'\t' || value[end - 1] == b'\r')
    {
        end -= 1;
    }
    &value[start..end]
}

fn owned(value: &[u8]) -> Result<Vec<u8>, Error> {
    let mut copy = Vec::new();
    memory::extend_from_slice(&mut copy, value)?;
    Ok(copy)
}

fn lowercased(value: &[u8]) -> Result<Vec<u8>, Error> {
    let mut copy = owned(value)?;
    copy.make_ascii_lowercase();
    Ok(copy)
}

fn parse_u64(value: &[u8]) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    for &byte in value {
        if !byte.is_ascii_digit() {
            return None;
        }
        total = total.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
    }
    Some(total)
}

/// The numeric status out of `HTTP/1.x NNN Reason`.
///
/// `None` when the status line is not that shape, which is reported as-is
/// rather than guessed at: a reply that does not start with a status line is
/// not an HTTP response, and calling it 200 would be worse than saying so.
fn parse_status(headers: &[u8]) -> Option<u16> {
    let line = headers.split(|&byte| byte == b'\n').next()?;
    let mut fields = line.split(|&byte| byte == b' ');
    let version = fields.next()?;
    if !version.starts_with(b"HTTP/") {
        return None;
    }
    let code = fields.next()?;
    if code.len() != 3 || !code.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(
        code.iter()
            .fold(0u16, |value, byte| value * 10 + u16::from(byte - b'0')),
    )
}

/// HTTP/1.0 with an explicit `Host`, which every virtual host needs and
/// costs nothing to send. 1.0 rather than 1.1 so the server closes the
/// connection at the end of the body instead of leaving it open.
///
/// `Accept-Encoding: identity` is stated rather than left out: the default
/// for an absent header is "anything", and a server that took that up on a
/// gzip would be sending a body this has no decompressor for.
///
/// `User-Agent` is sent because a request without one is refused outright by
/// a good deal of the web -- measured on 2026-08-28, `en.wikipedia.org` and
/// `stackoverflow.com` both answer 403 to this request with the header
/// removed and 200 with it present, and adding `Accept` instead changes
/// nothing.
///
/// What it says is what this is. Claiming to be Firefox was tried and is
/// both dishonest and *worse*: `reddit.com` answers 403 to a bare
/// `Mozilla/5.0` and 200 to [`USER_AGENT`]. An operator who wants to know
/// what is fetching their pages can find out, and that turns out to be the
/// combination the web actually rewards.
///
/// The target and the host come from a `browser::url::Url`, which has
/// already refused CR, LF, space and every control character -- so nothing
/// here can add a line to the request.
fn build_request(
    method: Method,
    body: &[u8],
    host: &[u8],
    target: &[u8],
    if_none_match: Option<&[u8]>,
) -> Result<Vec<u8>, Error> {
    request::encode_http10_parts(
        method,
        body,
        host,
        target,
        USER_AGENT.as_bytes(),
        if_none_match,
    )
    .map_err(|error| match error {
        request::Error::TooLong | request::Error::InvalidHeadValue => Error::InvalidRequest,
        request::Error::OutOfMemory => Error::OutOfMemory,
    })
}

/// What this firmware calls itself to a server.
///
/// The version comes from `Cargo.toml` so the two cannot drift apart. No URL
/// and no imitation of a browser: see [`build_request`].
pub const USER_AGENT: &str = concat!("tab5-browser/", env!("CARGO_PKG_VERSION"));

// ---------------------------------------------------------------------------
// The blocking call.
// ---------------------------------------------------------------------------

pub struct Response {
    /// The status line and headers, without the blank line that ends them.
    pub headers: Vec<u8>,
    /// The status code, if the status line was shaped like one.
    ///
    /// Reported rather than acted on: whether a 404's body is worth keeping
    /// is the caller's question, and this layer handing the body to the sink
    /// either way keeps the decision in one place.
    pub status: Option<u16>,
    /// Body bytes handed to the sink.
    pub body_bytes: usize,
    /// Everything received, headers included.
    pub received: usize,
    pub elapsed_ms: u64,
}

/// Issues `GET <path>` against `address:port`, keeping the headers and
/// handing the body to `sink` as it arrives.
///
/// This is the shell's `httpget`: it blocks until the transfer is over,
/// which is fine for a command that owns the console while it runs and
/// wrong for anything with a frame loop. [`Transaction`] is the same
/// exchange for callers that cannot block.
///
/// `bare_host` is the host *without* a port: this builds the `Host:` header
/// from it and `port`. That is the opposite of [`Transaction::start`], which
/// takes the finished header value -- so a caller with a
/// [`Url`](crate::browser::url::Url) passes `host()` here and `host_header()`
/// there. Passing the wrong one produces `Host: name:8080:8080`, which most
/// servers ignore and which is therefore invisible until something reads it.
///
/// It is not always `address` written out: when the destination was given as
/// a name, the name is what the server needs to pick a virtual host, and the
/// address it resolved to tells it nothing.
pub fn get(
    stack: &mut Stack,
    rpc: &mut Rpc,
    address: Ipv4Address,
    port: u16,
    bare_host: &[u8],
    path: &[u8],
    security: Security<'_>,
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<Response, Error> {
    // The `Host:` header carries the port when it is not the default, the
    // same rule the browser's `Url::host_header` applies. Which port counts
    // as default depends on the scheme, so it comes from the transport
    // choice rather than from a constant.
    let default_port = match security {
        Security::Plain => 80,
        Security::Tls { .. } => 443,
    };
    let host_header = match port {
        port if port == default_port => owned(bare_host)?,
        port => {
            let mut value = owned(bare_host)?;
            value.push(b':');
            push_decimal(&mut value, port as u32);
            value
        }
    };
    // No body limit: `httpget` saves what it is given, and its 512 KiB
    // regression download is larger than the browser's page bound.
    let mut transaction =
        Transaction::start(stack, address, port, &host_header, path, u64::MAX, security)?;
    loop {
        match transaction.poll(stack, rpc, DEFAULT_POLL_BUDGET, sink) {
            Progress::Complete => break,
            Progress::Failed(error) => {
                transaction.close(stack, rpc);
                return Err(error);
            }
            _ => {}
        }
    }
    let head = transaction.take_head();
    let stats = transaction.close(stack, rpc);
    let (headers, status) = match head {
        Some(head) => (head.raw, head.status),
        None => (Vec::new(), None),
    };
    Ok(Response {
        headers,
        status,
        body_bytes: stats.body_bytes as usize,
        received: stats.received,
        elapsed_ms: stats.elapsed_ms,
    })
}

fn push_decimal(out: &mut Vec<u8>, value: u32) {
    if value >= 10 {
        push_decimal(out, value / 10);
    }
    out.push(b'0' + (value % 10) as u8);
}
