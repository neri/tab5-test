//! Where every character goes: the document wrapped to a viewport width.
//!
//! The output is a list of lines, and a line is a y position, a height and a
//! few pieces of text with a style each. That is all the renderer needs, and
//! deliberately all it gets -- there is no bitmap of the page anywhere. A
//! 1280x720 screen is 1.8 MB per copy; a document scrolled through is many
//! screens; and the whole point of the memory plan is that neither of those
//! multiplies. Scrolling here changes one number and redraws the lines that
//! now intersect the viewport.
//!
//! ```text
//!  y=0    ┌──────────────────────────────┐  line 0  scale 3  "Simple"
//!  y=24   │                              │  (gap after a heading)
//!  y=32   │ This page has a title, one   │  line 1  scale 2
//!  y=48   │ heading and one link.        │  line 2
//!  y=64   │ The link goes to a target    │  line 3  piece 0 plain
//!         │                     ^^^^^^   │          piece 1 link 0
//!         └──────────────────────────────┘
//! ```
//!
//! Wrapping runs over the document's text directly rather than over its
//! runs, which it can because a block's runs are contiguous ranges of one
//! `String` -- so a word split across `<b>bo</b>ld` is still one word to the
//! line breaker, and the styles are applied afterwards by cutting each line
//! at the run boundaries it crosses.
//!
//! Widths are pixels, and they come from [`tab5_font::advance`] -- the same
//! function the renderer paints with. Half-width characters take 8 pixels,
//! full-width ones 16, and a combining mark none at all. Nothing here counts
//! characters and multiplies: a line of Japanese, a line of ASCII and a line
//! that mixes them all break where they actually reach the edge, and the
//! underline under a link, the background behind a selection and the hit
//! rectangle for a tap are the width the text was drawn at rather than an
//! estimate of it.

use alloc::vec::Vec;

use crate::document::{Block, BlockKind, Document, Marker, Run};
use crate::error::Error;
use crate::limits::MAX_LAYOUT_LINES;
use crate::memory;

/// One half-width advance, which is the unit indentation is measured in.
///
/// Indentation is the one measurement that is not text: a list marker column
/// and a `pre` block's inset are asked for in characters, and this is what a
/// character is worth when the text could be any width.
pub const CELL_WIDTH: u16 = 8;

/// The advance of `character` at `scale`, in pixels.
///
/// The single place widths come from. A combining mark returns 0: it is
/// painted over the character before it and adds nothing to the line.
pub fn advance(character: char, scale: u8) -> u16 {
    tab5_font::advance(character) as u16 * scale as u16
}

/// The advance of `text` at `scale`, in pixels.
pub fn text_width(text: &str, scale: u8) -> u16 {
    text.chars()
        .fold(0u16, |total, character| {
            total.saturating_add(advance(character, scale))
        })
}

/// The vertical part of the font's size, at scale 1.
///
/// Passed in rather than assumed: the glyph box's height and the space
/// between lines are the renderer's business. Widths are not -- those come
/// from [`advance`], so that what is measured is what gets drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Metrics {
    /// Height of the box one glyph is drawn into.
    pub glyph_height: u16,
    /// Extra space below each line, as a percentage of the glyph box.
    ///
    /// Separate from the glyph box because it is spacing and not size:
    /// making the glyph box taller would space the lines out and stretch
    /// nothing, but every other measurement -- where a rule sits, where an
    /// underline goes, how tall a focused link's highlight is -- is derived
    /// from one of the two, and they want different ones.
    ///
    /// A percentage rather than a fixed number of pixels so that a heading
    /// at scale 3 is spaced like body text at scale 2 rather than being
    /// crowded by a gap sized for smaller type.
    pub line_gap_percent: u16,
}

impl Metrics {
    /// The box one glyph is drawn into, at `scale`.
    pub fn glyph_box(&self, scale: u8) -> u16 {
        self.glyph_height * scale as u16
    }

    /// The height of a whole line: the glyph box plus the gap below it.
    ///
    /// The gap is entirely below rather than split above and below. Half of
    /// it above would push the first line of a page down from the top of
    /// the viewport for no reason a reader would recognise, and the gap
    /// between two lines is the same either way.
    pub fn line_box(&self, scale: u8) -> u16 {
        let glyph = self.glyph_box(scale);
        glyph + glyph * self.line_gap_percent / 100
    }
}

/// Body text and the smaller headings: the 16 pixel font at its own size.
pub const BODY_SCALE: u8 = 1;
/// `h1` and `h2`, at 32 pixels. Bitmaps scale by whole numbers only, so two
/// sizes is what there is -- and two is enough to see the shape of a page
/// from across a desk. `h3` and below stay body-sized and are drawn heavier.
pub const HEADING_SCALE: u8 = 2;

/// Extra space below every line, as a percentage of the glyph box.
///
/// The 16 pixel font fills its box, so lines set solid touch each other and
/// a page reads as a wall.
///
/// A fifth was tried first, on the reasoning that it is roughly what a book
/// uses. On the panel it was still tight, so this is a quarter: four pixels
/// at body scale and eight at heading scale, against glyph boxes of sixteen
/// and thirty-two. The number is a percentage rather than a pixel count so
/// that raising it stays a single decision instead of one per size.
pub const LINE_GAP_PERCENT: u16 = 25;

