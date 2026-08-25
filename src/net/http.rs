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
//! it cannot. No cookies, no authentication, no keep-alive, no TLS.
//!
//! Names are resolved before they get here: the caller passes both the
//! address to connect to and the text to put in `Host:`.

use alloc::vec;
use alloc::vec::Vec;

use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

use crate::browser::memory::{self, OutOfMemory};
use crate::net::Stack;
use crate::wifi::Rpc;
use crate::{delay, tick, uart};

/// Socket buffers. 8 KiB each is more than the link can fill between two
/// polls and still nothing next to the PSRAM heap.
const BUFFER_BYTES: usize = 8192;

const CONNECT_TIMEOUT_MS: u64 = 5000;
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
    LinkLost,
    /// The connection was refused or never completed.
    NotConnected,
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
    Local,
}

impl From<OutOfMemory> for Error {
    fn from(_: OutOfMemory) -> Self {
        Error::OutOfMemory
    }
}

/// A short ASCII sentence, for the console and the browser's status line.
pub fn error_text(error: Error) -> &'static str {
    match error {
        Error::LinkLost => "the C6 link was lost during the transfer",
        Error::NotConnected => "the connection was refused or timed out",
        Error::TimedOut => "the server stopped responding",
        Error::HeadersTooLong => "the response headers never ended",
        Error::Truncated => "the connection closed before the page finished",
        Error::ChunkFraming => "the chunked response was malformed",
        Error::BodyTooLong => "the page is larger than this can hold",
        Error::UnsupportedEncoding => "the response is compressed, which is not supported",
        Error::SinkRefused => "the transfer was abandoned",
        Error::Cancelled => "cancelled",
        Error::OutOfMemory => "out of memory during the transfer",
        Error::Local => "a local socket operation failed",
    }
}

