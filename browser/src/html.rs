//! An HTML tokenizer that takes the input in whatever pieces it arrives in.
//!
//! Two properties matter here and everything else is arranged around them.
//!
//! **The result does not depend on how the input was cut up.** Bytes come
//! off a socket in chunks nobody chose: a tag name, a character reference,
//! a multi-byte character or the `</script` that ends a raw-text element can
//! all be split across two reads. So this is a byte-at-a-time state machine
//! with every partial thing it is in the middle of held in its own fields,
//! and [`Tokenizer::feed`] can be called with one byte or with four
//! kilobytes to the same effect. The tests feed every fixture both ways and
//! compare.
//!
//! **The raw HTML is never kept.** Text runs are handed to the [`Sink`] and
//! forgotten; markup is consumed and forgotten; `script` and `style` bodies
//! are consumed and dropped without ever reaching the sink. What the
//! tokenizer holds at any moment is one pending text run, one tag, and one
//! character reference -- kilobytes, not megabytes.
//!
//! It does not build a tree. Real HTML is unbalanced, and a tokenizer that
//! insisted on nesting would spend its time recovering from documents that
//! are perfectly readable as a stream of "text here, heading starts,
//! heading ends". Structure is the [`crate::document`] builder's problem,
//! and it treats every tag as advice.
//!
//! UTF-8 is validated when a text run is flushed rather than as bytes
//! arrive: every delimiter this looks for is ASCII, so a multi-byte
//! sequence can only ever be split across an input chunk, and the pending
//! run is where the two halves meet anyway. Invalid sequences become
//! U+FFFD, which keeps the text on both sides of them.

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Error;
use crate::limits::{MAX_ATTRIBUTES_PER_ELEMENT, MAX_DECODED_HTML_BYTES, MAX_URL_BYTES};
use crate::memory;

/// Longest element name kept. Everything real is far shorter -- `blockquote`
/// is ten -- and a name past this cannot be one the document builder acts
/// on, so it is truncated and treated as an unknown element rather than
/// stored.
const MAX_TAG_NAME: usize = 32;

/// Longest attribute name kept, for the same reason.
const MAX_ATTRIBUTE_NAME: usize = 64;

/// Longest `alt` text kept. It stands in for an image on one line; past
/// this it would be a paragraph pretending to be a picture.
const MAX_ALT_BYTES: usize = 256;

/// Longest `href` kept, one byte past the URL bound so that an over-long
/// one is still recognisably over-long when the resolver sees it rather
/// than arriving silently trimmed to the limit.
const MAX_HREF_BYTES: usize = MAX_URL_BYTES + 1;

/// Longest character reference considered. `&thereexists;` is thirteen and
/// this does not implement named references beyond six of them anyway; the
/// bound exists so that an unterminated `&` cannot accumulate a document.
const MAX_ENTITY_BYTES: usize = 32;

/// How much pending text is handed over before a token boundary arrives.
///
/// Only a memory bound: the sink stitches consecutive runs back together,
/// so where the split falls changes nothing. Without it, a page that is one
/// enormous paragraph would hold the whole paragraph twice -- once as
/// pending bytes and once as decoded text.
const TEXT_FLUSH_THRESHOLD: usize = 4096;

/// One start tag, with the only two attribute values this keeps.
///
/// `href` and `alt` and nothing else. Every other attribute is parsed far
/// enough to be skipped and its value is never stored, which is what makes
/// an element carrying a kilobyte of `data-` attributes cost a scan rather
/// than a page's memory budget.
pub struct Tag<'a> {
    /// Lowercased.
    pub name: &'a str,
    pub href: Option<&'a str>,
    pub alt: Option<&'a str>,
    /// `<br/>`. Advisory: the document builder already knows which elements
    /// are empty, and uses this only for the ones it does not know.
    pub self_closing: bool,
}

/// What the tokenizer hands its output to.
///
/// Returning `Result` rather than nothing is the point: this is how a
/// document that has reached a limit stops the parse instead of being
/// truncated into something that looks complete.
pub trait Sink {
    /// A run of text. Consecutive runs belong to the same place in the
    /// document; the split between them carries no meaning.
    fn text(&mut self, text: &str) -> Result<(), Error>;
    fn start_tag(&mut self, tag: Tag<'_>) -> Result<(), Error>;
    /// Lowercased element name.
    fn end_tag(&mut self, name: &str) -> Result<(), Error>;
}

/// Where the state machine is between two bytes.
///
/// Named after the HTML tokenizer's own states where they correspond, so
/// that the specification can be read alongside this. The states this does
/// not have are the ones that only matter for building a tree.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Text,
    /// After `<`.
    TagOpen,
    /// After `</`.
    EndTagOpen,
    TagName,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    /// `quote` is `"` or `'`, or 0 for an unquoted value.
    AttributeValue {
        quote: u8,
    },
    AfterAttributeValue,
    SelfClosing,
    /// `<!` or `<?` followed by something that is not a comment: consumed
    /// to the next `>` and discarded.
    BogusComment,
    /// After `<!`, deciding between a comment and a doctype.
    MarkupDeclaration {
        dashes: u8,
    },
    Comment,
    /// One `-` seen inside a comment.
    CommentDash,
    /// Two `-` seen inside a comment.
    CommentDashDash,
    /// Inside `script` or `style`: everything is data until the matching
    /// end tag.
    RawText,
    /// A `<` inside raw text, which may or may not begin the end tag.
    RawTextLessThan,
    /// Matching the end tag's name, `matched` bytes in.
    RawTextEndTagName {
        matched: usize,
    },
    /// After a matched raw-text end tag name, looking for `>`.
    RawTextEndTagRest,
    /// A character reference, in text or in an attribute value. `attribute`
    /// says which buffer the result goes to and which state to return to.
    Entity {
        attribute: bool,
        quote: u8,
    },
}