/// Cells of indentation per list level.
const INDENT_CELLS: u16 = 3;

/// Cells reserved to the left of a list item for its marker.
///
/// Fixed, and wide enough for `999.` -- the renderer right-aligns into it.
/// A number past that overflows into the indent rather than pushing the
/// text, because a list with a thousand items is not what the width should
/// be designed around.
pub const MARKER_CELLS: u16 = 4;

/// Cells of indentation for a `pre` block, so it reads as set apart.
const PRE_INDENT_CELLS: u16 = 1;

/// A horizontal rule's height, in line boxes at body scale.
const RULE_LINES: u16 = 1;

/// One drawn line of text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Line {
    /// Top edge, in pixels from the start of the document.
    pub y: u32,
    pub height: u16,
    /// Left edge, in pixels: the block's indentation.
    pub x: u16,
    pub scale: u8,
    /// Index into [`Layout::all_pieces`].
    pub first_piece: u32,
    pub piece_count: u16,
    /// Present on the first line of a list item only.
    pub marker: Option<Marker>,
    /// A heading. The renderer draws it heavier: `h1` and `h2` are already
    /// larger, but `h3` to `h6` are body-sized, and without this they are
    /// a paragraph with a gap above it.
    pub heading: bool,
    /// A horizontal rule. Has no pieces; the renderer draws a line.
    pub rule: bool,
}

/// A stretch of one line with a single style.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Piece {
    /// Offset from the line's `x`, in pixels.
    pub x: u16,
    /// Drawn width, in pixels. Kept so hit testing does not have to count
    /// characters again on every touch.
    pub width: u16,
    /// Byte range into [`Document::text`].
    pub start: u32,
    pub end: u32,
    pub style: u8,
    pub link: Option<u16>,
}

pub struct Layout {
    lines: Vec<Line>,
    pieces: Vec<Piece>,
    height: u32,
    width: u16,
    metrics: Metrics,
}

