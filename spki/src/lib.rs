//! Finding the public key inside a certificate the network sent.
//!
//! TLS 1.3's `CertificateVerify` is a signature over the handshake made with
//! the key in the leaf certificate, so verifying it means getting that key
//! out of a DER blob a stranger chose. That is the one piece of the TLS path
//! that parses fully attacker-controlled input, which is why it lives here:
//! a workspace member builds for the host, so every rejection this makes can
//! be a `cargo test` rather than a hope.
//!
//! Two things come out of a leaf, and they are not the same bytes:
//!
//! - the whole `SubjectPublicKeyInfo` structure, algorithm identifier
//!   included, which is what an SPKI pin is a SHA-256 of
//! - the `subjectPublicKey` bit string's contents, which is what a signature
//!   verifier wants
//!
//! Pinning the second would be a mistake: two different algorithms can carry
//! the same key bytes, and a pin has to name the algorithm as well as the
//! key. So [`leaf_spki`] returns the first and [`key_bytes`] narrows it to
//! the second, and the caller says which one it meant.
//!
//! ## What this deliberately does not do
//!
//! It does not validate a certificate. It reads no validity dates, no
//! subject, no SAN, no basic constraints, no extensions at all, and it never
//! looks at a second certificate in the chain. Those checks belong to the
//! public-CA profile (`docs/TLS_PLAN.md` Stage 9) and are not what the
//! unauthenticated and pinned profiles rest on. Nothing here should be read
//! as "the certificate is good"; the only claim it makes is "this is where
//! the key is".

#![cfg_attr(not(test), no_std)]

/// The largest DER length this will decode.
///
/// A length field can say four bytes' worth, and on a 32-bit target the
/// arithmetic that follows would then be a step away from overflowing. Every
/// structure this walks is inside one TLS certificate message, which the TLS
/// layer has already bounded far below this.
const MAX_LENGTH: usize = 1 << 24;

/// The bytes of a leaf certificate's `SubjectPublicKeyInfo`, tag and length
/// included.
///
/// `None` for anything that is not a certificate shaped the way X.509 says:
/// a truncated structure, a length that runs past the end, a field of the
/// wrong tag. There is no lenient path -- a certificate this cannot walk is
/// one whose key it cannot claim to have found.
pub fn leaf_spki(certificate: &[u8]) -> Option<&[u8]> {
    // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm,
    //                            signatureValue }
    let mut rest = enter(certificate, SEQUENCE)?;
    // TBSCertificate ::= SEQUENCE { [0] version DEFAULT v1, serialNumber,
    //                               signature, issuer, validity, subject,
    //                               subjectPublicKeyInfo, ... }
    rest = enter(take(rest)?, SEQUENCE)?;
    // The version is `[0] EXPLICIT` and defaulted, so a v1 certificate does
    // not carry it at all and the serial number comes first. Testing the
    // tag rather than assuming either shape is what makes both parse.
    if header(rest)?.tag & 0xE0 == CONTEXT_CONSTRUCTED {
        rest = skip(rest)?;
    }
    // serialNumber, signature, issuer, validity, subject.
    for _ in 0..5 {
        rest = skip(rest)?;
    }
    let spki = take(rest)?;
    // Confirming the tag here rather than trusting the count above: if any
    // of those five fields was absent, this lands on something that is not
    // an SPKI and the certificate is rejected instead of the wrong bytes
    // being returned as a key.
    (header(spki)?.tag == SEQUENCE).then_some(spki)
}

/// The `subjectPublicKey` bit string's contents, without its unused-bits
/// count.
///
/// `None` when the bit string has a non-zero unused-bit count as well as
/// when the structure does not parse: a public key is a whole number of
/// bytes in every algorithm this supports, so a partial final byte means the
/// encoding is not one this understands.
pub fn key_bytes(spki: &[u8]) -> Option<&[u8]> {
    // SubjectPublicKeyInfo ::= SEQUENCE { algorithm AlgorithmIdentifier,
    //                                     subjectPublicKey BIT STRING }
    let inner = enter(spki, SEQUENCE)?;
    let rest = skip(inner)?;
    let content = enter(take(rest)?, BIT_STRING)?;
    match content.split_first() {
        Some((0, key)) if !key.is_empty() => Some(key),
        _ => None,
    }
}

/// The `algorithm` `AlgorithmIdentifier` of an SPKI, tag and length
/// included.
///
/// A caller that pins does not need this -- the pin covers it -- but a
/// caller reporting what it saw does, and reaching into the SPKI a second
/// time by hand is how the two readings drift apart.
pub fn key_algorithm(spki: &[u8]) -> Option<&[u8]> {
    take(enter(spki, SEQUENCE)?)
}

