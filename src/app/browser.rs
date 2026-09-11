//! The hypertext viewer's screen: toolbar, page, status line.
//!
//! The screen is three fixed bands:
//!
//! ```text
//!  y=0    ┌───────────────────────────────────────────────┐
//!         │ ← → ↻  🔓 https://host/page              ▂▄▆ │  toolbar
//!  y=48   ├╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┤  8 px gap
//!  y=56   ├───────────────────────────────────────────────┤
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
//! The padlock says what the connection this page came off actually
//! proved, which is not the same question as what its address asked for.
//! Plaintext and unauthenticated TLS get the same red open lock, because
//! both mean the reader cannot be sure the page is from where the address
//! says: the second one is encrypted, and encryption without an identity
//! stops someone reading the page in transit but not someone writing it.
//! A closed lock -- a pin matched, the only case where the peer was
//! identified -- is the one that is not red, and even then nothing on
//! screen says "secure". Tapping the lock puts the whole sentence in the
//! status line; it used to be spelled out permanently, in fourteen cells
//! of the space the address needed.
//!
//! The three buttons are back, forward, and reload -- which is stop while
//! something is arriving, because the two are never both wanted. All of
//! them have keys as well (`[`, `]`, `r`, Escape), and the keys are what
//! CardKB has: the buttons are for the finger, not instead of them.
//!
//! System indicators, launcher and minis belong to `system_bar`.
//! This module receives only events for the active Browser.
//!
//! **A page is only ever shown complete.** While one is arriving the
//! previous one stays on screen and only the toolbar's byte count moves; the
//! swap happens in one step when the body has ended and the document has
//! been built. Nothing partial is ever displayed, because a page that
//! stopped halfway looks exactly like a page that ended there.
//!
//! Name resolution and transfer advance one bounded step per host timer
//! delivery (`net::dns::Query` and `net::http::Transaction`). The host
//! stops these deliveries while a mini or launcher owns the screen.
//!
//! The host uses `super::pointer` for all normal GUI screens, and the
//! drawing order it documents is obeyed exactly: lift the cursor, draw what
//! changed, put the cursor back, write back the union.

use alloc::string::String;
use alloc::vec::Vec;

use super::theme::{self, BACKGROUND as WHITE, TEXT as BLACK};
use crate::browser::document::{Document, Marker, Parser, STYLE_BOLD, STYLE_CODE, STYLE_ITALIC};
use crate::browser::error::{self, Error};
use crate::browser::layout::{Layout, Line, Metrics};
use crate::browser::limits::{MAX_HISTORY, MAX_URL_BYTES};
use crate::browser::memory;
use crate::browser::url::{self, Url};
use crate::framebuffer::{Framebuffer, HEIGHT, WIDTH};
use crate::input::{InputManager, Key};

use crate::{tick, uart};

use super::fetch::{self, Fetch, Network, Outcome as FetchOutcome};
use super::localfile::{LocalRead, Started};
use super::wifi_manager::Manager as WifiManager;

use crate::fs::vfs::Vfs;
use crate::fs::{Devices, RamBlockDevice, SdSlot};

/// One half-width cell of the 16 pixel font: the unit the chrome and the
/// list markers are laid out in.
///
/// Page text is not laid out in cells -- `layout` measures every character's
/// own advance, and half of them are twice this wide. This is for the parts
/// that are ASCII by contract (the address bar, the counters) and for the
/// indentation the layout reserves.
///
/// `CELL_HEIGHT` is the glyph's box, not a line's: the layout adds a gap
/// below every line (`layout::LINE_GAP_PERCENT`), so `Line::height` is the
/// larger of the two and is what the viewport steps by.
const CELL_WIDTH: usize = crate::browser::layout::CELL_WIDTH as usize;
const CELL_HEIGHT: usize = crate::font::HEIGHT;

/// The toolbar's height, which is also every button's hit height.
///
/// 48 pixels is about 4.1 mm on this panel (1280 across roughly 111 mm, so
/// 11.6 pixels per millimetre). Interface guidelines ask for something
/// nearer 7 mm, which here would be 81 pixels -- an eighth of the screen's
/// height for one bar, which is not a trade this screen can make. So the
/// number was a judgement, and it was made by building both and pressing
/// the buttons: 40 was reachable and 48 is comfortable, and the eight
/// pixels come out of a viewport that has 640 left.
///
/// Everything on the bar is derived from this constant -- `BUTTON_WIDTH`
/// included -- so changing it is one edit, and the assertions below are
/// what say whether the derivation still holds.
const TOOLBAR_HEIGHT: usize = tab5_system_ui::HEIGHT;
const STATUS_HEIGHT: usize = 32;
/// Blank space between the toolbar and the first line of the page.
///
/// The page's own top margin, in the page's own colour. Without it the
/// first line of text sits against the bottom edge of the address field,
/// which reads as though the chrome and the document are one surface --
/// and on a heading, whose glyph box starts at the very top of its line,
/// the two actually touch.
const CONTENT_GAP: usize = 8;
const VIEWPORT_TOP: usize = TOOLBAR_HEIGHT + CONTENT_GAP;
const VIEWPORT_BOTTOM: usize = HEIGHT - STATUS_HEIGHT;
const VIEWPORT_HEIGHT: usize = VIEWPORT_BOTTOM - VIEWPORT_TOP;
/// Left and right margin inside the viewport.
const MARGIN: usize = 12;
const PAGE_WIDTH: usize = WIDTH - 2 * MARGIN;

/// Toolbar and status text scale. The same as body text: chrome that is
/// harder to read than the page is chrome nobody reads.
const CHROME_SCALE: usize = 1;
const CHROME_CELL: usize = CELL_WIDTH * CHROME_SCALE;
const CHROME_TEXT_Y: usize = (TOOLBAR_HEIGHT - CELL_HEIGHT * CHROME_SCALE) / 2;
const STATUS_TEXT_Y: usize = VIEWPORT_BOTTOM + (STATUS_HEIGHT - CELL_HEIGHT * CHROME_SCALE) / 2;

/// The toolbar, left to right: three buttons, the security icon, and the
/// address field running to the right margin.
///
/// The order the space was found in matters. The link and line counts used
/// to hold 208 pixels at the right and the security badge 112 at the left,
/// spelled out in words; between them they paid for the buttons, the icon
/// and a wider address field with room left over. Adding the buttons first
/// would have meant taking the space out of the address, which is the one
/// part of this bar that is never wide enough.
/// Slightly wider than the bar is tall, which keeps three of them and the
/// lock inside the space the link and line counts used to hold.
const BUTTON_WIDTH: usize = tab5_system_ui::BUTTON_WIDTH;
const BUTTON_COUNT: usize = 3;
const BUTTONS_LEFT: usize = tab5_system_ui::APP.x;
/// What the three buttons are drawn with.
///
/// Characters and not bitmaps of their own: the font covers the arrows and
/// the open circle arrow (`FONT.md`'s subset takes U+2190..U+21FF whole),
/// and a glyph that is already there is one that cannot drift from the
/// renderer. Nearby characters that look right are *not* all there --
/// U+2715 MULTIPLICATION X and U+26A0 WARNING SIGN are outside the subset
/// and would draw as the missing-character box -- so these four are the
/// ones checked against `font/data/tab5font16.txt`.
const BACK_GLYPH: &str = "\u{2190}";
const FORWARD_GLYPH: &str = "\u{2192}";
const RELOAD_GLYPH: &str = "\u{21BB}";
const STOP_GLYPH: &str = "\u{00D7}";
/// The security icon's slot: a padlock, drawn rather than typed. There is
/// no padlock in the font -- U+1F512 is outside the BMP, which the source
/// font does not cover -- and it is the one icon here worth a few
/// rectangles of its own.
const ICON_WIDTH: usize = 24;
/// What the lock says when it is asked while a connection is being made and
/// has proved nothing. Not a security state: the absence of one.
const CONNECTING_TEXT: &str = "Connecting: nothing has been proved yet";
const ICON_LEFT: usize = tab5_system_ui::ICON_LEFT;
const ADDRESS_LEFT: usize = tab5_system_ui::ADDRESS_LEFT;
const ADDRESS_RIGHT: usize = tab5_system_ui::ADDRESS_RIGHT;
/// The clear button, inside the field's own right edge and present only
/// while the field is open. `ADDRESS_CELLS` is what the text gets, which is
/// the field less that button.
const CLEAR_WIDTH: usize = 32;
const CLEAR_LEFT: usize = ADDRESS_RIGHT - CLEAR_WIDTH;
/// The open field's own box: two scaled glyphs tall, which is what the
/// clear cross needs and what centres the text at `CHROME_TEXT_Y`.
const FIELD_HEIGHT: usize = CELL_HEIGHT * 2;
const FIELD_TOP: usize = (TOOLBAR_HEIGHT - FIELD_HEIGHT) / 2;
const ADDRESS_CELLS: usize = (CLEAR_LEFT - 8 - ADDRESS_LEFT) / CHROME_CELL;

// The bar has one free parameter (`TOOLBAR_HEIGHT`) and everything else is
// derived from it, so these are what "derived correctly" means. Without
// them a taller bar is a build that compiles and draws a lock through the
// address field.
const _: () = {
    assert!(
        FIELD_HEIGHT <= TOOLBAR_HEIGHT,
        "the address field is taller than the bar"
    );
    assert!(
        CELL_HEIGHT * 2 <= TOOLBAR_HEIGHT,
        "a button glyph is taller than the bar"
    );
    assert!(
        LOCK_HEIGHT <= TOOLBAR_HEIGHT,
        "the lock is taller than the bar"
    );
    assert!(
        ADDRESS_LEFT < CLEAR_LEFT,
        "the buttons have eaten the address field"
    );
    assert!(
        ADDRESS_RIGHT + 4 <= tab5_system_ui::WIFI.x,
        "the address field runs into the Wi-Fi icon"
    );
    assert!(
        ADDRESS_CELLS >= 64,
        "the address field is too narrow to edit in"
    );
};

const PAGE_BACKGROUND: u16 = WHITE;
const TEXT_COLOR: u16 = BLACK;
/// Pure blue on white, which the panel renders cleanly at this size.
const LINK_COLOR: u16 = theme::ACCENT;
const CODE_COLOR: u16 = theme::CODE;
/// Dark red. A grey was tried first and could not be told from black at
/// this size: two near-blacks in a bitmap font read as a rendering fault
/// rather than as emphasis. Emphasis has to differ in hue, not in
/// brightness.
const ITALIC_COLOR: u16 = theme::EMPHASIS;
const RULE_COLOR: u16 = theme::BORDER;
const CHROME_BACKGROUND: u16 = theme::BUTTON_FACE;
const CHROME_TEXT: u16 = BLACK;
/// Status-line messages, in the same red as the cleartext badge: almost
/// every one of them is the viewer refusing to do something.
const MESSAGE_COLOR: u16 = theme::EMPHASIS;
/// The lock when the connection proves nothing about who answered: plain
/// HTTP, and unauthenticated TLS. Red, because both mean the same thing to
/// a reader -- what is on screen may not be what the address says.
const INSECURE_COLOR: u16 = theme::ERROR;
/// The lock when a pin matched. Dark green: the one case where the peer
/// was actually identified. Still not the word "secure", which would claim
/// more than a pin does.
const AUTHENTICATED_COLOR: u16 = theme::SUCCESS;
/// A button whose action is not available: no history to go back to, no
/// forward entry to return to.
const DISABLED_COLOR: u16 = theme::BORDER;
/// The address field while it is being edited.
const EDIT_BACKGROUND: u16 = WHITE;
const EDIT_CARET: u16 = theme::ACCENT;

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

