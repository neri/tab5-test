//! The hypertext viewer's screen: toolbar, page, status line.
//!
//! The screen is three fixed bands:
//!
//! ```text
//!  y=0    ┌───────────────────────────────────────────────┐
//!         │ INSECURE HTTP  http://host/page      12 links │  toolbar
//!  y=40   ├───────────────────────────────────────────────┤
//!         │ A heading                                     │
//!         │                                               │  viewport
//!         │ Body text, wrapped to the width of the screen  │
//!         │ and drawn a line at a time.                    │
//!  y=688  ├───────────────────────────────────────────────┤
//!         │ http://host/where-the-selected-link-goes      │  status
//!  y=720  └───────────────────────────────────────────────┘
//! ```
//!
//! **Scrolling moves by whole lines, not by pixels.** The topmost drawn
//! line always starts exactly at the top of the viewport, and a line that
//! would not fit at the bottom is not drawn at all. That is a real
//! difference from a desktop browser -- there is no half-line at either
//! edge -- and it is worth it: the alternative is clipping every glyph
//! against two horizontal edges, in a font renderer that currently clips
//! against the panel and nothing else. Line heights vary (headings are
//! larger), so "scroll by one line" moves by different amounts in different
//! parts of a page, which reads perfectly naturally.
//!
//! **A page is only ever shown complete.** While one is arriving the
//! previous one stays on screen and only the toolbar's byte count moves; the
//! swap happens in one step when the body has ended and the document has
//! been built. Nothing partial is ever displayed, because a page that
//! stopped halfway looks exactly like a page that ended there.
//!
//! **The loop never blocks.** Name resolution and the transfer are both
//! polled a little at a time from the frame loop (`net::dns::Query` and
//! `net::http::Transaction`), so Escape is answered within a frame however
//! slow or dead the other end is. That is most of why those two types
//! exist.
//!
//! The pointer comes from `super::pointer`, shared with `win`, and the
//! drawing order it documents is obeyed exactly: lift the cursor, draw what
//! changed, put the cursor back, write back the union.

use alloc::string::String;
use alloc::vec::Vec;

use crate::browser::document::{Document, Marker, Parser, STYLE_BOLD, STYLE_CODE, STYLE_ITALIC};
use crate::browser::error::{self, Error};
use crate::browser::layout::{Layout, Line, Metrics};
use crate::browser::limits::{MAX_HISTORY, MAX_URL_BYTES};
use crate::browser::memory;
use crate::browser::url::{self, Url};
use crate::framebuffer::{BLACK, Framebuffer, HEIGHT, WHITE, WIDTH};
use crate::input::{InputManager, Key, PrimaryTouch};
use crate::net;
use crate::usb::MOUSE_BUTTON_LEFT;
use crate::{interrupts, tick, uart, wifi};

use super::fetch::{self, Fetch, Network, Outcome as FetchOutcome};

use super::pointer::{CURSOR_DRAWN_HEIGHT, CURSOR_DRAWN_WIDTH, Cursor, flush_union};

/// The 5x7 font in its 6x8 advance box, which is what the layout measures
/// against.
///
/// `CELL_HEIGHT` is the glyph's box, not a line's: the layout adds a gap
/// below every line (`layout::LINE_GAP_PERCENT`), so `Line::height` is the
/// larger of the two and is what the viewport steps by.
const CELL_WIDTH: usize = 6;
const CELL_HEIGHT: usize = 8;

const TOOLBAR_HEIGHT: usize = 40;
const STATUS_HEIGHT: usize = 32;
const VIEWPORT_TOP: usize = TOOLBAR_HEIGHT;
const VIEWPORT_BOTTOM: usize = HEIGHT - STATUS_HEIGHT;
const VIEWPORT_HEIGHT: usize = VIEWPORT_BOTTOM - VIEWPORT_TOP;
/// Left and right margin inside the viewport.
const MARGIN: usize = 12;
const PAGE_WIDTH: usize = WIDTH - 2 * MARGIN;

/// Toolbar and status text scale. The same as body text: chrome that is
/// harder to read than the page is chrome nobody reads.
const CHROME_SCALE: usize = 2;
const CHROME_CELL: usize = CELL_WIDTH * CHROME_SCALE;
const CHROME_TEXT_Y: usize = (TOOLBAR_HEIGHT - CELL_HEIGHT * CHROME_SCALE) / 2;
const STATUS_TEXT_Y: usize = VIEWPORT_BOTTOM + (STATUS_HEIGHT - CELL_HEIGHT * CHROME_SCALE) / 2;

/// The cleartext warning, and where the address field starts after it.
const BADGE: &str = "INSECURE HTTP";
const ADDRESS_LEFT: usize = MARGIN + (BADGE.len() + 2) * CHROME_CELL;
/// Room kept at the right for the link and line counts.
const SUMMARY_CELLS: usize = 26;
const ADDRESS_RIGHT: usize = WIDTH - MARGIN - SUMMARY_CELLS * CHROME_CELL;
const ADDRESS_CELLS: usize = (ADDRESS_RIGHT - ADDRESS_LEFT) / CHROME_CELL;

const PAGE_BACKGROUND: u16 = WHITE;
const TEXT_COLOR: u16 = BLACK;
/// Pure blue on white, which the panel renders cleanly at this size.
const LINK_COLOR: u16 = 0x001F;
const CODE_COLOR: u16 = 0x0320;
/// Dark red. A grey was tried first and could not be told from black at
/// this size: two near-blacks in a 5x7 font read as a rendering fault
/// rather than as emphasis. Emphasis has to differ in hue, not in
/// brightness.
const ITALIC_COLOR: u16 = 0x9000;
const RULE_COLOR: u16 = 0x8410;
const CHROME_BACKGROUND: u16 = 0xC618;
const CHROME_TEXT: u16 = BLACK;
/// Status-line messages, in the same red as the cleartext badge: almost
/// every one of them is the viewer refusing to do something.
const MESSAGE_COLOR: u16 = 0x9000;
/// The cleartext warning. Red, permanently, and never conditional: there is
/// no TLS here, so there is no state in which it should be absent.
const INSECURE_COLOR: u16 = 0xF800;
/// The address field while it is being edited.
const EDIT_BACKGROUND: u16 = WHITE;
const EDIT_CARET: u16 = 0x001F;

/// The path the viewer's own error page lives at, under the built-in host.
const ERROR_PATH: &str = "/error";

/// Non-ASCII characters have no glyph in the 5x7 font and are drawn as a
/// hollow box; this is its inset inside the cell, in unscaled pixels.
const PLACEHOLDER_INSET: usize = 1;

/// Lines one wheel detent scrolls.
const WHEEL_LINES: i32 = 3;

/// Drawn lines between two chances for the C6 link to be read.
///
/// A screenful is about thirty-five lines of body text, so this is a
/// handful of reads spread through the glyph drawing. Reading when there is
/// nothing waiting costs a couple of SDIO register reads, so erring small
/// is nearly free and erring large is a lost link.
const LINES_PER_SERVICE: usize = 8;

/// Width of one band of the viewport writeback, in logical pixels.
const FLUSH_BAND_WIDTH: usize = 128;

