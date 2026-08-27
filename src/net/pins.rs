//! The SPKI pins compiled into this firmware, and the lookup by hostname.
//!
//! A pin is the SHA-256 of a leaf certificate's DER `SubjectPublicKeyInfo`.
//! When a host has one, a TLS connection to it is only accepted if the key
//! it presents hashes to it -- and a mismatch fails as
//! [`tls::Error::Pin`](crate::net::tls::Error::Pin) rather than falling back
//! to the unauthenticated profile. That refusal is the whole value of the
//! mechanism: a pin that can be got around by making it fail is not a pin.
//!
//! ## Why the table is generated and not editable at run time
//!
//! There is no way to add a pin from the board, and there is no trust on
//! first use. Both are deliberate. Recording whatever key answered the first
//! time protects against nothing an attacker who is present at that moment
//! cannot do, while making the board carry a key nobody chose; and a runtime
//! store is a runtime store an attacker with a foothold can write to.
//!
//! So pins arrive the way the code does -- by a firmware update -- and the
//! table comes out of `tools/pins/generate.py`, which reads text files a
//! person edits and prints the hash of what it read. `generated.rs` beside
//! this file is its output and is not written by hand.
//!
//! ## What is actually pinned today
//!
//! Nothing. `tools/pins/pins.txt` is empty, so a normal build never reports
//! [`Authentication::Pinned`](crate::net::tls::Authentication::Pinned) and
//! every `https://` connection is unauthenticated. Pinning a host is a
//! promise to refuse it under any other key until a firmware update says
//! otherwise, and nothing this board does yet is worth that promise.
//!
//! The fixture pins are real, but only under the `tls-fixture-pins` feature:
//! their private keys are in `tools/tls/`, which is safe exactly as long as
//! they are never in a released image. `generate.py --check-release` looks
//! for their bytes in a linked ELF and fails if it finds them.

use crate::net::tls::{PinPolicy, SpkiPin};

mod generated;

/// One host's pins, as the generator writes them.
pub(super) struct Host {
    /// Lowercase, exactly as [`crate::browser::url::Url::host`] produces it.
    /// The generator rejects anything else, so no normalisation happens
    /// here -- a lookup that lowercased its argument would be hiding a
    /// caller that had the wrong string.
    name: &'static str,
    pins: &'static [SpkiPin],
}

/// The pins registered for a host, and whether one is required.
///
/// A host with no pins gets [`PinPolicy::UNAUTHENTICATED`]: it is opened,
/// unauthenticated, and displayed as such. That is not the same as
/// requiring a pin and not having one, which fails -- and the difference is
/// the caller's to choose, not this table's, so `required` stays false here
/// and a caller that will act on the content asks for it explicitly.
pub fn policy_for(host: &str) -> PinPolicy {
    match lookup(host) {
        Some(pins) => PinPolicy {
            pins,
            required: false,
        },
        None => PinPolicy::UNAUTHENTICATED,
    }
}

/// Whether anything is pinned for a host.
///
/// What a redirect away from an authenticated connection is checked
/// against: moving to a host this firmware cannot authenticate is the
/// downgrade `tls-auth-downgrade` refuses.
pub fn is_pinned(host: &str) -> bool {
    lookup(host).is_some()
}

/// The pins for a host, or `None` when it has none.
///
/// Binary search over a table the generator sorted. Linear would be fine at
/// today's size -- which is zero -- but the ordering is free to produce and
/// the search is what keeps a table that grows from costing anything.
fn lookup(host: &str) -> Option<&'static [SpkiPin]> {
    if let Some(pins) = search(generated::PRODUCTION, host) {
        return Some(pins);
    }
    #[cfg(feature = "tls-fixture-pins")]
    if let Some(pins) = search(generated::FIXTURES, host) {
        return Some(pins);
    }
    None
}

fn search(table: &'static [Host], host: &str) -> Option<&'static [SpkiPin]> {
    table
        .binary_search_by(|entry| entry.name.cmp(host))
        .ok()
        .map(|index| table[index].pins)
}
