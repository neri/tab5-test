//! The byte stream an HTTP exchange runs over, plaintext or TLS.
//!
//! There is one HTTP implementation in this firmware and there is going to
//! stay one. A second copy of the status-line parser, the chunked decoder
//! and the redirect rules -- written for HTTPS because the first copy talked
//! to a socket directly -- is two places for the two of them to disagree,
//! and the ones that matter most are the ones about where a body ends.
//!
//! So [`Transaction`](crate::net::http::Transaction) does not touch a socket
//! any more. It drives whichever [`Transport`] it was given, and what it
//! gets back is bytes:
//!
//! ```text
//!                    +-- Transport::Plain -- TCP socket
//! HTTP Transaction --+
//!                    +-- Transport::Tls ---- net::tls::Transaction -- TCP socket
//! ```
//!
//! The two are not the same shape underneath. `Plain` is a socket handle and
//! nothing else: reading is `recv_slice`, and the peer closing is
//! `may_recv` going false. `Tls` is a whole state machine with its own
//! future, its own record buffers and a handshake that has to finish before
//! a single request byte may be written. What this file does is make those
//! answer the same four questions -- is it ready, take these bytes, give me
//! bytes, is it over -- so that the layer above does not have to know which
//! one it has.
//!
//! What deliberately does *not* get flattened away is the authentication
//! state. [`Transport::authentication`] answers `None` for a plaintext
//! connection and `Some` for a TLS one, and a caller that wants to display
//! something about security has to look at it rather than infer it from the
//! scheme it asked for.

use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

use crate::net::Stack;
use crate::net::tls;
use crate::wifi::Rpc;
use crate::{delay, tick, uart};

/// Socket buffers for the plaintext path. 8 KiB each is more than the link
/// can fill between two polls and still nothing next to the PSRAM heap.
const BUFFER_BYTES: usize = 8192;

const CONNECT_TIMEOUT_MS: u64 = 5000;
/// How long a partly-written request may stall before it is called dead.
const SEND_TIMEOUT_MS: u64 = 5000;
/// How long the abort is pumped for so the RST actually leaves.
const ABORT_PUMP_MS: u64 = 20;

/// What to run the exchange over.
///
/// Chosen by the caller, from the URL's scheme, and never inferred further
/// down: a transport that could quietly decide to be plaintext is a
/// transport that can downgrade.
pub enum Security<'a> {
    Plain,
    Tls {
        /// The DNS name from the URL. It goes in SNI, and it is what pins
        /// are registered under -- not the address, which tells a virtual
        /// host nothing and identifies nobody.
        server_name: &'a str,
        policy: tls::PinPolicy,
    },
}

/// Why the byte stream stopped, in the vocabulary the layers above already
/// use.
///
/// The names are the ones `net::http::error_name` has always produced for
/// the plaintext cases, because the browser fixtures match on them and a
/// refactor that renames a failure is a refactor that breaks a test for no
/// reason. TLS failures keep their own names, which are finer-grained.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Error {
    LinkLost,
    /// The connection was refused or never completed.
    NotConnected,
    Cancelled,
    OutOfMemory,
    Local,
    Tls(tls::Error),
}

impl Error {
    pub fn name(self) -> &'static str {
        match self {
            Self::LinkLost => "link-lost",
            Self::NotConnected => "not-connected",
            Self::Cancelled => "cancelled",
            Self::OutOfMemory => "out-of-memory",
            Self::Local => "local",
            Self::Tls(error) => error.name(),
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::LinkLost => "the C6 link was lost during the transfer",
            Self::NotConnected => "the connection was refused or timed out",
            Self::Cancelled => "cancelled",
            Self::OutOfMemory => "out of memory during the transfer",
            Self::Local => "the transfer failed locally",
            Self::Tls(error) => error.message(),
        }
    }
}

impl From<tls::Error> for Error {
    fn from(error: tls::Error) -> Error {
        match error {
            // The three the plaintext path also has. Reporting them under
            // the TLS names would make an ordinary link loss look like a
            // cryptographic failure.
            tls::Error::LinkLost => Error::LinkLost,
            tls::Error::Cancelled => Error::Cancelled,
            tls::Error::OutOfMemory => Error::OutOfMemory,
            error => Error::Tls(error),
        }
    }
}

/// What one [`Transport::poll`] did.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Progress {
    /// Not usable yet: TCP is connecting, or a TLS handshake is running.
    Connecting,
    /// The request may be written and the response read.
    Ready,
    Failed(Error),
}