impl Layout {
    /// Wraps `document` to `width` pixels.
    ///
    /// Fails rather than truncating, the same as everything else that has a
    /// bound: a page half of which was laid out is a page that looks like it
    /// ends in the middle.
    pub fn build(document: &Document, width: u16, metrics: Metrics) -> Result<Layout, Error> {
        let mut layout = Layout {
            lines: Vec::new(),
            pieces: Vec::new(),
            height: 0,
            width,
            metrics,
        };
        let mut y = 0u32;
        for block in document.blocks() {
            y = layout.place_block(document, block, y)?;
        }
        layout.height = y;
        Ok(layout)
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn all_pieces(&self) -> &[Piece] {
        &self.pieces
    }

    /// The pieces of one line.
    pub fn pieces(&self, line: &Line) -> &[Piece] {
        let start = line.first_piece as usize;
        let end = start + line.piece_count as usize;
        self.pieces.get(start..end).unwrap_or(&[])
    }

    /// Total document height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// The lines intersecting the viewport `[top, top + height)`.
    ///
    /// A range rather than an iterator of lines because the renderer wants
    /// the indices: it redraws by index and remembers which range it drew
    /// last, so that a scroll of a few pixels does not repaint the screen.
    ///
    /// Binary search rather than a scan: this runs on every frame that
    /// scrolls, and a long document has thousands of lines.
    pub fn visible(&self, top: u32, height: u32) -> core::ops::Range<usize> {
        let bottom = top.saturating_add(height);
        // First line whose bottom edge is past the top of the viewport.
        let start = self
            .lines
            .partition_point(|line| line.y + line.height as u32 <= top);
        // First line that starts at or after the bottom of the viewport.
        let end = self.lines.partition_point(|line| line.y < bottom);
        start..end.max(start)
    }

    /// The link at a document-space point, for a tap or a click.
    pub fn hit(&self, x: u16, y: u32) -> Option<u16> {
        let index = self
            .lines
            .partition_point(|line| line.y + line.height as u32 <= y);
        let line = self.lines.get(index)?;
        if y < line.y {
            return None;
        }
        let offset = x.checked_sub(line.x)?;
        for piece in self.pieces(line) {
            if offset >= piece.x && offset < piece.x + piece.width {
                return piece.link;
            }
        }
        None
    }

    /// The first line a link appears on, for scrolling it into view.
    pub fn line_of_link(&self, link: u16) -> Option<usize> {
        for (index, line) in self.lines.iter().enumerate() {
            if self
                .pieces(line)
                .iter()
                .any(|piece| piece.link == Some(link))
            {
                return Some(index);
            }
        }
        None
    }

    /// Every link in the order it is laid out, which is the order `Tab`
    /// moves through.
    ///
    /// Document order, deduplicated: a link whose text wraps across two
    /// lines is one stop, not two.
    pub fn link_order(&self) -> Result<Vec<u16>, Error> {
        let mut order: Vec<u16> = Vec::new();
        for line in &self.lines {
            for piece in self.pieces(line) {
                let Some(link) = piece.link else {
                    continue;
                };
                if order.last() == Some(&link) {
                    continue;
                }
                if !order.contains(&link) {
                    memory::push(&mut order, link)?;
                }
            }
        }
        Ok(order)
    }

    pub fn owned_bytes(&self) -> usize {
        self.lines.capacity() * core::mem::size_of::<Line>()
            + self.pieces.capacity() * core::mem::size_of::<Piece>()
    }

    // --- building -----------------------------------------------------

    fn place_block(
        &mut self,
        document: &Document,
        block: &Block,
        top: u32,
    ) -> Result<u32, Error> {
        let (scale, indent_cells, gap_before, gap_after) = self.block_metrics(block.kind);
        let mut y = top + gap_before as u32;

        if block.kind == BlockKind::Rule {
            let height = self.metrics.line_box(BODY_SCALE) * RULE_LINES;
            self.push_line(Line {
                y,
                height,
                x: 0,
                scale: BODY_SCALE,
                first_piece: self.pieces.len() as u32,
                piece_count: 0,
                marker: None,
                heading: false,
                rule: true,
            })?;
            return Ok(y + height as u32 + gap_after as u32);
        }

        let runs = document.block_runs(block);
        let (Some(first), Some(last)) = (runs.first(), runs.last()) else {
            return Ok(top);
        };
        // Scaled with the text, not with the unscaled cell: an indent
        // measured in unscaled cells would be half the character widths it
        // claims to be, so a list item would sit two characters in where
        // four were asked for.
        let indent = indent_cells * CELL_WIDTH * scale as u16;
        let line_height = self.metrics.line_box(scale);
        // Pixels of text a line may hold. It can reach zero on a viewport
        // narrower than its own indentation; `next_line` still takes one
        // character in that case rather than looping forever.
        let budget = self.width.saturating_sub(indent);

        let span = first.start as usize..last.end as usize;
        let text = document.text().get(span.clone()).unwrap_or("");
        let preformatted = block.kind == BlockKind::Preformatted;
        let marker = match block.kind {
            BlockKind::ListItem { marker, .. } => Some(marker),
            _ => None,
        };
        let heading = matches!(block.kind, BlockKind::Heading(_));

        let mut cursor = 0usize;
        let mut first_line = true;
        loop {
            let (line_end, next) = next_line(text, cursor, budget, scale, preformatted);
            let start = span.start + cursor;
            let end = span.start + line_end;
            let first_piece = self.pieces.len() as u32;
            let piece_count = self.push_pieces(document.text(), runs, start, end, scale)?;
            self.push_line(Line {
                y,
                height: line_height,
                x: indent,
                scale,
                first_piece,
                piece_count,
                marker: if first_line { marker } else { None },
                heading,
                rule: false,
            })?;
            y += line_height as u32;
            first_line = false;
            if next >= text.len() {
                break;
            }
            cursor = next;
        }
        Ok(y + gap_after as u32)
    }

    /// Cuts `[start, end)` at the run boundaries it crosses.
    ///
    /// Each piece's width is the sum of its characters' advances, the same
    /// sum the renderer will walk when it draws them. That is what makes the
    /// underline under a link, the background behind a selection and the
    /// rectangle a tap is tested against agree with the pixels on screen.
    fn push_pieces(
        &mut self,
        text: &str,
        runs: &[Run],
        start: usize,
        end: usize,
        scale: u8,
    ) -> Result<u16, Error> {
        let mut count = 0u16;
        let mut x = 0u16;
        for run in runs {
            let from = (run.start as usize).max(start);
            let to = (run.end as usize).min(end);
            if from >= to {
                continue;
            }
            let width = text.get(from..to).map_or(0, |slice| text_width(slice, scale));
            memory::push(
                &mut self.pieces,
                Piece {
                    x,
                    width,
                    start: from as u32,
                    end: to as u32,
                    style: run.style,
                    link: run.link,
                },
            )?;
            x = x.saturating_add(width);
            count = count.saturating_add(1);
        }
        Ok(count)
    }

    fn push_line(&mut self, line: Line) -> Result<(), Error> {
        if self.lines.len() >= MAX_LAYOUT_LINES {
            return Err(Error::TooManyLines);
        }
        memory::push(&mut self.lines, line)?;
        Ok(())
    }

    /// Scale, indentation in cells, and the gaps above and below, in pixels.
    fn block_metrics(&self, kind: BlockKind) -> (u8, u16, u16, u16) {
        let half = self.metrics.line_box(BODY_SCALE) / 2;
        match kind {
            // A heading gets air above it and a little below, which is what
            // makes a page's structure visible without any other styling.
            BlockKind::Heading(1 | 2) => (HEADING_SCALE, 0, half * 2, half),
            BlockKind::Heading(_) => (BODY_SCALE, 0, half, half / 2),
            BlockKind::Paragraph => (BODY_SCALE, 0, 0, half),
            BlockKind::ListItem { depth, .. } => (
                BODY_SCALE,
                MARKER_CELLS + depth as u16 * INDENT_CELLS,
                0,
                0,
            ),
            BlockKind::Preformatted => (BODY_SCALE, PRE_INDENT_CELLS, half, half),
            BlockKind::Rule => (BODY_SCALE, 0, half, half),
        }
    }
}

/// Characters that may not begin a line.
///
/// Japanese has no spaces, so a line breaks wherever it reaches the edge --
/// which without this puts a closing bracket or a comma at the head of the
/// next line, where a reader trips over it. The minimum set: closing
/// brackets, the two full stops, and the small kana and the long vowel mark,
/// which belong to the character before them.
const NO_LINE_START: &str = concat!(
    "、。，．・：；？！\u{309B}\u{309C}",
    "）」』】〉》〕］｝＞",
    ")]},.:;?!",
    "ぁぃぅぇぉっゃゅょゎヵヶ",
    "ァィゥェォッャュョヮ",
    "ーゝゞヽヾ々〜～",
    "｡､･｣ﾞﾟ",
);

/// Characters that may not end a line: the opening halves of the pairs
/// above, which otherwise sit alone at the right edge.
const NO_LINE_END: &str = concat!("（「『【〈《〔［｛＜", "([{", "｢");

/// How far back a break may be pulled to satisfy the rules above.
///
/// A run of closing brackets longer than this is not worth dragging a whole
/// line for; the break stays where the width put it. Bounded rather than
/// unbounded so that this cannot walk back to the start of a line.
const MAX_KINSOKU_SHIFT: usize = 4;

/// Where the next line ends and where the one after it starts.
///
/// Returns `(end, next)`. They differ when the break falls on a space: the
/// space belongs to neither line.
///
/// Greedy and one pass, measured in pixels: characters are added until the
/// next one would not fit in `budget`. The alternative -- a proper line
/// breaker with penalties -- would look better on a page of prose and would
/// cost a pass per paragraph, on a panel with no kerning and no hyphenation.
///
/// Always consumes at least one character. A viewport narrower than a single
/// glyph would otherwise place nothing and be asked again from the same
/// place, forever.
fn next_line(
    text: &str,
    from: usize,
    budget: u16,
    scale: u8,
    preformatted: bool,
) -> (usize, usize) {
    if from >= text.len() {
        return (text.len(), text.len());
    }
    let rest = &text[from..];
    let mut used = 0u16;
    let mut placed = 0usize;
    let mut last_space: Option<usize> = None;
    let mut limit = text.len();
    for (offset, character) in rest.char_indices() {
        let absolute = from + offset;
        if character == '\n' {
            // A hard break: `<br>`, or a newline inside `pre`.
            return (absolute, absolute + 1);
        }
        let width = advance(character, scale);
        if placed > 0 && used.saturating_add(width) > budget {
            // One character past the line. If it is a space, the line is
            // exactly full and the space is the break -- taking it here
            // rather than rewinding keeps a word that ends on the boundary.
            if !preformatted && character == ' ' {
                return (absolute, absolute + 1);
            }
            limit = absolute;
            break;
        }
        if !preformatted && character == ' ' && absolute > from {
            last_space = Some(absolute);
        }
        used = used.saturating_add(width);
        placed += 1;
    }
    if limit >= text.len() {
        return (text.len(), text.len());
    }
    let (end, next) = match last_space {
        // Break at the last space that fits, dropping it.
        Some(space) => (space, space + 1),
        // One word longer than the whole line, or preformatted text: cut it
        // at the edge. Losing the end of a very long URL off the side of the
        // screen would be worse than breaking it.
        None => (limit, limit),
    };
    if preformatted {
        // `pre` is shown as written. Moving a break to tidy the punctuation
        // would misrepresent the source.
        return (end, next);
    }
    kinsoku(text, from, end, next)
}

/// Pulls a break back off a character that may not start or end a line.
///
/// Gives up rather than looping: if no allowed break exists within
/// [`MAX_KINSOKU_SHIFT`] characters, or pulling back would leave the line
/// empty, the width's own break stands. A tidier line is not worth a page
/// that never finishes laying out.
fn kinsoku(text: &str, from: usize, end: usize, next: usize) -> (usize, usize) {
    if !forbidden(text, from, end, next) {
        return (end, next);
    }
    let mut candidate = end;
    for _ in 0..MAX_KINSOKU_SHIFT {
        let Some(previous) = text.get(from..candidate).and_then(|s| s.chars().next_back()) else {
            break;
        };
        candidate -= previous.len_utf8();
        if candidate <= from {
            break;
        }
        // Pulled back to a character boundary, so nothing is dropped and the
        // next line starts exactly where this one ended.
        if !forbidden(text, from, candidate, candidate) {
            return (candidate, candidate);
        }
    }
    (end, next)
}

/// Whether breaking at `end`/`next` would strand a character on the wrong
/// side of the break.
fn forbidden(text: &str, from: usize, end: usize, next: usize) -> bool {
    let starts_next = text.get(next..).and_then(|rest| rest.chars().next());
    let ends_line = text
        .get(from..end)
        .and_then(|line| line.chars().next_back());
    starts_next.is_some_and(|character| NO_LINE_START.contains(character))
        || ends_line.is_some_and(|character| NO_LINE_END.contains(character))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use crate::document::Parser;
    use crate::url::Url;

    /// The panel's real numbers: the 16 pixel font, on a 1280-pixel-wide
    /// landscape screen.
    fn metrics() -> Metrics {
        Metrics {
            glyph_height: tab5_font::HEIGHT as u16,
            line_gap_percent: LINE_GAP_PERCENT,
        }
    }

    const WIDTH: u16 = 1280;
    /// Half-width characters that fit on one unindented line, which is what
    /// the ASCII wrapping tests below are measured against.
    const ASCII_COLUMNS: usize = (WIDTH / CELL_WIDTH) as usize;

    fn document(markup: &str) -> crate::document::Document {
        let url = Url::parse("http://example.com/a/page.html").unwrap();
        let mut parser = Parser::new(url).unwrap();
        parser.feed(markup.as_bytes()).unwrap();
        parser.finish().unwrap()
    }

    fn layout_of(markup: &str) -> (crate::document::Document, Layout) {
        let document = document(markup);
        let layout = Layout::build(&document, WIDTH, metrics()).unwrap();
        (document, layout)
    }

    /// Every line's text, in order.
    fn rendered(document: &crate::document::Document, layout: &Layout) -> Vec<String> {
        layout
            .lines()
            .iter()
            .map(|line| {
                if line.rule {
                    return "----".to_string();
                }
                let mut text = String::new();
                for piece in layout.pieces(line) {
                    text.push_str(
                        document
                            .text()
                            .get(piece.start as usize..piece.end as usize)
                            .unwrap_or(""),
                    );
                }
                text
            })
            .collect()
    }

    // --- wrapping ---------------------------------------------------------

    #[test]
    fn short_text_is_one_line() {
        let (document, layout) = layout_of("<p>one two three</p>");
        assert_eq!(rendered(&document, &layout), ["one two three"]);
    }

    #[test]
    fn long_text_wraps_at_word_boundaries() {
        let word = "abcdefghij";
        let markup = format!("<p>{}</p>", (0..30).map(|_| word).collect::<Vec<_>>().join(" "));
        let (document, layout) = layout_of(&markup);
        let lines = rendered(&document, &layout);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(
                line.chars().count() <= ASCII_COLUMNS,
                "{line:?} is {} wide",
                line.chars().count()
            );
            // No line starts or ends on the space that broke it.
            assert!(!line.starts_with(' '), "{line:?}");
            assert!(!line.ends_with(' '), "{line:?}");
        }
        // Nothing was lost or duplicated.
        assert_eq!(lines.join(" "), document.text());
    }

