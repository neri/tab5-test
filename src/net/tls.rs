//! TLS 1.3 over a smoltcp socket, driven one frame at a time.
//!
//! ## Why there is a future in here
//!
//! [`embedded_tls`] hands out a blocking API and an async one. The blocking
//! one runs a whole handshake before it returns, which is exactly what the
//! browser cannot afford: a frame loop that leaves for a round trip to a
//! server plus a signature verification is a screen that cannot be
//! cancelled, cannot service USB and cannot redraw. So this drives the async
//! API by hand -- one `poll` of one future per frame, with
//! [`core::task::Waker::noop`], because nothing here ever needs waking: the
//! caller polls again next frame regardless.
//!
//! The awkward part of driving a future by hand is that the future has to
//! own everything it touches, and what it touches includes the socket -- but
//! the socket lives in the [`Stack`]'s set, which is only borrowed for the
//! length of one `poll`. Rather than smuggle that borrow into the future,
//! the two are separated by byte queues:
//!
//! ```text
//! smoltcp socket --(cipher_rx/cipher_tx)-- TLS future --(plain_rx/plain_tx)-- caller
//! ```
//!
//! [`Shared`] holds those four queues and is owned jointly, by `Rc`, so
//! nothing is self-referential and nothing needs a raw pointer. One
//! [`Transaction::poll`] pumps the socket into `cipher_rx`, polls the future
//! once, and pumps `cipher_tx` back out. The future's `await` points are all
//! on those queues, so it returns `Pending` the moment one runs dry, and the
//! frame loop gets control back.
//!
//! ## What is verified, and what is not
//!
//! Every connection checks the server's `CertificateVerify` against the
//! public key in the leaf certificate it presented, checks Finished, and
//! checks the AEAD tag on every record. The library's `NoVerify` is never
//! used and there is no path through this file that skips those.
//!
//! What that proves is that the peer holds the private key for the
//! certificate it sent. It does *not* prove the certificate belongs to the
//! host in the URL: no chain is built, no root is consulted, no name is
//! matched and no validity date is read. An attacker able to answer in the
//! server's place can present a certificate of their own and pass all of the
//! above. That is [`Authentication::Unverified`], and it must never be shown
//! as secure -- see `docs/TLS_PLAN.md` for what it is and is not for.
//!
//! [`Authentication::Pinned`] is the stronger case: the SHA-256 of the
//! leaf's DER `SubjectPublicKeyInfo` matched one built into the firmware for
//! that host. A host with pins whose key does not match fails as
//! [`Error::Pin`] and never falls back to `Unverified`.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use embedded_tls::{
    Aes128GcmSha256, CertificateEntryRef, CertificateRef, CertificateVerifyRef, CryptoProvider,
    SignatureScheme, TlsCipherSuite, TlsConfig, TlsConnection, TlsContext, TlsError, TlsVerifier,
};
use sha2::Digest;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

use crate::entropy::{self, Csprng};
use crate::net::Stack;
use crate::wifi::Rpc;
use crate::{delay, tick, uart};

/// TCP socket buffers, matching the plain HTTP transaction's.
const SOCKET_BUFFER_BYTES: usize = 8192;

/// The record buffers `embedded-tls` decodes into and encodes out of.
///
/// TLS 1.3's largest plaintext record is 16 KiB and the ciphertext around it
/// is a little larger; the library warns below 16640, which is that bound.
/// Both are heap allocations rather than locals: 32 KiB of stack is a third
/// of what the linker guarantees, and the PSRAM heap has it to spare.
const RECORD_BUFFER_BYTES: usize = 16640;

/// How much plaintext may sit in `plain_rx` before the future stops reading.
///
/// Back-pressure, not a body limit: the caller drains this every poll, and
/// the body's real bound is the HTTP layer's. What it prevents is a fast
/// server filling the heap while a slow caller is still parsing.
const PLAIN_RX_CAP: usize = 64 * 1024;

/// How much ciphertext may sit in `cipher_tx` before the future stops
/// writing. One record plus a margin: the socket drains it every poll.
const CIPHER_TX_CAP: usize = RECORD_BUFFER_BYTES * 2;

const CONNECT_TIMEOUT_MS: u64 = 5000;
/// How long the connection may stall before it is called dead.
const IDLE_TIMEOUT_MS: u64 = 10_000;
/// How long the abort is pumped for so the RST actually leaves.
const ABORT_PUMP_MS: u64 = 20;

/// Plaintext moved between the queues and the TLS engine per step.
const CHUNK_BYTES: usize = 2048;

/// What a finished handshake proved about who is on the other end.
///
/// Deliberately not a boolean. "Encrypted" and "authenticated" are different
/// claims, and every display of this has to be able to tell them apart.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Authentication {
    /// The peer holds the key in the certificate it sent. Nothing says the
    /// certificate is the URL host's.
    Unverified,
    /// The leaf's SPKI matched a pin built into this firmware for the host
    /// that was asked for.
    Pinned,
}

impl Authentication {
    /// What a toolbar or a status line prints. Never "secure", never a
    /// padlock: `Unverified` has not earned either.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unverified => "TLS UNVERIFIED",
            Self::Pinned => "TLS PINNED",
        }
    }
}