/// A plaintext TCP stream.
pub struct Plain {
    handle: SocketHandle,
    connected: bool,
    /// The socket could not even be told to connect. Carried as state
    /// rather than returned, because the socket is already in the set and
    /// only `close` can take it out again.
    local_failure: bool,
    started_ms: u64,
}

#[must_use = "a Transport owns a socket and has to be closed"]
pub enum Transport {
    Plain(Plain),
    Tls(tls::Transaction),
}

impl Transport {
    /// Opens the connection. Nothing has been sent when this returns; the
    /// caller polls until [`Progress::Ready`].
    pub fn connect(
        stack: &mut Stack,
        address: Ipv4Address,
        port: u16,
        security: Security<'_>,
    ) -> Result<Transport, Error> {
        match security {
            Security::Plain => Ok(Transport::Plain(Plain::connect(stack, address, port))),
            Security::Tls {
                server_name,
                policy,
            } => {
                let transaction =
                    tls::Transaction::start(stack, address, port, server_name, policy)?;
                Ok(Transport::Tls(transaction))
            }
        }
    }

    /// Moves the transport forward by at most `budget` bytes off the socket.
    pub fn poll(&mut self, stack: &mut Stack, rpc: &mut Rpc, budget: usize) -> Progress {
        match self {
            Transport::Plain(plain) => plain.poll(stack, rpc),
            Transport::Tls(transaction) => {
                match transaction.poll(stack, rpc, budget) {
                    tls::Progress::Failed(error) => Progress::Failed(error.into()),
                    tls::Progress::Handshaking => Progress::Connecting,
                    // Everything else means the session is up: `Complete`
                    // included, because a stream that has already ended is
                    // one whose remaining plaintext still has to be read
                    // out. Whether *that* is a complete document is the
                    // HTTP framing's answer, not this layer's.
                    _ => Progress::Ready,
                }
            }
        }
    }

    /// Takes as much of `bytes` as the transport will hold, and says how
    /// much that was.
    ///
    /// Partial for the plaintext path -- an 8 KiB send buffer and a request
    /// with a long target do not always fit in one call -- so the caller
    /// carries an index and comes back.
    pub fn write(&mut self, stack: &mut Stack, bytes: &[u8]) -> Result<usize, Error> {
        match self {
            Transport::Plain(plain) => stack
                .sockets_mut()
                .get_mut::<tcp::Socket>(plain.handle)
                .send_slice(bytes)
                .map_err(|_| Error::Local),
            Transport::Tls(transaction) => {
                transaction.write(bytes)?;
                Ok(bytes.len())
            }
        }
    }

    /// Says the request is complete.
    ///
    /// Nothing for the plaintext path: the request ends where the blank line
    /// ends it, and the socket keeps carrying bytes either way. The TLS
    /// path needs telling, because its engine is a sequence -- write the
    /// request, then read the response -- and it has no other way to know
    /// the writing is over.
    pub fn finish_request(&mut self) {
        if let Transport::Tls(transaction) = self {
            transaction.finish_request();
        }
    }

    pub fn read(&mut self, stack: &mut Stack, buffer: &mut [u8]) -> usize {
        match self {
            Transport::Plain(plain) => stack
                .sockets_mut()
                .get_mut::<tcp::Socket>(plain.handle)
                .recv_slice(buffer)
                .unwrap_or(0),
            Transport::Tls(transaction) => transaction.read(buffer),
        }
    }

    /// Whether the peer has finished and nothing more will arrive.
    ///
    /// Only ever asked once a read has come back empty, which is why it can
    /// be this blunt: for TCP, the FIN has arrived and the buffer is drained;
    /// for TLS, the engine has stopped and its plaintext is spent.
    pub fn at_end(&self, stack: &mut Stack) -> bool {
        match self {
            Transport::Plain(plain) => !stack
                .sockets_mut()
                .get_mut::<tcp::Socket>(plain.handle)
                .may_recv(),
            Transport::Tls(transaction) => {
                transaction.is_finished() && transaction.available() == 0
            }
        }
    }

    /// What the connection proved about who is on the other end, or `None`
    /// for plaintext.
    ///
    /// `None` is not "insecure by default": it is "this question does not
    /// apply", and a caller displaying anything about security has to
    /// handle it rather than reading a missing answer as a reassuring one.
    pub fn authentication(&self) -> Option<tls::Authentication> {
        match self {
            Transport::Plain(_) => None,
            Transport::Tls(transaction) => transaction.authentication(),
        }
    }

