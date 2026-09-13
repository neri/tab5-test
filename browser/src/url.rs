//! HTTP URLs: parsing, validation, relative resolution and the two strings
//! that come out the other side.
//!
//! There is one rule this module exists to enforce, and it is the reason it
//! is a type rather than a few string functions: **what is shown to the user
//! and what is sent to the network come from the same value**. A viewer that
//! renders one string in the address bar and connects using a different one
//! parsed somewhere else is a viewer whose address bar can lie, and lying
//! about the host is the whole of a phishing attack. So [`Url::to_text`] and
//! [`Url::request_target`] are both projections of the same parsed
//! components, and nothing else in the browser is allowed to build either.
//!
//! The second rule is that nothing here decodes. `%2e` stays `%2e` and is
//! *not* a `.` for the purposes of `..` removal -- a server that
//! distinguishes them is entitled to, and collapsing them here would let a
//! reference walk out of a directory the server thought it was confined to.
//! Percent-encoding is only ever added (to non-ASCII bytes), never removed.
//!
//! Scope, from `docs/WEB_BROWSER_PLAN.md`: `http` connects, `https` parses
//! but is reported as unsupported rather than silently downgraded, and every
//! other scheme is refused. No userinfo, no IPv6 literals, no IDNA -- a
//! Unicode host is an error, though its punycode spelling can be typed.

use alloc::string::String;
use alloc::vec::Vec;

use crate::limits::MAX_URL_BYTES;
use crate::memory::{self, OutOfMemory};

pub fn decode_fragment(fragment: &str) -> Result<Option<String>, OutOfMemory> {
    let mut bytes = Vec::new();
    let raw = fragment.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            if i + 2 >= raw.len() {
                return Ok(None);
            }
            let Some(hi) = hex(raw[i + 1]) else {
                return Ok(None);
            };
            let Some(lo) = hex(raw[i + 2]) else {
                return Ok(None);
            };
            memory::push(&mut bytes, (hi << 4) | lo)?;
            i += 3;
        } else {
            memory::push(&mut bytes, raw[i])?;
            i += 1;
        }
    }
    Ok(String::from_utf8(bytes).ok())
}
fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

/// The schemes this recognises.
///
/// All three are fetchable, and the distinction is what decides how. A
/// value that says `Https` cannot be handed to the plaintext path by
/// accident, whereas a rewritten `http://` one could be, and a redirect
/// that changes this field from `Https` to `Http` is a redirect the viewer
/// refuses. `File` is not a network scheme at all: nothing about it goes
/// near a socket, and a redirect *to* it is refused outright -- a server
/// must never be able to steer the board into its own filesystem.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scheme {
    Http,
    Https,
    /// A file on a mounted volume. No host, no port, no network.
    File,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
            Scheme::File => "file",
        }
    }

    /// The port a URL of this scheme gets when it does not name one.
    ///
    /// Zero for `file`, which has no port at all. It is not a port that
    /// happens to be zero -- nothing ever connects to it -- and
    /// `has_default_port` is what keeps it out of the displayed address.
    pub fn default_port(self) -> u16 {
        match self {
            Scheme::Http => 80,
            Scheme::Https => 443,
            Scheme::File => 0,
        }
    }

    /// Whether a URL of this scheme has an authority at all.
    ///
    /// `file:` does not. RFC 8089 allows an empty one or `localhost`, and
    /// both mean the same thing: the machine this is running on.
    pub fn has_authority(self) -> bool {
        !matches!(self, Scheme::File)
    }

    /// Whether fetching this reaches the network.
    pub fn is_network(self) -> bool {
        !matches!(self, Scheme::File)
    }

    /// Whether the transport for this scheme authenticates nothing at all.
    ///
    /// True for `http`, and *also* true for `https` in this firmware:
    /// unauthenticated TLS stops passive eavesdropping and stops nothing
    /// else (`docs/TLS_PLAN.md`). Which of the two a page actually got is
    /// not a property of the scheme, so nothing here can answer it -- the
    /// connection's own authentication state has to.
    pub fn is_cleartext(self) -> bool {
        matches!(self, Scheme::Http)
    }
}

/// Why a URL was refused.
///
/// Every variant is something the user is shown, which is why they are
/// this specific: "bad URL" leaves someone staring at a link that looks
/// fine, while "this URL has a username in it, which is not supported"
/// tells them what to do about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// Nothing but whitespace.
    Empty,
    /// Past [`MAX_URL_BYTES`], as written or once percent-encoded.
    TooLong,
    /// A scheme that is none of `http`, `https` and `file`.
    UnsupportedScheme,
    /// `http://` with nothing after it, or `file:` with a path that is not
    /// absolute.
    MissingHost,
    /// `file://somewhere/x`. A `file:` URL names a file on this machine,
    /// and there is no other machine it could name instead: reading the
    /// local file anyway would answer a question nobody asked.
    RemoteFileHost,
    /// Not an ASCII DNS name and not a dotted-quad IPv4 address.
    InvalidHost,
    /// `user:password@host`. Credentials over cleartext HTTP are not
    /// something this sends, so the URL is refused rather than stripped.
    HasUserinfo,
    /// `[::1]`. IPv6 is out of scope for the whole network stack, so this
    /// is refused where it is legible rather than failing later at connect.
    Ipv6Literal,
    /// A port that is not digits, or does not fit in 16 bits.
    InvalidPort,
    /// A space, a control character, CR or LF. Any of these in a request
    /// target is a way to inject a second request line or a header.
    ForbiddenCharacter,
    /// A relative reference where no base URL was available.
    NoBase,
    OutOfMemory,
}

impl From<OutOfMemory> for Error {
    fn from(_: OutOfMemory) -> Self {
        Error::OutOfMemory
    }
}

/// A short ASCII sentence for the status line.
pub fn error_text(error: Error) -> &'static str {
    match error {
        Error::Empty => "the address is empty",
        Error::TooLong => "the address is too long",
        Error::UnsupportedScheme => "only http://, https:// and file:/// addresses are understood",
        Error::MissingHost => "the address has no host",
        Error::RemoteFileHost => "a file:// address can only name this machine",
        Error::InvalidHost => "the host is not an ASCII name or an IPv4 address",
        Error::HasUserinfo => "addresses carrying a user name are not supported",
        Error::Ipv6Literal => "IPv6 addresses are not supported",
        Error::InvalidPort => "the port is not a number below 65536",
        Error::ForbiddenCharacter => "the address contains a space or a control character",
        Error::NoBase => "a relative link with no page to resolve it against",
        Error::OutOfMemory => "out of memory while parsing the address",
    }
}

/// What shape a reference has, before it is resolved.
///
/// This is exposed because the browser acts on the distinction rather than
/// just resolving and forgetting: a [`Reference::Fragment`] link moves
/// inside the current page instead of fetching anything, and telling the
/// user that fragments are not implemented yet is only possible if the
/// difference survived resolution.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reference {
    /// `http://host/path`
    Absolute,
    /// `//host/path` -- inherits the base's scheme.
    SchemeRelative,
    /// `/path` -- inherits scheme, host and port.
    Root,
    /// `path`, `./path`, `../path`
    Relative,
    /// `?query` -- same document, different query.
    Query,
    /// `#fragment` -- the same document.
    Fragment,
    /// `` -- the same document, fragment dropped.
    Same,
}

/// Classifies a reference without resolving it.
///
/// Leading and trailing ASCII whitespace is ignored, the same way
/// [`Url::resolve`] ignores it: markup wraps `href` values across lines and
/// treating that as a different kind of reference than the same value on
/// one line would be a distinction with no meaning.
pub fn classify(reference: &str) -> Reference {
    let reference = reference.trim_matches(is_ascii_whitespace);
    if reference.is_empty() {
        return Reference::Same;
    }
    if split_scheme(reference).is_some() {
        return Reference::Absolute;
    }
    if reference.starts_with("//") {
        return Reference::SchemeRelative;
    }
    match reference.as_bytes()[0] {
        b'/' => Reference::Root,
        b'?' => Reference::Query,
        b'#' => Reference::Fragment,
        _ => Reference::Relative,
    }
}