/// A stopped value while a launcher or mini owns the screen.
pub struct Browser {
    viewer: Viewer,
    pending: Option<Pending>,
}
impl Browser {
    pub fn new(start: Option<Url>, wifi: &mut WifiManager) -> Option<Self> {
        let mut viewer = Viewer::new().ok()?;
        if addressed_network(wifi).is_none() {
            viewer.say("no network: open Network settings from the system bar");
        }
        if let Some(url) = start {
            viewer.request(Navigation::fresh(url));
        }
        Some(Self {
            viewer,
            pending: None,
        })
    }
    pub fn key(&mut self, key: Key, wifi: &mut WifiManager, vfs: &mut Vfs) -> bool {
        let action = self.viewer.handle_key(key, self.pending.is_some());
        self.answer(action, wifi, vfs)
    }
    pub fn click(&mut self, x: usize, y: usize, wifi: &mut WifiManager, vfs: &mut Vfs) -> bool {
        let action = self.viewer.click(x, y);
        self.answer(action, wifi, vfs)
    }
    fn answer(&mut self, action: Action, wifi: &mut WifiManager, vfs: &mut Vfs) -> bool {
        match action {
            Action::Continue => {}
            Action::Cancel => stop_pending(&mut self.viewer, &mut self.pending, wifi, vfs),
            Action::Report => report_state(&mut self.viewer, &self.pending, wifi),
            Action::Leave => return true,
        }
        false
    }
    pub fn wheel(&mut self, amount: i32) {
        self.viewer.scroll_by(-amount * WHEEL_LINES);
    }
    pub fn bar_target(&self, x: usize) -> tab5_system_ui::Rect {
        tab5_system_ui::browser_target(x, self.editing())
    }
    pub fn suspend(&mut self, wifi: &mut WifiManager, vfs: &mut Vfs) {
        // Network GETs restart on return. Local reads retain their VFS handle;
        // they are never polled while this screen is suspended.
        if self
            .pending
            .as_ref()
            .is_some_and(Pending::is_network_transfer)
        {
            let active = self.pending.take().expect("pending transfer");
            let navigation = Navigation {
                url: active.navigation.url.clone(),
                restore: active.navigation.restore,
                how: active.navigation.how,
            };
            close_pending(active, wifi, vfs);
            self.pending = Some(Pending {
                navigation,
                source: Source::WaitingForNetwork,
            });
        }
    }
    pub fn close(&mut self, wifi: &mut WifiManager, vfs: &mut Vfs) {
        if let Some(active) = self.pending.take() {
            close_pending(active, wifi, vfs);
        }
    }
    pub fn draw(&mut self, fb: &mut Framebuffer, wifi: &mut WifiManager, full: bool) -> bool {
        if full {
            self.viewer.draw_all(fb, &mut || service_link(wifi))
        } else {
            self.viewer.draw_dirty(fb, &mut || service_link(wifi))
        }
    }
    pub fn editing(&self) -> bool {
        self.viewer.editing.is_some()
    }
    pub fn dirty(&self) -> bool {
        self.viewer.dirty()
    }
    pub fn tick(
        &mut self,
        input: &mut InputManager,
        wifi: &mut WifiManager,
        vfs: &mut Vfs,
        mut ram_disk: Option<&mut RamBlockDevice>,
    ) {
        // A managed connection can disappear between two page-fetch steps.
        // Its old socket belongs to the old stack and cannot survive, but a
        // GET can: keep the navigation and restart it once reassociation and
        // DHCP have produced a new addressed stack.
        if self
            .pending
            .as_ref()
            .is_some_and(Pending::is_network_transfer)
            && !network_is_addressed(wifi)
        {
            if let Some(active) = self.pending.take() {
                suspend_or_fail(active, &mut self.viewer, &mut self.pending, wifi, vfs);
            }
        }

        // A navigation the reader asked for. Whatever is in flight is
        // abandoned first: the newest request is the one they meant, and
        // this is also what makes "cancel, then fetch something else"
        // work without a state in between.
        if let Some(navigation) = self.viewer.take_request() {
            if let Some(active) = self.pending.take() {
                close_pending(active, wifi, vfs);
            }
            self.pending = begin(
                &mut self.viewer,
                navigation,
                wifi,
                vfs,
                ram_disk.as_deref_mut(),
                input,
            );
        }

        // `Source::WaitingForNetwork` owns no socket. It deliberately stays
        // self.pending so Escape and a newer navigation still have their ordinary
        // meanings while the manager performs reassociation and DHCP.
        if self
            .pending
            .as_ref()
            .is_some_and(Pending::is_waiting_for_network)
        {
            if network_is_addressed(wifi) {
                let active = self.pending.take().expect("network wait is self.pending");
                let navigation = active.navigation;
                self.pending = begin(
                    &mut self.viewer,
                    navigation,
                    wifi,
                    vfs,
                    ram_disk.as_deref_mut(),
                    input,
                );
            } else if !network_is_recovering(wifi) {
                let active = self.pending.take().expect("network wait is self.pending");
                let url = active.navigation.url.clone();
                let how = active.navigation.how;
                close_pending(active, wifi, vfs);
                self.viewer.show_failure(&url, fetch::NO_NETWORK, how);
            }
        }

        let outcome = match self.pending.as_mut() {
            Some(active) => {
                let outcome = match &mut active.source {
                    Source::Network(fetch) => addressed_network(wifi)
                        .as_mut()
                        .map(|link| fetch.step(link)),
                    Source::Local(read) => {
                        let mut sd = SdSlot::new();
                        let mut devices = Devices {
                            ram: ram_disk.as_deref_mut(),
                            sd: &mut sd,
                            usb: input.usb_host_mut(),
                        };
                        Some(read.step(vfs, &mut devices))
                    }
                    // Handled just above: it either remains waiting, becomes
                    // a network transfer, or becomes a no-network failure.
                    Source::WaitingForNetwork => None,
                };
                if outcome.is_some() {
                    self.viewer
                        .update_loading(active.received(), active.security());
                }
                outcome
            }
            None => None,
        };
        match outcome {
            None | Some(FetchOutcome::Working) => {}
            Some(FetchOutcome::Page(document)) => {
                if let Some(active) = self.pending.take() {
                    // The final address, which is the last hop of a
                    // redirect chain rather than the one that was asked
                    // for -- so the toolbar and the base for this page's
                    // links are both where the page actually came from.
                    let landed = active.landed();
                    let security = active.security();
                    let status = active.status();
                    let peak = active.peak_owned();
                    let navigation = Navigation {
                        url: landed.clone(),
                        restore: active.navigation.restore,
                        how: active.navigation.how,
                    };
                    close_pending(active, wifi, vfs);
                    self.viewer.show_document(
                        document,
                        &navigation,
                        landed,
                        security,
                        status,
                        peak,
                    );
                }
            }
            Some(FetchOutcome::Failed(failure)) => {
                if let Some(active) = self.pending.take() {
                    // A transfer can be the operation that makes a dead C6
                    // link observable. Let the manager consume that evidence
                    // before deciding whether this is a page failure or an
                    // automatically recoverable Wi-Fi interruption.
                    service_link(wifi);
                    if active.is_network_transfer()
                        && !network_is_addressed(wifi)
                        && network_is_recovering(wifi)
                    {
                        suspend_or_fail(active, &mut self.viewer, &mut self.pending, wifi, vfs);
                    } else {
                        let url = active.landed();
                        let how = active.navigation.how;
                        close_pending(active, wifi, vfs);
                        self.viewer.show_failure(&url, failure, how);
                    }
                }
            }
        }
    }
}

/// Gives back whatever the pending read owns: a socket, or a file handle.
///
/// One function for both so that every site that abandons a read returns
/// the right thing without having to know which kind it had.
fn close_pending(pending: Pending, wifi: &mut WifiManager, vfs: &mut Vfs) {
    match pending.source {
        Source::Network(fetch) => {
            if let Some(mut link) = raw_network(wifi) {
                fetch.close(&mut link);
            }
        }
        Source::Local(read) => read.close(vfs),
        Source::WaitingForNetwork => {}
    }
}

/// Drops an interrupted transfer and either waits for the manager or reports
/// the same no-network failure a non-managed connection has always produced.
fn suspend_or_fail(
    pending: Pending,
    viewer: &mut Viewer,
    slot: &mut Option<Pending>,
    wifi: &mut WifiManager,
    vfs: &mut Vfs,
) {
    let Pending { source, navigation } = pending;
    match source {
        Source::Network(fetch) => {
            if let Some(mut link) = raw_network(wifi) {
                fetch.close(&mut link);
            }
        }
        Source::Local(read) => read.close(vfs),
        Source::WaitingForNetwork => {}
    }
    if network_is_recovering(wifi) {
        viewer.wait_for_network(&navigation.url);
        *slot = Some(Pending {
            source: Source::WaitingForNetwork,
            navigation,
        });
    } else {
        let url = navigation.url.clone();
        let how = navigation.how;
        viewer.show_failure(&url, fetch::NO_NETWORK, how);
    }
}

/// Puts the numbers a leak would show up in on the status line, and the
/// same line on the UART.
///
/// Every one of them is a thing that should come back to where it started.
/// A browser leaks in two ways that matter on a board with no process to
/// restart: heap that is never given back, and sockets that never return to
/// the set -- and the second is invisible until the set runs dry several
/// minutes later, somewhere else entirely. Both are differences between two
/// moments rather than values, so this is a key rather than a log line:
/// read it, do the thing twenty times, read it again.
///
/// `sockets` is the whole set, not this screen's share of it: DHCP and DNS
/// hold their own. What matters is that it is the same number before and
/// after, not what the number is.
fn report_state(viewer: &mut Viewer, pending: &Option<Pending>, wifi: &mut WifiManager) {
    let sockets = raw_network(wifi).map(|network| network.stack.sockets_mut().iter().count());
    let mut line = Summary::new();
    line.push("heap ");
    line.push_usize(crate::heap_used() / 1024);
    line.push("K sockets ");
    match sockets {
        Some(count) => line.push_usize(count),
        // No stack at all is a different state from a stack with no
        // sockets, and reading `0` for both would hide a lost link.
        None => line.push("-"),
    }
    line.push(pending.as_ref().map_or("", |_| "+1 loading"));
    line.push(" back ");
    line.push_usize(viewer.history.len());
    line.push(" fwd ");
    line.push_usize(viewer.forward.len());
    line.push(" page ");
    line.push_usize(viewer.page_owned_bytes() / 1024);
    line.push("K peak ");
    line.push_usize(viewer.last_peak / 1024);
    line.push("K");
    uart::log(b"BROWSER: ");
    uart::log(line.as_str().as_bytes());
    uart::log(b"\r\n");
    viewer.say(line.as_str());
}

