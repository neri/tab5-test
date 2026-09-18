//! Turning a page's bytes into the UTF-8 the tokenizer reads.
//!
//! Three encodings: UTF-8, which is what almost everything is, and the two
//! legacy Japanese encodings Shift_JIS and EUC-JP. A page in either legacy
//! encoding decoded as UTF-8 is not a page with a few wrong characters --
//! every kanji in it is invalid UTF-8, so the whole body becomes replacement
//! characters and there is nothing to read at all.
//!
//! This sits inside [`crate::document::Parser`] rather than in front of it,
//! so the firmware's fetch, the `hs` and `bt` diagnostics and this crate's
//! own fixture tests all go through the same decoder. A transcoder bolted
//! on at one call site is one the other callers do not have.
//!
//! ```text
//! bytes -> Decoder -> UTF-8 -> Tokenizer -> Builder -> Document
//! ```
//!
//! **UTF-8 costs nothing.** Once the encoding is settled and it is UTF-8,
//! [`Decoder::feed`] hands the caller's slice straight through: no copy, no
//! buffer, no allocation. The work below happens only for the pages that
//! need it.
//!
//! The shared JIS-row table is generated: `tools/encoding/generate_shiftjis.py` writes
//! `browser/data/shiftjis.bin` from CPython's `cp932`, which is checked in
//! and linked into DROM. Windows-31J and not the narrower JIS X 0208,
//! because what the web labels `Shift_JIS` is Windows-31J in practice --
//! the same choice WHATWG's encoding standard makes. EUC-JP uses the first
//! 94 rows of the same table with different byte arithmetic.

use alloc::vec::Vec;

use crate::error::Error;
use crate::memory;

/// How far into a document the `<meta>` scan looks when the response
/// headers did not say.
///
/// A kilobyte, which every real document puts its `<meta>` inside: the
/// element is only meaningful in `<head>`, and a `<head>` that has not
/// declared its encoding within a kilobyte has effectively not declared
/// one. Bytes are held -- not parsed and then reparsed -- until either a
/// declaration is found or this much has gone by, and this is the only
/// buffering the decoder ever does.
pub const SNIFF_BYTES: usize = 1024;

/// What one character's UTF-8 goes into before it is handed on.
///
/// A stack buffer and not a field: the decoder holds no output between
/// calls, so there is nothing here to count against the page's memory
/// budget. Flushed whenever the next character might not fit.
const OUT_BYTES: usize = 512;

/// What a byte that cannot be decoded becomes.
///
/// The same choice the tokenizer makes for invalid UTF-8: a visible
/// character in the reader's text, not a dropped byte and not a refused
/// page. A single bad byte in a megabyte of Japanese is a typo, not a
/// reason to show nothing.
const REPLACEMENT: char = '\u{FFFD}';

/// Where decoded UTF-8 goes.
///
/// Erased rather than generic: see [`Decoder::push_bytes`].
type Sink<'a> = dyn FnMut(&[u8]) -> Result<(), Error> + 'a;

/// The encodings a page's bytes may be in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Utf8,
    /// Windows-31J, which is what `Shift_JIS` labels in the wild.
    ShiftJis,
    /// JIS X 0208 plus the EUC-JP halfwidth-katakana shift sequence.
    EucJp,
}

/// The encoding a `charset` label names, or `None` for one this does not
/// know.
///
/// An unknown label is not an error and not a refusal. Reading it as UTF-8
/// -- which is what `None` leads to -- shows a page with some wrong
/// characters in it, and refusing shows nothing at all; the first is what a
/// reader can work with.
pub fn from_label(label: &[u8]) -> Option<Encoding> {
    let label = trim(label);
    // The spellings in the wild, all of which mean Windows-31J here. The
    // list is short on purpose: these are the ones that actually appear.
    for name in [
        b"shift_jis".as_slice(),
        b"shift-jis",
        b"sjis",
        b"s-jis",
        b"x-sjis",
        b"shiftjis",
        b"ms_kanji",
        b"windows-31j",
        b"cp932",
        b"csshiftjis",
    ] {
        if label.eq_ignore_ascii_case(name) {
            return Some(Encoding::ShiftJis);
        }
    }
    for name in [
        b"euc-jp".as_slice(),
        b"euc_jp",
        b"eucjp",
        b"x-euc-jp",
        b"cseucpkdfmtjapanese",
    ] {
        if label.eq_ignore_ascii_case(name) {
            return Some(Encoding::EucJp);
        }
    }
    for name in [b"utf-8".as_slice(), b"utf8", b"us-ascii", b"ascii"] {
        if label.eq_ignore_ascii_case(name) {
            return Some(Encoding::Utf8);
        }
    }
    None
}

