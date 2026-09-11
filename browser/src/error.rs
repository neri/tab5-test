//! The one error a page can fail with, shared by every layer above the URL.
//!
//! There is a single enum rather than one per module because the browser
//! only ever does one thing with a failure: stop, and put a sentence on the
//! screen. A tokenizer error and a document-limit error travel the same
//! path, get the same treatment, and differ only in what that sentence
//! says -- which is a reason for one type with several variants, not for
//! several types with conversions between them.
//!
//! Every variant except [`Error::OutOfMemory`] is a limit from
//! [`crate::limits`] being reached. That is deliberate: a hypertext viewer
//! with no bounds is one page away from taking the board down, and the
//! bounds are only real if reaching one is an outcome the user is told
//! about rather than a truncation dressed up as a finished page.

use crate::memory::OutOfMemory;
use crate::url;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// A URL -- the page's own, or a link's -- could not be used.
    Url(url::Error),
    /// The HTML input went past [`crate::limits::MAX_DECODED_HTML_BYTES`].
    InputTooLong,
    /// The display text went past [`crate::limits::MAX_TEXT_BYTES`].
    TextTooLong,
    /// Past [`crate::limits::MAX_ITEMS`] blocks and runs.
    TooManyItems,
    /// Past [`crate::limits::MAX_LINKS`].
    TooManyLinks,
    TooManyAnchors,
    /// The wrapped page came to more than [`crate::limits::MAX_LAYOUT_LINES`]
    /// lines.
    TooManyLines,
    OutOfMemory,
}

impl From<OutOfMemory> for Error {
    fn from(_: OutOfMemory) -> Self {
        Error::OutOfMemory
    }
}

impl From<url::Error> for Error {
    fn from(error: url::Error) -> Self {
        Error::Url(error)
    }
}

/// A sentence for the status line.
pub fn error_text(error: Error) -> &'static str {
    match error {
        Error::Url(inner) => url::error_text(inner),
        Error::InputTooLong => "the page is larger than this can read",
        Error::TextTooLong => "the page has more text than this can hold",
        Error::TooManyItems => "the page has more paragraphs than this can hold",
        Error::TooManyLinks => "the page has more links than this can hold",
        Error::TooManyAnchors => "the page has more anchors than this can hold",
        Error::TooManyLines => "the page has more lines than this can lay out",
        Error::OutOfMemory => "out of memory while reading the page",
    }
}

/// A short name, for one-line statistics on the UART.
pub fn error_name(error: Error) -> &'static str {
    match error {
        Error::Url(_) => "url",
        Error::InputTooLong => "input-limit",
        Error::TextTooLong => "text-limit",
        Error::TooManyItems => "item-limit",
        Error::TooManyLinks => "link-limit",
        Error::TooManyAnchors => "anchor-limit",
        Error::TooManyLines => "line-limit",
        Error::OutOfMemory => "out-of-memory",
    }
}