    #[test]
    fn a_word_longer_than_the_line_is_broken_at_the_edge() {
        // Two full lines and a remainder, whatever the width happens to be.
        let long = "x".repeat(ASCII_COLUMNS * 2 + 10);
        let (document, layout) = layout_of(&format!("<p>{long}</p>"));
        let lines = rendered(&document, &layout);
        assert!(lines.len() >= 3, "{lines:?}");
        assert_eq!(lines.concat(), long);
        assert_eq!(lines[0].chars().count(), ASCII_COLUMNS);
    }

    #[test]
    fn br_forces_a_break_without_ending_the_block() {
        let (document, layout) = layout_of("<p>one<br>two<br>three</p>");
        assert_eq!(rendered(&document, &layout), ["one", "two", "three"]);
        assert_eq!(layout.lines().len(), 3);
        // One block, so the lines are the same width and share an x.
        assert!(layout.lines().iter().all(|line| line.x == 0));
    }

    #[test]
    fn an_empty_line_from_two_breaks_still_takes_a_line() {
        let (document, layout) = layout_of("<p>a<br><br>b</p>");
        assert_eq!(rendered(&document, &layout), ["a", "", "b"]);
    }

    #[test]
    fn pre_keeps_its_spaces_and_wraps_only_at_the_edge() {
        let markup = format!("<pre>  a   b\n{}\n</pre>", "y".repeat(200));
        let (document, layout) = layout_of(&markup);
        let lines = rendered(&document, &layout);
        assert_eq!(lines[0], "  a   b");
        // The long line is broken at the width rather than not at all.
        assert!(lines[1].chars().count() <= ASCII_COLUMNS);
        assert!(lines[1].chars().all(|character| character == 'y'));
    }

