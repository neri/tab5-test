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
//! Widths are pixels, and they come from [`tab5_ui_font::text_width`] -- the same
//! function the renderer paints with. Half-width characters take 8 pixels,
//! full-width ones 16, and a combining mark none at all. Nothing here counts
//! characters and multiplies: a line of Japanese, a line of ASCII and a line
//! that mixes them all break where they actually reach the edge, and the
//! underline under a link, the background behind a selection and the hit
//! rectangle for a tap are the width the text was drawn at rather than an
//! estimate of it.

use alloc::vec::Vec;

use crate::document::{
    Block, BlockKind, Control, ControlKind, Document, Marker, Run, Table, TableCell,
};
use crate::error::Error;
use crate::limits::{MAX_LAYOUT_LINES, PLACEHOLDER_IMAGE_HEIGHT, PLACEHOLDER_IMAGE_WIDTH};
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
    let style = ui_style(scale, 0);
    if tab5_ui_font::is_english_latin(character) {
        tab5_ui_font::glyph(style, character).unwrap().advance as u16
    } else if let Some(glyph) = tab5_ui_font::japanese_glyph(character) {
        glyph.advance as u16 * tab5_ui_font::japanese_scale(style) as u16
    } else {
        tab5_font::advance(character) as u16 * style.fallback_scale() as u16
    }
}

/// The advance of `text` at `scale`, in pixels.
pub fn text_width(text: &str, scale: u8) -> u16 {
    text_width_styled(text, scale, 0)
}

pub fn text_width_styled(text: &str, scale: u8, run_style: u8) -> u16 {
    tab5_ui_font::text_width(text, ui_style(scale, run_style)).min(u16::MAX as usize) as u16
}

fn ui_style(scale: u8, run_style: u8) -> tab5_ui_font::TextStyle {
    let face = if run_style & crate::document::STYLE_CODE != 0 {
        tab5_ui_font::Face::Mono
    } else {
        tab5_ui_font::Face::Sans
    };
    tab5_ui_font::TextStyle::new(face, if scale >= HEADING_SCALE { 32 } else { 16 })
}

fn fit_columns(widths: &mut [u16], available: u16) {
    if widths.is_empty() {
        return;
    }
    let floor = CELL_WIDTH + TABLE_CELL_PADDING * 2;
    let minimum = floor as u32 * widths.len() as u32;
    if minimum >= available as u32 {
        let base = available / widths.len() as u16;
        let mut remainder = available % widths.len() as u16;
        for width in widths {
            *width = base + u16::from(remainder != 0);
            remainder = remainder.saturating_sub(1);
        }
        return;
    }
    let total: u32 = widths.iter().map(|&width| width as u32).sum();
    if total <= available as u32 {
        return;
    }
    let flexible = total - minimum;
    let room = available as u32 - minimum;
    let mut used = 0u16;
    for width in widths.iter_mut() {
        let extra = (*width as u32 - floor as u32) * room / flexible;
        *width = floor + extra as u16;
        used = used.saturating_add(*width);
    }
    let mut remainder = available.saturating_sub(used);
    for width in widths {
        if remainder == 0 {
            break;
        }
        *width += 1;
        remainder -= 1;
    }
}

/// Width a cell asks for before the table is fitted to the viewport.
///
/// Runs split on style boundaries, while newlines can occur inside a run.
/// Accumulate styled pieces across a logical line and retain the widest
/// line; adding every run wholesale would make `<br>` widen a column as if
/// the lines had been written side by side.
fn preferred_cell_width(document: &Document, block: &Block) -> u16 {
    // Attributes/fallback choose the desired image size first. Columns are
    // then fitted once, so an image can use spare table width without making
    // column measurement depend on the result of image placement.
    let mut widest = 0u16;
    if let Some((start, end)) = block_text_span(document, block) {
        for image in document.images().iter().filter(|image| {
            image.button.is_none() && image.text_offset >= start && image.text_end <= end
        }) {
            widest = widest.max(image_size(image, u16::MAX).0);
        }
    }
    let mut line = 0u16;
    for run in document.block_runs(block) {
        let text = document.run_text(run);
        for segment in text.split_inclusive('\n') {
            let (content, ended) = match segment.strip_suffix('\n') {
                Some(content) => (content, true),
                None => (segment, false),
            };
            line = line.saturating_add(text_width_styled(content, BODY_SCALE, run.style));
            if ended {
                widest = widest.max(line);
                line = 0;
            }
        }
    }
    widest.max(line).saturating_add(TABLE_CELL_PADDING * 2)
}

/// A control box's height: one text row with padding, or a textarea's rows.
fn control_box_height(control: Option<&Control>, line_box: u16) -> u16 {
    let rows = control
        .filter(|control| control.kind == ControlKind::Textarea)
        .map_or(1, |control| u16::from(control.rows.max(1)));
    line_box.saturating_mul(rows).saturating_add(8)
}

fn control_box_width(document: &Document, control_id: u16, control: &Control, height: u16) -> u16 {
    if control.kind.is_checkable() {
        return height;
    }
    if control.kind != ControlKind::Submit {
        return 320;
    }
    let text = if control.button_element {
        &control.display_label
    } else if control.display_label.is_empty() {
        if control.initial_value.is_empty() {
            "Submit"
        } else {
            &control.initial_value
        }
    } else {
        &control.display_label
    };
    let text_width = if control.button_run_count == 0 {
        text_width(text, BODY_SCALE)
    } else {
        document
            .button_runs(control)
            .iter()
            .map(|run| {
                text.get(run.start as usize..run.end as usize)
                    .map_or(0, |part| text_width_styled(part, BODY_SCALE, run.style))
            })
            .fold(0u16, u16::saturating_add)
    };
    let image_width = document
        .images()
        .iter()
        .filter(|image| image.button == Some(control_id))
        .map(|image| button_image_size(image, height.saturating_sub(8)).0)
        .fold(0u16, u16::saturating_add);
    text_width
        .saturating_add(image_width)
        .saturating_add(12)
        .clamp(80, 320)
}

fn button_text_width_to(document: &Document, control: &Control, end: usize) -> u16 {
    document
        .button_runs(control)
        .iter()
        .map(|run| {
            let from = run.start as usize;
            let to = (run.end as usize).min(end);
            if from >= to {
                0
            } else {
                control
                    .display_label
                    .get(from..to)
                    .map_or(0, |text| text_width_styled(text, BODY_SCALE, run.style))
            }
        })
        .fold(0u16, u16::saturating_add)
}

fn button_image_size(image: &crate::document::Image, line_box: u16) -> (u16, u16) {
    let (mut width, mut height) = image_size(image, 308);
    if height > line_box {
        width = ((u32::from(width) * u32::from(line_box)) / u32::from(height)).max(1) as u16;
        height = line_box.max(1);
    }
    (width.min(308), height)
}

fn preferred_table_cell_width(document: &Document, cell: &TableCell, block: &Block) -> u16 {
    let text = preferred_cell_width(document, block);
    let start = cell.first_control as usize;
    let end = start + cell.control_count as usize;
    let controls = document.controls()[start..end]
        .iter()
        .enumerate()
        .filter(|(_, control)| control.kind != ControlKind::Hidden)
        .map(|(offset, control)| {
            control_box_width(
                document,
                cell.first_control.saturating_add(offset as u16),
                control,
                28,
            )
        })
        .fold(0u16, u16::saturating_add);
    text.saturating_add(controls)
}

