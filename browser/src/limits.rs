//! Every bound the browser is allowed to grow to, in one place.
//!
//! The reason they are gathered here rather than sitting next to the code
//! that enforces them is that they are a single budget, not nine unrelated
//! numbers: a page is allowed [`MAX_DECODED_HTML_BYTES`] of input, and what
//! survives into the document has to fit inside [`MAX_BROWSER_OWNED_BYTES`]
//! together with the text, the links and the layout. Changing one in
//! isolation is how a limit set stops adding up.
//!
//! These are also the numbers `tools/browser_fixture_server.py` builds its
//! over-limit fixtures against, and the ones `docs/WEB_BROWSER_PLAN.md`
//! records. All three have to agree, so the server reads them from its own
//! copy of this table and the plan quotes them.
//!
//! None of these is a soft target. Reaching one is an error the user is
//! told about, never a truncation presented as a finished page: a document
//! cut off at 8,192 items looks exactly like a document that ended there,
//! and there is no way for a reader to tell the difference after the fact.

/// The response head, from the status line to the blank line that ends it.
///
/// Shared with `net::http`, which had this bound before the browser
/// existed. A server that has not finished its headers by 4 KiB is not one
/// this can work with, and continuing to buffer means the body's start
/// becomes a guess.
pub const MAX_HEADER_BYTES: usize = 4096;

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
pub const MAX_ITEMS: usize = 8192;

/// Links kept for one page, each with its resolved target.
pub const MAX_LINKS: usize = 1024;

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

/// Peak dynamic memory the browser may own at once.
///
/// The heap is 23,322,624 bytes (`docs/PSRAM.md`), so this is not a memory
/// ceiling -- it is a statement that a hypertext viewer which needs more
/// than 4 MiB for one page has a duplicated representation in it. It is
/// measured as the sum of the capacities the browser itself allocated,
/// counted independently rather than inferred from the global allocator.
pub const MAX_BROWSER_OWNED_BYTES: usize = 4 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget has to add up: the two per-page byte bounds and the
    /// worst-case link table all live inside the owned-memory peak at the
    /// same time, because a document is built while its input is still
    /// arriving.
    ///
    /// This is the check that catches a limit raised on its own. Text plus
    /// links is 3 MiB against a 4 MiB peak, which leaves 1 MiB for items,
    /// the layout and the in-flight input chunk.
    #[test]
    fn text_and_links_fit_inside_the_owned_budget() {
        let links = MAX_LINKS * MAX_URL_BYTES;
        assert!(MAX_TEXT_BYTES + links < MAX_BROWSER_OWNED_BYTES);
    }

    /// Stored text cannot exceed the input it is extracted from.
    #[test]
    fn text_cannot_exceed_input() {
        assert!(MAX_TEXT_BYTES <= MAX_DECODED_HTML_BYTES);
    }
}
