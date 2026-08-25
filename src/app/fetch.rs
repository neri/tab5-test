//! One page, fetched: name resolution, the transfer, redirects, and the
//! decision about what the response actually is.
//!
//! Split out of `app::browser` because two callers need exactly this and
//! nothing else. The viewer drives it from its frame loop and draws what
//! comes back; `app::browsertest` drives it from a shell command and prints
//! what comes back. Having the redirect rules, the status rules and the
//! "is this even HTML" rule in one place is what makes the diagnostic worth
//! running -- a walk that exercised its own copy of them would be checking
//! the wrong code.
//!
//! Nothing here blocks. [`Fetch::step`] does a bounded amount of work and
//! returns, so a caller with a screen can service its input between calls
//! and a caller without one loses nothing.
//!
//! Two things are owned and have to be given back: a slot in the DNS
//! socket, and a handle in the TCP socket set. [`Fetch::close`] is the only
//! way to return them, and it takes `self` so that forgetting is visible at
//! the call site rather than at the point the socket set runs dry.

use crate::browser::document::{Document, Parser};
use crate::browser::error::{self, Error};
use crate::browser::limits::{MAX_DECODED_HTML_BYTES, MAX_REDIRECTS};
use crate::browser::url::Url;
use crate::net::http::{self, Progress, Transaction};
use crate::net::{self, dns};
use crate::{tick, wifi};

use smoltcp::wire::Ipv4Address;

/// Body bytes moved in one [`Fetch::step`].
///
/// At the panel's 57 Hz this is nearly a megabyte a second, far more than
/// the C6 link delivers -- so in practice the budget is never the limit and
/// a frame loop always gets back to its input. It is here for the case
/// where it would be: a page served off the LAN at full speed must not be
/// able to hold the caller for longer than a frame.
pub const BYTES_PER_STEP: usize = 16 * 1024;

/// The two halves of the network, borrowed together because nothing here
/// ever needs one without the other.
pub struct Network<'a> {
    pub rpc: &'a mut wifi::Rpc,
    pub stack: &'a mut net::Stack,
}

impl<'a> Network<'a> {
    /// Pairs the link with the stack, if both are present and addressed.
    ///
    /// `None` is not an error anywhere it is used: the browser's built-in
    /// pages work without a network, and the diagnostic says so and stops.
    pub fn new(
        rpc: Option<&'a mut wifi::Rpc>,
        stack: Option<&'a mut net::Stack>,
    ) -> Option<Network<'a>> {
        match (rpc, stack) {
            (Some(rpc), Some(stack)) if stack.has_address() => Some(Network { rpc, stack }),
            _ => None,
        }
    }
}

/// Why a page was not shown.
///
/// `name` is the stable one-word form the fixture manifest and the UART use
/// and must not drift; `headline` and `detail` are what a reader sees.
#[derive(Clone, Copy)]
pub struct Failure {
    pub name: &'static str,
    pub headline: &'static str,
    pub detail: &'static str,
    /// The HTTP status, when there was one.
    pub status: Option<u16>,
}

impl Failure {
    const fn new(name: &'static str, headline: &'static str, detail: &'static str) -> Failure {
        Failure {
            name,
            headline,
            detail,
            status: None,
        }
    }

    const fn with_status(mut self, status: Option<u16>) -> Failure {
        self.status = status;
        self
    }
}

/// What one [`Fetch::step`] came to.
pub enum Outcome {
    /// Still going. Call again.
    Working,
    /// The body ended and the document was built.
    Page(Document),
    Failed(Failure),
}

/// One page being fetched, from a name to a document.
#[must_use = "a Fetch owns a socket and has to be closed"]
pub struct Fetch {
    /// The address currently being fetched, which moves with each redirect.
    url: Url,
    redirects: usize,
    query: Option<dns::Query>,
    transaction: Option<Transaction>,
    /// Built only once the head says the response is one to display, so a
    /// redirect or an error status never allocates a document.
    parser: Option<Parser>,
    /// A limit the document hit, which stops the transfer through the sink.
    document_error: Option<Error>,
    /// Bytes received across every hop of this navigation.
    received: usize,
    /// The largest the parser was ever holding, for the memory budget.
    peak_owned: usize,
    started_ms: u64,
}

