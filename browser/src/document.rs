//! The document a page becomes: text, the blocks it is divided into, the
//! styled runs inside them, and the links.
//!
//! This is deliberately not a DOM. A DOM is a tree of nodes with parents,
//! children and attributes, and it exists so that scripts can change the
//! page and stylesheets can select parts of it. Neither happens here, so
//! what is left is what a reader actually needs: where one paragraph ends
//! and the next begins, which words are a heading, which words are a link
//! and where it goes.
//!
//! Everything is stored flat and indexed:
//!
//! ```text
//! text    "Simple This page has a title...to a target page."
//!          ^-----^ ^-------------------^  ^---------------^
//! runs     run 0    run 1                  run 2 (link 0)
//! blocks  [heading: runs 0..1] [paragraph: runs 1..2] ...
//! links   [0] http://example.com/links/target.html
//! ```
//!
//! One `String` holds every character on the page, once. A run is two
//! offsets into it plus a byte of style and a link index -- twelve bytes,
//! not a node. That is what keeps a megabyte of prose costing a megabyte
//! and change rather than the ten or twenty megabytes a node per element
//! would, on a board whose whole heap is twenty-two.
//!
//! Structure comes from a stream of tags that is assumed to be wrong.
//! Unclosed paragraphs, `<li>` without `</li>`, `</div>` with nothing open,
//! headings inside links: all of it is normal, none of it is an error, and
//! the rule throughout is that a tag is advice about where a boundary goes
//! rather than a promise about nesting. What is *not* tolerated is a page
//! going past a limit -- that stops the parse, because a document silently
//! cut short is indistinguishable from a document that ended.

use alloc::string::String;
use alloc::vec::Vec;

use crate::encoding::Decoder;
use crate::error::Error;
use crate::html::{self, Tag, Tokenizer};
use crate::limits::{MAX_ITEMS, MAX_LINKS, MAX_NESTING_DEPTH, MAX_TEXT_BYTES};
use crate::memory;
use crate::url::Url;

/// The largest step the page's text buffer grows by.
///
/// Left to itself a `String` doubles, so a page with a megabyte of text
/// would hold two megabytes -- and the second one is slack, counted against
/// the four-megabyte budget for as long as the page is displayed. Capping
/// the step bounds the waste at this much regardless of page size, at the
/// cost of a handful of extra copies on the largest pages.
const TEXT_GROWTH_CAP: usize = 128 * 1024;

/// Style bits carried by a run. Deliberately few: the font has one weight
/// and one shape, so these become colour, or a second strike, and nothing
/// else.
pub const STYLE_BOLD: u8 = 1 << 0;
pub const STYLE_ITALIC: u8 = 1 << 1;
pub const STYLE_CODE: u8 = 1 << 2;

/// What a block is, which is everything the layout needs to know about it
/// besides its runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockKind {
    Paragraph,
    /// `h1` through `h6`.
    Heading(u8),
    ListItem {
        /// Nesting depth, from zero. Bounded by
        /// [`MAX_NESTING_DEPTH`]; deeper lists keep their text and stop
        /// indenting.
        depth: u8,
        marker: Marker,
    },
    /// `pre`: whitespace as written.
    Preformatted,
    /// `hr`. Has no runs.
    Rule,
}

/// What stands at the front of a list item.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Marker {
    Bullet,
    Number(u32),
}

/// A stretch of text with one style and at most one link.
///
/// Runs are contiguous and in document order, so a block's runs are a
/// slice, and the text of the whole page is the concatenation of every
/// run's range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    /// Byte offsets into [`Document::text`].
    pub start: u32,
    pub end: u32,
    pub style: u8,
    /// Index into [`Document::links`].
    pub link: Option<u16>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Block {
    pub kind: BlockKind,
    /// Index into [`Document::runs`].
    pub first_run: u32,
    pub run_count: u32,
}

/// A link target, already resolved against the page's own URL.
///
/// Resolved at parse time rather than at click time so that a page which
/// has been navigated away from cannot leave a relative link behind to be
/// resolved against the wrong base -- and so that a link this cannot use
/// is dropped while the reason is still known.
pub struct Link {
    pub url: Url,
}

/// What one page cost, for the UART line and the memory budget.
#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    /// HTML bytes fed in.
    pub input_bytes: usize,
    pub text_bytes: usize,
    /// Blocks plus runs, which together are what [`MAX_ITEMS`] bounds.
    pub items: usize,
    pub links: usize,
    /// Sum of the capacities the document holds.
    pub owned_bytes: usize,
    /// Longest tag name, attribute value or text run the tokenizer saw.
    pub longest_token: usize,
}

pub struct Document {
    url: Url,
    title: String,
    text: String,
    runs: Vec<Run>,
    blocks: Vec<Block>,
    links: Vec<Link>,
    stats: Stats,
}

