//! Every bound the browser is allowed to grow to, in one place.
//!
//! The reason they are gathered here rather than sitting next to the code
//! that enforces them is that they are a single budget, not nine unrelated
//! numbers: a page is allowed [`MAX_DECODED_HTML_BYTES`] of input, and what
//! survives into the document has independent bounds for text, resolved
//! link targets and layout structures. Changing one in isolation is how a
//! limit set stops adding up.
//!
//! These are also the numbers `tools/browser_fixture_server.py` builds its
//! over-limit fixtures against, and the ones `docs/WEB_BROWSER_PLAN.md`
//! records. All three have to agree, so the server reads them from its own
//! copy of this table and the plan quotes them.
//!
//! None of these is a soft target. Reaching one is an error the user is
//! told about, never a truncation presented as a finished page: a document
//! cut off at 16,384 items looks exactly like a document that ended there,
//! and there is no way for a reader to tell the difference after the fact.

/// The response head, from the status line to the blank line that ends it.
///
/// Shared with `net::http`, which had this bound before the browser
/// existed. A server that has not finished its headers by here is not one
/// this can work with, and continuing to buffer means the body's start
/// becomes a guess.
///
/// It was 4 KiB, which turned out to be a real limit rather than a
/// theoretical one: measured on 2026-08-28, `en.wikipedia.org` answers with
/// 6.4 KiB of headers and `github.com` with 5.2 KiB, and both were refused
/// with `header-limit` before a byte of page arrived. Modern sites spend
/// that much on security policy and cookies alone. 16 KiB clears everything
/// measured with room over, and costs nothing that matters: the head buffer
/// is a heap allocation that only exists while a head is arriving, on a
/// heap of some twenty megabytes.
pub const MAX_HEADER_BYTES: usize = 16384;

/// One URL, as text.
///
/// Long enough for the query strings real sites generate and short enough
/// that a page full of links cannot spend its whole document budget on
/// them: [`MAX_LINKS`] of these is 2 MiB in the worst case, which is why
/// links are counted against the owned-memory budget rather than being
/// treated as free.
pub const MAX_URL_BYTES: usize = 2048;

/// How many times a `Location` may be followed before the chain is called
/// a loop.
///
/// Five is what browsers settled on. The exact number matters less than
/// having one: without it a server that redirects to itself is an infinite
/// fetch that only ends when the user notices.
pub const MAX_REDIRECTS: usize = 5;

/// Pages that can be gone back to.
///
/// History holds URLs and scroll offsets, never documents -- going back is
/// a re-fetch. That is what makes eight affordable: eight entries is
/// 8 * ([`MAX_URL_BYTES`] + a few words), not eight parsed pages.
pub const MAX_HISTORY: usize = 8;

/// Decoded HTML handed to the tokenizer for one page.
///
/// Counted after transfer decoding (chunked) and before the document is
/// built, so a `Content-Length` past this is refused before the body is
/// read at all, and a response without one is aborted the moment it
/// crosses. The raw HTML is never kept -- this bounds what flows through,
/// not what is stored.
pub const MAX_DECODED_HTML_BYTES: usize = 2 * 1024 * 1024;

/// Display text kept for one page.
///
/// Half the input bound, because markup, `script` and `style` bodies are
/// dropped rather than stored: a page that is more than half text by
/// weight is unusual, and one that reaches this has more prose in it than
/// the viewport can be scrolled through in any useful way.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// Document items -- the blocks, lines, rules and images the layout walks.
pub const MAX_ITEMS: usize = 16384;

/// Table grid bounds.  A table is always fitted to the viewport; wider
/// structures are rejected instead of being silently truncated.
pub const MAX_TABLE_COLUMNS: usize = 32;
pub const MAX_TABLE_SPAN: usize = 32;
pub const MAX_TABLE_BORDER: usize = 4;

/// Links kept for one page, each with its resolved target.
pub const MAX_LINKS: usize = 4096;
/// Sum of the owned, resolved link targets. Short references can resolve
/// against a long base URL, so the HTML input bound alone does not bound it.
pub const MAX_LINK_URL_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ANCHORS: usize = 1024;
pub const MAX_ANCHOR_NAME_BYTES: usize = MAX_URL_BYTES;
pub const MAX_ANCHOR_BYTES: usize = 256 * 1024;

/// How deep list nesting and inline state may go.
///
/// Real documents nest a handful of levels; hand-written and generated
/// markup can nest thousands, and each level is indentation the viewport
/// does not have. Past this the element is processed as text without
/// pushing a level, which keeps the content rather than the structure.
pub const MAX_NESTING_DEPTH: usize = 32;

/// Attributes examined on one element.
///
/// Only the attributes the viewer acts on (`href`, `alt`) have their values
/// kept; the rest are parsed far enough to be skipped. The bound is on how
/// many are looked at, so an element carrying hundreds of data attributes
/// costs a scan rather than a store.
pub const MAX_ATTRIBUTES_PER_ELEMENT: usize = 16;

/// Wrapped lines one page may lay out to.
///
/// Not derivable from the others: a megabyte of text is about ten thousand
/// lines at the panel's 106 columns, but a megabyte of `<br>` is a million,
/// and a `Line` is two dozen bytes. Without this the layout is the one part
/// of a page whose size the input controls directly.
///
/// Three times what a full page of prose needs, so it only ever fires on a
/// document built to fire it.
pub const MAX_LAYOUT_LINES: usize = 32768;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_storage_is_bounded_independently_of_link_count() {
        assert!(MAX_LINK_URL_BYTES <= MAX_DECODED_HTML_BYTES);
    }

    /// Stored text cannot exceed the input it is extracted from.
    #[test]
    fn text_cannot_exceed_input() {
        assert!(MAX_TEXT_BYTES <= MAX_DECODED_HTML_BYTES);
    }
}