/// A page's bytes on the way to the tokenizer.
///
/// Holds the settled encoding, bytes held while it was undecided, and the
/// partial legacy-encoding character at a chunk boundary. The last one is
/// why this is a type and not a function -- a multibyte character split
/// across calls is the ordinary case, not an edge one.
pub struct Decoder {
    encoding: Option<Encoding>,
    /// Whether the document is markup, and so might declare its own
    /// encoding in a `<meta>`.
    ///
    /// False for `text/plain`, where the characters `<meta charset=...>`
    /// are a file that says so rather than a file that means it -- and
    /// where nothing then has to be held back while a kilobyte is
    /// searched. Only the byte order mark is looked for, which is three
    /// bytes.
    markup: bool,
    held: Vec<u8>,
    lead: Option<u8>,
    /// An EUC-JP `0x8F` sequence has consumed its second byte and is waiting
    /// for the third. JIS X 0212 is not in the display table, so the complete
    /// sequence becomes one replacement character rather than three.
    euc_plane2: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            encoding: None,
            markup: true,
            held: Vec::new(),
            lead: None,
            euc_plane2: false,
        }
    }

    /// A decoder for a document that is not markup: no `<meta>` is looked
    /// for, so nothing is held except while a byte order mark is decided.
    pub fn plain() -> Decoder {
        Decoder {
            markup: false,
            ..Decoder::new()
        }
    }

    /// How far in this will look before settling for the default.
    fn window(&self) -> usize {
        if self.markup { SNIFF_BYTES } else { BOM.len() }
    }

    /// Settles the encoding from a `Content-Type` header's `charset`.
    ///
    /// The header wins over anything in the document, and settling here
    /// means nothing is ever held: the sniff below is what happens when
    /// there was no header to ask. Call before the first [`Decoder::feed`];
    /// afterwards it does nothing, because the bytes it would have applied
    /// to have already gone.
    pub fn declare(&mut self, label: &[u8]) {
        if self.encoding.is_some() || !self.held.is_empty() {
            return;
        }
        self.encoding = from_label(label);
    }

    /// What the decoder is holding, for the page's memory accounting.
    pub fn owned_bytes(&self) -> usize {
        self.held.capacity()
    }

    /// Hands `bytes` on as UTF-8.
    ///
    /// `sink` may be called any number of times, including none: bytes are
    /// held while the encoding is undecided, and a Shift_JIS character
    /// split across two calls is completed on the second.
    pub fn feed<F>(&mut self, bytes: &[u8], mut sink: F) -> Result<(), Error>
    where
        F: FnMut(&[u8]) -> Result<(), Error>,
    {
        self.push_bytes(bytes, &mut sink)
    }

    /// The same, with the sink erased.
    ///
    /// Everything below this point takes `&mut dyn` rather than a type
    /// parameter. `sniff` finishes by handing the rest of its chunk back to
    /// this function, and a generic version of that is a function that
    /// instantiates itself with one more `&mut` on every recursion until
    /// the compiler gives up -- which is exactly what it did.
    fn push_bytes(&mut self, bytes: &[u8], sink: &mut Sink<'_>) -> Result<(), Error> {
        match self.encoding {
            Some(Encoding::Utf8) => sink(bytes),
            Some(Encoding::ShiftJis) => self.decode_shift_jis(bytes, sink),
            Some(Encoding::EucJp) => self.decode_euc_jp(bytes, sink),
            None => self.sniff(bytes, sink),
        }
    }

    /// Ends the input: whatever is still held goes out.
    ///
    /// A document shorter than the sniff window has never decided anything,
    /// so this is where most small pages are decoded. A lead byte with no
    /// trail becomes one replacement character, which is what a truncated
    /// page's last character is.
    pub fn finish<F>(&mut self, mut sink: F) -> Result<(), Error>
    where
        F: FnMut(&[u8]) -> Result<(), Error>,
    {
        let sink: &mut Sink<'_> = &mut sink;
        if self.encoding.is_none() {
            self.settle(sink)?;
        }
        if self.lead.take().is_some() {
            self.euc_plane2 = false;
            sink(replacement_bytes())?;
        }
        Ok(())
    }

    /// Holds bytes until the encoding is known, then releases them.
    fn sniff(&mut self, bytes: &[u8], sink: &mut Sink<'_>) -> Result<(), Error> {
        let window = self.window();
        let room = window - self.held.len();
        let taken = room.min(bytes.len());
        memory::extend_from_slice(&mut self.held, &bytes[..taken])?;
        // Still nothing to go on and still room to look: wait for more.
        // `taken == bytes.len()` here, because the window only fills when
        // there was less room than there were bytes.
        if detect(&self.held, self.markup).is_none() && self.held.len() < window {
            return Ok(());
        }
        self.settle(sink)?;
        self.push_bytes(&bytes[taken..], sink)
    }

    /// Decides what the held bytes are, and pushes them through.
    ///
    /// UTF-8 when nothing said otherwise. That is the only safe default:
    /// it is what the overwhelming majority of pages are, and a page that
    /// is actually Shift_JIS and says so nowhere is indistinguishable from
    /// one that is UTF-8 with a few damaged bytes.
    fn settle(&mut self, sink: &mut Sink<'_>) -> Result<(), Error> {
        let (encoding, skip) = detect(&self.held, self.markup).unwrap_or((Encoding::Utf8, 0));
        self.encoding = Some(encoding);
        let held = core::mem::take(&mut self.held);
        self.push_bytes(&held[skip.min(held.len())..], sink)
    }

    fn decode_shift_jis(&mut self, bytes: &[u8], sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut out = [0u8; OUT_BYTES];
        let mut used = 0usize;
        for &byte in bytes {
            // The tokenizer's "reconsume" in miniature: a byte that turned
            // out not to be a trail byte is very often the lead of the next
            // character, and throwing it away would lose a character for
            // every damaged one rather than one. This cannot spin -- the
            // second pass finds `self.lead` empty.
            let mut pending = Some(byte);
            while let Some(byte) = pending {
                pending = None;
                match self.lead.take() {
                    Some(lead) => match paired(lead, byte) {
                        Some(character) => push(&mut out, &mut used, character, sink)?,
                        None => {
                            push(&mut out, &mut used, REPLACEMENT, sink)?;
                            // Reconsidered only if it is ASCII, which is
                            // WHATWG's rule and the one that pays: a `>`
                            // or a `<` after a damaged character is markup
                            // that has to survive, while a second high
                            // byte is part of the same damage and putting
                            // it back would turn one replacement into two.
                            if byte.is_ascii() {
                                pending = Some(byte);
                            }
                        }
                    },
                    None => match single_shift_jis(byte) {
                        Some(character) => push(&mut out, &mut used, character, sink)?,
                        // A lead byte, whose trail may be in the next chunk.
                        None => self.lead = Some(byte),
                    },
                }
            }
        }
        if used > 0 {
            sink(&out[..used])?;
        }
        Ok(())
    }

    fn decode_euc_jp(&mut self, bytes: &[u8], sink: &mut Sink<'_>) -> Result<(), Error> {
        let mut out = [0u8; OUT_BYTES];
        let mut used = 0usize;
        for &byte in bytes {
            let mut pending = Some(byte);
            while let Some(byte) = pending {
                pending = None;
                if self.euc_plane2 {
                    self.euc_plane2 = false;
                    self.lead = None;
                    push(&mut out, &mut used, REPLACEMENT, sink)?;
                    if byte.is_ascii() {
                        pending = Some(byte);
                    }
                    continue;
                }
                match self.lead.take() {
                    Some(0x8E) => {
                        if let Some(character) = euc_halfwidth(byte) {
                            push(&mut out, &mut used, character, sink)?;
                        } else {
                            push(&mut out, &mut used, REPLACEMENT, sink)?;
                            if byte.is_ascii() {
                                pending = Some(byte);
                            }
                        }
                    }
                    Some(0x8F) => {
                        if (0xA1..=0xFE).contains(&byte) {
                            self.lead = Some(byte);
                            self.euc_plane2 = true;
                        } else {
                            push(&mut out, &mut used, REPLACEMENT, sink)?;
                            if byte.is_ascii() {
                                pending = Some(byte);
                            }
                        }
                    }
                    Some(lead) => match paired_euc_jp(lead, byte) {
                        Some(character) => push(&mut out, &mut used, character, sink)?,
                        None => {
                            push(&mut out, &mut used, REPLACEMENT, sink)?;
                            if byte.is_ascii() {
                                pending = Some(byte);
                            }
                        }
                    },
                    None => match byte {
                        0x00..=0x7F => push(&mut out, &mut used, byte as char, sink)?,
                        0x8E | 0x8F | 0xA1..=0xFE => self.lead = Some(byte),
                        _ => push(&mut out, &mut used, REPLACEMENT, sink)?,
                    },
                }
            }
        }
        if used > 0 {
            sink(&out[..used])?;
        }
        Ok(())
    }
}

