//! A read-only TFTP client (RFC 1350) on one UDP socket.
//!
//! Plain TFTP only: no option extension (RFC 2347/2348), so every transfer
//! is the 512-byte lock-step of RRQ, DATA, ACK, DATA, ACK. That makes the
//! round-trip time the transfer rate, which over polled SDIO is tens to a
//! few hundred KiB/s -- slow, but the protocol is small enough to be read
//! in one sitting, which is the point at this stage.
//!
//! Two details decide whether an implementation works at all:
//!
//! - the request goes to port 69, but the server answers from a *fresh*
//!   ephemeral port and expects the rest of the conversation there. The
//!   source port of the first DATA is the real peer.
//! - the transfer ends with a DATA shorter than 512 bytes. A file whose
//!   length is an exact multiple of 512 therefore ends with an empty DATA,
//!   which is not an error and must still be acknowledged.

use alloc::vec;
use alloc::vec::Vec;

use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

use crate::net::Stack;
use crate::wifi::Rpc;
use crate::{delay, tick};

/// TFTP's well-known port; only the request goes here.
const SERVER_PORT: u16 = 69;

/// Opcodes.
const OP_READ_REQUEST: u16 = 1;
const OP_DATA: u16 = 3;
const OP_ACKNOWLEDGE: u16 = 4;
const OP_ERROR: u16 = 5;

/// Fixed by RFC 1350; changing it needs the option extension.
pub const BLOCK_BYTES: usize = 512;

/// How long to wait for the next DATA before resending the last packet,
/// and how many times to try before giving up.
const REPLY_TIMEOUT_MS: u64 = 2000;
const RETRIES: u32 = 5;

/// Refuse anything that would eat the heap. The PSRAM heap is around
/// 30 MiB; this leaves it room to do something with the result.
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

const RECEIVE_PACKETS: usize = 8;
const SEND_PACKETS: usize = 4;
/// One datagram is four bytes of header plus a block.
const PACKET_BYTES: usize = BLOCK_BYTES + 4;

pub enum Error {
    /// The link to the C6 went away.
    LinkLost,
    /// No answer at all, or no answer to a retransmission.
    TimedOut,
    /// The server refused, with its own code and message.
    Server { code: u16, message: Vec<u8> },
    /// The file is larger than [`MAX_FILE_BYTES`].
    TooLarge,
    /// A socket operation failed locally.
    Local,
}

/// Reads `filename` from `server` in octet mode and returns its contents.
///
/// `progress` is called with the running byte count as blocks arrive, so a
/// long transfer is visibly making progress rather than merely not
/// finished.
pub fn get(
    stack: &mut Stack,
    rpc: &mut Rpc,
    server: Ipv4Address,
    filename: &[u8],
    progress: &mut dyn FnMut(usize),
) -> Result<Vec<u8>, Error> {
    let receive_buffer = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; RECEIVE_PACKETS],
        vec![0u8; RECEIVE_PACKETS * PACKET_BYTES],
    );
    let send_buffer = udp::PacketBuffer::new(
        vec![udp::PacketMetadata::EMPTY; SEND_PACKETS],
        vec![0u8; SEND_PACKETS * PACKET_BYTES],
    );

    let handle = stack
        .sockets_mut()
        .add(udp::Socket::new(receive_buffer, send_buffer));
    if stack
        .sockets_mut()
        .get_mut::<udp::Socket>(handle)
        .bind(ephemeral_port())
        .is_err()
    {
        stack.sockets_mut().remove(handle);
        return Err(Error::Local);
    }

    let result = transfer(stack, rpc, handle, server, filename, progress);
    stack.sockets_mut().remove(handle);
    result
}

