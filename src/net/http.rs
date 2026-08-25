//! A minimal HTTP/1.0 GET, present because TCP came with smoltcp.
//!
//! This is a TCP smoke test wearing a familiar shape, not an HTTP client:
//! there is no redirect handling, no chunked decoding and no TLS. What it
//! proves is that a connection opens, data flows in both directions and
//! the close is seen -- which is the part of TCP that is hard to get right
//! and easy to check.
//!
//! Names are resolved before they get here: the caller passes both the
//! address to connect to and the text to put in `Host:`.

use alloc::vec;
use alloc::vec::Vec;

use smoltcp::socket::tcp;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

use crate::net::Stack;
use crate::wifi::Rpc;
use crate::{delay, tick};

/// Socket buffers. 8 KiB each is more than the link can fill between two
/// polls and still nothing next to the PSRAM heap.
const BUFFER_BYTES: usize = 8192;

const CONNECT_TIMEOUT_MS: u64 = 5000;
/// How long the transfer may stall before it is called dead.
const IDLE_TIMEOUT_MS: u64 = 5000;

/// How much of the header block is kept. Enough for a status line and the
/// headers worth looking at; a server that has not finished its headers by
/// here is not one this can work with.
pub const MAX_HEADER_BYTES: usize = 4096;

pub enum Error {
    LinkLost,
    /// The connection was refused or never completed.
    NotConnected,
    /// Connected, but the peer stopped talking mid-response.
    TimedOut,
    /// No blank line ended the headers within [`MAX_HEADER_BYTES`], so where
    /// the body starts is unknown. Everything after that would be a guess.
    HeadersTooLong,
    /// The sink would not take the body, so the transfer was abandoned.
    SinkRefused,
    Local,
}

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
/// The body is not collected. It used to be, up to a 4 KiB cap, with
/// everything past that received and dropped -- which made this a way to
/// look at a status line rather than a way to fetch anything. The split is
/// at the blank line that ends the headers, so what reaches the sink is the
/// body and nothing else.
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
    let handle = stack.sockets_mut().add(tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0u8; BUFFER_BYTES]),
        tcp::SocketBuffer::new(vec![0u8; BUFFER_BYTES]),
    ));

    let result = exchange(stack, rpc, handle, address, port, host, path, sink);

    // `abort` rather than `close`: the socket is going away with the
    // handle, so there is nobody left to finish a graceful shutdown.
    stack.sockets_mut().get_mut::<tcp::Socket>(handle).abort();
    stack.pump_until(rpc, 100, |_| false);
    stack.sockets_mut().remove(handle);
    result
}

fn exchange(
    stack: &mut Stack,
    rpc: &mut Rpc,
    handle: smoltcp::iface::SocketHandle,
    address: Ipv4Address,
    port: u16,
    host: &[u8],
    path: &[u8],
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<Response, Error> {
    let started = tick::now_ms();
    let local_port = 49152 + (delay::cycle_count() % 16384) as u16;
    let remote = IpEndpoint::new(IpAddress::Ipv4(address), port);
    if stack.connect_tcp(handle, remote, local_port).is_err() {
        return Err(Error::Local);
    }

    if !stack.pump_until(rpc, CONNECT_TIMEOUT_MS, |stack| {
        stack
            .sockets_mut()
            .get_mut::<tcp::Socket>(handle)
            .may_send()
    }) {
        return Err(if rpc.is_alive() {
            Error::NotConnected
        } else {
            Error::LinkLost
        });
    }

    let request = build_request(host, port, path);
    if stack
        .sockets_mut()
        .get_mut::<tcp::Socket>(handle)
        .send_slice(&request)
        .is_err()
    {
        return Err(Error::Local);
    }

    // Header bytes accumulate here until the blank line turns up. The
    // terminator can straddle two reads, which is why the search runs over
    // what has been collected rather than over each chunk.
    let mut headers = Vec::new();
    let mut body_started = false;
    let mut body_bytes = 0usize;
    let mut received = 0usize;
    let mut failure = None;
    let mut last_progress = tick::now_ms();
    loop {
        let mut grew = false;
        stack.pump_until(rpc, 100, |stack| {
            let socket = stack.sockets_mut().get_mut::<tcp::Socket>(handle);
            let mut chunk = [0u8; 512];
            while let Ok(count) = socket.recv_slice(&mut chunk) {
                if count == 0 {
                    break;
                }
                received += count;
                grew = true;
                let arrived = &chunk[..count];
                if !body_started {
                    let searched_from = headers.len().saturating_sub(3);
                    headers.extend_from_slice(arrived);
                    match find_header_end(&headers, searched_from) {
                        Some(end) => {
                            // Everything past the blank line in this chunk is
                            // already body.
                            let body_start = end + HEADER_TERMINATOR.len();
                            let carried = headers.split_off(body_start);
                            headers.truncate(end);
                            body_started = true;
                            // Borrowed from the chunk no longer; the split-off
                            // tail is the body's first bytes.
                            body_bytes += carried.len();
                            if !carried.is_empty() && !sink(&carried) {
                                failure = Some(Error::SinkRefused);
                                return true;
                            }
                            continue;
                        }
                        None => {
                            if headers.len() > MAX_HEADER_BYTES {
                                failure = Some(Error::HeadersTooLong);
                                return true;
                            }
                            continue;
                        }
                    }
                }
                body_bytes += arrived.len();
                if !sink(arrived) {
                    failure = Some(Error::SinkRefused);
                    return true;
                }
            }
            false
        });

        if let Some(error) = failure {
            return Err(error);
        }
        if grew {
            last_progress = tick::now_ms();
        }

        let socket = stack.sockets_mut().get_mut::<tcp::Socket>(handle);
        // `may_recv` goes false once the peer has sent its FIN and the
        // receive buffer is drained -- which for HTTP/1.0 is the end of
        // the response.
        if !socket.may_recv() && !grew {
            break;
        }
        if !rpc.is_alive() {
            return Err(Error::LinkLost);
        }
        if tick::now_ms().saturating_sub(last_progress) > IDLE_TIMEOUT_MS {
            return Err(Error::TimedOut);
        }
    }

    if !body_started {
        // The peer closed without ever finishing its headers.
        return Err(Error::HeadersTooLong);
    }

    Ok(Response {
        status: parse_status(&headers),
        headers,
        body_bytes,
        received,
        elapsed_ms: tick::now_ms().saturating_sub(started),
    })
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
fn build_request(host: &[u8], port: u16, path: &[u8]) -> Vec<u8> {
    let mut request = Vec::with_capacity(path.len() + host.len() + 64);
    request.extend_from_slice(b"GET ");
    request.extend_from_slice(path);
    request.extend_from_slice(b" HTTP/1.0\r\nHost: ");
    request.extend_from_slice(host);
    if port != 80 {
        request.push(b':');
        push_decimal(&mut request, port as u32);
    }
    request.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    request
}

fn push_decimal(out: &mut Vec<u8>, value: u32) {
    if value >= 10 {
        push_decimal(out, value / 10);
    }
    out.push(b'0' + (value % 10) as u8);
}