/// The raw-text elements. Their content is not markup and is not displayed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RawKind {
    Script,
    Style,
}

impl RawKind {
    fn name(self) -> &'static [u8] {
        match self {
            RawKind::Script => b"script",
            RawKind::Style => b"style",
        }
    }
}

/// Which attribute value is currently being collected.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Capture {
    Nothing,
    Href,
    Alt,
}

/// The tag being read, and the two attribute values worth keeping.
struct TagBuffer {
    name: Vec<u8>,
    is_end: bool,
    self_closing: bool,
    attribute_name: Vec<u8>,
    capture: Capture,
    href: Option<Vec<u8>>,
    alt: Option<Vec<u8>>,
    attributes_seen: usize,
}

impl TagBuffer {
    fn new() -> Self {
        Self {
            name: Vec::new(),
            is_end: false,
            self_closing: false,
            attribute_name: Vec::new(),
            capture: Capture::Nothing,
            href: None,
            alt: None,
            attributes_seen: 0,
        }
    }

    fn reset(&mut self, is_end: bool) {
        self.name.clear();
        self.is_end = is_end;
        self.self_closing = false;
        self.attribute_name.clear();
        self.capture = Capture::Nothing;
        self.href = None;
        self.alt = None;
        self.attributes_seen = 0;
    }
}

pub struct Tokenizer {
    state: State,
    /// Whether this is reading a document that is not markup, in which case
    /// `state` never moves off `State::Text`. See [`Tokenizer::plain`].
    plain: bool,
    /// The state to return to once a character reference resolves.
    raw: Option<RawKind>,
    tag: TagBuffer,
    /// Pending text, as bytes, before UTF-8 repair.
    text: Vec<u8>,
    /// The pending text, repaired, handed to the sink and cleared.
    decoded: String,
    /// A character reference including its leading `&`.
    entity: Vec<u8>,
    /// `href` and `alt`, repaired, for the borrow the sink is handed.
    scratch_href: String,
    scratch_alt: String,
    scratch_name: String,
    /// Total input bytes, for [`MAX_DECODED_HTML_BYTES`].
    consumed: usize,
    /// The longest single tag name, attribute value or text run seen, which
    /// is the number worth knowing when a page behaves strangely.
    longest_token: usize,
}

impl Default for Tokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Tokenizer {
    pub fn new() -> Self {
        Self {
            state: State::Text,
            plain: false,
            raw: None,
            tag: TagBuffer::new(),
            text: Vec::new(),
            decoded: String::new(),
            entity: Vec::new(),
            scratch_href: String::new(),
            scratch_alt: String::new(),
            scratch_name: String::new(),
            consumed: 0,
            longest_token: 0,
        }
    }

    /// A tokenizer for a document that is not markup: every byte is text.
    ///
    /// `<` opens nothing and `&` starts nothing, so a `text/plain` file
    /// that happens to contain either shows them as itself. This is a flag
    /// on the tokenizer rather than a second path into the builder because
    /// of what the tokenizer does *besides* recognising tags: it holds an
    /// incomplete UTF-8 sequence across a chunk boundary, replaces invalid
    /// bytes with U+FFFD, counts the input against the size limit and
    /// flushes text to the sink in bounded pieces. All four are exactly as
    /// necessary for plain text, and none of them is about markup.
    pub fn plain() -> Self {
        Self {
            plain: true,
            ..Self::new()
        }
    }

    /// Input bytes seen so far.
    pub fn consumed(&self) -> usize {
        self.consumed
    }

    pub fn longest_token(&self) -> usize {
        self.longest_token
    }

    /// Sum of the capacities this holds, for the owned-memory budget.
    pub fn owned_bytes(&self) -> usize {
        self.tag.name.capacity()
            + self.tag.attribute_name.capacity()
            + self.tag.href.as_ref().map_or(0, |value| value.capacity())
            + self.tag.alt.as_ref().map_or(0, |value| value.capacity())
            + self.text.capacity()
            + self.decoded.capacity()
            + self.entity.capacity()
            + self.scratch_href.capacity()
            + self.scratch_alt.capacity()
            + self.scratch_name.capacity()
    }

    /// Feeds a chunk of input. Any size, including one byte.
    pub fn feed(&mut self, bytes: &[u8], sink: &mut dyn Sink) -> Result<(), Error> {
        for &byte in bytes {
            self.consumed += 1;
            if self.consumed > MAX_DECODED_HTML_BYTES {
                return Err(Error::InputTooLong);
            }
            // The specification's "reconsume in the new state": a byte that
            // ended one construct is often the first byte of the next, and
            // returning it is cheaper than every state having to know what
            // the previous one was. Every state that returns a byte also
            // changes state, so this cannot spin.
            let mut pending = Some(byte);
            while let Some(byte) = pending {
                pending = self.step(byte, sink)?;
            }
            if self.text.len() >= TEXT_FLUSH_THRESHOLD {
                self.flush_text(sink, false)?;
            }
        }
        Ok(())
    }

    /// Ends the input.
    ///
    /// Whatever was still pending is handed over as text: a document that
    /// stops in the middle of a tag has that tag's bytes as its last
    /// content, and dropping them would lose the end of a page whose only
    /// fault is that it was cut off.
    pub fn finish(&mut self, sink: &mut dyn Sink) -> Result<(), Error> {
        // An unterminated character reference is text, exactly as written.
        if !self.entity.is_empty() {
            let entity = core::mem::take(&mut self.entity);
            memory::extend_from_slice(&mut self.text, &entity)?;
            self.entity = entity;
            self.entity.clear();
        }
        self.state = State::Text;
        self.flush_text(sink, true)
    }