    // --- mixed widths -----------------------------------------------------

    /// Half-width and full-width alternating, so a line that was measured by
    /// counting characters would be half again as wide as the viewport.
    const MIXED: &str = "aあiいuうeえoお";

    #[test]
    fn a_full_width_character_is_twice_a_half_width_one() {
        assert_eq!(advance('a', BODY_SCALE), 8);
        assert_eq!(advance('あ', BODY_SCALE), 16);
        assert_eq!(advance('a', HEADING_SCALE), 16);
        assert_eq!(advance('あ', HEADING_SCALE), 32);
        assert_eq!(text_width(MIXED, BODY_SCALE), 5 * 8 + 5 * 16);
    }

    #[test]
    fn combining_marks_add_no_width() {
        // The mark rides on the kana before it, so this is one full-width
        // character, not two.
        assert_eq!(text_width("か\u{3099}", BODY_SCALE), 16);
        assert_eq!(text_width("e\u{301}", BODY_SCALE), 8);
    }

    #[test]
    fn mixed_width_lines_stop_at_the_pixel_edge() {
        let markup = format!("<p>{}</p>", MIXED.repeat(60));
        let (document, layout) = layout_of(&markup);
        let lines = rendered(&document, &layout);
        assert!(lines.len() > 1);
        for (index, line) in lines.iter().enumerate() {
            let width = text_width(line, BODY_SCALE);
            assert!(width <= WIDTH, "{line:?} is {width} pixels wide");
            // Every line but the last is full, to within the one character
            // that did not fit. Counting characters instead of measuring them
            // would have overrun this by half again.
            if index + 1 < lines.len() {
                assert!(width + 16 > WIDTH, "{line:?} is only {width} pixels wide");
            }
        }
        assert_eq!(lines.concat(), document.text());
    }