impl Document {
    /// The page's own address, which is also the base its links resolved
    /// against.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The `title` element's text, or empty. Never part of [`Document::text`].
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    pub fn links(&self) -> &[Link] {
        &self.links
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The runs belonging to `block`.
    pub fn block_runs(&self, block: &Block) -> &[Run] {
        let start = block.first_run as usize;
        let end = start + block.run_count as usize;
        self.runs.get(start..end).unwrap_or(&[])
    }

    /// A run's text. Empty rather than panicking if the offsets are not a
    /// character boundary, which they always are -- runs only ever start
    /// and end where text was appended.
    pub fn run_text(&self, run: &Run) -> &str {
        self.text
            .get(run.start as usize..run.end as usize)
            .unwrap_or("")
    }

    pub fn link(&self, index: u16) -> Option<&Link> {
        self.links.get(index as usize)
    }

    fn owned_bytes(&self) -> usize {
        let links: usize = self.links.iter().map(|link| link.url.owned_bytes()).sum();
        self.title.capacity()
            + self.text.capacity()
            + self.runs.capacity() * core::mem::size_of::<Run>()
            + self.blocks.capacity() * core::mem::size_of::<Block>()
            + self.links.capacity() * core::mem::size_of::<Link>()
            + links
    }
}

/// A tokenizer and a document builder wired together.
///
/// This is what the browser drives: hand it whatever bytes arrived, and ask
/// for the document when the body ends. It never publishes a partial
/// document -- [`Parser::finish`] is the only way to get one, and a limit
/// reached on the way there is an error rather than a shorter page.
pub struct Parser {
    /// What the page's bytes are turned into UTF-8 by, before the tokenizer
    /// ever sees them.
    ///
    /// In here and not in front of the parser so that every caller gets it:
    /// the firmware's fetch, the `hs` and `bt` diagnostics and this crate's
    /// fixture tests all feed a `Parser`, and a transcoder attached to one
    /// of them would be missing from the other three.
    decoder: Decoder,
    tokenizer: Tokenizer,
    builder: Builder,
}

impl Parser {
    /// `url` is the page's own address: the base for every relative link
    /// and what the document reports as its own.
    pub fn new(url: Url) -> Result<Parser, Error> {
        Ok(Parser {
            decoder: Decoder::new(),
            tokenizer: Tokenizer::new(),
            builder: Builder::new(url)?,
        })
    }

    /// A parser for a document that is text rather than markup.
    ///
    /// `text/plain` and everything else under `text/` that is not HTML.
    /// The whole file becomes one preformatted block, so its own spacing
    /// and line breaks survive and nothing in it is read as a tag or a
    /// character reference. Wrapping still happens at the screen's width,
    /// because a line longer than the screen has to go somewhere.
    ///
    /// The encoding is settled exactly as it is for markup -- the header's
    /// `charset` through [`Parser::declare_charset`], then a BOM -- except
    /// that there is no `<meta>` to look for, and none is looked for: a
    /// plain file that contains the characters `<meta charset=...>` is a
    /// file that says so, not a file that means it.
    pub fn plain(url: Url) -> Result<Parser, Error> {
        let mut parser = Parser {
            decoder: Decoder::plain(),
            tokenizer: Tokenizer::plain(),
            builder: Builder::new(url)?,
        };
        parser.builder.begin_preformatted()?;
        Ok(parser)
    }

    /// Names the encoding from a `Content-Type` header's `charset`.
    ///
    /// Before the first [`Parser::feed`]: a header applies to the whole
    /// body, and this is what stops the decoder holding the first kilobyte
    /// back to look for a `<meta>` that cannot overrule it anyway. An
    /// unrecognised label is ignored, which leaves the document to say.
    pub fn declare_charset(&mut self, label: &[u8]) {
        self.decoder.declare(label);
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let tokenizer = &mut self.tokenizer;
        let builder = &mut self.builder;
        self.decoder
            .feed(bytes, |decoded| tokenizer.feed(decoded, builder))
    }

    pub fn finish(mut self) -> Result<Document, Error> {
        {
            // Whatever the decoder was still holding -- a document shorter
            // than the sniff window has never released a byte until now.
            let tokenizer = &mut self.tokenizer;
            let builder = &mut self.builder;
            self.decoder
                .finish(|decoded| tokenizer.feed(decoded, builder))?;
        }
        self.tokenizer.finish(&mut self.builder)?;
        let longest = self.tokenizer.longest_token();
        let input = self.tokenizer.consumed();
        Ok(self.builder.finish(input, longest))
    }

    /// What the parse owns right now: the decoder's held bytes, the
    /// tokenizer and the document together.
    pub fn owned_bytes(&self) -> usize {
        self.decoder.owned_bytes()
            + self.tokenizer.owned_bytes()
            + self.builder.document.owned_bytes()
    }

    /// Blocks and runs so far, for a progress line during a long parse.
    pub fn items(&self) -> usize {
        self.builder.items()
    }
}

/// One entry on the inline stack: an element that turned a style or a link
/// on and has to turn it off again.
struct InlineFrame {
    /// The element name, so an end tag can find its own frame rather than
    /// closing whatever happens to be open.
    name: [u8; 8],
    length: u8,
    /// Style bits this frame added, to be removed when it closes.
    added: u8,
    /// The link that was current before this frame, restored when it
    /// closes. Nested `a` elements are not legal but do occur.
    previous_link: Option<u16>,
    sets_link: bool,
}

struct ListLevel {
    ordered: bool,
    next_number: u32,
}

/// Turns a tag stream into a [`Document`].
struct Builder {
    document: Document,
    /// The block being filled, if any. Blocks are opened lazily: a `<div>`
    /// with nothing in it never becomes one.
    open_kind: Option<BlockKind>,
    open_first_run: u32,
    /// The run being extended.
    run_open: bool,
    run_start: u32,
    style: u8,
    link: Option<u16>,
    inline: Vec<InlineFrame>,
    lists: Vec<ListLevel>,
    /// Inside `pre`: whitespace is kept exactly.
    preformatted: bool,
    /// Inside `title`: text goes to the title, not the body.
    in_title: bool,
    /// Whitespace has been seen and a single space owes the next word --
    /// unless a block boundary intervenes, which cancels it.
    pending_space: bool,
    /// Nothing has been written to the current block yet, so leading
    /// whitespace is dropped.
    at_block_start: bool,
}

impl Builder {
    fn new(url: Url) -> Result<Builder, Error> {
        Ok(Builder {
            document: Document {
                url,
                title: String::new(),
                text: String::new(),
                runs: Vec::new(),
                blocks: Vec::new(),
                links: Vec::new(),
                stats: Stats::default(),
            },
            open_kind: None,
            open_first_run: 0,
            run_open: false,
            run_start: 0,
            style: 0,
            link: None,
            inline: Vec::new(),
            lists: Vec::new(),
            preformatted: false,
            in_title: false,
            pending_space: false,
            at_block_start: true,
        })
    }