    /// One byte. Returns the byte to reconsider in the new state, if any.
    fn step(&mut self, byte: u8, sink: &mut dyn Sink) -> Result<Option<u8>, Error> {
        if self.plain {
            // No state machine at all: there is nothing in a plain text
            // document that means anything other than itself.
            memory::push(&mut self.text, byte)?;
            return Ok(None);
        }
        match self.state {
            State::Text => match byte {
                b'<' => {
                    self.state = State::TagOpen;
                }
                b'&' => {
                    self.entity.clear();
                    memory::push(&mut self.entity, b'&')?;
                    self.state = State::Entity {
                        attribute: false,
                        quote: 0,
                    };
                }
                _ => memory::push(&mut self.text, byte)?,
            },

            State::TagOpen => match byte {
                b'/' => self.state = State::EndTagOpen,
                b'!' => self.state = State::MarkupDeclaration { dashes: 0 },
                b'?' => self.state = State::BogusComment,
                byte if byte.is_ascii_alphabetic() => {
                    self.flush_text(sink, true)?;
                    self.tag.reset(false);
                    self.state = State::TagName;
                    return Ok(Some(byte));
                }
                _ => {
                    // `<` followed by anything else is not a tag. It is
                    // text, and so is the byte after it.
                    memory::push(&mut self.text, b'<')?;
                    self.state = State::Text;
                    return Ok(Some(byte));
                }
            },

            State::EndTagOpen => {
                if byte.is_ascii_alphabetic() {
                    self.flush_text(sink, true)?;
                    self.tag.reset(true);
                    self.state = State::TagName;
                    return Ok(Some(byte));
                }
                // `</>` and `</ ` are neither a tag nor worth keeping.
                self.state = State::BogusComment;
                return Ok(Some(byte));
            }

            State::TagName => match byte {
                b'>' => return self.emit_tag(sink).map(|()| None),
                b'/' => self.state = State::SelfClosing,
                byte if byte.is_ascii_whitespace() => self.state = State::BeforeAttributeName,
                byte => {
                    if self.tag.name.len() < MAX_TAG_NAME {
                        memory::push(&mut self.tag.name, byte.to_ascii_lowercase())?;
                        self.note_token(self.tag.name.len());
                    }
                }
            },

            State::BeforeAttributeName => match byte {
                b'>' => return self.emit_tag(sink).map(|()| None),
                b'/' => self.state = State::SelfClosing,
                byte if byte.is_ascii_whitespace() => {}
                byte => {
                    self.tag.attribute_name.clear();
                    self.tag.attributes_seen += 1;
                    self.state = State::AttributeName;
                    return Ok(Some(byte));
                }
            },

            State::AttributeName => match byte {
                b'>' => return self.emit_tag(sink).map(|()| None),
                b'/' => self.state = State::SelfClosing,
                b'=' => {
                    self.begin_capture();
                    self.state = State::BeforeAttributeValue;
                }
                byte if byte.is_ascii_whitespace() => self.state = State::AfterAttributeName,
                byte => {
                    if self.tag.attribute_name.len() < MAX_ATTRIBUTE_NAME {
                        memory::push(&mut self.tag.attribute_name, byte.to_ascii_lowercase())?;
                    }
                }
            },

            State::AfterAttributeName => match byte {
                b'>' => return self.emit_tag(sink).map(|()| None),
                b'/' => self.state = State::SelfClosing,
                b'=' => {
                    self.begin_capture();
                    self.state = State::BeforeAttributeValue;
                }
                byte if byte.is_ascii_whitespace() => {}
                byte => {
                    // A valueless attribute, then the next one starts.
                    self.tag.attribute_name.clear();
                    self.tag.attributes_seen += 1;
                    self.state = State::AttributeName;
                    return Ok(Some(byte));
                }
            },

            State::BeforeAttributeValue => match byte {
                b'>' => return self.emit_tag(sink).map(|()| None),
                byte if byte.is_ascii_whitespace() => {}
                quote @ (b'"' | b'\'') => self.state = State::AttributeValue { quote },
                byte => {
                    self.state = State::AttributeValue { quote: 0 };
                    return Ok(Some(byte));
                }
            },

            State::AttributeValue { quote } => match byte {
                byte if quote != 0 && byte == quote => self.state = State::AfterAttributeValue,
                b'>' if quote == 0 => return self.emit_tag(sink).map(|()| None),
                byte if quote == 0 && byte.is_ascii_whitespace() => {
                    self.state = State::BeforeAttributeName;
                }
                b'&' => {
                    self.entity.clear();
                    memory::push(&mut self.entity, b'&')?;
                    self.state = State::Entity {
                        attribute: true,
                        quote,
                    };
                }
                byte => self.push_attribute_byte(byte)?,
            },

            State::AfterAttributeValue => match byte {
                b'>' => return self.emit_tag(sink).map(|()| None),
                b'/' => self.state = State::SelfClosing,
                _ => {
                    self.state = State::BeforeAttributeName;
                    return Ok(Some(byte));
                }
            },

            State::SelfClosing => match byte {
                b'>' => {
                    self.tag.self_closing = true;
                    return self.emit_tag(sink).map(|()| None);
                }
                _ => {
                    self.state = State::BeforeAttributeName;
                    return Ok(Some(byte));
                }
            },

            State::MarkupDeclaration { dashes } => match byte {
                b'-' if dashes == 0 => self.state = State::MarkupDeclaration { dashes: 1 },
                // Two dashes seen: a comment. Entering at `CommentDashDash`
                // rather than at `Comment` is what makes `<!-->` and
                // `<!--->` close immediately, the way the specification's
                // "abrupt closing of empty comment" does -- a stray one in
                // a page would otherwise swallow the rest of the document.
                b'-' if dashes == 1 => self.state = State::CommentDashDash,
                _ => {
                    // A doctype, a CDATA section or something malformed.
                    // None of them contributes text.
                    self.state = State::BogusComment;
                    return Ok(Some(byte));
                }
            },

            State::Comment => {
                if byte == b'-' {
                    self.state = State::CommentDash;
                }
            }
            State::CommentDash => {
                self.state = if byte == b'-' {
                    State::CommentDashDash
                } else {
                    State::Comment
                };
            }
            State::CommentDashDash => match byte {
                b'>' => self.state = State::Text,
                // `--->` and longer dash runs still end the comment.
                b'-' => {}
                _ => self.state = State::Comment,
            },

            State::BogusComment => {
                if byte == b'>' {
                    self.state = State::Text;
                }
            }

            State::RawText => {
                if byte == b'<' {
                    self.state = State::RawTextLessThan;
                }
                // Everything else is discarded: `script` and `style` bodies
                // are not displayed, and keeping them would mean holding a
                // page's worth of JavaScript to throw away later.
            }
            State::RawTextLessThan => {
                if byte == b'/' {
                    self.state = State::RawTextEndTagName { matched: 0 };
                } else if byte == b'<' {
                    // Stay here: `<<` inside a script still has a `<` that
                    // might start the end tag.
                } else {
                    self.state = State::RawText;
                }
            }
            State::RawTextEndTagName { matched } => {
                let Some(kind) = self.raw else {
                    self.state = State::Text;
                    return Ok(Some(byte));
                };
                let expected = kind.name();
                if matched < expected.len() {
                    if byte.to_ascii_lowercase() == expected[matched] {
                        self.state = State::RawTextEndTagName {
                            matched: matched + 1,
                        };
                    } else {
                        // Not the end tag after all -- `</scriptnot`, or a
                        // `</` inside a string.
                        self.state = State::RawText;
                        return Ok(Some(byte));
                    }
                } else {
                    self.state = State::RawTextEndTagRest;
                    return Ok(Some(byte));
                }
            }
            State::RawTextEndTagRest => match byte {
                b'>' => {
                    let kind = self.raw.take();
                    self.state = State::Text;
                    if let Some(kind) = kind {
                        let name = core::str::from_utf8(kind.name()).unwrap_or("");
                        sink.end_tag(name)?;
                    }
                }
                byte if byte.is_ascii_whitespace() || byte == b'/' => {}
                _ => {
                    // `</scriptx` -- the name only matched a prefix.
                    self.state = State::RawText;
                    return Ok(Some(byte));
                }
            },

            State::Entity { attribute, quote } => {
                return self.step_entity(byte, attribute, quote);
            }
        }
        Ok(None)
    }