/// Runs the viewer until Escape is pressed on a page.
///
/// `rpc` and `stack` are borrowed for the whole screen but owned by
/// `app::run`; the browser polls them and gives them back every frame
/// rather than holding them across one. Both being `None`, or the stack
/// having no address, is not an error -- the built-in pages still work, and
/// a link that needs the network says so.
pub fn run(
    framebuffer: &mut Framebuffer,
    input: &mut InputManager,
    rpc: Option<&mut wifi::Rpc>,
    stack: Option<&mut net::Stack>,
    base: Option<Url>,
    start: Option<Url>,
) {
    let mut network = Network::new(rpc, stack);
    let mut viewer = match Viewer::new(base) {
        Ok(viewer) => viewer,
        Err(failure) => {
            uart::log(b"Browser: the built-in home page did not parse: ");
            uart::log(error::error_name(failure).as_bytes());
            uart::log(b"\r\n");
            return;
        }
    };
    if network.is_none() {
        viewer.say("no network: leave and run wificonnect, then ipconfig dhcp");
    }
    if let Some(url) = start {
        viewer.request(Navigation::fresh(url));
    }

    framebuffer.fill(PAGE_BACKGROUND);
    viewer.draw_all(framebuffer, &mut || service_link(&mut network));
    let mut cursor = Cursor::new(WIDTH / 2, HEIGHT / 2);
    input.reset_primary_touch();
    cursor.show(framebuffer);
    if !framebuffer.flush() {
        uart::log(b"Browser: initial flush failed\r\n");
        return;
    }

    let mut pending: Option<Pending> = None;
    let mut sequence = interrupts::frame_sequence();
    loop {
        if interrupts::dma_error() != 0 {
            uart::log(b"Browser: DMA interrupt error\r\n");
            break;
        }
        interrupts::wait_for_interrupt();
        let next_sequence = interrupts::frame_sequence();
        if next_sequence == sequence {
            // Not a frame boundary: the cheap input maintenance only. This
            // is the wake that keeps a USB keyboard alive between frames.
            input.service_fast();
            continue;
        }
        sequence = next_sequence;
        input.service();
        // Before anything else this frame. The C6 keeps received frames
        // until the host reads them, and a backlog larger than the
        // transport's staging buffer cannot be resynchronized -- the link
        // is gone for good at that point. The window that has to stay
        // small is "time between two reads", so the read comes first and
        // the expensive work below is broken up around more of them.
        service_link(&mut network);

        let mut leaving = false;
        while let Some(event) = input.poll_key() {
            match viewer.handle_key(event.key, pending.is_some()) {
                Action::Continue => {}
                Action::Cancel => {
                    if let (Some(active), Some(link)) = (pending.take(), network.as_mut()) {
                        active.fetch.close(link);
                        viewer.finish_loading();
                        viewer.say("stopped");
                    }
                }
                Action::Leave => leaving = true,
            }
        }
        if leaving {
            break;
        }

        let touch = input.poll_primary_touch();
        let motion = input.poll_mouse();
        let (target_x, target_y) = match touch {
            PrimaryTouch::Pressed(point) | PrimaryTouch::Moved(point) => (point.x, point.y),
            PrimaryTouch::Idle | PrimaryTouch::Released => match motion {
                Some(update) => cursor.moved_to(update.dx, update.dy),
                None => (cursor.x, cursor.y),
            },
        };
        let pointer_moved = (target_x, target_y) != (cursor.x, cursor.y);

        if let PrimaryTouch::Pressed(point) = touch {
            viewer.click(point.x, point.y);
        }
        if let Some(update) = motion {
            if update.pressed & MOUSE_BUTTON_LEFT != 0 {
                viewer.click(target_x, target_y);
            }
            if update.wheel != 0 {
                viewer.scroll_by(-update.wheel * WHEEL_LINES);
            }
        }

        // A link that has died is otherwise only discovered by trying to
        // use it, which from the reader's side looks like every page
        // failing for its own reason.
        if let Some(link) = network.as_ref()
            && !link.rpc.is_alive()
        {
            viewer.report_lost_link();
        }

        // A navigation the reader asked for. Whatever is in flight is
        // abandoned first: the newest request is the one they meant, and
        // this is also what makes "cancel, then fetch something else"
        // work without a state in between.
        if let Some(navigation) = viewer.take_request() {
            if let (Some(active), Some(link)) = (pending.take(), network.as_mut()) {
                active.fetch.close(link);
            }
            pending = begin(&mut viewer, navigation, network.as_mut());
        }

        let outcome = match (pending.as_mut(), network.as_mut()) {
            (Some(active), Some(link)) => {
                let outcome = active.fetch.step(link);
                viewer.update_loading(active.fetch.received());
                Some(outcome)
            }
            _ => None,
        };
        match outcome {
            None | Some(FetchOutcome::Working) => {}
            Some(FetchOutcome::Page(document)) => {
                if let Some(active) = pending.take() {
                    let elapsed = active.fetch.elapsed_ms();
                    // The final address, which is the last hop of a
                    // redirect chain rather than the one that was asked
                    // for -- so the toolbar and the base for this page's
                    // links are both where the page actually came from.
                    let landed = active.fetch.url().clone();
                    let peak = active.fetch.peak_owned();
                    if let Some(link) = network.as_mut() {
                        active.fetch.close(link);
                    }
                    viewer.show_document(document, &active.navigation, landed, elapsed, peak);
                }
            }
            Some(FetchOutcome::Failed(failure)) => {
                if let Some(active) = pending.take() {
                    let url = active.fetch.url().clone();
                    if let Some(link) = network.as_mut() {
                        active.fetch.close(link);
                    }
                    viewer.show_failure(&url, failure);
                }
            }
        }

        if !viewer.dirty() && !pointer_moved {
            continue;
        }

        // The pointer is topmost, so it comes off before anything repaints
        // and goes back on last -- see `super::pointer`.
        let (previous_x, previous_y) = (cursor.x, cursor.y);
        cursor.hide(framebuffer);
        viewer.draw_dirty(framebuffer, &mut || service_link(&mut network));
        cursor.move_to(target_x, target_y);
        cursor.show(framebuffer);
        flush_union(
            framebuffer,
            (previous_x, previous_y),
            (cursor.x, cursor.y),
            CURSOR_DRAWN_WIDTH,
            CURSOR_DRAWN_HEIGHT,
        );
    }

    // Leaving with a transfer still running would leak its socket out of
    // the set for the rest of the run. Every exit from the loop above is a
    // `break` so that this is the only way out.
    if let (Some(active), Some(link)) = (pending.take(), network.as_mut()) {
        active.fetch.close(link);
    }
}

/// Reads whatever the C6 has waiting.
///
/// Called wherever this screen is about to spend longer than a frame not
/// looking at the link -- which is most of a viewport repaint. Cheap when
/// there is nothing waiting: a couple of SDIO register reads.
fn service_link(network: &mut Option<Network<'_>>) {
    if let Some(link) = network.as_mut() {
        link.stack.poll(link.rpc);
    }
}

/// Starts a navigation, or answers it without the network when it can.
fn begin(
    viewer: &mut Viewer,
    navigation: Navigation,
    network: Option<&mut Network<'_>>,
) -> Option<Pending> {
    if navigation.url.host() == builtin::HOST {
        match builtin::by_path(navigation.url.path()) {
            Some(page) => viewer.show_builtin(page, &navigation),
            None => viewer.show_failure(&navigation.url, fetch::NO_SUCH_BUILTIN),
        }
        return None;
    }
    if !navigation.url.scheme().is_fetchable() {
        viewer.show_failure(&navigation.url, fetch::HTTPS);
        return None;
    }
    let Some(network) = network else {
        viewer.show_failure(&navigation.url, fetch::NO_NETWORK);
        return None;
    };
    match Fetch::start(navigation.url.clone(), network) {
        Ok(fetch) => {
            viewer.begin_loading(&navigation.url);
            Some(Pending { fetch, navigation })
        }
        Err(failure) => {
            viewer.show_failure(&navigation.url, failure);
            None
        }
    }
}

/// Where a navigation came from and where it should land.
///
/// The address is the viewer's business, and so are the two things beside
/// it: `fetch::Fetch` neither knows nor cares about history or scroll
/// positions, which is why they live here and not there.
struct Navigation {
    url: Url,
    /// The line to put at the top once the page is up. Non-zero only when
    /// going back, which is a re-fetch: history keeps a scroll position but
    /// never a document.
    restore: usize,
    /// Whether the page being left should be pushed onto history. False for
    /// `back` itself, which is what stops the two from fighting.
    push_history: bool,
}

impl Navigation {
    fn fresh(url: Url) -> Navigation {
        Navigation {
            url,
            restore: 0,
            push_history: true,
        }
    }
}

/// A fetch together with what the viewer wants done when it lands.
struct Pending {
    fetch: Fetch,
    navigation: Navigation,
}