    #[test]
    fn piece_width_is_what_the_renderer_will_draw() {
        let (document, layout) = layout_of("<p>ASCII と<b>日本語</b>の混在</p>");
        for line in layout.lines() {
            for piece in layout.pieces(line) {
                let text = document
                    .text()
                    .get(piece.start as usize..piece.end as usize)
                    .unwrap();
                assert_eq!(
                    piece.width,
                    text_width(text, line.scale),
                    "piece {text:?} measured differently from its own text"
                );
            }
        }
    }

    #[test]
    fn pieces_of_a_line_abut_without_gaps() {
        let (document, layout) = layout_of("<p>a<b>あ</b>i<code>い</code>u</p>");
        let line = &layout.lines()[0];
        let mut expected = 0;
        for piece in layout.pieces(line) {
            assert_eq!(piece.x, expected);
            expected += piece.width;
        }
        assert_eq!(expected, text_width(document.text(), line.scale));
    }

    #[test]
    fn a_link_after_full_width_text_is_hit_where_it_is_drawn() {
        let (_, layout) = layout_of("<p>日本語の<a href=\"/t\">リンク</a>です</p>");
        let line = &layout.lines()[0];
        let piece = layout
            .pieces(line)
            .iter()
            .find(|piece| piece.link.is_some())
            .expect("the link is a piece of its own");
        let link = piece.link.unwrap();
        // Both edges of the drawn rectangle, and just outside each of them.
        assert_eq!(layout.hit(line.x + piece.x, line.y), Some(link));
        assert_eq!(
            layout.hit(line.x + piece.x + piece.width - 1, line.y),
            Some(link)
        );
        assert_ne!(layout.hit(line.x + piece.x - 1, line.y), Some(link));
        assert_ne!(
            layout.hit(line.x + piece.x + piece.width, line.y),
            Some(link)
        );
    }

    // --- kinsoku ----------------------------------------------------------

    /// Lays out `text` in a viewport `columns` full-width characters wide.
    fn wrapped_at(text: &str, columns: u16) -> Vec<String> {
        let document = document(&format!("<p>{text}</p>"));
        let layout = Layout::build(&document, columns * 16, metrics()).unwrap();
        rendered(&document, &layout)
    }

    #[test]
    fn a_closing_bracket_does_not_start_a_line() {
        // Without the rule the break falls after the fourth character and
        // the closing bracket leads the second line.
        let lines = wrapped_at("あいう「え」おかき", 4);
        assert!(!lines[1].starts_with('」'), "{lines:?}");
        assert_eq!(lines.concat(), "あいう「え」おかき");
    }

    #[test]
    fn a_full_stop_does_not_start_a_line() {
        let lines = wrapped_at("あいうえ。おかきく", 4);
        assert!(!lines[1].starts_with('。'), "{lines:?}");
        assert_eq!(lines.concat(), "あいうえ。おかきく");
    }

    #[test]
    fn an_opening_bracket_does_not_end_a_line() {
        let lines = wrapped_at("あいう「えおかき", 4);
        assert!(!lines[0].ends_with('「'), "{lines:?}");
        assert_eq!(lines.concat(), "あいう「えおかき");
    }

    #[test]
    fn kinsoku_gives_up_before_it_empties_a_line() {
        // Every break is forbidden and the viewport is one character wide.
        // The rule has to yield: a line that placed nothing would be asked
        // for again from the same place, forever.
        let lines = wrapped_at("。。。。。", 1);
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert_eq!(lines.concat(), "。。。。。");
    }

    #[test]
    fn pre_is_never_retouched_by_kinsoku() {
        // `pre` is shown as written, so the break stays at the edge even
        // though it strands a full stop.
        let markup = format!("<pre>{}</pre>", "あいうえ。おかきく");
        let document = document(&markup);
        // Four full-width characters of text, past `pre`'s own indent.
        let width = 4 * 16 + PRE_INDENT_CELLS * CELL_WIDTH;
        let layout = Layout::build(&document, width, metrics()).unwrap();
        let lines = rendered(&document, &layout);
        assert!(lines[1].starts_with('。'), "{lines:?}");
    }

    #[test]
    fn a_viewport_narrower_than_one_glyph_still_makes_progress() {
        let document = document("<p>あいう</p>");
        let layout = Layout::build(&document, 4, metrics()).unwrap();
        assert_eq!(rendered(&document, &layout), ["あ", "い", "う"]);
    }

    // --- geometry ---------------------------------------------------------

    #[test]
    fn lines_are_stacked_downward_and_never_overlap() {
        let (_, layout) = layout_of(
            "<h1>Head</h1><p>One paragraph.</p><hr><ul><li>a</li><li>b</li></ul>\
             <pre>code</pre><p>Last.</p>",
        );
        let mut previous_bottom = 0u32;
        for line in layout.lines() {
            assert!(line.y >= previous_bottom, "{line:?} above {previous_bottom}");
            previous_bottom = line.y + line.height as u32;
        }
        assert_eq!(layout.height() >= previous_bottom, true);
    }