fn transfer(
    stack: &mut Stack,
    rpc: &mut Rpc,
    handle: smoltcp::iface::SocketHandle,
    server: Ipv4Address,
    filename: &[u8],
    progress: &mut dyn FnMut(usize),
) -> Result<Vec<u8>, Error> {
    let request = read_request(filename);
    // Until the first DATA arrives the only known endpoint is port 69.
    let mut peer = IpEndpoint::new(IpAddress::Ipv4(server), SERVER_PORT);
    let mut outgoing = request;

    let mut file = Vec::new();
    let mut expected_block = 1u16;

    loop {
        let mut attempts = 0;
        let block = loop {
            if attempts > RETRIES {
                return Err(Error::TimedOut);
            }
            attempts += 1;

            send(stack, handle, peer, &outgoing)?;

            let mut received = None;
            stack.pump_until(rpc, REPLY_TIMEOUT_MS, |stack| {
                let socket = stack.sockets_mut().get_mut::<udp::Socket>(handle);
                while let Ok((datagram, metadata)) = socket.recv() {
                    // A datagram from somewhere else on this port is not
                    // part of this transfer.
                    if metadata.endpoint.addr != IpAddress::Ipv4(server) {
                        continue;
                    }
                    received = Some((datagram.to_vec(), metadata.endpoint));
                    return true;
                }
                false
            });

            let Some((datagram, endpoint)) = received else {
                if !rpc.is_alive() {
                    return Err(Error::LinkLost);
                }
                continue;
            };

            match parse(&datagram) {
                Some(Packet::Data { block, data }) => {
                    // The server picked its own port for the transfer with
                    // its first answer; everything after goes there.
                    peer = endpoint;
                    break (block, data.to_vec());
                }
                Some(Packet::Error { code, message }) => {
                    return Err(Error::Server {
                        code,
                        message: message.to_vec(),
                    });
                }
                _ => continue,
            }
        };

        let (block_number, data) = block;
        if block_number == expected_block {
            if file.len() + data.len() > MAX_FILE_BYTES {
                return Err(Error::TooLarge);
            }
            file.extend_from_slice(&data);
            progress(file.len());
            expected_block = expected_block.wrapping_add(1);
        }
        // A repeat of an earlier block means our acknowledgement was lost;
        // acknowledging it again is the whole recovery.
        outgoing = acknowledge(block_number);

        if data.len() < BLOCK_BYTES {
            send(stack, handle, peer, &outgoing)?;
            // Give the last acknowledgement a moment to actually leave
            // before the socket is torn down.
            stack.pump_until(rpc, 200, |_| false);
            return Ok(file);
        }
    }
}

fn send(
    stack: &mut Stack,
    handle: smoltcp::iface::SocketHandle,
    peer: IpEndpoint,
    datagram: &[u8],
) -> Result<(), Error> {
    stack
        .sockets_mut()
        .get_mut::<udp::Socket>(handle)
        .send_slice(datagram, peer)
        .map_err(|_| Error::Local)
}

enum Packet<'a> {
    Data { block: u16, data: &'a [u8] },
    Error { code: u16, message: &'a [u8] },
    Other,
}

fn parse(datagram: &[u8]) -> Option<Packet<'_>> {
    if datagram.len() < 4 {
        return None;
    }
    let opcode = u16::from_be_bytes([datagram[0], datagram[1]]);
    let field = u16::from_be_bytes([datagram[2], datagram[3]]);
    match opcode {
        OP_DATA => Some(Packet::Data {
            block: field,
            data: &datagram[4..],
        }),
        OP_ERROR => {
            // The message is NUL-terminated; servers vary on whether the
            // terminator is included in what they send.
            let message = &datagram[4..];
            let end = message
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(message.len());
            Some(Packet::Error {
                code: field,
                message: &message[..end],
            })
        }
        _ => Some(Packet::Other),
    }
}

/// `RRQ | filename | 0 | "octet" | 0`. Octet mode is the only one worth
/// having: netascii would rewrite line endings in a file being checksummed.
fn read_request(filename: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(filename.len() + 10);
    packet.extend_from_slice(&OP_READ_REQUEST.to_be_bytes());
    packet.extend_from_slice(filename);
    packet.push(0);
    packet.extend_from_slice(b"octet");
    packet.push(0);
    packet
}

fn acknowledge(block: u16) -> Vec<u8> {
    let mut packet = Vec::with_capacity(4);
    packet.extend_from_slice(&OP_ACKNOWLEDGE.to_be_bytes());
    packet.extend_from_slice(&block.to_be_bytes());
    packet
}

/// A source port from the ephemeral range. Anything unused will do; the
/// server replies to whatever it sees.
fn ephemeral_port() -> u16 {
    49152 + (delay::cycle_count() % 16384) as u16
}

/// CRC-32 (the zlib/PNG polynomial) of the transferred bytes, so the result
/// can be compared with `crc32` on the machine serving the file.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Wall-clock helper for the transfer-rate line: bytes per second, or 0
/// when the transfer was too quick to measure.
pub fn throughput(bytes: usize, started_ms: u64) -> u32 {
    let elapsed = tick::now_ms().saturating_sub(started_ms);
    if elapsed == 0 {
        return 0;
    }
    ((bytes as u64 * 1000) / elapsed) as u32
}