/// Why a TLS connection did not deliver plaintext.
///
/// One variant per failure name in `docs/TLS_PLAN.md`, because the browser
/// fixtures match on those names: collapsing a certificate error into a
/// timeout, or a pin mismatch into a generic certificate error, is what
/// makes a failing fixture unable to say what broke.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Error {
    /// No true randomness, so nothing was sent at all.
    Entropy,
    /// The C6 link went away mid-connection.
    LinkLost,
    /// TCP never connected, or the connection died before the handshake
    /// could finish.
    Connect,
    /// The peer stopped responding.
    TimedOut,
    /// No shared TLS version or cipher suite.
    Version,
    /// The peer ended the connection with a TLS alert.
    Alert,
    /// The certificate did not parse, or its `CertificateVerify` signature
    /// was wrong, or the signature scheme is not one this can check.
    Certificate,
    /// The host has pins and none of them matched the key it presented.
    Pin,
    /// The caller required a pin for a host that has none registered.
    PinMissing,
    /// A record, certificate or buffer went past its bound.
    Limit,
    /// The caller stopped it.
    Cancelled,
    OutOfMemory,
    /// Something on this side gave up: an allocation, a socket call.
    Local,
}

impl Error {
    /// The one-word name fixtures match on.
    pub fn name(self) -> &'static str {
        match self {
            Self::Entropy => "entropy",
            Self::LinkLost => "link-lost",
            Self::Connect => "tls-connect",
            Self::TimedOut => "tls-timeout",
            Self::Version => "tls-version",
            Self::Alert => "tls-alert",
            Self::Certificate => "tls-cert",
            Self::Pin => "tls-pin",
            Self::PinMissing => "tls-pin-missing",
            Self::Limit => "tls-limit",
            Self::Cancelled => "cancelled",
            Self::OutOfMemory => "out-of-memory",
            Self::Local => "tls-local",
        }
    }

    /// A short sentence for the screen. Deliberately not the library's
    /// parser detail: "the certificate's signature is wrong" is what a
    /// reader can act on, and an ASN.1 offset is not.
    pub fn message(self) -> &'static str {
        match self {
            Self::Entropy => "no hardware randomness, so no connection was attempted",
            Self::LinkLost => "the C6 link was lost during the connection",
            Self::Connect => "the TLS connection could not be established",
            Self::TimedOut => "the server stopped responding",
            Self::Version => "the server offered no TLS 1.3 suite this supports",
            Self::Alert => "the server rejected the connection",
            Self::Certificate => "the server's certificate or signature is not valid",
            Self::Pin => "the server's key does not match this firmware's pin",
            Self::PinMissing => "no pin is registered for this host",
            Self::Limit => "the server sent more than this can hold",
            Self::Cancelled => "cancelled",
            Self::OutOfMemory => "out of memory during the connection",
            Self::Local => "the TLS connection failed locally",
        }
    }
}

/// The SHA-256 of a leaf certificate's DER `SubjectPublicKeyInfo`.
///
/// The whole SPKI rather than the bare key: the structure names the
/// algorithm as well, and a pin that did not cover the algorithm would match
/// a key reused under a different one.
pub type SpkiPin = [u8; 32];

/// What a host's pins say about a connection to it.
#[derive(Clone, Copy)]
pub struct PinPolicy {
    /// The pins registered for this host. Empty means none are.
    pub pins: &'static [SpkiPin],
    /// Whether a host with no pins may be opened unauthenticated.
    ///
    /// False is what a caller that will act on the content asks for; it
    /// turns "no pin registered" into [`Error::PinMissing`] rather than into
    /// a connection nobody checked.
    pub required: bool,
}

impl PinPolicy {
    /// The policy for a host nothing is pinned for, opened knowingly
    /// unauthenticated.
    pub const UNAUTHENTICATED: PinPolicy = PinPolicy {
        pins: &[],
        required: false,
    };
}

// --------------------------------------------------------------- the queues

/// Everything the future and the socket pump share.
///
/// Held by `Rc` so that both sides own it outright. The alternative -- the
/// future borrowing state that lives in [`Transaction`] -- is a
/// self-referential struct, and the whole point of this arrangement is not
/// to have one.
struct Shared {
    /// Ciphertext from the socket, waiting to be decrypted.
    cipher_rx: VecDeque<u8>,
    /// Ciphertext from the engine, waiting to go out on the socket.
    cipher_tx: VecDeque<u8>,
    /// Plaintext from the engine, waiting for the caller.
    plain_rx: VecDeque<u8>,
    /// Plaintext from the caller, waiting to be encrypted.
    plain_tx: VecDeque<u8>,
    /// The peer closed the TCP connection; no more ciphertext will arrive.
    peer_closed: bool,
    /// The engine has been told the transport is at its end.
    ///
    /// `embedded-tls` reports a zero-length read as `IoError`, the same
    /// error a broken transport gives, so this is what lets the two be told
    /// apart afterwards: the difference between "the server finished and
    /// hung up" and "the connection broke" is not one to guess at.
    transport_eof: bool,
    /// The stream ended without a `close_notify`.
    ///
    /// Not an error by itself -- plenty of servers, including Google's, just
    /// close the TCP connection when an HTTP/1.0 response is done -- but it
    /// does mean TLS cannot vouch that nothing was cut off the end. Whether
    /// the *body* was complete is the HTTP layer's judgement, from its own
    /// framing, and this is what it needs in order to make it.
    closed_without_notify: bool,
    /// `open` returned, so Finished has been verified as well as the
    /// certificate. Until this is set there is nothing to report about who
    /// the peer is, however far the certificate messages got.
    handshake_done: bool,
    /// The caller has written the whole request, so the write phase may end.
    request_complete: bool,
    /// The response ended cleanly.
    response_complete: bool,
    /// Ciphertext this poll may still decrypt. Refilled by
    /// [`Transaction::poll`] so that one frame cannot be spent draining a
    /// buffer that arrived all at once.
    read_budget: usize,
    /// The caller asked to stop, which the future sees as an I/O failure.
    cancelled: bool,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            cipher_rx: VecDeque::new(),
            cipher_tx: VecDeque::new(),
            plain_rx: VecDeque::new(),
            plain_tx: VecDeque::new(),
            peer_closed: false,
            transport_eof: false,
            closed_without_notify: false,
            handshake_done: false,
            request_complete: false,
            response_complete: false,
            read_budget: 0,
            cancelled: false,
        }
    }
}