    #[test]
    fn headings_are_drawn_larger_than_body_text() {
        let (_, layout) = layout_of("<h1>Big</h1><h4>Small</h4><p>Body</p>");
        let scales: Vec<u8> = layout.lines().iter().map(|line| line.scale).collect();
        assert_eq!(scales, [HEADING_SCALE, BODY_SCALE, BODY_SCALE]);
        // A small heading is the same size as body text, so it is marked
        // for the renderer to draw heavier -- otherwise `h4` and `p` are
        // the same thing with a different gap above them.
        let headings: Vec<bool> = layout.lines().iter().map(|line| line.heading).collect();
        assert_eq!(headings, [true, true, false]);
    }

    #[test]
    fn list_items_are_indented_by_depth_and_carry_a_marker() {
        let (_, layout) = layout_of(
            "<ul><li>one</li><li>two<ol><li>inner</li></ol></li></ul>",
        );
        let lines = layout.lines();
        assert_eq!(lines[0].marker, Some(Marker::Bullet));
        assert_eq!(lines[2].marker, Some(Marker::Number(1)));
        // Deeper items sit further right.
        assert!(lines[2].x > lines[0].x, "{:?}", lines);
    }

    #[test]
    fn only_the_first_line_of_an_item_carries_its_marker() {
        let long = (0..40).map(|_| "word").collect::<Vec<_>>().join(" ");
        let (_, layout) = layout_of(&format!("<ul><li>{long}</li></ul>"));
        assert!(layout.lines().len() > 1);
        assert_eq!(layout.lines()[0].marker, Some(Marker::Bullet));
        for line in &layout.lines()[1..] {
            assert_eq!(line.marker, None);
        }
    }

    #[test]
    fn every_line_is_taller_than_the_glyphs_in_it() {
        let (_, layout) = layout_of("<h1>Big</h1><p>one</p><p>two</p>");
        let metrics = metrics();
        for line in layout.lines() {
            let glyph = metrics.glyph_box(line.scale);
            assert!(
                line.height > glyph,
                "{line:?} is {} tall for {glyph} of type",
                line.height
            );
        }
        // Two lines of one block, so the spacing between them is the line
        // box alone -- separate paragraphs would add the gap between blocks
        // on top of it.
        let (_, wrapped) = layout_of("<p>one<br>two</p>");
        let lines = wrapped.lines();
        let spacing = lines[1].y - lines[0].y;
        assert_eq!(spacing, metrics.line_box(BODY_SCALE) as u32);
        assert!(spacing > metrics.glyph_box(BODY_SCALE) as u32);
    }

    #[test]
    fn the_gap_scales_with_the_type() {
        let metrics = metrics();
        let body = metrics.line_box(BODY_SCALE) - metrics.glyph_box(BODY_SCALE);
        let heading = metrics.line_box(HEADING_SCALE) - metrics.glyph_box(HEADING_SCALE);
        assert!(heading > body, "{heading} should exceed {body}");
    }

    #[test]
    fn a_rule_is_a_line_with_no_pieces() {
        let (_, layout) = layout_of("<p>a</p><hr><p>b</p>");
        let rule = layout.lines()[1];
        assert!(rule.rule);
        assert_eq!(rule.piece_count, 0);
        assert!(rule.height > 0);
    }

    #[test]
    fn a_document_with_nothing_in_it_lays_out_to_nothing() {
        let (_, layout) = layout_of("<!-- empty -->");
        assert!(layout.lines().is_empty());
        assert_eq!(layout.height(), 0);
        assert_eq!(layout.visible(0, 720), 0..0);
    }

    // --- pieces and style -------------------------------------------------

    #[test]
    fn a_line_is_cut_at_every_style_change() {
        let (document, layout) = layout_of("<p>plain <b>bold</b> plain</p>");
        let line = layout.lines()[0];
        let pieces = layout.pieces(&line);
        assert_eq!(pieces.len(), 3);
        let texts: Vec<&str> = pieces
            .iter()
            .map(|piece| {
                &document.text()[piece.start as usize..piece.end as usize]
            })
            .collect();
        assert_eq!(texts, ["plain ", "bold", " plain"]);
        // Pieces are laid out left to right with no gaps.
        assert_eq!(pieces[0].x, 0);
        assert_eq!(pieces[1].x, pieces[0].width);
        assert_eq!(pieces[2].x, pieces[1].x + pieces[1].width);
    }

    #[test]
    fn a_word_split_across_runs_is_still_one_word() {
        // The wrapping sees "bold" as one word even though the markup cuts
        // it in half, which is the reason it runs over the text rather than
        // over the runs.
        let (document, layout) = layout_of("<p><b>bo</b>ld</p>");
        assert_eq!(rendered(&document, &layout), ["bold"]);
        assert_eq!(layout.pieces(&layout.lines()[0]).len(), 2);
    }