const SEQUENCE: u8 = 0x30;
const BIT_STRING: u8 = 0x03;
/// Class bits for a constructed context-specific tag, `[n]`.
const CONTEXT_CONSTRUCTED: u8 = 0xA0;

struct Header {
    tag: u8,
    /// How many bytes the tag and length take up.
    length_of_header: usize,
    /// How many bytes the contents take up.
    length: usize,
}

/// Decodes one tag-length pair, checking that the contents it promises are
/// actually there.
///
/// Rejecting a length that runs past the end here, rather than when the
/// slice is indexed, is what lets everything below use plain slicing: by the
/// time a `Header` exists, `length_of_header + length` is in bounds.
fn header(input: &[u8]) -> Option<Header> {
    let tag = *input.first()?;
    let first = *input.get(1)?;
    let (length, length_of_header) = if first & 0x80 == 0 {
        // Short form: the byte is the length.
        (usize::from(first), 2)
    } else {
        // Long form: the low bits count the length's own bytes. Zero of them
        // is the indefinite form, which DER does not allow.
        let count = usize::from(first & 0x7F);
        if count == 0 || count > 4 {
            return None;
        }
        let mut length = 0usize;
        for index in 0..count {
            length = (length << 8) | usize::from(*input.get(2 + index)?);
        }
        if length > MAX_LENGTH {
            return None;
        }
        (length, 2 + count)
    };
    (length_of_header.checked_add(length)? <= input.len()).then_some(Header {
        tag,
        length_of_header,
        length,
    })
}

/// The first value in `input`, tag and length included.
fn take(input: &[u8]) -> Option<&[u8]> {
    let header = header(input)?;
    Some(&input[..header.length_of_header + header.length])
}

/// Everything after the first value in `input`.
fn skip(input: &[u8]) -> Option<&[u8]> {
    let header = header(input)?;
    Some(&input[header.length_of_header + header.length..])
}

