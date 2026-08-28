//! Reading a `file:` URL: a file off a mounted volume, or a directory as a
//! page of links.
//!
//! The counterpart to [`super::fetch`], and deliberately shaped like it: a
//! [`LocalRead`] is started, stepped a bounded amount at a time, and closed
//! exactly once. The shape is not for symmetry's sake -- it is because the
//! viewer's frame loop is the same loop either way, and a read that took a
//! megabyte in one go would hold it for far longer than a frame. The C6
//! link dies rather than slows when it is not read often enough
//! (`docs/APPS.md`), so "bounded work, then return" is the rule here too.
//!
//! What this does *not* share with `fetch` is a socket, a resolver, a
//! redirect chain or a status code. There is no server: a path either names
//! something on a mounted volume or it does not.
//!
//! There is no writing. `file:` is a way to read what is already on a card
//! or a stick, and a viewer that could write would need a form, a method
//! and a confirmation none of which exist.

use alloc::string::String;

use crate::browser::document::{Document, Parser};
use crate::browser::error::{self, Error};
use crate::browser::limits::MAX_DECODED_HTML_BYTES;
use crate::browser::memory;
use crate::browser::url::Url;
use crate::fs::Devices;
use crate::fs::vfs::{EntryKind, FileHandle, FsError, OpenMode, Vfs};

use super::fetch::{Failure, Outcome};

/// Bytes moved in one [`LocalRead::step`].
///
/// The same budget the network path uses, for the same reason: whatever the
/// medium can deliver, the caller gets its loop back inside a frame. A card
/// reading at a few megabytes a second spends single-digit milliseconds
/// here.
const BYTES_PER_STEP: usize = 16 * 1024;

/// How many entries a directory listing shows before it stops.
///
/// A page rather than a file browser: a directory with more entries than
/// this is one nobody is going to read to the end of in a viewer with no
/// search. The page says it was cut rather than pretending it was short.
const MAX_LISTED_ENTRIES: usize = 512;

/// One `file:` URL being read.
#[must_use = "a LocalRead owns a file handle and has to be closed"]
pub struct LocalRead {
    handle: FileHandle,
    parser: Parser,
    read: usize,
}

impl LocalRead {
    /// Opens what `url` names, or builds the whole page at once when it
    /// names a directory.
    ///
    /// Directories come back finished because a listing is bounded by the
    /// directory and a file is not: the entries are read once, in one call,
    /// and there is nothing left to step. A file gets a handle and is read
    /// a piece at a time below.
    pub fn start(url: &Url, vfs: &mut Vfs, devices: &mut Devices) -> Result<Started, Failure> {
        let path = url.path();
        let metadata = vfs
            .metadata(devices, path)
            .map_err(|error| failure(error, NOT_FOUND))?;
        if metadata.kind == EntryKind::Directory {
            // Spelled with a trailing slash before anything is built with
            // it. Every name on the page below is a relative link, and a
            // relative link resolves against the base up to its last `/`:
            // against `file:///tmp` the name `notes.txt` is
            // `file:///notes.txt`, one level too high, and against
            // `file:///tmp/` it is the file that was clicked. The address
            // the reader sees comes from the same value, so the two cannot
            // disagree.
            let base = url
                .as_directory()
                .map_err(|error| document_failure(error.into()))?;
            return match listing(&base, vfs, devices) {
                Ok(document) => Ok(Started::Page(document)),
                Err(failure) => Err(failure),
            };
        }
        // Refused on the size rather than after reading two megabytes of
        // it. The number is the browser's own input limit, so a file that
        // is too big fails for the same reason and at the same size a page
        // off the network would.
        if metadata.size > MAX_DECODED_HTML_BYTES as u64 {
            return Err(TOO_LARGE);
        }
        let handle = vfs
            .open(devices, path, OpenMode::Read)
            .map_err(|error| failure(error, NOT_FOUND))?;
        let parser = match build_parser(url) {
            Ok(parser) => parser,
            Err(_) => {
                vfs.close(handle);
                return Err(OUT_OF_MEMORY);
            }
        };
        Ok(Started::Reading(LocalRead {
            handle,
            parser,
            read: 0,
        }))
    }

    /// Bytes read so far, for the toolbar's progress.
    pub fn received(&self) -> usize {
        self.read
    }

    pub fn peak_owned(&self) -> usize {
        self.parser.owned_bytes()
    }