fn cell_has_standalone_items(document: &Document, cell: &TableCell) -> bool {
    let start = cell.first_image as usize;
    let end = start.saturating_add(cell.image_count as usize);
    let standalone_image = document.images()[start..end]
        .iter()
        .any(|image| image.button.is_none());
    let control_start = cell.first_control as usize;
    let control_end = control_start.saturating_add(cell.control_count as usize);
    standalone_image
        || document.controls()[control_start..control_end]
            .iter()
            .any(|control| control.kind == ControlKind::Textarea)
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
    /// Font role, independent of colour/emphasis style. `pre` sets this even
    /// though it is not an inline `code` element.
    pub mono: bool,
    pub link: Option<u16>,
}

pub const TABLE_CELL_PADDING: u16 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CellBox {
    pub x: u16,
    pub y: u32,
    pub width: u16,
    pub height: u32,
    pub header: bool,
    pub border: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImageBox {
    pub image: u16,
    pub x: u16,
    pub y: u32,
    pub width: u16,
    pub height: u16,
    pub link: Option<u16>,
    pub button: Option<u16>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ControlBox {
    pub control: u16,
    pub x: u16,
    pub y: u32,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy)]
struct PendingControl {
    control: u16,
    x: u16,
    width: u16,
    height: u16,
}

/// A viewport anchor that survives rebuilding layout with different image
/// dimensions. `offset` keeps the same pixel within the anchored object at
/// the top of the viewport.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReadingPosition {
    target: ReadingTarget,
    offset: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReadingTarget {
    Text(u32),
    Image(u16),
}

pub struct Layout {
    lines: Vec<Line>,
    pieces: Vec<Piece>,
    height: u32,
    width: u16,
    metrics: Metrics,
    anchors: Vec<(u16, usize)>,
    cells: Vec<CellBox>,
    images: Vec<ImageBox>,
    controls: Vec<ControlBox>,
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
            anchors: Vec::new(),
            cells: Vec::new(),
            images: Vec::new(),
            controls: Vec::new(),
        };
        let mut y = 0u32;
        let mut laid_tables = 0usize;
        for (block_index, block) in document.blocks().iter().enumerate() {
            if let Some(cell) = document
                .table_cells()
                .iter()
                .find(|cell| cell.block as usize == block_index)
            {
                let table_index = cell.table as usize;
                if table_index >= laid_tables {
                    y = layout.place_table(document, &document.tables()[table_index], y)?;
                    laid_tables = table_index + 1;
                }
                continue;
            }
            let first_line = layout.lines.len();
            y = layout.place_block(document, block, y)?;
            for (anchor_index, anchor) in document.anchors().iter().enumerate() {
                if anchor.block_index as usize == block_index
                    && !layout.anchors.iter().any(|x| x.0 as usize == anchor_index)
                {
                    let line = layout.lines[first_line..]
                        .iter()
                        .position(|line| {
                            layout.pieces(line).iter().any(|p| {
                                p.start <= anchor.text_offset && anchor.text_offset < p.end
                            })
                        })
                        .map(|n| first_line + n)
                        .unwrap_or(first_line);
                    memory::push(&mut layout.anchors, (anchor_index as u16, line))?;
                }
            }
        }
        layout.lines.sort_by_key(|line| line.y);
        layout.anchors.clear();
        for (anchor_index, anchor) in document.anchors().iter().enumerate() {
            let line = layout
                .lines
                .iter()
                .position(|line| {
                    layout.pieces(line).iter().any(|piece| {
                        piece.start <= anchor.text_offset && anchor.text_offset < piece.end
                    })
                })
                .unwrap_or_else(|| layout.lines.len().saturating_sub(1));
            memory::push(&mut layout.anchors, (anchor_index as u16, line))?;
        }
        let last = layout.lines.len().saturating_sub(1);
        for i in 0..document.anchors().len() {
            if !layout.anchors.iter().any(|x| x.0 as usize == i) {
                memory::push(&mut layout.anchors, (i as u16, last))?;
            }
        }
        layout.place_all_button_images(document)?;
        layout.height = y;
        Ok(layout)
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn all_pieces(&self) -> &[Piece] {
        &self.pieces
    }
    pub fn cells(&self) -> &[CellBox] {
        &self.cells
    }

    pub fn images(&self) -> &[ImageBox] {
        &self.images
    }

    pub fn controls(&self) -> &[ControlBox] {
        &self.controls
    }

    pub fn control_at(&self, x: u16, y: u32) -> Option<u16> {
        self.controls.iter().find_map(|control| {
            (x >= control.x
                && x < control.x.saturating_add(control.width)
                && y >= control.y
                && y < control.y.saturating_add(control.height as u32))
            .then_some(control.control)
        })
    }

    /// The control named by an explicit `label for=...` under this point.
    pub fn label_control_at(&self, document: &Document, x: u16, y: u32) -> Option<u16> {
        let end = self.lines.partition_point(|line| line.y <= y);
        for line in self.lines[..end].iter().rev() {
            if line.y + line.height as u32 <= y {
                continue;
            }
            let Some(offset) = x.checked_sub(line.x) else {
                continue;
            };
            for piece in self.pieces(line) {
                if offset < piece.x || offset >= piece.x.saturating_add(piece.width) {
                    continue;
                }
                return document.labels().iter().find_map(|label| {
                    (piece.start < label.text_end && piece.end > label.text_start)
                        .then_some(label.control)
                        .flatten()
                });
            }
        }
        None
    }

    /// Captures the closest laid-out object at or above `y` by logical ID,
    /// rather than retaining an absolute pixel coordinate.
    pub fn reading_position(&self, y: u32) -> ReadingPosition {
        let mut target = ReadingTarget::Text(0);
        let mut base_y = 0;
        for line in &self.lines {
            if line.y > y {
                break;
            }
            if let Some(piece) = self.pieces(line).first() {
                if line.y >= base_y {
                    target = ReadingTarget::Text(piece.start);
                    base_y = line.y;
                }
            }
        }
        for image in &self.images {
            if image.y <= y && image.y >= base_y {
                target = ReadingTarget::Image(image.image);
                base_y = image.y;
            }
        }
        ReadingPosition {
            target,
            offset: y.saturating_sub(base_y),
        }
    }

    /// Resolves a logical viewport anchor after layout has been rebuilt.
    pub fn y_of_reading_position(&self, position: ReadingPosition) -> u32 {
        let base = match position.target {
            ReadingTarget::Image(index) => self
                .images
                .iter()
                .find(|image| image.image == index)
                .map(|image| image.y),
            ReadingTarget::Text(offset) => self
                .lines
                .iter()
                .find(|line| {
                    self.pieces(line)
                        .iter()
                        .any(|piece| piece.start <= offset && offset < piece.end)
                })
                .map(|line| line.y),
        };
        base.unwrap_or(0).saturating_add(position.offset)
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
        let mut start = self.lines.partition_point(|line| line.y < top);
        while start > 0 && self.lines[start - 1].y + self.lines[start - 1].height as u32 > top {
            start -= 1;
        }
        if let Some(line) = self.lines.get(start) {
            start = self.lines.partition_point(|candidate| candidate.y < line.y);
        }
        // First line that starts at or after the bottom of the viewport.
        let end = self.lines.partition_point(|line| line.y < bottom);
        start..end.max(start)
    }

    /// The link at a document-space point, for a tap or a click.
    pub fn hit(&self, x: u16, y: u32) -> Option<u16> {
        for image in &self.images {
            if x >= image.x
                && x < image.x.saturating_add(image.width)
                && y >= image.y
                && y < image.y.saturating_add(image.height as u32)
                && image.link.is_some()
            {
                return image.link;
            }
        }
        // A table has several lines at the same y, one per cell.  The old
        // single-line lookup was valid only while lines occupied the whole
        // content width.
        let end = self.lines.partition_point(|line| line.y <= y);
        for line in self.lines[..end].iter().rev() {
            if line.y + line.height as u32 <= y {
                continue;
            }
            let Some(offset) = x.checked_sub(line.x) else {
                continue;
            };
            for piece in self.pieces(line) {
                if offset >= piece.x && offset < piece.x + piece.width {
                    if piece.link.is_some() {
                        return piece.link;
                    }
                }
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
        if let Some(image) = self.images.iter().find(|image| image.link == Some(link)) {
            return Some(
                self.lines
                    .partition_point(|line| line.y < image.y)
                    .min(self.lines.len().saturating_sub(1)),
            );
        }
        None
    }

    pub fn position_of_link(&self, link: u16) -> Option<(u32, u16)> {
        for line in &self.lines {
            if let Some(piece) = self
                .pieces(line)
                .iter()
                .find(|piece| piece.link == Some(link))
            {
                return Some((line.y, line.x.saturating_add(piece.x)));
            }
        }
        self.images
            .iter()
            .find(|image| image.link == Some(link))
            .map(|image| (image.y, image.x))
    }

    /// Every link in the order it is laid out, which is the order `Tab`
    /// moves through.
    ///
    /// Document order, deduplicated: a link whose text wraps across two
    /// lines is one stop, not two.
    pub fn link_order(&self) -> Result<Vec<u16>, Error> {
        let mut positions: Vec<(u32, u16, u16)> = Vec::new();
        for line in &self.lines {
            for piece in self.pieces(line) {
                if let Some(link) = piece.link {
                    memory::push(
                        &mut positions,
                        (line.y, line.x.saturating_add(piece.x), link),
                    )?;
                }
            }
        }
        for image in &self.images {
            if let Some(link) = image.link {
                memory::push(&mut positions, (image.y, image.x, link))?;
            }
        }
        positions.sort_unstable();
        let mut order: Vec<u16> = Vec::new();
        for (_, _, link) in positions {
            if !order.contains(&link) {
                memory::push(&mut order, link)?;
            }
        }
        Ok(order)
    }

    pub fn owned_bytes(&self) -> usize {
        self.lines.capacity() * core::mem::size_of::<Line>()
            + self.pieces.capacity() * core::mem::size_of::<Piece>()
            + self.anchors.capacity() * core::mem::size_of::<(u16, usize)>()
            + self.cells.capacity() * core::mem::size_of::<CellBox>()
            + self.images.capacity() * core::mem::size_of::<ImageBox>()
            + self.controls.capacity() * core::mem::size_of::<ControlBox>()
    }

    pub fn line_of_anchor(&self, document: &Document, name: &str) -> Option<usize> {
        let index = document.anchors().iter().position(|a| a.name == name)?;
        self.anchors
            .iter()
            .find(|entry| entry.0 as usize == index)
            .map(|entry| entry.1)
    }

    // --- building -----------------------------------------------------

    fn place_block(&mut self, document: &Document, block: &Block, top: u32) -> Result<u32, Error> {
        let (scale, indent_cells, gap_before, gap_after) = self.block_metrics(block.kind);
        let mut y = top + gap_before as u32;

        if let BlockKind::Control(control) = block.kind {
            let height = control_box_height(
                document.controls().get(control as usize),
                self.metrics.line_box(BODY_SCALE),
            );
            let width = if document
                .controls()
                .get(control as usize)
                .is_some_and(|item| item.kind.is_checkable())
            {
                height.min(self.width).max(1)
            } else {
                self.width.min(320).max(80)
            };
            memory::push(
                &mut self.controls,
                ControlBox {
                    control,
                    x: 0,
                    y,
                    width,
                    height,
                },
            )?;
            return Ok(y + height as u32 + gap_after as u32);
        }

        if let Some((index, image)) = image_for_block(document, block) {
            let (width, height) = image_size(image, self.width);
            memory::push(
                &mut self.images,
                ImageBox {
                    image: index as u16,
                    x: 0,
                    y,
                    width,
                    height,
                    link: image.link,
                    button: None,
                },
            )?;
            return Ok(y
                .saturating_add(height as u32)
                .saturating_add(gap_after as u32));
        }

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
        if block.control_count != 0 {
            return self.place_inline_block(document, block, y, gap_after);
        }
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
            let (line_end, next) =
                next_line(text, cursor, budget, scale, preformatted, span.start, runs);
            let start = span.start + cursor;
            let end = span.start + line_end;
            let first_piece = self.pieces.len() as u32;
            let piece_count =
                self.push_pieces(document.text(), runs, start, end, scale, preformatted)?;
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

    fn place_inline_block(
        &mut self,
        document: &Document,
        block: &Block,
        y: u32,
        gap_after: u16,
    ) -> Result<u32, Error> {
        self.place_inline_region(document, block, y, 0, self.width, gap_after)
    }

    fn place_inline_region(
        &mut self,
        document: &Document,
        block: &Block,
        y: u32,
        origin_x: u16,
        available: u16,
        gap_after: u16,
    ) -> Result<u32, Error> {
        let runs = document.block_runs(block);
        let controls = &document.controls()[block.first_control as usize
            ..block.first_control as usize + block.control_count as usize];
        let start = runs
            .first()
            .map(|run| run.start)
            .or_else(|| controls.first().map(|control| control.text_offset))
            .unwrap_or(0);
        let end = runs
            .last()
            .map(|run| run.end)
            .unwrap_or(start)
            .max(controls.last().map_or(start, |control| control.text_offset));
        self.place_inline_region_range(
            document,
            block,
            y,
            origin_x,
            available,
            gap_after,
            start,
            end,
            block.first_control as usize,
            block.first_control as usize + block.control_count as usize,
            matches!(block.kind, BlockKind::Heading(_)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn place_inline_region_range(
        &mut self,
        document: &Document,
        block: &Block,
        mut y: u32,
        origin_x: u16,
        available: u16,
        gap_after: u16,
        start: u32,
        end: u32,
        control_start: usize,
        control_end: usize,
        heading: bool,
    ) -> Result<u32, Error> {
        let (scale, indent_cells, _, _) = self.block_metrics(block.kind);
        let indent_amount = indent_cells * CELL_WIDTH * scale as u16;
        let indent = origin_x.saturating_add(indent_amount);
        let budget = available.saturating_sub(indent_amount).max(1);
        let text_height = self.metrics.line_box(scale);
        let runs = document.block_runs(block);
        let controls = &document.controls()[control_start..control_end];
        let mut cursor = start;
        let mut line_x = 0u16;
        let mut line_first_piece = self.pieces.len() as u32;
        let mut pending: Vec<PendingControl> = Vec::new();
        let mut line_has_visible = false;
        let mut first_line = true;
        let marker = match block.kind {
            BlockKind::ListItem { marker, .. } => Some(marker),
            _ => None,
        };

        for (offset, control) in controls.iter().enumerate() {
            let stop = control.text_offset.clamp(cursor, end);
            self.place_inline_text(
                document,
                runs,
                &mut cursor,
                stop,
                indent,
                budget,
                scale,
                text_height,
                marker,
                heading,
                &mut y,
                &mut line_x,
                &mut line_first_piece,
                &mut pending,
                &mut line_has_visible,
                &mut first_line,
            )?;
            let control_id = (control_start + offset).min(u16::MAX as usize) as u16;
            if control.kind == ControlKind::Hidden {
                continue;
            }
            let height = control_box_height(Some(control), self.metrics.line_box(BODY_SCALE));
            let desired = control_box_width(document, control_id, control, height);
            let width = desired.min(budget).max(1);
            if line_has_visible && line_x.saturating_add(width) > budget {
                self.finish_inline_line(
                    indent,
                    scale,
                    text_height,
                    marker,
                    heading,
                    &mut y,
                    &mut line_x,
                    &mut line_first_piece,
                    &mut pending,
                    &mut line_has_visible,
                    &mut first_line,
                )?;
            }
            memory::push(
                &mut pending,
                PendingControl {
                    control: control_id,
                    x: line_x,
                    width,
                    height,
                },
            )?;
            line_x = line_x.saturating_add(width);
            line_has_visible = true;
        }
        self.place_inline_text(
            document,
            runs,
            &mut cursor,
            end,
            indent,
            budget,
            scale,
            text_height,
            marker,
            heading,
            &mut y,
            &mut line_x,
            &mut line_first_piece,
            &mut pending,
            &mut line_has_visible,
            &mut first_line,
        )?;
        if line_has_visible {
            self.finish_inline_line(
                indent,
                scale,
                text_height,
                marker,
                heading,
                &mut y,
                &mut line_x,
                &mut line_first_piece,
                &mut pending,
                &mut line_has_visible,
                &mut first_line,
            )?;
        }
        Ok(y.saturating_add(gap_after as u32))
    }

    #[allow(clippy::too_many_arguments)]
    fn place_inline_text(
        &mut self,
        document: &Document,
        runs: &[Run],
        cursor: &mut u32,
        end: u32,
        indent: u16,
        budget: u16,
        scale: u8,
        text_height: u16,
        marker: Option<Marker>,
        heading: bool,
        y: &mut u32,
        line_x: &mut u16,
        line_first_piece: &mut u32,
        pending: &mut Vec<PendingControl>,
        line_has_visible: &mut bool,
        first_line: &mut bool,
    ) -> Result<(), Error> {
        while *cursor < end {
            if *line_x >= budget && *line_has_visible {
                self.finish_inline_line(
                    indent,
                    scale,
                    text_height,
                    marker,
                    heading,
                    y,
                    line_x,
                    line_first_piece,
                    pending,
                    line_has_visible,
                    first_line,
                )?;
            }
            let Some(text) = document.text().get(*cursor as usize..end as usize) else {
                break;
            };
            let remaining = budget.saturating_sub(*line_x).max(1);
            let (line_end, next) =
                next_line(text, 0, remaining, scale, false, *cursor as usize, runs);
            let piece_start = self.pieces.len();
            let count = self.push_pieces(
                document.text(),
                runs,
                *cursor as usize,
                *cursor as usize + line_end,
                scale,
                false,
            )?;
            for piece in &mut self.pieces[piece_start..] {
                piece.x = piece.x.saturating_add(*line_x);
            }
            let added = self.pieces[piece_start..]
                .iter()
                .map(|piece| piece.width)
                .fold(0u16, u16::saturating_add);
            *line_x = line_x.saturating_add(added);
            *line_has_visible |= count != 0;
            *cursor = cursor.saturating_add(next as u32);
            if next < text.len() {
                self.finish_inline_line(
                    indent,
                    scale,
                    text_height,
                    marker,
                    heading,
                    y,
                    line_x,
                    line_first_piece,
                    pending,
                    line_has_visible,
                    first_line,
                )?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_inline_line(
        &mut self,
        indent: u16,
        scale: u8,
        text_height: u16,
        marker: Option<Marker>,
        heading: bool,
        y: &mut u32,
        line_x: &mut u16,
        line_first_piece: &mut u32,
        pending: &mut Vec<PendingControl>,
        line_has_visible: &mut bool,
        first_line: &mut bool,
    ) -> Result<(), Error> {
        let height = pending
            .iter()
            .map(|control| control.height)
            .fold(text_height, u16::max);
        let text_y = y.saturating_add(u32::from(height.saturating_sub(text_height) / 2));
        let piece_count = (self.pieces.len() as u32 - *line_first_piece) as u16;
        self.push_line(Line {
            y: text_y,
            height: text_height,
            x: indent,
            scale,
            first_piece: *line_first_piece,
            piece_count,
            marker: if *first_line { marker } else { None },
            heading,
            rule: false,
        })?;
        for control in pending.drain(..) {
            let control_y = y.saturating_add(u32::from(height.saturating_sub(control.height) / 2));
            memory::push(
                &mut self.controls,
                ControlBox {
                    control: control.control,
                    x: indent.saturating_add(control.x),
                    y: control_y,
                    width: control.width,
                    height: control.height,
                },
            )?;
        }
        *y = y.saturating_add(height as u32);
        *line_x = 0;
        *line_first_piece = self.pieces.len() as u32;
        *line_has_visible = false;
        *first_line = false;
        Ok(())
    }

    fn place_all_button_images(&mut self, document: &Document) -> Result<(), Error> {
        for box_ in &self.controls {
            let Some(control) = document.controls().get(box_.control as usize) else {
                continue;
            };
            let images: Vec<_> = document
                .images()
                .iter()
                .enumerate()
                .filter(|(_, image)| image.button == Some(box_.control))
                .collect();
            let mut preceding_images = 0u16;
            for (image_index, image) in images {
                let (width, height) = button_image_size(image, self.metrics.line_box(BODY_SCALE));
                let text_width =
                    button_text_width_to(document, control, image.text_offset as usize);
                let x = box_
                    .x
                    .saturating_add(6)
                    .saturating_add(text_width)
                    .saturating_add(preceding_images);
                let y = box_
                    .y
                    .saturating_add(u32::from(box_.height.saturating_sub(height) / 2));
                let visible_width = width.min(box_.x.saturating_add(box_.width).saturating_sub(x));
                if visible_width == 0 {
                    preceding_images = preceding_images.saturating_add(width);
                    continue;
                }
                memory::push(
                    &mut self.images,
                    ImageBox {
                        image: image_index as u16,
                        x,
                        y,
                        width: visible_width,
                        height,
                        link: None,
                        button: Some(box_.control),
                    },
                )?;
                preceding_images = preceding_images.saturating_add(width);
            }
        }
        Ok(())
    }

    fn place_table(&mut self, document: &Document, table: &Table, top: u32) -> Result<u32, Error> {
        if table.column_count == 0 || table.row_count == 0 {
            return Ok(top);
        }
        let cells = &document.table_cells()
            [table.first_cell as usize..(table.first_cell + table.cell_count) as usize];
        let columns = table.column_count as usize;
        let mut widths = alloc::vec![CELL_WIDTH + TABLE_CELL_PADDING * 2; columns];
        for cell in cells.iter().filter(|cell| cell.colspan == 1) {
            let block = &document.blocks()[cell.block as usize];
            let preferred = preferred_table_cell_width(document, cell, block);
            widths[cell.column as usize] = widths[cell.column as usize].max(preferred);
        }
        for cell in cells.iter().filter(|cell| cell.colspan > 1) {
            let block = &document.blocks()[cell.block as usize];
            let preferred = preferred_table_cell_width(document, cell, block);
            let start = cell.column as usize;
            let end = (start + cell.colspan as usize).min(columns);
            let current: u32 = widths[start..end].iter().map(|&width| width as u32).sum();
            if preferred as u32 > current {
                let missing = preferred as u32 - current;
                let count = (end - start) as u32;
                for (offset, width) in widths[start..end].iter_mut().enumerate() {
                    *width = width.saturating_add(
                        (missing / count + u32::from((offset as u32) < missing % count)) as u16,
                    );
                }
            }
        }
        fit_columns(&mut widths, self.width);
        let mut x = alloc::vec![0u16; columns + 1];
        for column in 0..columns {
            x[column + 1] = x[column].saturating_add(widths[column]);
        }
        let line_height = self.metrics.line_box(BODY_SCALE);
        let mut heights =
            alloc::vec![line_height + TABLE_CELL_PADDING * 2; table.row_count as usize];
        for cell in cells.iter().filter(|cell| cell.rowspan == 1) {
            let width = x[(cell.column + cell.colspan) as usize] - x[cell.column as usize];
            let needed = self
                .measure_cell(document, cell, width)
                .saturating_add(TABLE_CELL_PADDING * 2);
            heights[cell.row as usize] = heights[cell.row as usize].max(needed);
        }
        for cell in cells.iter().filter(|cell| cell.rowspan > 1) {
            let width = x[(cell.column + cell.colspan) as usize] - x[cell.column as usize];
            let needed = self
                .measure_cell(document, cell, width)
                .saturating_add(TABLE_CELL_PADDING * 2);
            let end = (cell.row as usize + cell.rowspan as usize).min(heights.len());
            let available: u32 = heights[cell.row as usize..end]
                .iter()
                .map(|&h| h as u32)
                .sum();
            if needed as u32 > available {
                heights[end - 1] = heights[end - 1]
                    .saturating_add((needed as u32 - available).min(u16::MAX as u32) as u16);
            }
        }
        let mut row_y = alloc::vec![top; heights.len() + 1];
        for row in 0..heights.len() {
            row_y[row + 1] = row_y[row].saturating_add(heights[row] as u32);
        }
        for cell in cells {
            let right = (cell.column as usize + cell.colspan as usize).min(columns);
            let bottom = (cell.row as usize + cell.rowspan as usize).min(heights.len());
            let box_ = CellBox {
                x: x[cell.column as usize],
                y: row_y[cell.row as usize],
                width: x[right] - x[cell.column as usize],
                height: row_y[bottom] - row_y[cell.row as usize],
                header: cell.header,
                border: table.border,
            };
            memory::push(&mut self.cells, box_)?;
            self.place_cell(document, cell, box_)?;
        }
        Ok(*row_y.last().unwrap_or(&top) + line_height as u32 / 2)
    }

    fn measure_cell(&self, document: &Document, cell: &TableCell, width: u16) -> u16 {
        let block = &document.blocks()[cell.block as usize];
        let budget = width.saturating_sub(TABLE_CELL_PADDING * 2);
        let mut scratch = Layout {
            lines: Vec::new(),
            pieces: Vec::new(),
            height: 0,
            width: budget,
            metrics: self.metrics,
            anchors: Vec::new(),
            cells: Vec::new(),
            images: Vec::new(),
            controls: Vec::new(),
        };
        scratch
            .place_cell_contents(document, cell, block, 0, 0, budget)
            .unwrap_or(u32::from(u16::MAX))
            .min(u32::from(u16::MAX)) as u16
    }

    fn place_cell(
        &mut self,
        document: &Document,
        cell_data: &TableCell,
        cell: CellBox,
    ) -> Result<(), Error> {
        let block = &document.blocks()[cell_data.block as usize];
        let x = cell.x + TABLE_CELL_PADDING;
        let budget = cell.width.saturating_sub(TABLE_CELL_PADDING * 2);
        self.place_cell_contents(
            document,
            cell_data,
            block,
            cell.y + TABLE_CELL_PADDING as u32,
            x,
            budget,
        )?;
        Ok(())
    }

    fn place_cell_contents(
        &mut self,
        document: &Document,
        cell: &TableCell,
        block: &Block,
        mut y: u32,
        x: u16,
        budget: u16,
    ) -> Result<u32, Error> {
        if !cell_has_standalone_items(document, cell) {
            return self.place_inline_region_range(
                document,
                block,
                y,
                x,
                budget,
                0,
                cell.text_start,
                cell.text_end,
                cell.first_control as usize,
                cell.first_control as usize + cell.control_count as usize,
                cell.header,
            );
        }

        let line_height = self.metrics.line_box(BODY_SCALE);
        let mut cursor = cell.text_start;
        let mut image = cell.first_image as usize;
        let image_end = image + cell.image_count as usize;
        let mut control = cell.first_control as usize;
        let control_end = control + cell.control_count as usize;
        loop {
            while image < image_end && document.images()[image].button.is_some() {
                image += 1;
            }
            let image_at = document
                .images()
                .get(image)
                .filter(|_| image < image_end)
                .map_or(u32::MAX, |item| item.text_offset);
            let textarea = (control..control_end)
                .find(|&index| document.controls()[index].kind == ControlKind::Textarea);
            let textarea_at =
                textarea.map_or(u32::MAX, |index| document.controls()[index].text_offset);
            if image_at == u32::MAX && textarea_at == u32::MAX {
                break;
            }

            if textarea_at <= image_at {
                let textarea = textarea.expect("textarea offset came from an index");
                let stop = textarea_at.max(cursor);
                y = self.place_inline_region_range(
                    document,
                    block,
                    y,
                    x,
                    budget,
                    0,
                    cursor,
                    stop,
                    control,
                    textarea,
                    cell.header,
                )?;
                let item = &document.controls()[textarea];
                let height = control_box_height(Some(item), line_height);
                memory::push(
                    &mut self.controls,
                    ControlBox {
                        control: textarea as u16,
                        x,
                        y,
                        width: budget.min(320).max(1),
                        height,
                    },
                )?;
                y = y.saturating_add(height as u32);
                cursor = stop;
                control = textarea + 1;
            } else {
                let segment_control_start = control;
                while control < control_end
                    && document.controls()[control].kind != ControlKind::Textarea
                    && document.controls()[control].text_offset <= image_at
                {
                    control += 1;
                }
                let stop = image_at.max(cursor);
                y = self.place_inline_region_range(
                    document,
                    block,
                    y,
                    x,
                    budget,
                    0,
                    cursor,
                    stop,
                    segment_control_start,
                    control,
                    cell.header,
                )?;
                let item = &document.images()[image];
                let (width, height) = image_size(item, budget);
                memory::push(
                    &mut self.images,
                    ImageBox {
                        image: image as u16,
                        x,
                        y,
                        width,
                        height,
                        link: item.link,
                        button: None,
                    },
                )?;
                y = y.saturating_add(height as u32);
                cursor = item.text_end.max(cursor);
                image += 1;
            }
        }
        self.place_inline_region_range(
            document,
            block,
            y,
            x,
            budget,
            0,
            cursor,
            cell.text_end,
            control,
            control_end,
            cell.header,
        )
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
        force_mono: bool,
    ) -> Result<u16, Error> {
        let mut count = 0u16;
        let mut x = 0u16;
        let mut consumed_to = start;
        for run in runs {
            let from = (run.start as usize).max(start).max(consumed_to);
            let mut to = (run.end as usize).min(end);
            if from >= to {
                continue;
            }
            // A style boundary may sit between a base and its combining
            // mark (`<b>e</b>&#x301;`). Keep the whole cluster in the base's
            // piece so both measurement and drawing route it to the fixed-cell
            // font together.
            if to < end {
                while let Some(character) = text.get(to..end).and_then(|tail| tail.chars().next()) {
                    if !tab5_ui_font::is_combining(character) {
                        break;
                    }
                    to += character.len_utf8();
                }
            }
            let mono = force_mono || run.style & crate::document::STYLE_CODE != 0;
            let measurement_style = run.style | if mono { crate::document::STYLE_CODE } else { 0 };
            let width = text.get(from..to).map_or(0, |slice| {
                text_width_styled(slice, scale, measurement_style)
            });
            memory::push(
                &mut self.pieces,
                Piece {
                    x,
                    width,
                    start: from as u32,
                    end: to as u32,
                    style: run.style,
                    mono,
                    link: run.link,
                },
            )?;
            x = x.saturating_add(width);
            count = count.saturating_add(1);
            consumed_to = to;
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
            BlockKind::ListItem { depth, .. } => {
                (BODY_SCALE, MARKER_CELLS + depth as u16 * INDENT_CELLS, 0, 0)
            }
            BlockKind::Preformatted => (BODY_SCALE, PRE_INDENT_CELLS, half, half),
            BlockKind::Rule => (BODY_SCALE, 0, half, half),
            BlockKind::Control(_) => (BODY_SCALE, 0, half / 2, half / 2),
        }
    }
}

fn image_for_block<'a>(
    document: &'a Document,
    block: &Block,
) -> Option<(usize, &'a crate::document::Image)> {
    let runs = document.block_runs(block);
    let first = runs.first()?;
    let last = runs.last()?;
    document
        .images()
        .iter()
        .enumerate()
        .find(|(_, image)| image.text_offset == first.start && image.text_end == last.end)
        .filter(|(_, image)| image.button.is_none())
}

fn block_text_span(document: &Document, block: &Block) -> Option<(u32, u32)> {
    let runs = document.block_runs(block);
    Some((runs.first()?.start, runs.last()?.end))
}

fn image_size(image: &crate::document::Image, available: u16) -> (u16, u16) {
    let intrinsic = image.intrinsic_width.zip(image.intrinsic_height);
    let (mut width, mut height) = match (image.width, image.height, intrinsic) {
        (Some(width), Some(height), _) => (width, height),
        (Some(width), None, Some((iw, ih))) => (
            width,
            ((u32::from(width) * u32::from(ih)) / u32::from(iw))
                .max(1)
                .min(u32::from(u16::MAX)) as u16,
        ),
        (None, Some(height), Some((iw, ih))) => (
            ((u32::from(height) * u32::from(iw)) / u32::from(ih))
                .max(1)
                .min(u32::from(u16::MAX)) as u16,
            height,
        ),
        (None, None, Some(dimensions)) => dimensions,
        (width, height, None) => (
            width.unwrap_or(PLACEHOLDER_IMAGE_WIDTH),
            height.unwrap_or(PLACEHOLDER_IMAGE_HEIGHT),
        ),
    };
    let available = available.max(1);
    if width > available {
        height = ((height as u32 * available as u32) / width as u32)
            .max(1)
            .min(u16::MAX as u32) as u16;
        width = available;
    }
    (width, height)
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
    block_start: usize,
    runs: &[Run],
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
        let run_style = runs
            .iter()
            .find(|run| {
                (run.start as usize) <= block_start + offset
                    && block_start + offset < run.end as usize
            })
            .map_or(0, |run| run.style)
            | if preformatted {
                crate::document::STYLE_CODE
            } else {
                0
            };
        let next_is_combining = rest[offset + character.len_utf8()..]
            .chars()
            .next()
            .is_some_and(tab5_ui_font::is_combining);
        let width = if tab5_ui_font::is_english_latin(character) && next_is_combining {
            tab5_font::advance(character) as u16
                * ui_style(scale, run_style).fallback_scale() as u16
        } else if tab5_ui_font::is_combining(character) {
            0
        } else {
            let mut encoded = [0u8; 4];
            text_width_styled(character.encode_utf8(&mut encoded), scale, run_style)
        };
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
        let Some(previous) = text
            .get(from..candidate)
            .and_then(|s| s.chars().next_back())
        else {
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

    fn layout_of_width(markup: &str, width: u16) -> (crate::document::Document, Layout) {
        let document = document(markup);
        let layout = Layout::build(&document, width, metrics()).unwrap();
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
        let markup = format!(
            "<p>{}</p>",
            (0..30).map(|_| word).collect::<Vec<_>>().join(" ")
        );
        let (document, layout) = layout_of(&markup);
        let lines = rendered(&document, &layout);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(
                text_width(line, BODY_SCALE) <= WIDTH,
                "{line:?} exceeds the pixel width"
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
        for line in layout.lines() {
            for piece in layout.pieces(line) {
                assert!(piece.mono);
                let text = &document.text()[piece.start as usize..piece.end as usize];
                assert_eq!(piece.width, text.chars().count() as u16 * CELL_WIDTH);
            }
        }
    }

    #[test]
    fn pre_is_monospace_even_without_an_inner_code_element() {
        let (document, layout) = layout_of("<p>iiiiWWWW</p><pre>iiiiWWWW</pre>");
        let paragraph = layout.pieces(&layout.lines()[0])[0];
        let pre = layout.pieces(&layout.lines()[1])[0];
        assert!(!paragraph.mono);
        assert!(pre.mono);
        assert_ne!(paragraph.width, pre.width);
        assert_eq!(pre.width, 8 * CELL_WIDTH);
        assert_eq!(
            &document.text()[pre.start as usize..pre.end as usize],
            "iiiiWWWW"
        );
    }

    // --- mixed widths -----------------------------------------------------

    /// Half-width and full-width alternating, so a line that was measured by
    /// counting characters would be half again as wide as the viewport.
    const MIXED: &str = "aあiいuうeえoお";

    #[test]
    fn latin_is_proportional_and_fallback_full_width_keeps_its_cells() {
        assert_eq!(advance('i', BODY_SCALE), 4);
        assert_eq!(advance('W', BODY_SCALE), 13);
        assert_eq!(advance('あ', BODY_SCALE), 16);
        assert_eq!(advance('あ', HEADING_SCALE), 32);
        assert_eq!(text_width(MIXED, BODY_SCALE), 116);
    }

    #[test]
    fn combining_marks_add_no_width() {
        // The mark rides on the kana before it, so this is one full-width
        // character, not two.
        assert_eq!(text_width("か\u{3099}", BODY_SCALE), 16);
        assert_eq!(text_width("e\u{301}", BODY_SCALE), 8);
    }

    #[test]
    fn a_style_boundary_does_not_split_a_combining_cluster() {
        let (document, layout) = layout_of("<p><b>e</b>&#x301; next</p>");
        let line = &layout.lines()[0];
        let pieces = layout.pieces(line);
        assert_eq!(
            &document.text()[pieces[0].start as usize..pieces[0].end as usize],
            "e\u{301}"
        );
        assert_eq!(pieces[0].width, 8);
        assert_eq!(pieces[1].x, 8);
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
            assert!(
                line.y >= previous_bottom,
                "{line:?} above {previous_bottom}"
            );
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
        let (_, layout) = layout_of("<ul><li>one</li><li>two<ol><li>inner</li></ol></li></ul>");
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
            .map(|piece| &document.text()[piece.start as usize..piece.end as usize])
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
        let markup: String = (0..200)
            .map(|index| format!("<p>Line {index}</p>"))
            .collect();
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
        let markup: String = (0..200)
            .map(|index| format!("<p>Line {index}</p>"))
            .collect();
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
    fn a_long_page_reports_its_owned_memory() {
        let markup: String = (0..3000)
            .map(|index| format!("<p>Paragraph {index} with a few words in it.</p>"))
            .collect();
        let (document, layout) = layout_of(&markup);
        let total = document.stats().owned_bytes + layout.owned_bytes();
        assert!(total > document.text().len(), "{total}");
    }

    #[test]
    fn anchors_map_to_heading_inline_rule_and_document_end() {
        let (document, layout) = layout_of(
            "<h2 id='head'>heading</h2><p>before <span id='inline'>target</span></p><hr id='rule'><span id='end'></span>",
        );
        assert_eq!(layout.line_of_anchor(&document, "head"), Some(0));
        assert!(layout.line_of_anchor(&document, "inline").unwrap() >= 1);
        assert!(layout.line_of_anchor(&document, "rule").is_some());
        assert_eq!(
            layout.line_of_anchor(&document, "end"),
            Some(layout.lines().len() - 1)
        );
    }

    #[test]
    fn table_cells_fit_the_viewport_and_links_hit_at_their_drawn_position() {
        let (document, layout) = layout_of(
            "<table><tr><th>A</th><th>B</th><th>C</th></tr><tr><td>short</td><td>日本語</td><td>a deliberately long value that wraps</td></tr><tr><td rowspan='2'>span</td><td colspan='2'><a href='/x'>linked cell</a></td></tr><tr><td></td><td>end</td></tr></table>",
        );
        assert_eq!(layout.cells().len(), 10);
        assert!(
            layout
                .cells()
                .iter()
                .all(|cell| cell.x + cell.width <= WIDTH)
        );
        assert!(layout.lines().windows(2).all(|pair| pair[0].y <= pair[1].y));
        let line = layout.line_of_link(0).unwrap();
        let piece = layout
            .pieces(&layout.lines()[line])
            .iter()
            .find(|piece| piece.link == Some(0))
            .unwrap();
        assert_eq!(
            layout.hit(layout.lines()[line].x + piece.x, layout.lines()[line].y),
            Some(0)
        );
        assert!(!document.table_cells().is_empty());
    }

    #[test]
    fn table_preferred_width_uses_the_widest_logical_line() {
        let (document, layout) =
            layout_of("<table><tr><td><b>WW</b>WW<br>i</td><td>xx</td></tr></table>");
        let first = layout.cells()[0];
        let block = &document.blocks()[document.table_cells()[0].block as usize];
        assert_eq!(
            preferred_cell_width(&document, block),
            text_width("WWWW", BODY_SCALE) + TABLE_CELL_PADDING * 2
        );
        assert_eq!(first.width, preferred_cell_width(&document, block));
    }

    #[test]
    fn image_boxes_apply_each_dimension_fallback_independently() {
        let (_, layout) = layout_of(
            "<img src='both' width='320' height='180'>\
             <img src='width' width='200'>\
             <img src='height' height='60'>\
             <img src='neither'>",
        );
        assert_eq!(
            layout
                .images()
                .iter()
                .map(|image| (image.width, image.height))
                .collect::<Vec<_>>(),
            [(320, 180), (200, 90), (160, 60), (160, 90)]
        );
        assert!(layout.images().windows(2).all(|pair| pair[0].y < pair[1].y));
    }

    #[test]
    fn image_wider_than_its_region_shrinks_with_its_ratio() {
        let document = document("<img src='wide' width='400' height='200'>");
        let layout = Layout::build(&document, 100, metrics()).unwrap();
        assert_eq!(
            (layout.images()[0].width, layout.images()[0].height),
            (100, 50)
        );
    }

    #[test]
    fn intrinsic_dimensions_replace_only_unspecified_axes() {
        let mut document = document(
            "<img src='a'><img src='b' width='96'><img src='c' height='64'><img src='d' width='70' height='50'>",
        );
        for index in 0..4 {
            assert!(document.set_image_intrinsic(index, Some((192, 128))));
        }
        let layout = Layout::build(&document, WIDTH, metrics()).unwrap();
        let sizes: Vec<_> = layout
            .images()
            .iter()
            .map(|image| (image.width, image.height))
            .collect();
        assert_eq!(sizes, [(192, 128), (96, 64), (96, 64), (70, 50)]);
    }

    #[test]
    fn linked_image_uses_its_rectangle_for_hit_testing() {
        let (_, layout) =
            layout_of("<a href='/target'><img src='button' width='80' height='40'></a>");
        let image = layout.images()[0];
        assert_eq!(layout.hit(image.x + 79, image.y + 39), image.link);
        assert_eq!(layout.hit(image.x + 80, image.y + 39), None);
        assert_eq!(layout.link_order().unwrap(), [0]);
    }

    #[test]
    fn visible_form_controls_reserve_space_and_are_hit_tested() {
        let (document, layout) = layout_of(
            "<p>before</p><form><input name=q value=hello><input type=hidden name=h value=x><input type=submit value=Go></form><p>after</p>",
        );
        assert_eq!(layout.controls().len(), 2);
        let first = layout.controls()[0];
        assert_eq!(layout.control_at(first.x + 1, first.y + 1), Some(0));
        assert_eq!(layout.controls()[1].control, 2);
        assert!(layout.lines().last().unwrap().y > layout.controls()[1].y);
        assert_eq!(document.controls().len(), 3);
    }

    #[test]
    fn controls_share_a_line_with_text_and_wrap_as_one_inline_flow() {
        let (document, layout) = layout_of(
            "<p>before <input name=q value=hello> after <input type=checkbox name=c> tail</p>",
        );
        let input = layout.controls()[0];
        let checkbox = layout.controls()[1];
        let line = layout
            .lines()
            .iter()
            .find(|line| {
                line.y < input.y + input.height as u32 && line.y + line.height as u32 > input.y
            })
            .unwrap();
        let pieces = layout.pieces(line);
        assert!(pieces.first().unwrap().width <= input.x);
        assert!(pieces.last().unwrap().x >= input.x + input.width);
        assert_eq!(input.y, checkbox.y);
        assert_eq!(layout.control_at(input.x + 1, input.y + 1), Some(0));
        assert_eq!(document.blocks()[0].control_count, 2);

        let (_, narrow) = layout_of_width("<p>word<input name=q>tail</p>", 100);
        let control = narrow.controls()[0];
        assert!(control.y > narrow.lines()[0].y);
        assert_eq!(control.width, 100);
    }

    #[test]
    fn styled_button_uses_content_width_and_owns_its_scaled_image() {
        let (document, layout) = layout_of(
            "<p>x<button name=go><strong>Go</strong><img src=icon width=40 height=80>now</button>y</p>",
        );
        let button = layout.controls()[0];
        assert!(button.width >= 80 && button.width < 320);
        let image = layout
            .images()
            .iter()
            .find(|image| image.button == Some(0))
            .unwrap();
        assert_eq!(image.height, metrics().line_box(BODY_SCALE));
        assert!(image.x >= button.x + 6);
        assert!(image.x + image.width <= button.x + button.width);
        assert_eq!(document.images()[image.image as usize].button, Some(0));
        assert_eq!(layout.control_at(image.x + 1, image.y + 1), Some(0));
        assert!(layout.hit(image.x + 1, image.y + 1).is_none());
    }

    #[test]
    fn explicit_label_text_hits_its_control() {
        let (document, layout) =
            layout_of("<p><label for=q>Search term</label></p><input id=q name=q value=old>");
        let line = layout.lines()[0];
        assert_eq!(
            layout.label_control_at(&document, line.x + 2, line.y + 2),
            Some(0)
        );
    }

    #[test]
    fn table_cell_control_uses_the_same_inline_flow_and_grows_the_row() {
        let (document, layout) = layout_of(
            "<form><table border=1><tr><td>before<input id=q name=q value=old>after</td><td>peer</td></tr></table></form>",
        );
        let control = layout
            .controls()
            .iter()
            .find(|item| item.control == 0)
            .unwrap();
        let cell = layout.cells()[0];
        assert!(control.x >= cell.x);
        assert!(control.x + control.width <= cell.x + cell.width);
        assert!(control.y >= cell.y);
        assert!(control.y + control.height as u32 <= cell.y + cell.height);
        let lines: Vec<_> = layout
            .lines()
            .iter()
            .filter(|line| line.x >= cell.x && line.x < cell.x + cell.width)
            .collect();
        assert!(lines.iter().any(|line| {
            line.y < control.y + control.height as u32 && line.y + line.height as u32 > control.y
        }));
        assert_eq!(layout.control_at(control.x + 1, control.y + 1), Some(0));
        assert_eq!(document.controls()[0].name, "q");
    }

    #[test]
    fn image_only_table_cell_grows_the_row_and_stays_inside_the_cell() {
        let (_, layout) = layout_of(
            "<table border='1'><tr><td>text</td><td><img src='cell' width='600' height='300'></td></tr></table>",
        );
        let image = layout.images()[0];
        let cell = layout.cells()[1];
        assert!(image.x >= cell.x + TABLE_CELL_PADDING);
        assert!(image.x + image.width <= cell.x + cell.width - TABLE_CELL_PADDING);
        assert!(image.y >= cell.y + TABLE_CELL_PADDING as u32);
        assert!(image.y + image.height as u32 <= cell.y + cell.height);
        assert!(cell.height >= image.height as u32 + TABLE_CELL_PADDING as u32 * 2);
    }

    #[test]
    fn image_mixed_with_text_in_a_table_cell_is_stacked_in_document_order() {
        let (document, layout) = layout_of(
            "<table><tr><td rowspan='2'><img src='portrait' alt='portrait' width='350' height='414'><br><br><table><tr><td>nested text</td></tr></table>after image</td><td>right</td></tr><tr><td>below</td></tr></table>",
        );
        let image = layout.images()[0];
        let cell = layout.cells()[0];
        assert!(image.y >= cell.y + TABLE_CELL_PADDING as u32);
        assert!(image.y + image.height as u32 <= cell.y + cell.height);
        let after_line = layout
            .lines()
            .iter()
            .find(|line| {
                layout.pieces(line).iter().any(|piece| {
                    document
                        .text()
                        .get(piece.start as usize..piece.end as usize)
                        .is_some_and(|text| text.contains("after image"))
                })
            })
            .unwrap();
        assert!(after_line.y >= image.y + image.height as u32);
        assert!(layout.all_pieces().iter().all(|piece| {
            document
                .text()
                .get(piece.start as usize..piece.end as usize)
                != Some("[portrait]")
        }));
    }

    #[test]
    fn controls_remain_inline_on_each_side_of_a_standalone_table_image() {
        let (document, layout) = layout_of(
            "<form><table><tr><td>before<input type=checkbox name=left><img src='cell' alt='skip' width='80' height='40'>after<input type=radio name=right>tail</td></tr></table></form>",
        );
        let image = layout.images()[0];
        let before = layout
            .lines()
            .iter()
            .find(|line| {
                layout.pieces(line).iter().any(|piece| {
                    document
                        .text()
                        .get(piece.start as usize..piece.end as usize)
                        .is_some_and(|text| text.contains("before"))
                })
            })
            .unwrap();
        let after = layout
            .lines()
            .iter()
            .find(|line| {
                layout.pieces(line).iter().any(|piece| {
                    document
                        .text()
                        .get(piece.start as usize..piece.end as usize)
                        .is_some_and(|text| text.contains("after"))
                })
            })
            .unwrap();
        let left = layout
            .controls()
            .iter()
            .find(|item| item.control == 0)
            .unwrap();
        let right = layout
            .controls()
            .iter()
            .find(|item| item.control == 1)
            .unwrap();

        assert!(before.y < image.y);
        assert!(left.y < image.y);
        assert!(before.y < left.y + left.height as u32);
        assert!(left.y < before.y + before.height as u32);
        assert!(after.y >= image.y + image.height as u32);
        assert!(right.y >= image.y + image.height as u32);
        assert!(after.y < right.y + right.height as u32);
        assert!(right.y < after.y + after.height as u32);
        assert!(layout.all_pieces().iter().all(|piece| {
            document
                .text()
                .get(piece.start as usize..piece.end as usize)
                != Some("[skip]")
        }));
    }

    #[test]
    fn table_gives_an_image_its_requested_width_when_room_is_available() {
        let (_, layout) = layout_of(
            "<table><tr><td>label</td><td><img src='cell' width='300' height='120'></td></tr></table>",
        );
        let image = layout.images()[0];
        let cell = layout.cells()[1];
        assert_eq!((image.width, image.height), (300, 120));
        assert_eq!(cell.width, 300 + TABLE_CELL_PADDING * 2);
    }

    #[test]
    fn logical_reading_position_survives_an_image_height_change_above_it() {
        let short_document =
            document("<p>before</p><img src='same' width='100' height='40'><p>reading target</p>");
        let tall_document =
            document("<p>before</p><img src='same' width='100' height='240'><p>reading target</p>");
        let short = Layout::build(&short_document, WIDTH, metrics()).unwrap();
        let tall = Layout::build(&tall_document, WIDTH, metrics()).unwrap();
        let short_target = short
            .lines()
            .iter()
            .find(|line| {
                short.pieces(line).iter().any(|piece| {
                    &short_document.text()[piece.start as usize..piece.end as usize]
                        == "reading target"
                })
            })
            .unwrap()
            .y;
        let position = short.reading_position(short_target + 3);
        let tall_target = tall
            .lines()
            .iter()
            .find(|line| {
                tall.pieces(line).iter().any(|piece| {
                    &tall_document.text()[piece.start as usize..piece.end as usize]
                        == "reading target"
                })
            })
            .unwrap()
            .y;
        assert_eq!(tall.y_of_reading_position(position), tall_target + 3);
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