impl Fetch {
    /// Begins a fetch. The first byte does not leave until [`Fetch::step`].
    pub fn start(url: Url, network: &mut Network<'_>) -> Result<Fetch, Failure> {
        if !url.scheme().is_fetchable() {
            // Refused here rather than at connect time, and never rewritten
            // to `http`. A secure address that quietly becomes an insecure
            // one is worse than an address that does not load.
            return Err(HTTPS);
        }
        let mut fetch = Fetch {
            url,
            redirects: 0,
            query: None,
            transaction: None,
            parser: None,
            document_error: None,
            received: 0,
            peak_owned: 0,
            started_ms: tick::now_ms(),
        };
        fetch.open(network)?;
        Ok(fetch)
    }

    /// The address being fetched now, which is the last hop of a redirect
    /// chain rather than the one the caller asked for.
    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn received(&self) -> usize {
        self.received + self.current_received()
    }

    pub fn redirects(&self) -> usize {
        self.redirects
    }

    /// The most the parser held at once. Zero until a body starts arriving.
    pub fn peak_owned(&self) -> usize {
        self.peak_owned
    }

    pub fn elapsed_ms(&self) -> u64 {
        tick::now_ms().saturating_sub(self.started_ms)
    }

    fn current_received(&self) -> usize {
        self.transaction
            .as_ref()
            .map_or(0, |transaction| transaction.stats().received)
    }