/// A parsed absolute HTTP or HTTPS URL.
///
/// The components are private on purpose. `path` always begins with `/`,
/// `host` is always lowercase ASCII, `port` is always resolved (never
/// "absent, meaning the default"), and `query`/`fragment` never include
/// their leading punctuation. Those invariants are what let
/// [`Url::request_target`] be a concatenation rather than a parser, and
/// they only hold if the only way to make one of these is through
/// [`Url::parse`] or [`Url::resolve`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Url {
    scheme: Scheme,
    host: String,
    port: u16,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

impl Url {
    /// Parses an absolute URL. A reference without a scheme is refused
    /// here; use [`Url::resolve`] when there is a page to resolve against.
    pub fn parse(text: &str) -> Result<Url, Error> {
        let text = text.trim_matches(is_ascii_whitespace);
        if text.is_empty() {
            return Err(Error::Empty);
        }
        if text.len() > MAX_URL_BYTES {
            return Err(Error::TooLong);
        }
        reject_forbidden(text)?;
        let Some((scheme, rest)) = split_scheme(text) else {
            return Err(Error::UnsupportedScheme);
        };
        let scheme = match_scheme(scheme)?;
        let (host, port, remainder) = if scheme.has_authority() {
            let Some(rest) = rest.strip_prefix("//") else {
                // `http:example.com` is legal generic-URI syntax and means
                // something quite different from `http://example.com`.
                // Rather than guess which was meant, refuse it: an address
                // bar entry that reached here is a typo, and a link that
                // did is broken.
                return Err(Error::MissingHost);
            };
            let (authority, remainder) = split_authority(rest);
            let (host, port) = parse_authority(authority, scheme)?;
            (host, port, remainder)
        } else {
            // `file:` has no authority. RFC 8089 writes it `file:///path`
            // with an empty one; `file://localhost/path` means the same
            // machine and is accepted as the same thing; and `file:/path`
            // -- no slashes at all -- is what a person types, so it is
            // taken rather than refused. All three end up identical, which
            // is what stops the same file having three addresses.
            (String::new(), 0, strip_file_authority(rest)?)
        };
        let (path, query, fragment) = split_path_query_fragment(remainder);
        let url = Url {
            scheme,
            host,
            port,
            path: normalize_path(path)?,
            query: encode_optional(query)?,
            fragment: encode_optional(fragment)?,
        };
        url.check_length()?;
        Ok(url)
    }

    /// Parses an address a person typed, supplying `http://` when they did
    /// not.
    ///
    /// Separate from [`Url::parse`] because the two have different callers
    /// and must keep different rules. `parse` is what an `href`, a
    /// `Location:` header and a redirect target go through, and a missing
    /// scheme there is a broken document that must stay an error. This one
    /// is only ever reached from a keyboard -- the shell's `browser` and
    /// `hs` commands, and the viewer's address field -- where a scheme is
    /// seven characters of ceremony on a thumb keyboard and no other scheme
    /// could have been meant.
    ///
    /// What counts as "already has a scheme" is [`has_scheme`], and the
    /// distinction it draws is the whole reason this is not a `starts_with`
    /// at each call site.
    pub fn parse_typed(text: &str) -> Result<Url, Error> {
        let text = text.trim_matches(is_ascii_whitespace);
        if text.is_empty() {
            return Err(Error::Empty);
        }
        if has_scheme(text) {
            return Url::parse(text);
        }
        // `//host/path` says "the scheme of wherever this came from", and
        // what it came from here is a keyboard. Only the scheme is added,
        // so the `//` it already has is the one that gets used.
        let prefix = if text.starts_with("//") {
            "http:"
        } else {
            "http://"
        };
        let mut completed = memory::string_with_capacity(prefix.len() + text.len())?;
        memory::push_str(&mut completed, prefix)?;
        memory::push_str(&mut completed, text)?;
        Url::parse(&completed)
    }