    #[test]
    fn a_style_run_spanning_a_line_break_appears_on_both_lines() {
        let long = (0..40).map(|_| "word").collect::<Vec<_>>().join(" ");
        let (_, layout) = layout_of(&format!("<p><b>{long}</b></p>"));
        assert!(layout.lines().len() > 1);
        for line in layout.lines() {
            for piece in layout.pieces(line) {
                assert_eq!(piece.style, crate::document::STYLE_BOLD);
            }
        }
    }

    // --- viewport ---------------------------------------------------------

    #[test]
    fn only_the_lines_crossing_the_viewport_are_visible() {
        let markup: String = (0..200).map(|index| format!("<p>Line {index}</p>")).collect();
        let (_, layout) = layout_of(&markup);
        let range = layout.visible(0, 720);
        assert_eq!(range.start, 0);
        assert!(range.end < layout.lines().len(), "{range:?}");
        // Every line in the range really does cross it, and the ones just
        // outside really do not.
        for index in range.clone() {
            let line = layout.lines()[index];
            assert!(line.y < 720);
            assert!(line.y + line.height as u32 > 0);
        }
        if range.end < layout.lines().len() {
            assert!(layout.lines()[range.end].y >= 720);
        }
    }

    #[test]
    fn a_viewport_past_the_end_shows_nothing() {
        let (_, layout) = layout_of("<p>short</p>");
        let range = layout.visible(100_000, 720);
        assert!(range.is_empty(), "{range:?}");
    }

    #[test]
    fn a_viewport_in_the_middle_starts_at_a_partly_visible_line() {
        let markup: String = (0..200).map(|index| format!("<p>Line {index}</p>")).collect();
        let (_, layout) = layout_of(&markup);
        // Land inside a line rather than on a boundary.
        let target = layout.lines()[50].y + 3;
        let range = layout.visible(target, 720);
        assert_eq!(range.start, 50);
    }

    // --- links ------------------------------------------------------------

    #[test]
    fn a_tap_on_a_link_finds_it_and_a_tap_beside_it_does_not() {
        let (_, layout) = layout_of("<p>before <a href=\"/x\">link</a> after</p>");
        let line = layout.lines()[0];
        let pieces = layout.pieces(&line);
        let link_piece = pieces.iter().find(|piece| piece.link.is_some()).unwrap();
        let inside = line.x + link_piece.x + link_piece.width / 2;
        assert_eq!(layout.hit(inside, line.y + 1), Some(0));
        // Just before the link is the plain text run.
        assert_eq!(layout.hit(line.x + 1, line.y + 1), None);
        // Past the end of the line is nothing at all.
        assert_eq!(layout.hit(WIDTH - 1, line.y + 1), None);
        // Below the document is nothing.
        assert_eq!(layout.hit(inside, layout.height() + 100), None);
    }

    #[test]
    fn links_are_ordered_the_way_they_are_read() {
        let (_, layout) = layout_of(
            "<p><a href=\"/a\">a</a> <a href=\"/b\">b</a></p>\
             <p><a href=\"/c\">c</a></p>",
        );
        assert_eq!(layout.link_order().unwrap(), [0, 1, 2]);
        assert_eq!(layout.line_of_link(2), Some(1));
    }

    #[test]
    fn a_link_that_wraps_is_one_stop_not_two() {
        let long = (0..40).map(|_| "word").collect::<Vec<_>>().join(" ");
        let (_, layout) = layout_of(&format!("<p><a href=\"/x\">{long}</a></p>"));
        assert!(layout.lines().len() > 1);
        assert_eq!(layout.link_order().unwrap(), [0]);
        assert_eq!(layout.line_of_link(0), Some(0));
    }

    // --- limits and memory ------------------------------------------------

    #[test]
    fn too_many_lines_is_an_error() {
        let markup: String = (0..MAX_LAYOUT_LINES + 16).map(|_| "x<br>").collect();
        let document = document(&markup);
        assert_eq!(
            Layout::build(&document, WIDTH, metrics()),
            Err(Error::TooManyLines)
        );
    }

    #[test]
    fn a_narrow_viewport_still_terminates() {
        // Narrower than one indented character, which is the case that
        // would divide by zero or loop for ever if `columns` were allowed
        // to reach zero.
        let document = document("<ul><li><ul><li>deep text here</li></ul></li></ul>");
        let layout = Layout::build(&document, 8, metrics()).unwrap();
        assert!(!layout.lines().is_empty());
        assert!(layout.height() > 0);
    }

    #[test]
    fn a_long_page_lays_out_inside_the_budget() {
        use crate::limits::MAX_BROWSER_OWNED_BYTES;
        let markup: String = (0..3000)
            .map(|index| format!("<p>Paragraph {index} with a few words in it.</p>"))
            .collect();
        let (document, layout) = layout_of(&markup);
        let total = document.stats().owned_bytes + layout.owned_bytes();
        assert!(total < MAX_BROWSER_OWNED_BYTES, "{total}");
    }
}

impl PartialEq for Layout {
    fn eq(&self, other: &Self) -> bool {
        self.lines == other.lines && self.pieces == other.pieces
    }
}

impl core::fmt::Debug for Layout {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Layout")
            .field("lines", &self.lines.len())
            .field("pieces", &self.pieces.len())
            .field("height", &self.height)
            .finish()
    }
}