    /// Starts the lookup or the connection for `self.url`.
    fn open(&mut self, network: &mut Network<'_>) -> Result<(), Failure> {
        // A literal address skips the resolver entirely, which is what
        // makes a fixture server reachable on a network with no DNS.
        match self.url.ipv4() {
            Some(octets) => {
                let address = Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]);
                self.transaction = Some(self.connect(address, network)?);
                Ok(())
            }
            None => match dns::Query::start(network.stack, self.url.host().as_bytes()) {
                Ok(query) => {
                    self.query = Some(query);
                    Ok(())
                }
                Err(error) => Err(Failure {
                    name: "dns",
                    headline: "Cannot find that host",
                    detail: dns::error_text(error),
                    status: None,
                }),
            },
        }
    }

    fn connect(
        &self,
        address: Ipv4Address,
        network: &mut Network<'_>,
    ) -> Result<Transaction, Failure> {
        let (Ok(target), Ok(host)) = (self.url.request_target(), self.url.host_header()) else {
            return Err(OUT_OF_MEMORY);
        };
        Transaction::start(
            network.stack,
            address,
            self.url.port(),
            host.as_bytes(),
            target.as_bytes(),
            MAX_DECODED_HTML_BYTES as u64,
        )
        .map_err(|error| Failure {
            name: http::error_name(error),
            headline: "Cannot connect",
            detail: http::error_text(error),
            status: None,
        })
    }

    /// Moves to the next address in a redirect chain, giving back the
    /// socket the previous hop was using.
    fn redirect_to(&mut self, target: Url, network: &mut Network<'_>) -> Outcome {
        if let Some(transaction) = self.transaction.take() {
            self.received += transaction.close(network.stack, network.rpc).received;
        }
        self.parser = None;
        self.document_error = None;
        self.url = target;
        self.redirects += 1;
        if !self.url.scheme().is_fetchable() {
            return Outcome::Failed(HTTPS);
        }
        match self.open(network) {
            Ok(()) => Outcome::Working,
            Err(failure) => Outcome::Failed(failure),
        }
    }

    pub fn step(&mut self, network: &mut Network<'_>) -> Outcome {
        if self.query.is_some() {
            return self.step_query(network);
        }
        self.step_transfer(network)
    }

    fn step_query(&mut self, network: &mut Network<'_>) -> Outcome {
        let Some(query) = self.query.as_mut() else {
            return Outcome::Working;
        };
        let progress = query.poll(network.stack, network.rpc);
        match progress {
            dns::Progress::Pending => Outcome::Working,
            dns::Progress::Ready(answer) => {
                if let Some(query) = self.query.take() {
                    query.cancel(network.stack);
                }
                // The first A record. A round-robin name gives several, and
                // trying the next one on a refused connection would be a
                // retry policy; there is not one here yet.
                let Some(&address) = answer.addresses.first() else {
                    return Outcome::Failed(Failure {
                        name: "dns",
                        headline: "Cannot find that host",
                        detail: dns::error_text(dns::Error::NotFound),
                        status: None,
                    });
                };
                match self.connect(address, network) {
                    Ok(transaction) => {
                        self.transaction = Some(transaction);
                        Outcome::Working
                    }
                    Err(failure) => Outcome::Failed(failure),
                }
            }
            dns::Progress::Failed(error) => {
                if let Some(query) = self.query.take() {
                    query.cancel(network.stack);
                }
                Outcome::Failed(Failure {
                    name: "dns",
                    headline: "Cannot find that host",
                    detail: dns::error_text(error),
                    status: None,
                })
            }
        }
    }

    fn step_transfer(&mut self, network: &mut Network<'_>) -> Outcome {
        let Some(transaction) = self.transaction.as_mut() else {
            return Outcome::Working;
        };
        // Borrowed as separate fields so the sink can hold the parser while
        // `poll` holds the transaction.
        let parser = &mut self.parser;
        let document_error = &mut self.document_error;
        let mut spent = 0usize;
        let mut progress = Progress::Idle;
        while spent < BYTES_PER_STEP {
            let before = transaction.stats().received;
            let mut sink = |bytes: &[u8]| {
                let Some(parser) = parser.as_mut() else {
                    // No parser yet means the head has not been accepted;
                    // nothing should be arriving, and if it is, it is not
                    // wanted.
                    return true;
                };
                match parser.feed(bytes) {
                    Ok(()) => true,
                    Err(failure) => {
                        *document_error = Some(failure);
                        false
                    }
                }
            };
            progress = transaction.poll(
                network.stack,
                network.rpc,
                http::DEFAULT_POLL_BUDGET,
                &mut sink,
            );
            spent += transaction.stats().received.saturating_sub(before);
            match progress {
                Progress::Body | Progress::Connecting => continue,
                _ => break,
            }
        }
        if let Some(parser) = self.parser.as_ref() {
            self.peak_owned = self.peak_owned.max(parser.owned_bytes());
        }

        match progress {
            Progress::HeadReady => self.head_ready(network),
            Progress::Complete => self.complete(),
            Progress::Failed(error) => {
                // A limit inside the document is the more specific reason,
                // and the transport error it produced -- a refused sink --
                // says nothing useful.
                match self.document_error {
                    Some(failure) => Outcome::Failed(Failure {
                        name: error::error_name(failure),
                        headline: "Cannot show this page",
                        detail: error::error_text(failure),
                        status: None,
                    }),
                    None => Outcome::Failed(Failure {
                        name: http::error_name(error),
                        headline: "Cannot show this page",
                        detail: http::error_text(error),
                        status: None,
                    }),
                }
            }
            Progress::Idle | Progress::Body | Progress::Connecting => Outcome::Working,
        }
    }

    /// The head has arrived, and none of the body has. Everything that can
    /// be decided from it is decided here, before a document exists.
    fn head_ready(&mut self, network: &mut Network<'_>) -> Outcome {
        let Some(transaction) = self.transaction.as_ref() else {
            return Outcome::Working;
        };
        let Some(head) = transaction.head() else {
            return Outcome::Working;
        };
        let status = head.status;

        if head.is_redirect() {
            if self.redirects >= MAX_REDIRECTS {
                return Outcome::Failed(REDIRECT_LIMIT.with_status(status));
            }
            let Some(location) = head.location.as_deref() else {
                return Outcome::Failed(BROKEN_REDIRECT.with_status(status));
            };
            let Ok(text) = core::str::from_utf8(location) else {
                return Outcome::Failed(BROKEN_REDIRECT.with_status(status));
            };
            // Resolved against the request's own URL, so a relative
            // `Location` -- which RFC 7231 allows -- lands where the server
            // meant rather than at the site root.
            let Ok(target) = self.url.resolve(text) else {
                return Outcome::Failed(BROKEN_REDIRECT.with_status(status));
            };
            return self.redirect_to(target, network);
        }

        if status.is_none() {
            // The first line was not a status line, so this is not an HTTP
            // response at all.
            return Outcome::Failed(NOT_HTTP);
        }
        if !head.is_success() {
            return Outcome::Failed(REFUSED.with_status(status));
        }
        if !head.is_html() {
            return Outcome::Failed(NOT_HTML.with_status(status));
        }

        // Only now is a document allocated. A redirect chain, an error
        // status and a download all get this far without one.
        match Parser::new(self.url.clone()) {
            Ok(parser) => {
                self.parser = Some(parser);
                Outcome::Working
            }
            Err(_) => Outcome::Failed(OUT_OF_MEMORY),
        }
    }

    fn complete(&mut self) -> Outcome {
        let Some(parser) = self.parser.take() else {
            return Outcome::Failed(EMPTY);
        };
        self.peak_owned = self.peak_owned.max(parser.owned_bytes());
        match parser.finish() {
            Ok(document) => Outcome::Page(document),
            Err(failure) => Outcome::Failed(Failure {
                name: error::error_name(failure),
                headline: "Cannot show this page",
                detail: error::error_text(failure),
                status: None,
            }),
        }
    }

    /// Gives back the resolver slot and the socket.
    pub fn close(self, network: &mut Network<'_>) {
        if let Some(query) = self.query {
            query.cancel(network.stack);
        }
        if let Some(transaction) = self.transaction {
            transaction.close(network.stack, network.rpc);
        }
    }
}

