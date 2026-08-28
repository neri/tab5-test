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
use crate::browser::url::{Scheme, Url};
use crate::net::http::{self, Progress, Transaction};
use crate::net::pins;
use crate::net::tls::Authentication;
use crate::net::transport::Security;
use crate::net::{self, dns};
use crate::wifi;

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
    pub const fn new(name: &'static str, headline: &'static str, detail: &'static str) -> Failure {
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

/// What a page's connection proved, for the toolbar to show.
///
/// Not a boolean and not a scheme. `https://` says what was *asked* for;
/// this says what was *got*, and the two come apart exactly where it
/// matters -- an unauthenticated TLS connection is an `https://` URL whose
/// peer nobody identified.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum PageSecurity {
    /// Plaintext. Everything on the wire is readable by anyone carrying it.
    Cleartext,
    /// TLS, with the peer's identity unestablished.
    Tls(Authentication),
}

impl PageSecurity {
    /// What the toolbar's padlock means, in words.
    ///
    /// The whole sentence and not a label, because it is only ever read
    /// when somebody taps the lock to ask. `SECURE` appears nowhere:
    /// nothing this firmware can do earns it yet, and the one state that
    /// proved anything says exactly what it proved.
    pub fn explanation(self) -> &'static str {
        match self {
            Self::Cleartext => "INSECURE HTTP: plaintext; anyone carrying it can read it",
            Self::Tls(Authentication::Unverified) => {
                "TLS UNVERIFIED: encrypted, but nobody checked who answered"
            }
            Self::Tls(Authentication::Pinned) => {
                "TLS PINNED: the peer's key matches a pin built into this firmware"
            }
        }
    }

    /// Whether the badge is a warning rather than a statement.
    ///
    /// True for plaintext *and* for unauthenticated TLS. The second one is
    /// the whole reason this is not `self == Cleartext`: encryption without
    /// identity is not a safe state to display calmly.
    pub fn is_warning(self) -> bool {
        !matches!(self, Self::Tls(Authentication::Pinned))
    }
}

/// One page being fetched, from a name to a document.
#[must_use = "a Fetch owns a socket and has to be closed"]
pub struct Fetch {
    /// The address currently being fetched, which moves with each redirect.
    url: Url,
    /// What the hop currently open proved, once its handshake finished.
    security: Option<PageSecurity>,
    redirects: usize,
    query: Option<dns::Query>,
    transaction: Option<Transaction>,
    /// Built only once the head says the response is one to display, so a
    /// redirect or an error status never allocates a document.
    parser: Option<Parser>,
    /// A limit the document hit, which stops the transfer through the sink.
    document_error: Option<Error>,
    /// The status of a response that is being read but is not the page that
    /// was asked for -- anything outside 2xx that still sent HTML.
    ///
    /// `None` for a response that succeeded, so that "is this what was
    /// asked for" and "what number came back" stay one question. Cleared
    /// on every redirect: it belongs to the hop, not to the navigation.
    status: Option<u16>,
    /// Bytes received across every hop of this navigation.
    received: usize,
    /// The largest the parser was ever holding, for the memory budget.
    peak_owned: usize,
}

impl Fetch {
    /// Begins a fetch. The first byte does not leave until [`Fetch::step`].
    ///
    /// Network schemes only. `file:` is read by `app::localfile`, which
    /// shares this module's `Failure` and `Outcome` but none of its
    /// machinery -- there is no name to resolve, no socket to own and no
    /// status to interpret.
    pub fn start(url: Url, network: &mut Network<'_>) -> Result<Fetch, Failure> {
        if !url.scheme().is_network() {
            return Err(NOT_NETWORK);
        }
        let mut fetch = Fetch {
            url,
            security: None,
            redirects: 0,
            query: None,
            transaction: None,
            parser: None,
            document_error: None,
            status: None,
            received: 0,
            peak_owned: 0,
        };
        fetch.open(network)?;
        Ok(fetch)
    }

    /// The address being fetched now, which is the last hop of a redirect
    /// chain rather than the one the caller asked for.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// What the connection currently open proved, or `None` before a TLS
    /// handshake has finished.
    ///
    /// Plaintext answers immediately -- there is nothing to wait for and
    /// nothing to establish. TLS answers only once the handshake is over,
    /// which is what stops a toolbar showing a security state for a session
    /// that may still fail.
    pub fn security(&self) -> Option<PageSecurity> {
        self.security
    }