    fn items(&self) -> usize {
        self.document.blocks.len() + self.document.runs.len()
    }

    fn finish(mut self, input_bytes: usize, longest_token: usize) -> Document {
        // The last block is committed the same way every other one is; a
        // page that ends mid-paragraph is not a special case.
        let _ = self.close_block();
        self.document.stats = Stats {
            input_bytes,
            text_bytes: self.document.text.len(),
            items: self.items(),
            links: self.document.links.len(),
            owned_bytes: self.document.owned_bytes(),
            longest_token,
        };
        self.document
    }

    // --- text ---------------------------------------------------------

    /// Appends display text, collapsing whitespace outside `pre`.
    ///
    /// Collapsing happens here rather than in the tokenizer because a text
    /// run can be handed over in several pieces, and the state that decides
    /// whether a space is owed has to survive the split. That is the whole
    /// reason [`crate::html::Sink::text`] promises nothing about where runs
    /// are cut.
    /// Opens a preformatted block and leaves it open.
    ///
    /// For a document that is text rather than markup: `pre` is exactly
    /// what `text/plain` means -- spaces and line breaks are the author's
    /// and are kept -- so a plain file is one `pre` block with the whole
    /// file in it, rather than a block kind of its own that the layout and
    /// the renderer would each need a case for.
    fn begin_preformatted(&mut self) -> Result<(), Error> {
        self.start_block(BlockKind::Preformatted)?;
        self.preformatted = true;
        Ok(())
    }

    fn push_text(&mut self, text: &str) -> Result<(), Error> {
        for character in text.chars() {
            if self.preformatted {
                self.push_preformatted(character)?;
                continue;
            }
            // U+00A0 is not whitespace for this purpose: a page uses it
            // precisely to stop a break happening there.
            if character.is_ascii_whitespace() {
                self.pending_space = true;
                continue;
            }
            self.flush_pending_space()?;
            self.push_character(character)?;
        }
        Ok(())
    }

    /// Writes the single space a run of whitespace collapsed to, if one is
    /// owed and there is something for it to follow.
    ///
    /// Called before anything that changes where the next character lands
    /// -- a style change, a link, an image -- and not only before the next
    /// word. Without that, `plain <strong>bold` puts the space at the front
    /// of the bold run instead of the end of the plain one, which is a
    /// space in the right place on screen and in the wrong run for
    /// everything that indexes by run: hit testing, link highlighting and
    /// the layout's own idea of where a word begins.
    fn flush_pending_space(&mut self) -> Result<(), Error> {
        if !self.pending_space {
            return Ok(());
        }
        self.pending_space = false;
        if !self.at_block_start {
            self.push_character(' ')?;
        }
        Ok(())
    }

    fn push_preformatted(&mut self, character: char) -> Result<(), Error> {
        match character {
            // CRLF becomes one break; a lone CR would otherwise show as a
            // blank line the source does not have.
            '\r' => Ok(()),
            // A tab is one space. Expanding to a tab stop would need a
            // column count, and there is no column: characters are 8 pixels
            // wide or 16, so a `pre` block that relies on tab stops has
            // nothing to align to here.
            '\t' => self.push_character(' '),
            _ => self.push_character(character),
        }
    }

    fn push_character(&mut self, character: char) -> Result<(), Error> {
        if self.in_title {
            // The title is not part of the body, so it opens no block and
            // no run -- but it collapses whitespace the same way, which is
            // what `at_block_start` is tracking here.
            //
            // Bounded by the same text limit: a `title` is one line, and a
            // page that puts a megabyte in one is not one to indulge.
            if self.document.title.len() + character.len_utf8() > MAX_TEXT_BYTES {
                return Err(Error::TextTooLong);
            }
            memory::push_char(&mut self.document.title, character)?;
            self.at_block_start = false;
            return Ok(());
        }
        self.open_block_if_needed()?;
        if !self.run_open {
            self.run_start = self.document.text.len() as u32;
            self.run_open = true;
        }
        if self.document.text.len() + character.len_utf8() > MAX_TEXT_BYTES {
            return Err(Error::TextTooLong);
        }
        memory::reserve_capped(
            &mut self.document.text,
            character.len_utf8(),
            TEXT_GROWTH_CAP,
        )?;
        memory::push_char(&mut self.document.text, character)?;
        self.at_block_start = false;
        Ok(())
    }

    /// A hard break inside the current block: `<br>`, or a newline in `pre`.
    fn push_break(&mut self) -> Result<(), Error> {
        if self.in_title {
            return Ok(());
        }
        self.open_block_if_needed()?;
        self.pending_space = false;
        if !self.run_open {
            self.run_start = self.document.text.len() as u32;
            self.run_open = true;
        }
        if self.document.text.len() + 1 > MAX_TEXT_BYTES {
            return Err(Error::TextTooLong);
        }
        memory::push_char(&mut self.document.text, '\n')?;
        // Not `at_block_start = false`: a break followed by whitespace
        // should not produce a leading space on the new line.
        self.at_block_start = true;
        Ok(())
    }

    // --- runs and blocks ----------------------------------------------

    fn open_block_if_needed(&mut self) -> Result<(), Error> {
        if self.open_kind.is_some() || self.in_title {
            return Ok(());
        }
        self.open_kind = Some(BlockKind::Paragraph);
        self.open_first_run = self.document.runs.len() as u32;
        self.at_block_start = true;
        Ok(())
    }