/// The transport `embedded-tls` writes its records to.
///
/// Not the socket: the queues. Everything that would block here yields
/// instead, so a poll of the whole future ends the moment the engine wants
/// bytes that have not arrived.
struct Io(Rc<RefCell<Shared>>);

#[derive(Debug)]
struct IoError;

impl core::fmt::Display for IoError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("the TLS transport stopped")
    }
}

impl core::error::Error for IoError {}

impl embedded_io::Error for IoError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

impl embedded_io::ErrorType for Io {
    type Error = IoError;
}

/// Returns `Pending` once, then `Ready`.
///
/// No waker is registered, deliberately: this is not an executor that sleeps
/// until something happens, it is a frame loop that will poll again anyway.
/// Registering a waker nobody wakes would be the pretence of one.
struct YieldNow(bool);

impl YieldNow {
    fn new() -> YieldNow {
        YieldNow(false)
    }
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

impl embedded_io_async::Read for Io {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, IoError> {
        loop {
            {
                let mut shared = self.0.borrow_mut();
                if shared.cancelled {
                    return Err(IoError);
                }
                let count = buffer
                    .len()
                    .min(shared.cipher_rx.len())
                    .min(shared.read_budget);
                if count > 0 {
                    for slot in buffer.iter_mut().take(count) {
                        // The length above came from the queue, so there is
                        // a byte for every slot.
                        *slot = shared.cipher_rx.pop_front().unwrap_or(0);
                    }
                    shared.read_budget -= count;
                    return Ok(count);
                }
                // A closed peer is only the end once the queue is drained;
                // the last records usually arrive with the FIN.
                if shared.peer_closed && shared.cipher_rx.is_empty() {
                    shared.transport_eof = true;
                    return Ok(0);
                }
            }
            YieldNow::new().await;
        }
    }
}

impl embedded_io_async::Write for Io {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, IoError> {
        loop {
            {
                let mut shared = self.0.borrow_mut();
                if shared.cancelled {
                    return Err(IoError);
                }
                if shared.cipher_tx.len() < CIPHER_TX_CAP {
                    shared.cipher_tx.extend(buffer.iter().copied());
                    return Ok(buffer.len());
                }
            }
            YieldNow::new().await;
        }
    }

    async fn flush(&mut self) -> Result<(), IoError> {
        // The socket pump is what actually flushes; there is nothing held
        // back here for this to release.
        Ok(())
    }
}

// ------------------------------------------------------------- the verifier

/// What the verifier concluded, published where the transaction can read it.
///
/// `open` takes its context by value and drops it, so the verifier cannot be
/// asked afterwards -- and the distinction between "no pin registered" and
/// "the pin did not match" has to survive, because they are different
/// answers to the user.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Outcome {
    Pending,
    Authenticated(Authentication),
    PinMismatch,
    PinMissing,
    BadCertificate,
}

/// Checks `CertificateVerify` against the leaf's key, and the leaf's key
/// against the firmware's pins.
///
/// This is the whole of what an unauthenticated connection proves, so the
/// two things it does not do are worth naming: it does not walk the chain
/// past the leaf, and [`TlsVerifier::set_hostname_verification`] is a
/// deliberate no-op. Matching the certificate's names without a chain to a
/// trusted root would prove nothing at all -- anyone can put any name in a
/// certificate they signed themselves -- so pretending to check them would
/// be worse than not checking them.
struct LeafVerifier<CipherSuite: TlsCipherSuite> {
    transcript: Option<CipherSuite::Hash>,
    /// What the certificate would prove *if* its signature checks out.
    ///
    /// Held rather than published, because `verify_certificate` runs before
    /// `verify_signature`: announcing an authentication state at the point
    /// where the peer has merely *claimed* a key would be announcing it for
    /// a peer that cannot sign with it.
    pending: Option<Authentication>,
    /// The leaf's DER `SubjectPublicKeyInfo`, copied because the buffer it
    /// was parsed out of is reused before `verify_signature` runs.
    spki: Vec<u8>,
    policy: PinPolicy,
    outcome: Rc<Cell<Outcome>>,
}