    /// Reads at most [`BYTES_PER_STEP`] and hands them to the parser.
    pub fn step(&mut self, vfs: &mut Vfs, devices: &mut Devices) -> Outcome {
        let mut buffer = [0u8; BYTES_PER_STEP];
        let count = match vfs.read(devices, &self.handle, &mut buffer) {
            Ok(count) => count,
            Err(error) => return Outcome::Failed(failure(error, READ_FAILED)),
        };
        if count == 0 {
            // End of file. The parser is finished here rather than on a
            // separate call, so there is one place a document is built.
            return self.finish();
        }
        self.read += count;
        match self.parser.feed(&buffer[..count]) {
            Ok(()) => Outcome::Working,
            Err(error) => Outcome::Failed(document_failure(error)),
        }
    }

    fn finish(&mut self) -> Outcome {
        // Replaced rather than moved out: `step` takes `&mut self`, and a
        // `LocalRead` still owns its handle until `close`.
        let parser = match build_empty_parser() {
            Ok(parser) => core::mem::replace(&mut self.parser, parser),
            Err(_) => return Outcome::Failed(OUT_OF_MEMORY),
        };
        match parser.finish() {
            Ok(document) => Outcome::Page(document),
            Err(error) => Outcome::Failed(document_failure(error)),
        }
    }

    /// Gives the file handle back. The only way to do so, and it takes
    /// `self` so that forgetting is visible at the call site.
    pub fn close(self, vfs: &mut Vfs) {
        vfs.close(self.handle);
    }
}

/// What [`LocalRead::start`] came to.
pub enum Started {
    /// A file, opened and ready to be stepped.
    Reading(LocalRead),
    /// A directory, listed and finished in one go.
    Page(Document),
}

/// The parser for a file, chosen by what its name ends in.
///
/// The extension and nothing else: there is no `Content-Type` on a
/// filesystem, and sniffing the first bytes would mean holding them back
/// and guessing. A `.html` file is markup and everything else is text --
/// which is the safe way round, because text shown as markup silently
/// loses everything between its angle brackets, while markup shown as text
/// merely looks like markup.
fn build_parser(url: &Url) -> Result<Parser, Error> {
    if is_markup(url.path()) {
        Parser::new(url.clone())
    } else {
        Parser::plain(url.clone())
    }
}

fn is_markup(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
}

/// A parser to leave behind when the real one is consumed by `finish`.
fn build_empty_parser() -> Result<Parser, Error> {
    Parser::plain(Url::parse("file:///")?)
}

/// Builds a directory's page: one link per entry.
///
/// Markup put through the ordinary tokenizer, the same as the viewer's
/// error page. A listing drawn by hand would be the one page whose wrapping
/// and hit testing nothing else exercises.
fn listing(url: &Url, vfs: &Vfs, devices: &mut Devices) -> Result<Document, Failure> {
    // `url` is the directory form, so its path ends in `/`. The heading,
    // the `..` link and the listing itself all come off it rather than off
    // a second copy, which is what stops the page from naming one directory
    // and linking into another. `Vfs::list` treats the trailing slash as an
    // empty component and ignores it.
    let mut markup = String::new();
    build_listing(url.path(), vfs, devices, &mut markup).map_err(document_failure)?;
    let mut parser = Parser::new(url.clone()).map_err(document_failure)?;
    parser.feed(markup.as_bytes()).map_err(document_failure)?;
    parser.finish().map_err(document_failure)
}

fn build_listing(
    path: &str,
    vfs: &Vfs,
    devices: &mut Devices,
    markup: &mut String,
) -> Result<(), Error> {
    memory::push_str(markup, "<title>")?;
    push_escaped(markup, path)?;
    memory::push_str(markup, "</title><h1>")?;
    push_escaped(markup, path)?;
    memory::push_str(markup, "</h1><ul>")?;
    // A link up, except at the root where there is nowhere up to go.
    if path != "/" {
        memory::push_str(markup, "<li><a href=\"..\">..</a></li>")?;
    }

    let mut listed = 0usize;
    let mut overflowed = false;
    let mut failed: Option<Error> = None;
    let outcome = vfs.list(devices, path, |entry| {
        if failed.is_some() {
            return;
        }
        if listed >= MAX_LISTED_ENTRIES {
            overflowed = true;
            return;
        }
        listed += 1;
        if let Err(error) = push_entry(markup, entry.name, entry.kind, entry.size) {
            failed = Some(error);
        }
    });
    if let Some(error) = failed {
        return Err(error);
    }
    if outcome.is_err() {
        memory::push_str(markup, "<li>this directory could not be read</li>")?;
    }
    memory::push_str(markup, "</ul>")?;
    if overflowed {
        memory::push_str(
            markup,
            "<p>The listing stops here: this directory has more entries than \
             this shows.</p>",
        )?;
    }
    memory::push_str(
        markup,
        "<hr><p><a href=\"file:///\">The whole tree</a>. \
         <a href=\"http://built-in/\">Home</a></p>",
    )?;
    Ok(())
}