enum Action {
    Continue,
    /// Escape while a page is arriving.
    Cancel,
    Leave,
}

/// What has changed and therefore needs repainting.
///
/// Tracked per band rather than as one flag because the bands cost wildly
/// different amounts: the status line is a strip of a few thousand pixels
/// and the viewport is most of the screen. A `Tab` that only moves the
/// focus repaints two of them; a pointer that only moves repaints none.
#[derive(Default, Clone, Copy)]
struct Dirty {
    toolbar: bool,
    viewport: bool,
    status: bool,
}

/// One displayed page. Replaced wholesale on every navigation, which is
/// what drops the previous document, its layout and its links together.
struct Page {
    document: Document,
    layout: Layout,
    /// Index of the topmost drawn line.
    first_line: usize,
    /// Position within `order`, not a link index.
    focus: Option<usize>,
    /// Links in the order they are laid out, which is the order `Tab`
    /// visits them.
    order: Vec<u16>,
}

/// A page that can be gone back to.
///
/// The URL and a scroll position, never a document: going back re-fetches.
/// That is what makes eight entries cost a few kilobytes instead of eight
/// parsed pages, and it is why `MAX_HISTORY` can be a small number without
/// anybody minding.
struct HistoryEntry {
    url: Url,
    line: usize,
}

/// The address field while it is being typed into.
///
/// It opens holding the current address with the caret at the end, and
/// editing it is ordinary text editing. It used to open fully selected, so
/// that the first character typed replaced everything -- which is right for
/// a desktop browser, where the common action is pasting a whole new
/// address, and wrong here, where the common action is changing the end of
/// the one already showing and retyping it on a thumb keyboard is the
/// expensive part.
struct Editing {
    /// ASCII only -- non-ASCII is never accepted -- so a byte offset and a
    /// character offset are the same number, which is what lets the caret
    /// be a single `usize`.
    text: String,
    caret: usize,
}

impl Editing {
    fn insert(&mut self, character: char) {
        if self.text.len() >= MAX_URL_BYTES {
            return;
        }
        self.caret = self.caret.min(self.text.len());
        self.text.insert(self.caret, character);
        self.caret += 1;
    }

    /// Deletes the character before the caret.
    fn backspace(&mut self) {
        if self.caret == 0 || self.caret > self.text.len() {
            return;
        }
        self.caret -= 1;
        self.text.remove(self.caret);
    }

    /// Deletes the character at the caret.
    fn delete(&mut self) {
        if self.caret >= self.text.len() {
            return;
        }
        self.text.remove(self.caret);
    }
}

/// What the toolbar shows while a page is arriving.
struct Loading {
    url: String,
    received: usize,
    /// Kilobytes last drawn, so the toolbar is repainted when the number
    /// changes rather than on every frame.
    shown_kib: usize,
}

struct Viewer {
    page: Page,
    /// What a partial address typed into the field completes against while
    /// a built-in page is showing -- the shell's `hbase`.
    ///
    /// Needed because the built-in pages are not a real site: resolving
    /// `/simple.html` against `http://built-in/` gives a built-in page that
    /// does not exist, which is never what somebody typing a path meant.
    /// On a fetched page the page's own address is the base, as it should
    /// be.
    base: Option<Url>,
    history: Vec<HistoryEntry>,
    editing: Option<Editing>,
    loading: Option<Loading>,
    /// A sentence for the status line. Takes priority over the focused
    /// link's target, because it is only ever set as the answer to
    /// something the reader just did.
    message: Option<String>,
    /// A navigation waiting for the loop to act on it.
    request: Option<Navigation>,
    /// How far down the viewport the last repaint actually drew.
    ///
    /// A repaint has to erase what the previous one left, and nothing more.
    /// Keeping the previous extent is what lets a short page -- the home
    /// page, an error page, most of the fixtures -- clear a couple of
    /// hundred pixels instead of the whole viewport. A full screen of text
    /// still costs what it costs.
    painted_bottom: usize,
    /// The longest viewport repaint seen, in milliseconds.
    ///
    /// Kept so the UART gets a line only when a new worst is set rather
    /// than on every scroll. What it is watching for is the plan's own
    /// stopping condition: a repaint that holds the loop for longer than a
    /// frame is one that has to be broken up further, and the way to know
    /// is to have measured it on the panel rather than estimated it.
    slowest_repaint_ms: u64,
    /// Whether the link was already reported as lost, so it is said once.
    link_reported: bool,
    dirty: Dirty,
}

impl Viewer {
    fn new(base: Option<Url>) -> Result<Viewer, Error> {
        let page = load_builtin(builtin::HOME)?;
        Ok(Viewer {
            page,
            base,
            history: Vec::new(),
            editing: None,
            loading: None,
            message: None,
            request: None,
            painted_bottom: VIEWPORT_BOTTOM,
            slowest_repaint_ms: 0,
            link_reported: false,
            dirty: Dirty {
                toolbar: true,
                viewport: true,
                status: true,
            },
        })
    }

    fn dirty(&self) -> bool {
        self.dirty.toolbar || self.dirty.viewport || self.dirty.status
    }

    fn take_request(&mut self) -> Option<Navigation> {
        self.request.take()
    }

    fn request(&mut self, navigation: Navigation) {
        self.request = Some(navigation);
    }

    /// Sets the status line. Silently keeps the old one if the allocation
    /// fails: a browser that cannot report an error because it could not
    /// allocate the report is not improved by trying harder.
    fn say(&mut self, text: &str) {
        if let Ok(owned) = memory::string_from(text) {
            self.message = Some(owned);
            self.dirty.status = true;
        }
    }

    /// Says once that the link is gone.
    fn report_lost_link(&mut self) {
        if self.link_reported {
            return;
        }
        self.link_reported = true;
        self.say("the Wi-Fi link is gone; leave and run wificonnect again");
    }

    fn clear_message(&mut self) {
        if self.message.is_some() {
            self.message = None;
            self.dirty.status = true;
        }
    }

    // --- navigation ---------------------------------------------------

    fn begin_loading(&mut self, url: &Url) {
        let text = url.to_text().unwrap_or_default();
        self.loading = Some(Loading {
            url: text,
            received: 0,
            shown_kib: usize::MAX,
        });
        self.message = None;
        self.dirty.toolbar = true;
        self.dirty.status = true;
    }

    fn update_loading(&mut self, received: usize) {
        let Some(loading) = self.loading.as_mut() else {
            return;
        };
        loading.received = received;
        let kib = received / 1024;
        if kib != loading.shown_kib {
            loading.shown_kib = kib;
            self.dirty.toolbar = true;
        }
    }

    fn finish_loading(&mut self) {
        if self.loading.take().is_some() {
            self.dirty.toolbar = true;
            self.dirty.status = true;
        }
    }

    /// Puts a finished document on screen.
    ///
    /// `landed` is where the page actually came from, which is the last hop
    /// of a redirect chain rather than the address that was asked for.
    fn show_document(
        &mut self,
        document: Document,
        navigation: &Navigation,
        landed: Url,
        elapsed_ms: u64,
        peak_owned: usize,
    ) {
        let statistics = document.stats();
        match build_page(document) {
            Ok(page) => {
                if navigation.push_history {
                    self.push_current();
                }
                self.page = page;
                // Through `scroll_to` rather than assigned, so a remembered
                // position past the end of a page that has since got
                // shorter lands on the last screen instead of below it.
                self.scroll_to(navigation.restore);
                self.loading = None;
                self.message = None;
                self.dirty = Dirty {
                    toolbar: true,
                    viewport: true,
                    status: true,
                };
                let _ = landed;
                log_page(&self.page, statistics, elapsed_ms, peak_owned);
            }
            Err(failure) => {
                let url = navigation.url.clone();
                self.show_failure(
                    &url,
                    fetch::Failure {
                        name: error::error_name(failure),
                        headline: "Cannot lay out this page",
                        detail: error::error_text(failure),
                        status: None,
                    },
                );
            }
        }
    }