impl<CipherSuite: TlsCipherSuite> LeafVerifier<CipherSuite> {
    fn new(policy: PinPolicy, outcome: Rc<Cell<Outcome>>) -> LeafVerifier<CipherSuite> {
        LeafVerifier {
            transcript: None,
            pending: None,
            spki: Vec::new(),
            policy,
            outcome,
        }
    }
}

impl<CipherSuite: TlsCipherSuite> TlsVerifier<CipherSuite> for LeafVerifier<CipherSuite> {
    fn set_hostname_verification(&mut self, _hostname: &str) -> Result<(), TlsError> {
        Ok(())
    }

    fn verify_certificate(
        &mut self,
        transcript: &CipherSuite::Hash,
        certificate: CertificateRef<'_>,
    ) -> Result<(), TlsError> {
        let Some(CertificateEntryRef::X509(leaf)) = certificate.entries.first() else {
            self.outcome.set(Outcome::BadCertificate);
            return Err(TlsError::InvalidCertificate);
        };
        let Some(spki) = tab5_spki::leaf_spki(leaf) else {
            self.outcome.set(Outcome::BadCertificate);
            return Err(TlsError::InvalidCertificate);
        };
        self.spki.clear();
        if self.spki.try_reserve(spki.len()).is_err() {
            self.outcome.set(Outcome::BadCertificate);
            return Err(TlsError::InsufficientSpace);
        }
        self.spki.extend_from_slice(spki);
        self.transcript = Some(transcript.clone());

        if self.policy.pins.is_empty() {
            if self.policy.required {
                self.outcome.set(Outcome::PinMissing);
                return Err(TlsError::InvalidCertificate);
            }
            self.pending = Some(Authentication::Unverified);
            return Ok(());
        }

        let fingerprint: SpkiPin = sha2::Sha256::digest(spki).into();
        if self.policy.pins.contains(&fingerprint) {
            self.pending = Some(Authentication::Pinned);
            Ok(())
        } else {
            // No fallback. A host this firmware has an opinion about is a
            // host whose key it will not accept an alternative to.
            self.outcome.set(Outcome::PinMismatch);
            Err(TlsError::InvalidCertificate)
        }
    }

    fn verify_signature(&mut self, verify: CertificateVerifyRef<'_>) -> Result<(), TlsError> {
        let Some(transcript) = self.transcript.take() else {
            // `verify_certificate` did not run, so there is no key to check
            // the signature with and no transcript to check it over.
            self.outcome.set(Outcome::BadCertificate);
            return Err(TlsError::InvalidCertificate);
        };
        let Some(key) = tab5_spki::key_bytes(&self.spki) else {
            self.outcome.set(Outcome::BadCertificate);
            return Err(TlsError::InvalidCertificate);
        };

        // RFC 8446 4.4.3: 64 spaces, the context string, a zero byte, then
        // the transcript hash. The 64 spaces are what stops a signature made
        // for one purpose being replayed as another.
        let mut message = Vec::new();
        if message.try_reserve(160).is_err() {
            return Err(TlsError::InsufficientSpace);
        }
        message.resize(64, 0x20);
        message.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
        message.extend_from_slice(&transcript.finalize());

        let verified = match verify_scheme(verify.signature_scheme, key, &message, verify.signature)
        {
            Some(verified) => verified,
            None => {
                uart::log(b"TLS: unsupported CertificateVerify signature scheme\r\n");
                self.outcome.set(Outcome::BadCertificate);
                return Err(TlsError::InvalidSignatureScheme);
            }
        };
        if !verified {
            self.outcome.set(Outcome::BadCertificate);
            return Err(TlsError::InvalidSignature);
        }
        // The peer can sign with the key it presented, so what
        // `verify_certificate` worked out is now true rather than claimed.
        match self.pending.take() {
            Some(authentication) => {
                self.outcome.set(Outcome::Authenticated(authentication));
                Ok(())
            }
            None => {
                self.outcome.set(Outcome::BadCertificate);
                Err(TlsError::InvalidCertificate)
            }
        }
    }
}

