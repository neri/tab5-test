//! A minimal HTTP/1.0 GET, present because TCP came with smoltcp.
//!
//! This is a TCP smoke test wearing a familiar shape, not an HTTP client:
//! there is no redirect handling, no chunked decoding, no TLS and no name
//! resolution. What it proves is that a connection opens, data flows in
//! both directions and the close is seen -- which is the part of TCP that
//! is hard to get right and easy to check.

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

/// How much of the response is kept. The point is to look at the status
/// line and the first headers, not to hold a page.
pub const MAX_RESPONSE_BYTES: usize = 4096;

pub enum Error {
    LinkLost,
    /// The connection was refused or never completed.
    NotConnected,
    /// Connected, but the peer stopped talking mid-response.
    TimedOut,
    Local,
}

pub struct Response {
    pub body: Vec<u8>,
    /// Total bytes received, which may exceed what `body` kept.
    pub received: usize,
    pub elapsed_ms: u64,
}

/// Issues `GET <path>` against `address:port` and returns the start of the
/// response, headers included.
pub fn get(
    stack: &mut Stack,
    rpc: &mut Rpc,
    address: Ipv4Address,
    port: u16,
    path: &[u8],
) -> Result<Response, Error> {
    let handle = stack.sockets_mut().add(tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0u8; BUFFER_BYTES]),
        tcp::SocketBuffer::new(vec![0u8; BUFFER_BYTES]),
    ));

    let result = exchange(stack, rpc, handle, address, port, path);

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
    path: &[u8],
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

    let request = build_request(address, port, path);
    if stack
        .sockets_mut()
        .get_mut::<tcp::Socket>(handle)
        .send_slice(&request)
        .is_err()
    {
        return Err(Error::Local);
    }

    let mut body = Vec::new();
    let mut received = 0usize;
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
                let room = MAX_RESPONSE_BYTES.saturating_sub(body.len());
                if room > 0 {
                    body.extend_from_slice(&chunk[..count.min(room)]);
                }
                grew = true;
            }
            false
        });

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

    Ok(Response {
        body,
        received,
        elapsed_ms: tick::now_ms().saturating_sub(started),
    })
}

/// HTTP/1.0 with an explicit `Host`, which every virtual host needs and
/// costs nothing to send. 1.0 rather than 1.1 so the server closes the
/// connection at the end of the body instead of leaving it open.
fn build_request(address: Ipv4Address, port: u16, path: &[u8]) -> Vec<u8> {
    let mut request = Vec::with_capacity(path.len() + 64);
    request.extend_from_slice(b"GET ");
    request.extend_from_slice(path);
    request.extend_from_slice(b" HTTP/1.0\r\nHost: ");
    for (index, octet) in address.octets().iter().enumerate() {
        if index != 0 {
            request.push(b'.');
        }
        push_decimal(&mut request, *octet as u32);
    }
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