    fn show_builtin(&mut self, page: &'static builtin::Page, navigation: &Navigation) {
        match load_builtin(page) {
            Ok(loaded) => {
                if navigation.push_history {
                    self.push_current();
                }
                self.page = loaded;
                self.scroll_to(navigation.restore);
                self.loading = None;
                self.message = None;
                self.dirty = Dirty {
                    toolbar: true,
                    viewport: true,
                    status: true,
                };
            }
            Err(failure) => self.say(error::error_text(failure)),
        }
    }

    /// Replaces the page with the viewer's own explanation of a failure.
    ///
    /// A page rather than a status line, because a failed navigation has to
    /// leave somewhere to go: this one carries the address that failed, the
    /// reason, and a link home. Backspace still goes back.
    fn show_failure(&mut self, url: &Url, failure: fetch::Failure) {
        self.show_error(url, failure.headline, failure.detail, failure.status);
    }

    fn show_error(&mut self, url: &Url, headline: &str, detail: &str, status: Option<u16>) {
        self.loading = None;
        // The page the reader was on when they followed the failing link is
        // pushed, so Backspace from the error page returns to it. Not when
        // this error replaces another one: that would stack duplicates and
        // put a wall of error pages between them and where they were.
        if !self.showing_error() {
            self.push_current();
        }
        match error_page(url, headline, detail, status) {
            Ok(page) => {
                self.page = page;
                self.message = None;
                self.dirty = Dirty {
                    toolbar: true,
                    viewport: true,
                    status: true,
                };
            }
            // Even the error page could not be built. The status line is
            // all that is left, and it is enough to say what happened.
            Err(_) => self.say(headline),
        }
    }

    fn showing_error(&self) -> bool {
        let url = self.page.document.url();
        url.host() == builtin::HOST && url.path() == ERROR_PATH
    }

    /// Records the page now on screen so it can be gone back to.
    fn push_current(&mut self) {
        let entry = HistoryEntry {
            url: self.page.document.url().clone(),
            line: self.page.first_line,
        };
        if self.history.len() >= MAX_HISTORY {
            // The oldest goes, which is what makes this a bounded cost
            // rather than a growing one.
            self.history.remove(0);
        }
        let _ = memory::push(&mut self.history, entry);
    }

    fn go_back(&mut self) {
        let Some(entry) = self.history.pop() else {
            self.say("nothing to go back to");
            return;
        };
        self.request(Navigation {
            url: entry.url,
            restore: entry.line,
            // Going back must not push the page being left, or every back
            // would add an entry and the history would never shrink.
            push_history: false,
        });
    }

    // --- input --------------------------------------------------------

    fn handle_key(&mut self, key: Key, loading: bool) -> Action {
        if self.editing.is_some() {
            return self.handle_editing_key(key);
        }
        match key {
            // While a page is arriving Escape stops it; otherwise it
            // leaves. One key, and which it means is exactly what the
            // toolbar is showing at the time.
            Key::Escape => {
                return if loading { Action::Cancel } else { Action::Leave };
            }
            Key::ArrowDown => self.scroll_by(1),
            Key::ArrowUp => self.scroll_by(-1),
            Key::PageDown => self.scroll_by(self.lines_per_screen()),
            Key::PageUp => self.scroll_by(-self.lines_per_screen()),
            Key::Home => self.scroll_to(0),
            Key::End => self.scroll_to(self.last_top_line()),
            // Space pages down, the way every reader does.
            Key::Ascii(b' ') => self.scroll_by(self.lines_per_screen()),
            Key::Ascii(b'\t') => self.focus_next(),
            Key::Ascii(b'\r') | Key::Ascii(b'\n') => {
                // Enter on a selected link follows it; Enter with nothing
                // selected is "where do you want to go?". That is one key
                // doing two things, but they are never both available: a
                // reader who has not pressed Tab has nothing to follow.
                if self.page.focus.is_some() {
                    self.follow_focused();
                } else {
                    self.start_editing();
                }
            }
            Key::Ascii(0x08) | Key::Ascii(0x7F) => self.go_back(),
            Key::Function(2) => self.start_editing(),
            _ => {}
        }
        Action::Continue
    }

    fn handle_editing_key(&mut self, key: Key) -> Action {
        let Some(editing) = self.editing.as_mut() else {
            return Action::Continue;
        };
        // Every arm below changes what the field looks like, so the toolbar
        // is marked once here rather than in each of them.
        self.dirty.toolbar = true;
        match key {
            // Escape cancels the edit rather than leaving the browser: one
            // key, and the field being open says which it means.
            Key::Escape => {
                self.editing = None;
                self.clear_message();
            }
            Key::Ascii(b'\r') | Key::Ascii(b'\n') => {
                let text = core::mem::take(&mut editing.text);
                self.editing = None;
                self.navigate_to_text(&text);
            }
            Key::Ascii(0x08) => editing.backspace(),
            // A keyboard that sends DEL for its backspace key is the common
            // case; one that has a separate forward-delete sends
            // `Key::Delete`. Both are handled, and neither guesses.
            Key::Ascii(0x7F) => editing.backspace(),
            Key::Delete => editing.delete(),
            Key::ArrowLeft => editing.caret = editing.caret.saturating_sub(1),
            Key::ArrowRight => editing.caret = (editing.caret + 1).min(editing.text.len()),
            Key::Home => editing.caret = 0,
            Key::End => editing.caret = editing.text.len(),
            Key::Ascii(byte) if (0x20..0x7F).contains(&byte) => editing.insert(byte as char),
            _ => self.dirty.toolbar = false,
        }
        Action::Continue
    }

    fn start_editing(&mut self) {
        if self.editing.is_some() {
            return;
        }
        // The address that is showing, with the caret after it. Changing
        // the end of an address is the usual reason to open this at all.
        let text = self.page.document.url().to_text().unwrap_or_default();
        let caret = text.len();
        self.editing = Some(Editing { text, caret });
        self.page.focus = None;
        self.say("edit the address; Enter goes, Escape cancels");
        self.dirty.toolbar = true;
        self.dirty.viewport = true;
    }