/// Writes one character's UTF-8 into `out`, flushing first if it might not
/// fit.
fn push(
    out: &mut [u8; OUT_BYTES],
    used: &mut usize,
    character: char,
    sink: &mut Sink<'_>,
) -> Result<(), Error> {
    if *used + 4 > out.len() {
        sink(&out[..*used])?;
        *used = 0;
    }
    *used += character.encode_utf8(&mut out[*used..]).len();
    Ok(())
}

fn replacement_bytes() -> &'static [u8] {
    "\u{FFFD}".as_bytes()
}

/// What the start of a document says about its own encoding, and how many
/// bytes of it were the declaration rather than content.
fn detect(head: &[u8], markup: bool) -> Option<(Encoding, usize)> {
    // A byte order mark outranks everything, including a `<meta>` that
    // disagrees with it: it is not a claim about the document, it is three
    // bytes that only exist in one encoding.
    if head.starts_with(BOM) {
        return Some((Encoding::Utf8, BOM.len()));
    }
    if !markup {
        return None;
    }
    meta_charset(head).map(|encoding| (encoding, 0))
}

/// The UTF-8 byte order mark.
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Looks for a `charset` inside a `<meta>` element.
///
/// Bounded to the inside of the tag rather than searching the whole window
/// for the word, so that a page that happens to discuss character sets in
/// its first paragraph does not re-encode itself. Both spellings fall out
/// of the same scan:
///
/// ```text
/// <meta charset="Shift_JIS">
/// <meta http-equiv="Content-Type" content="text/html; charset=Shift_JIS">
/// ```
///
/// This is not HTML parsing and does not try to be. It cannot be: the
/// tokenizer that would do it properly is downstream of this decision, and
/// running it twice -- once to find the encoding and once to read the
/// document -- is the cost this exists to avoid.
fn meta_charset(head: &[u8]) -> Option<Encoding> {
    let mut index = 0;
    while index < head.len() {
        let start = index + find_ignore_case(&head[index..], b"<meta")?;
        let inside = start + 5;
        let end = match head[inside..].iter().position(|&byte| byte == b'>') {
            Some(offset) => inside + offset,
            // The tag has not finished arriving. Nothing later in the
            // window can be inside it either, so there is nothing more to
            // look at yet.
            None => return charset_in(&head[inside..]),
        };
        if let Some(encoding) = charset_in(&head[inside..end]) {
            return Some(encoding);
        }
        index = end + 1;
    }
    None
}