/// Abandons the transfer in flight, if there is one, and says so.
///
/// The one place a running fetch is dropped by the reader's own choice, so
/// that Escape and the stop button cannot end up returning the socket in
/// two slightly different ways.
fn stop_pending(
    viewer: &mut Viewer,
    pending: &mut Option<Pending>,
    wifi: &mut WifiManager,
    vfs: &mut Vfs,
) {
    let Some(active) = pending.take() else {
        return;
    };
    close_pending(active, wifi, vfs);
    viewer.finish_loading();
    viewer.say("stopped");
}

/// Reads whatever the C6 has waiting.
///
/// Called wherever this screen is about to spend longer than a frame not
/// looking at the link -- which is most of a viewport repaint. Cheap when
/// there is nothing waiting: a couple of SDIO register reads.
fn service_link(wifi: &mut WifiManager) {
    wifi.service_io();
}

fn raw_network(wifi: &mut WifiManager) -> Option<Network<'_>> {
    let (rpc, stack) = wifi.options_mut();
    match (rpc.as_mut(), stack.as_mut()) {
        (Some(rpc), Some(stack)) => Some(Network { rpc, stack }),
        _ => None,
    }
}

fn addressed_network(wifi: &mut WifiManager) -> Option<Network<'_>> {
    raw_network(wifi).filter(|network| network.stack.has_address())
}

fn network_is_addressed(wifi: &WifiManager) -> bool {
    wifi.stack().is_some_and(crate::net::Stack::has_address)
}

/// Whether the manager is already doing work that can produce a fresh stack.
///
/// `Associated` is deliberately absent: that is also the terminal state of a
/// CLI connection whose address must be configured manually. The other four
/// states are transient states of managed reassociation or DHCP recovery.
fn network_is_recovering(wifi: &WifiManager) -> bool {
    use super::wifi_manager::State;
    matches!(
        wifi.state(),
        State::Associating { .. }
            | State::RetryWaiting { .. }
            | State::RequestingDhcp { .. }
            | State::AssociatedNoLease(_)
    )
}

/// Starts a navigation, or answers it without leaving the board when it
/// can.
///
/// Three answers in order, and the order is the point. A built-in page is
/// decided before any resolver is asked; a `file:` URL is decided before
/// any socket is opened; only what is left goes to the network.
fn begin(
    viewer: &mut Viewer,
    navigation: Navigation,
    wifi: &mut WifiManager,
    vfs: &mut Vfs,
    ram_disk: Option<&mut RamBlockDevice>,
    input: &mut InputManager,
) -> Option<Pending> {
    let navigation = match viewer.page_navigation(navigation) {
        Some(navigation) => navigation,
        None => return None,
    };
    if navigation.url.host() == builtin::HOST && navigation.url.scheme().is_network() {
        match builtin::by_path(navigation.url.path()) {
            Some(page) => viewer.show_builtin(page, &navigation),
            None => viewer.show_failure(&navigation.url, fetch::NO_SUCH_BUILTIN, navigation.how),
        }
        return None;
    }
    if !navigation.url.scheme().is_network() {
        return begin_local(viewer, navigation, vfs, ram_disk, input);
    }
    if !network_is_addressed(wifi) && network_is_recovering(wifi) {
        viewer.wait_for_network(&navigation.url);
        return Some(Pending {
            source: Source::WaitingForNetwork,
            navigation,
        });
    }
    let mut network = addressed_network(wifi);
    let Some(network) = network.as_mut() else {
        viewer.show_failure(&navigation.url, fetch::NO_NETWORK, navigation.how);
        return None;
    };
    match Fetch::start(navigation.url.clone(), network) {
        Ok(fetch) => {
            viewer.begin_loading(&navigation.url);
            Some(Pending {
                source: Source::Network(fetch),
                navigation,
            })
        }
        Err(failure) => {
            viewer.show_failure(&navigation.url, failure, navigation.how);
            None
        }
    }
}

/// Opens a `file:` URL, or shows the directory it names.
///
/// A directory is finished here rather than stepped: the whole listing is
/// one read of one directory, bounded by what is in it, where a file is
/// bounded by nothing the reader can see before opening it.
fn begin_local(
    viewer: &mut Viewer,
    navigation: Navigation,
    vfs: &mut Vfs,
    ram_disk: Option<&mut RamBlockDevice>,
    input: &mut InputManager,
) -> Option<Pending> {
    let mut sd = SdSlot::new();
    let mut devices = Devices {
        ram: ram_disk,
        sd: &mut sd,
        usb: input.usb_host_mut(),
    };
    match LocalRead::start(&navigation.url, vfs, &mut devices) {
        Ok(Started::Reading(read)) => {
            viewer.begin_loading(&navigation.url);
            Some(Pending {
                source: Source::Local(read),
                navigation,
            })
        }
        Ok(Started::Page(document)) => {
            let landed = navigation.url.clone();
            viewer.show_document(document, &navigation, landed, None, None, 0);
            None
        }
        Err(failure) => {
            viewer.show_failure(&navigation.url, failure, navigation.how);
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
    /// The line to put at the top once the page is up. Non-zero when going
    /// back or forward, which are re-fetches -- history keeps a scroll
    /// position but never a document -- and when reloading, which is the
    /// same page and should not jump to the top of it.
    restore: usize,
    how: Direction,
}

/// Which of the four ways a navigation started, which is the only thing
/// that decides what happens to the two stacks when it lands.
///
/// A single flag ("push history?") was not enough once there was a forward
/// stack: `back` and `forward` both decline to push history and do
/// opposite things with the other stack, and `reload` touches neither.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// A link, a typed address, a command-line argument.
    Fresh,
    Back,
    Forward,
    Reload,
}

impl Navigation {
    fn fresh(url: Url) -> Navigation {
        Navigation {
            url,
            restore: 0,
            how: Direction::Fresh,
        }
    }
}

/// Something being read, together with what the viewer wants done when it
/// lands.
///
/// Two sources, one loop. A `file:` URL has no name to resolve, no socket,
/// no redirect chain and no status code, so it does not go through
/// [`Fetch`] -- but it is stepped, cancelled, drawn and finished by exactly
/// the same code here, because from the reader's side it is the same act.
struct Pending {
    source: Source,
    navigation: Navigation,
}

enum Source {
    Network(Fetch),
    Local(LocalRead),
    /// Reassociation and DHCP are in progress. Owns no socket; the navigation
    /// is restarted from its original URL once a new addressed stack exists.
    WaitingForNetwork,
}

impl Pending {
    /// Where the page actually came from.
    ///
    /// For a fetch that is the last hop of a redirect chain rather than the
    /// address that was asked for. A local read cannot be redirected, so
    /// the two are the same thing there.
    fn landed(&self) -> Url {
        match &self.source {
            Source::Network(fetch) => fetch.url().clone(),
            Source::Local(_) => self.navigation.url.clone(),
            Source::WaitingForNetwork => self.navigation.url.clone(),
        }
    }

    /// What the connection proved. Nothing, for a file: nobody was asked.
    fn security(&self) -> Option<fetch::PageSecurity> {
        match &self.source {
            Source::Network(fetch) => fetch.security(),
            Source::Local(_) | Source::WaitingForNetwork => None,
        }
    }

    fn status(&self) -> Option<u16> {
        match &self.source {
            Source::Network(fetch) => fetch.status(),
            Source::Local(_) | Source::WaitingForNetwork => None,
        }
    }

    fn received(&self) -> usize {
        match &self.source {
            Source::Network(fetch) => fetch.received(),
            Source::Local(read) => read.received(),
            Source::WaitingForNetwork => 0,
        }
    }

    fn peak_owned(&self) -> usize {
        match &self.source {
            Source::Network(fetch) => fetch.peak_owned(),
            Source::Local(read) => read.peak_owned(),
            Source::WaitingForNetwork => 0,
        }
    }

    fn is_network_transfer(&self) -> bool {
        matches!(self.source, Source::Network(_))
    }

    fn is_waiting_for_network(&self) -> bool {
        matches!(self.source, Source::WaitingForNetwork)
    }
}

enum Action {
    Continue,
    /// Escape while a page is arriving.
    Cancel,
    /// Put the numbers a leak would show up in on the status line.
    ///
    /// Answered by the loop and not by the viewer, because two of the four
    /// numbers -- the sockets in use and whether a transfer is running --
    /// belong to things the viewer does not hold.
    Report,
    /// Open the Wi-Fi menu. Answered by the loop for the same reason, and
    /// because handing the whole screen to another mode is not something
    /// the viewer can do while it is drawing itself.
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
    visit_url: Url,
    /// What the connection this page came off proved, or `None` for a page
    /// that never crossed a network -- a built-in page, or this viewer's
    /// own error page. `None` is displayed from the scheme instead, which
    /// for those is always `http://built-in/...`.
    security: Option<fetch::PageSecurity>,
    /// Whether this is the viewer's own explanation of a failure rather
    /// than something that was fetched.
    ///
    /// A flag and not a look at the address, because the address is now the
    /// one that failed: an error page carries the URL the reader asked for,
    /// so that the toolbar does not name a page that does not exist and so
    /// that reloading retries what actually went wrong.
    error: bool,
    layout: Layout,
    /// Index of the topmost drawn line.
    first_line: usize,
    /// Position within `order`, not a link index.
    focus: Option<usize>,
    /// Links in the order they are laid out, which is the order `Tab`
    /// visits them.
    order: Vec<u16>,
}

/// The toolbar's buttons, left to right.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Button {
    Back,
    Forward,
    /// Reload, or stop while something is arriving. One button because the
    /// two are never both wanted, and because a stop button that is dead
    /// most of the time teaches nobody where it is.
    Reload,
}

/// The button under a toolbar x, if any.
fn button_at(x: usize) -> Option<Button> {
    let offset = x.checked_sub(BUTTONS_LEFT)?;
    match offset / BUTTON_WIDTH {
        0 => Some(Button::Back),
        1 => Some(Button::Forward),
        2 => Some(Button::Reload),
        _ => None,
    }
}

/// Which of the viewer's two stacks a page is being put on.
#[derive(Clone, Copy)]
enum Stack {
    History,
    Forward,
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
    /// What the connection being made has proved so far. `None` until a TLS
    /// handshake finishes -- which is the point: a badge drawn from a
    /// handshake that has not happened is a badge that can turn out to have
    /// been a lie.
    security: Option<fetch::PageSecurity>,
    received: usize,
    /// Kilobytes last drawn, so the toolbar is repainted when the number
    /// changes rather than on every frame.
    shown_kib: usize,
}