    /// Takes what was typed and turns it into a navigation.
    ///
    /// Resolved against the current page, so a bare path works the same way
    /// it does in a link. Something that looks like a host -- it has a dot
    /// and does not start with one -- is given `http://` first, because
    /// `example.com/page` typed into an address field is an address and not
    /// a file in the current directory.
    fn navigate_to_text(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.say("no address typed");
            return;
        }
        let mut buffer = String::new();
        let looks_like_a_host = url::classify(trimmed) == url::Reference::Relative
            && trimmed.contains('.')
            && !trimmed.starts_with('.');
        let candidate = if looks_like_a_host {
            if memory::push_str(&mut buffer, "http://").is_err()
                || memory::push_str(&mut buffer, trimmed).is_err()
            {
                self.say("out of memory");
                return;
            }
            buffer.as_str()
        } else {
            trimmed
        };
        let base = match (&self.base, self.page.document.url()) {
            (Some(base), current) if current.host() == builtin::HOST => base,
            (_, current) => current,
        };
        match base.resolve(candidate) {
            Ok(target) => self.request(Navigation::fresh(target)),
            Err(error) => self.say(url::error_text(error)),
        }
    }

    /// A tap or a click at a screen position.
    fn click(&mut self, x: usize, y: usize) {
        if y < TOOLBAR_HEIGHT {
            if (ADDRESS_LEFT..ADDRESS_RIGHT).contains(&x) {
                self.start_editing();
            }
            return;
        }
        if y >= VIEWPORT_BOTTOM {
            return;
        }
        if self.editing.is_some() {
            self.editing = None;
            self.clear_message();
            self.dirty.toolbar = true;
        }
        let Some(top) = self.top_offset() else {
            return;
        };
        let document_y = top + (y - VIEWPORT_TOP) as u32;
        let Some(document_x) = x.checked_sub(MARGIN) else {
            return;
        };
        match self.page.layout.hit(document_x as u16, document_y) {
            Some(link) => {
                self.page.focus = self.page.order.iter().position(|&index| index == link);
                self.clear_message();
                self.dirty.viewport = true;
                self.dirty.status = true;
                self.follow_focused();
            }
            None => {
                if self.page.focus.is_some() {
                    self.page.focus = None;
                    self.clear_message();
                    self.dirty.viewport = true;
                    self.dirty.status = true;
                }
            }
        }
    }

    fn focus_next(&mut self) {
        if self.page.order.is_empty() {
            self.say("this page has no links");
            return;
        }
        self.page.focus = Some(match self.page.focus {
            Some(position) => (position + 1) % self.page.order.len(),
            None => 0,
        });
        self.clear_message();
        self.scroll_focus_into_view();
        self.dirty.viewport = true;
        self.dirty.status = true;
    }

    fn follow_focused(&mut self) {
        let Some(link) = self.focused_link() else {
            self.say("no link selected; press Tab");
            return;
        };
        let Some(target) = self.page.document.link(link) else {
            return;
        };
        let url = target.url.clone();
        self.request(Navigation::fresh(url));
    }

    fn focused_link(&self) -> Option<u16> {
        self.page.order.get(self.page.focus?).copied()
    }

    // --- scrolling ----------------------------------------------------

    /// The document-space y of the top of the viewport.
    fn top_offset(&self) -> Option<u32> {
        Some(self.page.layout.lines().get(self.page.first_line)?.y)
    }

    /// How many lines a page-sized jump moves.
    ///
    /// Measured from the current position rather than from an average line
    /// height, because a page of headings and a page of prose hold very
    /// different numbers of lines.
    fn lines_per_screen(&self) -> i32 {
        let Some(top) = self.top_offset() else {
            return 1;
        };
        let range = self.page.layout.visible(top, VIEWPORT_HEIGHT as u32);
        // One line of overlap, so the reader keeps their place across a
        // page turn.
        (range.end.saturating_sub(range.start).saturating_sub(1)).max(1) as i32
    }

    fn scroll_by(&mut self, lines: i32) {
        let target = self.page.first_line as i64 + lines as i64;
        self.scroll_to(target.clamp(0, self.last_top_line() as i64) as usize);
    }

    fn scroll_to(&mut self, line: usize) {
        let line = line.min(self.last_top_line());
        if line != self.page.first_line {
            self.page.first_line = line;
            self.dirty.viewport = true;
            self.dirty.toolbar = true;
        }
    }

    /// The furthest the page can scroll: the topmost line that still leaves
    /// the document's last line on screen.
    ///
    /// Computed by walking back from the end rather than by dividing the
    /// height, because line heights vary.
    fn last_top_line(&self) -> usize {
        let lines = self.page.layout.lines();
        let Some(last) = lines.last() else {
            return 0;
        };
        let bottom = last.y + last.height as u32;
        let mut index = lines.len() - 1;
        while index > 0 {
            let candidate = lines[index - 1];
            if bottom.saturating_sub(candidate.y) > VIEWPORT_HEIGHT as u32 {
                break;
            }
            index -= 1;
        }
        index
    }

    fn scroll_focus_into_view(&mut self) {
        let Some(link) = self.focused_link() else {
            return;
        };
        let Some(line) = self.page.layout.line_of_link(link) else {
            return;
        };
        if line < self.page.first_line {
            self.scroll_to(line);
            return;
        }
        let Some(top) = self.top_offset() else {
            return;
        };
        let range = self.page.layout.visible(top, VIEWPORT_HEIGHT as u32);
        // `visible` includes a line that only partly fits; a focused link
        // on it would be invisible, so it counts as off screen.
        if line + 1 >= range.end {
            let target = line.saturating_sub(self.lines_per_screen().max(1) as usize - 1);
            self.scroll_to(target.max(line.saturating_sub(4)));
        }
    }

    // --- drawing ------------------------------------------------------

    fn draw_all(&mut self, framebuffer: &mut Framebuffer, service: &mut dyn FnMut()) {
        self.dirty = Dirty {
            toolbar: true,
            viewport: true,
            status: true,
        };
        self.draw_dirty(framebuffer, service);
    }

    /// `service` is called at every point in here where the work between
    /// two calls would otherwise run past a frame.
    ///
    /// A repaint of the viewport is a PPA fill of most of the screen, some
    /// tens of thousands of scattered glyph writes and a 1.6 MB writeback,
    /// and none of it touches the network. Left alone it was long enough
    /// for the C6 to collect more than its staging buffer holds, which is
    /// not a slow link -- it is a dead one, permanently. The toolbar and
    /// the status line are small enough not to need this; the viewport is
    /// broken up below.
    fn draw_dirty(&mut self, framebuffer: &mut Framebuffer, service: &mut dyn FnMut()) {
        let dirty = self.dirty;
        self.dirty = Dirty::default();
        if dirty.toolbar {
            self.draw_toolbar(framebuffer);
            framebuffer.flush_rect(0, 0, WIDTH, TOOLBAR_HEIGHT);
        }
        if dirty.viewport {
            let started = tick::now_ms();
            let height = self.draw_viewport(framebuffer, service);
            flush_viewport(framebuffer, height, service);
            let elapsed = tick::now_ms().saturating_sub(started);
            if elapsed > self.slowest_repaint_ms {
                self.slowest_repaint_ms = elapsed;
                uart::log_hex(b"BROWSER: slowest viewport repaint so far, ms=", elapsed as u32);
            }
        }
        if dirty.status {
            self.draw_status(framebuffer);
            framebuffer.flush_rect(0, VIEWPORT_BOTTOM, WIDTH, STATUS_HEIGHT);
        }
    }

    fn draw_toolbar(&self, framebuffer: &mut Framebuffer) {
        framebuffer.fill_rect(0, 0, WIDTH, TOOLBAR_HEIGHT, CHROME_BACKGROUND);
        // The cleartext warning is first and is red, on every page, with no
        // condition attached. HTTP is the only thing this can fetch, so
        // there is no state in which the badge should be absent or a
        // different colour -- and a warning that only sometimes appears is
        // one nobody reads.
        draw_ascii(
            framebuffer,
            MARGIN,
            CHROME_TEXT_Y,
            BADGE,
            CHROME_SCALE,
            INSECURE_COLOR,
        );

        match (&self.editing, &self.loading) {
            (Some(editing), _) => self.draw_address_field(framebuffer, editing),
            (None, Some(loading)) => draw_clipped(
                framebuffer,
                ADDRESS_LEFT,
                CHROME_TEXT_Y,
                &loading.url,
                ADDRESS_CELLS,
                CHROME_TEXT,
            ),
            (None, None) => {
                if let Ok(text) = self.page.document.url().to_text() {
                    draw_clipped(
                        framebuffer,
                        ADDRESS_LEFT,
                        CHROME_TEXT_Y,
                        &text,
                        ADDRESS_CELLS,
                        CHROME_TEXT,
                    );
                }
            }
        }

        let mut summary = Summary::new();
        match &self.loading {
            Some(loading) => {
                summary.push("loading ");
                summary.push_usize(loading.received / 1024);
                summary.push(" KiB");
            }
            None => {
                summary.push_usize(self.page.order.len());
                summary.push(" links  ");
                summary.push_usize(self.page.layout.lines().len());
                summary.push(" lines");
            }
        }
        let width = summary.len() * CHROME_CELL;
        draw_ascii(
            framebuffer,
            WIDTH.saturating_sub(MARGIN + width),
            CHROME_TEXT_Y,
            summary.as_str(),
            CHROME_SCALE,
            CHROME_TEXT,
        );
    }

    /// The address field, scrolled so the caret is always on screen.
    fn draw_address_field(&self, framebuffer: &mut Framebuffer, editing: &Editing) {
        let width = ADDRESS_RIGHT - ADDRESS_LEFT;
        framebuffer.fill_rect(
            ADDRESS_LEFT - 4,
            CHROME_TEXT_Y - 4,
            width + 8,
            CELL_HEIGHT * CHROME_SCALE + 8,
            EDIT_BACKGROUND,
        );
        // One cell is kept for the caret, so it has somewhere to sit when it
        // is past the last character.
        let columns = ADDRESS_CELLS.saturating_sub(1).max(1);
        // The window follows the caret rather than the end of the text:
        // editing the middle of a long address has to be visible too.
        let first = editing.caret.saturating_sub(columns);
        let visible = editing
            .text
            .get(first..(first + columns).min(editing.text.len()))
            .unwrap_or("");
        draw_ascii(
            framebuffer,
            ADDRESS_LEFT,
            CHROME_TEXT_Y,
            visible,
            CHROME_SCALE,
            BLACK,
        );
        let caret_column = editing.caret.saturating_sub(first);
        framebuffer.fill_rect(
            ADDRESS_LEFT + caret_column * CHROME_CELL,
            CHROME_TEXT_Y,
            2,
            CELL_HEIGHT * CHROME_SCALE,
            EDIT_CARET,
        );
    }

    fn draw_status(&self, framebuffer: &mut Framebuffer) {
        framebuffer.fill_rect(0, VIEWPORT_BOTTOM, WIDTH, STATUS_HEIGHT, CHROME_BACKGROUND);
        let columns = (WIDTH - 2 * MARGIN) / CHROME_CELL;
        if self.loading.is_some() && self.message.is_none() {
            draw_ascii(
                framebuffer,
                MARGIN,
                STATUS_TEXT_Y,
                "loading; Escape stops",
                CHROME_SCALE,
                CHROME_TEXT,
            );
            return;
        }
        // A message wins over the focused link's target, because a message
        // is only ever set as the answer to something the reader just did.
        // It used to be the other way round, and the effect was that
        // pressing Enter on an https link did nothing at all: the refusal
        // was written into a field the status line then declined to show.
        // Every path that moves the focus clears the message first, so the
        // link's target comes back as soon as the reader moves on.
        if let Some(message) = &self.message {
            draw_clipped(
                framebuffer,
                MARGIN,
                STATUS_TEXT_Y,
                message,
                columns,
                MESSAGE_COLOR,
            );
            return;
        }
        if let Some(link) = self.focused_link()
            && let Some(target) = self.page.document.link(link)
            && let Ok(text) = target.url.to_text()
        {
            draw_clipped(
                framebuffer,
                MARGIN,
                STATUS_TEXT_Y,
                &text,
                columns,
                CHROME_TEXT,
            );
        }
    }

    /// Repaints the whole viewport.
    ///
    /// The background goes down as one PPA fill, which writes no read
    /// traffic to PSRAM and does not compete with scanout the way a CPU
    /// clear does. The glyphs then go on top with no background of their
    /// own, so each one writes only the pixels it actually inks -- about
    /// fifteen per character instead of the 192 an opaque cell costs. A
    /// screenful is then tens of thousands of scattered writes rather than
    /// the eight hundred thousand a full opaque repaint would be.
    /// Repaints the viewport and returns how many rows of it were touched,
    /// which is what has to be written back.
    fn draw_viewport(&mut self, framebuffer: &mut Framebuffer, service: &mut dyn FnMut()) -> usize {
        // Everything this repaint will draw, and everything the last one
        // left behind. Below that the viewport is already background and
        // clearing it again is a megabyte of PSRAM traffic for no change.
        let content = self.content_bottom();
        let height = content.max(self.painted_bottom) - VIEWPORT_TOP;
        self.painted_bottom = content;
        framebuffer.fill_rect(0, VIEWPORT_TOP, WIDTH, height, PAGE_BACKGROUND);
        // The fill is a DMA transfer over most of the screen and is waited
        // for; that wait is the single longest part of the repaint.
        service();
        let Some(top) = self.top_offset() else {
            return height;
        };
        let focused = self.focused_link();
        for (drawn, line) in self.page.layout.lines()[self.page.first_line..]
            .iter()
            .enumerate()
        {
            if drawn % LINES_PER_SERVICE == 0 && drawn > 0 {
                service();
            }
            let offset = (line.y - top) as usize;
            // A line that does not fit entirely is not drawn: there is no
            // vertical clipping in the glyph renderer, and half a line of
            // text spilling into the status bar would be worse than a
            // margin at the bottom.
            if offset + line.height as usize > VIEWPORT_HEIGHT {
                break;
            }
            self.draw_line(framebuffer, line, VIEWPORT_TOP + offset, focused);
        }
        height
    }

    /// The screen y just past the last line this page will draw.
    ///
    /// Walked rather than derived from the layout's height: which lines fit
    /// depends on where the viewport starts, and the last one that does not
    /// fit is not drawn at all.
    fn content_bottom(&self) -> usize {
        let Some(top) = self.top_offset() else {
            return VIEWPORT_TOP;
        };
        let mut bottom = VIEWPORT_TOP;
        for line in &self.page.layout.lines()[self.page.first_line..] {
            let offset = (line.y - top) as usize;
            if offset + line.height as usize > VIEWPORT_HEIGHT {
                break;
            }
            bottom = VIEWPORT_TOP + offset + line.height as usize;
        }
        bottom
    }

    fn draw_line(
        &self,
        framebuffer: &mut Framebuffer,
        line: &Line,
        screen_y: usize,
        focused: Option<u16>,
    ) {
        if line.rule {
            framebuffer.fill_rect(
                MARGIN,
                screen_y + line.height as usize / 2,
                PAGE_WIDTH,
                2,
                RULE_COLOR,
            );
            return;
        }
        if let Some(marker) = line.marker {
            draw_marker(framebuffer, line, screen_y, marker);
        }
        let scale = line.scale as usize;
        for piece in self.page.layout.pieces(line) {
            let text = self
                .page
                .document
                .text()
                .get(piece.start as usize..piece.end as usize)
                .unwrap_or("");
            let x = MARGIN + line.x as usize + piece.x as usize;
            let is_link = piece.link.is_some();
            let is_focused = is_link && piece.link == focused;
            let color = if is_focused {
                framebuffer.fill_rect(
                    x,
                    screen_y,
                    piece.width as usize,
                    line.height as usize,
                    LINK_COLOR,
                );
                WHITE
            } else {
                piece_color(piece.style, is_link)
            };
            draw_text_run(
                framebuffer,
                x,
                screen_y,
                text,
                scale,
                color,
                line.heading || piece.style & STYLE_BOLD != 0,
            );
            if is_link && !is_focused {
                // Underlined as well as coloured: colour alone is not an
                // affordance for everyone, and the panel's blue on white is
                // not a large contrast step.
                //
                // Drawn in the line's gap rather than against the glyphs.
                // The 5x7 font has descenders that reach the bottom of its
                // box, so a rule at the box's edge touches every `g` and
                // `y`; the gap the line spacing adds is exactly the room
                // this needs.
                let glyph_box = CELL_HEIGHT * scale;
                let underline = screen_y + glyph_box.min(line.height as usize - scale);
                framebuffer.fill_rect(x, underline, piece.width as usize, scale, LINK_COLOR);
            }
        }
    }
}