    /// Ends the run being extended, if it covers any text.
    fn close_run(&mut self) -> Result<(), Error> {
        if !self.run_open {
            return Ok(());
        }
        self.run_open = false;
        let end = self.document.text.len() as u32;
        if end <= self.run_start {
            return Ok(());
        }
        self.check_items(1)?;
        memory::push(
            &mut self.document.runs,
            Run {
                start: self.run_start,
                end,
                style: self.style,
                link: self.link,
            },
        )?;
        Ok(())
    }

    /// Commits the open block. A block with no runs is dropped: an empty
    /// `<div>` is not a blank line, it is nothing.
    fn close_block(&mut self) -> Result<(), Error> {
        self.close_run()?;
        let Some(kind) = self.open_kind.take() else {
            return Ok(());
        };
        let first_run = self.open_first_run;
        let run_count = self.document.runs.len() as u32 - first_run;
        if run_count == 0 && kind != BlockKind::Rule {
            self.at_block_start = true;
            self.pending_space = false;
            return Ok(());
        }
        self.check_items(1)?;
        memory::push(
            &mut self.document.blocks,
            Block {
                kind,
                first_run,
                run_count,
            },
        )?;
        self.at_block_start = true;
        self.pending_space = false;
        Ok(())
    }

    /// Closes whatever is open and opens a block of `kind`.
    fn start_block(&mut self, kind: BlockKind) -> Result<(), Error> {
        self.close_block()?;
        self.open_kind = Some(kind);
        self.open_first_run = self.document.runs.len() as u32;
        self.at_block_start = true;
        self.pending_space = false;
        Ok(())
    }

    fn check_items(&self, adding: usize) -> Result<(), Error> {
        if self.items() + adding > MAX_ITEMS {
            return Err(Error::TooManyItems);
        }
        Ok(())
    }

    /// Changes the style or link, ending the run first so the change takes
    /// effect at exactly this point in the text.
    fn set_style(&mut self, style: u8, link: Option<u16>) -> Result<(), Error> {
        if self.style == style && self.link == link {
            return Ok(());
        }
        self.flush_pending_space()?;
        self.close_run()?;
        self.style = style;
        self.link = link;
        Ok(())
    }

    // --- elements -----------------------------------------------------

    fn start_element(&mut self, tag: Tag<'_>) -> Result<(), Error> {
        match tag.name {
            "title" => {
                self.close_block()?;
                self.in_title = true;
                self.at_block_start = true;
                self.pending_space = false;
            }
            "br" => self.push_break()?,
            "hr" => {
                self.start_block(BlockKind::Rule)?;
                self.close_block()?;
            }
            "img" => self.push_image(tag.alt)?,
            "pre" => {
                self.start_block(BlockKind::Preformatted)?;
                self.preformatted = true;
            }
            "ul" | "ol" => {
                self.close_block()?;
                if self.lists.len() < MAX_NESTING_DEPTH {
                    memory::push(
                        &mut self.lists,
                        ListLevel {
                            ordered: tag.name == "ol",
                            next_number: 1,
                        },
                    )?;
                }
            }
            "li" => {
                let (depth, marker) = self.list_marker();
                self.start_block(BlockKind::ListItem { depth, marker })?;
            }
            "a" => self.start_link(tag.href)?,
            name => {
                if let Some(level) = heading_level(name) {
                    self.start_block(BlockKind::Heading(level))?;
                } else if let Some(bit) = inline_style(name) {
                    self.push_inline(name, bit, false)?;
                } else if is_block(name) {
                    self.close_block()?;
                }
                // Anything else is ignored, and its text is treated as
                // belonging to whatever block is open. That is the right
                // answer for `<span>`, `<font>` and the hundred elements
                // this has never heard of.
            }
        }
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Error> {
        match name {
            "title" => {
                self.in_title = false;
                self.run_open = false;
                self.at_block_start = true;
                self.pending_space = false;
            }
            "pre" => {
                self.preformatted = false;
                self.close_block()?;
            }
            "ul" | "ol" => {
                self.close_block()?;
                self.lists.pop();
            }
            "li" => self.close_block()?,
            // `script` and `style` reach here from the tokenizer's raw-text
            // path. Nothing to close: their content never arrived.
            "script" | "style" => {}
            name => {
                if heading_level(name).is_some() {
                    self.close_block()?;
                } else if name == "a" || inline_style(name).is_some() {
                    self.pop_inline(name)?;
                } else if is_block(name) {
                    self.close_block()?;
                }
            }
        }
        Ok(())
    }

    /// `[alt text]`, or `[image]` when there is nothing to say.
    ///
    /// The brackets are there because the alternative -- dropping images
    /// entirely -- makes a page of thumbnails read as a page of nothing,
    /// and running the alt text into the prose around it makes a caption
    /// look like a sentence.
    fn push_image(&mut self, alt: Option<&str>) -> Result<(), Error> {
        let alt = alt.unwrap_or("").trim();
        // An image is a word: separated from what is around it, whether or
        // not the markup put whitespace there.
        self.pending_space = true;
        self.flush_pending_space()?;
        self.push_character('[')?;
        if alt.is_empty() {
            self.push_text("image")?;
        } else {
            self.push_text(alt)?;
        }
        self.push_character(']')?;
        self.pending_space = true;
        Ok(())
    }

    fn list_marker(&mut self) -> (u8, Marker) {
        let depth = self.lists.len().saturating_sub(1).min(u8::MAX as usize) as u8;
        match self.lists.last_mut() {
            Some(level) if level.ordered => {
                let number = level.next_number;
                level.next_number = level.next_number.saturating_add(1);
                (depth, Marker::Number(number))
            }
            Some(_) => (depth, Marker::Bullet),
            // `<li>` with no list around it. Still a list item: that is
            // what the author meant, and refusing to show it helps nobody.
            None => (0, Marker::Bullet),
        }
    }

    fn start_link(&mut self, href: Option<&str>) -> Result<(), Error> {
        let Some(href) = href else {
            // An anchor with no target -- `<a name="x">`. It is a frame all
            // the same, so its `</a>` closes something.
            return self.push_inline("a", 0, false);
        };
        let resolved = self.document.url.resolve(href);
        let Ok(url) = resolved else {
            // A link this cannot use -- `javascript:`, `mailto:`, one past
            // the URL bound. The text stays and stops being clickable,
            // which is better than dropping the words with the link.
            return self.push_inline("a", 0, false);
        };
        if self.document.links.len() >= MAX_LINKS {
            return Err(Error::TooManyLinks);
        }
        let index = self.document.links.len() as u16;
        memory::push(&mut self.document.links, Link { url })?;
        self.push_inline("a", 0, true)?;
        let style = self.style;
        self.set_style(style, Some(index))
    }

    /// Pushes an inline frame and applies its style.
    ///
    /// Past [`MAX_NESTING_DEPTH`] no frame is pushed and no style applied:
    /// the text keeps arriving, and a document nested thirty-three deep
    /// gets read rather than refused. The end tags that follow find no
    /// frame and do nothing, which is the same outcome.
    fn push_inline(&mut self, name: &str, bit: u8, sets_link: bool) -> Result<(), Error> {
        if self.inline.len() >= MAX_NESTING_DEPTH {
            return Ok(());
        }
        let mut stored = [0u8; 8];
        let bytes = name.as_bytes();
        let length = bytes.len().min(stored.len());
        stored[..length].copy_from_slice(&bytes[..length]);
        let added = bit & !self.style;
        memory::push(
            &mut self.inline,
            InlineFrame {
                name: stored,
                length: length as u8,
                added,
                previous_link: self.link,
                sets_link,
            },
        )?;
        if added != 0 {
            let style = self.style | added;
            let link = self.link;
            self.set_style(style, link)?;
        }
        Ok(())
    }

    /// Closes the innermost frame with this name, and everything inside it.
    ///
    /// Closing the frames above it too is what makes `<b><i>x</b>` behave:
    /// the `</b>` was meant to end the bold, and leaving the italic open
    /// would run it to the end of the page. An end tag with no frame at all
    /// is ignored.
    fn pop_inline(&mut self, name: &str) -> Result<(), Error> {
        let bytes = name.as_bytes();
        let found = self
            .inline
            .iter()
            .rposition(|frame| &frame.name[..frame.length as usize] == bytes);
        let Some(index) = found else {
            return Ok(());
        };
        let mut style = self.style;
        let mut link = self.link;
        while self.inline.len() > index {
            let Some(frame) = self.inline.pop() else {
                break;
            };
            style &= !frame.added;
            if frame.sets_link {
                link = frame.previous_link;
            }
        }
        self.set_style(style, link)
    }
}

impl html::Sink for Builder {
    fn text(&mut self, text: &str) -> Result<(), Error> {
        self.push_text(text)
    }