struct Viewer {
    page: Page,
    history: Vec<HistoryEntry>,
    /// Pages gone back from, to be gone forward to.
    ///
    /// Emptied by any navigation that is not a `back` or a `forward`,
    /// which is what stops "back, then somewhere else, then forward" from
    /// returning to a page the reader has already left behind.
    forward: Vec<HistoryEntry>,
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
    /// The most the last fetch's parser held at once.
    ///
    /// Kept only so `i` can report it. It is not recoverable after the fetch
    /// is closed, so it is caught on the way past rather than asked for later.
    last_peak: usize,
    /// What the Wi-Fi indicator is currently drawn as.
    ///
    /// Sampled every frame and compared, rather than drawn every frame: the
    /// toolbar only repaints when something on it changed, and the Wi-Fi
    /// state changes a handful of times in a session.
    dirty: Dirty,
}

impl Viewer {
    fn new() -> Result<Viewer, Error> {
        let page = load_builtin(builtin::HOME)?;
        Ok(Viewer {
            page,
            history: Vec::new(),
            forward: Vec::new(),
            editing: None,
            loading: None,
            message: None,
            request: None,
            painted_bottom: VIEWPORT_BOTTOM,
            slowest_repaint_ms: 0,
            last_peak: 0,
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

    /// What the page on screen costs: its document and its layout.
    ///
    /// The two together, because they are freed together -- a page is
    /// replaced whole -- and because a layout that grew while a document
    /// did not is exactly the shape a wrapping bug takes.
    fn page_owned_bytes(&self) -> usize {
        self.page.document.stats().owned_bytes + self.page.layout.owned_bytes()
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

    /// Keeps the requested address visibly loading while managed Wi-Fi
    /// reassociation and DHCP run underneath it.
    fn wait_for_network(&mut self, url: &Url) {
        self.begin_loading(url);
        self.say("reconnecting Wi-Fi; this page will retry automatically");
    }

    /// Takes the current Wi-Fi state, repainting the bar only on a change.

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
            security: None,
            received: 0,
            shown_kib: usize::MAX,
        });
        self.message = None;
        self.dirty.toolbar = true;
        self.dirty.status = true;
    }

    fn update_loading(&mut self, received: usize, security: Option<fetch::PageSecurity>) {
        let Some(loading) = self.loading.as_mut() else {
            return;
        };
        if loading.security != security {
            loading.security = security;
            self.dirty.toolbar = true;
        }
        loading.received = received;
        let kib = received / 1024;
        if kib != loading.shown_kib {
            loading.shown_kib = kib;
            // The status line and not the toolbar: the count moved down
            // there when the address field grew to the right margin, and
            // the band that repaints for it is the cheaper of the two.
            self.dirty.status = true;
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
    /// `status` is set when the server answered with something outside 2xx
    /// and sent a page anyway, which is shown as itself with the number in
    /// the status line -- see `fetch::Fetch::status`.
    fn show_document(
        &mut self,
        document: Document,
        navigation: &Navigation,
        landed: Url,
        security: Option<fetch::PageSecurity>,
        status: Option<u16>,
        peak_owned: usize,
    ) {
        match build_page(document) {
            Ok(mut page) => {
                page.security = security;
                self.settle_history(navigation.how);
                page.visit_url = landed;
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
                self.last_peak = peak_owned;
                if navigation.how == Direction::Fresh && navigation.restore == 0 {
                    self.scroll_to_fragment();
                }
                if let Some(status) = status {
                    self.say_status(status);
                }
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
                    navigation.how,
                );
            }
        }
    }

    fn show_builtin(&mut self, page: &'static builtin::Page, navigation: &Navigation) {
        match load_builtin(page) {
            Ok(loaded) => {
                self.settle_history(navigation.how);
                self.page = loaded;
                self.page.visit_url = navigation.url.clone();
                self.scroll_to(navigation.restore);
                self.loading = None;
                self.message = None;
                self.dirty = Dirty {
                    toolbar: true,
                    viewport: true,
                    status: true,
                };
                if navigation.how == Direction::Fresh && navigation.restore == 0 {
                    self.scroll_to_fragment();
                }
            }
            Err(failure) => self.say(error::error_text(failure)),
        }
    }

    /// Replaces the page with the viewer's own explanation of a failure.
    ///
    /// A page rather than a status line, because a failed navigation has to
    /// leave somewhere to go: this one carries the address that failed, the
    /// reason, and a link home. Backspace still goes back.
    fn show_failure(&mut self, url: &Url, failure: fetch::Failure, how: Direction) {
        self.show_error(url, failure.headline, failure.detail, failure.status, how);
    }

    fn show_error(
        &mut self,
        url: &Url,
        headline: &str,
        detail: &str,
        status: Option<u16>,
        how: Direction,
    ) {
        self.loading = None;
        // The page the reader was on when they followed the failing link is
        // pushed, so Backspace from the error page returns to it. Not when
        // this error replaces another one: that would stack duplicates and
        // put a wall of error pages between them and where they were.
        if self.showing_error() {
            // The forward stack still goes, though. A navigation the reader
            // asked for happened, whether or not it arrived, and the pages
            // that were ahead of them are not ahead of them any more.
            if how == Direction::Fresh {
                self.forward.clear();
            }
        } else {
            self.settle_history(how);
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
        self.page.error
    }

    /// Says which status a page that is not the one asked for came with.
    fn say_status(&mut self, status: u16) {
        let mut text = Summary::new();
        text.push("the server answered ");
        text.push_usize(status as usize);
        self.say(text.as_str());
    }

    /// Moves the page now on screen onto one of the two stacks.
    fn push_current(&mut self, stack: Stack) {
        let entry = HistoryEntry {
            url: self.page.visit_url.clone(),
            line: self.page.first_line,
        };
        let stack = match stack {
            Stack::History => &mut self.history,
            Stack::Forward => &mut self.forward,
        };
        if stack.len() >= MAX_HISTORY {
            // The oldest goes, which is what makes this a bounded cost
            // rather than a growing one.
            stack.remove(0);
        }
        let _ = memory::push(stack, entry);
    }

    /// What a landed navigation does to the two stacks.
    ///
    /// In one place rather than at each call site, because the four cases
    /// are only correct as a set: every one of them either moves the page
    /// being left onto a stack or deliberately does not, and a fifth
    /// behaviour appearing somewhere else is how a forward button starts
    /// returning to pages nobody visited.
    fn settle_history(&mut self, how: Direction) {
        match how {
            Direction::Fresh => {
                self.push_current(Stack::History);
                self.forward.clear();
            }
            Direction::Back => self.push_current(Stack::Forward),
            Direction::Forward => self.push_current(Stack::History),
            Direction::Reload => {}
        }
    }

    fn go_back(&mut self) {
        let Some(entry) = self.history.pop() else {
            self.say("nothing to go back to");
            return;
        };
        self.request(Navigation {
            url: entry.url,
            restore: entry.line,
            how: Direction::Back,
        });
    }

    fn go_forward(&mut self) {
        let Some(entry) = self.forward.pop() else {
            self.say("nothing to go forward to");
            return;
        };
        self.request(Navigation {
            url: entry.url,
            restore: entry.line,
            how: Direction::Forward,
        });
    }

    /// Fetches the address showing again, keeping the reader's place.
    ///
    /// The scroll position is restored because the usual reason to reload
    /// is that the page may have changed under a reader who is partway
    /// down it. On an error page this retries what failed, which works
    /// because an error page's address is the address that failed.
    fn reload(&mut self) {
        let url = self.page.visit_url.clone();
        self.request(Navigation {
            url,
            restore: self.page.first_line,
            how: Direction::Reload,
        });
    }

    fn page_navigation(&mut self, navigation: Navigation) -> Option<Navigation> {
        let same_document = self.page.document.url().same_document(&navigation.url);
        let internal = match navigation.how {
            Direction::Reload => false,
            Direction::Back | Direction::Forward => same_document,
            Direction::Fresh => same_document && navigation.url.fragment().is_some(),
        };
        if !internal {
            return Some(navigation);
        }
        let repeated = navigation.how == Direction::Fresh && navigation.url == self.page.visit_url;
        if !repeated {
            self.settle_history(navigation.how);
        }
        self.page.visit_url = navigation.url;
        self.page.focus = None;
        self.clear_message();
        if matches!(navigation.how, Direction::Back | Direction::Forward) {
            self.scroll_to(navigation.restore);
        } else {
            self.scroll_to_fragment();
        }
        self.dirty.toolbar = true;
        self.dirty.status = true;
        None
    }

    fn scroll_to_fragment(&mut self) {
        let Some(fragment) = self.page.visit_url.fragment() else {
            return;
        };
        if fragment.is_empty() {
            self.scroll_to(0);
            return;
        }
        let Ok(Some(decoded)) = url::decode_fragment(fragment) else {
            self.say("fragment not found");
            return;
        };
        if let Some(line) = self
            .page
            .layout
            .line_of_anchor(&self.page.document, &decoded)
        {
            self.scroll_to(line);
        } else if decoded.eq_ignore_ascii_case("top") {
            self.scroll_to(0);
        } else {
            self.say("fragment not found");
        }
    }

    // --- input --------------------------------------------------------

    fn handle_key(&mut self, key: Key, loading: bool) -> Action {
        // Ctrl+Q is the way out from anywhere: mid-load, and with the
        // address field open on half-typed text. Checked before the field
        // gets the key so that leaving never depends on what is on screen,
        // which is the property Escape used to have and lost below. The
        // loop's own cleanup closes a transfer that is still running.
        if key == Key::Control(b'q') {
            return Action::Leave;
        }
        if self.editing.is_some() {
            return self.handle_editing_key(key);
        }
        match key {
            // Escape no longer leaves. It was the single most reachable key
            // on every keyboard here and it threw the page away, which is
            // exactly the accident it invited. What is left is stopping a
            // load, and otherwise undoing the two things that change the
            // screen without moving the page: the selected link and the
            // status line's answer.
            Key::Escape => {
                if loading {
                    return Action::Cancel;
                }
                if self.page.focus.is_some() {
                    self.page.focus = None;
                    self.dirty.viewport = true;
                    self.dirty.status = true;
                }
                self.clear_message();
            }
            // A plain `q` as well as Ctrl+Q, because CardKB v1.1 has no
            // Ctrl key: on that keyboard this is the only way out, not a
            // fallback. Free to bind because nothing outside the address
            // field takes typed text, and it is what every pager does.
            Key::Ascii(b'q') | Key::Ascii(b'Q') => return Action::Leave,
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
            // `[` and `]` beside Backspace because a pager's reader knows
            // them and because Backspace has no opposite: there is no
            // forward key on any of the keyboards here.
            Key::Ascii(0x08) | Key::Ascii(0x7F) | Key::Ascii(b'[') => self.go_back(),
            Key::Ascii(b']') => self.go_forward(),
            // Reload, on a bare letter for the same reason `q` is one:
            // CardKB v1.1 has no Ctrl key, and outside the address field
            // nothing here takes typed text. The other two are what a
            // desktop keyboard's reader will try first.
            Key::Ascii(b'r') | Key::Ascii(b'R') | Key::Control(b'r') | Key::Function(5) => {
                self.reload()
            }
            // The numbers behind "does this leak". Asked for rather than
            // logged: the per-page UART line this replaces printed on every
            // navigation, which made the log unreadable and still did not
            // answer the question, because the question is about the
            // difference between two moments the reader chooses.
            Key::Ascii(b'i') | Key::Ascii(b'I') | Key::Function(1) => return Action::Report,
            // Three ways into the address field: Ctrl+L as on a desktop
            // browser, F2 for CardKB (which has no Ctrl key), and Enter
            // with nothing selected. The last is not redundant with the
            // first -- it is the one a reader finds without being told.
            Key::Control(b'l') | Key::Function(2) => self.start_editing(),
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
        let text = self.page.visit_url.to_text().unwrap_or_default();
        let caret = text.len();
        self.editing = Some(Editing { text, caret });
        self.page.focus = None;
        self.say("edit the address; Enter goes, Escape cancels");
        self.dirty.toolbar = true;
        self.dirty.viewport = true;
    }

    /// Takes what was typed and turns it into a navigation.
    ///
    /// Two readings, and which one applies is decided by the shape of the
    /// text alone. A reference that only means something beside a page --
    /// `/path`, `?q=1`, `#part`, `../up` -- is resolved against the page
    /// showing, by the same `Url::resolve` a link on it would get. Anything
    /// else is an address in its own right and goes through
    /// `Url::parse_typed`, which supplies `http://` when no scheme was
    /// typed: `example.com/page` and `192.168.0.2:8080/x` are addresses,
    /// not files beside the current one.
    ///
    /// The split matters most for the host-and-port spelling. `Url` reads
    /// `localhost:8080` as a scheme called `localhost`, because that is what
    /// the grammar says; asking `parse_typed` instead of `classify` is what
    /// keeps the field from answering "only http:// and https:// addresses
    /// are understood" to the most ordinary thing anybody types into it.
    fn navigate_to_text(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.say("no address typed");
            return;
        }
        let page_relative = match url::classify(trimmed) {
            url::Reference::Root
            | url::Reference::Query
            | url::Reference::Fragment
            | url::Reference::Same => true,
            // A leading dot is the only thing that makes a bare word a
            // path rather than a host: `./page.html` is beside this page,
            // `page.html` is a host nobody can reach, and both readings
            // have to belong to somebody.
            url::Reference::Relative => trimmed.starts_with('.'),
            url::Reference::Absolute | url::Reference::SchemeRelative => false,
        };
        let outcome = if page_relative {
            self.page.document.url().resolve(trimmed)
        } else {
            Url::parse_typed(trimmed)
        };
        match outcome {
            Ok(target) => self.request(Navigation::fresh(target)),
            Err(error) => self.say(url::error_text(error)),
        }
    }

    /// A tap or a click at a screen position.
    ///
    /// Answers with an [`Action`] for the same reason `handle_key` does:
    /// the stop button and Escape have to reach the one piece of code that
    /// gives a running transfer's socket back, and a second copy of it
    /// behind the button is a second place to forget.
    fn click(&mut self, x: usize, y: usize) -> Action {
        if y < TOOLBAR_HEIGHT {
            return self.click_toolbar(x);
        }
        if y >= VIEWPORT_BOTTOM {
            return Action::Continue;
        }
        if self.editing.is_some() {
            self.editing = None;
            self.clear_message();
            self.dirty.toolbar = true;
        }
        let Some(top) = self.top_offset() else {
            return Action::Continue;
        };
        let document_y = top + (y - VIEWPORT_TOP) as u32;
        let Some(document_x) = x.checked_sub(MARGIN) else {
            return Action::Continue;
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
        Action::Continue
    }

    /// A tap on the toolbar: a button, the lock, the clear cross, or the
    /// address field.
    ///
    /// Every hit area is the full height of the bar. There is nothing else
    /// up here to hit, and a target as tall as the band is the difference
    /// between a button that can be pressed with a finger and one that
    /// needs the mouse.
    fn click_toolbar(&mut self, x: usize) -> Action {
        if let Some(button) = button_at(x) {
            return self.press(button);
        }
        if (ICON_LEFT..ICON_LEFT + ICON_WIDTH).contains(&x) {
            self.explain_security();
            return Action::Continue;
        }
        // Only while the field is open, which is the whole of what makes
        // this position mean two things safely: closed, it is part of the
        // address and opens the field like the rest of it.
        if self.editing.is_some() && (CLEAR_LEFT..ADDRESS_RIGHT).contains(&x) {
            self.clear_address();
            return Action::Continue;
        }
        if (ADDRESS_LEFT..ADDRESS_RIGHT).contains(&x) {
            self.start_editing();
        }
        Action::Continue
    }

    fn press(&mut self, button: Button) -> Action {
        // A button is not the address field, so an open field closes on the
        // way: leaving half-typed text over a page that is being replaced
        // is the one state where the toolbar says two things at once.
        if self.editing.is_some() {
            self.editing = None;
            self.clear_message();
            self.dirty.toolbar = true;
        }
        match button {
            Button::Back => self.go_back(),
            Button::Forward => self.go_forward(),
            // The same button, and the same code Escape reaches: while
            // something is arriving it stops it, and otherwise it fetches
            // the address again.
            Button::Reload if self.loading.is_some() => return Action::Cancel,
            Button::Reload => self.reload(),
        }
        Action::Continue
    }

    /// Empties the address field, leaving it open with the caret at the
    /// start. Only reachable while it is open.
    fn clear_address(&mut self) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        editing.text.clear();
        editing.caret = 0;
        self.dirty.toolbar = true;
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
        let lines = self.page.layout.lines();
        let mut count = 0usize;
        let mut previous = None;
        for line in &lines[range] {
            if previous != Some(line.y) {
                count += 1;
                previous = Some(line.y);
            }
        }
        count.saturating_sub(1).max(1) as i32
    }

    fn scroll_by(&mut self, lines: i32) {
        let all = self.page.layout.lines();
        if all.is_empty() {
            return;
        }
        let mut target = self.page.first_line.min(all.len() - 1);
        if lines > 0 {
            for _ in 0..lines {
                let y = all[target].y;
                target = all.partition_point(|line| line.y <= y).min(all.len() - 1);
            }
        } else {
            for _ in 0..lines.unsigned_abs() {
                let y = all[target].y;
                let first = all.partition_point(|line| line.y < y);
                if first == 0 {
                    target = 0;
                    break;
                }
                let previous_y = all[first - 1].y;
                target = all.partition_point(|line| line.y < previous_y);
            }
        }
        self.scroll_to(target);
    }

    fn scroll_to(&mut self, line: usize) {
        let lines = self.page.layout.lines();
        let line = line.min(self.last_top_line());
        let line = lines
            .get(line)
            .map(|target| lines.partition_point(|candidate| candidate.y < target.y))
            .unwrap_or(0);
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

    fn draw_all(&mut self, framebuffer: &mut Framebuffer, service: &mut dyn FnMut()) -> bool {
        // A modal screen may have painted below this page's previous last line.
        self.painted_bottom = VIEWPORT_BOTTOM;
        framebuffer.fill_rect(0, TOOLBAR_HEIGHT, WIDTH, CONTENT_GAP, PAGE_BACKGROUND);
        let gap_ok = framebuffer.flush_rect(0, TOOLBAR_HEIGHT, WIDTH, CONTENT_GAP);
        self.dirty = Dirty {
            toolbar: true,
            viewport: true,
            status: true,
        };
        self.draw_dirty(framebuffer, service) && gap_ok
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
    fn draw_dirty(&mut self, framebuffer: &mut Framebuffer, service: &mut dyn FnMut()) -> bool {
        let mut ok = true;
        let dirty = self.dirty;
        self.dirty = Dirty::default();
        if dirty.toolbar {
            self.draw_toolbar(framebuffer);
            ok &= framebuffer.flush_rect(
                tab5_system_ui::APP.x,
                0,
                tab5_system_ui::APP.width,
                TOOLBAR_HEIGHT,
            );
        }
        if dirty.viewport {
            let started = tick::now_ms();
            let height = self.draw_viewport(framebuffer, service);
            ok &= flush_viewport(framebuffer, height, service);
            let elapsed = tick::now_ms().saturating_sub(started);
            if elapsed > self.slowest_repaint_ms {
                self.slowest_repaint_ms = elapsed;
                uart::log_hex(
                    b"BROWSER: slowest viewport repaint so far, ms=",
                    elapsed as u32,
                );
            }
        }
        if dirty.status {
            self.draw_status(framebuffer);
            ok &= framebuffer.flush_rect(0, VIEWPORT_BOTTOM, WIDTH, STATUS_HEIGHT);
        }
        ok
    }

    fn draw_toolbar(&self, framebuffer: &mut Framebuffer) {
        framebuffer.fill_rect(
            tab5_system_ui::APP.x,
            0,
            tab5_system_ui::APP.width,
            TOOLBAR_HEIGHT,
            CHROME_BACKGROUND,
        );
        // The gap below the bar is painted here rather than by the
        // viewport, so it belongs to the band that is cheapest to repaint
        // and cannot be left behind by a viewport repaint that starts
        // lower down.

        self.draw_buttons(framebuffer);
        // The lock comes next and is never absent. What it says is what the
        // connection actually proved, not what the address asked for: an
        // `https://` URL whose peer nobody identified gets the same red
        // open lock as plaintext, because to a reader the two mean the same
        // thing -- what is on screen may not be from where the address
        // says. The words are one tap away, on the lock itself.
        let (lock, color, _) = self.security_state();
        draw_lock(framebuffer, ICON_LEFT, lock, color);

        match (&self.editing, &self.loading) {
            (Some(editing), _) => self.draw_address_field(framebuffer, editing),
            (None, Some(loading)) => draw_clipped(
                framebuffer,
                ADDRESS_LEFT,
                CHROME_TEXT_Y,
                &loading.url,
                ADDRESS_RIGHT - ADDRESS_LEFT,
                CHROME_TEXT,
            ),
            (None, None) => {
                if let Ok(text) = self.page.visit_url.to_text() {
                    draw_clipped(
                        framebuffer,
                        ADDRESS_LEFT,
                        CHROME_TEXT_Y,
                        &text,
                        ADDRESS_RIGHT - ADDRESS_LEFT,
                        CHROME_TEXT,
                    );
                }
            }
        }
    }

    /// Back, forward, and the one that is reload or stop depending on
    /// whether anything is arriving.
    ///
    /// Greyed rather than hidden when there is nowhere to go: a button that
    /// disappears takes the two beside it with it, and a reader who has
    /// learned where "back" is has to find it again on every page.
    fn draw_buttons(&self, framebuffer: &mut Framebuffer) {
        let enabled = [
            !self.history.is_empty(),
            !self.forward.is_empty(),
            // Reload always works, and so does stopping something that is
            // on its way.
            true,
        ];
        let glyphs = [
            BACK_GLYPH,
            FORWARD_GLYPH,
            if self.loading.is_some() {
                STOP_GLYPH
            } else {
                RELOAD_GLYPH
            },
        ];
        for index in 0..BUTTON_COUNT {
            let color = if enabled[index] {
                CHROME_TEXT
            } else {
                DISABLED_COLOR
            };
            // Twice the body's size, centred from the same A4 metrics the
            // renderer uses. Symbol advances need not all be half-width.
            let glyph_width =
                crate::font::ui_text_width(glyphs[index], crate::font::UiTextStyle::HEADING);
            let x =
                BUTTONS_LEFT + index * BUTTON_WIDTH + BUTTON_WIDTH.saturating_sub(glyph_width) / 2;
            let y = (TOOLBAR_HEIGHT - CELL_HEIGHT * 2) / 2;
            framebuffer.draw_gui_text(x, y, glyphs[index], 2, color, None);
        }
    }

    /// How the lock is drawn, in what colour, and the sentence that says
    /// the same thing in words.
    ///
    /// All three from one place, because they are one statement: an icon
    /// whose colour and whose explanation are decided separately is an icon
    /// that can end up green beside a sentence saying nothing was proved.
    ///
    /// While a page is loading this describes the connection being made,
    /// not the page still on screen: the address field has already moved to
    /// the new URL, and a lock left describing the old page beside the new
    /// address would be the one combination that actively misleads.
    ///
    /// A connection that has not proved anything yet says so. It does not
    /// borrow the previous page's answer and it does not guess from the
    /// scheme.
    fn security_state(&self) -> (Lock, u16, &'static str) {
        let security = match &self.loading {
            Some(loading) => match loading.security {
                Some(security) => security,
                None => {
                    return (Lock::Outline, CHROME_TEXT, CONNECTING_TEXT);
                }
            },
            None => match self.page.security {
                Some(security) => security,
                // A built-in page or this viewer's error page: never
                // fetched, so nothing was proved about anybody.
                None => fetch::PageSecurity::Cleartext,
            },
        };
        // `is_warning` is the same question the lock asks: was the peer
        // identified at all? Plaintext and unauthenticated TLS both answer
        // no, and both get the open lock.
        let (lock, color) = if security.is_warning() {
            (Lock::Open, INSECURE_COLOR)
        } else {
            (Lock::Closed, AUTHENTICATED_COLOR)
        };
        (lock, color, security.explanation())
    }

    /// Puts the lock's meaning into the status line, in words.
    ///
    /// This is what an icon costs and what pays for it back. The toolbar
    /// used to spell `TLS UNVERIFIED` out in fourteen cells of the space
    /// the address needed; now it says it when asked.
    fn explain_security(&mut self) {
        let (_, _, explanation) = self.security_state();
        self.say(explanation);
    }

    /// The address field, scrolled so the caret is always on screen.
    fn draw_address_field(&self, framebuffer: &mut Framebuffer, editing: &Editing) {
        let width = ADDRESS_RIGHT - ADDRESS_LEFT;
        // As tall as a scaled glyph, so the clear cross at the right sits
        // inside the field rather than half on the toolbar behind it. The
        // text is centred in it either way: `CHROME_TEXT_Y` leaves the same
        // eight pixels above and below.
        framebuffer.fill_rect(
            ADDRESS_LEFT - 4,
            FIELD_TOP,
            width + 8,
            FIELD_HEIGHT,
            EDIT_BACKGROUND,
        );
        let text_budget = CLEAR_LEFT.saturating_sub(8 + ADDRESS_LEFT);
        // Follow the caret by measured pixels. URL input is ASCII, so walking
        // byte offsets is also walking character boundaries.
        let mut first = editing.caret.min(editing.text.len());
        while first > 0 {
            let candidate = first - 1;
            let width = crate::font::ui_text_width(
                &editing.text[candidate..editing.caret],
                crate::font::UiTextStyle::BODY,
            );
            if width + 2 > text_budget {
                break;
            }
            first = candidate;
        }
        draw_clipped(
            framebuffer,
            ADDRESS_LEFT,
            CHROME_TEXT_Y,
            &editing.text[first..],
            text_budget,
            BLACK,
        );
        let caret_x = crate::font::ui_text_width(
            &editing.text[first..editing.caret],
            crate::font::UiTextStyle::BODY,
        );
        framebuffer.fill_rect(
            ADDRESS_LEFT + caret_x,
            CHROME_TEXT_Y,
            2,
            CELL_HEIGHT * CHROME_SCALE,
            EDIT_CARET,
        );
        // The cross that empties the field, inside the field's own right
        // edge and drawn only while it is open. An address is usually
        // edited at its end, which is what `start_editing` is arranged
        // around; this is for the other case, where the whole thing is
        // being replaced and deleting it a character at a time on a thumb
        // keyboard is the expensive part.
        //
        // Grey rather than black when there is nothing to clear, so that
        // the button says whether it will do anything before it is pressed.
        let color = if editing.text.is_empty() {
            DISABLED_COLOR
        } else {
            CHROME_TEXT
        };
        let glyph_width = crate::font::ui_text_width(STOP_GLYPH, crate::font::UiTextStyle::HEADING);
        framebuffer.draw_gui_text(
            CLEAR_LEFT + CLEAR_WIDTH.saturating_sub(glyph_width) / 2,
            FIELD_TOP,
            STOP_GLYPH,
            2,
            color,
            None,
        );
    }

    fn draw_status(&self, framebuffer: &mut Framebuffer) {
        framebuffer.fill_rect(0, VIEWPORT_BOTTOM, WIDTH, STATUS_HEIGHT, CHROME_BACKGROUND);
        let budget = WIDTH - 2 * MARGIN;
        if let Some(loading) = &self.loading
            && self.message.is_none()
        {
            // How much has arrived, which used to sit at the right of the
            // toolbar beside the link and line counts. Those went; this
            // stayed, because it is the only thing on the screen that says
            // a slow page is moving at all.
            let mut text = Summary::new();
            text.push("loading ");
            text.push_usize(loading.received / 1024);
            text.push(" KiB; Escape or the stop button stops");
            draw_ascii(
                framebuffer,
                MARGIN,
                STATUS_TEXT_Y,
                text.as_str(),
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
                budget,
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
                budget,
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
        // Table geometry is painted before its text.  Cell outlines rather
        // than row-wide rules naturally omit boundaries through rowspan and
        // colspan cells.
        for cell in self.page.layout.cells() {
            let cell_bottom = cell.y.saturating_add(cell.height);
            let viewport_bottom = top.saturating_add(VIEWPORT_HEIGHT as u32);
            if cell_bottom <= top || cell.y >= viewport_bottom {
                continue;
            }
            let visible_top = cell.y.max(top);
            let visible_bottom = cell_bottom.min(viewport_bottom);
            let screen_y = VIEWPORT_TOP + (visible_top - top) as usize;
            let visible_height = (visible_bottom - visible_top) as usize;
            let screen_x = MARGIN + cell.x as usize;
            if cell.header && visible_height != 0 {
                framebuffer.fill_rect(
                    screen_x,
                    screen_y,
                    cell.width as usize,
                    visible_height,
                    CHROME_BACKGROUND,
                );
            }
            let border = cell.border as usize;
            if border == 0 {
                continue;
            }
            if cell.y >= top {
                framebuffer.fill_rect(
                    screen_x,
                    VIEWPORT_TOP + (cell.y - top) as usize,
                    cell.width as usize,
                    border,
                    RULE_COLOR,
                );
            }
            if cell_bottom <= viewport_bottom {
                framebuffer.fill_rect(
                    screen_x,
                    VIEWPORT_TOP + (cell_bottom - top) as usize - border,
                    cell.width as usize,
                    border,
                    RULE_COLOR,
                );
            }
            framebuffer.fill_rect(screen_x, screen_y, border, visible_height, RULE_COLOR);
            framebuffer.fill_rect(
                screen_x + cell.width as usize - border,
                screen_y,
                border,
                visible_height,
                RULE_COLOR,
            );
        }
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
                piece.mono,
            );
            if is_link && !is_focused {
                // Underlined as well as coloured: colour alone is not an
                // affordance for everyone, and the panel's blue on white is
                // not a large contrast step.
                //
                // Drawn in the line's gap rather than against the glyphs.
                // The font's descenders reach the bottom of its box, so a
                // rule at the box's edge touches every `g` and `y`; the gap
                // the line spacing adds is exactly the room this needs.
                let glyph_box = CELL_HEIGHT * scale;
                let underline = screen_y + glyph_box.min(line.height as usize - scale);
                framebuffer.fill_rect(x, underline, piece.width as usize, scale, LINK_COLOR);
            }
        }
    }
}

/// How far the Wi-Fi has got, as four steps a reader can learn.
///
/// Not signal strength. Asking the C6 for an RSSI is an RPC round trip,
/// and the number would answer a question this screen is not being asked:
/// what a reader wants to know when a page will not load is whether the
/// board is on the network at all. The bars fill up as the connection
/// gets further along, and tapping them says exactly where it stopped.
/// The three states the padlock is drawn in.
///
/// Two of them, not three: plaintext and unauthenticated TLS share the open
/// lock. The reader's question is whether what is on screen is from where
/// the address says, and neither of those answers it -- one is not
/// encrypted and the other is encrypted to nobody in particular. The
/// difference between them is `http://` against `https://`, which is
/// already in the address field a few pixels to the right.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lock {
    /// Nothing was proved about who answered.
    Open,
    /// A pin matched.
    Closed,
    /// Nothing has been asked yet: a connection still being made.
    Outline,
}

/// Draws the padlock into its slot.
///
/// Rectangles rather than a glyph, because the font has no padlock: the
/// body is one, the shackle is three, and the open one is the same shackle
/// unhooked on the left.
fn draw_lock(framebuffer: &mut Framebuffer, slot_x: usize, lock: Lock, color: u16) {
    // 14 by 20 inside a 24 wide slot, vertically centred in the toolbar.
    let x = slot_x + (ICON_WIDTH - LOCK_WIDTH) / 2;
    let y = (TOOLBAR_HEIGHT - LOCK_HEIGHT) / 2;
    let body_y = y + LOCK_SHACKLE_HEIGHT;
    let body_height = LOCK_HEIGHT - LOCK_SHACKLE_HEIGHT;
    match lock {
        Lock::Outline => {
            framebuffer.stroke_rect(x, body_y, LOCK_WIDTH, body_height, color);
            // A one-pixel shackle, closed. Thinner than the other two all
            // over, which is what says "not an answer yet" without
            // inventing a third shape.
            framebuffer.fill_rect(x + 3, y, 1, LOCK_SHACKLE_HEIGHT, color);
            framebuffer.fill_rect(x + 3, y, 9, 1, color);
            framebuffer.fill_rect(x + 10, y, 1, LOCK_SHACKLE_HEIGHT, color);
            return;
        }
        Lock::Closed => {
            framebuffer.fill_rect(x + 2, y, 2, LOCK_SHACKLE_HEIGHT, color);
            framebuffer.fill_rect(x + 2, y, 10, 2, color);
            framebuffer.fill_rect(x + 10, y, 2, LOCK_SHACKLE_HEIGHT, color);
        }
        Lock::Open => {
            // Hinged on the right and lifted clear of the body on the left,
            // which is the shape everything else draws an open lock as.
            framebuffer.fill_rect(x + 6, y, 2, LOCK_SHACKLE_HEIGHT / 2, color);
            framebuffer.fill_rect(x + 6, y, 10, 2, color);
            framebuffer.fill_rect(x + 14, y, 2, LOCK_SHACKLE_HEIGHT, color);
        }
    }
    framebuffer.fill_rect(x, body_y, LOCK_WIDTH, body_height, color);
    // The keyhole, punched back out in the toolbar's own colour so the body
    // reads as a body rather than as a filled rectangle.
    framebuffer.fill_rect(
        x + LOCK_WIDTH / 2 - 1,
        body_y + 3,
        2,
        body_height - 6,
        CHROME_BACKGROUND,
    );
}

const LOCK_WIDTH: usize = 14;
const LOCK_HEIGHT: usize = 20;
const LOCK_SHACKLE_HEIGHT: usize = 8;

/// Writes the viewport back in vertical bands, servicing the link between
/// them.
///
/// Banded by *x* and not by *y*. The framebuffer is rotated, so a logical
/// column is a run of native addresses and a logical row is a stride across
/// all of them: splitting by y would hand `flush_rect` a rectangle whose
/// bounding span is still almost the whole buffer, and write back the same
/// 1.6 MB in ten goes instead of one.
#[inline(never)]
fn flush_viewport(framebuffer: &Framebuffer, height: usize, service: &mut dyn FnMut()) -> bool {
    if height == 0 {
        return true;
    }
    let mut ok = true;
    let mut x = 0;
    while x < WIDTH {
        let width = FLUSH_BAND_WIDTH.min(WIDTH - x);
        ok &= framebuffer.flush_rect(x, VIEWPORT_TOP, width, height);
        service();
        x += width;
    }
    ok
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
    let marker_style =
        crate::font::UiTextStyle::new(crate::font::UiFace::Sans, if scale >= 2 { 32 } else { 16 });
    let x = right.saturating_sub(crate::font::ui_text_width(text.as_str(), marker_style));
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

/// Draws one run of page text.
///
/// Transparent: the caller has already laid down the background, so only
/// inked pixels are written. Widths, combining marks and the boxes drawn for
/// characters the font does not cover are all `draw_text`'s business, which
/// is the same function `layout` measured this run with.
#[inline(never)]
fn draw_text_run(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    text: &str,
    scale: usize,
    color: u16,
    bold: bool,
    mono: bool,
) {
    let face = if mono {
        crate::font::UiFace::Mono
    } else {
        crate::font::UiFace::Sans
    };
    let style = crate::font::UiTextStyle::new(face, if scale >= 2 { 32 } else { 16 });
    framebuffer.draw_ui_text(x, y, text, style, color, None);
    if bold {
        // Struck twice, one pixel apart. The font has one weight, so bold
        // has to be synthesised or dropped -- and dropping it means `<b>`
        // renders as nothing at all. One physical pixel rather than one
        // glyph pixel (`scale`): at scale 2 a full-cell offset would smear
        // into the next column.
        framebuffer.draw_ui_text(x + 1, y, text, style, color, None);
    }
}

/// Draws chrome text and returns where it ended.
#[inline(never)]
fn draw_ascii(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    text: &str,
    scale: usize,
    color: u16,
) -> usize {
    let style =
        crate::font::UiTextStyle::new(crate::font::UiFace::Sans, if scale >= 2 { 32 } else { 16 });
    x + framebuffer.draw_ui_text(x, y, text, style, color, None)
}

/// Draws as much of `text` as fits in `budget` pixels, so a long URL or a
/// long message cannot run into whatever is at the other end of the bar.
///
/// Measured in pixels rather than characters because a status message is no
/// longer guaranteed to be ASCII: a page's title or a host name can carry
/// full-width characters, and those take two cells each.
#[inline(never)]
fn draw_clipped(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    text: &str,
    budget: usize,
    color: u16,
) {
    let mut cursor = x;
    let end = x + budget;
    let style = crate::font::UiTextStyle::BODY;
    for character in text.chars() {
        let mut encoded = [0u8; 4];
        let character_text = character.encode_utf8(&mut encoded);
        let advance = crate::font::ui_text_width(character_text, style);
        if cursor + advance > end {
            break;
        }
        framebuffer.draw_ui_text(cursor, y, character_text, style, color, None);
        cursor += advance;
    }
}

/// Builds a page from a finished document.
fn build_page(document: Document) -> Result<Page, Error> {
    let layout = Layout::build(
        &document,
        PAGE_WIDTH as u16,
        Metrics {
            glyph_height: CELL_HEIGHT as u16,
            line_gap_percent: crate::browser::layout::LINE_GAP_PERCENT,
        },
    )?;
    let order = layout.link_order()?;
    let visit_url = document.url().clone();
    Ok(Page {
        document,
        visit_url,
        security: None,
        error: false,
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
        "</code></p><hr><p>Backspace goes back, r tries again. \
         <a href=\"http://built-in/\">Home</a></p>",
    )?;

    // Parsed rather than laid out by hand: an error page that goes through
    // the same tokenizer, document builder and layout as every other page
    // cannot be the one place where a wrapping or drawing bug hides.
    //
    // The document's own address is the address that failed, not a made-up
    // one under the built-in host. It used to be `http://built-in/error`,
    // which put an address in the toolbar that nothing could be at: the
    // reader could not see what had failed, could not open the field and
    // correct a typo in it, and had nothing to reload. Everything on this
    // page links absolutely, so nothing resolves against it.
    let mut parser = Parser::new(url.clone())?;
    parser.feed(markup.as_bytes())?;
    let mut page = build_page(parser.finish()?)?;
    page.error = true;
    Ok(page)
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
        FragmentDocument,
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
                Body::FragmentDocument => write_fragments(parser),
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

    fn write_fragment_filler(parser: &mut Parser, label: &[u8]) -> Result<(), Error> {
        for _ in 0..14 {
            parser.feed(b"<p>")?;
            for _ in 0..24 {
                parser.feed(label)?;
                parser.feed(b" corridor text ")?;
            }
            parser.feed(b"</p>")?;
        }
        Ok(())
    }

    fn write_fragments(parser: &mut Parser) -> Result<(), Error> {
        parser.feed(
            b"<title>fragment navigation</title><h1 id='top-heading'>Fragment navigation</h1>\
              <p>Every target is separated by many screens. The repeated uppercase word identifies the current corridor.</p>\
              <ul><li><a href='#first'>FIRST target</a></li><li><a href='#second'>SECOND target</a></li>\
              <li><a href='#legacy'>LEGACY named target</a></li><li><a href='#inline'>INLINE target</a></li>\
              <li><a href='#end'>END target</a></li><li><a href='#missing'>Missing target</a></li></ul>\
              <p>After a jump, scroll several screens into that corridor. Back and Forward must restore that exact position.</p>\
              <h2 id='first'>FIRST TARGET -- corridor starts here</h2>",
        )?;
        write_fragment_filler(parser, b"FIRST")?;
        parser.feed(b"<h2 id='second'>SECOND TARGET -- corridor starts here</h2>")?;
        write_fragment_filler(parser, b"SECOND")?;
        parser.feed(b"<h2><a name='legacy'></a>LEGACY TARGET -- corridor starts here</h2>")?;
        write_fragment_filler(parser, b"LEGACY")?;
        parser.feed(b"<p>INLINE corridor begins with text before the target so its line can be checked. More text before the <span id='inline'>INLINE TARGET INSIDE THIS PARAGRAPH</span>, followed by text that must remain on the same wrapped paragraph.</p>")?;
        write_fragment_filler(parser, b"INLINE")?;
        parser.feed(b"<p><a href='#'>Document top via empty fragment</a>. <a href=''>Reload without fragment</a>.</p><p><a href='/sample'>Other document</a>, for A to B to Back testing.</p>")?;
        write_fragment_filler(parser, b"FINAL")?;
        parser.feed(b"<span id='end'></span><h2>END TARGET -- final screen</h2><p><a href='#first'>First again</a> | <a href='/'>Home</a></p>")
    }

    pub const HOME: &Page = &Page {
        url: "http://built-in/",
        body: Body::Fixed(
            "<title>Tab5 browser</title>\
             <h1>Tab5 browser</h1>\
             <p>This is a hypertext viewer, not a web browser. It fetches HTML \
             and plain text over HTTP or HTTPS, reads files off this device with \
             <code>file:</code>, and shows the text and the links in it. There \
             is no CSS, no JavaScript and no images, and an HTTPS connection \
             proves who answered only where a pin matches -- the padlock at \
             the top says which, and says it in words if you tap it.</p>\
             <h2>Driving it</h2>\
             <ul>\
             <li><b>Tab</b> selects the next link; the status line shows where \
             it goes</li>\
             <li><b>Enter</b> follows the selected link, or -- with nothing \
             selected -- opens the address field on the address already \
             showing, with the caret at the end; <b>Ctrl+L</b> and <b>F2</b> \
             open it whether or not a link is selected</li>\
             <li>in the address field, <b>Left</b> and <b>Right</b> move the \
             caret, <b>Home</b> and <b>End</b> jump to either end, and \
             <b>Backspace</b> and <b>Delete</b> remove a character</li>\
             <li><b>Backspace</b> or <b>[</b> goes back a page, <b>]</b> goes \
             forward again, and <b>r</b> fetches this page again -- the three \
             buttons at the top left do the same, and the third one stops a \
             page that is still arriving</li>\
             <li><b>Up</b> and <b>Down</b> scroll a line, <b>Page Up</b> and \
             <b>Page Down</b> a screen, <b>Home</b> and <b>End</b> the whole \
             document</li>\
             <li><b>Space</b> is another Page Down</li>\
             <li>A touch or a click selects and follows a link; a click on the \
             address field opens it, and while it is open the cross at its \
             right empties it; a mouse wheel scrolls</li>\
             <li><b>Escape</b> stops a page that is loading, closes the address \
             field, and otherwise drops the selected link. It does not \
             leave</li>\
             <li><b>Ctrl+Q</b> leaves, from anywhere. So does <b>q</b> when \
             the address field is closed</li>\
             </ul>\
             <h2>Files on this device</h2>\
             <p><a href=\"file:///\">Browse the mounted volumes</a>. A directory \
             becomes a page of links; a <code>.html</code> file is read as \
             markup and everything else as plain text. Nothing is written.</p>\
             <h2>Built-in pages</h2>\
             <ul>\
             <li><a href=\"/sample\">Everything it can display</a></li>\
             <li><a href=\"/long\">A long document, for scrolling</a></li>\
             <li><a href=\"/wide\">One line as long as a URL may be</a></li>\
             <li><a href=\"/japanese\">Japanese text, mixed widths</a></li>\
             <li><a href=\"/table\">Table layout acceptance page</a></li>\
             <li><a href=\"/fragments\">Fragment navigation acceptance</a></li>\
             <li><a href=\"/empty\">A document with nothing in it</a></li>\
             </ul>\
             <hr>\
             <p>These pages are in flash and need no network. Fetching anything \
             else needs a connection: tap the Wi-Fi bars at the right of the \
             toolbar to choose a network. <code>browser &lt;url&gt;</code> opens \
             one directly, and an address typed without a scheme -- here or in \
             the address field -- is read as <code>http://</code>.</p>",
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
             <h2>Proportional and fixed width</h2>\
             <p>Proportional Sans makes these groups visibly different: \
             iiiiiiii WWWWWWWW 00000000.</p>\
             <p>Inline code uses Mono, so every character has one advance: \
             <code>iiiiiiii|WWWWWWWW|00000000|Tab5</code></p>\
             <pre>MONO GRID -- bars must align by column
12345678|12345678|12345678|1234
iiiiiiii|WWWWWWWW|00000000|Tab5
</pre>\
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
             <p>Non-ASCII is drawn from the same font: \u{65e5}\u{672c}\u{8a9e}. \
             Only what the subset leaves out becomes a box -- see the \
             <a href=\"/japanese\">Japanese page</a>.</p>\
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

    /// Japanese text, for the cases that only appear once glyphs have two
    /// widths.
    ///
    /// Not a demo. Line breaking, piece widths, underlines and hit testing
    /// all changed when characters stopped being one cell each
    /// (`docs/FONT_MIGRATION_PLAN.md`), and this is the page that shows
    /// whether they agree with each other -- with no network involved, so it
    /// can be looked at on a board that has never associated.
    pub const JAPANESE: &Page = &Page {
        url: "http://built-in/japanese",
        body: Body::Fixed(
            "<title>\u{65E5}\u{672C}\u{8A9E}</title>\
             <h1>\u{65E5}\u{672C}\u{8A9E}\u{306E}\u{8868}\u{793A}</h1>\
             <p>\u{534A}\u{89D2}\u{306F}8\u{30D4}\u{30AF}\u{30BB}\u{30EB}\u{3001}\
             \u{5168}\u{89D2}\u{306F}16\u{30D4}\u{30AF}\u{30BB}\u{30EB}\u{9001}\u{308A}\u{3067}\u{3059}\u{3002}\
             ASCII\u{3068}\u{6DF7}\u{3056}\u{3063}\u{305F}text\u{3082}\u{3001}\
             \u{6298}\u{8FD4}\u{3057}\u{306F}\u{6587}\u{5B57}\u{6570}\u{3067}\u{306F}\u{306A}\u{304F}\
             pixel\u{3067}\u{6C7A}\u{307E}\u{308A}\u{307E}\u{3059}\u{3002}\
             \u{53E5}\u{8AAD}\u{70B9}\u{3084}\u{9589}\u{3058}\u{62EC}\u{5F27}\u{304C}\
             \u{884C}\u{982D}\u{3078}\u{6765}\u{306A}\u{3044}\u{3053}\u{3068}\u{3082}\
             \u{78BA}\u{304B}\u{3081}\u{3089}\u{308C}\u{307E}\u{3059}\u{3002}</p>\
             <h2>\u{7D50}\u{5408}\u{6587}\u{5B57}\u{3068}\u{7570}\u{4F53}\u{5B57}</h2>\
             <p>\u{5206}\u{89E3}\u{3055}\u{308C}\u{305F}\u{6FC1}\u{70B9}\u{FF1A}\
             \u{304B}\u{3099} \u{304D}\u{309A} e\u{301} \u{2014} \
             \u{5E45}\u{3092}\u{5897}\u{3084}\u{3055}\u{305A}\u{76F4}\u{524D}\u{306E}\
             \u{5B57}\u{3078}\u{91CD}\u{306A}\u{308A}\u{307E}\u{3059}\u{3002}\
             \u{4EBA}\u{540D}\u{306E}\u{7570}\u{4F53}\u{5B57}\u{FF1A}\u{9AD9}\u{FA11}\u{3002}\
             \u{53CE}\u{9332}\u{3057}\u{3066}\u{3044}\u{306A}\u{3044}\u{6587}\u{5B57}\u{FF1A}\
             \u{20BB7} \u{1F600} \u{FDFD} \u{2014} \
             \u{7A7A}\u{767D}\u{3067}\u{306F}\u{306A}\u{304F}\u{4E2D}\u{7A7A}\u{306E}\
             \u{67A0}\u{306B}\u{306A}\u{308A}\u{307E}\u{3059}\u{3002}</p>\
             <h3>\u{30EA}\u{30F3}\u{30AF}\u{306E}\u{6298}\u{8FD4}\u{3057}</h3>\
             <p>\u{9577}\u{3044}\u{6587}\u{306E}\u{9014}\u{4E2D}\u{306B}\
             <a href=\"/\">\u{884C}\u{3092}\u{307E}\u{305F}\u{3050}\u{307B}\u{3069}\
             \u{9577}\u{3044}\u{30EA}\u{30F3}\u{30AF}\u{3092}\u{7F6E}\u{3044}\u{3066}\
             \u{3042}\u{308A}\u{307E}\u{3059}\u{3002}\u{4E0B}\u{7DDA}\u{3068}\
             \u{9078}\u{629E}\u{80CC}\u{666F}\u{3001}touch\u{306E}\u{5F53}\u{305F}\u{308A}\
             \u{5224}\u{5B9A}\u{304C}\u{63CF}\u{753B}\u{3068}\u{4E00}\u{81F4}\u{3059}\u{308B}\u{304B}\
             \u{898B}\u{3066}\u{304F}\u{3060}\u{3055}\u{3044}\u{3002}\u{884C}\u{3092}\u{307E}\u{305F}\u{3044}\u{3060}\u{5F8C}\u{534A}\u{306B}\u{3082}\u{540C}\u{3058}\u{4E0B}\u{7DDA}\u{304C}\u{4ED8}\u{304D}\u{3001}\u{9078}\u{629E}\u{3057}\u{305F}\u{3068}\u{304D}\u{306B}\u{4E21}\u{65B9}\u{306E}\u{884C}\u{304C}\u{53CD}\u{8EE2}\u{3059}\u{308B}\u{306F}\u{305A}\u{3067}\u{3059}</a>\u{3002}\
             \u{3053}\u{306E}\u{5F8C}\u{308D}\u{306B}\u{3082}\u{6587}\u{7AE0}\u{304C}\
             \u{7D9A}\u{304D}\u{307E}\u{3059}\u{3002}</p>\
             <ul><li>\u{534A}\u{89D2}\u{30AB}\u{30CA}\u{FF1A}\u{FF76}\u{FF9E}\u{FF77}\u{FF9E}\u{FF78}\u{FF9E}\u{FF80}\u{FF9E}</li>\
             <li>Latin-1\u{FF1A}r\u{E9}sum\u{E9} \u{FC}ber Stra\u{DF}e</li>\
             <li>\u{5168}\u{89D2}\u{82F1}\u{6570}\u{FF1A}\u{FF21}\u{FF22}\u{FF23}\u{FF10}\u{FF11}\u{FF12}</li></ul>\
             <pre>pre \u{3067}\u{3082} \u{6298}\u{8FD4}\u{3057}\u{306F} pixel \u{5358}\u{4F4D}\n\u{7A7A}\u{767D}\u{3068}   \u{6539}\u{884C}\u{306F}\u{305D}\u{306E}\u{307E}\u{307E}\n</pre>\
             <p><a href=\"/\">Home</a></p>",
        ),
    };

    pub const EMPTY: &Page = &Page {
        url: "http://built-in/empty",
        body: Body::Fixed(
            "<title>empty</title><!-- nothing at all --><p><a href=\"/\">Home</a></p>",
        ),
    };

    /// Flash-resident acceptance page: it exercises tables with Wi-Fi off.
    pub const TABLE: &Page = &Page {
        url: "http://built-in/table",
        body: Body::Fixed(
            "<title>table acceptance</title><h1>Table acceptance</h1>\
             <p>Caption, headers, mixed 日本語 and ASCII, spans, empty cells, and a link.</p>\
             <table border='1'><caption>Browser table fixture (border=1)</caption>\
             <thead><tr><th>Item</th><th>ASCII / 日本語</th><th>Long value</th></tr></thead>\
             <tbody><tr><td>one</td><td>short 日本語</td><td>This deliberately long cell must wrap inside its column.</td></tr>\
             <tr><th rowspan='2'>rowspan header</th><td colspan='2'>A colspan cell with a <a href='/'>link back home</a>.</td></tr>\
             <tr><td>left after rowspan</td><td>right after rowspan</td></tr>\
             <tr><td></td><td colspan='2' rowspan='2'>Both spans: this text wraps while occupying two columns and two rows.</td></tr>\
             <tr><td>empty-neighbour</td></tr>\
             <tr><td rowspan='0'>zero means one</td><td colspan='oops'>invalid means one</td><td>omitted end tags\
             </tbody></table>\
             <h2>No border, many narrow columns</h2><table border='0'><tr><th>A</th><th>B</th><th>C</th><th>D</th><th>E</th><th>F</th><th>G</th><th>H</th></tr>\
             <tr><td>alpha wraps</td><td>bravo wraps</td><td>日本語</td><td>delta</td><td>echo echo</td><td>foxtrot</td><td>golf</td><td>hotel</td></tr></table>\
             <p><a href='/'>Home</a></p>",
        ),
    };

    pub const FRAGMENTS: &Page = &Page {
        url: "http://built-in/fragments",
        body: Body::FragmentDocument,
    };

    /// The built-in page at `path`, if there is one.
    pub fn by_path(path: &str) -> Option<&'static Page> {
        match path {
            "/" => Some(HOME),
            "/sample" => Some(SAMPLE),
            "/long" => Some(LONG),
            "/wide" => Some(WIDE),
            "/japanese" => Some(JAPANESE),
            "/table" => Some(TABLE),
            "/empty" => Some(EMPTY),
            "/fragments" => Some(FRAGMENTS),
            _ => None,
        }
    }
}