/// Writes the viewport back in vertical bands, servicing the link between
/// them.
///
/// Banded by *x* and not by *y*. The framebuffer is rotated, so a logical
/// column is a run of native addresses and a logical row is a stride across
/// all of them: splitting by y would hand `flush_rect` a rectangle whose
/// bounding span is still almost the whole buffer, and write back the same
/// 1.6 MB in ten goes instead of one.
#[inline(never)]
fn flush_viewport(framebuffer: &Framebuffer, height: usize, service: &mut dyn FnMut()) {
    if height == 0 {
        return;
    }
    let mut x = 0;
    while x < WIDTH {
        let width = FLUSH_BAND_WIDTH.min(WIDTH - x);
        framebuffer.flush_rect(x, VIEWPORT_TOP, width, height);
        service();
        x += width;
    }
}

/// The bullet or number to the left of a list item, right-aligned into the
/// space the layout reserved for it.
fn draw_marker(framebuffer: &mut Framebuffer, line: &Line, screen_y: usize, marker: Marker) {
    let scale = line.scale as usize;
    let mut text = Summary::new();
    match marker {
        Marker::Bullet => text.push("*"),
        Marker::Number(number) => {
            text.push_usize(number as usize);
            text.push(".");
        }
    }
    let cell = CELL_WIDTH * scale;
    // The marker column ends one cell before the text starts.
    let right = (MARGIN + line.x as usize).saturating_sub(cell);
    let x = right.saturating_sub(text.len() * cell);
    draw_ascii(framebuffer, x, screen_y, text.as_str(), scale, TEXT_COLOR);
}