    /// One byte of a character reference.
    ///
    /// A reference that does not resolve is put back as the text it was
    /// written as, character for character. That is the rule the plan sets
    /// -- "未対応の参照は入力を失わない形でそのまま表示する" -- and it is
    /// also what makes `&` in prose safe: a bare ampersand followed by a
    /// word is simply an ampersand followed by a word.
    ///
    /// The terminating `;` is required, unlike in HTML proper, where a
    /// short list of named references also resolves without one so that
    /// twenty-year-old pages keep working. Requiring it costs a rendering
    /// difference on `&amp` written without a semicolon -- which is shown
    /// as typed rather than as `&` -- and buys a rule with no exceptions
    /// in it, which is worth more here than bug-compatibility with 1997.
    fn step_entity(&mut self, byte: u8, attribute: bool, quote: u8) -> Result<Option<u8>, Error> {
        let return_state = if attribute {
            State::AttributeValue { quote }
        } else {
            State::Text
        };
        if byte == b';' {
            let decoded = decode_entity(&self.entity[1..]);
            self.state = return_state;
            match decoded {
                Some(value) => {
                    self.entity.clear();
                    let mut buffer = [0u8; 4];
                    let encoded = value.encode_utf8(&mut buffer);
                    self.emit_entity_bytes(encoded.as_bytes(), attribute)?;
                }
                None => {
                    memory::push(&mut self.entity, b';')?;
                    let raw = core::mem::take(&mut self.entity);
                    self.emit_entity_bytes(&raw, attribute)?;
                    self.entity = raw;
                    self.entity.clear();
                }
            }
            return Ok(None);
        }
        let continues = self.entity.len() < MAX_ENTITY_BYTES
            && match self.entity.len() {
                // `&#`
                1 => byte.is_ascii_alphanumeric() || byte == b'#',
                // `&#x`
                2 if self.entity[1] == b'#' => byte.is_ascii_alphanumeric(),
                _ => byte.is_ascii_alphanumeric(),
            };
        if continues {
            memory::push(&mut self.entity, byte)?;
            return Ok(None);
        }
        // Not a reference. Everything collected is literal text, and this
        // byte has not been used yet.
        let raw = core::mem::take(&mut self.entity);
        self.emit_entity_bytes(&raw, attribute)?;
        self.entity = raw;
        self.entity.clear();
        self.state = return_state;
        Ok(Some(byte))
    }

    fn emit_entity_bytes(&mut self, bytes: &[u8], attribute: bool) -> Result<(), Error> {
        if attribute {
            for &byte in bytes {
                self.push_attribute_byte(byte)?;
            }
            Ok(())
        } else {
            memory::extend_from_slice(&mut self.text, bytes)?;
            Ok(())
        }
    }