/// Checks one signature, or `None` for a scheme this cannot check.
///
/// `None` and `Some(false)` are kept apart on purpose: one is "this
/// firmware does not implement that algorithm" and the other is "the server
/// signed something else". Only the second is an attack.
///
/// ECDSA P-256 and RSA-PSS are what `docs/TLS_PLAN.md` fixes. P-384 and
/// Ed25519 are left out for code size; the library advertises them in the
/// ClientHello and this cannot stop it from doing so (the list is private),
/// so a server holding only such a certificate fails here rather than being
/// refused earlier. That is a compatibility limit, not a security one.
fn verify_scheme(
    scheme: SignatureScheme,
    key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Option<bool> {
    match scheme {
        SignatureScheme::EcdsaSecp256r1Sha256 => {
            use p256::ecdsa::signature::Verifier;
            use p256::ecdsa::{Signature, VerifyingKey};
            let Ok(key) = VerifyingKey::from_sec1_bytes(key) else {
                return Some(false);
            };
            let Ok(signature) = Signature::from_der(signature) else {
                return Some(false);
            };
            Some(key.verify(message, &signature).is_ok())
        }
        SignatureScheme::RsaPssRsaeSha256 => Some(verify_rsa_pss::<sha2::Sha256>(
            key, message, signature,
        )),
        SignatureScheme::RsaPssRsaeSha384 => Some(verify_rsa_pss::<sha2::Sha384>(
            key, message, signature,
        )),
        SignatureScheme::RsaPssRsaeSha512 => Some(verify_rsa_pss::<sha2::Sha512>(
            key, message, signature,
        )),
        _ => None,
    }
}

fn verify_rsa_pss<Hash>(key: &[u8], message: &[u8], signature: &[u8]) -> bool
where
    Hash: Digest + digest::FixedOutputReset,
{
    use rsa::pkcs1::DecodeRsaPublicKey;
    use rsa::signature::Verifier;
    let Ok(key) = rsa::RsaPublicKey::from_pkcs1_der(key) else {
        return false;
    };
    let Ok(signature) = rsa::pss::Signature::try_from(signature) else {
        return false;
    };
    rsa::pss::VerifyingKey::<Hash>::from(key)
        .verify(message, &signature)
        .is_ok()
}

/// The randomness and the verifier, handed to the library together.
struct Provider<CipherSuite: TlsCipherSuite> {
    rng: Csprng,
    verifier: LeafVerifier<CipherSuite>,
}

impl<CipherSuite: TlsCipherSuite> CryptoProvider for Provider<CipherSuite> {
    type CipherSuite = CipherSuite;
    /// Only used for client certificates, which this never sends.
    type Signature = p256::ecdsa::DerSignature;

    fn rng(&mut self) -> impl rand_core::CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, TlsError> {
        // Always `Ok`. The library skips verification entirely when this
        // returns an error, which is the one behaviour this file exists to
        // make unreachable.
        Ok(&mut self.verifier)
    }
}

// ------------------------------------------------------------- the sequence

/// The whole life of one connection, as one future.
///
/// Sequential rather than concurrent because that is what an HTTP exchange
/// is: the request goes out, then the response comes back. Nothing here has
/// to read and write at the same time, so nothing here needs a `select`.
async fn run(
    shared: Rc<RefCell<Shared>>,
    outcome: Rc<Cell<Outcome>>,
    server_name: String,
    policy: PinPolicy,
    csprng: Csprng,
) -> Result<(), TlsError> {
    let mut read_buffer = vec![0u8; RECORD_BUFFER_BYTES];
    let mut write_buffer = vec![0u8; RECORD_BUFFER_BYTES];
    let mut tls: TlsConnection<'_, Io, Aes128GcmSha256> = TlsConnection::new(
        Io(shared.clone()),
        &mut read_buffer,
        &mut write_buffer,
    );

    let config = TlsConfig::new().with_server_name(&server_name);
    let provider = Provider {
        rng: csprng,
        verifier: LeafVerifier::<Aes128GcmSha256>::new(policy, outcome),
    };
    tls.open(TlsContext::new(&config, provider)).await?;
    // `open` returning is the point at which server Finished has been
    // verified. Nothing before it is a finished handshake, so nothing
    // before it may be displayed as one.
    shared.borrow_mut().handshake_done = true;

    // The request. The caller may still be writing it, so an empty queue is
    // only the end once it says so.
    let mut chunk = [0u8; CHUNK_BYTES];
    loop {
        let count = {
            let mut shared = shared.borrow_mut();
            let count = shared.plain_tx.len().min(chunk.len());
            for slot in chunk.iter_mut().take(count) {
                *slot = shared.plain_tx.pop_front().unwrap_or(0);
            }
            if count == 0 && shared.request_complete {
                break;
            }
            count
        };
        if count == 0 {
            YieldNow::new().await;
            continue;
        }
        let mut sent = 0;
        while sent < count {
            sent += tls.write(&chunk[sent..count]).await?;
        }
    }
    tls.flush().await?;

    // The response, until the peer says there is no more.
    loop {
        while shared.borrow().plain_rx.len() >= PLAIN_RX_CAP {
            YieldNow::new().await;
        }
        let count = match tls.read(&mut chunk).await {
            Ok(0) => break,
            Ok(count) => count,
            // A `close_notify`: the orderly end of a TLS stream.
            Err(TlsError::ConnectionClosed) => break,
            // A TCP close with no `close_notify` under it. The library
            // cannot tell that from a transport that broke -- both reach it
            // as a zero-length read -- so the transport says which it was.
            // Common enough to be ordinary: an HTTP/1.0 server that has
            // finished its response has nothing left to be polite with.
            //
            // The borrow is taken and released in its own block rather than
            // in a match guard, where how long it lives is a question
            // nobody reading this should have to answer.
            Err(TlsError::IoError) => {
                let at_end = {
                    let mut shared = shared.borrow_mut();
                    shared.closed_without_notify = shared.transport_eof;
                    shared.transport_eof
                };
                if at_end {
                    break;
                }
                return Err(TlsError::IoError);
            }
            Err(error) => return Err(error),
        };
        let mut shared = shared.borrow_mut();
        if shared.plain_rx.try_reserve(count).is_err() {
            return Err(TlsError::InsufficientSpace);
        }
        shared.plain_rx.extend(chunk[..count].iter().copied());
    }
    shared.borrow_mut().response_complete = true;
    Ok(())
}

// ----------------------------------------------------------- the transaction