/// A short name, for one-line UART statistics.
pub fn error_name(error: Error) -> &'static str {
    match error {
        Error::LinkLost => "link-lost",
        Error::NotConnected => "not-connected",
        Error::TimedOut => "timed-out",
        Error::HeadersTooLong => "header-limit",
        Error::Truncated => "truncated",
        Error::ChunkFraming => "chunk",
        Error::BodyTooLong => "body-limit",
        Error::UnsupportedEncoding => "encoding",
        Error::SinkRefused => "sink-refused",
        Error::Cancelled => "cancelled",
        Error::OutOfMemory => "out-of-memory",
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
/// Owns a socket handle in the caller's `SocketSet` and nothing else --
/// specifically not the [`Stack`] or the [`Rpc`], which are borrowed for
/// the length of a single [`Transaction::poll`] and given back. That is
/// what lets the browser's frame loop keep the stack for its own polling
/// while a fetch is running.
///
/// It must be handed to [`Transaction::close`]. Dropping it instead leaks
/// the socket out of the set, and since the set is `'static` that socket
/// is gone for the run; the `Drop` below says so on the UART rather than
/// letting it be silent.
#[must_use = "a Transaction owns a socket and has to be closed"]
pub struct Transaction {
    handle: SocketHandle,
    state: State,
    /// The request, and how much of it has been handed to the socket.
    /// Sending can be partial: the send buffer is 8 KiB but a request with
    /// a long target is still a request that might not fit in one call.
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
    /// `host` is what goes in the `Host:` header, and is not always
    /// `address` written out: when the destination was given as a name,
    /// the name is what the server needs to pick a virtual host, and the
    /// address it resolved to tells it nothing.
    ///
    /// `max_body` bounds the decoded body. A transfer that reaches it fails
    /// with [`Error::BodyTooLong`] rather than delivering a prefix -- and a
    /// `Content-Length` past it fails before the body is read at all.
    pub fn start(
        stack: &mut Stack,
        address: Ipv4Address,
        port: u16,
        host: &[u8],
        target: &[u8],
        max_body: u64,
    ) -> Result<Transaction, Error> {
        let request = build_request(host, target)?;
        let handle = stack.sockets_mut().add(tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0u8; BUFFER_BYTES]),
            tcp::SocketBuffer::new(vec![0u8; BUFFER_BYTES]),
        ));
        let now = tick::now_ms();
        let mut transaction = Transaction {
            handle,
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
        };
        let local_port = 49152 + (delay::cycle_count() % 16384) as u16;
        let remote = IpEndpoint::new(IpAddress::Ipv4(address), port);
        if stack.connect_tcp(handle, remote, local_port).is_err() {
            // The socket is in the set and has to come out of it, which is
            // the caller's job through `close` -- so report the failure as
            // state rather than as an early return that drops the handle.
            transaction.state = State::Failed(Error::Local);
        }
        Ok(transaction)
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
        if !stack.poll(rpc) {
            return self.fail(Error::LinkLost);
        }

        match self.state {
            State::Connecting => self.poll_connecting(stack),
            State::Sending => self.poll_sending(stack),
            State::Head | State::Body(_) => self.poll_receiving(stack, budget, sink),
            State::Complete => Progress::Complete,
            State::Failed(error) => Progress::Failed(error),
        }
    }

    fn poll_connecting(&mut self, stack: &mut Stack) -> Progress {
        if stack
            .sockets_mut()
            .get_mut::<tcp::Socket>(self.handle)
            .may_send()
        {
            self.state = State::Sending;
            self.last_progress_ms = tick::now_ms();
            return Progress::Connecting;
        }
        if tick::now_ms().saturating_sub(self.started_ms) > CONNECT_TIMEOUT_MS {
            return self.fail(Error::NotConnected);
        }
        Progress::Connecting
    }

    fn poll_sending(&mut self, stack: &mut Stack) -> Progress {
        let socket = stack.sockets_mut().get_mut::<tcp::Socket>(self.handle);
        match socket.send_slice(&self.request[self.sent..]) {
            Ok(count) => {
                self.sent += count;
                if count > 0 {
                    self.last_progress_ms = tick::now_ms();
                }
            }
            Err(_) => return self.fail(Error::Local),
        }
        if self.sent >= self.request.len() {
            self.state = State::Head;
        } else if tick::now_ms().saturating_sub(self.last_progress_ms) > IDLE_TIMEOUT_MS {
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
            let socket = stack.sockets_mut().get_mut::<tcp::Socket>(self.handle);
            let count = match socket.recv_slice(&mut chunk[..want]) {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
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

        // Nothing arrived. Either the peer has closed, or it is quiet.
        let socket = stack.sockets_mut().get_mut::<tcp::Socket>(self.handle);
        // `may_recv` goes false once the peer has sent its FIN and the
        // receive buffer is drained.
        if !socket.may_recv() {
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
            _ => self.fail(Error::NotConnected),
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
        let done = matches!(
            &self.state,
            State::Body(Framing::Chunked(Chunked::Done))
        );
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
            self.state = State::Failed(Error::Cancelled);
            self.stats.elapsed_ms = tick::now_ms().saturating_sub(self.started_ms);
        }
    }

    /// Aborts the connection and takes the socket out of the set.
    ///
    /// `abort` rather than `close`: the socket is going away with the
    /// handle, so there is nobody left to finish a graceful shutdown. The
    /// short pump afterwards is what actually puts the RST on the wire --
    /// without it the peer is left holding a connection that no longer
    /// exists at this end, which after a hundred cancellations is a hundred
    /// sockets on the other machine.
    pub fn close(mut self, stack: &mut Stack, rpc: &mut Rpc) -> Stats {
        stack
            .sockets_mut()
            .get_mut::<tcp::Socket>(self.handle)
            .abort();
        stack.pump_until(rpc, ABORT_PUMP_MS, |_| false);
        stack.sockets_mut().remove(self.handle);
        self.closed = true;
        if self.stats.elapsed_ms == 0 {
            self.stats.elapsed_ms = tick::now_ms().saturating_sub(self.started_ms);
        }
        self.stats
    }
}

/// How long the abort is pumped for so the RST actually leaves.
const ABORT_PUMP_MS: u64 = 20;

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.closed {
            // Nothing can be done about it from here: removing the socket
            // needs the stack, and the stack is not reachable from a
            // `Drop`. Saying so is the whole of the remedy, and it turns a
            // silent leak into a line in the log.
            uart::log(b"HTTP: a transaction was dropped without close()\r\n");
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
            head.identity_encoding =
                value.is_empty() || value.eq_ignore_ascii_case(b"identity");
        } else if name.eq_ignore_ascii_case(b"location") {
            head.location = Some(owned(value)?);
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
/// The target and the host come from a `browser::url::Url`, which has
/// already refused CR, LF, space and every control character -- so nothing
/// here can add a line to the request.
fn build_request(host: &[u8], target: &[u8]) -> Result<Vec<u8>, Error> {
    let mut request = Vec::new();
    let length = target.len() + host.len() + 96;
    request.try_reserve_exact(length).map_err(|_| Error::OutOfMemory)?;
    request.extend_from_slice(b"GET ");
    request.extend_from_slice(target);
    request.extend_from_slice(b" HTTP/1.0\r\nHost: ");
    request.extend_from_slice(host);
    request.extend_from_slice(b"\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n");
    Ok(request)
}

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
/// `host` is what goes in the `Host:` header, and is not always `address`
/// written out: when the destination was given as a name, the name is what
/// the server needs to pick a virtual host, and the address it resolved to
/// tells it nothing.
pub fn get(
    stack: &mut Stack,
    rpc: &mut Rpc,
    address: Ipv4Address,
    port: u16,
    host: &[u8],
    path: &[u8],
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<Response, Error> {
    // The `Host:` header carries the port when it is not the default, the
    // same rule the browser's `Url::host_header` applies.
    let host_header = match port {
        80 => owned(host)?,
        port => {
            let mut value = owned(host)?;
            value.push(b':');
            push_decimal(&mut value, port as u32);
            value
        }
    };
    // No body limit: `httpget` saves what it is given, and its 512 KiB
    // regression download is larger than the browser's page bound.
    let mut transaction =
        Transaction::start(stack, address, port, &host_header, path, u64::MAX)?;
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
