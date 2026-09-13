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
use crate::limits::{
    MAX_ANCHOR_BYTES, MAX_ANCHOR_NAME_BYTES, MAX_ANCHORS, MAX_IMAGE_HEIGHT, MAX_IMAGE_WIDTH,
    MAX_IMAGES, MAX_INPUT_VALUE_BYTES, MAX_ITEMS, MAX_LINK_URL_BYTES, MAX_LINKS, MAX_NESTING_DEPTH,
    MAX_TEXT_BYTES,
};
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
    /// A visible form control. The index addresses `Document::controls`.
    Control(u16),
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
    /// Controls occurring in this block, in document order. Hidden controls
    /// are included but consume no layout space.
    pub first_control: u16,
    pub control_count: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowGroup {
    Head,
    Body,
    Foot,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Table {
    pub first_cell: u32,
    pub cell_count: u32,
    pub first_row: u32,
    pub row_count: u16,
    pub column_count: u8,
    pub border: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TableRow {
    pub table: u16,
    pub number: u16,
    pub group: RowGroup,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TableCell {
    pub table: u16,
    pub block: u32,
    pub text_start: u32,
    pub text_end: u32,
    pub first_image: u16,
    pub image_count: u16,
    pub first_control: u16,
    pub control_count: u16,
    pub row: u16,
    pub column: u8,
    pub rowspan: u8,
    pub colspan: u8,
    pub header: bool,
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

pub struct Anchor {
    pub name: String,
    pub text_offset: u32,
    pub block_index: u32,
}

/// One `img` occurrence. Pixels arrive later; this preserves identity and
/// layout inputs without retaining the source markup.
pub struct Image {
    pub source: Option<Url>,
    pub alt: String,
    pub width: Option<u16>,
    pub height: Option<u16>,
    pub intrinsic_width: Option<u16>,
    pub intrinsic_height: Option<u16>,
    pub text_offset: u32,
    pub text_end: u32,
    pub link: Option<u16>,
    /// A button owning this image. Such an image is laid out inside the
    /// button rather than as an independent document image.
    pub button: Option<u16>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ButtonRun {
    /// Byte offsets into the owning control's `display_label`.
    pub start: u16,
    pub end: u16,
    pub style: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FormMethod {
    Get,
    Post,
    Unsupported,
}

pub struct Form {
    pub action: Url,
    pub method: FormMethod,
    pub first_control: u32,
    pub control_count: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlKind {
    Text,
    Hidden,
    Submit,
    Checkbox,
    Radio,
    Textarea,
    Select,
    Unsupported,
}

impl ControlKind {
    /// Checkbox and radio: a control whose state is a checkedness rather
    /// than an edited value.
    pub fn is_checkable(self) -> bool {
        matches!(self, Self::Checkbox | Self::Radio)
    }
}

pub struct Control {
    pub form: Option<u16>,
    pub kind: ControlKind,
    pub name: String,
    pub initial_value: String,
    /// Visible button text. Empty for input controls, whose value is shown.
    pub display_label: String,
    /// Distinguishes `<button></button>` from an `<input type=submit>` with
    /// no value; only the latter receives the conventional `Submit` label.
    pub button_element: bool,
    pub first_button_run: u32,
    pub button_run_count: u16,
    pub element_id: String,
    pub disabled: bool,
    /// Initial checkedness of a checkbox or radio button. For a radio group
    /// only the last `checked` in document order stays set.
    pub checked: bool,
    /// Visible rows of a textarea, 1..=`MAX_TEXTAREA_ROWS`. Zero otherwise.
    pub rows: u8,
    /// A textarea whose initial content was longer than
    /// `MAX_INPUT_VALUE_BYTES`. `initial_value` holds only the part that fit,
    /// so the control cannot be edited and its form refuses to submit rather
    /// than send a cut value.
    pub value_overflow: bool,
    /// A select's options: `Document::options()[first_option..]`, then
    /// `option_count` of them.
    pub first_option: u32,
    pub option_count: u16,
    /// A select that allows more than one selected option.
    pub multiple: bool,
    pub text_offset: u32,
}

/// One `option` of a select.
pub struct SelectOption {
    pub control: u16,
    /// The text between the tags, with whitespace collapsed.
    pub label: String,
    /// `value`, or the label when the attribute is missing.
    pub value: String,
    /// Initial selectedness after HTML's rules: in a single select exactly
    /// one enabled option when there is any, the last `selected` winning.
    pub selected: bool,
    /// The option's own `disabled`, or its `optgroup`'s.
    pub disabled: bool,
}

/// Rows a textarea shows when `rows` is missing or invalid, as in HTML.
pub const DEFAULT_TEXTAREA_ROWS: u8 = 2;
/// The most rows a textarea box may take; taller ones scroll inside.
pub const MAX_TEXTAREA_ROWS: u8 = 8;

/// Which element produced a control.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlElement<'a> {
    Input(Option<&'a str>),
    Button(&'a str),
    Textarea(Option<&'a str>),
    Select { multiple: bool },
}

pub struct Label {
    pub control: Option<u16>,
    pub text_start: u32,
    pub text_end: u32,
    target_id: String,
}

/// What one page cost, for the UART line and the memory budget.
#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    /// HTML bytes fed in.
    pub input_bytes: usize,
    pub text_bytes: usize,
    /// Blocks, runs, and table structures bounded by [`MAX_ITEMS`].
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
    anchors: Vec<Anchor>,
    images: Vec<Image>,
    tables: Vec<Table>,
    table_rows: Vec<TableRow>,
    table_cells: Vec<TableCell>,
    forms: Vec<Form>,
    controls: Vec<Control>,
    button_runs: Vec<ButtonRun>,
    options: Vec<SelectOption>,
    labels: Vec<Label>,
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
    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }
    pub fn images(&self) -> &[Image] {
        &self.images
    }
    pub fn set_image_intrinsic(&mut self, index: usize, dimensions: Option<(u16, u16)>) -> bool {
        let Some(image) = self.images.get_mut(index) else {
            return false;
        };
        let (width, height) = dimensions
            .map(|(width, height)| (Some(width.max(1)), Some(height.max(1))))
            .unwrap_or((None, None));
        image.intrinsic_width = width;
        image.intrinsic_height = height;
        true
    }
    pub fn tables(&self) -> &[Table] {
        &self.tables
    }
    pub fn table_rows(&self) -> &[TableRow] {
        &self.table_rows
    }
    pub fn table_cells(&self) -> &[TableCell] {
        &self.table_cells
    }
    pub fn forms(&self) -> &[Form] {
        &self.forms
    }
    pub fn controls(&self) -> &[Control] {
        &self.controls
    }
    pub fn button_runs(&self, control: &Control) -> &[ButtonRun] {
        let start = control.first_button_run as usize;
        self.button_runs
            .get(start..start + control.button_run_count as usize)
            .unwrap_or(&[])
    }
    pub fn options(&self) -> &[SelectOption] {
        &self.options
    }
    /// The options belonging to one select; empty for any other control.
    pub fn control_options(&self, control: &Control) -> &[SelectOption] {
        let start = control.first_option as usize;
        self.options
            .get(start..start + control.option_count as usize)
            .unwrap_or(&[])
    }
    pub fn labels(&self) -> &[Label] {
        &self.labels
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
        let anchors: usize = self
            .anchors
            .iter()
            .map(|anchor| anchor.name.capacity())
            .sum();
        self.title.capacity()
            + self.text.capacity()
            + self.runs.capacity() * core::mem::size_of::<Run>()
            + self.blocks.capacity() * core::mem::size_of::<Block>()
            + self.links.capacity() * core::mem::size_of::<Link>()
            + links
            + self.anchors.capacity() * core::mem::size_of::<Anchor>()
            + anchors
            + self.images.capacity() * core::mem::size_of::<Image>()
            + self
                .images
                .iter()
                .map(|image| {
                    image.alt.capacity() + image.source.as_ref().map_or(0, Url::owned_bytes)
                })
                .sum::<usize>()
            + self.tables.capacity() * core::mem::size_of::<Table>()
            + self.table_rows.capacity() * core::mem::size_of::<TableRow>()
            + self.table_cells.capacity() * core::mem::size_of::<TableCell>()
            + self.forms.capacity() * core::mem::size_of::<Form>()
            + self
                .forms
                .iter()
                .map(|form| form.action.owned_bytes())
                .sum::<usize>()
            + self.controls.capacity() * core::mem::size_of::<Control>()
            + self.button_runs.capacity() * core::mem::size_of::<ButtonRun>()
            + self
                .controls
                .iter()
                .map(|control| {
                    control.name.capacity()
                        + control.initial_value.capacity()
                        + control.display_label.capacity()
                        + control.element_id.capacity()
                })
                .sum::<usize>()
            + self.options.capacity() * core::mem::size_of::<SelectOption>()
            + self
                .options
                .iter()
                .map(|option| option.label.capacity() + option.value.capacity())
                .sum::<usize>()
            + self.labels.capacity() * core::mem::size_of::<Label>()
            + self
                .labels
                .iter()
                .map(|label| label.target_id.capacity())
                .sum::<usize>()
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
        self.builder.finish(input, longest)
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
    open_first_control: u16,
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
    link_url_bytes: usize,
    table: Option<TableBuild>,
    form: Option<u16>,
    button: Option<ButtonBuild>,
    textarea: Option<TextareaBuild>,
    select: Option<SelectBuild>,
    label: Option<LabelBuild>,
}

struct SelectBuild {
    control: u16,
    /// Inside a disabled `optgroup`.
    group_disabled: bool,
    option: Option<OptionBuild>,
}

struct OptionBuild {
    index: usize,
    text: String,
    pending_space: bool,
    has_value: bool,
}

struct TextareaBuild {
    control: u16,
    text: String,
    /// Nothing has been kept yet, so a first line break is still dropped.
    at_start: bool,
    /// The previous character was a CR, so an LF right after it is part of
    /// the same line break.
    after_cr: bool,
    overflow: bool,
}

struct ButtonBuild {
    control: u16,
    text: String,
    pending_space: bool,
    style: u8,
    inline: Vec<InlineFrame>,
    suppressed_controls: u8,
}

struct LabelBuild {
    target_id: String,
    text_start: u32,
}

struct TableBuild {
    index: u16,
    first_cell: u32,
    first_row: u32,
    row: Option<u16>,
    group: RowGroup,
    occupied: [u8; crate::limits::MAX_TABLE_COLUMNS],
    open_cell: Option<OpenCell>,
    columns: u8,
    nested: u8,
    nested_cell_in_row: bool,
}

#[derive(Clone, Copy)]
struct OpenCell {
    row: u16,
    column: u8,
    rowspan: u8,
    colspan: u8,
    header: bool,
    text_start: u32,
    first_image: u16,
    first_control: u16,
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
                anchors: Vec::new(),
                images: Vec::new(),
                tables: Vec::new(),
                table_rows: Vec::new(),
                table_cells: Vec::new(),
                forms: Vec::new(),
                controls: Vec::new(),
                button_runs: Vec::new(),
                options: Vec::new(),
                labels: Vec::new(),
                stats: Stats::default(),
            },
            open_kind: None,
            open_first_run: 0,
            open_first_control: 0,
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
            link_url_bytes: 0,
            table: None,
            form: None,
            button: None,
            textarea: None,
            select: None,
            label: None,
        })
    }

    fn items(&self) -> usize {
        self.document.blocks.len()
            + self.document.runs.len()
            + self.document.tables.len()
            + self.document.table_rows.len()
            + self.document.table_cells.len()
            + self.document.images.len()
            + self.document.forms.len()
            + self.document.controls.len()
            + self.document.button_runs.len()
            + self.document.options.len()
            + self.document.labels.len()
    }

    fn finish(mut self, input_bytes: usize, longest_token: usize) -> Result<Document, Error> {
        // The last block is committed the same way every other one is; a
        // page that ends mid-paragraph is not a special case.
        self.end_button();
        self.end_textarea();
        self.end_select()?;
        self.end_label()?;
        if self.table.is_some() {
            self.end_table()?;
        }
        self.close_block()?;
        for label in &mut self.document.labels {
            label.control = self
                .document
                .controls
                .iter()
                .position(|control| {
                    !label.target_id.is_empty() && control.element_id == label.target_id
                })
                .and_then(|index| u16::try_from(index).ok());
        }
        self.document.stats = Stats {
            input_bytes,
            text_bytes: self.document.text.len(),
            items: self.items(),
            links: self.document.links.len(),
            owned_bytes: self.document.owned_bytes(),
            longest_token,
        };
        Ok(self.document)
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
        if self.select.is_some() {
            return self.push_option_text(text);
        }
        if self.textarea.is_some() {
            return self.push_textarea_text(text);
        }
        if self.button.is_some() {
            return self.push_button_text(text);
        }
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

    /// Keeps a textarea's content as its initial value: whitespace as written,
    /// every line break as LF, and the one line break right after the start
    /// tag dropped, as HTML does.
    fn push_textarea_text(&mut self, text: &str) -> Result<(), Error> {
        let Some(textarea) = self.textarea.as_mut() else {
            return Ok(());
        };
        for character in text.chars() {
            let after_cr = core::mem::replace(&mut textarea.after_cr, character == '\r');
            if character == '\n' && after_cr {
                continue;
            }
            let character = if character == '\r' { '\n' } else { character };
            if core::mem::replace(&mut textarea.at_start, false) && character == '\n' {
                continue;
            }
            if textarea.overflow {
                continue;
            }
            if textarea.text.len() + character.len_utf8() > MAX_INPUT_VALUE_BYTES {
                textarea.overflow = true;
                continue;
            }
            memory::push_char(&mut textarea.text, character)?;
        }
        Ok(())
    }

    /// Option text, collapsed like a button's label. Text in a select but
    /// outside any option is not shown anywhere.
    fn push_option_text(&mut self, text: &str) -> Result<(), Error> {
        let Some(option) = self
            .select
            .as_mut()
            .and_then(|select| select.option.as_mut())
        else {
            return Ok(());
        };
        for character in text.chars() {
            if character.is_whitespace() {
                option.pending_space |= !option.text.is_empty();
                continue;
            }
            let added = character.len_utf8() + usize::from(option.pending_space);
            if option.text.len().saturating_add(added) > MAX_INPUT_VALUE_BYTES {
                return Err(Error::TextTooLong);
            }
            if option.pending_space {
                memory::push_char(&mut option.text, ' ')?;
                option.pending_space = false;
            }
            memory::push_char(&mut option.text, character)?;
        }
        Ok(())
    }

    fn begin_option(
        &mut self,
        value: Option<&str>,
        selected: bool,
        disabled: bool,
    ) -> Result<(), Error> {
        self.end_option()?;
        let Some(select) = self.select.as_ref() else {
            return Ok(());
        };
        if self.document.options.len() >= crate::limits::MAX_SELECT_OPTIONS {
            return Err(Error::TooManyItems);
        }
        self.check_items(1)?;
        let control = select.control;
        let disabled = disabled || select.group_disabled;
        let index = self.document.options.len();
        memory::push(
            &mut self.document.options,
            SelectOption {
                control,
                label: String::new(),
                value: memory::string_from(value.unwrap_or(""))?,
                selected,
                disabled,
            },
        )?;
        if let Some(item) = self.document.controls.get_mut(control as usize) {
            item.option_count = item.option_count.saturating_add(1);
        }
        if let Some(select) = self.select.as_mut() {
            select.option = Some(OptionBuild {
                index,
                text: String::new(),
                pending_space: false,
                has_value: value.is_some(),
            });
        }
        Ok(())
    }

    fn end_option(&mut self) -> Result<(), Error> {
        let Some(option) = self.select.as_mut().and_then(|select| select.option.take()) else {
            return Ok(());
        };
        let Some(item) = self.document.options.get_mut(option.index) else {
            return Ok(());
        };
        if !option.has_value {
            item.value = memory::string_from(&option.text)?;
        }
        item.label = option.text;
        Ok(())
    }

    /// Closes a select and settles its initial selectedness the way HTML
    /// does for a single-line list: the last `selected` wins, and with none
    /// the first enabled option is selected.
    fn end_select(&mut self) -> Result<(), Error> {
        self.end_option()?;
        let Some(select) = self.select.take() else {
            return Ok(());
        };
        let Some(control) = self.document.controls.get(select.control as usize) else {
            return Ok(());
        };
        if control.multiple {
            return Ok(());
        }
        let start = control.first_option as usize;
        let end = start + control.option_count as usize;
        let options = &mut self.document.options[start..end];
        let last = options.iter().rposition(|option| option.selected);
        for (index, option) in options.iter_mut().enumerate() {
            option.selected = Some(index) == last;
        }
        if last.is_none()
            && let Some(first) = options.iter_mut().find(|option| !option.disabled)
        {
            first.selected = true;
        }
        Ok(())
    }

    fn end_textarea(&mut self) {
        if let Some(textarea) = self.textarea.take()
            && let Some(control) = self.document.controls.get_mut(textarea.control as usize)
        {
            control.initial_value = textarea.text;
            control.value_overflow = textarea.overflow;
        }
    }

    fn push_button_text(&mut self, text: &str) -> Result<(), Error> {
        for character in text.chars() {
            let Some(button) = self.button.as_mut() else {
                return Ok(());
            };
            if button.suppressed_controls != 0 {
                continue;
            }
            if character.is_whitespace() {
                button.pending_space |= !button.text.is_empty();
                continue;
            }
            let added = character.len_utf8() + usize::from(button.pending_space);
            if button.text.len().saturating_add(added) > MAX_INPUT_VALUE_BYTES {
                return Err(Error::TextTooLong);
            }
            if button.pending_space {
                button.pending_space = false;
                self.push_button_character(' ')?;
            }
            self.push_button_character(character)?;
        }
        Ok(())
    }

    fn push_button_character(&mut self, character: char) -> Result<(), Error> {
        let (control, style, start) = {
            let button = self.button.as_ref().ok_or(Error::TooManyItems)?;
            (button.control, button.style, button.text.len())
        };
        let new_run = self
            .document
            .button_runs
            .last()
            .is_none_or(|run| run.style != style || run.end as usize != start);
        if new_run {
            self.check_items(1)?;
        }
        let button = self.button.as_mut().ok_or(Error::TooManyItems)?;
        memory::push_char(&mut button.text, character)?;
        let end = button.text.len() as u16;
        if new_run {
            memory::push(
                &mut self.document.button_runs,
                ButtonRun {
                    start: start as u16,
                    end,
                    style,
                },
            )?;
            if let Some(item) = self.document.controls.get_mut(control as usize) {
                item.button_run_count = item.button_run_count.saturating_add(1);
            }
        } else if let Some(run) = self.document.button_runs.last_mut() {
            run.end = end;
        }
        Ok(())
    }

    fn start_button_child(&mut self, tag: Tag<'_>) -> Result<(), Error> {
        let suppressed = self
            .button
            .as_ref()
            .is_some_and(|button| button.suppressed_controls != 0);
        if suppressed {
            if matches!(tag.name, "select" | "textarea" | "button")
                && let Some(button) = self.button.as_mut()
            {
                button.suppressed_controls = button.suppressed_controls.saturating_add(1);
            }
            return Ok(());
        }
        if tag.name == "input" {
            return Ok(());
        }
        if matches!(tag.name, "select" | "textarea" | "button") {
            if let Some(button) = self.button.as_mut() {
                button.suppressed_controls = 1;
            }
            return Ok(());
        }
        if tag.name == "img" {
            return self.push_button_image(tag.src, tag.alt, tag.width, tag.height);
        }
        let Some(bit) = inline_style(tag.name) else {
            return Ok(());
        };
        let button = self.button.as_mut().unwrap();
        if button.inline.len() >= MAX_NESTING_DEPTH {
            return Err(Error::TooManyItems);
        }
        let added = bit & !button.style;
        let mut stored = [0; 8];
        let bytes = tag.name.as_bytes();
        stored[..bytes.len()].copy_from_slice(bytes);
        memory::push(
            &mut button.inline,
            InlineFrame {
                name: stored,
                length: bytes.len() as u8,
                added,
                previous_link: None,
                sets_link: false,
            },
        )?;
        button.style |= bit;
        Ok(())
    }

    fn end_button_child(&mut self, name: &str) -> Result<(), Error> {
        if self
            .button
            .as_ref()
            .is_some_and(|button| button.suppressed_controls != 0)
        {
            if matches!(name, "select" | "textarea" | "button")
                && let Some(button) = self.button.as_mut()
            {
                button.suppressed_controls -= 1;
            }
            return Ok(());
        }
        if name == "button" || name == "form" {
            self.end_button();
            if name == "form" {
                self.form = None;
                self.close_block()?;
            }
            return Ok(());
        }
        let bytes = name.as_bytes();
        let Some(button) = self.button.as_mut() else {
            return Ok(());
        };
        let Some(index) = button
            .inline
            .iter()
            .rposition(|frame| &frame.name[..frame.length as usize] == bytes)
        else {
            return Ok(());
        };
        while button.inline.len() > index {
            if let Some(frame) = button.inline.pop() {
                button.style &= !frame.added;
            }
        }
        Ok(())
    }

    fn push_button_image(
        &mut self,
        src: Option<&str>,
        alt: Option<&str>,
        width: Option<&str>,
        height: Option<&str>,
    ) -> Result<(), Error> {
        if self
            .button
            .as_ref()
            .is_some_and(|button| button.pending_space)
        {
            if let Some(button) = self.button.as_mut() {
                button.pending_space = false;
            }
            self.push_button_character(' ')?;
        }
        if self.document.images.len() >= MAX_IMAGES {
            return Err(Error::TooManyItems);
        }
        self.check_items(1)?;
        let button = self.button.as_ref().unwrap();
        let source = src.and_then(|src| self.document.url.resolve(src).ok());
        memory::push(
            &mut self.document.images,
            Image {
                source,
                alt: memory::string_from(alt.unwrap_or("").trim())?,
                width: image_dimension(width, MAX_IMAGE_WIDTH),
                height: image_dimension(height, MAX_IMAGE_HEIGHT),
                intrinsic_width: None,
                intrinsic_height: None,
                text_offset: button.text.len() as u32,
                text_end: button.text.len() as u32,
                link: None,
                button: Some(button.control),
            },
        )?;
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
        self.open_first_control = self.document.controls.len() as u16;
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
        let first_control = self.open_first_control;
        let control_count = (self.document.controls.len() as u16).saturating_sub(first_control);
        if run_count == 0
            && control_count == 0
            && kind != BlockKind::Rule
            && !matches!(kind, BlockKind::Control(_))
        {
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
                first_control,
                control_count,
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
        self.open_first_control = self.document.controls.len() as u16;
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
        if self.button.is_some() {
            return self.start_button_child(tag);
        }
        // Inside a select only its own structure means anything. A control
        // that cannot be inside one ends it, as HTML's parser does.
        if self.select.is_some() {
            match tag.name {
                "option" => return self.begin_option(tag.value, tag.selected, tag.disabled),
                "optgroup" => {
                    self.end_option()?;
                    if let Some(select) = self.select.as_mut() {
                        select.group_disabled = tag.disabled;
                    }
                    return Ok(());
                }
                "select" => return self.end_select(),
                "input" | "textarea" => self.end_select()?,
                _ => return Ok(()),
            }
        }
        if self.table.as_ref().is_some_and(|table| table.nested != 0) {
            match tag.name {
                "table" => {
                    if let Some(table) = self.table.as_mut() {
                        table.nested = table.nested.saturating_add(1);
                    }
                    return self.push_break();
                }
                "tr" => {
                    if let Some(table) = self.table.as_mut() {
                        table.nested_cell_in_row = false;
                    }
                    return self.push_break();
                }
                "td" | "th" => {
                    let separated = self
                        .table
                        .as_ref()
                        .is_some_and(|table| table.nested_cell_in_row);
                    if let Some(table) = self.table.as_mut() {
                        table.nested_cell_in_row = true;
                    }
                    return if separated {
                        self.push_text(" | ")
                    } else {
                        Ok(())
                    };
                }
                "thead" | "tbody" | "tfoot" | "caption" => return Ok(()),
                _ => {}
            }
        }
        match tag.name {
            "table" => {
                if let Some(table) = self.table.as_mut() {
                    table.nested = table.nested.saturating_add(1);
                    table.nested_cell_in_row = false;
                    return self.push_break();
                }
                self.begin_table(tag.border)?;
                return Ok(());
            }
            "thead" | "tbody" | "tfoot" if self.table.is_some() => {
                self.end_table_cell()?;
                if let Some(table) = self.table.as_mut() {
                    table.group = match tag.name {
                        "thead" => RowGroup::Head,
                        "tfoot" => RowGroup::Foot,
                        _ => RowGroup::Body,
                    };
                }
                return Ok(());
            }
            "tr" if self.table.is_some() => {
                self.begin_table_row()?;
                return Ok(());
            }
            "td" | "th" if self.table.is_some() => {
                self.begin_table_cell(tag.name == "th", tag.rowspan, tag.colspan)?;
                return Ok(());
            }
            // Paragraph-like markup inside a cell remains cell content. A
            // hard break retains the useful boundary without creating a
            // second block that would no longer belong to the cell.
            name if self.table.as_ref().is_some_and(|t| t.open_cell.is_some())
                && is_block(name) =>
            {
                return self.push_break();
            }
            _ => {}
        }
        let starts_block = tag.name == "hr"
            || tag.name == "pre"
            || tag.name == "li"
            || heading_level(tag.name).is_some()
            || is_block(tag.name);
        let pending_block =
            self.open_kind.is_some() && (self.run_open || self.open_kind == Some(BlockKind::Rule));
        let anchor_block =
            self.document.blocks.len() as u32 + u32::from(starts_block && pending_block);
        if let Some(id) = tag.id {
            self.add_anchor_at(id, anchor_block)?;
        }
        if tag.name == "a" {
            if let Some(name) = tag.anchor_name {
                self.add_anchor_at(name, anchor_block)?;
            }
        }
        match tag.name {
            "form" => {
                self.begin_form(tag.action, tag.method)?;
            }
            "input" => {
                let _ = self.push_control(
                    ControlElement::Input(tag.input_type),
                    tag.anchor_name,
                    tag.value,
                    tag.id,
                    tag.disabled,
                    tag.checked,
                )?;
                return Ok(());
            }
            "button" => {
                if self.button.is_none() {
                    let control = self.push_control(
                        ControlElement::Button(tag.input_type.unwrap_or("submit")),
                        tag.anchor_name,
                        tag.value,
                        tag.id,
                        tag.disabled,
                        false,
                    )?;
                    self.button = Some(ButtonBuild {
                        control,
                        text: String::new(),
                        pending_space: false,
                        style: 0,
                        inline: Vec::new(),
                        suppressed_controls: 0,
                    });
                }
                return Ok(());
            }
            "select" => {
                let control = self.push_control(
                    ControlElement::Select {
                        multiple: tag.multiple,
                    },
                    tag.anchor_name,
                    None,
                    tag.id,
                    tag.disabled,
                    false,
                )?;
                self.select = Some(SelectBuild {
                    control,
                    group_disabled: false,
                    option: None,
                });
                return Ok(());
            }
            "textarea" => {
                if self.textarea.is_none() {
                    let control = self.push_control(
                        ControlElement::Textarea(tag.rows),
                        tag.anchor_name,
                        None,
                        tag.id,
                        tag.disabled,
                        false,
                    )?;
                    self.textarea = Some(TextareaBuild {
                        control,
                        text: String::new(),
                        at_start: true,
                        after_cr: false,
                        overflow: false,
                    });
                }
                return Ok(());
            }
            "label" => self.begin_label(tag.label_for)?,
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
            "img" => self.push_image(tag.src, tag.alt, tag.width, tag.height)?,
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

    fn add_anchor_at(&mut self, name: &str, block_index: u32) -> Result<(), Error> {
        if name.is_empty() || name.len() > MAX_ANCHOR_NAME_BYTES {
            return Ok(());
        }
        if self.document.anchors.iter().any(|a| a.name == name) {
            return Ok(());
        }
        if self.document.anchors.len() >= MAX_ANCHORS
            || self
                .document
                .anchors
                .iter()
                .map(|a| a.name.capacity())
                .sum::<usize>()
                + name.len()
                > MAX_ANCHOR_BYTES
        {
            return Err(Error::TooManyAnchors);
        }
        memory::push(
            &mut self.document.anchors,
            Anchor {
                name: memory::string_from(name)?,
                text_offset: self.document.text.len() as u32,
                block_index,
            },
        )?;
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Error> {
        if self.select.is_some() {
            match name {
                "option" => return self.end_option(),
                "optgroup" => {
                    self.end_option()?;
                    if let Some(select) = self.select.as_mut() {
                        select.group_disabled = false;
                    }
                    return Ok(());
                }
                "select" => return self.end_select(),
                "form" => self.end_select()?,
                _ => return Ok(()),
            }
        }
        if self.button.is_some() {
            return self.end_button_child(name);
        }
        if self.table.as_ref().is_some_and(|table| table.nested != 0) {
            match name {
                "table" => {
                    if let Some(table) = self.table.as_mut() {
                        table.nested -= 1;
                        table.nested_cell_in_row = false;
                    }
                    return self.push_break();
                }
                "tr" => return self.push_break(),
                "thead" | "tbody" | "tfoot" | "td" | "th" | "caption" => return Ok(()),
                _ => {}
            }
        }
        match name {
            "table" if self.table.is_some() => {
                if self.table.as_ref().is_some_and(|table| table.nested != 0) {
                    if let Some(table) = self.table.as_mut() {
                        table.nested -= 1;
                    }
                    return self.push_break();
                }
                return self.end_table();
            }
            "td" | "th" if self.table.is_some() => return self.end_table_cell(),
            "tr" if self.table.is_some() => {
                self.end_table_cell()?;
                return Ok(());
            }
            "thead" | "tbody" | "tfoot" if self.table.is_some() => {
                self.end_table_cell()?;
                return Ok(());
            }
            name if self.table.as_ref().is_some_and(|t| t.open_cell.is_some())
                && is_block(name) =>
            {
                return Ok(());
            }
            _ => {}
        }
        match name {
            "button" => {
                self.end_button();
            }
            "textarea" => self.end_textarea(),
            "label" => self.end_label()?,
            "form" => {
                self.end_button();
                self.form = None;
                self.close_block()?;
            }
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

    fn end_button(&mut self) {
        if let Some(button) = self.button.take()
            && let Some(control) = self.document.controls.get_mut(button.control as usize)
        {
            control.display_label = button.text;
        }
    }

    fn begin_label(&mut self, target_id: Option<&str>) -> Result<(), Error> {
        self.end_label()?;
        let Some(target_id) = target_id.filter(|target| !target.is_empty()) else {
            return Ok(());
        };
        self.flush_pending_space()?;
        self.close_run()?;
        self.label = Some(LabelBuild {
            target_id: memory::string_from(target_id)?,
            text_start: self.document.text.len() as u32,
        });
        Ok(())
    }

    fn end_label(&mut self) -> Result<(), Error> {
        let Some(label) = self.label.take() else {
            return Ok(());
        };
        self.flush_pending_space()?;
        self.close_run()?;
        self.check_items(1)?;
        memory::push(
            &mut self.document.labels,
            Label {
                control: None,
                text_start: label.text_start,
                text_end: self.document.text.len() as u32,
                target_id: label.target_id,
            },
        )?;
        Ok(())
    }

    fn begin_form(&mut self, action: Option<&str>, method: Option<&str>) -> Result<(), Error> {
        if self.form.is_some() {
            return Ok(());
        }
        self.check_items(1)?;
        let action = self.document.url.resolve(action.unwrap_or(""))?;
        let method = match method.unwrap_or("get") {
            value if value.eq_ignore_ascii_case("get") => FormMethod::Get,
            value if value.eq_ignore_ascii_case("post") => FormMethod::Post,
            _ => FormMethod::Unsupported,
        };
        let index = self.document.forms.len() as u16;
        memory::push(
            &mut self.document.forms,
            Form {
                action,
                method,
                first_control: self.document.controls.len() as u32,
                control_count: 0,
            },
        )?;
        self.form = Some(index);
        Ok(())
    }

    fn push_control(
        &mut self,
        element: ControlElement<'_>,
        name: Option<&str>,
        value: Option<&str>,
        id: Option<&str>,
        disabled: bool,
        checked: bool,
    ) -> Result<u16, Error> {
        self.check_items(1)?;
        let (kind, rows) = match element {
            ControlElement::Input(input_type) => match input_type.unwrap_or("text") {
                value if value.eq_ignore_ascii_case("hidden") => (ControlKind::Hidden, 0),
                value if value.eq_ignore_ascii_case("submit") => (ControlKind::Submit, 0),
                value if value.eq_ignore_ascii_case("checkbox") => (ControlKind::Checkbox, 0),
                value if value.eq_ignore_ascii_case("radio") => (ControlKind::Radio, 0),
                // HTML's input invalid-value default is the Text state. This
                // small browser applies the same fallback to valid states
                // whose specialised UI it does not implement yet (for example
                // date), preserving editability and the submitted value.
                _ => (ControlKind::Text, 0),
            },
            ControlElement::Button(value) if value.eq_ignore_ascii_case("submit") => {
                (ControlKind::Submit, 0)
            }
            ControlElement::Button(_) => (ControlKind::Unsupported, 0),
            ControlElement::Textarea(rows) => (ControlKind::Textarea, textarea_rows(rows)),
            ControlElement::Select { .. } => (ControlKind::Select, 0),
        };
        let multiple = matches!(element, ControlElement::Select { multiple: true });
        let button_element = matches!(element, ControlElement::Button(_));
        let checked = checked && kind.is_checkable();
        let name = name.unwrap_or("");
        // Setting a radio button's checkedness clears the rest of its group,
        // so of several `checked` in one group the last one wins.
        if checked && kind == ControlKind::Radio && !name.is_empty() {
            for earlier in &mut self.document.controls {
                if earlier.kind == ControlKind::Radio
                    && earlier.form == self.form
                    && earlier.name == name
                {
                    earlier.checked = false;
                }
            }
        }
        // A checkable input without `value` submits the HTML default `on`.
        let value = match value {
            None if kind.is_checkable() => "on",
            value => value.unwrap_or(""),
        };
        if kind == ControlKind::Textarea && self.table.is_none() {
            self.start_block(BlockKind::Control(self.document.controls.len() as u16))?;
        } else if kind != ControlKind::Textarea {
            self.flush_pending_space()?;
            self.open_block_if_needed()?;
            self.close_run()?;
        }
        memory::push(
            &mut self.document.controls,
            Control {
                form: self.form,
                kind,
                name: memory::string_from(name)?,
                initial_value: memory::string_from(value)?,
                display_label: String::new(),
                button_element,
                first_button_run: self.document.button_runs.len() as u32,
                button_run_count: 0,
                element_id: memory::string_from(id.unwrap_or(""))?,
                disabled,
                checked,
                rows,
                value_overflow: false,
                first_option: self.document.options.len() as u32,
                option_count: 0,
                multiple,
                text_offset: self.document.text.len() as u32,
            },
        )?;
        let control_index = self.document.controls.len() as u16 - 1;
        if let Some(form) = self
            .form
            .and_then(|index| self.document.forms.get_mut(index as usize))
        {
            form.control_count = form.control_count.saturating_add(1);
        }
        if kind == ControlKind::Textarea && self.table.is_none() {
            self.close_block()?;
        }
        Ok(control_index)
    }

    fn begin_table(&mut self, border: Option<&str>) -> Result<(), Error> {
        self.close_block()?;
        self.check_items(1)?;
        let index = self.document.tables.len() as u16;
        memory::push(
            &mut self.document.tables,
            Table {
                first_cell: self.document.table_cells.len() as u32,
                cell_count: 0,
                first_row: self.document.table_rows.len() as u32,
                row_count: 0,
                column_count: 0,
                border: parse_border(border),
            },
        )?;
        self.table = Some(TableBuild {
            index,
            first_cell: self.document.table_cells.len() as u32,
            first_row: self.document.table_rows.len() as u32,
            row: None,
            group: RowGroup::Body,
            occupied: [0; crate::limits::MAX_TABLE_COLUMNS],
            open_cell: None,
            columns: 0,
            nested: 0,
            nested_cell_in_row: false,
        });
        Ok(())
    }

    fn begin_table_row(&mut self) -> Result<(), Error> {
        self.end_table_cell()?;
        let (index, number, group) = {
            let Some(table) = self.table.as_mut() else {
                return Ok(());
            };
            for occupied in &mut table.occupied {
                *occupied = occupied.saturating_sub(1);
            }
            let number = table.row.map_or(0, |row| row.saturating_add(1));
            table.row = Some(number);
            (table.index, number, table.group)
        };
        self.check_items(1)?;
        memory::push(
            &mut self.document.table_rows,
            TableRow {
                table: index,
                number,
                group,
            },
        )?;
        Ok(())
    }

    fn begin_table_cell(
        &mut self,
        header: bool,
        rowspan: Option<&str>,
        colspan: Option<&str>,
    ) -> Result<(), Error> {
        self.end_table_cell()?;
        if self.table.as_ref().is_some_and(|table| table.row.is_none()) {
            self.begin_table_row()?;
        }
        let rowspan = parse_span(rowspan);
        let mut colspan = parse_span(colspan);
        let table = self.table.as_mut().unwrap();
        let row = table.row.unwrap();
        let mut column = 0usize;
        loop {
            while column < table.occupied.len() && table.occupied[column] != 0 {
                column += 1;
            }
            if column >= table.occupied.len() {
                return Err(Error::TooManyItems);
            }
            colspan = colspan.min((table.occupied.len() - column) as u8);
            let end = column + colspan as usize;
            if table.occupied[column..end].iter().all(|&value| value == 0) {
                break;
            }
            column += 1;
        }
        for slot in &mut table.occupied[column..column + colspan as usize] {
            *slot = rowspan;
        }
        table.columns = table.columns.max((column + colspan as usize) as u8);
        table.open_cell = Some(OpenCell {
            row,
            column: column as u8,
            rowspan,
            colspan,
            header,
            text_start: self.document.text.len() as u32,
            first_image: self.document.images.len() as u16,
            first_control: self.document.controls.len() as u16,
        });
        self.open_kind = Some(BlockKind::Paragraph);
        self.open_first_run = self.document.runs.len() as u32;
        self.open_first_control = self.document.controls.len() as u16;
        self.at_block_start = true;
        self.pending_space = false;
        Ok(())
    }

    fn end_table_cell(&mut self) -> Result<(), Error> {
        let Some(cell) = self.table.as_mut().and_then(|table| table.open_cell.take()) else {
            return Ok(());
        };
        let block = self.document.blocks.len() as u32;
        self.close_run()?;
        let first_run = self.open_first_run;
        let run_count = self.document.runs.len() as u32 - first_run;
        self.open_kind = None;
        self.check_items(2)?;
        memory::push(
            &mut self.document.blocks,
            Block {
                kind: BlockKind::Paragraph,
                first_run,
                run_count,
                first_control: cell.first_control,
                control_count: (self.document.controls.len() as u16)
                    .saturating_sub(cell.first_control),
            },
        )?;
        let table_index = self.table.as_ref().unwrap().index;
        memory::push(
            &mut self.document.table_cells,
            TableCell {
                table: table_index,
                block,
                text_start: cell.text_start,
                text_end: self.document.text.len() as u32,
                first_image: cell.first_image,
                image_count: (self.document.images.len() as u16).saturating_sub(cell.first_image),
                first_control: cell.first_control,
                control_count: (self.document.controls.len() as u16)
                    .saturating_sub(cell.first_control),
                row: cell.row,
                column: cell.column,
                rowspan: cell.rowspan,
                colspan: cell.colspan,
                header: cell.header,
            },
        )?;
        self.at_block_start = true;
        self.pending_space = false;
        Ok(())
    }

    fn end_table(&mut self) -> Result<(), Error> {
        self.end_table_cell()?;
        let Some(table) = self.table.take() else {
            return Ok(());
        };
        let row_count = table.row.map_or(0, |row| row.saturating_add(1));
        self.document.tables[table.index as usize] = Table {
            first_cell: table.first_cell,
            cell_count: self.document.table_cells.len() as u32 - table.first_cell,
            first_row: table.first_row,
            row_count,
            column_count: table.columns,
            border: self.document.tables[table.index as usize].border,
        };
        Ok(())
    }

    /// `[alt text]`, or `[image]` when there is nothing to say.
    ///
    /// The brackets are there because the alternative -- dropping images
    /// entirely -- makes a page of thumbnails read as a page of nothing,
    /// and running the alt text into the prose around it makes a caption
    /// look like a sentence.
    fn push_image(
        &mut self,
        src: Option<&str>,
        alt: Option<&str>,
        width: Option<&str>,
        height: Option<&str>,
    ) -> Result<(), Error> {
        let alt = alt.unwrap_or("").trim();
        let separate_block = self.table.is_none();
        if separate_block {
            self.start_block(BlockKind::Paragraph)?;
        } else {
            self.close_run()?;
        }
        // An image is a word: separated from what is around it, whether or
        // not the markup put whitespace there.
        self.pending_space = true;
        self.flush_pending_space()?;
        let text_offset = self.document.text.len() as u32;
        let image_index = if self.document.images.len() < MAX_IMAGES {
            let source = src.and_then(|src| self.document.url.resolve(src).ok());
            let owned_alt = memory::string_from(alt)?;
            memory::push(
                &mut self.document.images,
                Image {
                    source,
                    alt: owned_alt,
                    width: image_dimension(width, MAX_IMAGE_WIDTH),
                    height: image_dimension(height, MAX_IMAGE_HEIGHT),
                    intrinsic_width: None,
                    intrinsic_height: None,
                    text_offset,
                    text_end: text_offset,
                    link: self.link,
                    button: None,
                },
            )?;
            Some(self.document.images.len() - 1)
        } else {
            None
        };
        self.push_character('[')?;
        if alt.is_empty() {
            self.push_text("image")?;
        } else {
            self.push_text(alt)?;
        }
        self.push_character(']')?;
        self.close_run()?;
        if let Some(index) = image_index {
            self.document.images[index].text_end = self.document.text.len() as u32;
        }
        if separate_block {
            self.close_block()?;
        }
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
        let owned = url.owned_bytes();
        if self.document.links.len() >= MAX_LINKS
            || self.link_url_bytes.saturating_add(owned) > MAX_LINK_URL_BYTES
        {
            // Keep the anchor's text and inline nesting, but stop making
            // further targets interactive. A large link farm should not
            // turn an otherwise readable page into an error page.
            return self.push_inline("a", 0, false);
        }
        let index = self.document.links.len() as u16;
        memory::push(&mut self.document.links, Link { url })?;
        self.link_url_bytes += owned;
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

/// `rows` as HTML reads it: a positive integer, otherwise the default. Kept
/// within `MAX_TEXTAREA_ROWS` so a page cannot make one box taller than the
/// screen.
fn textarea_rows(value: Option<&str>) -> u8 {
    value
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse::<u32>().ok())
        .filter(|&rows| rows != 0)
        .map_or(DEFAULT_TEXTAREA_ROWS, |rows| {
            rows.min(u32::from(MAX_TEXTAREA_ROWS)) as u8
        })
}

fn parse_span(value: Option<&str>) -> u8 {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&value| value != 0)
        .unwrap_or(1)
        .min(crate::limits::MAX_TABLE_SPAN) as u8
}

fn image_dimension(value: Option<&str>, maximum: u32) -> Option<u16> {
    let text = value?;
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value = text.parse::<u32>().ok()?;
    if value == 0 || value > maximum || value > u16::MAX as u32 {
        None
    } else {
        Some(value as u16)
    }
}

fn parse_border(value: Option<&str>) -> u8 {
    let Some(value) = value else { return 0 };
    if value.is_empty() {
        return 1;
    }
    value
        .parse::<usize>()
        .ok()
        .unwrap_or(0)
        .min(crate::limits::MAX_TABLE_BORDER) as u8
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
                    BlockKind::Control(index) => format!("control#{index}"),
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
    fn images_keep_accessible_fallback_text_in_separate_blocks() {
        let document = parse(
            b"<p>a <img src=\"x.png\" alt=\"a red square\"> b \
              <img src=\"y.png\"> c <img src=\"z.png\" alt=\"\"> d</p>",
        );
        assert_eq!(
            outline(&document),
            [
                "p|a",
                "p|[a red square]",
                "p|b",
                "p|[image]",
                "p|c",
                "p|[image]",
                "p|d"
            ]
        );
    }

    #[test]
    fn images_keep_resolved_sources_dimensions_and_link_identity() {
        let document = parse(
            b"<p><a href='/target'><img src='../pic.png' alt='a cat' width='320' height='0'></a>\
              <img src='bad scheme:x' width='12px' height='40'></p>",
        );
        assert_eq!(document.images().len(), 2);
        let first = &document.images()[0];
        assert_eq!(
            first.source.as_ref().unwrap().to_text().unwrap(),
            "http://example.com/a/pic.png"
        );
        assert_eq!(first.alt, "a cat");
        assert_eq!((first.width, first.height), (Some(320), None));
        assert_eq!(first.link, Some(0));
        let second = &document.images()[1];
        assert!(second.source.is_none());
        assert_eq!((second.width, second.height), (None, Some(40)));
        assert_eq!(second.link, None);
    }

    #[test]
    fn forms_keep_ordered_controls_defaults_and_disabled_state() {
        let document = parse(
            b"<form action='/find?old=1#x' method='GET'>\
              <input name=q value='first'>\
              <input type=hidden name=q value='second'>\
              <input type=submit name=go value=Search disabled>\
              <input type=date name=unsupported></form>",
        );
        assert_eq!(document.forms().len(), 1);
        let form = &document.forms()[0];
        assert_eq!(
            form.action.to_text().unwrap(),
            "http://example.com/find?old=1#x"
        );
        assert_eq!(form.method, FormMethod::Get);
        assert_eq!(form.first_control, 0);
        assert_eq!(form.control_count, 4);
        assert_eq!(
            document
                .controls()
                .iter()
                .map(|control| (
                    control.kind,
                    control.name.as_str(),
                    control.initial_value.as_str(),
                    control.disabled
                ))
                .collect::<Vec<_>>(),
            vec![
                (ControlKind::Text, "q", "first", false),
                (ControlKind::Hidden, "q", "second", false),
                (ControlKind::Submit, "go", "Search", true),
                (ControlKind::Text, "unsupported", "", false),
            ]
        );
    }

    #[test]
    fn button_keeps_submission_value_separate_from_visible_text() {
        let document = parse(
            b"<p>before</p><form><button name=mode value=advanced> Apply <strong>changes</strong> now </button></form><p>after</p>",
        );
        let control = &document.controls()[0];
        assert_eq!(control.kind, ControlKind::Submit);
        assert_eq!(control.name, "mode");
        assert_eq!(control.initial_value, "advanced");
        assert_eq!(control.display_label, "Apply changes now");
        assert_eq!(document.text(), "beforeafter");
        assert_eq!(document.blocks()[1].first_control, 0);
        assert_eq!(document.blocks()[1].control_count, 1);
        assert_eq!(document.button_runs(control).len(), 3);
        assert_eq!(document.button_runs(control)[1].style, STYLE_BOLD);
    }

    #[test]
    fn inline_controls_keep_offsets_spaces_and_control_only_blocks() {
        let document = parse(
            b"<p>before<input name=a>after <input name=b> tail</p><p><input name=c></p><p><input type=hidden name=h></p><textarea name=t>two lines</textarea>",
        );
        assert_eq!(document.text(), "beforeafter  tail");
        assert_eq!(document.controls()[0].text_offset, 6);
        assert_eq!(document.controls()[1].text_offset, 12);
        assert_eq!(document.blocks()[0].control_count, 2);
        assert_eq!(document.blocks()[1].control_count, 1);
        assert_eq!(document.blocks()[2].control_count, 1);
        assert!(matches!(document.blocks()[3].kind, BlockKind::Control(4)));
    }

    #[test]
    fn button_children_keep_styles_images_and_suppress_nested_controls() {
        let document = parse(
            b"<form><button name=go value=sent>plain <strong>bold <b>deep</b></strong><div><em> red</em></div><a href=/bad> link</a><img src=icon.png width=40 height=80 alt=icon><input name=bad><select name=bad2><option>leak</option></select> end</button></form>",
        );
        assert_eq!(document.controls().len(), 1);
        let control = &document.controls()[0];
        assert_eq!(control.initial_value, "sent");
        assert_eq!(control.display_label, "plain bold deep red link end");
        assert!(
            document
                .button_runs(control)
                .iter()
                .any(|run| run.style & STYLE_BOLD != 0)
        );
        assert!(
            document
                .button_runs(control)
                .iter()
                .any(|run| run.style & STYLE_ITALIC != 0)
        );
        assert!(document.links().is_empty());
        assert_eq!(document.images().len(), 1);
        assert_eq!(document.images()[0].button, Some(0));
        assert_eq!(document.images()[0].text_offset, 24);
    }

    #[test]
    fn explicit_label_resolves_a_control_declared_after_it() {
        let document = parse(b"<label for=q>Search term</label><input id=q name=q>");
        assert_eq!(document.labels().len(), 1);
        assert_eq!(document.labels()[0].control, Some(0));
        assert_eq!(
            &document.text()
                [document.labels()[0].text_start as usize..document.labels()[0].text_end as usize],
            "Search term"
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
    fn links_past_the_bound_keep_their_text_and_stop_being_clickable() {
        let mut markup = String::new();
        for index in 0..MAX_LINKS + 10 {
            markup.push_str(&format!("<a href=\"/t?n={index}\">l</a>"));
        }
        let document = parse(markup.as_bytes());
        assert_eq!(document.links().len(), MAX_LINKS);
        assert_eq!(document.text().len(), MAX_LINKS + 10);
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
    fn resolved_link_bytes_are_bounded_without_rejecting_the_page() {
        let base = Url::parse(&format!("http://example.com/{}/page", "x".repeat(1800))).unwrap();
        let mut parser = Parser::new(base).unwrap();
        let markup = (0..2000)
            .map(|index| format!("<a href=\"#n{index}\">l</a>"))
            .collect::<String>();
        parser.feed(markup.as_bytes()).unwrap();
        let document = parser.finish().unwrap();
        assert!(document.links().len() < 2000);
        assert_eq!(document.text().len(), 2000);
        assert!(
            document
                .links()
                .iter()
                .map(|link| link.url.owned_bytes())
                .sum::<usize>()
                <= MAX_LINK_URL_BYTES
        );
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
    fn a_large_page_remains_bounded_by_structural_limits() {
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
                    assert!(matches!(error, Error::TooManyItems | Error::TextTooLong));
                    return;
                }
            }
            peak = peak.max(parser.owned_bytes());
        }
        assert!(peak > 0);
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

    #[test]
    fn ids_and_legacy_names_are_anchors_and_first_duplicate_wins() {
        let document = parse(
            b"<h1 id='x'>one</h1><p id='x'>two</p><a name='old'>three</a><div name='no'>four</div>",
        );
        assert_eq!(document.anchors().len(), 2);
        assert_eq!(document.anchors()[0].name, "x");
        assert_eq!(document.anchors()[1].name, "old");
        assert_eq!(document.anchors()[0].block_index, 0);
    }

    #[test]
    fn tables_build_a_flat_grid_and_normalize_spans() {
        let document = parse(b"<table><tr><th rowspan='2'>a</th><td>b</td></tr><tr><td colspan='99'>c</td></tr></table>");
        assert_eq!(document.tables().len(), 1);
        assert_eq!(document.tables()[0].row_count, 2);
        assert_eq!(document.tables()[0].column_count, 32);
        let cells = document.table_cells();
        assert_eq!(
            (
                cells[0].row,
                cells[0].column,
                cells[0].rowspan,
                cells[0].colspan,
                cells[0].header
            ),
            (0, 0, 2, 1, true)
        );
        assert_eq!((cells[1].row, cells[1].column), (0, 1));
        assert_eq!(
            (cells[2].row, cells[2].column, cells[2].colspan),
            (1, 1, 31)
        );
    }

    #[test]
    fn nested_table_rows_stay_inside_the_outer_cell_as_compact_text() {
        let document = parse(
            b"<table><tr><td>before<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>after</td><td>right</td></tr></table>",
        );
        assert_eq!(document.tables().len(), 1);
        assert_eq!(document.table_cells().len(), 2);
        let block = &document.blocks()[document.table_cells()[0].block as usize];
        let runs = document.block_runs(block);
        let text = &document.text()
            [runs.first().unwrap().start as usize..runs.last().unwrap().end as usize];
        assert!(text.contains("a | b"), "{text:?}");
        assert!(text.contains("c | d"), "{text:?}");
        assert!(text.contains("after"), "{text:?}");
    }

    #[test]
    fn invalid_and_zero_spans_mean_one_and_empty_cells_survive() {
        let document = parse(b"<table><td rowspan='0'></td><td colspan='x'>x</td></table>");
        let cells = document.table_cells();
        assert_eq!(cells.len(), 2);
        assert_eq!((cells[0].rowspan, cells[0].colspan), (1, 1));
        assert_eq!(document.blocks()[cells[0].block as usize].run_count, 0);
    }

    #[test]
    fn table_border_is_absent_zero_present_and_bounded() {
        let document = parse(b"<table><td>a</table><table border=0><td>b</table><table border><td>c</table><table border=99><td>d</table>");
        assert_eq!(
            document
                .tables()
                .iter()
                .map(|table| table.border)
                .collect::<Vec<_>>(),
            [0, 0, 1, 4]
        );
    }
}
