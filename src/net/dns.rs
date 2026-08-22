//! Name resolution: A records over smoltcp's DNS socket.
//!
//! Only the driving is here. The socket itself lives in [`Stack`] because
//! it has to stay in the socket set to be retransmitted and delivered to,
//! which makes this module the thinnest of the clients in `net/`: start a
//! query, pump until it settles, take the answer once.
//!
//! There is no cache. The shell asks for names at the speed a person types
//! them, so a TTL table would cost more to be wrong about than it could
//! ever save.

use alloc::vec::Vec;

use smoltcp::socket::dns::{GetQueryResultError, StartQueryError};
use smoltcp::wire::{IpAddress, Ipv4Address};

use crate::net::Stack;
use crate::tick;
use crate::wifi::Rpc;

/// smoltcp's own per-server interval (`RETRANSMIT_TIMEOUT`).
///
/// Two things hang off it, and they pull in opposite directions:
///
/// - the switch to the next server happens once this has elapsed on the
///   current one, so a budget below it never falls back at all
/// - once it has elapsed on the *last* server, smoltcp gives up and marks
///   the query `Failure` -- **the same state a name that does not exist
///   produces**. `get_query_result` returns `Failed` either way, so a
///   budget that reaches `PER_SERVER_MS * servers` would report a network
///   with no working resolver as "no such name"
///
/// Every budget below therefore sits strictly between those two points.
const PER_SERVER_MS: u64 = 10_000;

/// Budget when exactly one resolver is set: there is no second server to
/// reach, so this only has to be long enough to call the first one dead,
/// and short enough to stay under the give-up point.
const SINGLE_SERVER_MS: u64 = 5_000;

/// Budget when two or more are set: past `PER_SERVER_MS` so the switch
/// actually happens, plus a little for the next resolver to answer.
///
/// The second server does **not** need a full interval. Ten seconds is
/// when smoltcp stops waiting, not how long an answer takes -- a live
/// resolver replies in milliseconds, so three seconds is generous.
const FALLBACK_MS: u64 = PER_SERVER_MS + 3_000;

// The reasoning above is load-bearing, so it is checked rather than
// trusted. Debug assertions would not do: this firmware is only ever built
// in release (a debug build does not fit in RAM).
const _: () = assert!(SINGLE_SERVER_MS < PER_SERVER_MS);
const _: () = assert!(FALLBACK_MS > PER_SERVER_MS);
const _: () = assert!(FALLBACK_MS < PER_SERVER_MS * 2);

pub enum Error {
    /// No resolver is configured, so there is nobody to ask.
    ///
    /// This is checked before starting rather than reported by the query,
    /// because smoltcp reads an empty server list as "already tried them
    /// all" and fails the query on its first dispatch -- which arrives
    /// looking exactly like [`NotFound`](Error::NotFound).
    NoServers,
    /// Not something that can go in a query: not UTF-8, an empty label, a
    /// label over 63 bytes, or a name over 255.
    InvalidName,
    /// A server answered, and the name has no address.
    NotFound,
    /// Nobody answered in time.
    TimedOut,
    LinkLost,
    Local,
}

pub struct Answer {
    /// Every A record that came back, in the order the server gave them.
    /// Never empty: an answer with no address is reported as
    /// [`Error::NotFound`].
    pub addresses: Vec<Ipv4Address>,
    pub elapsed_ms: u64,
}

/// Resolves `name` to its A records.
///
/// `name` is taken as raw bytes because that is what a shell argument is;
/// anything that is not a usable name comes back as
/// [`Error::InvalidName`] rather than making every caller check first.
pub fn resolve(stack: &mut Stack, rpc: &mut Rpc, name: &[u8]) -> Result<Answer, Error> {
    let servers = stack.dns_servers().len() as u64;
    if servers == 0 {
        return Err(Error::NoServers);
    }
    let Ok(name) = core::str::from_utf8(name) else {
        return Err(Error::InvalidName);
    };
    let timeout_ms = if servers > 1 {
        FALLBACK_MS
    } else {
        SINGLE_SERVER_MS
    };

    let started = tick::now_ms();
    let handle = match stack.start_dns_query(name) {
        Ok(handle) => handle,
        Err(StartQueryError::InvalidName | StartQueryError::NameTooLong) => {
            return Err(Error::InvalidName);
        }
        Err(StartQueryError::NoFreeSlot) => return Err(Error::Local),
    };

    // `get_query_result` frees the slot as it hands the answer over, and
    // calling it on a free slot panics -- so the result is captured the
    // one time it is not pending, and the predicate returning true stops
    // `pump_until` from asking again.
    let mut outcome = None;
    let settled = stack.pump_until(rpc, timeout_ms, |stack| {
        match stack.dns_socket_mut().get_query_result(handle) {
            Err(GetQueryResultError::Pending) => false,
            result => {
                outcome = Some(result);
                true
            }
        }
    });

    let Some(result) = outcome else {
        // Nothing was taken, so the slot is still ours to release. This is
        // the only path that may cancel: after an answer there is no slot
        // left and `cancel_query` would panic on it.
        debug_assert!(!settled);
        stack.dns_socket_mut().cancel_query(handle);
        return Err(if rpc.is_alive() {
            Error::TimedOut
        } else {
            Error::LinkLost
        });
    };

    let addresses: Vec<Ipv4Address> = match result {
        Ok(addresses) => addresses
            .iter()
            .filter_map(|address| match address {
                IpAddress::Ipv4(address) => Some(*address),
            })
            .collect(),
        Err(_) => return Err(Error::NotFound),
    };

    // smoltcp already fails a query whose answer held no address, so this
    // is the leftover case of an answer carrying only records this build
    // cannot use.
    if addresses.is_empty() {
        return Err(Error::NotFound);
    }

    Ok(Answer {
        addresses,
        elapsed_ms: tick::now_ms().saturating_sub(started),
    })
}
