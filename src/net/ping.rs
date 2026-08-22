//! ICMP echo, the shortest round trip that exercises the whole path.
//!
//! One `ping` covers ARP resolution, IPv4 transmit, IPv4 receive and ICMP
//! in a single observation, which is why it is the first thing worth
//! running after an address is set.
//!
//! Answering someone else's echo requests is not here: smoltcp's
//! `auto-icmp-echo-reply` feature has the interface reply on its own, so
//! this module only sends.

use alloc::vec;

use smoltcp::phy::ChecksumCapabilities;
use smoltcp::socket::icmp;
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpAddress, Ipv4Address};

use crate::net::Stack;
use crate::wifi::Rpc;
use crate::{delay, tick};

/// Payload carried by each request. 32 bytes is what most ping utilities
/// use, which keeps a capture easy to compare against a familiar one.
const PAYLOAD_BYTES: usize = 32;

/// How long one request waits for its reply.
const REPLY_TIMEOUT_MS: u64 = 2000;
/// Gap between requests, so a run of them is readable as it happens.
const INTERVAL_MS: u64 = 1000;

const RECEIVE_PACKETS: usize = 4;
const SEND_PACKETS: usize = 4;
const PACKET_BYTES: usize = 128;

/// What happened to one request.
pub enum Reply {
    Received { sequence: u16, elapsed_ms: u64 },
    TimedOut { sequence: u16 },
}

/// Summary of a whole run.
pub struct Summary {
    pub sent: u32,
    pub received: u32,
    pub min_ms: u64,
    pub max_ms: u64,
    pub total_ms: u64,
}

impl Summary {
    pub fn average_ms(&self) -> u64 {
        if self.received == 0 {
            0
        } else {
            self.total_ms / self.received as u64
        }
    }
}

/// Sends `count` echo requests to `address`, reporting each one through
/// `report` as it resolves so the console fills in as the run goes.
///
/// Returns `None` if the link died partway through; the caller should drop
/// its session in that case.
pub fn run(
    stack: &mut Stack,
    rpc: &mut Rpc,
    address: Ipv4Address,
    count: u32,
    report: &mut dyn FnMut(Reply),
) -> Option<Summary> {
    let receive_buffer = icmp::PacketBuffer::new(
        vec![icmp::PacketMetadata::EMPTY; RECEIVE_PACKETS],
        vec![0u8; RECEIVE_PACKETS * PACKET_BYTES],
    );
    let send_buffer = icmp::PacketBuffer::new(
        vec![icmp::PacketMetadata::EMPTY; SEND_PACKETS],
        vec![0u8; SEND_PACKETS * PACKET_BYTES],
    );

    // The identifier is what tells our replies from anyone else's on a
    // shared network; it only has to be unlikely to collide.
    let identifier = (delay::cycle_count() as u16) | 1;
    let handle = stack
        .sockets_mut()
        .add(icmp::Socket::new(receive_buffer, send_buffer));
    if stack
        .sockets_mut()
        .get_mut::<icmp::Socket>(handle)
        .bind(icmp::Endpoint::Ident(identifier))
        .is_err()
    {
        stack.sockets_mut().remove(handle);
        return None;
    }

    let checksum = ChecksumCapabilities::default();
    let payload = [b'a'; PAYLOAD_BYTES];
    let mut summary = Summary {
        sent: 0,
        received: 0,
        min_ms: u64::MAX,
        max_ms: 0,
        total_ms: 0,
    };
    let mut alive = true;

    for sequence in 0..count as u16 {
        if !alive {
            break;
        }
        // Counted as an attempt: what the operator wants from the summary
        // is "of the N I asked for, how many came back".
        summary.sent += 1;

        // The send buffer is only full if the previous request never left,
        // which on this link means the C6 is throttling.
        if !stack.pump_until(rpc, REPLY_TIMEOUT_MS, |stack| {
            stack
                .sockets_mut()
                .get_mut::<icmp::Socket>(handle)
                .can_send()
        }) {
            alive = rpc.is_alive();
            report(Reply::TimedOut { sequence });
            continue;
        }

        let request = Icmpv4Repr::EchoRequest {
            ident: identifier,
            seq_no: sequence,
            data: &payload,
        };
        {
            let socket = stack.sockets_mut().get_mut::<icmp::Socket>(handle);
            let Ok(buffer) = socket.send(request.buffer_len(), IpAddress::Ipv4(address)) else {
                report(Reply::TimedOut { sequence });
                continue;
            };
            request.emit(&mut Icmpv4Packet::new_unchecked(buffer), &checksum);
        }
        let sent_at = tick::now_ms();

        let mut elapsed = None;
        let finished = stack.pump_until(rpc, REPLY_TIMEOUT_MS, |stack| {
            let socket = stack.sockets_mut().get_mut::<icmp::Socket>(handle);
            while let Ok((packet, _)) = socket.recv() {
                let Ok(packet) = Icmpv4Packet::new_checked(packet) else {
                    continue;
                };
                let Ok(Icmpv4Repr::EchoReply { ident, seq_no, .. }) =
                    Icmpv4Repr::parse(&packet, &checksum)
                else {
                    continue;
                };
                // A reply to an earlier request that arrived after its
                // deadline is not this one; keep waiting.
                if ident == identifier && seq_no == sequence {
                    elapsed = Some(tick::now_ms() - sent_at);
                    return true;
                }
            }
            false
        });
        if !finished {
            alive = rpc.is_alive();
        }

        match elapsed {
            Some(elapsed_ms) => {
                summary.received += 1;
                summary.total_ms += elapsed_ms;
                summary.min_ms = summary.min_ms.min(elapsed_ms);
                summary.max_ms = summary.max_ms.max(elapsed_ms);
                report(Reply::Received {
                    sequence,
                    elapsed_ms,
                });

                // Keep pumping through the gap: leaving the link unread
                // for a second is what lets frames pile up on the C6.
                let remaining = INTERVAL_MS.saturating_sub(elapsed_ms);
                if sequence + 1 < count as u16 && remaining > 0 {
                    stack.pump_until(rpc, remaining, |_| false);
                    alive = rpc.is_alive();
                }
            }
            None => report(Reply::TimedOut { sequence }),
        }
    }

    stack.sockets_mut().remove(handle);
    if summary.min_ms == u64::MAX {
        summary.min_ms = 0;
    }
    Some(summary)
}