/// One entry, as a link. A directory's link ends in `/` so that the
/// relative references on the page it leads to resolve inside it.
fn push_entry(markup: &mut String, name: &str, kind: EntryKind, size: u64) -> Result<(), Error> {
    memory::push_str(markup, "<li><a href=\"")?;
    push_escaped(markup, name)?;
    if kind == EntryKind::Directory {
        memory::push_str(markup, "/")?;
    }
    memory::push_str(markup, "\">")?;
    push_escaped(markup, name)?;
    if kind == EntryKind::Directory {
        memory::push_str(markup, "/")?;
    }
    memory::push_str(markup, "</a>")?;
    if kind != EntryKind::Directory {
        memory::push_str(markup, " <code>")?;
        push_decimal(markup, size)?;
        memory::push_str(markup, "</code>")?;
    }
    memory::push_str(markup, "</li>")?;
    Ok(())
}

fn push_decimal(target: &mut String, value: u64) -> Result<(), Error> {
    let mut digits = [0u8; 20];
    let mut count = 0;
    let mut remaining = value;
    loop {
        digits[count] = b'0' + (remaining % 10) as u8;
        count += 1;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    for index in (0..count).rev() {
        memory::push_char(target, digits[index] as char)?;
    }
    Ok(())
}

/// Escapes the characters that would otherwise be markup.
///
/// A file name may legally contain `<`, `&` and `"`, and one of them is
/// inside an attribute here. Escaping all three is cheaper than reasoning
/// about which names can.
fn push_escaped(target: &mut String, text: &str) -> Result<(), Error> {
    for character in text.chars() {
        match character {
            '<' => memory::push_str(target, "&lt;")?,
            '&' => memory::push_str(target, "&amp;")?,
            '"' => memory::push_str(target, "&quot;")?,
            _ => memory::push_char(target, character)?,
        }
    }
    Ok(())
}

/// A filesystem error as something the reader is shown.
///
/// `NotFound` and the rest each keep their own sentence, because "cannot
/// read that file" leaves somebody guessing between a typo, an unmounted
/// volume and a card that was pulled out.
fn failure(error: FsError, fallback: Failure) -> Failure {
    match error {
        FsError::NotFound | FsError::Path(_) => NOT_FOUND,
        FsError::NotMounted => NOT_MOUNTED,
        FsError::NotAFile => NOT_A_FILE,
        FsError::DeviceNotPresent | FsError::StaleHandle => GONE,
        _ => fallback,
    }
}

fn document_failure(error: Error) -> Failure {
    Failure {
        name: error::error_name(error),
        headline: "Cannot show this file",
        detail: error::error_text(error),
        status: None,
    }
}

pub const NOT_FOUND: Failure = Failure::new(
    "file-not-found",
    "No such file",
    "Nothing is at that path. `ls` from the shell lists what is, and `mounts` \
     lists the volumes that are attached.",
);
pub const NOT_MOUNTED: Failure = Failure::new(
    "file-not-mounted",
    "Nothing is mounted there",
    "That path is not on any attached volume. USB storage mounts itself; a \
     card needs `mount` from the shell.",
);
pub const NOT_A_FILE: Failure = Failure::new(
    "file-not-a-file",
    "Not a file",
    "That path names something this cannot show as a page.",
);
pub const GONE: Failure = Failure::new(
    "file-gone",
    "The volume went away",
    "The card or the stick this was on has been removed, or replaced by \
     another.",
);
pub const READ_FAILED: Failure = Failure::new(
    "file-read",
    "Cannot read that file",
    "The volume answered, but not with the bytes that were asked for.",
);
pub const TOO_LARGE: Failure = Failure::new(
    "file-too-large",
    "That file is too large",
    "It is larger than this viewer will read into memory. The limit is the \
     same one a page off the network gets.",
);
pub const OUT_OF_MEMORY: Failure = Failure::new(
    "out-of-memory",
    "Out of memory",
    "There was not enough heap left to read this file.",
);
