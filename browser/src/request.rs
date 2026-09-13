//! Bounded, pure HTTP request values and wire encoding.
//!
//! The network transaction owns sockets; this module owns the statement of
//! what is to be sent. Keeping method, URL and body together prevents a
//! redirect or retry from accidentally retaining only part of a POST.

use alloc::string::String;
use alloc::vec::Vec;

use crate::limits::MAX_ENCODED_REQUEST_BYTES;
use crate::memory;
use crate::url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    fn token(self) -> &'static [u8] {
        match self {
            Self::Get => b"GET",
            Self::Post => b"POST",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub url: Url,
    pub method: Method,
    body: String,
    /// A validator to send as `If-None-Match`. GET only, and only for the
    /// URL it was stored under: a redirect drops it.
    if_none_match: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    TooLong,
    InvalidHeadValue,
    OutOfMemory,
}

impl Request {
    pub fn get(url: Url) -> Self {
        Self {
            url,
            method: Method::Get,
            body: String::new(),
            if_none_match: None,
        }
    }

    pub fn post_urlencoded(url: Url, body: String) -> Result<Self, Error> {
        if body.len() > MAX_ENCODED_REQUEST_BYTES {
            return Err(Error::TooLong);
        }
        Ok(Self {
            url,
            method: Method::Post,
            body,
            if_none_match: None,
        })
    }

    pub fn body(&self) -> &[u8] {
        self.body.as_bytes()
    }

    pub fn content_type(&self) -> Option<&'static str> {
        (self.method == Method::Post).then_some("application/x-www-form-urlencoded")
    }

    /// Applies the method rewrite required by an HTTP redirect.
    pub fn redirect_to(&mut self, status: u16, target: Url) {
        if self.method == Method::Post && matches!(status, 301 | 302 | 303) {
            self.method = Method::Get;
            self.body.clear();
        }
        self.if_none_match = None;
        self.url = target;
    }

    /// Makes a GET conditional on the cached copy still being current.
    /// Ignored for any other method.
    pub fn set_if_none_match(&mut self, etag: String) {
        if self.method == Method::Get {
            self.if_none_match = Some(etag);
        }
    }

    pub fn if_none_match(&self) -> Option<&str> {
        self.if_none_match.as_deref()
    }

    pub fn has_body(&self) -> bool {
        !self.body.is_empty()
    }

    /// Copies the whole statement for an explicit resend.
    ///
    /// Fallible rather than `Clone`: the body can be tens of kilobytes, and
    /// a copy that cannot be made must degrade the resend offer instead of
    /// aborting the board.
    pub fn try_clone(&self) -> Result<Self, Error> {
        Ok(Self {
            url: self.url.clone(),
            method: self.method,
            body: memory::string_from(&self.body).map_err(|_| Error::OutOfMemory)?,
            if_none_match: match &self.if_none_match {
                Some(etag) => Some(memory::string_from(etag).map_err(|_| Error::OutOfMemory)?),
                None => None,
            },
        })
    }
}

/// Encodes one request as HTTP/1.0 bytes.
///
/// Header names and values other than `Host` and `User-Agent` are fixed in
/// this function. The two supplied values and the request target are still
/// checked here even though ordinary callers derive them from a validated
/// [`Url`], so a future caller cannot introduce another header with CRLF.
pub fn encode_http10(
    request: &Request,
    host: &[u8],
    target: &[u8],
    user_agent: &[u8],
) -> Result<Vec<u8>, Error> {
    encode_http10_parts(
        request.method,
        request.body.as_bytes(),
        host,
        target,
        user_agent,
        request.if_none_match.as_deref().map(str::as_bytes),
    )
}