/// The contents of the first value in `input`, if it has the expected tag.
fn enter(input: &[u8], expected: u8) -> Option<&[u8]> {
    let header = header(input)?;
    (header.tag == expected)
        .then(|| &input[header.length_of_header..header.length_of_header + header.length])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Certificates and the answers OpenSSL gives for them, so that what is
    /// being checked is agreement with a real X.509 implementation rather
    /// than agreement with this file's own idea of the format.
    ///
    /// `ecdsa_v1_leaf` is signed with the same key as `ecdsa_p256_leaf` but
    /// has no `[0] version` field, which is the one optional element in the
    /// walk.
    const ECDSA_LEAF: &[u8] = include_bytes!("../data/ecdsa_p256_leaf.der");
    const ECDSA_SPKI: &[u8] = include_bytes!("../data/ecdsa_p256_leaf_spki.der");
    const RSA_LEAF: &[u8] = include_bytes!("../data/rsa2048_leaf.der");
    const RSA_SPKI: &[u8] = include_bytes!("../data/rsa2048_leaf_spki.der");
    const V1_LEAF: &[u8] = include_bytes!("../data/ecdsa_v1_leaf.der");
    const V1_SPKI: &[u8] = include_bytes!("../data/ecdsa_v1_leaf_spki.der");

    #[test]
    fn finds_the_key_openssl_reports() {
        for (leaf, expected) in [
            (ECDSA_LEAF, ECDSA_SPKI),
            (RSA_LEAF, RSA_SPKI),
            (V1_LEAF, V1_SPKI),
        ] {
            assert_eq!(leaf_spki(leaf), Some(expected));
        }
    }

    #[test]
    fn a_v1_certificate_parses_without_a_version_field() {
        // Both fixtures carry the same key, so the walk having taken the
        // other branch is visible as the same answer rather than as a
        // coincidence of two different certificates.
        assert_eq!(leaf_spki(V1_LEAF), leaf_spki(ECDSA_LEAF));
    }

    #[test]
    fn key_bytes_are_the_uncompressed_point_and_the_rsa_key() {
        let ecdsa = key_bytes(ECDSA_SPKI).expect("a P-256 key");
        // SEC1 uncompressed: 0x04 then two 32-byte coordinates.
        assert_eq!(ecdsa.len(), 65);
        assert_eq!(ecdsa[0], 0x04);

        let rsa = key_bytes(RSA_SPKI).expect("an RSA key");
        // RSAPublicKey ::= SEQUENCE { modulus, publicExponent }.
        assert_eq!(rsa[0], SEQUENCE);
        assert!(rsa.len() > 256, "a 2048-bit modulus and an exponent");
    }

    #[test]
    fn the_algorithm_is_reported_separately_from_the_key() {
        let ecdsa = key_algorithm(ECDSA_SPKI).expect("an algorithm identifier");
        let rsa = key_algorithm(RSA_SPKI).expect("an algorithm identifier");
        assert_eq!(ecdsa[0], SEQUENCE);
        assert_ne!(ecdsa, rsa);
        // The whole SPKI is the algorithm plus the key plus its own header,
        // which is the difference a pin covers and a bare key does not.
        assert!(ECDSA_SPKI.len() > ecdsa.len() + key_bytes(ECDSA_SPKI).unwrap().len());
    }

    #[test]
    fn nothing_at_all_is_not_a_certificate() {
        for input in [&b""[..], &b"\x30"[..], &b"\x30\x82"[..], &b"not der"[..]] {
            assert_eq!(leaf_spki(input), None, "{input:?}");
        }
    }

    /// Every prefix of a real certificate has to be rejected. A parser that
    /// answers from a truncated certificate is one that can be fed half a
    /// certificate and asked to name a key.
    #[test]
    fn every_truncation_is_rejected() {
        for leaf in [ECDSA_LEAF, RSA_LEAF, V1_LEAF] {
            for end in 0..leaf.len() {
                assert_eq!(leaf_spki(&leaf[..end]), None, "truncated to {end}");
            }
            assert!(leaf_spki(leaf).is_some());
        }
    }

    /// Flipping a bit in a length byte must not produce an answer. The
    /// content bytes are a different matter -- this does not authenticate a
    /// certificate, and a corrupted key is the signature check's problem --
    /// so what is checked here is that the *structure* is not walked past
    /// its own bounds.
    #[test]
    fn a_length_past_the_end_is_rejected() {
        let mut leaf = ECDSA_LEAF.to_vec();
        // The outer SEQUENCE's long-form length, made one larger than the
        // bytes that follow it.
        leaf[3] = leaf[3].wrapping_add(1);
        assert_eq!(leaf_spki(&leaf), None);

        let mut leaf = ECDSA_LEAF.to_vec();
        // The indefinite-length form, which DER forbids.
        leaf[1] = 0x80;
        assert_eq!(leaf_spki(&leaf), None);

        let mut leaf = ECDSA_LEAF.to_vec();
        // A length claiming more bytes than a `usize` walk should accept.
        leaf[1] = 0x85;
        assert_eq!(leaf_spki(&leaf), None);
    }

    #[test]
    fn a_certificate_whose_body_is_not_a_sequence_is_rejected() {
        let mut leaf = ECDSA_LEAF.to_vec();
        leaf[4] = 0x31; // SET instead of SEQUENCE
        assert_eq!(leaf_spki(&leaf), None);

        let mut leaf = ECDSA_LEAF.to_vec();
        leaf[0] = 0x31;
        assert_eq!(leaf_spki(&leaf), None);
    }

    /// A body with fewer fields than X.509 requires must not have whatever
    /// happens to sit in the seventh position returned as its key.
    #[test]
    fn a_short_body_has_no_key() {
        // A certificate whose TBSCertificate holds one INTEGER and nothing
        // else, wrapped the way a real one is.
        let body = [0x30u8, 0x03, 0x02, 0x01, 0x00];
        let mut certificate = vec![0x30, (body.len() + 2) as u8];
        certificate.extend_from_slice(&body);
        certificate.extend_from_slice(&[0x05, 0x00]); // a NULL after it
        assert_eq!(leaf_spki(&certificate), None);
    }

    #[test]
    fn a_key_that_is_not_a_whole_number_of_bytes_is_rejected() {
        let mut spki = ECDSA_SPKI.to_vec();
        // The BIT STRING's unused-bit count, which is zero in every key this
        // supports. Finding it non-zero means the encoding is not one this
        // can hand to a verifier.
        let bit_string = spki.len() - 66;
        assert_eq!(spki[bit_string], 0, "the fixture's unused-bit count");
        spki[bit_string] = 4;
        assert_eq!(key_bytes(&spki), None);
    }

    #[test]
    fn an_empty_key_is_not_a_key() {
        // SEQUENCE { SEQUENCE {}, BIT STRING { 0 unused bits, no content } }
        let spki = [0x30u8, 0x07, 0x30, 0x02, 0x05, 0x00, 0x03, 0x01, 0x00];
        assert_eq!(key_bytes(&spki), None);
    }

    #[test]
    fn a_key_that_is_not_a_bit_string_is_rejected() {
        let mut spki = ECDSA_SPKI.to_vec();
        let bit_string = spki.len() - 68;
        assert_eq!(spki[bit_string], BIT_STRING, "the fixture's key tag");
        spki[bit_string] = 0x04; // OCTET STRING
        assert_eq!(key_bytes(&spki), None);
    }
}