/// What one [`Transaction::poll`] did.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Progress {
    /// TCP is still connecting, or the handshake is still running.
    Handshaking,
    /// The handshake finished during this poll; [`Transaction::authentication`]
    /// now answers. The caller decides here whether to go on -- an
    /// unauthenticated connection is a decision, and this is where it is
    /// made rather than after the body has arrived.
    Established,
    /// Plaintext arrived during this poll.
    Data,
    /// Nothing happened. Not an error.
    Idle,
    /// The peer finished the stream cleanly.
    Complete,
    Failed(Error),
}

#[derive(Clone, Copy, Default)]
pub struct Stats {
    /// Ciphertext read off the socket.
    pub received: usize,
    /// Ciphertext written to the socket.
    pub sent: usize,
    /// Plaintext handed to the caller.
    pub plaintext: u64,
    pub elapsed_ms: u64,
    pub handshake_ms: u64,
    /// The stream ended without a `close_notify`, so TLS cannot say nothing
    /// was cut off the end. The body's completeness is the HTTP framing's
    /// to judge.
    pub closed_without_notify: bool,
    /// The longest single [`Transaction::poll`], which is what the frame
    /// loop actually feels. Signature verification happens inside one of
    /// them and cannot be split, so this is the number that says whether
    /// the browser stutters.
    pub longest_poll_us: u32,
    pub polls: u32,
}

/// The whole connection as one boxed future.
///
/// Named because it appears in a struct field, a constructor and a `poll`,
/// and three spellings of the same type are three places to get it wrong.
type Engine = Pin<Box<dyn Future<Output = Result<(), TlsError>>>>;

enum State {
    Connecting,
    Running,
    Complete,
    Failed(Error),
}

/// One TLS connection in flight.
///
/// Owns a socket handle in the caller's `SocketSet` and the future driving
/// the engine, and nothing else -- specifically not the [`Stack`] or the
/// [`Rpc`], which are borrowed for the length of a single [`Transaction::poll`].
///
/// It must be handed to [`Transaction::close`]. Dropping it instead leaks
/// the socket out of the set, and since the set is `'static` that socket is
/// gone for the run; the `Drop` below says so on the UART rather than
/// letting it be silent.
#[must_use = "a Transaction owns a socket and has to be closed"]
pub struct Transaction {
    handle: SocketHandle,
    state: State,
    shared: Rc<RefCell<Shared>>,
    outcome: Rc<Cell<Outcome>>,
    /// `None` once the future has finished, so a completed connection is not
    /// polled again.
    future: Option<Engine>,
    authentication: Option<Authentication>,
    stats: Stats,
    started_ms: u64,
    last_progress_ms: u64,
    closed: bool,
}