fn piece_color(style: u8, is_link: bool) -> u16 {
    if is_link {
        return LINK_COLOR;
    }
    // Checked in order of how much the distinction matters when several
    // apply at once: code is a different kind of text and italic is
    // emphasis. Bold is not here at all -- it keeps the plain text colour
    // and is drawn heavier instead, see `draw_text_run`.
    if style & STYLE_CODE != 0 {
        CODE_COLOR
    } else if style & STYLE_ITALIC != 0 {
        ITALIC_COLOR
    } else {
        TEXT_COLOR
    }
}

/// Draws one run, one cell per character.
///
/// Transparent: the caller has already laid down the background, so only
/// inked pixels are written.
#[inline(never)]
fn draw_text_run(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    text: &str,
    scale: usize,
    color: u16,
    bold: bool,
) {
    let advance = CELL_WIDTH * scale;
    let mut cursor = x;
    for character in text.chars() {
        if cursor >= WIDTH {
            break;
        }
        if character.is_ascii() {
            framebuffer.draw_ascii_char(cursor, y, character, scale, color, None);
            if bold {
                // Struck twice, one pixel apart. The 5x7 font has one
                // weight, so bold has to be synthesised or dropped -- and
                // dropping it means `<b>` renders as nothing at all. One
                // physical pixel rather than one glyph pixel (`scale`): at
                // scale 2 a full-cell offset would smear into the next
                // column.
                framebuffer.draw_ascii_char(cursor + 1, y, character, scale, color, None);
            }
        } else {
            draw_placeholder(framebuffer, cursor, y, scale, color);
        }
        cursor += advance;
    }
}

/// A hollow box, for a character the 5x7 font has no glyph for.
///
/// Deliberately not a space and not a `?`. A space would make a page of
/// Japanese look blank, and a `?` is indistinguishable from one the author
/// typed. A box says "there is a character here that this cannot draw",
/// which is the true statement, and it is one box per character so the line
/// still measures correctly.
#[inline(never)]
fn draw_placeholder(framebuffer: &mut Framebuffer, x: usize, y: usize, scale: usize, color: u16) {
    let width = (CELL_WIDTH - 1) * scale;
    let height = (CELL_HEIGHT - 1) * scale;
    let inset = PLACEHOLDER_INSET * scale;
    framebuffer.stroke_rect(x + inset, y + inset, width - inset, height - inset, color);
}

/// Draws ASCII chrome text and returns where it ended.
#[inline(never)]
fn draw_ascii(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    text: &str,
    scale: usize,
    color: u16,
) -> usize {
    framebuffer.draw_text(x, y, text, scale, color, None);
    x + text.chars().count() * CELL_WIDTH * scale
}

/// Draws at most `cells` characters, so a long URL cannot run into the
/// counters at the other end of the bar.
#[inline(never)]
fn draw_clipped(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    text: &str,
    cells: usize,
    color: u16,
) {
    let mut cursor = x;
    let advance = CELL_WIDTH * CHROME_SCALE;
    for (index, character) in text.chars().enumerate() {
        if index >= cells {
            break;
        }
        if character.is_ascii() {
            framebuffer.draw_ascii_char(cursor, y, character, CHROME_SCALE, color, None);
        } else {
            draw_placeholder(framebuffer, cursor, y, CHROME_SCALE, color);
        }
        cursor += advance;
    }
}

/// Builds a page from a finished document.
fn build_page(document: Document) -> Result<Page, Error> {
    let layout = Layout::build(
        &document,
        PAGE_WIDTH as u16,
        Metrics {
            char_width: CELL_WIDTH as u16,
            glyph_height: CELL_HEIGHT as u16,
            line_gap_percent: crate::browser::layout::LINE_GAP_PERCENT,
        },
    )?;
    let order = layout.link_order()?;
    Ok(Page {
        document,
        layout,
        first_line: 0,
        // Nothing is focused until the reader asks: an automatically
        // focused first link looks like something is already selected, and
        // `Enter` would then follow a link nobody chose.
        focus: None,
        order,
    })
}

fn load_builtin(page: &builtin::Page) -> Result<Page, Error> {
    let url = Url::parse(page.url)?;
    let mut parser = Parser::new(url)?;
    page.write(&mut parser)?;
    build_page(parser.finish()?)
}

/// The viewer's own page for a failure.
fn error_page(url: &Url, headline: &str, detail: &str, status: Option<u16>) -> Result<Page, Error> {
    let mut markup = String::new();
    memory::push_str(&mut markup, "<title>error</title><h1>")?;
    push_escaped(&mut markup, headline)?;
    memory::push_str(&mut markup, "</h1>")?;
    if let Some(status) = status {
        memory::push_str(&mut markup, "<p>The server answered ")?;
        let mut number = Summary::new();
        number.push_usize(status as usize);
        push_escaped(&mut markup, number.as_str())?;
        memory::push_str(&mut markup, ".</p>")?;
    }
    if !detail.is_empty() {
        memory::push_str(&mut markup, "<p>")?;
        push_escaped(&mut markup, detail)?;
        memory::push_str(&mut markup, "</p>")?;
    }
    memory::push_str(&mut markup, "<p><code>")?;
    if let Ok(text) = url.to_text() {
        push_escaped(&mut markup, &text)?;
    }
    memory::push_str(
        &mut markup,
        "</code></p><hr><p>Backspace goes back. \
         <a href=\"http://built-in/\">Home</a></p>",
    )?;

    // Parsed rather than laid out by hand: an error page that goes through
    // the same tokenizer, document builder and layout as every other page
    // cannot be the one place where a wrapping or drawing bug hides.
    let url = Url::parse("http://built-in/error")?;
    debug_assert_eq!(url.path(), ERROR_PATH);
    let mut parser = Parser::new(url)?;
    parser.feed(markup.as_bytes())?;
    build_page(parser.finish()?)
}

/// Escapes the two characters that would otherwise be markup.
///
/// The strings put through here are the viewer's own sentences and a URL,
/// and a URL may legally contain `<`. Escaping is cheaper than reasoning
/// about which of them can.
fn push_escaped(target: &mut String, text: &str) -> Result<(), Error> {
    for character in text.chars() {
        match character {
            '<' => memory::push_str(target, "&lt;")?,
            '&' => memory::push_str(target, "&amp;")?,
            _ => memory::push_char(target, character)?,
        }
    }
    Ok(())
}

/// One line per page on the UART: what it cost and how long it took.
fn log_page(
    page: &Page,
    statistics: crate::browser::document::Stats,
    elapsed_ms: u64,
    peak_owned: usize,
) {
    uart::log(b"BROWSER page\r\n");
    uart::log_hex(b"  bytes=", statistics.input_bytes as u32);
    uart::log_hex(b"  text=", statistics.text_bytes as u32);
    uart::log_hex(b"  items=", statistics.items as u32);
    uart::log_hex(b"  links=", statistics.links as u32);
    uart::log_hex(b"  lines=", page.layout.lines().len() as u32);
    uart::log_hex(
        b"  owned=",
        (statistics.owned_bytes + page.layout.owned_bytes()) as u32,
    );
    uart::log_hex(b"  peak=", peak_owned as u32);
    uart::log_hex(b"  ms=", elapsed_ms as u32);
}

/// A short fixed-size text buffer for chrome lines.
struct Summary {
    buffer: [u8; 128],
    length: usize,
}