    fn start_tag(&mut self, tag: Tag<'_>) -> Result<(), Error> {
        self.start_element(tag)
    }

    fn end_tag(&mut self, name: &str) -> Result<(), Error> {
        self.end_element(name)
    }
}

fn heading_level(name: &str) -> Option<u8> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn inline_style(name: &str) -> Option<u8> {
    match name {
        "strong" | "b" => Some(STYLE_BOLD),
        "em" | "i" => Some(STYLE_ITALIC),
        "code" | "kbd" | "samp" | "tt" | "var" => Some(STYLE_CODE),
        _ => None,
    }
}

/// Elements that end the current block and start a new one.
///
/// A list rather than "anything not inline", because the failure modes
/// point opposite ways: an unknown element treated as a block breaks a
/// sentence in half, while an unknown element treated as inline merely
/// fails to add a break that was probably there. The second is the one to
/// be wrong in, so unknown elements are inline and this list is explicit.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "aside"
            | "blockquote"
            | "figure"
            | "figcaption"
            | "address"
            | "form"
            | "fieldset"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "caption"
            | "dl"
            | "dt"
            | "dd"
            | "body"
            | "html"
            | "head"
            | "hgroup"
            | "details"
            | "summary"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec::Vec;

    fn base() -> Url {
        Url::parse("http://example.com/a/b/page.html").unwrap()
    }

    fn parse(input: &[u8]) -> Document {
        let mut parser = Parser::new(base()).unwrap();
        parser.feed(input).unwrap();
        parser.finish().unwrap()
    }

    fn parse_in_chunks(input: &[u8], size: usize) -> Document {
        let mut parser = Parser::new(base()).unwrap();
        for chunk in input.chunks(size) {
            parser.feed(chunk).unwrap();
        }
        parser.finish().unwrap()
    }

    fn parse_error(input: &[u8]) -> Error {
        let mut parser = Parser::new(base()).unwrap();
        match parser.feed(input) {
            Err(error) => error,
            Ok(()) => match parser.finish() {
                Err(error) => error,
                Ok(_) => panic!("expected a limit to be reached"),
            },
        }
    }

    /// Every block rendered as `kind|text`, which is compact enough to
    /// compare a whole document with one assertion.
    fn outline(document: &Document) -> Vec<String> {
        document
            .blocks()
            .iter()
            .map(|block| {
                let kind = match block.kind {
                    BlockKind::Paragraph => "p".to_string(),
                    BlockKind::Heading(level) => format!("h{level}"),
                    BlockKind::ListItem { depth, marker } => match marker {
                        Marker::Bullet => format!("li{depth}"),
                        Marker::Number(number) => format!("li{depth}#{number}"),
                    },
                    BlockKind::Preformatted => "pre".to_string(),
                    BlockKind::Rule => "hr".to_string(),
                };
                let mut text = String::new();
                for run in document.block_runs(block) {
                    text.push_str(document.run_text(run));
                }
                format!("{kind}|{text}")
            })
            .collect()
    }