    /// Decides whether the attribute now being read is one worth keeping.
    ///
    /// The decision is made here, at the `=`, rather than at the end of the
    /// value: an attribute this does not want never has its value stored at
    /// all, which is what bounds the cost of an element carrying a very
    /// large one.
    fn begin_capture(&mut self) {
        self.tag.capture = Capture::Nothing;
        if self.tag.attributes_seen > MAX_ATTRIBUTES_PER_ELEMENT {
            return;
        }
        if self.tag.attribute_name == b"href" && self.tag.href.is_none() {
            self.tag.href = Some(Vec::new());
            self.tag.capture = Capture::Href;
        } else if self.tag.attribute_name == b"alt" && self.tag.alt.is_none() {
            self.tag.alt = Some(Vec::new());
            self.tag.capture = Capture::Alt;
        }
    }

    fn push_attribute_byte(&mut self, byte: u8) -> Result<(), Error> {
        let (buffer, bound) = match self.tag.capture {
            Capture::Nothing => return Ok(()),
            Capture::Href => (&mut self.tag.href, MAX_HREF_BYTES),
            Capture::Alt => (&mut self.tag.alt, MAX_ALT_BYTES),
        };
        let Some(buffer) = buffer.as_mut() else {
            return Ok(());
        };
        if buffer.len() < bound {
            memory::push(buffer, byte)?;
            let length = buffer.len();
            self.note_token(length);
        }
        Ok(())
    }

    /// Hands the finished tag to the sink and returns to text.
    fn emit_tag(&mut self, sink: &mut dyn Sink) -> Result<(), Error> {
        self.state = State::Text;
        self.scratch_name.clear();
        push_repaired(&mut self.scratch_name, &self.tag.name)?;

        if self.tag.is_end {
            sink.end_tag(&self.scratch_name)?;
            return Ok(());
        }

        self.scratch_href.clear();
        if let Some(bytes) = &self.tag.href {
            push_repaired(&mut self.scratch_href, bytes)?;
        }
        self.scratch_alt.clear();
        if let Some(bytes) = &self.tag.alt {
            push_repaired(&mut self.scratch_alt, bytes)?;
        }
        sink.start_tag(Tag {
            name: &self.scratch_name,
            href: self.tag.href.as_ref().map(|_| self.scratch_href.as_str()),
            alt: self.tag.alt.as_ref().map(|_| self.scratch_alt.as_str()),
            self_closing: self.tag.self_closing,
        })?;

        // A raw-text element switches the whole machine over: from here to
        // its end tag, nothing is markup.
        self.raw = match self.tag.name.as_slice() {
            b"script" => Some(RawKind::Script),
            b"style" => Some(RawKind::Style),
            _ => None,
        };
        if self.raw.is_some() && !self.tag.self_closing {
            self.state = State::RawText;
        }
        Ok(())
    }

    /// Hands pending text over.
    ///
    /// `complete` says whether the run has definitely ended. When it has
    /// not, the cut is moved back off any UTF-8 sequence whose remaining
    /// bytes are still to come -- the one place where feeding the input in
    /// different sized pieces could otherwise change the result.
    fn flush_text(&mut self, sink: &mut dyn Sink, complete: bool) -> Result<(), Error> {
        if self.text.is_empty() {
            return Ok(());
        }
        let split = if complete {
            self.text.len()
        } else {
            safe_split(&self.text)
        };
        if split == 0 {
            return Ok(());
        }
        self.note_token(split);
        self.decoded.clear();
        push_repaired(&mut self.decoded, &self.text[..split])?;
        // Borrowed separately from `self.text`, which is what lets the sink
        // be called without copying the run a second time.
        sink.text(&self.decoded)?;
        self.text.drain(..split);
        Ok(())
    }

    fn note_token(&mut self, length: usize) {
        if length > self.longest_token {
            self.longest_token = length;
        }
    }
}

/// Where a byte buffer can be cut without splitting a UTF-8 sequence.
///
/// A sequence is at most four bytes, so the answer is within three bytes of
/// the end: walk back to the first byte that is not a continuation, and
/// keep it only if all of its sequence has arrived.
fn safe_split(bytes: &[u8]) -> usize {
    let limit = 3.min(bytes.len());
    for back in 1..=limit {
        let start = bytes.len() - back;
        let byte = bytes[start];
        if byte & 0b1100_0000 == 0b1000_0000 {
            continue;
        }
        return if sequence_length(byte) <= back {
            bytes.len()
        } else {
            start
        };
    }
    bytes.len()
}

/// How many bytes the sequence starting with `byte` has. An invalid lead
/// byte counts as one, so it is repaired rather than waited on.
fn sequence_length(byte: u8) -> usize {
    match byte {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 1,
    }
}

/// Appends `bytes` as text, turning anything that is not valid UTF-8 into
/// U+FFFD.
///
/// Replacement rather than refusal: an encoding error in one paragraph is
/// no reason to lose the rest of a page, and a page with a mojibake
/// character in it is still readable. The bytes on both sides survive,
/// which is what `/broken/bad-utf8.html` checks.
fn push_repaired(target: &mut String, bytes: &[u8]) -> Result<(), Error> {
    let mut rest = bytes;
    loop {
        match core::str::from_utf8(rest) {
            Ok(text) => {
                memory::push_str(target, text)?;
                return Ok(());
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid > 0 {
                    let text = core::str::from_utf8(&rest[..valid]).unwrap_or("");
                    memory::push_str(target, text)?;
                }
                memory::push_char(target, char::REPLACEMENT_CHARACTER)?;
                match error.error_len() {
                    Some(length) => rest = &rest[valid + length..],
                    // The input ends inside a sequence. There is no more
                    // coming -- `flush_text` only passes an incomplete tail
                    // when the document itself ended there.
                    None => return Ok(()),
                }
            }
        }
    }
}