    /// Resolves `reference` against this URL, following RFC 3986's rules
    /// for every shape in [`Reference`].
    ///
    /// The result is always absolute, so the caller never has to ask
    /// whether it got a relative one back.
    pub fn resolve(&self, reference: &str) -> Result<Url, Error> {
        let reference = reference.trim_matches(is_ascii_whitespace);
        if reference.len() > MAX_URL_BYTES {
            return Err(Error::TooLong);
        }
        reject_forbidden(reference)?;
        let resolved = match classify(reference) {
            Reference::Absolute => return Url::parse(reference),
            Reference::SchemeRelative => {
                // `//host/path` on a `file:` page would mean a file URL
                // with an authority, which is not a thing this has: there
                // is no host to inherit and nothing sensible to resolve
                // against.
                if !self.scheme.has_authority() {
                    return Err(Error::MissingHost);
                }
                // Inherit only the scheme. Everything after `//` is a fresh
                // authority, so a `//other.example/` link genuinely leaves
                // this host -- which is why it cannot be treated as a path.
                let rest = &reference[2..];
                let (authority, remainder) = split_authority(rest);
                let (host, port) = parse_authority(authority, self.scheme)?;
                let (path, query, fragment) = split_path_query_fragment(remainder);
                Url {
                    scheme: self.scheme,
                    host,
                    port,
                    path: normalize_path(path)?,
                    query: encode_optional(query)?,
                    fragment: encode_optional(fragment)?,
                }
            }
            Reference::Root => {
                let (path, query, fragment) = split_path_query_fragment(reference);
                Url {
                    scheme: self.scheme,
                    host: memory::string_from(&self.host)?,
                    port: self.port,
                    path: normalize_path(path)?,
                    query: encode_optional(query)?,
                    fragment: encode_optional(fragment)?,
                }
            }
            Reference::Relative => {
                let (path, query, fragment) = split_path_query_fragment(reference);
                let merged = self.merge_path(path)?;
                Url {
                    scheme: self.scheme,
                    host: memory::string_from(&self.host)?,
                    port: self.port,
                    path: remove_dot_segments(&merged)?,
                    query: encode_optional(query)?,
                    fragment: encode_optional(fragment)?,
                }
            }
            Reference::Query => {
                let (_, query, fragment) = split_path_query_fragment(reference);
                Url {
                    scheme: self.scheme,
                    host: memory::string_from(&self.host)?,
                    port: self.port,
                    path: memory::string_from(&self.path)?,
                    query: encode_optional(query)?,
                    fragment: encode_optional(fragment)?,
                }
            }
            Reference::Fragment => {
                let mut url = self.clone_components()?;
                url.fragment = encode_optional(Some(&reference[1..]))?;
                url
            }
            Reference::Same => {
                // An empty reference is the current document *without* its
                // fragment, per RFC 3986 5.3. `<a href="">` is a reload.
                let mut url = self.clone_components()?;
                url.fragment = None;
                url
            }
        };
        resolved.check_length()?;
        Ok(resolved)
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Always begins with `/`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Without the leading `?`.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// Returns this URL with its query replaced and fragment removed.
    /// Used by HTML GET forms, whose successful controls replace rather
    /// than append to the action URL's query.
    pub fn with_query(&self, query: String) -> Result<Self, Error> {
        let mut result = self.clone_components()?;
        result.query = Some(query);
        result.fragment = None;
        if result.text_length() > MAX_URL_BYTES {
            return Err(Error::TooLong);
        }
        Ok(result)
    }

    /// Returns this URL without its fragment, preserving its query.
    /// HTTP never sends a fragment; form submission also uses the returned
    /// value as the address of the response document.
    pub fn without_fragment(&self) -> Result<Self, Error> {
        let mut result = self.clone_components()?;
        result.fragment = None;
        Ok(result)
    }

    /// Without the leading `#`.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// Whether the port is the scheme's default, which is what decides
    /// whether [`Url::to_text`] shows it.
    pub fn has_default_port(&self) -> bool {
        self.port == self.scheme.default_port()
    }

    /// Whether the two URLs name the same document, ignoring fragments.
    ///
    /// This is what makes a fragment-only link a same-page move rather
    /// than a fetch.
    pub fn same_document(&self, other: &Url) -> bool {
        self.scheme == other.scheme
            && self.host == other.host
            && self.port == other.port
            && self.path == other.path
            && self.query == other.query
    }

    /// Scheme, host and effective port -- the boundary across which a POST
    /// body must not be resent without the reader's approval.
    pub fn same_origin(&self, other: &Url) -> bool {
        self.scheme == other.scheme && self.host == other.host && self.port == other.port
    }

    /// The origin-form request target: path, then query. **Never** the
    /// fragment -- that is client-side only, and sending it would tell the
    /// server something the user did not ask to disclose.
    pub fn request_target(&self) -> Result<String, Error> {
        let length = self.path.len() + self.query.as_ref().map_or(0, |q| q.len() + 1);
        let mut target = memory::string_with_capacity(length)?;
        memory::push_str(&mut target, &self.path)?;
        if let Some(query) = &self.query {
            memory::push_str(&mut target, "?")?;
            memory::push_str(&mut target, query)?;
        }
        Ok(target)
    }

    /// What goes in the `Host:` header: the name as written, with the port
    /// only when it is not the default.
    ///
    /// The name and not the resolved address, deliberately. A server
    /// hosting several sites on one address picks the site from this
    /// header, so sending the address asks for whichever one is default.
    pub fn host_header(&self) -> Result<String, Error> {
        let mut value = memory::string_with_capacity(self.host.len() + self.port_suffix_length())?;
        memory::push_str(&mut value, &self.host)?;
        if !self.has_default_port() {
            memory::push_str(&mut value, ":")?;
            push_decimal(&mut value, self.port)?;
        }
        Ok(value)
    }

    /// The same URL with its path ending in `/`.
    ///
    /// This is not cosmetic. A relative reference resolves against
    /// everything up to the base's **last** `/`, so `notes.txt` beside
    /// `file:///tmp` is `file:///notes.txt` and beside `file:///tmp/` it is
    /// `file:///tmp/notes.txt`. A directory's page is a page of relative
    /// links, so its own address has to be the second spelling or every
    /// link on it points one level too high.
    ///
    /// The query and the fragment are dropped: they belong to the reference
    /// that was typed, not to the directory, and leaving them on the base
    /// would put them on the address the reader is shown as well.
    pub fn as_directory(&self) -> Result<Url, Error> {
        if self.path.ends_with('/') {
            return Ok(self.clone());
        }
        let mut path = memory::string_with_capacity(self.path.len() + 1)?;
        memory::push_str(&mut path, &self.path)?;
        memory::push_str(&mut path, "/")?;
        let url = Url {
            scheme: self.scheme,
            host: memory::string_from(&self.host)?,
            port: self.port,
            path,
            query: None,
            fragment: None,
        };
        url.check_length()?;
        Ok(url)
    }

    /// The whole URL as text, for the address bar and for link targets in
    /// the status line. Includes the fragment; hides a default port.
    pub fn to_text(&self) -> Result<String, Error> {
        let mut text = memory::string_with_capacity(self.text_length())?;
        memory::push_str(&mut text, self.scheme.as_str())?;
        // `file:///path`: the empty authority is written out, because
        // `file:/path` and `file://host/path` are different productions in
        // the grammar and only the three-slash form is unambiguous.
        memory::push_str(&mut text, "://")?;
        memory::push_str(&mut text, &self.host)?;
        if !self.has_default_port() {
            memory::push_str(&mut text, ":")?;
            push_decimal(&mut text, self.port)?;
        }
        memory::push_str(&mut text, &self.path)?;
        if let Some(query) = &self.query {
            memory::push_str(&mut text, "?")?;
            memory::push_str(&mut text, query)?;
        }
        if let Some(fragment) = &self.fragment {
            memory::push_str(&mut text, "#")?;
            memory::push_str(&mut text, fragment)?;
        }
        Ok(text)
    }

    /// Sum of the capacities this holds, for the owned-memory budget.
    ///
    /// A thousand links is a thousand of these, so they are counted rather
    /// than assumed small: a URL is bounded at 2 KiB and that bound is only
    /// meaningful if somebody adds it up.
    pub fn owned_bytes(&self) -> usize {
        core::mem::size_of::<Url>()
            + self.host.capacity()
            + self.path.capacity()
            + self.query.as_ref().map_or(0, |value| value.capacity())
            + self.fragment.as_ref().map_or(0, |value| value.capacity())
    }

    /// The host as four octets when it was written as a dotted quad, so
    /// that a literal address skips name resolution entirely.
    pub fn ipv4(&self) -> Option<[u8; 4]> {
        parse_ipv4(&self.host)
    }

    fn clone_components(&self) -> Result<Url, Error> {
        Ok(Url {
            scheme: self.scheme,
            host: memory::string_from(&self.host)?,
            port: self.port,
            path: memory::string_from(&self.path)?,
            query: match &self.query {
                Some(query) => Some(memory::string_from(query)?),
                None => None,
            },
            fragment: None,
        })
    }

    /// RFC 3986 5.2.3: everything up to and including the base path's last
    /// `/`, then the reference.
    fn merge_path(&self, reference: &str) -> Result<String, Error> {
        let base = match self.path.rfind('/') {
            Some(index) => &self.path[..=index],
            // The invariant says this cannot happen -- `path` always starts
            // with `/`. Treat it as the root rather than panicking.
            None => "/",
        };
        let mut merged = memory::string_with_capacity(base.len() + reference.len())?;
        memory::push_str(&mut merged, base)?;
        encode_into(&mut merged, reference)?;
        Ok(merged)
    }

    /// `:port`, or nothing when the port is the scheme's default.
    ///
    /// Exact rather than a worst case, because [`Url::check_length`] uses
    /// it: a bound that is six bytes pessimistic would refuse URLs that fit.
    fn port_suffix_length(&self) -> usize {
        if self.has_default_port() {
            0
        } else {
            1 + decimal_length(self.port)
        }
    }

    fn text_length(&self) -> usize {
        self.scheme.as_str().len()
            + 3
            + self.host.len()
            + self.port_suffix_length()
            + self.path.len()
            + self.query.as_ref().map_or(0, |q| q.len() + 1)
            + self.fragment.as_ref().map_or(0, |f| f.len() + 1)
    }

    /// The bound applies to the URL as it will be shown and stored, not to
    /// the text it was parsed from: percent-encoding a non-ASCII path makes
    /// it longer, and it is the longer form that gets kept.
    fn check_length(&self) -> Result<(), Error> {
        if self.text_length() > MAX_URL_BYTES {
            return Err(Error::TooLong);
        }
        Ok(())
    }
}

fn is_ascii_whitespace(value: char) -> bool {
    matches!(value, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

/// Rejects everything that must never reach a request line.
///
/// The space and the two line endings are the dangerous ones -- each can
/// end the request target early and start something the user did not ask
/// for -- but the whole control range goes with them, because there is no
/// URL in which a `\x01` is meaningful and allowing it only widens what has
/// to be reasoned about downstream. DEL is included for the same reason.
///
/// This runs on the input before any parsing, so it covers the host, the
/// path and the query in one pass and nothing can slip in between stages.
fn reject_forbidden(text: &str) -> Result<(), Error> {
    for &byte in text.as_bytes() {
        if byte <= 0x20 || byte == 0x7F {
            return Err(Error::ForbiddenCharacter);
        }
    }
    Ok(())
}

/// Whether `text` begins with something that has to be read as a scheme.
///
/// Not the same question as "does it contain a colon", and not the same
/// answer as [`classify`] gives either. `localhost:8080/x` splits exactly
/// like a scheme does -- `split_scheme` returns `("localhost", "8080/x")`,
/// so `classify` calls it [`Reference::Absolute`] -- but nobody typing it
/// meant a scheme called `localhost`. The `//` is what separates the two
/// cases, because a URL that names an authority always has it.
///
/// `http:` and `https:` count as schemes with or without the `//`, so
/// `http:example.com` keeps its own "no host" error instead of being
/// completed into an address nobody typed.
pub fn has_scheme(text: &str) -> bool {
    let text = text.trim_matches(is_ascii_whitespace);
    match split_scheme(text) {
        Some((scheme, rest)) => rest.starts_with("//") || match_scheme(scheme).is_ok(),
        None => false,
    }
}

/// Splits `scheme:` off the front, if the text starts with a valid one.
///
/// Returns `None` rather than an error for anything that is not
/// scheme-shaped: `foo.html:8080` is not a scheme, it is a relative
/// reference, and the caller decides what to do about that.
fn split_scheme(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    for (index, &byte) in bytes.iter().enumerate() {
        match byte {
            b':' => return Some((&text[..index], &text[index + 1..])),
            b if b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.' => {}
            _ => return None,
        }
    }
    None
}

fn match_scheme(scheme: &str) -> Result<Scheme, Error> {
    if scheme.eq_ignore_ascii_case("http") {
        Ok(Scheme::Http)
    } else if scheme.eq_ignore_ascii_case("https") {
        Ok(Scheme::Https)
    } else if scheme.eq_ignore_ascii_case("file") {
        Ok(Scheme::File)
    } else {
        Err(Error::UnsupportedScheme)
    }
}

/// Takes the empty (or `localhost`) authority off a `file:` URL and
/// returns the path.
///
/// The three spellings RFC 8089 and everyday use produce:
///
/// ```text
/// file:///tmp/notes.txt          the canonical form
/// file://localhost/tmp/notes.txt the same machine, named
/// file:/tmp/notes.txt            what a person types
/// ```
///
/// Anything else after `file://` is a host, and there is no host here to
/// reach: a `file://other-machine/x` that silently read the local file
/// would be answering a question nobody asked.
fn strip_file_authority(rest: &str) -> Result<&str, Error> {
    let Some(rest) = rest.strip_prefix("//") else {
        // `file:/path`, or `file:path`. The second is a relative reference
        // with a scheme on it, which is not something to guess at.
        return if rest.starts_with('/') {
            Ok(rest)
        } else {
            Err(Error::MissingHost)
        };
    };
    let (authority, remainder) = split_authority(rest);
    if authority.is_empty() || authority.eq_ignore_ascii_case("localhost") {
        Ok(remainder)
    } else {
        Err(Error::RemoteFileHost)
    }
}

/// Splits the authority from what follows it. The authority ends at the
/// first `/`, `?` or `#`; the remainder keeps that delimiter.
fn split_authority(rest: &str) -> (&str, &str) {
    match rest.find(['/', '?', '#']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    }
}

fn parse_authority(authority: &str, scheme: Scheme) -> Result<(String, u16), Error> {
    if authority.is_empty() {
        return Err(Error::MissingHost);
    }
    if authority.contains('@') {
        return Err(Error::HasUserinfo);
    }
    if authority.starts_with('[') || authority.contains(']') {
        return Err(Error::Ipv6Literal);
    }
    // The last colon, not the first: there is no userinfo left by this
    // point, so any colon before the last would be inside a host, which is
    // not a host this accepts anyway.
    let (host, port) = match authority.rfind(':') {
        Some(index) => {
            let port = &authority[index + 1..];
            // `host:` with nothing after it means the default port, which
            // is what an empty authority port means in RFC 3986 too.
            let port = if port.is_empty() {
                scheme.default_port()
            } else {
                parse_port(port)?
            };
            (&authority[..index], port)
        }
        None => (authority, scheme.default_port()),
    };
    Ok((validate_host(host)?, port))
}

fn parse_port(text: &str) -> Result<u16, Error> {
    let mut value: u32 = 0;
    for &byte in text.as_bytes() {
        if !byte.is_ascii_digit() {
            return Err(Error::InvalidPort);
        }
        value = value * 10 + u32::from(byte - b'0');
        if value > u16::MAX as u32 {
            return Err(Error::InvalidPort);
        }
    }
    Ok(value as u16)
}

/// Longest DNS name, as text: 253 characters plus the implicit root.
const MAX_HOST_BYTES: usize = 253;
/// Longest single DNS label.
const MAX_LABEL_BYTES: usize = 63;

/// Checks a host and returns it lowercased.
///
/// ASCII only, and no IDNA. A Unicode host would have to be punycoded to
/// be resolvable, and doing that correctly is a Unicode normalisation
/// problem with a homograph-attack surface attached -- far more than a
/// hypertext viewer needs, and worse than useless done approximately. The
/// punycode spelling can still be typed or linked to, because that is
/// already ASCII.
///
/// Underscores are accepted despite not being legal in a hostname: they
/// turn up in LAN names handed out by consumer routers, which is exactly
/// the network this board is on.
fn validate_host(host: &str) -> Result<String, Error> {
    if host.is_empty() {
        return Err(Error::MissingHost);
    }
    // A single trailing dot is the DNS root and is dropped rather than
    // refused, so `example.com.` and `example.com` reach the same server.
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.len() > MAX_HOST_BYTES {
        return Err(Error::InvalidHost);
    }
    // Something made only of digits and dots is meant to be an address. If
    // it is not a valid one it is refused rather than looked up as a name:
    // `999.1.1.1` is a typo, and asking DNS about it is not what the user
    // wanted.
    let numeric = host
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.');
    if numeric {
        return match parse_ipv4(host) {
            Some(_) => memory::string_from(host).map_err(Error::from),
            None => Err(Error::InvalidHost),
        };
    }
    for label in host.split('.') {
        if label.is_empty() || label.len() > MAX_LABEL_BYTES {
            return Err(Error::InvalidHost);
        }
        for &byte in label.as_bytes() {
            if !(byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_') {
                return Err(Error::InvalidHost);
            }
        }
    }
    let mut lowered = memory::string_with_capacity(host.len())?;
    for value in host.chars() {
        memory::push_char(&mut lowered, value.to_ascii_lowercase())?;
    }
    Ok(lowered)
}

/// A dotted quad, strictly: four fields, decimal, no leading zeros.
///
/// Leading zeros are refused rather than read as decimal, because `0177.1`
/// means something in `inet_aton` and something else here, and a host that
/// resolves differently depending on which library reads it is a way to
/// get a request sent somewhere the address bar does not show.
fn parse_ipv4(host: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut count = 0;
    for field in host.split('.') {
        if count == 4 || field.is_empty() || field.len() > 3 {
            return None;
        }
        if field.len() > 1 && field.starts_with('0') {
            return None;
        }
        let mut value: u32 = 0;
        for &byte in field.as_bytes() {
            if !byte.is_ascii_digit() {
                return None;
            }
            value = value * 10 + u32::from(byte - b'0');
        }
        if value > 255 {
            return None;
        }
        octets[count] = value as u8;
        count += 1;
    }
    if count == 4 { Some(octets) } else { None }
}

/// Splits what follows the authority into path, query and fragment, each
/// still undecoded and without its leading delimiter.
fn split_path_query_fragment(rest: &str) -> (&str, Option<&str>, Option<&str>) {
    let (before_fragment, fragment) = match rest.find('#') {
        Some(index) => (&rest[..index], Some(&rest[index + 1..])),
        None => (rest, None),
    };
    let (path, query) = match before_fragment.find('?') {
        Some(index) => (
            &before_fragment[..index],
            Some(&before_fragment[index + 1..]),
        ),
        None => (before_fragment, None),
    };
    (path, query, fragment)
}

/// An absolute path with `.` and `..` removed, defaulting to `/`.
fn normalize_path(path: &str) -> Result<String, Error> {
    if path.is_empty() {
        return Ok(memory::string_from("/")?);
    }
    let mut encoded = memory::string_with_capacity(path.len())?;
    encode_into(&mut encoded, path)?;
    remove_dot_segments(&encoded)
}

fn encode_optional(text: Option<&str>) -> Result<Option<String>, Error> {
    match text {
        Some(text) => {
            let mut encoded = memory::string_with_capacity(text.len())?;
            encode_into(&mut encoded, text)?;
            Ok(Some(encoded))
        }
        None => Ok(None),
    }
}

/// Copies `text`, percent-encoding every byte outside ASCII.
///
/// Only non-ASCII. Everything already in the ASCII range is passed through
/// untouched -- including `%`, which is what keeps this from double-encoding
/// a reference that was already encoded, and which is why `%2e` survives to
/// [`remove_dot_segments`] as three characters rather than as a dot.
///
/// The bytes below `0x21` cannot appear: [`reject_forbidden`] has already
/// refused them.
fn encode_into(target: &mut String, text: &str) -> Result<(), Error> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in text.as_bytes() {
        if byte.is_ascii() {
            memory::push_char(target, byte as char)?;
            continue;
        }
        memory::push_char(target, '%')?;
        memory::push_char(target, HEX[(byte >> 4) as usize] as char)?;
        memory::push_char(target, HEX[(byte & 0x0F) as usize] as char)?;
    }
    Ok(())
}

/// RFC 3986 5.2.4, on a path that is already percent-encoded.
///
/// Written as the RFC's loop rather than as a split/filter over segments
/// because the edge cases -- a path ending in `..`, a `..` that would climb
/// above the root, an empty segment -- are exactly what the RFC's phrasing
/// is careful about and what a segment filter tends to get subtly wrong.
///
/// Climbing above the root is a no-op rather than an error: `/../../x`
/// resolves to `/x`, which is what every other client does, and treating it
/// as an attack would break links that are merely sloppy.
fn remove_dot_segments(path: &str) -> Result<String, Error> {
    // The result is never longer than the input: every rule either drops
    // characters or moves a segment across unchanged. The `+ 1` covers the
    // leading slash an input that has none would gain at the end.
    let mut output = memory::string_with_capacity(path.len() + 1)?;
    let mut input = path;
    while !input.is_empty() {
        // Each of the RFC's "replace the prefix with `/`" rules is done by
        // advancing past everything in the prefix *except* its final slash,
        // which leaves that slash at the front of the remaining input. That
        // is the same string the RFC describes, without building one.
        if input.starts_with("../") {
            input = &input[3..];
        } else if input.starts_with("./") {
            input = &input[2..];
        } else if input.starts_with("/./") {
            input = &input[2..];
        } else if input == "/." {
            input = "/";
        } else if input.starts_with("/../") {
            remove_last_segment(&mut output);
            input = &input[3..];
        } else if input == "/.." {
            remove_last_segment(&mut output);
            input = "/";
        } else if input == "." || input == ".." {
            input = "";
        } else {
            // Move one segment: the leading slash, if any, plus everything
            // up to but not including the next one.
            let start = usize::from(input.starts_with('/'));
            let end = match input[start..].find('/') {
                Some(index) => start + index,
                None => input.len(),
            };
            memory::push_str(&mut output, &input[..end])?;
            input = &input[end..];
        }
    }
    if !output.starts_with('/') {
        let mut rooted = memory::string_with_capacity(output.len() + 1)?;
        memory::push_str(&mut rooted, "/")?;
        memory::push_str(&mut rooted, &output)?;
        return Ok(rooted);
    }
    Ok(output)
}

/// Drops the last `/segment` from `output`, per the RFC's "remove the last
/// segment and its preceding `/`".
///
/// An output with no slash left in it is cleared rather than left alone:
/// that is the `..` which would climb above the root, and every other
/// client treats it as a no-op rather than as an error, so links that are
/// merely sloppy keep working.
fn remove_last_segment(output: &mut String) {
    match output.rfind('/') {
        Some(index) => output.truncate(index),
        None => output.clear(),
    }
}

fn decimal_length(value: u16) -> usize {
    match value {
        0..=9 => 1,
        10..=99 => 2,
        100..=999 => 3,
        1000..=9999 => 4,
        _ => 5,
    }
}

fn push_decimal(target: &mut String, value: u16) -> Result<(), Error> {
    let mut digits = [0u8; 5];
    let mut count = 0;
    let mut remaining = value;
    loop {
        digits[count] = b'0' + (remaining % 10) as u8;
        count += 1;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    for index in (0..count).rev() {
        memory::push_char(target, digits[index] as char)?;
    }
    Ok(())
}

/// Splits a URL list the way the address bar does, for tests and for the
/// diagnostic walk: one URL per line, blank lines and `#` comments ignored.
///
/// Here rather than in the firmware because the manifest the on-device
/// walk reads is fetched over HTTP and parsed by the same code the tests
/// exercise.
pub fn split_manifest(text: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_matches(is_ascii_whitespace);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        entries.push(line);
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    fn parsed(text: &str) -> Url {
        Url::parse(text).unwrap_or_else(|error| panic!("{text}: {error:?}"))
    }

    fn text_of(url: &Url) -> String {
        url.to_text().unwrap()
    }

    /// The base every relative-reference case below resolves against.
    fn base() -> Url {
        parsed("http://example.com/a/b/c.html?x=1#frag")
    }

    fn resolved(reference: &str) -> String {
        text_of(&base().resolve(reference).unwrap())
    }

    // --- absolute parsing -------------------------------------------------

    #[test]
    fn parses_the_ordinary_shape() {
        let url = parsed("http://example.com/path/page.html?a=1&b=2#part");
        assert_eq!(url.scheme(), Scheme::Http);
        assert_eq!(url.host(), "example.com");
        assert_eq!(url.port(), 80);
        assert_eq!(url.path(), "/path/page.html");
        assert_eq!(url.query(), Some("a=1&b=2"));
        assert_eq!(url.fragment(), Some("part"));
    }

    #[test]
    fn default_ports_come_from_the_scheme() {
        assert_eq!(parsed("http://example.com/").port(), 80);
        assert_eq!(parsed("https://example.com/").port(), 443);
        assert!(parsed("http://example.com/").has_default_port());
        assert!(!parsed("http://example.com:8080/").has_default_port());
    }

    #[test]
    fn an_empty_path_becomes_a_slash() {
        let url = parsed("http://example.com");
        assert_eq!(url.path(), "/");
        assert_eq!(url.request_target().unwrap(), "/");
        assert_eq!(text_of(&url), "http://example.com/");
    }

    #[test]
    fn an_explicit_default_port_is_not_shown_again() {
        assert_eq!(
            text_of(&parsed("http://example.com:80/x")),
            "http://example.com/x"
        );
        assert_eq!(
            text_of(&parsed("http://example.com:8080/x")),
            "http://example.com:8080/x"
        );
    }

    #[test]
    fn an_empty_port_means_the_default() {
        assert_eq!(parsed("http://example.com:/x").port(), 80);
    }

    #[test]
    fn the_scheme_and_host_are_lowercased() {
        let url = parsed("HTTP://EXAMPLE.COM/Path");
        assert_eq!(url.scheme(), Scheme::Http);
        assert_eq!(url.host(), "example.com");
        // The path keeps its case: only the scheme and host are
        // case-insensitive.
        assert_eq!(url.path(), "/Path");
    }

    #[test]
    fn a_trailing_root_dot_is_dropped() {
        assert_eq!(parsed("http://example.com./x").host(), "example.com");
    }

    #[test]
    fn https_parses_but_cannot_be_fetched() {
        let url = parsed("https://example.com/secure");
        assert_eq!(url.scheme(), Scheme::Https);
        assert!(!url.scheme().is_cleartext());
        assert!(parsed("http://example.com/").scheme().is_cleartext());
    }

    #[test]
    fn other_schemes_are_refused() {
        for text in [
            "ftp://example.com/f",
            "mailto:someone@example.com",
            "javascript:alert(1)",
            "data:text/html,hi",
            "gopher://example.com/",
        ] {
            assert_eq!(Url::parse(text), Err(Error::UnsupportedScheme), "{text}");
        }
    }

    #[test]
    fn the_three_spellings_of_a_file_url_are_one_url() {
        // Every form RFC 8089 and everyday typing produce, ending as the
        // same value -- which is what stops one file having three
        // addresses, and what makes the toolbar's text comparable.
        let canonical = parsed("file:///tmp/notes.txt");
        assert_eq!(canonical.scheme(), Scheme::File);
        assert_eq!(canonical.host(), "");
        assert_eq!(canonical.path(), "/tmp/notes.txt");
        assert_eq!(canonical.to_text().unwrap(), "file:///tmp/notes.txt");
        for text in ["file://localhost/tmp/notes.txt", "file:/tmp/notes.txt"] {
            assert_eq!(parsed(text), canonical, "{text}");
        }
        assert_eq!(parsed("file://LOCALHOST/tmp/notes.txt"), canonical);
    }

    #[test]
    fn a_file_url_naming_another_machine_is_refused() {
        // Not read as the local file, which would be answering a question
        // nobody asked.
        assert_eq!(
            Url::parse("file://other-machine/tmp/x"),
            Err(Error::RemoteFileHost)
        );
        assert_eq!(
            Url::parse("file://192.168.0.2/tmp/x"),
            Err(Error::RemoteFileHost)
        );
    }

    #[test]
    fn a_file_url_needs_an_absolute_path() {
        assert_eq!(Url::parse("file:tmp/x"), Err(Error::MissingHost));
        assert_eq!(Url::parse("file:"), Err(Error::MissingHost));
    }

    #[test]
    fn a_file_url_has_no_port_in_its_text() {
        let url = parsed("file:///tmp/x");
        assert_eq!(url.port(), 0);
        // Zero is this scheme's default, so `has_default_port` keeps it
        // out of the address the reader sees.
        assert_eq!(url.to_text().unwrap(), "file:///tmp/x");
    }

    #[test]
    fn relative_links_resolve_inside_a_file_url() {
        let base = parsed("file:///vol/usb0p1/docs/index.html");
        assert_eq!(
            base.resolve("notes.txt").unwrap().to_text().unwrap(),
            "file:///vol/usb0p1/docs/notes.txt"
        );
        assert_eq!(
            base.resolve("../readme.txt").unwrap().to_text().unwrap(),
            "file:///vol/usb0p1/readme.txt"
        );
        assert_eq!(
            base.resolve("/tmp/x").unwrap().to_text().unwrap(),
            "file:///tmp/x"
        );
        // `..` cannot climb out of the root, exactly as for http.
        assert_eq!(
            base.resolve("/../../etc").unwrap().to_text().unwrap(),
            "file:///etc"
        );
        // A scheme-relative reference has no authority to land in.
        assert_eq!(base.resolve("//host/x"), Err(Error::MissingHost));
        // An absolute link off the local machine still works.
        assert_eq!(
            base.resolve("http://example.com/x").unwrap().scheme(),
            Scheme::Http
        );
    }

    #[test]
    fn a_directory_url_ends_in_a_slash_so_its_links_land_inside_it() {
        // The bug this exists for: without the trailing slash the last
        // segment is a *sibling*, so every name on a listing page resolves
        // one level too high.
        let bare = parsed("file:///tmp");
        assert_eq!(
            bare.resolve("notes.txt").unwrap().to_text().unwrap(),
            "file:///notes.txt"
        );
        let directory = bare.as_directory().unwrap();
        assert_eq!(directory.to_text().unwrap(), "file:///tmp/");
        assert_eq!(
            directory.resolve("notes.txt").unwrap().to_text().unwrap(),
            "file:///tmp/notes.txt"
        );
        // `..` from a directory page goes exactly one level up, which is
        // what the `..` link on a listing needs.
        assert_eq!(
            parsed("file:///vol/usb0p1/docs/")
                .resolve("..")
                .unwrap()
                .to_text()
                .unwrap(),
            "file:///vol/usb0p1/"
        );
        // Already a directory: unchanged, and not given a second slash.
        let already = parsed("file:///tmp/");
        assert_eq!(already.as_directory().unwrap(), already);
        // The root is already one.
        assert_eq!(parsed("file:///").as_directory().unwrap().path(), "/");
        // Works for http too, where the same rule has always applied.
        assert_eq!(
            parsed("http://example.com/a/b")
                .as_directory()
                .unwrap()
                .to_text()
                .unwrap(),
            "http://example.com/a/b/"
        );
    }

    #[test]
    fn a_directory_url_drops_the_query_and_the_fragment() {
        let url = parsed("file:///tmp?x=1#part").as_directory().unwrap();
        assert_eq!(url.to_text().unwrap(), "file:///tmp/");
    }

    #[test]
    fn a_typed_file_address_is_not_completed_to_http() {
        assert_eq!(
            Url::parse_typed("file:///tmp/x").unwrap().scheme(),
            Scheme::File
        );
        assert_eq!(
            Url::parse_typed("file:/tmp/x").unwrap().scheme(),
            Scheme::File
        );
    }

    #[test]
    fn a_scheme_without_slashes_is_refused_rather_than_guessed() {
        assert_eq!(Url::parse("http:example.com/x"), Err(Error::MissingHost));
    }

    #[test]
    fn a_relative_reference_is_not_an_absolute_url() {
        assert_eq!(Url::parse("/just/a/path"), Err(Error::UnsupportedScheme));
        assert_eq!(Url::parse("page.html"), Err(Error::UnsupportedScheme));
    }

    #[test]
    fn userinfo_is_refused() {
        assert_eq!(
            Url::parse("http://user:secret@example.com/"),
            Err(Error::HasUserinfo)
        );
        assert_eq!(
            Url::parse("http://user@example.com/"),
            Err(Error::HasUserinfo)
        );
    }

    #[test]
    fn ipv6_literals_are_refused() {
        assert_eq!(Url::parse("http://[::1]/"), Err(Error::Ipv6Literal));
        assert_eq!(
            Url::parse("http://[2001:db8::1]:8080/"),
            Err(Error::Ipv6Literal)
        );
    }

    #[test]
    fn ports_must_be_numbers_below_65536() {
        assert_eq!(parsed("http://example.com:65535/").port(), 65535);
        assert_eq!(
            Url::parse("http://example.com:65536/"),
            Err(Error::InvalidPort)
        );
        assert_eq!(
            Url::parse("http://example.com:80x/"),
            Err(Error::InvalidPort)
        );
        assert_eq!(
            Url::parse("http://example.com:-1/"),
            Err(Error::InvalidPort)
        );
    }

    #[test]
    fn missing_hosts_are_refused() {
        assert_eq!(Url::parse("http://"), Err(Error::MissingHost));
        assert_eq!(Url::parse("http:///path"), Err(Error::MissingHost));
        assert_eq!(Url::parse("http://:8080/"), Err(Error::MissingHost));
    }

    // --- hosts ------------------------------------------------------------

    #[test]
    fn dotted_quads_are_recognised() {
        let url = parsed("http://192.168.1.42:8080/x");
        assert_eq!(url.host(), "192.168.1.42");
        assert_eq!(url.ipv4(), Some([192, 168, 1, 42]));
        assert_eq!(url.port(), 8080);
    }

    #[test]
    fn a_name_is_not_an_address() {
        assert_eq!(parsed("http://example.com/").ipv4(), None);
    }

    #[test]
    fn numeric_hosts_that_are_not_addresses_are_refused() {
        // Refused rather than resolved as names: each of these means
        // something to `inet_aton` that it does not mean here, and a host
        // read two ways is a host the address bar cannot be trusted about.
        for host in [
            "999.1.1.1",
            "1.2.3",
            "1.2.3.4.5",
            "0177.0.0.1",
            "010.1.1.1",
            "1.2.3.",
            "..1.2",
        ] {
            let text = format!("http://{host}/");
            assert_eq!(Url::parse(&text), Err(Error::InvalidHost), "{host}");
        }
    }

    #[test]
    fn zero_and_broadcast_addresses_still_parse() {
        assert_eq!(parsed("http://0.0.0.0/").ipv4(), Some([0, 0, 0, 0]));
        assert_eq!(
            parsed("http://255.255.255.255/").ipv4(),
            Some([255, 255, 255, 255])
        );
    }

    #[test]
    fn names_take_letters_digits_hyphens_and_underscores() {
        assert_eq!(parsed("http://my-nas_01.local/").host(), "my-nas_01.local");
    }

    #[test]
    fn malformed_names_are_refused() {
        for host in [
            "exa mple.com",
            "example..com",
            ".example.com",
            "ex%41mple.com",
        ] {
            let text = format!("http://{host}/");
            assert!(Url::parse(&text).is_err(), "{host}");
        }
    }

    #[test]
    fn a_unicode_host_is_refused_because_there_is_no_idna() {
        assert_eq!(Url::parse("http://日本.example/"), Err(Error::InvalidHost));
        // Its punycode spelling is already ASCII and goes through.
        assert_eq!(
            parsed("http://xn--wgv71a.example/").host(),
            "xn--wgv71a.example"
        );
    }

    #[test]
    fn an_overlong_label_is_refused() {
        let label = "a".repeat(64);
        assert_eq!(
            Url::parse(&format!("http://{label}.com/")),
            Err(Error::InvalidHost)
        );
        let label = "a".repeat(63);
        assert!(Url::parse(&format!("http://{label}.com/")).is_ok());
    }

    // --- forbidden characters ---------------------------------------------

    #[test]
    fn control_characters_and_spaces_cannot_reach_a_request() {
        for text in [
            "http://example.com/a b",
            "http://example.com/a\rb",
            "http://example.com/a\nb",
            "http://exam\rple.com/",
            "http://example.com/a\x01b",
            "http://example.com/a\x7fb",
            // The classic injection: a CRLF and a second request line.
            "http://example.com/x\r\nGET /y HTTP/1.0\r\n",
        ] {
            assert_eq!(Url::parse(text), Err(Error::ForbiddenCharacter), "{text:?}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_ignored_but_interior_is_not() {
        // Markup wraps `href` values across lines; the value is the same
        // one either way.
        assert_eq!(
            text_of(&parsed("\n  http://example.com/x  \t")),
            "http://example.com/x"
        );
        assert_eq!(
            base().resolve("\n   page.html  ").unwrap().path(),
            "/a/b/page.html"
        );
    }

    #[test]
    fn a_request_target_never_carries_the_fragment() {
        let url = parsed("http://example.com/p?q=1#secret");
        assert_eq!(url.request_target().unwrap(), "/p?q=1");
        assert!(!url.request_target().unwrap().contains("secret"));
    }

    #[test]
    fn the_host_header_carries_a_non_default_port() {
        assert_eq!(
            parsed("http://example.com/").host_header().unwrap(),
            "example.com"
        );
        assert_eq!(
            parsed("http://example.com:8080/").host_header().unwrap(),
            "example.com:8080"
        );
        assert_eq!(
            parsed("https://example.com:443/").host_header().unwrap(),
            "example.com"
        );
    }

    // --- length -----------------------------------------------------------

    #[test]
    fn the_url_length_bound_is_exact() {
        let prefix = "http://example.com/";
        let filler = "x".repeat(MAX_URL_BYTES - prefix.len());
        let at_limit = format!("{prefix}{filler}");
        assert_eq!(at_limit.len(), MAX_URL_BYTES);
        assert!(Url::parse(&at_limit).is_ok());

        let over = format!("{at_limit}x");
        assert_eq!(over.len(), MAX_URL_BYTES + 1);
        assert_eq!(Url::parse(&over), Err(Error::TooLong));
    }

    #[test]
    fn percent_encoding_counts_toward_the_bound() {
        // Each of these is one byte written and three stored, so a URL
        // that fits as typed can still be too long once encoded.
        let filler = "\u{3042}".repeat(MAX_URL_BYTES / 4);
        let text = format!("http://example.com/{filler}");
        assert!(text.len() <= MAX_URL_BYTES);
        assert_eq!(Url::parse(&text), Err(Error::TooLong));
    }

    #[test]
    fn an_overlong_relative_reference_is_refused() {
        let reference = "x".repeat(MAX_URL_BYTES + 1);
        assert_eq!(base().resolve(&reference), Err(Error::TooLong));
    }

    #[test]
    fn empty_input_is_its_own_error() {
        assert_eq!(Url::parse(""), Err(Error::Empty));
        assert_eq!(Url::parse("   \r\n "), Err(Error::Empty));
    }

    // --- percent-encoding --------------------------------------------------

    #[test]
    fn non_ascii_is_percent_encoded_in_the_path_and_query() {
        let url = parsed("http://example.com/日本?q=語#部");
        assert_eq!(url.path(), "/%E6%97%A5%E6%9C%AC");
        assert_eq!(url.query(), Some("q=%E8%AA%9E"));
        assert_eq!(url.fragment(), Some("%E9%83%A8"));
        assert!(url.request_target().unwrap().is_ascii());
    }

    #[test]
    fn already_encoded_input_is_not_encoded_again() {
        let url = parsed("http://example.com/a%20b?x=%25");
        assert_eq!(url.path(), "/a%20b");
        assert_eq!(url.query(), Some("x=%25"));
    }

    /// The one that matters: `%2e` is not a dot.
    ///
    /// Collapsing it would let `/a/%2e%2e/secret` climb out of `/a/`, which
    /// is a directory traversal against whatever the server thought it was
    /// confining the request to.
    #[test]
    fn percent_encoded_dots_are_not_dot_segments() {
        assert_eq!(
            parsed("http://example.com/a/%2e%2e/b").path(),
            "/a/%2e%2e/b"
        );
        assert_eq!(parsed("http://example.com/a/%2E./b").path(), "/a/%2E./b");
        assert_eq!(resolved("%2e%2e/x"), "http://example.com/a/b/%2e%2e/x");
        // And the unencoded form still is one.
        assert_eq!(parsed("http://example.com/a/../b").path(), "/b");
    }

    // --- dot segments ------------------------------------------------------

    #[test]
    fn dot_segments_are_removed_on_absolute_paths() {
        let cases = [
            ("/a/./b", "/a/b"),
            ("/a/../b", "/b"),
            ("/a/b/../../c", "/c"),
            ("/a/b/c/./../../g", "/a/g"),
            ("/./a", "/a"),
            ("/a/.", "/a/"),
            ("/a/..", "/"),
            ("/a/b/", "/a/b/"),
            ("/..", "/"),
            ("/../../x", "/x"),
            ("/a//b", "/a//b"),
        ];
        for (input, expected) in cases {
            let url = parsed(&format!("http://example.com{input}"));
            assert_eq!(url.path(), expected, "{input}");
        }
    }

    // --- relative resolution (RFC 3986 5.4) --------------------------------

    #[test]
    fn same_directory_references() {
        assert_eq!(resolved("g.html"), "http://example.com/a/b/g.html");
        assert_eq!(resolved("./g.html"), "http://example.com/a/b/g.html");
    }

    #[test]
    fn downward_and_upward_references() {
        assert_eq!(resolved("d/g.html"), "http://example.com/a/b/d/g.html");
        assert_eq!(resolved("d/../g.html"), "http://example.com/a/b/g.html");
        assert_eq!(resolved("../g.html"), "http://example.com/a/g.html");
        assert_eq!(resolved("../../g.html"), "http://example.com/g.html");
        // Past the root, which is a no-op rather than an error.
        assert_eq!(resolved("../../../../g.html"), "http://example.com/g.html");
    }

    #[test]
    fn root_relative_references_replace_the_whole_path() {
        assert_eq!(resolved("/g.html"), "http://example.com/g.html");
        assert_eq!(resolved("/"), "http://example.com/");
    }

    #[test]
    fn scheme_relative_references_change_host_but_keep_the_scheme() {
        assert_eq!(resolved("//other.example/g"), "http://other.example/g");
        let secure = parsed("https://example.com/a/");
        assert_eq!(
            text_of(&secure.resolve("//other.example/g").unwrap()),
            "https://other.example/g"
        );
    }

    #[test]
    fn query_only_references_keep_the_path() {
        assert_eq!(resolved("?y=2"), "http://example.com/a/b/c.html?y=2");
        // And an empty query is a query, not an absent one.
        assert_eq!(base().resolve("?").unwrap().query(), Some(""));
    }

    #[test]
    fn fragment_only_references_stay_on_the_same_document() {
        let target = base().resolve("#other").unwrap();
        assert_eq!(text_of(&target), "http://example.com/a/b/c.html?x=1#other");
        assert!(target.same_document(&base()));
    }

    #[test]
    fn an_empty_reference_is_the_same_document_without_its_fragment() {
        let target = base().resolve("").unwrap();
        assert_eq!(text_of(&target), "http://example.com/a/b/c.html?x=1");
        assert!(target.same_document(&base()));
    }

    #[test]
    fn an_absolute_reference_ignores_the_base_entirely() {
        assert_eq!(
            resolved("http://other.example:8080/g"),
            "http://other.example:8080/g"
        );
        assert_eq!(
            resolved("https://other.example/g"),
            "https://other.example/g"
        );
    }

    #[test]
    fn a_relative_reference_drops_the_bases_query_and_fragment() {
        assert_eq!(resolved("g.html"), "http://example.com/a/b/g.html");
        assert!(!resolved("g.html").contains("x=1"));
        assert!(!resolved("g.html").contains("frag"));
    }

    #[test]
    fn a_reference_carries_its_own_query_and_fragment() {
        assert_eq!(
            resolved("g.html?y=2#z"),
            "http://example.com/a/b/g.html?y=2#z"
        );
    }

    #[test]
    fn resolution_against_a_directory_base_keeps_the_directory() {
        let directory = parsed("http://example.com/links/");
        assert_eq!(
            text_of(&directory.resolve("target.html").unwrap()),
            "http://example.com/links/target.html"
        );
        assert_eq!(
            text_of(&directory.resolve("deep/../target.html").unwrap()),
            "http://example.com/links/target.html"
        );
        assert_eq!(
            text_of(&directory.resolve("../simple.html").unwrap()),
            "http://example.com/simple.html"
        );
    }

    #[test]
    fn a_relative_reference_from_the_root_stays_at_the_root() {
        let root = parsed("http://example.com/");
        assert_eq!(
            text_of(&root.resolve("simple.html").unwrap()),
            "http://example.com/simple.html"
        );
    }

    #[test]
    fn unsupported_schemes_in_a_link_are_refused_at_resolution() {
        assert_eq!(
            base().resolve("ftp://example.com/f"),
            Err(Error::UnsupportedScheme)
        );
        assert_eq!(
            base().resolve("javascript:void(0)"),
            Err(Error::UnsupportedScheme)
        );
    }

    #[test]
    fn resolution_refuses_forbidden_characters_too() {
        assert_eq!(base().resolve("g\r\nX: y"), Err(Error::ForbiddenCharacter));
    }

    // --- classification ----------------------------------------------------

    #[test]
    fn references_are_classified_before_resolution() {
        let cases = [
            ("http://example.com/", Reference::Absolute),
            ("HTTPS://example.com/", Reference::Absolute),
            ("ftp://example.com/", Reference::Absolute),
            ("//example.com/", Reference::SchemeRelative),
            ("/path", Reference::Root),
            ("path", Reference::Relative),
            ("./path", Reference::Relative),
            ("../path", Reference::Relative),
            ("?q=1", Reference::Query),
            ("#part", Reference::Fragment),
            ("", Reference::Same),
            ("   ", Reference::Same),
            // Not a scheme: no colon before the first slash.
            ("page.html:8080", Reference::Absolute),
            ("a/b:c", Reference::Relative),
        ];
        for (reference, expected) in cases {
            assert_eq!(classify(reference), expected, "{reference:?}");
        }
    }

    // --- typed addresses ----------------------------------------------------

    #[test]
    fn a_typed_address_without_a_scheme_gets_http() {
        let cases = [
            ("example.com", "http://example.com/"),
            ("example.com/page.html", "http://example.com/page.html"),
            ("192.168.0.159:8080", "http://192.168.0.159:8080/"),
            (
                "192.168.0.159:8080/simple.html",
                "http://192.168.0.159:8080/simple.html",
            ),
            // The case `classify` calls Absolute and nobody means that way.
            ("localhost:8080", "http://localhost:8080/"),
            ("example.com:8080/x", "http://example.com:8080/x"),
            // Scheme-relative: only the scheme is missing, so only the
            // scheme is added.
            ("//example.com/x", "http://example.com/x"),
            ("built-in/", "http://built-in/"),
        ];
        for (typed, expected) in cases {
            let url = Url::parse_typed(typed).unwrap_or_else(|e| panic!("{typed}: {e:?}"));
            assert_eq!(text_of(&url), expected, "{typed:?}");
        }
    }

    #[test]
    fn a_typed_address_that_has_a_scheme_keeps_it() {
        assert_eq!(
            text_of(&Url::parse_typed("http://example.com/x").unwrap()),
            "http://example.com/x"
        );
        // Recognised, refused later by the fetch path -- never completed
        // into an http address, which would be the downgrade this browser
        // does not do.
        assert_eq!(
            Url::parse_typed("https://example.com/x").unwrap().scheme(),
            Scheme::Https
        );
        // Another scheme stays its own error rather than becoming
        // `http://ftp://...`.
        assert_eq!(
            Url::parse_typed("ftp://example.com/x"),
            Err(Error::UnsupportedScheme)
        );
        // `http:` without the slashes is a typo, and it keeps saying so.
        assert_eq!(
            Url::parse_typed("http:example.com"),
            Err(Error::MissingHost)
        );
    }

    #[test]
    fn typing_nothing_is_still_empty() {
        assert_eq!(Url::parse_typed(""), Err(Error::Empty));
        assert_eq!(Url::parse_typed("   "), Err(Error::Empty));
    }

    #[test]
    fn completion_does_not_rescue_a_bad_address() {
        // The host is checked after the scheme is added, the same way it
        // would have been if the scheme had been typed.
        assert_eq!(
            Url::parse_typed("0177.0.0.1/x"),
            Err(Error::InvalidHost),
            "a non-dotted-quad numeric host stays refused"
        );
        assert_eq!(
            Url::parse_typed("user@example.com/x"),
            Err(Error::HasUserinfo)
        );
    }

    #[test]
    fn what_counts_as_a_scheme() {
        assert!(has_scheme("http://example.com"));
        assert!(has_scheme("HTTPS://example.com"));
        assert!(has_scheme("http:example.com"));
        assert!(has_scheme("ftp://example.com"));
        // A colon that is a port, not a scheme.
        assert!(!has_scheme("localhost:8080"));
        assert!(!has_scheme("example.com:8080/x"));
        assert!(!has_scheme("example.com"));
        assert!(!has_scheme("/path"));
        assert!(!has_scheme("//example.com/x"));
        assert!(!has_scheme(""));
    }

    #[test]
    fn same_document_ignores_only_the_fragment() {
        let a = parsed("http://example.com/p?q=1#one");
        let b = parsed("http://example.com/p?q=1#two");
        let c = parsed("http://example.com/p?q=2#one");
        let d = parsed("http://example.com:8080/p?q=1#one");
        assert!(a.same_document(&b));
        assert!(!a.same_document(&c));
        assert!(!a.same_document(&d));
    }

    // --- the display/network agreement -------------------------------------

    /// The invariant this module exists for: whatever the address bar
    /// shows is the host that gets connected to.
    #[test]
    fn the_shown_url_and_the_connected_host_come_from_one_value() {
        let url = parsed("http://real.example:8080/a/../b?q=1#f");
        let text = text_of(&url);
        assert_eq!(text, "http://real.example:8080/b?q=1#f");
        assert!(text.contains(url.host()));
        assert_eq!(url.request_target().unwrap(), "/b?q=1");
        assert_eq!(url.host_header().unwrap(), "real.example:8080");
        // And re-parsing what is shown gives the same value back, so the
        // address bar is a faithful serialisation rather than a summary.
        assert_eq!(Url::parse(&text).unwrap(), url);
    }

    #[test]
    fn round_tripping_is_stable_for_every_fixture_shape() {
        for text in [
            "http://example.com/",
            "http://example.com/a/b/c.html",
            "http://example.com:8080/a?b=c#d",
            "https://example.com/secure",
            "http://192.168.1.42:8080/manifest.txt",
            "http://example.com/a%20b",
        ] {
            let once = parsed(text);
            let twice = Url::parse(&text_of(&once)).unwrap();
            assert_eq!(once, twice, "{text}");
            assert_eq!(text_of(&once), text_of(&twice), "{text}");
        }
    }

    #[test]
    fn manifest_lines_are_split_and_comments_dropped() {
        let text = "# a comment\n/simple.html\tok\n\n  /links/\tok  \n";
        assert_eq!(split_manifest(text), ["/simple.html\tok", "/links/\tok"]);
    }

    #[test]
    fn fragment_decode_is_strict_and_not_form_encoding() {
        assert_eq!(decode_fragment("a%20b").unwrap().as_deref(), Some("a b"));
        assert_eq!(decode_fragment("%E6%97%A5").unwrap().as_deref(), Some("日"));
        assert_eq!(decode_fragment("a+b").unwrap().as_deref(), Some("a+b"));
        assert_eq!(decode_fragment("%").unwrap(), None);
        assert_eq!(decode_fragment("%GG").unwrap(), None);
        assert_eq!(decode_fragment("%ff").unwrap(), None);
    }
}