/// The encoding named by a `charset=` inside one tag's attributes.
fn charset_in(tag: &[u8]) -> Option<Encoding> {
    let mut index = find_ignore_case(tag, b"charset")? + 7;
    // `charset = "x"` and `charset="x"` and `; charset=x` all reach here.
    while index < tag.len() && (tag[index] == b' ' || tag[index] == b'\t') {
        index += 1;
    }
    if index >= tag.len() || tag[index] != b'=' {
        return None;
    }
    index += 1;
    while index < tag.len() && (tag[index] == b' ' || tag[index] == b'\t') {
        index += 1;
    }
    // `Some` only for an actual quote. Reading the first character of an
    // unquoted value as one is how `charset=shift_jis"` -- the tail of a
    // `content="text/html; charset=shift_jis"` attribute -- ended up
    // carrying the closing quote into the label and matching nothing.
    let quote = match tag.get(index) {
        Some(&byte @ (b'"' | b'\'')) => {
            index += 1;
            Some(byte)
        }
        _ => None,
    };
    let rest = &tag[index..];
    let end = rest
        .iter()
        .position(|&byte| match quote {
            Some(quote) => byte == quote,
            None => matches!(byte, b';' | b' ' | b'\t' | b'/' | b'>' | b'"' | b'\''),
        })
        .unwrap_or(rest.len());
    from_label(&rest[..end])
}