    fn link_targets(document: &Document) -> Vec<String> {
        document
            .links()
            .iter()
            .map(|link| link.url.to_text().unwrap())
            .collect()
    }

    // --- structure --------------------------------------------------------

    #[test]
    fn paragraphs_and_headings_become_blocks() {
        let document = parse(b"<h1>Simple</h1><p>First paragraph.</p><p>Second paragraph.</p>");
        assert_eq!(
            outline(&document),
            ["h1|Simple", "p|First paragraph.", "p|Second paragraph."]
        );
    }

    #[test]
    fn the_title_is_kept_out_of_the_body() {
        let document = parse(b"<html><head><title>A title</title></head><body><p>Body</p>");
        assert_eq!(document.title(), "A title");
        assert_eq!(outline(&document), ["p|Body"]);
        assert!(!document.text().contains("A title"));
    }

    #[test]
    fn whitespace_between_words_collapses() {
        let document = parse(b"<p>one   two\n\tthree\r\n    four</p>");
        assert_eq!(outline(&document), ["p|one two three four"]);
    }

    #[test]
    fn leading_and_trailing_whitespace_in_a_block_is_dropped() {
        let document = parse(b"<p>\n   text   \n</p>");
        assert_eq!(outline(&document), ["p|text"]);
    }

    #[test]
    fn empty_blocks_are_not_kept() {
        let document = parse(b"<div></div><p>  </p><p>real</p><div>   </div>");
        assert_eq!(outline(&document), ["p|real"]);
    }

    #[test]
    fn a_document_with_no_text_has_no_blocks() {
        let document = parse(b"<!DOCTYPE html><html><body><!-- nothing --></body></html>");
        assert!(document.blocks().is_empty());
        assert_eq!(document.text(), "");
        assert_eq!(document.stats().items, 0);
    }

    #[test]
    fn br_breaks_a_line_without_ending_the_block() {
        let document = parse(b"<p>one<br>two<br/>three</p>");
        assert_eq!(outline(&document), ["p|one\ntwo\nthree"]);
    }

    #[test]
    fn hr_is_a_block_of_its_own_with_no_runs() {
        let document = parse(b"<p>above</p><hr><p>below</p>");
        assert_eq!(outline(&document), ["p|above", "hr|", "p|below"]);
        let rule = document.blocks()[1];
        assert_eq!(rule.run_count, 0);
    }

    #[test]
    fn unknown_elements_keep_their_text_inline() {
        let document = parse(b"<p>a <span>b</span> <custom-thing>c</custom-thing> d</p>");
        assert_eq!(outline(&document), ["p|a b c d"]);
    }

    #[test]
    fn block_elements_separate_text_that_would_otherwise_run_together() {
        let document = parse(b"<div>one</div><div>two</div>");
        assert_eq!(outline(&document), ["p|one", "p|two"]);
    }

    // --- lists ------------------------------------------------------------

    #[test]
    fn lists_carry_markers_and_depth() {
        let document = parse(
            b"<ul><li>first</li><li>second<ul><li>nested</li>\
              <ol><li>one</li><li>two</li></ol></ul></li></ul>",
        );
        assert_eq!(
            outline(&document),
            [
                "li0|first",
                "li0|second",
                "li1|nested",
                "li2#1|one",
                "li2#2|two",
            ]
        );
    }

    #[test]
    fn ordered_lists_number_from_one_and_restart() {
        let document = parse(b"<ol><li>a</li><li>b</li></ol><ol><li>c</li></ol>");
        assert_eq!(outline(&document), ["li0#1|a", "li0#2|b", "li0#1|c"]);
    }

    #[test]
    fn list_items_without_closing_tags_still_separate() {
        let document = parse(b"<ul><li>one<li>two<li>three</ul>");
        assert_eq!(outline(&document), ["li0|one", "li0|two", "li0|three"]);
    }

    #[test]
    fn a_list_item_outside_a_list_is_still_an_item() {
        let document = parse(b"<li>orphan</li>");
        assert_eq!(outline(&document), ["li0|orphan"]);
    }

    #[test]
    fn nesting_past_the_depth_bound_keeps_the_text() {
        let depth = MAX_NESTING_DEPTH * 4;
        let mut markup = String::new();
        for _ in 0..depth {
            markup.push_str("<ul><li>");
        }
        markup.push_str("innermost");
        for _ in 0..depth {
            markup.push_str("</li></ul>");
        }
        let document = parse(markup.as_bytes());
        let text: String = outline(&document).join("");
        assert!(text.contains("innermost"), "{text}");
        // The depth is clamped rather than allowed to grow with the input.
        for block in document.blocks() {
            if let BlockKind::ListItem { depth, .. } = block.kind {
                assert!(depth as usize <= MAX_NESTING_DEPTH, "{depth}");
            }
        }
    }

    // --- preformatted -----------------------------------------------------

    #[test]
    fn pre_keeps_its_spaces_and_newlines() {
        let document = parse(b"<pre>a   b\n  c\n</pre>");
        assert_eq!(outline(&document), ["pre|a   b\n  c\n"]);
    }

    #[test]
    fn text_after_a_pre_collapses_again() {
        let document = parse(b"<pre>a   b</pre><p>c    d</p>");
        assert_eq!(outline(&document), ["pre|a   b", "p|c d"]);
    }

    // --- inline style -----------------------------------------------------