/// Lower-level form used by the transport's backwards-compatible GET API.
pub fn encode_http10_parts(
    method: Method,
    body: &[u8],
    host: &[u8],
    target: &[u8],
    user_agent: &[u8],
    if_none_match: Option<&[u8]>,
) -> Result<Vec<u8>, Error> {
    if body.len() > MAX_ENCODED_REQUEST_BYTES || (method == Method::Get && !body.is_empty()) {
        return Err(Error::TooLong);
    }
    if let Some(etag) = if_none_match
        && (method != Method::Get || etag.is_empty() || !head_value(etag))
    {
        return Err(Error::InvalidHeadValue);
    }
    let (conditional_head, etag) = match if_none_match {
        Some(etag) => (b"\r\nIf-None-Match: ".as_slice(), etag),
        None => (b"".as_slice(), b"".as_slice()),
    };
    if host.is_empty()
        || !target.starts_with(b"/")
        || !head_value(host)
        || !request_target(target)
        || !head_value(user_agent)
    {
        return Err(Error::InvalidHeadValue);
    }

    let content_length = if method == Method::Post {
        decimal(body.len())?
    } else {
        String::new()
    };
    let post_head = if method == Method::Post {
        b"\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: ".as_slice()
    } else {
        b"".as_slice()
    };
    let length = method.token().len()
        + 1
        + target.len()
        + b" HTTP/1.0\r\nHost: ".len()
        + host.len()
        + b"\r\nUser-Agent: ".len()
        + user_agent.len()
        + b"\r\nAccept-Encoding: identity".len()
        + conditional_head.len()
        + etag.len()
        + post_head.len()
        + content_length.len()
        + b"\r\nConnection: close\r\n\r\n".len()
        + body.len();
    let mut wire = Vec::new();
    wire.try_reserve_exact(length)
        .map_err(|_| Error::OutOfMemory)?;
    wire.extend_from_slice(method.token());
    wire.push(b' ');
    wire.extend_from_slice(target);
    wire.extend_from_slice(b" HTTP/1.0\r\nHost: ");
    wire.extend_from_slice(host);
    wire.extend_from_slice(b"\r\nUser-Agent: ");
    wire.extend_from_slice(user_agent);
    wire.extend_from_slice(b"\r\nAccept-Encoding: identity");
    wire.extend_from_slice(conditional_head);
    wire.extend_from_slice(etag);
    wire.extend_from_slice(post_head);
    wire.extend_from_slice(content_length.as_bytes());
    wire.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    wire.extend_from_slice(body);
    debug_assert_eq!(wire.len(), length);
    Ok(wire)
}

fn head_value(value: &[u8]) -> bool {
    value
        .iter()
        .all(|byte| matches!(byte, b' '..=b'~') && *byte != b'\r' && *byte != b'\n')
}

fn request_target(value: &[u8]) -> bool {
    value.iter().all(|byte| matches!(byte, b'!'..=b'~'))
}