    /// Whether the stream ended without the peer saying so in-band.
    ///
    /// Always false for plaintext, where a FIN is all there ever was. For
    /// TLS it means the TCP connection closed with no `close_notify`, so
    /// nothing cryptographic vouches that the end was not cut off -- the
    /// HTTP framing has to.
    pub fn closed_without_notify(&self) -> bool {
        match self {
            Transport::Plain(_) => false,
            Transport::Tls(transaction) => transaction.closed_without_notify(),
        }
    }

    /// Stops the transport without giving up the socket, so that a caller
    /// that has decided to stop can release the engine's buffers before it
    /// gets round to closing.
    pub fn cancel(&mut self) {
        if let Transport::Tls(transaction) = self {
            transaction.cancel();
        }
    }

    /// Aborts the connection and takes the socket out of the set.
    ///
    /// `abort` rather than a graceful shutdown: the socket is going away
    /// with the handle, so there is nobody left to finish one. The short
    /// pump afterwards is what actually puts the RST on the wire -- without
    /// it the peer is left holding a connection that no longer exists at
    /// this end, which after a hundred cancellations is a hundred sockets on
    /// the other machine.
    pub fn close(self, stack: &mut Stack, rpc: &mut Rpc) {
        match self {
            Transport::Plain(plain) => {
                stack
                    .sockets_mut()
                    .get_mut::<tcp::Socket>(plain.handle)
                    .abort();
                stack.pump_until(rpc, ABORT_PUMP_MS, |_| false);
                stack.sockets_mut().remove(plain.handle);
            }
            Transport::Tls(transaction) => {
                transaction.close(stack, rpc);
            }
        }
    }

    /// Counters the TLS path keeps and the plaintext one has no equivalent
    /// for. `None` rather than zeroes: a plaintext connection did not take
    /// zero milliseconds to hand-shake, it did not hand-shake.
    pub fn tls_stats(&self) -> Option<tls::Stats> {
        match self {
            Transport::Plain(_) => None,
            Transport::Tls(transaction) => Some(transaction.stats()),
        }
    }
}

impl Plain {
    fn connect(stack: &mut Stack, address: Ipv4Address, port: u16) -> Plain {
        let handle = stack.sockets_mut().add(tcp::Socket::new(
            tcp::SocketBuffer::new(alloc::vec![0u8; BUFFER_BYTES]),
            tcp::SocketBuffer::new(alloc::vec![0u8; BUFFER_BYTES]),
        ));
        let mut plain = Plain {
            handle,
            connected: false,
            local_failure: false,
            started_ms: tick::now_ms(),
        };
        let local_port = 49152 + (delay::cycle_count() % 16384) as u16;
        let remote = IpEndpoint::new(IpAddress::Ipv4(address), port);
        if stack.connect_tcp(handle, remote, local_port).is_err() {
            plain.local_failure = true;
        }
        plain
    }

    fn poll(&mut self, stack: &mut Stack, rpc: &mut Rpc) -> Progress {
        if self.local_failure {
            // Not `NotConnected`: nothing was ever put on the wire for the
            // peer to refuse, so this is this end's failure and says so.
            return Progress::Failed(Error::Local);
        }
        if !stack.poll(rpc) {
            return Progress::Failed(Error::LinkLost);
        }
        if self.connected {
            return Progress::Ready;
        }
        if stack
            .sockets_mut()
            .get_mut::<tcp::Socket>(self.handle)
            .may_send()
        {
            self.connected = true;
            return Progress::Ready;
        }
        if tick::now_ms().saturating_sub(self.started_ms) > CONNECT_TIMEOUT_MS {
            return Progress::Failed(Error::NotConnected);
        }
        Progress::Connecting
    }
}

/// How long a stalled write may go on before the exchange is called dead.
///
/// Exposed because the layer above owns the timer: it is the one that knows
/// whether nothing moving means "the request is stuck" or "the server is
/// thinking about the response".
pub const WRITE_STALL_MS: u64 = SEND_TIMEOUT_MS;

/// Says on the UART that a transport went away without being closed.
///
/// Called from the owner's `Drop`, because nothing can be done about it from
/// there: removing the socket needs the stack, and the stack is not
/// reachable. Saying so is the whole of the remedy, and it turns a silent
/// leak into a line in the log.
pub fn report_unclosed(owner: &[u8]) {
    uart::log(owner);
    uart::log(b": a transaction was dropped without close()\r\n");
}