    #[test]
    fn inline_elements_split_runs_and_set_style() {
        let document = parse(b"<p>plain <strong>bold</strong> plain</p>");
        let block = document.blocks()[0];
        let runs = document.block_runs(&block);
        assert_eq!(runs.len(), 3);
        assert_eq!(document.run_text(&runs[0]), "plain ");
        assert_eq!(runs[0].style, 0);
        assert_eq!(document.run_text(&runs[1]), "bold");
        assert_eq!(runs[1].style, STYLE_BOLD);
        assert_eq!(document.run_text(&runs[2]), " plain");
        assert_eq!(runs[2].style, 0);
    }

    #[test]
    fn styles_nest_and_come_back_off() {
        let document = parse(b"<p><b>a<i>b</i>c</b>d</p>");
        let runs = document.block_runs(&document.blocks()[0]);
        let styles: Vec<u8> = runs.iter().map(|run| run.style).collect();
        assert_eq!(
            styles,
            [STYLE_BOLD, STYLE_BOLD | STYLE_ITALIC, STYLE_BOLD, 0]
        );
    }

    #[test]
    fn crossed_inline_tags_do_not_leak_style_to_the_end_of_the_page() {
        // `</b>` was meant to end the bold; the italic goes with it rather
        // than staying open for the rest of the document.
        let document = parse(b"<p><b>a<i>b</b>c</p><p>d</p>");
        let last = document.blocks().last().copied().unwrap();
        for run in document.block_runs(&last) {
            assert_eq!(run.style, 0, "{:?}", document.run_text(run));
        }
    }

    #[test]
    fn an_end_tag_with_nothing_open_is_ignored() {
        let document = parse(b"<p>a</b></i></a>b</p>");
        assert_eq!(outline(&document), ["p|ab"]);
    }

    // --- links ------------------------------------------------------------

    #[test]
    fn links_resolve_against_the_page() {
        let document = parse(
            b"<p><a href=\"target.html\">a</a> <a href=\"/root.html\">b</a> \
              <a href=\"http://other.example/c\">c</a></p>",
        );
        assert_eq!(
            link_targets(&document),
            [
                "http://example.com/a/b/target.html",
                "http://example.com/root.html",
                "http://other.example/c",
            ]
        );
    }

    #[test]
    fn link_runs_carry_their_index() {
        let document = parse(b"<p>before <a href=\"/x\">link</a> after</p>");
        let runs = document.block_runs(&document.blocks()[0]);
        let linked: Vec<&str> = runs
            .iter()
            .filter(|run| run.link.is_some())
            .map(|run| document.run_text(run))
            .collect();
        assert_eq!(linked, ["link"]);
        let index = runs.iter().find_map(|run| run.link).unwrap();
        assert_eq!(
            document.link(index).unwrap().url.to_text().unwrap(),
            "http://example.com/x"
        );
    }

    #[test]
    fn an_https_link_is_kept_so_it_can_be_explained() {
        let document = parse(b"<p><a href=\"https://example.com/s\">secure</a></p>");
        assert_eq!(link_targets(&document), ["https://example.com/s"]);
        assert!(!document.links()[0].url.scheme().is_cleartext());
    }

    #[test]
    fn a_link_this_cannot_follow_keeps_its_text_and_stops_being_a_link() {
        let document = parse(
            b"<p><a href=\"javascript:void(0)\">js</a> \
              <a href=\"mailto:a@b\">mail</a> \
              <a href=\"ftp://h/f\">ftp</a></p>",
        );
        assert!(document.links().is_empty());
        assert_eq!(outline(&document), ["p|js mail ftp"]);
    }

    #[test]
    fn an_over_long_href_does_not_become_a_link() {
        let href = "/".to_string() + &"p".repeat(4096);
        let markup = format!("<p><a href=\"{href}\">text</a></p>");
        let document = parse(markup.as_bytes());
        assert!(document.links().is_empty());
        assert_eq!(outline(&document), ["p|text"]);
    }

    #[test]
    fn an_unclosed_link_does_not_swallow_the_rest_of_the_page() {
        let document = parse(b"<p>a <a href=\"/x\">link<p>next paragraph</p>");
        let last = document.blocks().last().copied().unwrap();
        // The link is still open by HTML's rules, and this does not try to
        // be cleverer than that -- but it is one link, not one per block.
        assert_eq!(document.links().len(), 1);
        assert!(!document.block_runs(&last).is_empty());
    }

    #[test]
    fn a_fragment_link_resolves_to_the_same_document() {
        let document = parse(b"<p><a href=\"#part\">jump</a></p>");
        let link = &document.links()[0];
        assert!(link.url.same_document(&base()));
        assert_eq!(link.url.fragment(), Some("part"));
    }

    // --- images -----------------------------------------------------------

    #[test]
    fn images_show_their_alt_text_in_brackets() {
        let document = parse(
            b"<p>a <img src=\"x.png\" alt=\"a red square\"> b \
              <img src=\"y.png\"> c <img src=\"z.png\" alt=\"\"> d</p>",
        );
        assert_eq!(
            outline(&document),
            ["p|a [a red square] b [image] c [image] d"]
        );
    }

    // --- entities and encoding -------------------------------------------

    #[test]
    fn character_references_reach_the_text() {
        let document = parse(b"<p>&amp; &lt; &gt; &#65; &#x42;</p>");
        assert_eq!(outline(&document), ["p|& < > A B"]);
    }

    #[test]
    fn a_non_breaking_space_is_not_collapsed_away() {
        let document = parse(b"<p>a&nbsp;&nbsp;b</p>");
        assert_eq!(outline(&document), ["p|a\u{00A0}\u{00A0}b"]);
    }

    #[test]
    fn invalid_utf8_does_not_lose_the_text_around_it() {
        let document = parse(b"<p>before \xff after</p><p>\xe6\x97\xa5\xe6\x9c\xac</p>");
        assert_eq!(outline(&document), ["p|before \u{FFFD} after", "p|日本"]);
    }

