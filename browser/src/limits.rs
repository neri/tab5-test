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
//! over-limit fixtures against, and the ones `docs/plans/archive/WEB_BROWSER_PLAN.md`
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

// Browser extension budgets.  Compressed bytes and decoded pixels are
// deliberately separate: a tiny, adversarial PNG can describe a very large
// output and must be refused before its dimensions are multiplied.
pub const MAX_IMAGES: usize = 64;
pub const MAX_IMAGE_COMPRESSED_BYTES: usize = 512 * 1024;
pub const MAX_IMAGE_WIDTH: u32 = 1280;
pub const MAX_IMAGE_HEIGHT: u32 = 1280;
pub const MAX_IMAGE_PIXELS: usize = 1024 * 1024;
pub const MAX_IMAGE_DECODE_WORK_BYTES: usize = 1024 * 1024;
pub const PLACEHOLDER_IMAGE_WIDTH: u16 = 160;
pub const PLACEHOLDER_IMAGE_HEIGHT: u16 = 90;

pub const MAX_FORMS: usize = 32;
pub const MAX_FORM_CONTROLS: usize = 256;
pub const MAX_INPUT_VALUE_BYTES: usize = 4096;
pub const MAX_FORM_VALUE_BYTES: usize = 32 * 1024;
pub const MAX_ENCODED_REQUEST_BYTES: usize = 48 * 1024;
/// `option` elements across every `select` of one page. Each keeps a label
/// and a value, both bounded by [`MAX_INPUT_VALUE_BYTES`].
pub const MAX_SELECT_OPTIONS: usize = 4096;

/// One cached response body. The cache itself lives in files on the RAM
/// disk and has no total bound of its own: a write that finds the volume
/// full purges entries and retries. Only this body -- captured while it
/// arrives, then written out -- is ever held in memory.
pub const MAX_HTTP_CACHE_ENTRY_BYTES: usize = 512 * 1024;
/// How long a response with no `max-age` and no usable `Expires` stays
/// fresh.
pub const DEFAULT_CACHE_FRESHNESS_SECS: usize = 3600;
/// One cache entry's metadata file: the key, the validator and a few
/// numbers.
pub const MAX_CACHE_META_BYTES: usize = 4096;

/// POST results kept so back/forward can show them again without resending.
///
/// Separate from the HTTP cache: these are history state, never reused for
/// a new request. Only the parsed document is kept (layout and decoded
/// images are rebuilt), and a result larger than the byte budget is not kept
/// at all. Requests kept for an explicit resend have their own body budget,
/// two full-size encoded requests.
pub const MAX_RETAINED_POST_RESULTS: usize = 2;
pub const MAX_RETAINED_POST_RESULT_BYTES: usize = 320 * 1024;
pub const MAX_RETAINED_POST_REQUEST_BYTES: usize = 2 * 48 * 1024;

/// Aggregate extension-owned memory outside the existing Document/Layout
/// accounting.  Every cache/image/form allocation must reserve against this
/// budget before growing its own buffer.
pub const MAX_EXTENSION_OWNED_BYTES: usize = 6 * 1024 * 1024;

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

    #[test]
    fn extension_limits_fit_the_aggregate_budget() {
        // One active compressed image, its decoder workspace and decoded
        // RGB565 pixels may coexist with one cache body in memory and all
        // form values.
        let active_image = MAX_IMAGE_COMPRESSED_BYTES
            + MAX_IMAGE_DECODE_WORK_BYTES
            + MAX_IMAGE_PIXELS * core::mem::size_of::<u16>();
        let history = MAX_RETAINED_POST_RESULT_BYTES + MAX_RETAINED_POST_REQUEST_BYTES;
        assert!(
            active_image + MAX_HTTP_CACHE_ENTRY_BYTES + MAX_FORM_VALUE_BYTES + history
                <= MAX_EXTENSION_OWNED_BYTES
        );
        assert!(MAX_ENCODED_REQUEST_BYTES <= MAX_RETAINED_POST_REQUEST_BYTES);
        assert!(MAX_URL_BYTES + 256 + 512 <= MAX_CACHE_META_BYTES);
        assert!(MAX_INPUT_VALUE_BYTES <= MAX_FORM_VALUE_BYTES);
        assert!(MAX_FORM_VALUE_BYTES <= MAX_ENCODED_REQUEST_BYTES);
    }

    #[test]
    fn image_dimensions_cannot_overflow_the_pixel_limit() {
        let largest_declared = (MAX_IMAGE_WIDTH as usize)
            .checked_mul(MAX_IMAGE_HEIGHT as usize)
            .expect("dimension product");
        assert!(MAX_IMAGE_PIXELS <= largest_declared);
    }
}
