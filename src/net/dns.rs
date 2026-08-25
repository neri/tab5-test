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

use smoltcp::socket::dns::{self, GetQueryResultError, StartQueryError};
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

#[derive(Clone, Copy, PartialEq, Eq)]
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

/// A sentence for a status line.
pub fn error_text(error: Error) -> &'static str {
    match error {
        Error::NoServers => "no name server is configured; run ipconfig dhcp",
        Error::InvalidName => "that host name cannot be looked up",
        Error::NotFound => "no such host",
        Error::TimedOut => "the name server did not answer",
        Error::LinkLost => "the Wi-Fi link was lost",
        Error::Local => "the resolver had no free slot",
    }
}

pub struct Answer {
    /// Every A record that came back, in the order the server gave them.
    /// Never empty: an answer with no address is reported as
    /// [`Error::NotFound`].
    pub addresses: Vec<Ipv4Address>,
    pub elapsed_ms: u64,
}

/// A query in flight, polled by a caller that cannot block.
///
/// The browser is the reason this exists. A name takes up to thirteen
/// seconds to be called dead (see the budgets above), and a screen that
/// stops servicing its input for thirteen seconds is a screen whose Escape
/// key does not work -- on exactly the pages where somebody would reach for
/// it. [`resolve`] below is this same query with a `pump_until` around it,
/// so the two cannot disagree about timeouts or about what a failure means.
///
/// The slot in the DNS socket is released by [`Query::poll`] taking the
/// answer, or by [`Query::cancel`]. Dropping one without either leaks the
/// slot until the socket recycles it, which is why `cancel` exists at all.
#[must_use = "a Query holds a slot in the DNS socket"]
pub struct Query {
    handle: dns::QueryHandle,
    started_ms: u64,
    deadline_ms: u64,
    /// Set once the slot has been given back, so `cancel` cannot release
    /// it twice -- `cancel_query` panics on a free slot.
    settled: bool,
}

/// What one [`Query::poll`] found.
pub enum Progress {
    /// No answer yet, and the deadline has not passed.
    Pending,
    Ready(Answer),
    Failed(Error),
}

impl Query {
    /// Starts a query. Nothing is sent until the first [`Query::poll`].
    pub fn start(stack: &mut Stack, name: &[u8]) -> Result<Query, Error> {
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
        let handle = match stack.start_dns_query(name) {
            Ok(handle) => handle,
            Err(StartQueryError::InvalidName | StartQueryError::NameTooLong) => {
                return Err(Error::InvalidName);
            }
            Err(StartQueryError::NoFreeSlot) => return Err(Error::Local),
        };
        let started_ms = tick::now_ms();
        Ok(Query {
            handle,
            started_ms,
            deadline_ms: started_ms + timeout_ms,
            settled: false,
        })
    }

    /// One turn. Returns immediately whatever the answer is.
    pub fn poll(&mut self, stack: &mut Stack, rpc: &mut Rpc) -> Progress {
        if self.settled {
            return Progress::Failed(Error::Local);
        }
        if !stack.poll(rpc) {
            return self.fail(stack, Error::LinkLost);
        }
        // `get_query_result` frees the slot as it hands the answer over,
        // and calling it on a free slot panics -- so `settled` is set the
        // one time it is not pending, and nothing asks again.
        match stack.dns_socket_mut().get_query_result(self.handle) {
            Err(GetQueryResultError::Pending) => {
                if tick::now_ms() >= self.deadline_ms {
                    return self.fail(stack, Error::TimedOut);
                }
                Progress::Pending
            }
            Ok(addresses) => {
                self.settled = true;
                let addresses: Vec<Ipv4Address> = addresses
                    .iter()
                    .filter_map(|address| match address {
                        IpAddress::Ipv4(address) => Some(*address),
                    })
                    .collect();
                // smoltcp already fails a query whose answer held no
                // address, so this is the leftover case of an answer
                // carrying only records this build cannot use.
                if addresses.is_empty() {
                    return Progress::Failed(Error::NotFound);
                }
                Progress::Ready(Answer {
                    addresses,
                    elapsed_ms: tick::now_ms().saturating_sub(self.started_ms),
                })
            }
            Err(_) => {
                self.settled = true;
                Progress::Failed(Error::NotFound)
            }
        }
    }

    /// Gives the slot back. Safe to call after the query has settled.
    pub fn cancel(mut self, stack: &mut Stack) {
        if !self.settled {
            stack.dns_socket_mut().cancel_query(self.handle);
            self.settled = true;
        }
    }

    fn fail(&mut self, stack: &mut Stack, error: Error) -> Progress {
        if !self.settled {
            stack.dns_socket_mut().cancel_query(self.handle);
            self.settled = true;
        }
        Progress::Failed(error)
    }
}

/// Resolves `name` to its A records, blocking until it settles.
///
/// `name` is taken as raw bytes because that is what a shell argument is;
/// anything that is not a usable name comes back as
/// [`Error::InvalidName`] rather than making every caller check first.
///
/// This is [`Query`] with a pump loop around it: the shell is
/// single-threaded and a command that owns the console may as well wait.
pub fn resolve(stack: &mut Stack, rpc: &mut Rpc, name: &[u8]) -> Result<Answer, Error> {
    let mut query = Query::start(stack, name)?;
    loop {
        match query.poll(stack, rpc) {
            Progress::Pending => {}
            Progress::Ready(answer) => {
                query.cancel(stack);
                return Ok(answer);
            }
            Progress::Failed(error) => {
                query.cancel(stack);
                return Err(error);
            }
        }
    }
}