/// The named references this knows, and numeric ones.
///
/// Six names, not the full table. The full table is about two and a half
/// thousand entries and several kilobytes of flash, and the ones that
/// actually change whether a page is readable are these: the four that
/// escape the markup delimiters, the apostrophe, and the non-breaking
/// space. Everything else falls through and is displayed as written, which
/// is worse-looking but not wrong.
fn decode_entity(body: &[u8]) -> Option<char> {
    if let Some(digits) = body.strip_prefix(b"#") {
        let value = if let Some(hex) = digits
            .strip_prefix(b"x")
            .or_else(|| digits.strip_prefix(b"X"))
        {
            parse_radix(hex, 16)?
        } else {
            parse_radix(digits, 10)?
        };
        // Surrogates and out-of-range values are not characters. They are
        // replaced rather than refused, so the reference still occupies
        // one position in the text.
        return Some(char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    match body {
        b"amp" => Some('&'),
        b"lt" => Some('<'),
        b"gt" => Some('>'),
        b"quot" => Some('"'),
        b"apos" => Some('\''),
        // A real U+00A0 rather than a space: whitespace collapsing has to
        // leave it alone, which is the whole reason a page uses it.
        b"nbsp" => Some('\u{00A0}'),
        _ => None,
    }
}

fn parse_radix(digits: &[u8], radix: u32) -> Option<u32> {
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    let mut value: u32 = 0;
    for &byte in digits {
        let digit = (byte as char).to_digit(radix)?;
        value = value.checked_mul(radix)?.checked_add(digit)?;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Records everything the tokenizer emits, as text, so a whole parse
    /// can be compared with one `assert_eq!`.
    #[derive(Default)]
    struct Recorder {
        events: Vec<String>,
    }

    impl Sink for Recorder {
        fn text(&mut self, text: &str) -> Result<(), Error> {
            // Consecutive text runs are joined: where the tokenizer chose
            // to split one is not part of the result.
            if let Some(last) = self.events.last_mut()
                && let Some(existing) = last.strip_prefix("text:")
            {
                let joined = format!("text:{existing}{text}");
                *last = joined;
                return Ok(());
            }
            self.events.push(format!("text:{text}"));
            Ok(())
        }

        fn start_tag(&mut self, tag: Tag<'_>) -> Result<(), Error> {
            let mut event = format!("<{}", tag.name);
            if let Some(href) = tag.href {
                event.push_str(&format!(" href={href}"));
            }
            if let Some(alt) = tag.alt {
                event.push_str(&format!(" alt={alt}"));
            }
            if tag.self_closing {
                event.push('/');
            }
            event.push('>');
            self.events.push(event);
            Ok(())
        }

        fn end_tag(&mut self, name: &str) -> Result<(), Error> {
            self.events.push(format!("</{name}>"));
            Ok(())
        }
    }

    /// Parses in one piece.
    fn parse(input: &[u8]) -> Vec<String> {
        let mut tokenizer = Tokenizer::new();
        let mut recorder = Recorder::default();
        tokenizer.feed(input, &mut recorder).unwrap();
        tokenizer.finish(&mut recorder).unwrap();
        recorder.events
    }

    /// Parses one byte at a time.
    fn parse_byte_by_byte(input: &[u8]) -> Vec<String> {
        let mut tokenizer = Tokenizer::new();
        let mut recorder = Recorder::default();
        for &byte in input {
            tokenizer.feed(&[byte], &mut recorder).unwrap();
        }
        tokenizer.finish(&mut recorder).unwrap();
        recorder.events
    }

    /// Parses in fixed-size pieces.
    fn parse_in_chunks(input: &[u8], size: usize) -> Vec<String> {
        let mut tokenizer = Tokenizer::new();
        let mut recorder = Recorder::default();
        for chunk in input.chunks(size) {
            tokenizer.feed(chunk, &mut recorder).unwrap();
        }
        tokenizer.finish(&mut recorder).unwrap();
        recorder.events
    }

    /// The property the whole design is for: chunking is invisible.
    ///
    /// Every size from one byte upward, so a boundary lands inside every
    /// construct in the input at some point.
    fn assert_chunking_invisible(input: &[u8]) {
        let expected = parse(input);
        assert_eq!(parse_byte_by_byte(input), expected, "one byte at a time");
        for size in 2..=input.len().min(64) {
            assert_eq!(parse_in_chunks(input, size), expected, "chunks of {size}");
        }
    }

    fn text_of(events: &[String]) -> String {
        let mut joined = String::new();
        for event in events {
            if let Some(text) = event.strip_prefix("text:") {
                joined.push_str(text);
            }
        }
        joined
    }

    // --- shape ------------------------------------------------------------

    #[test]
    fn tags_and_text_alternate() {
        assert_eq!(
            parse(b"<p>hello</p>"),
            [
                "<p>".to_string(),
                "text:hello".to_string(),
                "</p>".to_string()
            ]
        );
    }

    #[test]
    fn tag_names_are_lowercased() {
        assert_eq!(
            parse(b"<DIV></DIV>"),
            ["<div>".to_string(), "</div>".to_string()]
        );
    }

    #[test]
    fn only_href_and_alt_are_kept() {
        assert_eq!(
            parse(b"<a href=\"/x\" class=\"big\" id=q>t</a>"),
            [
                "<a href=/x>".to_string(),
                "text:t".to_string(),
                "</a>".to_string()
            ]
        );
        assert_eq!(
            parse(b"<img src=\"a.png\" alt=\"a cat\">"),
            ["<img alt=a cat>"]
        );
    }

    #[test]
    fn attribute_values_take_any_quoting() {
        for markup in [
            b"<a href=\"/x\">".as_slice(),
            b"<a href='/x'>".as_slice(),
            b"<a href=/x>".as_slice(),
            b"<a  href = \"/x\" >".as_slice(),
        ] {
            assert_eq!(parse(markup), ["<a href=/x>"], "{markup:?}");
        }
    }

    #[test]
    fn self_closing_is_reported() {
        assert_eq!(parse(b"<br/>"), ["<br/>"]);
        assert_eq!(parse(b"<br />"), ["<br/>"]);
        assert_eq!(parse(b"<br>"), ["<br>"]);
    }

    #[test]
    fn comments_and_doctypes_contribute_nothing() {
        assert_eq!(parse(b"<!DOCTYPE html><!-- hidden -->a"), ["text:a"]);
        assert_eq!(parse(b"<!-- a -- b --->x"), ["text:x"]);
        assert_eq!(parse(b"<!-->x"), ["text:x"]);
        assert_eq!(parse(b"<?php echo 1; ?>x"), ["text:x"]);
    }

    #[test]
    fn a_lone_less_than_is_text() {
        assert_eq!(text_of(&parse(b"a < b and c > d")), "a < b and c > d");
        assert_eq!(text_of(&parse(b"5<3")), "5<3");
    }

    // --- raw text ---------------------------------------------------------

    #[test]
    fn script_and_style_bodies_never_reach_the_sink() {
        let events = parse(
            b"<style>body > p { content: \"</style is not the end\"; }</style>\
              <p>after</p>\
              <script>if (a < b && c > d) { w(\"</scriptnot\"); }</script>\
              <p>end</p>",
        );
        let text = text_of(&events);
        assert!(!text.contains("content"), "{text}");
        assert!(!text.contains("scriptnot"), "{text}");
        // "afterend" and not "after end": the two words are in separate
        // paragraphs, and the tokenizer does not invent whitespace at a
        // block boundary. Turning that boundary into a break is the
        // document builder's job.
        assert_eq!(text, "afterend");
        assert!(events.contains(&"</style>".to_string()));
        assert!(events.contains(&"</script>".to_string()));
    }

    #[test]
    fn a_partial_end_tag_inside_a_script_does_not_end_it() {
        let events = parse(b"<script>x = \"</scr\" + \"ipt>\";</script>after");
        assert_eq!(text_of(&events), "after");
    }

    #[test]
    fn a_raw_text_end_tag_takes_whitespace_and_a_slash() {
        assert_eq!(text_of(&parse(b"<script>a</script  >b")), "b");
        assert_eq!(text_of(&parse(b"<style>a</style/>b")), "b");
    }

    // --- character references ---------------------------------------------

    #[test]
    fn the_six_named_references_resolve() {
        assert_eq!(
            text_of(&parse(b"&amp;&lt;&gt;&quot;&apos;&nbsp;")),
            "&<>\"'\u{00A0}"
        );
    }

    #[test]
    fn numeric_references_resolve_in_both_bases() {
        assert_eq!(text_of(&parse(b"&#65;&#x42;&#X43;")), "ABC");
        assert_eq!(text_of(&parse(b"&#128169;")), "\u{1F4A9}");
        assert_eq!(text_of(&parse(b"&#x1F600;")), "\u{1F600}");
    }

    #[test]
    fn an_unresolvable_reference_is_kept_as_written() {
        // Nothing is lost, which is the rule: the reader sees what the
        // author typed rather than a hole.
        assert_eq!(text_of(&parse(b"&notareference;")), "&notareference;");
        // No semicolon, so not a reference: shown exactly as typed.
        assert_eq!(text_of(&parse(b"&amp not-an-entity")), "&amp not-an-entity");
        assert_eq!(text_of(&parse(b"&#;")), "&#;");
        assert_eq!(text_of(&parse(b"&#x;")), "&#x;");
        assert_eq!(text_of(&parse(b"a & b")), "a & b");
        assert_eq!(text_of(&parse(b"100% & more")), "100% & more");
    }

    #[test]
    fn a_surrogate_or_out_of_range_reference_becomes_the_replacement() {
        assert_eq!(text_of(&parse(b"&#xD800;")), "\u{FFFD}");
        assert_eq!(text_of(&parse(b"&#x110000;")), "\u{FFFD}");
    }

    #[test]
    fn references_resolve_inside_attribute_values() {
        assert_eq!(
            parse(b"<a href=\"/t?a=1&amp;b=2\">x</a>"),
            [
                "<a href=/t?a=1&b=2>".to_string(),
                "text:x".to_string(),
                "</a>".to_string()
            ]
        );
    }

    #[test]
    fn an_unterminated_reference_at_the_end_is_text() {
        assert_eq!(text_of(&parse(b"tail &amp")), "tail &amp");
    }

    // --- UTF-8 ------------------------------------------------------------

    #[test]
    fn multi_byte_characters_survive() {
        assert_eq!(
            text_of(&parse("日本語 ひらがな \u{1F600}".as_bytes())),
            "日本語 ひらがな \u{1F600}"
        );
    }

    #[test]
    fn invalid_sequences_become_the_replacement_character() {
        let input = b"before \xff\xfe after";
        assert_eq!(text_of(&parse(input)), "before \u{FFFD}\u{FFFD} after");
    }

    #[test]
    fn text_on_both_sides_of_bad_bytes_survives() {
        let input = b"<p>a\x80b</p><p>\xe6\x97 c</p><p>\xe6\x97\xa5</p>";
        let text = text_of(&parse(input));
        assert!(text.contains('a'), "{text}");
        assert!(text.contains('b'), "{text}");
        assert!(text.contains('c'), "{text}");
        assert!(text.contains('日'), "{text}");
    }

    // --- limits -----------------------------------------------------------

    #[test]
    fn the_input_bound_is_enforced() {
        let mut tokenizer = Tokenizer::new();
        let mut recorder = Recorder::default();
        let block = vec![b'x'; 64 * 1024];
        let mut fed = 0usize;
        loop {
            match tokenizer.feed(&block, &mut recorder) {
                Ok(()) => fed += block.len(),
                Err(error) => {
                    assert_eq!(error, Error::InputTooLong);
                    assert!(fed <= MAX_DECODED_HTML_BYTES, "{fed}");
                    assert!(fed + block.len() > MAX_DECODED_HTML_BYTES);
                    return;
                }
            }
        }
    }

    #[test]
    fn an_element_with_hundreds_of_attributes_keeps_only_what_it_should() {
        let mut markup = String::from("<a");
        for index in 0..200 {
            markup.push_str(&format!(" data-{index}=\"{}\"", "v".repeat(64)));
        }
        markup.push_str(" href=\"/late\">t</a>");
        let events = parse(markup.as_bytes());
        // `href` came after the attribute bound, so its value is not kept --
        // but the element still parses and the text after it survives,
        // which is the property that matters.
        assert_eq!(events[0], "<a>");
        assert_eq!(text_of(&events), "t");
    }

    #[test]
    fn a_huge_attribute_value_does_not_become_text() {
        let markup = format!("<p title=\"{}\">after</p>", "t".repeat(100_000));
        assert_eq!(text_of(&parse(markup.as_bytes())), "after");
    }

    #[test]
    fn an_over_long_href_is_kept_one_byte_past_the_url_bound() {
        let href = "/".to_string() + &"p".repeat(MAX_URL_BYTES * 2);
        let markup = format!("<a href=\"{href}\">t</a>");
        let events = parse(markup.as_bytes());
        let kept = events[0]
            .strip_prefix("<a href=")
            .unwrap()
            .trim_end_matches('>');
        assert_eq!(kept.len(), MAX_HREF_BYTES);
    }

    #[test]
    fn a_very_long_tag_name_is_truncated_rather_than_stored() {
        let markup = format!("<{}>x", "n".repeat(1000));
        let events = parse(markup.as_bytes());
        assert_eq!(events[0].len(), MAX_TAG_NAME + 2);
        assert_eq!(text_of(&events), "x");
    }

    // --- malformed markup -------------------------------------------------

    #[test]
    fn unterminated_markup_still_yields_its_text() {
        let input = b"<h1>Unterminated\n<p>A paragraph that is never closed.\n\
                      <ul><li>one<li>two\n<p>and an <a href=\"/x\">unclosed link\n";
        let text = text_of(&parse(input));
        assert!(text.contains("Unterminated"), "{text}");
        assert!(text.contains("never closed"), "{text}");
        assert!(text.contains("unclosed link"), "{text}");
    }

    #[test]
    fn a_document_ending_inside_a_tag_loses_nothing_before_it() {
        let events = parse(b"<p>text</p><div class=\"unfinis");
        assert_eq!(text_of(&events), "text");
    }

    #[test]
    fn a_document_ending_inside_text_flushes_it() {
        assert_eq!(text_of(&parse(b"<p>no closing tag")), "no closing tag");
    }

    // --- chunking ---------------------------------------------------------

    #[test]
    fn chunking_is_invisible_for_ordinary_markup() {
        assert_chunking_invisible(
            b"<html><head><title>t</title></head><body>\
              <h1>Heading</h1><p>Some <strong>bold</strong> text.</p>\
              <p><a href=\"/links/target.html?a=1&amp;b=2\">a link</a></p>\
              </body></html>",
        );
    }

    #[test]
    fn chunking_is_invisible_across_entities_and_utf8() {
        assert_chunking_invisible(
            "<p>&amp; &#128169; &#x1F600; 日本語 &nbsp;end &notareference;</p>".as_bytes(),
        );
    }

    #[test]
    fn chunking_is_invisible_across_raw_text_ends() {
        assert_chunking_invisible(
            b"<script>var s = \"</scr\" + \"ipt>\"; if (a<b) {}</script><p>after</p>",
        );
    }

    #[test]
    fn chunking_is_invisible_across_comments() {
        assert_chunking_invisible(b"a<!-- comment -- with -- dashes --->b<!DOCTYPE x>c");
    }

    #[test]
    fn chunking_is_invisible_for_broken_utf8() {
        assert_chunking_invisible(b"<p>before \xff\xfe after</p><p>\xe6\x97\xa5\xe6\x9c\xac</p>");
    }

    #[test]
    fn chunking_is_invisible_across_a_long_text_run() {
        // Longer than TEXT_FLUSH_THRESHOLD, so the soft flush fires and has
        // to fall on a character boundary.
        let long = "日本語".repeat(3000);
        let markup = format!("<p>{long}</p>");
        let expected = text_of(&parse(markup.as_bytes()));
        assert_eq!(expected, long);
        assert_eq!(text_of(&parse_in_chunks(markup.as_bytes(), 7)), long);
        assert_eq!(text_of(&parse_byte_by_byte(markup.as_bytes())), long);
    }

    // --- bookkeeping ------------------------------------------------------

    #[test]
    fn the_tokenizer_holds_only_kilobytes() {
        let markup = format!("<p>{}</p>", "word ".repeat(100_000));
        let mut tokenizer = Tokenizer::new();
        let mut recorder = Recorder::default();
        let mut peak = 0usize;
        for chunk in markup.as_bytes().chunks(512) {
            tokenizer.feed(chunk, &mut recorder).unwrap();
            peak = peak.max(tokenizer.owned_bytes());
        }
        tokenizer.finish(&mut recorder).unwrap();
        // The raw HTML is half a megabyte; what the tokenizer keeps is the
        // pending run and its decoded copy.
        assert!(peak < 64 * 1024, "{peak}");
    }
}