impl Transaction {
    /// Opens a TCP connection and starts a TLS handshake over it.
    ///
    /// `server_name` is the DNS name from the URL: it goes in SNI and, when
    /// the host is pinned, it is what the pins were looked up under. It is
    /// not the address, which tells a virtual host nothing.
    ///
    /// Fails before touching the socket when there is no hardware
    /// randomness. That order is the point: a handshake seeded from a
    /// guessable value is worse than no handshake, so not one packet goes
    /// out (`docs/TLS_PLAN.md` Stage 2).
    pub fn start(
        stack: &mut Stack,
        address: Ipv4Address,
        port: u16,
        server_name: &str,
        policy: PinPolicy,
    ) -> Result<Transaction, Error> {
        let csprng = entropy::Csprng::from_hardware().map_err(|error| {
            uart::log(b"TLS: ");
            uart::log(error.message().as_bytes());
            uart::log(b"; not connecting\r\n");
            Error::Entropy
        })?;

        let mut name = String::new();
        if name.try_reserve(server_name.len()).is_err() {
            return Err(Error::OutOfMemory);
        }
        name.push_str(server_name);

        let shared = Rc::new(RefCell::new(Shared::new()));
        let outcome = Rc::new(Cell::new(Outcome::Pending));
        let future = Box::pin(run(
            shared.clone(),
            outcome.clone(),
            name,
            policy,
            csprng,
        ));

        let handle = stack.sockets_mut().add(tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0u8; SOCKET_BUFFER_BYTES]),
            tcp::SocketBuffer::new(vec![0u8; SOCKET_BUFFER_BYTES]),
        ));
        let now = tick::now_ms();
        let mut transaction = Transaction {
            handle,
            state: State::Connecting,
            shared,
            outcome,
            future: Some(future),
            authentication: None,
            stats: Stats::default(),
            started_ms: now,
            last_progress_ms: now,
            closed: false,
        };
        let local_port = 49152 + (delay::cycle_count() % 16384) as u16;
        let remote = IpEndpoint::new(IpAddress::Ipv4(address), port);
        if stack.connect_tcp(handle, remote, local_port).is_err() {
            // The socket is in the set and has to come out of it, which is
            // the caller's job through `close` -- so report the failure as
            // state rather than as an early return that drops the handle.
            transaction.state = State::Failed(Error::Local);
        }
        Ok(transaction)
    }

    /// Queues plaintext to be encrypted and sent.
    ///
    /// Nothing goes out until the handshake has finished; the future picks
    /// this up when it reaches its write phase.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut shared = self.shared.borrow_mut();
        shared
            .plain_tx
            .try_reserve(bytes.len())
            .map_err(|_| Error::OutOfMemory)?;
        shared.plain_tx.extend(bytes.iter().copied());
        Ok(())
    }

    /// Says the request is finished, so the future can stop waiting for more
    /// of it and start reading the response.
    pub fn finish_request(&mut self) {
        self.shared.borrow_mut().request_complete = true;
    }

    /// Takes decrypted bytes, up to `buffer.len()`.
    pub fn read(&mut self, buffer: &mut [u8]) -> usize {
        let count = {
            let mut shared = self.shared.borrow_mut();
            let count = buffer.len().min(shared.plain_rx.len());
            for slot in buffer.iter_mut().take(count) {
                *slot = shared.plain_rx.pop_front().unwrap_or(0);
            }
            count
        };
        self.stats.plaintext += count as u64;
        count
    }

    /// How much plaintext is waiting to be read.
    pub fn available(&self) -> usize {
        self.shared.borrow().plain_rx.len()
    }

    /// What the handshake proved, once it has finished.
    pub fn authentication(&self) -> Option<Authentication> {
        self.authentication
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Whether the stream ended with a TCP close rather than a
    /// `close_notify`. See [`Stats::closed_without_notify`].
    pub fn closed_without_notify(&self) -> bool {
        self.shared.borrow().closed_without_notify
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Complete | State::Failed(_))
    }

    /// Moves at most `budget` bytes of ciphertext off the socket, steps the
    /// engine once, and returns.
    ///
    /// `budget` is the caller's, and it is spent on *ciphertext*: a record
    /// has to be whole before any of it becomes plaintext, so a budget
    /// smaller than a record means several cheap polls to assemble one and
    /// then one expensive poll to decrypt it. That is the right way round
    /// for a frame loop.
    ///
    /// The three steps are always in this order: fill the inbound queue,
    /// step the engine, drain the outbound queue. Draining last is what
    /// puts a record the engine produced *this* poll on the wire in the same
    /// poll, rather than a frame later.
    pub fn poll(&mut self, stack: &mut Stack, rpc: &mut Rpc, budget: usize) -> Progress {
        self.stats.polls = self.stats.polls.saturating_add(1);
        match self.state {
            State::Complete => return Progress::Complete,
            State::Failed(error) => return Progress::Failed(error),
            _ => {}
        }
        if !stack.poll(rpc) {
            return self.fail(Error::LinkLost);
        }
        if matches!(self.state, State::Connecting)
            && let Some(progress) = self.poll_connecting(stack)
        {
            return progress;
        }

        let started = delay::cycle_count();
        let received = self.fill_from_socket(stack, budget);
        let established_before = self.authentication.is_some();
        let engine = self.step_engine();
        let sent = self.drain_to_socket(stack);
        self.record_poll_time(started);

        if let Some(error) = engine {
            return self.fail(error);
        }
        let delivered = self.shared.borrow().plain_rx.len();
        if received > 0 || sent > 0 || delivered > 0 {
            self.last_progress_ms = tick::now_ms();
        } else if tick::now_ms().saturating_sub(self.last_progress_ms) > IDLE_TIMEOUT_MS {
            return self.fail(Error::TimedOut);
        }

        if !established_before && self.authentication.is_some() {
            self.stats.handshake_ms = tick::now_ms().saturating_sub(self.started_ms);
            return Progress::Established;
        }
        if self.future.is_none() && delivered == 0 {
            return self.complete();
        }
        if delivered > 0 {
            return Progress::Data;
        }
        if self.authentication.is_none() {
            return Progress::Handshaking;
        }
        Progress::Idle
    }

    /// `Some` while the connection is still being made, `None` once the
    /// handshake may start.
    fn poll_connecting(&mut self, stack: &mut Stack) -> Option<Progress> {
        if stack
            .sockets_mut()
            .get_mut::<tcp::Socket>(self.handle)
            .may_send()
        {
            self.state = State::Running;
            self.last_progress_ms = tick::now_ms();
            return None;
        }
        if tick::now_ms().saturating_sub(self.started_ms) > CONNECT_TIMEOUT_MS {
            return Some(self.fail(Error::Connect));
        }
        Some(Progress::Handshaking)
    }

    fn fill_from_socket(&mut self, stack: &mut Stack, budget: usize) -> usize {
        let socket = stack.sockets_mut().get_mut::<tcp::Socket>(self.handle);
        let mut received = 0usize;
        while received < budget {
            let mut chunk = [0u8; 512];
            let want = chunk.len().min(budget - received);
            match socket.recv_slice(&mut chunk[..want]) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    self.shared
                        .borrow_mut()
                        .cipher_rx
                        .extend(chunk[..count].iter().copied());
                    received += count;
                }
            }
        }
        if !socket.may_recv() {
            self.shared.borrow_mut().peer_closed = true;
        }
        self.stats.received += received;
        self.shared.borrow_mut().read_budget = budget;
        received
    }

    fn drain_to_socket(&mut self, stack: &mut Stack) -> usize {
        let socket = stack.sockets_mut().get_mut::<tcp::Socket>(self.handle);
        let mut sent = 0usize;
        loop {
            let chunk: Vec<u8> = {
                let mut shared = self.shared.borrow_mut();
                let count = shared.cipher_tx.len().min(1024);
                if count == 0 {
                    break;
                }
                shared.cipher_tx.drain(..count).collect()
            };
            match socket.send_slice(&chunk) {
                Ok(count) => {
                    sent += count;
                    if count < chunk.len() {
                        // The socket took only part of it; the rest goes
                        // back at the front of the queue for the next poll.
                        let mut shared = self.shared.borrow_mut();
                        for &byte in chunk[count..].iter().rev() {
                            shared.cipher_tx.push_front(byte);
                        }
                        break;
                    }
                }
                Err(_) => {
                    let mut shared = self.shared.borrow_mut();
                    for &byte in chunk.iter().rev() {
                        shared.cipher_tx.push_front(byte);
                    }
                    break;
                }
            }
        }
        self.stats.sent += sent;
        sent
    }

    /// Polls the future once, and turns whatever it said into an [`Error`].
    fn step_engine(&mut self) -> Option<Error> {
        let future = self.future.as_mut()?;
        let mut context = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut context) {
            Poll::Pending => {
                self.note_outcome();
                None
            }
            Poll::Ready(result) => {
                self.future = None;
                self.note_outcome();
                match result {
                    Ok(()) => None,
                    Err(error) => Some(self.classify(error)),
                }
            }
        }
    }

    /// Picks up what the verifier concluded, once, and not before the
    /// handshake it belongs to has finished.
    ///
    /// Both conditions matter. The verifier's conclusion is only true once
    /// its signature check has passed, and the handshake is only finished
    /// once server Finished has been checked too -- which happens after the
    /// verifier has had its say. Reporting between the two would be
    /// reporting an authentication for a session that may still fail.
    fn note_outcome(&mut self) {
        if self.authentication.is_none()
            && self.shared.borrow().handshake_done
            && let Outcome::Authenticated(authentication) = self.outcome.get()
        {
            self.authentication = Some(authentication);
        }
    }

    /// Turns a library error into this module's vocabulary.
    ///
    /// The verifier's own conclusion wins where it has one: the library
    /// reports a pin mismatch as `InvalidCertificate`, which is true but is
    /// not the answer the person in front of the screen needs.
    fn classify(&self, error: TlsError) -> Error {
        match self.outcome.get() {
            Outcome::PinMismatch => return Error::Pin,
            Outcome::PinMissing => return Error::PinMissing,
            Outcome::BadCertificate => return Error::Certificate,
            _ => {}
        }
        match error {
            TlsError::HandshakeAborted(..) | TlsError::AbortHandshake(..) => Error::Alert,
            TlsError::InvalidCertificate
            | TlsError::InvalidCertificateEntry
            | TlsError::InvalidSignature
            | TlsError::InvalidSignatureScheme => Error::Certificate,
            TlsError::InvalidCipherSuite | TlsError::InvalidSupportedVersions => Error::Version,
            TlsError::InsufficientSpace | TlsError::OutOfMemory => Error::Limit,
            TlsError::ConnectionClosed | TlsError::IoError => {
                if self.shared.borrow().cancelled {
                    Error::Cancelled
                } else {
                    Error::Connect
                }
            }
            _ => Error::Local,
        }
    }

    fn record_poll_time(&mut self, started: u32) {
        // The cycle counter wraps every ~11.9 s, which no single poll comes
        // near; a wrapped subtraction is exact for anything shorter.
        let cycles = delay::cycle_count().wrapping_sub(started);
        let microseconds = delay::cycles_to_us(cycles);
        self.stats.longest_poll_us = self.stats.longest_poll_us.max(microseconds);
    }

    fn complete(&mut self) -> Progress {
        self.state = State::Complete;
        self.stats.closed_without_notify = self.shared.borrow().closed_without_notify;
        self.stats.elapsed_ms = tick::now_ms().saturating_sub(self.started_ms);
        Progress::Complete
    }

    fn fail(&mut self, error: Error) -> Progress {
        self.state = State::Failed(error);
        self.stats.elapsed_ms = tick::now_ms().saturating_sub(self.started_ms);
        Progress::Failed(error)
    }

    /// Stops the connection. The socket still has to be [`Transaction::close`]d.
    pub fn cancel(&mut self) {
        if !self.is_finished() {
            self.shared.borrow_mut().cancelled = true;
            // Dropping the future here rather than at `close` so that the
            // record buffers go back to the heap as soon as the caller has
            // stopped caring about them.
            self.future = None;
            self.state = State::Failed(Error::Cancelled);
            self.stats.elapsed_ms = tick::now_ms().saturating_sub(self.started_ms);
        }
    }

    /// Aborts the connection and takes the socket out of the set.
    ///
    /// No `close_notify` is sent. Writing one needs the engine to encrypt
    /// it and the socket to carry it, which is two more polls at a moment
    /// the caller has already decided to stop; the peer sees a TCP reset
    /// instead, which it has to handle anyway.
    pub fn close(mut self, stack: &mut Stack, rpc: &mut Rpc) -> Stats {
        self.future = None;
        stack
            .sockets_mut()
            .get_mut::<tcp::Socket>(self.handle)
            .abort();
        stack.pump_until(rpc, ABORT_PUMP_MS, |_| false);
        stack.sockets_mut().remove(self.handle);
        self.closed = true;
        if self.stats.elapsed_ms == 0 {
            self.stats.elapsed_ms = tick::now_ms().saturating_sub(self.started_ms);
        }
        self.stats
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.closed {
            uart::log(b"TLS: a transaction was dropped without close()\r\n");
        }
    }
}