impl Summary {
    fn new() -> Self {
        Self {
            buffer: [0; 128],
            length: 0,
        }
    }

    fn push(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            if self.length < self.buffer.len() {
                self.buffer[self.length] = byte;
                self.length += 1;
            }
        }
    }

    fn push_usize(&mut self, value: usize) {
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
            if self.length < self.buffer.len() {
                self.buffer[self.length] = digits[index];
                self.length += 1;
            }
        }
    }

    fn len(&self) -> usize {
        self.length
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buffer[..self.length]).unwrap_or("")
    }
}

/// The pages that are always available, with no network at all.
///
/// They are not a demo that gets deleted later. The home page is what the
/// browser opens on and what an error page links to, and the sample pages
/// are the only documents that can be looked at when the Wi-Fi is down --
/// which is exactly when someone wants to know whether the display side
/// still works.
mod builtin {
    use super::{Error, Parser};

    /// The host the built-in pages live under.
    ///
    /// Not a real name and not resolvable, which is the point: a link to it
    /// is recognised before any resolver is asked, so a page in flash can
    /// never turn into a request to whatever a network happens to answer
    /// for that label.
    pub const HOST: &str = "built-in";

    pub struct Page {
        pub url: &'static str,
        body: Body,
    }

    enum Body {
        Fixed(&'static str),
        LongDocument,
        WideLine,
    }

    impl Page {
        /// Feeds the page to a parser in small pieces.
        ///
        /// Small pieces on purpose: this is the same path a body off the
        /// network takes, and running it in one call here would leave the
        /// chunked path untested on the device.
        pub fn write(&self, parser: &mut Parser) -> Result<(), Error> {
            match self.body {
                Body::Fixed(text) => {
                    for chunk in text.as_bytes().chunks(64) {
                        parser.feed(chunk)?;
                    }
                    Ok(())
                }
                Body::LongDocument => write_long(parser),
                Body::WideLine => write_wide(parser),
            }
        }
    }

    fn write_long(parser: &mut Parser) -> Result<(), Error> {
        parser.feed(b"<title>long</title><h1>A long document</h1>")?;
        for _ in 0..24 {
            parser.feed(b"<h2>Section</h2>")?;
            for _ in 0..6 {
                parser.feed(b"<p>")?;
                for _ in 0..40 {
                    parser.feed(b"word ")?;
                }
                parser.feed(b"</p>")?;
            }
        }
        parser.feed(b"<p><a href=\"/\">back to the home page</a></p>")
    }

    fn write_wide(parser: &mut Parser) -> Result<(), Error> {
        parser.feed(b"<title>wide</title><h1>One unbroken line</h1><p>")?;
        // As long as a URL may be, which is the width the wrapping has to
        // survive without a break opportunity anywhere in it.
        for _ in 0..(crate::browser::limits::MAX_URL_BYTES / 16) {
            parser.feed(b"wwwwwwwwwwwwwwww")?;
        }
        parser.feed(b"</p><p>A normal paragraph after it.</p>")?;
        parser.feed(b"<p><a href=\"/\">back to the home page</a></p>")
    }

    pub const HOME: &Page = &Page {
        url: "http://built-in/",
        body: Body::Fixed(
            "<title>Tab5 browser</title>\
             <h1>Tab5 browser</h1>\
             <p>This is a hypertext viewer, not a web browser. It fetches HTML \
             over plain HTTP and shows the text and the links in it. There is \
             no CSS, no JavaScript, no images and no TLS.</p>\
             <h2>Driving it</h2>\
             <ul>\
             <li><b>Tab</b> selects the next link; the status line shows where \
             it goes</li>\
             <li><b>Enter</b> follows the selected link, or -- with nothing \
             selected -- opens the address field on the address already \
             showing, with the caret at the end</li>\
             <li>in the address field, <b>Left</b> and <b>Right</b> move the \
             caret, <b>Home</b> and <b>End</b> jump to either end, and \
             <b>Backspace</b> and <b>Delete</b> remove a character</li>\
             <li><b>Backspace</b> goes back a page</li>\
             <li><b>Up</b> and <b>Down</b> scroll a line, <b>Page Up</b> and \
             <b>Page Down</b> a screen, <b>Home</b> and <b>End</b> the whole \
             document</li>\
             <li><b>Space</b> is another Page Down</li>\
             <li>A touch or a click selects and follows a link; a click on the \
             address field opens it; a mouse wheel scrolls</li>\
             <li><b>Escape</b> stops a page that is loading, closes the address \
             field, and otherwise leaves</li>\
             </ul>\
             <h2>Built-in pages</h2>\
             <ul>\
             <li><a href=\"/sample\">Everything it can display</a></li>\
             <li><a href=\"/long\">A long document, for scrolling</a></li>\
             <li><a href=\"/wide\">One line as long as a URL may be</a></li>\
             <li><a href=\"/empty\">A document with nothing in it</a></li>\
             </ul>\
             <hr>\
             <p>These four are in flash and need no network. Fetching anything \
             else needs <code>wificonnect</code> and <code>ipconfig dhcp</code> \
             first; <code>browser &lt;url&gt;</code> opens one directly, and \
             a path completes against <code>hbase</code>.</p>",
        ),
    };

    pub const SAMPLE: &Page = &Page {
        url: "http://built-in/sample",
        body: Body::Fixed(
            "<title>sample</title>\
             <h1>Heading level 1</h1>\
             <p>An ordinary paragraph, with <b>bold, which is struck twice so \
             it looks heavier</b>, <i>italic, which is dark red because the \
             font has one shape</i> and <code>code(), which is green</code>. \
             Whitespace     between     words collapses, and the paragraph \
             wraps at the width of the screen rather than at whatever width \
             the author had in mind.</p>\
             <h2>Heading level 2</h2>\
             <p>A second paragraph, so the gap between blocks is visible. It \
             also has a <a href=\"/\">link back to the home page</a> in the \
             middle of a sentence, which is where links usually are.</p>\
             <h3>Heading level 3 -- body sized, but struck heavier</h3>\
             <ul><li>An unordered item</li>\
             <li>Another, with a nested list\
             <ul><li>one level in</li>\
             <li>and an ordered list<ol><li>first</li><li>second</li></ol></li>\
             </ul></li>\
             <li>Back at the outer level</li></ul>\
             <ol><li>Ordered items are numbered</li>\
             <li>from one</li>\
             <li>and the numbers are right-aligned</li></ol>\
             <hr>\
             <pre>  preformatted   text\n  keeps       its spaces\n  and its newlines\n</pre>\
             <p>Character references: &amp; &lt; &gt; &quot; &nbsp; &#65; \
             &#x42;. An unresolvable one, &notareference;, is shown as \
             written.</p>\
             <p>Non-ASCII has no glyph and is drawn as a box, one per \
             character: \u{65e5}\u{672c}\u{8a9e}.</p>\
             <p>An image is its alt text: <img src=\"x.png\" alt=\"a red \
             square\"> and one without: <img src=\"y.png\">.</p>\
             <p><a href=\"https://example.invalid/\">An https link</a> is \
             recognised and refused rather than downgraded.</p>\
             <p><a href=\"/\">Home</a></p>",
        ),
    };

    pub const LONG: &Page = &Page {
        url: "http://built-in/long",
        body: Body::LongDocument,
    };

    pub const WIDE: &Page = &Page {
        url: "http://built-in/wide",
        body: Body::WideLine,
    };

    pub const EMPTY: &Page = &Page {
        url: "http://built-in/empty",
        body: Body::Fixed(
            "<title>empty</title><!-- nothing at all --><p><a href=\"/\">Home</a></p>",
        ),
    };

    /// The built-in page at `path`, if there is one.
    pub fn by_path(path: &str) -> Option<&'static Page> {
        match path {
            "/" => Some(HOME),
            "/sample" => Some(SAMPLE),
            "/long" => Some(LONG),
            "/wide" => Some(WIDE),
            "/empty" => Some(EMPTY),
            _ => None,
        }
    }
}