/// The failures that do not come from `net::http` or the document layer.
///
/// Named as constants so the one-word `name` -- which the fixture manifest
/// is written against -- is in one place rather than spelled out at each
/// site that produces it.
pub const HTTPS: Failure = Failure::new(
    "https",
    "HTTPS is not supported",
    "This build has no TLS, and a secure address is never quietly turned \
     into an insecure one.",
);
pub const REDIRECT_LIMIT: Failure = Failure::new(
    "redirect-limit",
    "Too many redirects",
    "The chain either loops or is longer than this follows.",
);
pub const BROKEN_REDIRECT: Failure = Failure::new(
    "redirect-broken",
    "Broken redirect",
    "The server asked for a redirect to somewhere this cannot read.",
);
pub const NOT_HTTP: Failure = Failure::new(
    "not-http",
    "Not an HTTP response",
    "The first line was not a status line, so nothing after it can be \
     trusted to be a response.",
);
pub const REFUSED: Failure = Failure::new(
    "status",
    "The server refused",
    "Its own page for this is not shown: it is not the page that was asked \
     for.",
);
pub const NOT_HTML: Failure = Failure::new(
    "not-html",
    "Not a page",
    "This is not HTML, and this viewer shows nothing else.",
);
pub const EMPTY: Failure = Failure::new(
    "empty",
    "Empty response",
    "The server sent no page.",
);
pub const OUT_OF_MEMORY: Failure = Failure::new(
    "out-of-memory",
    "Out of memory",
    "There was not enough heap left to read this page.",
);
pub const NO_NETWORK: Failure = Failure::new(
    "no-network",
    "No network",
    "Leave the browser and run wificonnect, then ipconfig dhcp.",
);
pub const NO_SUCH_BUILTIN: Failure = Failure::new(
    "no-such-page",
    "No such page",
    "There is no built-in page at that address.",
);