fn decimal(mut value: usize) -> Result<String, Error> {
    let mut reversed = [0u8; 20];
    let mut used = 0;
    loop {
        reversed[used] = b'0' + (value % 10) as u8;
        used += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let mut result = memory::string_with_capacity(used).map_err(|_| Error::OutOfMemory)?;
    for byte in reversed[..used].iter().rev() {
        memory::push_str(&mut result, core::str::from_utf8(&[*byte]).unwrap())
            .map_err(|_| Error::OutOfMemory)?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_keeps_the_existing_wire_shape() {
        let request = Request::get(Url::parse("http://example.com/a?q=1").unwrap());
        assert_eq!(
            encode_http10(&request, b"example.com", b"/a?q=1", b"tab5-browser/test").unwrap(),
            b"GET /a?q=1 HTTP/1.0\r\nHost: example.com\r\nUser-Agent: tab5-browser/test\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn post_has_an_exact_length_and_body() {
        let request = Request::post_urlencoded(
            Url::parse("https://example.com/submit").unwrap(),
            memory::string_from("q=two+words&empty=").unwrap(),
        )
        .unwrap();
        assert_eq!(
            request.content_type(),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(
            encode_http10(&request, b"example.com", b"/submit", b"tab5-browser/test").unwrap(),
            b"POST /submit HTTP/1.0\r\nHost: example.com\r\nUser-Agent: tab5-browser/test\r\nAccept-Encoding: identity\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 18\r\nConnection: close\r\n\r\nq=two+words&empty="
        );
    }

    #[test]
    fn a_conditional_get_carries_its_validator_and_a_redirect_drops_it() {
        let mut request = Request::get(Url::parse("http://example.com/a").unwrap());
        request.set_if_none_match("W/\"v1\"".into());
        assert_eq!(
            encode_http10(&request, b"example.com", b"/a", b"ua").unwrap(),
            b"GET /a HTTP/1.0\r\nHost: example.com\r\nUser-Agent: ua\r\nAccept-Encoding: identity\r\nIf-None-Match: W/\"v1\"\r\nConnection: close\r\n\r\n"
        );
        assert_eq!(request.try_clone().unwrap(), request);
        request.redirect_to(302, Url::parse("http://example.com/b").unwrap());
        assert_eq!(request.if_none_match(), None);
        assert_eq!(
            encode_http10_parts(Method::Get, b"", b"h", b"/", b"ua", Some(b"\"a\"\r\nX: y")),
            Err(Error::InvalidHeadValue)
        );
        assert_eq!(
            encode_http10_parts(Method::Post, b"q=1", b"h", b"/", b"ua", Some(b"\"a\"")),
            Err(Error::InvalidHeadValue)
        );
    }

    #[test]
    fn a_head_value_or_target_cannot_inject_a_line() {
        let request = Request::get(Url::parse("http://example.com/").unwrap());
        assert_eq!(
            encode_http10(&request, b"example.com\r\nX: y", b"/", b"ua"),
            Err(Error::InvalidHeadValue)
        );
        assert_eq!(
            encode_http10(&request, b"example.com", b"/x\r\nX: y", b"ua"),
            Err(Error::InvalidHeadValue)
        );
        assert_eq!(
            encode_http10(&request, b"example.com", b"/", b"ua\nX: y"),
            Err(Error::InvalidHeadValue)
        );
    }

    #[test]
    fn post_body_has_a_hard_bound() {
        let url = Url::parse("http://example.com/").unwrap();
        let at_limit = "x".repeat(MAX_ENCODED_REQUEST_BYTES);
        assert!(Request::post_urlencoded(url.clone(), at_limit).is_ok());
        let over = "x".repeat(MAX_ENCODED_REQUEST_BYTES + 1);
        assert_eq!(Request::post_urlencoded(url, over), Err(Error::TooLong));
    }

    #[test]
    fn a_copy_keeps_method_url_and_body_together() {
        let request = Request::post_urlencoded(
            Url::parse("http://example.com/submit?kept=1").unwrap(),
            memory::string_from("q=one&q=two").unwrap(),
        )
        .unwrap();
        let copy = request.try_clone().unwrap();
        assert_eq!(copy, request);
    }

    #[test]
    fn redirects_rewrite_or_preserve_post_as_required() {
        for status in [301, 302, 303] {
            let mut request = Request::post_urlencoded(
                Url::parse("http://example.com/old").unwrap(),
                memory::string_from("q=one").unwrap(),
            )
            .unwrap();
            request.redirect_to(status, Url::parse("http://example.com/new").unwrap());
            assert_eq!(request.method, Method::Get);
            assert_eq!(request.body(), b"");
        }
        for status in [307, 308] {
            let mut request = Request::post_urlencoded(
                Url::parse("http://example.com/old").unwrap(),
                memory::string_from("q=one").unwrap(),
            )
            .unwrap();
            request.redirect_to(status, Url::parse("http://example.com/new").unwrap());
            assert_eq!(request.method, Method::Post);
            assert_eq!(request.body(), b"q=one");
        }
    }
}