    /// The status of a page that came back but was not the one asked for.
    ///
    /// `None` when the response succeeded, which is every ordinary page.
    /// Set alongside [`Outcome::Page`] when a server answered outside 2xx
    /// and sent HTML anyway: that HTML is what it has to say about the
    /// refusal, and it is shown, with this number beside it.
    pub fn status(&self) -> Option<u16> {
        self.status
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
        // Every string the connection is made of comes from the same `Url`:
        // the address was resolved from `host()`, the SNI name is `host()`,
        // the `Host:` header is `host_header()` and the request target is
        // `request_target()`. Building any of them separately is how a
        // request ends up asking one host for another host's page.
        let security = match self.url.scheme() {
            Scheme::Http => Security::Plain,
            // Refused here as well as in `start`, so that a caller added
            // later cannot turn a `file:` URL into a connection to port 0.
            Scheme::File => return Err(NOT_NETWORK),
            Scheme::Https => Security::Tls {
                server_name: self.url.host(),
                // The pin table is keyed by the same string that goes in
                // SNI, so a host cannot be looked up under one name and
                // connected to under another.
                policy: pins::policy_for(self.url.host()),
            },
        };
        Transaction::start(
            network.stack,
            address,
            self.url.port(),
            host.as_bytes(),
            target.as_bytes(),
            MAX_DECODED_HTML_BYTES as u64,
            security,
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
    ///
    /// A redirect is the one place a page's security can change without
    /// anyone choosing it, so it is the one place the rules have to be
    /// applied. Both refusals are about going *down*:
    ///
    /// - `https` to `http` is a server asking to be read in the clear
    ///   instead. Refused, always. It is not a rule against the user typing
    ///   an `http://` address or following a link to one -- those are
    ///   choices, made with the address visible -- it is a rule against the
    ///   choice being made for them by the server they were talking to.
    /// - a pinned connection to a host with no pin is the same move one
    ///   level up: the identity that was established is being traded for
    ///   one that is not. Refused for the same reason.
    ///
    /// Going *up* -- `http` to `https` -- is allowed and unremarkable.
    fn redirect_to(&mut self, target: Url, network: &mut Network<'_>) -> Outcome {
        let from_scheme = self.url.scheme();
        let from_security = self.security;
        if let Some(transaction) = self.transaction.take() {
            self.received += transaction.close(network.stack, network.rpc).received;
        }
        self.parser = None;
        self.document_error = None;
        self.status = None;
        self.security = None;

        // Before the downgrade rules, because this one is not about
        // degrees of protection. A `Location: file:///...` is a server
        // asking the board to open its own filesystem and show what is in
        // it. Following an address the reader typed or a link they chose
        // is their decision with the address visible; this is the server
        // making it for them, which is exactly what a redirect must never
        // be allowed to do.
        if !target.scheme().is_network() {
            return Outcome::Failed(FILE_REDIRECT);
        }
        if from_scheme == Scheme::Https && target.scheme() == Scheme::Http {
            return Outcome::Failed(HTTPS_DOWNGRADE);
        }
        if from_security == Some(PageSecurity::Tls(Authentication::Pinned))
            && !self.host_is_pinned(&target)
        {
            return Outcome::Failed(TLS_AUTH_DOWNGRADE);
        }

        self.url = target;
        self.redirects += 1;
        match self.open(network) {
            Ok(()) => Outcome::Working,
            Err(failure) => Outcome::Failed(failure),
        }
    }

    /// Picks up what the open connection proved, once and not before.
    ///
    /// Plaintext is known the moment there is a connection: there is
    /// nothing to establish. TLS is not known until its handshake has
    /// finished, and asking earlier gets `None` -- which is the answer the
    /// toolbar needs, because a badge drawn from a handshake that has not
    /// happened is a badge that can turn out to have been wrong.
    fn note_security(&mut self) {
        if self.security.is_some() {
            return;
        }
        let Some(transaction) = self.transaction.as_ref() else {
            return;
        };
        self.security = match self.url.scheme() {
            Scheme::Http => Some(PageSecurity::Cleartext),
            Scheme::Https => transaction.authentication().map(PageSecurity::Tls),
            // Nothing was proved about anybody, because nobody was asked.
            // A local file's page gets the same lock a built-in page does.
            Scheme::File => None,
        };
    }

    /// Whether a redirect target is a host this firmware could authenticate.
    ///
    /// Being pinned is a property of the host, not of the connection that
    /// has not been made yet: a redirect to a pinned host is allowed to
    /// proceed and then has to satisfy that host's pins, and one to a host
    /// with no pins is the downgrade this refuses.
    fn host_is_pinned(&self, target: &Url) -> bool {
        target.scheme() == Scheme::Https && pins::is_pinned(target.host())
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
        self.note_security();

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
        // HTML, or text that is not HTML -- a `.txt`, a `.md`, a server's
        // own `manifest.txt`. Anything else has no reading this can give
        // it: an image is not text, and neither is a firmware image.
        let markup = head.is_html();
        if !markup && !head.is_text() {
            return Outcome::Failed(NOT_HTML.with_status(status));
        }
        // A status outside 2xx is read rather than refused, as long as what
        // came with it is HTML. A server's own 404 or 500 page is usually
        // the only thing that says which of the many possible reasons this
        // one was -- which resource, which parameter, which login -- and
        // discarding it left the reader with the viewer's four words and
        // nothing else. It is still not the page that was asked for, so
        // the number is carried out with it and the status line says so.
        //
        // The two failures above stay failures: a response that is not
        // HTTP, or not HTML, has nothing to show whatever its status.
        if !head.is_success() {
            self.status = status;
        }
        // Only now is a document allocated. A redirect chain, a non-HTML
        // response and a download all get this far without one.
        let built = if markup {
            Parser::new(self.url.clone())
        } else {
            Parser::plain(self.url.clone())
        };
        let mut parser = match built {
            Ok(parser) => parser,
            Err(_) => return Outcome::Failed(OUT_OF_MEMORY),
        };
        // The `charset` off the head, before any body byte reaches the
        // parser. A header outranks the document's own `<meta>`, and
        // saying so here is also what keeps the decoder from holding the
        // first kilobyte back to look for a `<meta>` that could not have
        // overruled it anyway. Borrowed rather than cloned: the head is
        // still alive here and its label is the only copy needed.
        if let Some(charset) = head.charset.as_deref() {
            parser.declare_charset(charset);
        }
        self.parser = Some(parser);
        Outcome::Working
    }

    fn complete(&mut self) -> Outcome {
        let Some(parser) = self.parser.take() else {
            return Outcome::Failed(EMPTY.with_status(self.status));
        };
        self.peak_owned = self.peak_owned.max(parser.owned_bytes());
        match parser.finish() {
            Ok(document) => {
                // An error status whose page turned out to have no text is
                // no better than no page at all: a blank screen and a
                // number in the status line does not tell a reader that
                // anything went wrong. The viewer's own error page does,
                // so that is what an empty refusal falls back to.
                if self.status.is_some() && document.text().trim().is_empty() {
                    return Outcome::Failed(REFUSED.with_status(self.status));
                }
                Outcome::Page(document)
            }
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
pub const FILE_REDIRECT: Failure = Failure::new(
    "file-redirect",
    "Refused to open a local file",
    "The server redirected to a `file:` address. Opening one is something \
     the reader does with the address in front of them, not something a \
     server gets to ask for.",
);
pub const HTTPS_DOWNGRADE: Failure = Failure::new(
    "https-downgrade",
    "Refused to leave HTTPS",
    "The server redirected a secure address to a plaintext one. Following \
     that would put the rest of this page on the wire in the clear without \
     anyone having chosen it.",
);
pub const TLS_AUTH_DOWNGRADE: Failure = Failure::new(
    "tls-auth-downgrade",
    "Refused to lose the pinned identity",
    "The server redirected an authenticated connection to a host this \
     firmware cannot authenticate.",
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
    "It sent no page to go with the refusal, or nothing that could be read \
     as one.",
);
pub const NOT_HTML: Failure = Failure::new(
    "not-html",
    "Not a page",
    "This is not HTML, and this viewer shows nothing else.",
);
pub const EMPTY: Failure = Failure::new("empty", "Empty response", "The server sent no page.");
pub const OUT_OF_MEMORY: Failure = Failure::new(
    "out-of-memory",
    "Out of memory",
    "There was not enough heap left to read this page.",
);
pub const NO_NETWORK: Failure = Failure::new(
    "no-network",
    "No network",
    "Tap the Wi-Fi bars at the right of the toolbar to choose a network.",
);
pub const NOT_NETWORK: Failure = Failure::new(
    "not-network",
    "Not something to fetch",
    "That address does not name anything on a network.",
);
pub const NO_SUCH_BUILTIN: Failure = Failure::new(
    "no-such-page",
    "No such page",
    "There is no built-in page at that address.",
);