/// The offset of `needle` in `haystack`, ignoring ASCII case.
fn find_ignore_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|&start| haystack[start..start + needle.len()].eq_ignore_ascii_case(needle))
}

fn trim(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

// --- the table -----------------------------------------------------------

const DATA: &[u8] = include_bytes!("../data/shiftjis.bin");
const HEADER_BYTES: usize = 16;

const fn u16_at(offset: usize) -> u16 {
    (DATA[offset] as u16) | ((DATA[offset + 1] as u16) << 8)
}

/// JIS rows in the table. 120, because the highest lead byte reaches there
/// and the rows past 94 are the Windows extensions.
pub const ROWS: usize = u16_at(6) as usize;
/// Cells in a row, which is what JIS X 0208 fixes at 94.
pub const CELLS: usize = u16_at(8) as usize;

// A wrong or truncated data file is a build failure rather than a page of
// wrong characters. The generator and this file agree on the header or
// neither of them is right.
const _: () = {
    assert!(DATA.len() >= HEADER_BYTES);
    assert!(DATA[0] == b'T' && DATA[1] == b'5' && DATA[2] == b'S' && DATA[3] == b'1');
    assert!(u16_at(4) == 1, "unexpected shiftjis.bin format version");
    assert!(DATA.len() == HEADER_BYTES + ROWS * CELLS * 2);
};

/// The character a lead and trail byte pair stand for.
fn paired(lead: u8, trail: u8) -> Option<char> {
    let first = match lead {
        0x81..=0x9F => lead as usize - 0x81,
        0xE0..=0xFC => lead as usize - 0xC1,
        _ => return None,
    };
    let second = match trail {
        0x40..=0x7E => trail as usize - 0x40,
        0x80..=0xFC => trail as usize - 0x41,
        _ => return None,
    };
    // The two halves of a JIS row live in one lead byte, which is what the
    // `* 2` and the split at 94 are: the low trail bytes are the odd row,
    // the high ones the even row after it.
    let row = first * 2 + usize::from(second >= CELLS);
    let cell = second % CELLS;
    if row >= ROWS {
        return None;
    }
    let offset = HEADER_BYTES + (row * CELLS + cell) * 2;
    match u16::from_le_bytes([DATA[offset], DATA[offset + 1]]) {
        // Zero is the table's "no such character": nothing maps to U+0000.
        0 => None,
        scalar => char::from_u32(scalar as u32),
    }
}

/// The character a byte stands for on its own, or `None` if it is a lead
/// byte and the next one is needed.
fn single_shift_jis(byte: u8) -> Option<char> {
    match byte {
        // ASCII as itself, including 0x5C. Shift_JIS inherits JIS X 0201's
        // yen sign there, and pages are written as though it were a
        // backslash -- WHATWG maps it to U+005C for the same reason.
        0x00..=0x7F => Some(byte as char),
        // Halfwidth katakana, a fixed offset rather than a table: the block
        // is contiguous in Unicode and in Shift_JIS both.
        0xA1..=0xDF => char::from_u32(0xFF61 + (byte as u32 - 0xA1)),
        0x81..=0x9F | 0xE0..=0xFC => None,
        // 0x80, 0xA0, 0xFD..=0xFF: not characters and not leads.
        _ => Some(REPLACEMENT),
    }
}

/// The JIS X 0208 character selected by an EUC-JP pair. Plane 1 is exactly
/// the first 94 rows of the table used by Shift_JIS.
fn paired_euc_jp(lead: u8, trail: u8) -> Option<char> {
    let row = lead.checked_sub(0xA1)? as usize;
    let cell = trail.checked_sub(0xA1)? as usize;
    if row >= 94 || cell >= CELLS {
        return None;
    }
    let offset = HEADER_BYTES + (row * CELLS + cell) * 2;
    match u16::from_le_bytes([DATA[offset], DATA[offset + 1]]) {
        0 => None,
        scalar => char::from_u32(scalar as u32),
    }
}

fn euc_halfwidth(byte: u8) -> Option<char> {
    if (0xA1..=0xDF).contains(&byte) {
        char::from_u32(0xFF61 + (byte as u32 - 0xA1))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;

    /// Everything `bytes` decodes to, in one string.
    fn decode(bytes: &[u8], chunk: usize) -> String {
        let mut decoder = Decoder::new();
        let mut out = String::new();
        for piece in bytes.chunks(chunk.max(1)) {
            decoder
                .feed(piece, |decoded| {
                    out.push_str(&alloc::string::String::from_utf8_lossy(decoded));
                    Ok(())
                })
                .unwrap();
        }
        decoder
            .finish(|decoded| {
                out.push_str(&alloc::string::String::from_utf8_lossy(decoded));
                Ok(())
            })
            .unwrap();
        out
    }

    fn declared(label: &[u8], bytes: &[u8], chunk: usize) -> String {
        let mut decoder = Decoder::new();
        decoder.declare(label);
        let mut out = String::new();
        for piece in bytes.chunks(chunk.max(1)) {
            decoder
                .feed(piece, |decoded| {
                    out.push_str(&alloc::string::String::from_utf8_lossy(decoded));
                    Ok(())
                })
                .unwrap();
        }
        decoder
            .finish(|decoded| {
                out.push_str(&alloc::string::String::from_utf8_lossy(decoded));
                Ok(())
            })
            .unwrap();
        out
    }

    #[test]
    fn table_header_matches_the_generator() {
        assert_eq!(ROWS, 120);
        assert_eq!(CELLS, 94);
        assert_eq!(DATA.len(), HEADER_BYTES + ROWS * CELLS * 2);
    }

    #[test]
    fn known_cells() {
        // Row 1 cell 1, the ideographic space: the first entry in the file.
        assert_eq!(paired(0x81, 0x40), Some('\u{3000}'));
        assert_eq!(paired(0x82, 0xA0), Some('\u{3042}')); // HIRAGANA A
        assert_eq!(paired(0x88, 0x9F), Some('\u{4E9C}')); // the first kanji
        assert_eq!(paired(0x93, 0xFA), Some('\u{65E5}')); // 日
        assert_eq!(paired(0x96, 0x7B), Some('\u{672C}')); // 本
        // NEC row 13, which plain JIS X 0208 does not have.
        assert_eq!(paired(0x87, 0x40), Some('\u{2460}')); // circled one
        // The NEC-selected IBM extensions, past row 94: both of the name
        // characters the font carries explicitly for the same reason.
        assert_eq!(paired(0xED, 0x95), Some('\u{FA11}')); // Miyazaki's saki
        assert_eq!(paired(0xEE, 0xE0), Some('\u{9AD9}')); // Takahashi's taka
    }

    #[test]
    fn unassigned_cells_have_no_character() {
        // Rows 9 to 12 are empty in Windows-31J.
        assert_eq!(paired(0x84, 0xBF), None);
        assert_eq!(paired(0x00, 0x40), None);
        assert_eq!(paired(0x81, 0x3F), None);
    }

    #[test]
    fn ascii_passes_through() {
        assert_eq!(declared(b"shift_jis", b"<p>hello</p>", 4), "<p>hello</p>");
        // The one byte where Shift_JIS and ASCII are said to disagree.
        assert_eq!(declared(b"shift_jis", b"a\\b", 1), "a\\b");
    }

    #[test]
    fn halfwidth_katakana_is_arithmetic() {
        assert_eq!(
            declared(b"shift_jis", &[0xB1, 0xB2, 0xB3], 1),
            "\u{FF71}\u{FF72}\u{FF73}"
        );
    }

    #[test]
    fn every_chunk_boundary_gives_the_same_answer() {
        // Two-byte characters, halfwidth katakana and ASCII mixed, so that
        // a split lands inside a pair at several different places.
        let bytes = b"<p>\x93\xfa\x96\x7b\x8c\xea a\xb1\xb2 \x82\xa0\x82\xa2</p>";
        let whole = declared(b"shift_jis", bytes, bytes.len());
        assert_eq!(
            whole,
            "<p>\u{65E5}\u{672C}\u{8A9E} a\u{FF71}\u{FF72} \u{3042}\u{3044}</p>"
        );
        for chunk in 1..=bytes.len() {
            assert_eq!(declared(b"shift_jis", bytes, chunk), whole, "chunk {chunk}");
        }
    }

    #[test]
    fn euc_jp_decodes_jis_rows_and_halfwidth_katakana() {
        let bytes = b"<p>\xc6\xfc\xcb\xdc\xb8\xec \xa4\xa2\xa4\xa4 \x8e\xb1\x8e\xb2</p>";
        let whole = declared(b"euc-jp", bytes, bytes.len());
        assert_eq!(
            whole,
            "<p>\u{65E5}\u{672C}\u{8A9E} \u{3042}\u{3044} \u{FF71}\u{FF72}</p>"
        );
        for chunk in 1..=bytes.len() {
            assert_eq!(declared(b"euc-jp", bytes, chunk), whole, "chunk {chunk}");
        }
    }

    #[test]
    fn unsupported_euc_jp_plane_two_costs_one_character() {
        assert_eq!(declared(b"euc-jp", b"a\x8f\xa1\xa1b", 1), "a\u{FFFD}b");
        assert_eq!(declared(b"euc-jp", b"a\x8f\xa1", 1), "a\u{FFFD}");
    }

    #[test]
    fn damaged_euc_jp_preserves_following_ascii() {
        assert_eq!(declared(b"euc-jp", b"a\xa4 b", 1), "a\u{FFFD} b");
        assert_eq!(declared(b"euc-jp", b"ab\xa4", 1), "ab\u{FFFD}");
        assert_eq!(declared(b"euc-jp", b"a\x8e b", 1), "a\u{FFFD} b");
    }

    #[test]
    fn a_bad_trail_byte_costs_one_character() {
        // 0x93 is a lead; 0x20 is not a trail. The space survives, because
        // a byte that was not a trail is reconsidered on its own.
        assert_eq!(declared(b"shift_jis", b"a\x93 b", 1), "a\u{FFFD} b");
        // A lead byte at the very end of the document.
        assert_eq!(declared(b"shift_jis", b"ab\x93", 1), "ab\u{FFFD}");
        // A cell the table has nothing in.
        assert_eq!(declared(b"shift_jis", b"\x84\xbf", 1), "\u{FFFD}");
    }

    #[test]
    fn labels_that_mean_shift_jis() {
        for label in [
            &b"shift_jis"[..],
            b"Shift_JIS",
            b"SHIFT-JIS",
            b"sjis",
            b"x-sjis",
            b"windows-31j",
            b"cp932",
            b"ms_kanji",
            b"  shift_jis  ",
        ] {
            assert_eq!(from_label(label), Some(Encoding::ShiftJis), "{label:?}");
        }
        assert_eq!(from_label(b"utf-8"), Some(Encoding::Utf8));
        assert_eq!(from_label(b"UTF8"), Some(Encoding::Utf8));
        for label in [
            &b"euc-jp"[..],
            b"EUC_JP",
            b"eucjp",
            b"x-euc-jp",
            b"cseucpkdfmtjapanese",
            b"  euc-jp  ",
        ] {
            assert_eq!(from_label(label), Some(Encoding::EucJp), "{label:?}");
        }
        // Not known, and not an error: the caller falls back to UTF-8.
        assert_eq!(from_label(b"iso-2022-jp"), None);
        assert_eq!(from_label(b""), None);
    }

    #[test]
    fn a_header_settles_it_without_holding_anything() {
        let mut decoder = Decoder::new();
        decoder.declare(b"shift_jis");
        let mut seen = 0;
        decoder
            .feed(b"\x93\xfa", |decoded| {
                seen += decoded.len();
                Ok(())
            })
            .unwrap();
        // Three bytes of UTF-8 out for two in, immediately: nothing was
        // held waiting for a `<meta>` that the header made unnecessary.
        assert_eq!(seen, 3);
        assert_eq!(decoder.owned_bytes(), 0);
    }

    #[test]
    fn meta_charset_is_found() {
        let page = b"<html><head><meta charset=\"Shift_JIS\"><title>\x93\xfa</title>";
        assert!(decode(page, 7).ends_with("<title>\u{65E5}</title>"));
        let equiv =
            b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=shift_jis\">\x93\xfa";
        assert!(decode(equiv, 5).ends_with('\u{65E5}'));
        // Unquoted, which is legal and does occur.
        assert!(decode(b"<meta charset=shift_jis>\x93\xfa", 3).ends_with('\u{65E5}'));
        assert!(decode(b"<meta charset=euc-jp>\xc6\xfc", 3).ends_with('\u{65E5}'));
    }

    #[test]
    fn a_header_beats_a_meta_that_disagrees() {
        let page = b"<meta charset=\"utf-8\">\x93\xfa";
        assert!(declared(b"shift_jis", page, 4).ends_with('\u{65E5}'));
    }

    #[test]
    fn the_word_charset_in_the_text_is_not_a_declaration() {
        // Outside a tag, so it is prose about encodings, not one.
        let page = "<p>charset=shift_jis is what old pages say</p>".as_bytes();
        assert_eq!(decode(page, 6), core::str::from_utf8(page).unwrap());
    }

    #[test]
    fn nothing_declared_is_utf8() {
        let page = "<p>\u{65E5}\u{672C}</p>".as_bytes();
        assert_eq!(decode(page, 3), "<p>\u{65E5}\u{672C}</p>");
    }

    #[test]
    fn a_declaration_past_the_window_is_not_looked_for() {
        let mut page = vec![b' '; SNIFF_BYTES];
        page.extend_from_slice(b"<meta charset=\"shift_jis\">\x93\xfa");
        // The bytes are still delivered -- nothing is lost -- but as UTF-8,
        // so the two Shift_JIS bytes are not a character.
        let decoded = decode(&page, 64);
        assert!(decoded.contains("<meta charset=\"shift_jis\">"));
        assert!(!decoded.contains('\u{65E5}'));
    }

    #[test]
    fn a_byte_order_mark_is_utf8_and_is_eaten() {
        assert_eq!(decode(b"\xef\xbb\xbf<p>a</p>", 2), "<p>a</p>");
    }

    #[test]
    fn a_short_document_still_comes_out() {
        // Shorter than the sniff window, so it is only decoded at `finish`.
        assert_eq!(decode(b"<p>hi</p>", 3), "<p>hi</p>");
        assert_eq!(decode(b"", 1), "");
    }
}