    // --- chunking ---------------------------------------------------------

    /// The Stage 3 completion condition: the document does not depend on
    /// how the body arrived.
    fn assert_chunking_invisible(input: &[u8]) {
        let expected = outline(&parse(input));
        let expected_links = link_targets(&parse(input));
        for size in 1..=input.len().min(48) {
            let document = parse_in_chunks(input, size);
            assert_eq!(outline(&document), expected, "chunks of {size}");
            assert_eq!(link_targets(&document), expected_links, "chunks of {size}");
        }
    }

    #[test]
    fn chunking_does_not_change_the_document() {
        assert_chunking_invisible(
            "<html><head><title>t</title></head><body>\
             <h1>Head</h1><p>Some <strong>bold</strong> and a \
             <a href=\"target.html?a=1&amp;b=2\">link</a>.</p>\
             <ul><li>one</li><li>two</li></ul><hr>\
             <pre>  spaced\n  lines\n</pre>\
             <p>日本語 &#128169; <img src=\"x\" alt=\"pic\"></p>\
             <script>if (a<b) {}</script><p>end</p></body></html>"
                .as_bytes(),
        );
    }

    #[test]
    fn chunking_does_not_change_a_broken_document() {
        assert_chunking_invisible(
            b"<h1>Unterminated\n<p>never closed\n<ul><li>one<li>two\n\
              <p>an <a href=\"/x\">unclosed link\n<div><div><span>tail",
        );
    }

    // --- limits -----------------------------------------------------------

    #[test]
    fn too_many_items_is_an_error_rather_than_a_short_page() {
        let mut markup = String::new();
        for index in 0..MAX_ITEMS {
            markup.push_str(&format!("<p>Item number {index}.</p>"));
        }
        assert_eq!(parse_error(markup.as_bytes()), Error::TooManyItems);
    }

    #[test]
    fn too_many_links_is_an_error() {
        let mut markup = String::new();
        for index in 0..=MAX_LINKS {
            markup.push_str(&format!("<a href=\"/t?n={index}\">l</a>"));
        }
        assert_eq!(parse_error(markup.as_bytes()), Error::TooManyLinks);
    }

    #[test]
    fn exactly_the_link_bound_is_allowed() {
        let mut markup = String::new();
        for index in 0..MAX_LINKS {
            markup.push_str(&format!("<a href=\"/t?n={index}\">l</a>"));
        }
        let document = parse(markup.as_bytes());
        assert_eq!(document.links().len(), MAX_LINKS);
    }

    #[test]
    fn too_much_text_is_an_error() {
        let paragraph = "x".repeat(4096);
        let mut markup = String::new();
        while markup.len() < MAX_TEXT_BYTES + 8192 {
            markup.push_str(&paragraph);
        }
        assert_eq!(parse_error(markup.as_bytes()), Error::TextTooLong);
    }

    #[test]
    fn a_document_that_reaches_a_limit_is_never_returned() {
        let mut markup = String::new();
        for index in 0..MAX_ITEMS {
            markup.push_str(&format!("<p>{index}</p>"));
        }
        let mut parser = Parser::new(base()).unwrap();
        let outcome = parser.feed(markup.as_bytes());
        assert!(outcome.is_err());
        // There is no way to ask for the partial document: `finish`
        // consumes the parser, and the error path never gets there.
    }

    // --- memory -----------------------------------------------------------

    #[test]
    fn a_large_page_stays_inside_the_owned_budget() {
        use crate::limits::MAX_BROWSER_OWNED_BYTES;
        let mut markup = String::new();
        // A page of real shape: headings, prose and links, up to about a
        // megabyte of markup.
        while markup.len() < 1024 * 1024 {
            markup.push_str(
                "<h2>Section</h2><p>Some prose with a \
                 <a href=\"/target.html\">link</a> in it, of the sort a page \
                 is actually made of.</p>",
            );
        }
        let mut parser = Parser::new(base()).unwrap();
        let mut peak = 0usize;
        for chunk in markup.as_bytes().chunks(4096) {
            match parser.feed(chunk) {
                Ok(()) => {}
                // A page this size reaches the link bound long before the
                // memory bound, which is the point: the limits bite first.
                Err(error) => {
                    assert!(matches!(
                        error,
                        Error::TooManyLinks | Error::TooManyItems | Error::TextTooLong
                    ));
                    return;
                }
            }
            peak = peak.max(parser.owned_bytes());
            assert!(peak < MAX_BROWSER_OWNED_BYTES, "{peak}");
        }
    }

    #[test]
    fn the_raw_html_is_not_kept() {
        // Half a megabyte of markup, a few kilobytes of text.
        let mut markup = String::new();
        while markup.len() < 512 * 1024 {
            markup.push_str("<div class=\"a b c\" data-x=\"yyyyyyyyyyyyyyyyyyyy\"></div>");
        }
        markup.push_str("<p>the only text</p>");
        let document = parse(markup.as_bytes());
        assert_eq!(outline(&document), ["p|the only text"]);
        assert!(
            document.stats().owned_bytes < 8192,
            "{:?}",
            document.stats()
        );
        assert!(document.stats().input_bytes > 512 * 1024);
    }

    #[test]
    fn statistics_describe_the_page() {
        let document = parse(
            b"<title>t</title><h1>Head</h1><p>Text with a \
              <a href=\"/x\">link</a>.</p>",
        );
        let stats = document.stats();
        assert_eq!(stats.links, 1);
        assert_eq!(stats.text_bytes, document.text().len());
        assert_eq!(stats.items, document.blocks().len() + document.runs().len());
        assert!(stats.input_bytes > 0);
        assert!(stats.owned_bytes > 0);
    }
}
