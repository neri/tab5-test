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
//! Scrolling is a document-space pixel offset. Glyphs and table geometry are
//! clipped to the viewport, so a line may be partly visible at either edge
//! without painting over the toolbar or status band.
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

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::theme::{self, BACKGROUND as WHITE, TEXT as BLACK};
use crate::browser::document::{
    ControlKind, Document, Marker, Parser, STYLE_BOLD, STYLE_CODE, STYLE_ITALIC,
};
use crate::browser::error::{self, Error};
use crate::browser::form;
use crate::browser::image::{DecodeError, DecodedImage, decode, png_chunk_crc};
use crate::browser::layout::{Layout, Line, Metrics};
use crate::browser::limits::{
    MAX_HISTORY, MAX_INPUT_VALUE_BYTES, MAX_RETAINED_POST_REQUEST_BYTES,
    MAX_RETAINED_POST_RESULT_BYTES, MAX_RETAINED_POST_RESULTS, MAX_URL_BYTES,
};
use crate::browser::memory;
use crate::browser::request::{Method as RequestMethod, Request as HttpRequest};
use crate::browser::text_input::{self, Mode as TextInputMode, TextInput};
use crate::browser::url::{self, Url};
use crate::framebuffer::{Framebuffer, HEIGHT, WIDTH};
use crate::input::{InputManager, Key};
use crate::net::pins;
use crate::net::tls::Authentication;

use crate::{tick, uart};

use super::cache_store::{self, CacheRead, CacheWrite, WriteProgress};
use super::fetch::{self, Fetch, Network, Outcome as FetchOutcome};
use super::localfile::{ImageOutcome, ImageRead, LocalRead, Started};
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
const EDIT_SELECTION: u16 = theme::SUBTLE;

/// The two buttons a POST resend question puts at the right of the status
/// line. Keys answer it as well (`y`/Enter, `n`/Escape).
/// The open select list: rows as tall as a text control, at most this many
/// before it scrolls.
const POPUP_ROW_HEIGHT: usize = CELL_HEIGHT + 8;
const POPUP_MAX_ROWS: usize = 10;

const CONFIRM_BUTTON_WIDTH: usize = 112;
const CONFIRM_CANCEL_LEFT: usize = WIDTH - MARGIN - CONFIRM_BUTTON_WIDTH;
const CONFIRM_SEND_LEFT: usize = CONFIRM_CANCEL_LEFT - 8 - CONFIRM_BUTTON_WIDTH;

/// Keyboard and wheel movement in document pixels.
const SCROLL_STEP: i32 = 20;
const WHEEL_STEP: i32 = 60;

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
    local_image: Option<LocalImagePending>,
    network_image: Option<NetworkImagePending>,
    next_image: usize,
    /// The file-backed HTTP cache's work in progress and counters.
    cache: CacheState,
    cache_image: Option<CacheImagePending>,
    /// The page now loading was a forced reload, so its images skip the
    /// cache as well.
    images_bypass_cache: bool,
}

/// The HTTP cache work this screen owns between frames.
struct CacheState {
    /// At most one body on its way to the RAM disk.
    write: Option<CacheWrite>,
    /// The bucket the expiry sweep does next, while one is running.
    sweep_bucket: Option<u8>,
    next_sweep_ms: u64,
    /// Entries a POST made stale, removed at the next maintenance step.
    invalidations: Vec<Url>,
    stats: cache_store::Stats,
}

impl CacheState {
    fn new() -> Self {
        Self {
            write: None,
            sweep_bucket: None,
            next_sweep_ms: 0,
            invalidations: Vec::new(),
            stats: cache_store::Stats::default(),
        }
    }
}

/// How often expired cache entries are looked for while the Browser is idle.
const CACHE_SWEEP_INTERVAL_MS: u64 = 60_000;

struct CacheImagePending {
    image: usize,
    read: CacheRead,
}

struct LocalImagePending {
    image: usize,
    read: ImageRead,
}
struct NetworkImagePending {
    image: usize,
    fetch: Fetch,
}

fn decode_failure(error: DecodeError) -> &'static str {
    match error {
        DecodeError::Unsupported => "unsupported image",
        DecodeError::Malformed => "broken image",
        DecodeError::TooLarge => "image too large",
        DecodeError::OutOfMemory => "image out of memory",
    }
}

fn shared_decoded_image(page: &Page, image: usize, url: &Url) -> Option<Rc<DecodedImage>> {
    (0..image).find_map(|candidate| {
        let same = page.document.images()[candidate]
            .source
            .as_ref()
            .is_some_and(|source| source == url);
        same.then(|| page.decoded_images[candidate].clone())
            .flatten()
    })
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
            local_image: None,
            network_image: None,
            next_image: 0,
            cache: CacheState::new(),
            cache_image: None,
            images_bypass_cache: false,
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
            Action::Report => {
                report_state(&mut self.viewer, &self.pending, wifi, &self.cache.stats)
            }
            Action::Leave => return true,
        }
        false
    }
    pub fn wheel(&mut self, amount: i32) {
        self.viewer.close_select();
        self.viewer
            .scroll_by_pixels(-amount.saturating_mul(WHEEL_STEP));
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
            if active.is_post() {
                end_post(active, &mut self.viewer, wifi, vfs, PostEnd::Interrupted);
            } else {
                let navigation = Navigation {
                    url: active.navigation.url.clone(),
                    restore: active.navigation.restore,
                    how: active.navigation.how,
                    bypass_cache: active.navigation.bypass_cache,
                };
                close_pending(active, wifi, vfs);
                self.pending = Some(Pending {
                    navigation,
                    source: Source::WaitingForNetwork,
                    post: None,
                });
            }
        }
        if let Some(active) = self.network_image.take() {
            self.next_image = self.next_image.min(active.image);
            if let Some(mut link) = raw_network(wifi) {
                active.fetch.close(&mut link);
            }
        }
    }
    pub fn close(&mut self, wifi: &mut WifiManager, vfs: &mut Vfs) {
        if let Some(active) = self.pending.take() {
            close_pending(active, wifi, vfs);
        }
        if let Some(active) = self.local_image.take() {
            active.read.close(vfs);
        }
        if let Some(active) = self.network_image.take() {
            if let Some(mut link) = raw_network(wifi) {
                active.fetch.close(&mut link);
            }
        }
        if let Some(active) = self.cache_image.take() {
            active.read.close(vfs);
        }
        if let Some(write) = self.cache.write.take() {
            write.abandon(vfs);
        }
    }
    pub fn draw(&mut self, fb: &mut Framebuffer, wifi: &mut WifiManager, full: bool) -> bool {
        if full {
            self.viewer.draw_all(fb, &mut || service_link(wifi))
        } else {
            self.viewer.draw_dirty(fb, &mut || service_link(wifi))
        }
    }
    /// Whether a text field owns plain character keys: the address field,
    /// or a text input or textarea, which starts editing as soon as it is
    /// focused.
    pub fn editing(&self) -> bool {
        self.viewer.editing.is_some() || self.viewer.form_editing.is_some()
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
        if self.network_image.is_some() && !network_is_addressed(wifi) {
            if let Some(active) = self.network_image.take() {
                self.next_image = self.next_image.min(active.image);
                if let Some(mut link) = raw_network(wifi) {
                    active.fetch.close(&mut link);
                }
            }
        }
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
        if let Some(requested) = self.viewer.take_request() {
            self.images_bypass_cache = matches!(
                &requested,
                Requested::Navigation(navigation) if navigation.bypass_cache
            );
            if let Some(mut active) = self.pending.take() {
                // An abandoned history resend returns its entry before the
                // new navigation settles the stacks.
                if let Some(post) = active.post.take() {
                    self.viewer
                        .put_back_entry(post.entry, active.navigation.how);
                }
                close_pending(active, wifi, vfs);
            }
            if let Some(active) = self.local_image.take() {
                active.read.close(vfs);
            }
            if let Some(active) = self.network_image.take() {
                if let Some(mut link) = raw_network(wifi) {
                    active.fetch.close(&mut link);
                }
            }
            if let Some(active) = self.cache_image.take() {
                active.read.close(vfs);
            }
            // A body still being written is finished before the next page
            // can capture another, so at most one is ever held in memory.
            self.finish_cache_write(vfs, ram_disk.as_deref_mut(), input);
            self.next_image = 0;
            self.pending = match requested {
                Requested::Navigation(navigation) => begin(
                    &mut self.viewer,
                    navigation,
                    wifi,
                    vfs,
                    ram_disk.as_deref_mut(),
                    input,
                    &mut self.cache.stats,
                ),
                Requested::Submission(submission) => {
                    begin_submission(&mut self.viewer, submission, wifi)
                }
                Requested::Restore(entry, how) => {
                    self.viewer.restore_post(entry, how);
                    None
                }
            };
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
                    &mut self.cache.stats,
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
                    Source::Cache(read) => {
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
                if let Source::Network(fetch) = &mut active.source {
                    for url in fetch.take_invalidations() {
                        if self.cache.invalidations.try_reserve(1).is_ok() {
                            self.cache.invalidations.push(url);
                        }
                    }
                }
                outcome
            }
            None => None,
        };
        match outcome {
            None | Some(FetchOutcome::Working) => {}
            Some(FetchOutcome::Image(_)) => unreachable!("page fetch returned image mode"),
            Some(FetchOutcome::Page(document)) => {
                if let Some(mut active) = self.pending.take() {
                    // The final address, which is the last hop of a
                    // redirect chain rather than the one that was asked
                    // for -- so the toolbar and the base for this page's
                    // links are both where the page actually came from.
                    let landed = active.landed();
                    let security = active.security();
                    let status = active.status();
                    let peak = active.peak_owned();
                    let request_method = active.method();
                    let post = active.landed_post();
                    let from_cache = match &mut active.source {
                        Source::Network(fetch) => {
                            let mut sd = SdSlot::new();
                            let mut devices = Devices {
                                ram: ram_disk.as_deref_mut(),
                                sd: &mut sd,
                                usb: input.usb_host_mut(),
                            };
                            note_fetch_cache(fetch, &mut self.cache, vfs, &mut devices);
                            None
                        }
                        Source::Cache(read) => Some(read.revalidated()),
                        _ => None,
                    };
                    let navigation = Navigation {
                        url: landed.clone(),
                        restore: active.navigation.restore,
                        how: active.navigation.how,
                        bypass_cache: active.navigation.bypass_cache,
                    };
                    close_pending(active, wifi, vfs);
                    self.viewer.show_document(
                        document,
                        &navigation,
                        landed,
                        security,
                        status,
                        peak,
                        request_method,
                        post,
                    );
                    match from_cache {
                        Some(true) => self
                            .viewer
                            .say("not modified (304): the cached copy is shown"),
                        Some(false) => self.viewer.say("shown from the cache; no request was sent"),
                        None => {}
                    }
                    self.next_image = 0;
                }
            }
            Some(FetchOutcome::NotModified) => {
                if let Some(active) = self.pending.take() {
                    let Pending {
                        source, navigation, ..
                    } = active;
                    let refresh = match &source {
                        Source::Network(fetch) => fetch.refresh(),
                        _ => None,
                    };
                    close_source(source, wifi, vfs);
                    let now = tick::now_ms();
                    let mut sd = SdSlot::new();
                    let mut devices = Devices {
                        ram: ram_disk.as_deref_mut(),
                        sd: &mut sd,
                        usb: input.usb_host_mut(),
                    };
                    let read = match cache_store::find(vfs, &mut devices, &navigation.url) {
                        Some(mut hit) => {
                            cache_store::refresh(&mut hit.record, refresh, now);
                            let _ = cache_store::update_record(vfs, &mut devices, &hit);
                            let read = CacheRead::start(
                                vfs,
                                &mut devices,
                                &navigation.url,
                                &hit,
                                false,
                                true,
                            );
                            if read.is_none() {
                                cache_store::remove_hit(vfs, &mut devices, &hit);
                            }
                            read
                        }
                        None => None,
                    };
                    match read {
                        Some(read) => {
                            self.cache.stats.revalidated += 1;
                            self.pending = Some(Pending {
                                source: Source::Cache(read),
                                navigation,
                                post: None,
                            });
                        }
                        // The copy the server confirmed is gone or cannot be
                        // read: fetch the page whole instead.
                        None => self.viewer.request(Navigation {
                            bypass_cache: true,
                            ..navigation
                        }),
                    }
                }
            }
            Some(FetchOutcome::Failed(failure)) => {
                if let Some(active) = self.pending.take() {
                    // A transfer can be the operation that makes a dead C6
                    // link observable. Let the manager consume that evidence
                    // before deciding whether this is a page failure or an
                    // automatically recoverable Wi-Fi interruption.
                    service_link(wifi);
                    if matches!(active.source, Source::Cache(_)) {
                        // An unreadable cache file is not the page failing:
                        // the entry goes and the page is fetched whole.
                        let Pending {
                            source, navigation, ..
                        } = active;
                        close_source(source, wifi, vfs);
                        let mut sd = SdSlot::new();
                        let mut devices = Devices {
                            ram: ram_disk.as_deref_mut(),
                            sd: &mut sd,
                            usb: input.usb_host_mut(),
                        };
                        cache_store::remove(vfs, &mut devices, &navigation.url);
                        self.cache.stats.purged += 1;
                        self.viewer.request(Navigation {
                            bypass_cache: true,
                            ..navigation
                        });
                    } else if active.is_network_transfer()
                        && !network_is_addressed(wifi)
                        && network_is_recovering(wifi)
                    {
                        suspend_or_fail(active, &mut self.viewer, &mut self.pending, wifi, vfs);
                    } else if active.is_post() {
                        end_post(
                            active,
                            &mut self.viewer,
                            wifi,
                            vfs,
                            PostEnd::Failed(failure),
                        );
                    } else {
                        let url = active.landed();
                        let how = active.navigation.how;
                        close_pending(active, wifi, vfs);
                        self.viewer.show_failure(&url, failure, how);
                    }
                }
            }
        }
        if self.pending.is_none() {
            self.step_local_images(vfs, ram_disk.as_deref_mut(), input);
            self.step_network_images(wifi, vfs, ram_disk.as_deref_mut(), input);
        }
        self.step_cache_maintenance(vfs, ram_disk.as_deref_mut(), input);
    }

    /// Advances the cache's own work: the body being written, and otherwise,
    /// while nothing else is loading, the periodic expiry sweep one bucket
    /// at a time.
    fn step_cache_maintenance(
        &mut self,
        vfs: &mut Vfs,
        ram_disk: Option<&mut RamBlockDevice>,
        input: &mut InputManager,
    ) {
        let now = tick::now_ms();
        let mut sd = SdSlot::new();
        let mut devices = Devices {
            ram: ram_disk,
            sd: &mut sd,
            usb: input.usb_host_mut(),
        };
        // What a POST changed goes first, including a body of it still
        // being written, so no later lookup can find the old response.
        for url in core::mem::take(&mut self.cache.invalidations) {
            if self
                .cache
                .write
                .as_ref()
                .is_some_and(|write| write.is_for(&url))
                && let Some(write) = self.cache.write.take()
            {
                write.abandon(vfs);
            }
            if cache_store::remove(vfs, &mut devices, &url) {
                self.cache.stats.invalidated += 1;
            }
        }
        if let Some(write) = self.cache.write.as_mut() {
            match write.step(vfs, &mut devices, now, &mut self.cache.stats) {
                WriteProgress::Working => {}
                WriteProgress::Stored | WriteProgress::NotKept => self.cache.write = None,
            }
            return;
        }
        if self.pending.is_some() || self.network_image.is_some() || self.cache_image.is_some() {
            return;
        }
        match self.cache.sweep_bucket {
            None if now >= self.cache.next_sweep_ms => self.cache.sweep_bucket = Some(0),
            None => {}
            Some(bucket) => {
                self.cache.stats.purged +=
                    cache_store::sweep_bucket(vfs, &mut devices, bucket, now, None);
                self.cache.sweep_bucket =
                    (bucket + 1 < crate::browser::cache::BUCKET_COUNT).then_some(bucket + 1);
                if self.cache.sweep_bucket.is_none() {
                    self.cache.next_sweep_ms = now.saturating_add(CACHE_SWEEP_INTERVAL_MS);
                }
            }
        }
    }

    /// Runs a pending cache write to its end in one call. Bounded: a whole
    /// entry is a handful of steps, and each purge retry removes something.
    fn finish_cache_write(
        &mut self,
        vfs: &mut Vfs,
        ram_disk: Option<&mut RamBlockDevice>,
        input: &mut InputManager,
    ) {
        let Some(mut write) = self.cache.write.take() else {
            return;
        };
        let mut sd = SdSlot::new();
        let mut devices = Devices {
            ram: ram_disk,
            sd: &mut sd,
            usb: input.usb_host_mut(),
        };
        for _ in 0..256 {
            match write.step(vfs, &mut devices, tick::now_ms(), &mut self.cache.stats) {
                WriteProgress::Working => {}
                WriteProgress::Stored | WriteProgress::NotKept => return,
            }
        }
        write.abandon(vfs);
    }

    /// Steps an image being read from the cache. True while one is.
    fn step_cache_image(&mut self, vfs: &mut Vfs, devices: &mut Devices) -> bool {
        let Some(active) = self.cache_image.as_mut() else {
            return false;
        };
        let outcome = active.read.step(vfs, devices);
        if matches!(outcome, FetchOutcome::Working) {
            return true;
        }
        let Some(active) = self.cache_image.take() else {
            return false;
        };
        active.read.close(vfs);
        match outcome {
            FetchOutcome::Image(bytes) => {
                match decode(&bytes) {
                    Ok(decoded) => {
                        let _ = self
                            .viewer
                            .install_decoded_image(active.image, Rc::new(decoded));
                    }
                    Err(error) => {
                        self.viewer.page.image_failures[active.image] = Some(decode_failure(error));
                    }
                }
                self.viewer.dirty.viewport = true;
            }
            _ => {
                // An unreadable entry goes, and the image is fetched whole.
                if let Some(url) = self
                    .viewer
                    .page
                    .document
                    .images()
                    .get(active.image)
                    .and_then(|image| image.source.clone())
                {
                    cache_store::remove(vfs, devices, &url);
                    self.cache.stats.purged += 1;
                }
                self.next_image = self.next_image.min(active.image);
            }
        }
        false
    }

    fn step_local_images(
        &mut self,
        vfs: &mut Vfs,
        ram_disk: Option<&mut RamBlockDevice>,
        input: &mut InputManager,
    ) {
        let mut sd = SdSlot::new();
        let mut devices = Devices {
            ram: ram_disk,
            sd: &mut sd,
            usb: input.usb_host_mut(),
        };
        if self.local_image.is_none() {
            while self.next_image < self.viewer.page.document.images().len() {
                let image = self.next_image;
                self.next_image += 1;
                let Some(url) = self.viewer.page.document.images()[image].source.clone() else {
                    continue;
                };
                if let Some(shared) = shared_decoded_image(&self.viewer.page, image, &url) {
                    let _ = self.viewer.install_decoded_image(image, shared);
                    continue;
                }
                if url.scheme() != crate::browser::url::Scheme::File {
                    self.next_image -= 1;
                    break;
                }
                if self.viewer.page.visit_url.scheme() != crate::browser::url::Scheme::File {
                    self.viewer.page.image_failures[image] = Some("local image refused");
                    self.viewer.dirty.viewport = true;
                    self.viewer.say("a network page cannot open a local image");
                    continue;
                }
                match ImageRead::start(&url, vfs, &mut devices) {
                    Ok(read) => {
                        self.local_image = Some(LocalImagePending { image, read });
                        break;
                    }
                    Err(_) => {
                        self.viewer.page.image_failures[image] = Some("image open failed");
                        self.viewer.dirty.viewport = true;
                        continue;
                    }
                }
            }
        }
        let outcome = self
            .local_image
            .as_mut()
            .map(|active| active.read.step(vfs, &mut devices));
        match outcome {
            Some(ImageOutcome::Working) | None => {}
            Some(ImageOutcome::Complete(bytes)) => {
                if let Some(active) = self.local_image.take() {
                    active.read.close(vfs);
                    match decode(&bytes) {
                        Ok(decoded) => {
                            let _ = self
                                .viewer
                                .install_decoded_image(active.image, Rc::new(decoded));
                        }
                        Err(error) => {
                            self.viewer.page.image_failures[active.image] =
                                Some(decode_failure(error));
                        }
                    }
                    self.viewer.dirty.viewport = true;
                }
            }
            Some(ImageOutcome::Failed(failure)) => {
                if let Some(active) = self.local_image.take() {
                    self.viewer.page.image_failures[active.image] = Some(failure.headline);
                    self.viewer.dirty.viewport = true;
                    active.read.close(vfs);
                }
                self.viewer.say(failure.detail);
            }
        }
    }

    fn step_network_images(
        &mut self,
        wifi: &mut WifiManager,
        vfs: &mut Vfs,
        ram_disk: Option<&mut RamBlockDevice>,
        input: &mut InputManager,
    ) {
        if self.local_image.is_some() {
            return;
        }
        let mut sd = SdSlot::new();
        let mut devices = Devices {
            ram: ram_disk,
            sd: &mut sd,
            usb: input.usb_host_mut(),
        };
        if self.step_cache_image(vfs, &mut devices) {
            return;
        }
        // A body still on its way to the RAM disk holds memory; the next
        // image waits for it rather than capturing a second one.
        if self.network_image.is_none() && self.cache.write.is_some() {
            return;
        }
        if self.network_image.is_none() {
            while self.next_image < self.viewer.page.document.images().len() {
                let image = self.next_image;
                let Some(url) = self.viewer.page.document.images()[image].source.clone() else {
                    self.next_image += 1;
                    continue;
                };
                if let Some(shared) = shared_decoded_image(&self.viewer.page, image, &url) {
                    let _ = self.viewer.install_decoded_image(image, shared);
                    self.next_image += 1;
                    continue;
                }
                if !url.scheme().is_network() {
                    return;
                }
                if self.viewer.page.visit_url.scheme() == crate::browser::url::Scheme::Https
                    && url.scheme() == crate::browser::url::Scheme::Http
                {
                    self.viewer.page.image_failures[image] = Some("HTTPS downgrade refused");
                    self.viewer.dirty.viewport = true;
                    self.viewer.say("refused an HTTPS image downgrade");
                    self.next_image += 1;
                    continue;
                }
                if self.viewer.page.security
                    == Some(fetch::PageSecurity::Tls(Authentication::Pinned))
                    && !(url.scheme() == crate::browser::url::Scheme::Https
                        && pins::is_pinned(url.host()))
                {
                    self.viewer.page.image_failures[image] = Some("TLS identity downgrade refused");
                    self.viewer.dirty.viewport = true;
                    self.next_image += 1;
                    continue;
                }
                let now = tick::now_ms();
                let hit = if self.images_bypass_cache {
                    None
                } else {
                    cache_store::lookup(vfs, &mut devices, &url, now, &mut self.cache.stats)
                };
                let mut validator = None;
                if let Some(mut hit) = hit {
                    if hit.record.reusable_without_request(now) {
                        if let Some(read) =
                            CacheRead::start(vfs, &mut devices, &url, &hit, true, false)
                        {
                            hit.record.used_ms = now;
                            let _ = cache_store::update_record(vfs, &mut devices, &hit);
                            self.cache.stats.hits += 1;
                            self.next_image += 1;
                            self.cache_image = Some(CacheImagePending { image, read });
                            break;
                        }
                        cache_store::remove_hit(vfs, &mut devices, &hit);
                        self.cache.stats.purged += 1;
                    } else if !hit.record.etag.is_empty() {
                        validator = memory::string_from(&hit.record.etag).ok();
                    }
                }
                let Some(mut network) = addressed_network(wifi) else {
                    return;
                };
                self.next_image += 1;
                match Fetch::start_image_cached(url, &mut network, validator) {
                    Ok(fetch) => {
                        self.network_image = Some(NetworkImagePending { image, fetch });
                        break;
                    }
                    Err(failure) => {
                        self.viewer.page.image_failures[image] = Some(failure.headline);
                        self.viewer.dirty.viewport = true;
                        self.viewer.say(failure.detail);
                    }
                }
            }
        }
        let outcome = {
            let Some(active) = self.network_image.as_mut() else {
                return;
            };
            let Some(mut network) = addressed_network(wifi) else {
                return;
            };
            active.fetch.step(&mut network)
        };
        match outcome {
            FetchOutcome::Working => {}
            FetchOutcome::Image(bytes) => {
                if let Some(mut active) = self.network_image.take() {
                    note_fetch_cache(&mut active.fetch, &mut self.cache, vfs, &mut devices);
                    if let Some(mut network) = raw_network(wifi) {
                        active.fetch.close(&mut network);
                    }
                    match decode(&bytes) {
                        Ok(decoded) => {
                            let _ = self
                                .viewer
                                .install_decoded_image(active.image, Rc::new(decoded));
                        }
                        Err(error) => {
                            self.viewer.page.image_failures[active.image] =
                                Some(decode_failure(error));
                        }
                    }
                    self.viewer.dirty.viewport = true;
                }
            }
            FetchOutcome::NotModified => {
                if let Some(active) = self.network_image.take() {
                    let refresh = active.fetch.refresh();
                    let url = active.fetch.url().clone();
                    if let Some(mut network) = raw_network(wifi) {
                        active.fetch.close(&mut network);
                    }
                    let now = tick::now_ms();
                    match cache_store::find(vfs, &mut devices, &url) {
                        Some(mut hit) => {
                            cache_store::refresh(&mut hit.record, refresh, now);
                            let _ = cache_store::update_record(vfs, &mut devices, &hit);
                            match CacheRead::start(vfs, &mut devices, &url, &hit, true, true) {
                                Some(read) => {
                                    self.cache.stats.revalidated += 1;
                                    self.cache_image = Some(CacheImagePending {
                                        image: active.image,
                                        read,
                                    });
                                }
                                None => {
                                    cache_store::remove_hit(vfs, &mut devices, &hit);
                                    self.next_image = self.next_image.min(active.image);
                                }
                            }
                        }
                        None => self.next_image = self.next_image.min(active.image),
                    }
                }
            }
            FetchOutcome::Failed(failure) => {
                if let Some(active) = self.network_image.take() {
                    self.viewer.page.image_failures[active.image] = Some(failure.headline);
                    self.viewer.dirty.viewport = true;
                    if let Some(mut network) = raw_network(wifi) {
                        active.fetch.close(&mut network);
                    }
                }
                self.viewer.say(failure.detail);
            }
            FetchOutcome::Page(_) => unreachable!("image fetch returned document mode"),
        }
    }
}

/// Tells the cache what a finished network fetch learned: a body to write,
/// or an entry that is no longer the current response.
fn note_fetch_cache(
    fetch: &mut Fetch,
    cache: &mut CacheState,
    vfs: &mut Vfs,
    devices: &mut Devices,
) {
    match fetch.take_cache_update() {
        fetch::CacheUpdate::Store(meta, body) => {
            match CacheWrite::new(fetch.url(), meta, body, fetch.security(), tick::now_ms()) {
                Some(write) => {
                    if let Some(previous) = cache.write.replace(write) {
                        previous.abandon(vfs);
                    }
                }
                None => cache.stats.not_kept += 1,
            }
        }
        fetch::CacheUpdate::Remove => {
            cache_store::remove(vfs, devices, fetch.url());
        }
        fetch::CacheUpdate::Keep => {}
    }
}

/// Gives back whatever the pending read owns: a socket, or a file handle.
///
/// One function for both so that every site that abandons a read returns
/// the right thing without having to know which kind it had.
fn close_pending(pending: Pending, wifi: &mut WifiManager, vfs: &mut Vfs) {
    close_source(pending.source, wifi, vfs);
}

fn close_source(source: Source, wifi: &mut WifiManager, vfs: &mut Vfs) {
    match source {
        Source::Network(fetch) => {
            if let Some(mut link) = raw_network(wifi) {
                fetch.close(&mut link);
            }
        }
        Source::Local(read) => read.close(vfs),
        Source::Cache(read) => read.close(vfs),
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
    if pending.is_post() {
        end_post(pending, viewer, wifi, vfs, PostEnd::Interrupted);
        return;
    }
    let Pending {
        source, navigation, ..
    } = pending;
    close_source(source, wifi, vfs);
    if network_is_recovering(wifi) {
        viewer.wait_for_network(&navigation.url);
        *slot = Some(Pending {
            source: Source::WaitingForNetwork,
            navigation,
            post: None,
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
fn report_state(
    viewer: &mut Viewer,
    pending: &Option<Pending>,
    wifi: &mut WifiManager,
    cache: &cache_store::Stats,
) {
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
    let (results, result_bytes, request_bytes) = viewer.retained_post();
    line.push(" post ");
    line.push_usize(results);
    line.push("/");
    line.push_usize(result_bytes / 1024);
    line.push("K+");
    line.push_usize(request_bytes / 1024);
    line.push("K cache h");
    line.push_usize(cache.hits);
    line.push(" r");
    line.push_usize(cache.revalidated);
    line.push(" s");
    line.push_usize(cache.stored);
    line.push(" p");
    line.push_usize(cache.purged);
    line.push(" x");
    line.push_usize(cache.invalidated);
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
    if active.is_post() {
        end_post(active, viewer, wifi, vfs, PostEnd::Stopped);
        return;
    }
    close_pending(active, wifi, vfs);
    viewer.finish_loading();
    viewer.say("stopped");
}

/// How a POST that did not land came to an end.
enum PostEnd {
    /// The reader stopped it. Nothing is offered: they chose this.
    Stopped,
    /// Wi-Fi loss, a system bar suspension, or a transport failure the
    /// manager is recovering from.
    Interrupted,
    Failed(fetch::Failure),
}

/// Ends a POST without resending it.
///
/// Whatever happened, the request is never sent again by itself. When a
/// resend could be what the reader wants -- the result is unknown, nothing
/// was sent, or a redirect asks for the body to go to another origin -- the
/// copy is offered as a question, and only an explicit yes sends it. A
/// history entry taken for a resend goes back onto its stack otherwise.
fn end_post(
    mut pending: Pending,
    viewer: &mut Viewer,
    wifi: &mut WifiManager,
    vfs: &mut Vfs,
    end: PostEnd,
) {
    let started = pending.request_started();
    let uncertain = started && !pending.response_started();
    let redirect = match (&end, &mut pending.source) {
        (PostEnd::Failed(failure), Source::Network(fetch))
            if failure.name == fetch::POST_REDIRECT_CONFIRMATION.name =>
        {
            Some((fetch.take_redirect_request(), fetch.redirects() + 1))
        }
        _ => None,
    };
    let copy = match &pending.source {
        Source::Network(fetch) if redirect.is_none() => fetch.request().try_clone().ok(),
        _ => None,
    };
    let Pending {
        source,
        navigation,
        post,
    } = pending;
    close_source(source, wifi, vfs);
    viewer.finish_loading();
    let (origin, entry) = post.map_or((None, None), |post| (post.source, post.entry));

    let lost = if uncertain {
        "POST interrupted; result unknown and not resent"
    } else if started {
        "POST response interrupted; not resent"
    } else {
        "POST was not sent"
    };
    let (offer, message) = match end {
        PostEnd::Stopped => (
            None,
            if uncertain {
                "POST stopped; result unknown and not resent"
            } else if started {
                "POST response stopped; not resent"
            } else {
                "POST stopped before sending"
            },
        ),
        PostEnd::Failed(_) if redirect.is_some() => match redirect {
            Some((Some(request), redirects)) => {
                (Some((request, redirects, ConfirmReason::Redirect)), "")
            }
            _ => (None, "POST redirect to another origin was not followed"),
        },
        // The server answered, and its answer could not be shown. A resend
        // offer would suggest the first one did not arrive.
        PostEnd::Failed(failure) if started && !uncertain => (None, failure.detail),
        PostEnd::Failed(_) | PostEnd::Interrupted => {
            let reason = if started {
                ConfirmReason::Unknown
            } else {
                ConfirmReason::NotSent
            };
            (copy.map(|request| (request, 0, reason)), lost)
        }
    };
    match offer {
        Some((request, redirects, reason)) => viewer.ask(
            Submission {
                request,
                how: navigation.how,
                restore: navigation.restore,
                source: origin,
                entry,
                redirects,
            },
            reason,
        ),
        None => {
            viewer.put_back_entry(entry, navigation.how);
            viewer.say(message);
        }
    }
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
    cache: &mut cache_store::Stats,
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
    // A fresh entry is shown without asking anybody, so it works without a
    // network as well. A reload always asks; a forced one does not look.
    let now = tick::now_ms();
    let mut sd = SdSlot::new();
    let mut devices = Devices {
        ram: ram_disk,
        sd: &mut sd,
        usb: input.usb_host_mut(),
    };
    let hit = if navigation.bypass_cache {
        None
    } else {
        cache_store::lookup(vfs, &mut devices, &navigation.url, now, cache)
    };
    let mut validator = None;
    if let Some(mut hit) = hit {
        if navigation.how != Direction::Reload && hit.record.reusable_without_request(now) {
            if let Some(read) =
                CacheRead::start(vfs, &mut devices, &navigation.url, &hit, false, false)
            {
                hit.record.used_ms = now;
                let _ = cache_store::update_record(vfs, &mut devices, &hit);
                cache.hits += 1;
                viewer.begin_loading(&navigation.url);
                return Some(Pending {
                    source: Source::Cache(read),
                    navigation,
                    post: None,
                });
            }
            cache_store::remove_hit(vfs, &mut devices, &hit);
            cache.purged += 1;
        } else if !hit.record.etag.is_empty() {
            validator = memory::string_from(&hit.record.etag).ok();
        }
    }
    if !network_is_addressed(wifi) && network_is_recovering(wifi) {
        viewer.wait_for_network(&navigation.url);
        return Some(Pending {
            source: Source::WaitingForNetwork,
            navigation,
            post: None,
        });
    }
    let mut network = addressed_network(wifi);
    let Some(network) = network.as_mut() else {
        viewer.show_failure(&navigation.url, fetch::NO_NETWORK, navigation.how);
        return None;
    };
    match Fetch::start_cached(navigation.url.clone(), network, validator) {
        Ok(fetch) => {
            viewer.begin_loading(&navigation.url);
            Some(Pending {
                source: Source::Network(fetch),
                navigation,
                post: None,
            })
        }
        Err(failure) => {
            viewer.finish_loading();
            viewer.say(failure.detail);
            None
        }
    }
}

/// Starts a POST: a form's own submission, or a resend the reader confirmed.
///
/// Failing to start leaves the page and its input values where they are.
fn begin_submission(
    viewer: &mut Viewer,
    submission: Submission,
    wifi: &mut WifiManager,
) -> Option<Pending> {
    let Submission {
        request,
        how,
        restore,
        source,
        entry,
        redirects,
    } = submission;
    let navigation = Navigation {
        url: request.url.clone(),
        restore,
        how,
        bypass_cache: false,
    };
    if !request.url.scheme().is_network() || request.url.host() == builtin::HOST {
        viewer.put_back_entry(entry, how);
        viewer.say("POST needs a network HTTP(S) action");
        return None;
    }
    let mut network = addressed_network(wifi);
    let Some(network) = network.as_mut() else {
        viewer.put_back_entry(entry, how);
        viewer.say("no network: POST was not sent");
        return None;
    };
    match Fetch::start_redirected_request(request, redirects, network) {
        Ok(fetch) => {
            viewer.begin_loading(fetch.url());
            Some(Pending {
                source: Source::Network(fetch),
                navigation,
                post: Some(PostContext { source, entry }),
            })
        }
        Err(failure) => {
            viewer.put_back_entry(entry, how);
            viewer.say(failure.detail);
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
                post: None,
            })
        }
        Ok(Started::Page(document)) => {
            let landed = navigation.url.clone();
            viewer.show_document(
                document,
                &navigation,
                landed,
                None,
                None,
                0,
                RequestMethod::Get,
                None,
            );
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
    /// The document pixel to put at the top once the page is up. Non-zero when going
    /// back or forward, which are re-fetches -- history keeps a scroll
    /// position but never a document -- and when reloading, which is the
    /// same page and should not jump to the top of it.
    restore: u32,
    how: Direction,
    /// Fetch without the cached copy's validator: the forced reload.
    bypass_cache: bool,
}

enum Requested {
    Navigation(Navigation),
    Submission(Submission),
    /// A kept POST result, shown again without any request.
    Restore(HistoryEntry, Direction),
}

/// A POST to send, and what it replaces when it lands.
struct Submission {
    request: HttpRequest,
    how: Direction,
    restore: u32,
    /// The page the form was on: where a POST result that can be neither
    /// shown from memory nor resent falls back to.
    source: Option<Url>,
    /// A history entry taken off its stack for this resend, returned to it
    /// if nothing lands.
    entry: Option<HistoryEntry>,
    /// Redirects already followed, when this continues a chain that paused
    /// for confirmation.
    redirects: usize,
}

/// Why the reader is being asked before a POST is sent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfirmReason {
    /// Reload, back or forward onto a result that was not kept.
    Resend,
    /// Sending started and no response was read.
    Unknown,
    /// Nothing was sent.
    NotSent,
    /// A 307/308 asked for the body to go to another origin.
    Redirect,
}

/// A POST waiting for the reader's yes or no.
struct Confirmation {
    submission: Submission,
    reason: ConfirmReason,
}

/// What a POST in flight carries besides its request.
struct PostContext {
    source: Option<Url>,
    entry: Option<HistoryEntry>,
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
            bypass_cache: false,
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
    /// Set for a POST only.
    post: Option<PostContext>,
}

enum Source {
    Network(Fetch),
    Local(LocalRead),
    /// A body from the HTTP cache: fresh, or just confirmed by a `304`.
    Cache(CacheRead),
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
            Source::Local(_) | Source::Cache(_) => self.navigation.url.clone(),
            Source::WaitingForNetwork => self.navigation.url.clone(),
        }
    }

    /// What the connection proved. Nothing, for a file: nobody was asked.
    fn security(&self) -> Option<fetch::PageSecurity> {
        match &self.source {
            Source::Network(fetch) => fetch.security(),
            Source::Cache(read) => read.security(),
            Source::Local(_) | Source::WaitingForNetwork => None,
        }
    }

    fn status(&self) -> Option<u16> {
        match &self.source {
            Source::Network(fetch) => fetch.status(),
            Source::Local(_) | Source::Cache(_) | Source::WaitingForNetwork => None,
        }
    }

    fn received(&self) -> usize {
        match &self.source {
            Source::Network(fetch) => fetch.received(),
            Source::Local(read) => read.received(),
            Source::Cache(read) => read.received(),
            Source::WaitingForNetwork => 0,
        }
    }

    fn peak_owned(&self) -> usize {
        match &self.source {
            Source::Network(fetch) => fetch.peak_owned(),
            Source::Local(read) => read.peak_owned(),
            Source::Cache(read) => read.peak_owned(),
            Source::WaitingForNetwork => 0,
        }
    }

    fn method(&self) -> RequestMethod {
        match &self.source {
            Source::Network(fetch) => fetch.method(),
            Source::Local(_) | Source::Cache(_) | Source::WaitingForNetwork => RequestMethod::Get,
        }
    }

    fn is_network_transfer(&self) -> bool {
        matches!(self.source, Source::Network(_))
    }

    fn is_waiting_for_network(&self) -> bool {
        matches!(self.source, Source::WaitingForNetwork)
    }

    /// What a landed POST result needs to be kept or resent later.
    ///
    /// `None` for a GET, including a POST that a 301/302/303 turned into one:
    /// that result is an ordinary page and history re-fetches it.
    fn landed_post(&mut self) -> Option<PagePost> {
        let Source::Network(fetch) = &self.source else {
            return None;
        };
        if fetch.method() != RequestMethod::Post {
            return None;
        }
        let context = self.post.take();
        Some(PagePost {
            request: fetch.request().try_clone().ok(),
            source: context.and_then(|context| context.source),
            keep: !fetch.no_store(),
        })
    }

    fn is_post(&self) -> bool {
        matches!(
            &self.source,
            Source::Network(fetch) if fetch.method() == RequestMethod::Post
        )
    }

    fn request_started(&self) -> bool {
        matches!(&self.source, Source::Network(fetch) if fetch.request_started())
    }

    fn response_started(&self) -> bool {
        matches!(&self.source, Source::Network(fetch) if fetch.response_started())
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
struct DamageRect {
    x: usize,
    width: usize,
}

#[derive(Clone, Copy)]
struct ViewportDamage {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

#[derive(Default, Clone, Copy)]
struct AddressDamage {
    rects: [DamageRect; 4],
}

#[derive(Clone, Copy)]
struct AddressVisual {
    first: usize,
    caret_x: usize,
    selection: Option<(usize, usize)>,
}

impl AddressDamage {
    fn add(&mut self, x: usize, width: usize) {
        let left = x.max(ADDRESS_LEFT.saturating_sub(4));
        let right = x.saturating_add(width).min(ADDRESS_RIGHT.saturating_add(4));
        if left >= right {
            return;
        }
        for rect in &mut self.rects {
            if rect.width == 0 {
                *rect = DamageRect {
                    x: left,
                    width: right - left,
                };
                return;
            }
            let rect_right = rect.x + rect.width;
            if left <= rect_right && right >= rect.x {
                let merged_left = left.min(rect.x);
                let merged_right = right.max(rect_right);
                rect.x = merged_left;
                rect.width = merged_right - merged_left;
                return;
            }
        }
        self.rects[0] = DamageRect {
            x: ADDRESS_LEFT.saturating_sub(4),
            width: ADDRESS_RIGHT - ADDRESS_LEFT + 8,
        };
        for rect in &mut self.rects[1..] {
            *rect = DamageRect::default();
        }
    }

    fn full(&mut self) {
        self.rects = [DamageRect::default(); 4];
        self.rects[0] = DamageRect {
            x: ADDRESS_LEFT.saturating_sub(4),
            width: ADDRESS_RIGHT - ADDRESS_LEFT + 8,
        };
    }

    fn is_dirty(self) -> bool {
        self.rects[0].width != 0
    }
}

#[derive(Default, Clone, Copy)]
struct Dirty {
    toolbar: bool,
    address: AddressDamage,
    focus: [Option<FocusTarget>; 4],
    control: ControlDamage,
    /// The open select list's rectangle, old and new together, repainted
    /// through the viewport renderer like a focus change.
    popup: Option<ViewportDamage>,
    viewport: bool,
    status: bool,
}

impl Dirty {
    fn full() -> Self {
        Dirty {
            toolbar: true,
            address: AddressDamage::default(),
            focus: [None; 4],
            control: ControlDamage::default(),
            popup: None,
            viewport: true,
            status: true,
        }
    }

    fn focus_change(&mut self, before: Option<FocusTarget>, after: Option<FocusTarget>) {
        for target in [before, after].into_iter().flatten() {
            if self.focus.contains(&Some(target)) {
                continue;
            }
            if let Some(slot) = self.focus.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(target);
            } else {
                // Several input events can arrive before the next frame.
                // Once every bounded damage slot is occupied, a full
                // viewport repaint is the only way to guarantee that no
                // earlier focus highlight survives.
                self.focus = [None; 4];
                self.viewport = true;
                return;
            }
        }
    }
}

#[derive(Default, Clone, Copy)]
struct ControlDamage {
    control: Option<u16>,
    rects: [DamageRect; 4],
}

impl ControlDamage {
    fn add(&mut self, control: u16, left: usize, right: usize, x: usize, width: usize) {
        if self.control != Some(control) {
            self.control = Some(control);
            self.rects = [DamageRect::default(); 4];
        }
        let left = x.max(left);
        let right = x.saturating_add(width).min(right);
        if left >= right {
            return;
        }
        for rect in &mut self.rects {
            if rect.width == 0 {
                *rect = DamageRect {
                    x: left,
                    width: right - left,
                };
                return;
            }
            let rect_right = rect.x + rect.width;
            if left <= rect_right && right >= rect.x {
                let merged_left = left.min(rect.x);
                let merged_right = right.max(rect_right);
                rect.x = merged_left;
                rect.width = merged_right - merged_left;
                return;
            }
        }
        self.rects = [DamageRect::default(); 4];
        self.rects[0] = DamageRect {
            x: left,
            width: right - left,
        };
    }

    fn is_dirty(self) -> bool {
        self.control.is_some() && self.rects[0].width != 0
    }
}

/// One displayed page. Replaced wholesale on every navigation, which is
/// what drops the previous document, its layout and its links together.
struct Page {
    document: Document,
    visit_url: Url,
    request_method: RequestMethod,
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
    /// Document-space pixel at the top of the viewport.
    scroll_y: u32,
    /// Position within `order`, not a link or control index.
    focus: Option<usize>,
    /// Enabled interactive objects in visual document order.
    order: Vec<FocusTarget>,
    /// Decode results by Document image ID. Duplicate source sharing is
    /// introduced with the fetch job; this stable index keeps drawing and
    /// layout independent of how bytes arrived.
    decoded_images: Vec<Option<Rc<DecodedImage>>>,
    /// Stable, image-local failure labels. A failed image keeps its layout
    /// box and never turns the whole document into an error page.
    image_failures: Vec<Option<&'static str>>,
    control_values: Vec<String>,
    /// Checkedness of checkboxes and radio buttons, by control ID.
    control_checked: Vec<bool>,
    /// Selectedness of select options, by option ID.
    option_selected: Vec<bool>,
    /// Set when this page is the result of a POST.
    post: Option<PagePost>,
}

/// What a POST result page knows about the request that produced it.
struct PagePost {
    /// A copy for an explicit resend, or `None` if it could not be made.
    request: Option<HttpRequest>,
    source: Option<Url>,
    /// False when the response said `Cache-Control: no-store`.
    keep: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusTarget {
    Link(u16),
    Control(u16),
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
    scroll_y: u32,
    method: RequestMethod,
    /// Set for a POST result only. Going back to one never re-fetches it as
    /// a GET: it is shown from `result`, resent after confirmation from
    /// `request`, or left for `source`, in that order.
    post: Option<PostEntry>,
}

struct PostEntry {
    request: Option<HttpRequest>,
    source: Option<Url>,
    result: Option<RetainedResult>,
}

/// A POST result's document, kept within `MAX_RETAINED_POST_RESULT_BYTES`.
/// Its layout and images are rebuilt when it is shown again.
struct RetainedResult {
    document: Document,
    security: Option<fetch::PageSecurity>,
    bytes: usize,
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
type Editing = TextInput;

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
    form_editing: Option<FormEditing>,
    /// A POST resend question, answered from the status line.
    confirm: Option<Confirmation>,
    select_popup: Option<SelectPopup>,
    loading: Option<Loading>,
    /// A sentence for the status line. Takes priority over the focused
    /// link's target, because it is only ever set as the answer to
    /// something the reader just did.
    message: Option<String>,
    /// A navigation waiting for the loop to act on it.
    request: Option<Requested>,
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

/// An open select list, anchored below or above its box in document space.
struct SelectPopup {
    control: u16,
    /// Highlighted option, counted from the select's first option.
    highlight: usize,
    /// First option shown.
    top: usize,
    rows: usize,
    x: u16,
    y: u32,
    width: u16,
}

fn popup_height(rows: usize) -> usize {
    rows * POPUP_ROW_HEIGHT + 2
}

fn union_damage(a: ViewportDamage, b: ViewportDamage) -> ViewportDamage {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    ViewportDamage {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

struct FormEditing {
    control: u16,
    input: TextInput,
    /// First displayed row of a textarea being edited.
    top_row: usize,
}

impl Viewer {
    fn new() -> Result<Viewer, Error> {
        let page = load_builtin(builtin::HOME)?;
        Ok(Viewer {
            page,
            history: Vec::new(),
            forward: Vec::new(),
            editing: None,
            form_editing: None,
            confirm: None,
            select_popup: None,
            loading: None,
            message: None,
            request: None,
            painted_bottom: VIEWPORT_BOTTOM,
            slowest_repaint_ms: 0,
            last_peak: 0,
            dirty: Dirty {
                toolbar: true,
                address: AddressDamage::default(),
                focus: [None; 4],
                control: ControlDamage::default(),
                popup: None,
                viewport: true,
                status: true,
            },
        })
    }

    fn dirty(&self) -> bool {
        self.dirty.toolbar
            || self.dirty.address.is_dirty()
            || self.dirty.focus.iter().any(Option::is_some)
            || self.dirty.control.is_dirty()
            || self.dirty.popup.is_some()
            || self.dirty.viewport
            || self.dirty.status
    }

    /// What the page on screen costs: its document and its layout.
    ///
    /// The two together, because they are freed together -- a page is
    /// replaced whole -- and because a layout that grew while a document
    /// did not is exactly the shape a wrapping bug takes.
    fn page_owned_bytes(&self) -> usize {
        self.page.document.stats().owned_bytes + self.page.layout.owned_bytes()
    }

    fn take_request(&mut self) -> Option<Requested> {
        self.request.take()
    }

    fn request(&mut self, navigation: Navigation) {
        self.drop_confirmation();
        self.request = Some(Requested::Navigation(navigation));
    }

    fn submit_request(&mut self, request: HttpRequest) {
        self.drop_confirmation();
        let source = self.page.visit_url.clone();
        self.request = Some(Requested::Submission(Submission {
            request,
            how: Direction::Fresh,
            restore: 0,
            source: Some(source),
            entry: None,
            redirects: 0,
        }));
    }

    // --- POST resend and retention ------------------------------------

    /// Puts a POST resend question on the status line.
    fn ask(&mut self, submission: Submission, reason: ConfirmReason) {
        self.drop_confirmation();
        self.close_select();
        if self.editing.take().is_some() {
            self.dirty.address.full();
        }
        self.finish_form_editing();
        self.confirm = Some(Confirmation { submission, reason });
        self.dirty.status = true;
    }

    fn accept_confirmation(&mut self) {
        let Some(confirmation) = self.confirm.take() else {
            return;
        };
        self.dirty.status = true;
        self.request = Some(Requested::Submission(confirmation.submission));
    }

    fn cancel_confirmation(&mut self) {
        let Some(confirmation) = self.confirm.take() else {
            return;
        };
        let reason = confirmation.reason;
        self.put_back_submission(confirmation.submission);
        self.say(if reason == ConfirmReason::Redirect {
            "POST was not sent to the other origin"
        } else {
            "POST was not resent"
        });
    }

    /// Withdraws an open question without saying anything, because what
    /// replaces it says something itself.
    fn drop_confirmation(&mut self) {
        if let Some(confirmation) = self.confirm.take() {
            self.put_back_submission(confirmation.submission);
            self.dirty.status = true;
        }
    }

    fn put_back_submission(&mut self, submission: Submission) {
        let Submission {
            request,
            how,
            mut entry,
            ..
        } = submission;
        if let Some(post) = entry.as_mut().and_then(|entry| entry.post.as_mut()) {
            post.request = Some(request);
        }
        self.put_back_entry(entry, how);
    }

    /// Returns an entry taken for a back/forward resend to where it was.
    fn put_back_entry(&mut self, entry: Option<HistoryEntry>, how: Direction) {
        let Some(entry) = entry else {
            return;
        };
        let stack = match how {
            Direction::Back => &mut self.history,
            Direction::Forward => &mut self.forward,
            Direction::Fresh | Direction::Reload => return,
        };
        if stack.len() >= MAX_HISTORY {
            stack.remove(0);
        }
        let _ = memory::push(stack, entry);
        self.enforce_post_budget();
    }

    /// Replaces the page on screen, keeping a POST result being left for
    /// history when the budget allows.
    fn replace_page(&mut self, page: Page, how: Direction) {
        self.drop_confirmation();
        self.select_popup = None;
        let left = core::mem::replace(&mut self.page, page);
        self.retain_post_result(left, how);
    }

    /// Attaches a POST result's document to the history entry that
    /// `settle_history` just made for it.
    fn retain_post_result(&mut self, left: Page, how: Direction) {
        if left.error || left.request_method != RequestMethod::Post {
            return;
        }
        if !left.post.as_ref().is_some_and(|post| post.keep) {
            return;
        }
        let stack = match how {
            Direction::Fresh | Direction::Forward => &mut self.history,
            Direction::Back => &mut self.forward,
            Direction::Reload => return,
        };
        let Some(entry) = stack.last_mut() else {
            return;
        };
        if entry.url != left.visit_url {
            return;
        }
        let Some(post) = entry.post.as_mut() else {
            return;
        };
        let bytes = left.document.stats().owned_bytes;
        if bytes > MAX_RETAINED_POST_RESULT_BYTES {
            return;
        }
        post.result = Some(RetainedResult {
            document: left.document,
            security: left.security,
            bytes,
        });
        self.enforce_post_budget();
    }

    /// Kept POST results and request bodies: count, result bytes, body bytes.
    fn retained_post(&self) -> (usize, usize, usize) {
        let mut totals = (0, 0, 0);
        for entry in self.history.iter().chain(self.forward.iter()) {
            let Some(post) = entry.post.as_ref() else {
                continue;
            };
            if let Some(result) = &post.result {
                totals.0 += 1;
                totals.1 += result.bytes;
            }
            if let Some(request) = &post.request {
                totals.2 += request.body().len();
            }
        }
        totals
    }

    /// Drops kept results, then kept requests, farthest from the page on
    /// screen first, until the retention budgets hold.
    ///
    /// Dropping only degrades what back/forward can do -- a dropped result
    /// asks before resending, a dropped request returns to the form -- and
    /// never touches the page on screen or an input value.
    fn enforce_post_budget(&mut self) {
        loop {
            let (results, result_bytes, request_bytes) = self.retained_post();
            let over_results = results > MAX_RETAINED_POST_RESULTS
                || result_bytes > MAX_RETAINED_POST_RESULT_BYTES;
            let over_requests = request_bytes > MAX_RETAINED_POST_REQUEST_BYTES;
            if !over_results && !over_requests {
                return;
            }
            let mut farthest: Option<(usize, bool, usize)> = None;
            for (forward, stack) in [(false, &self.history), (true, &self.forward)] {
                for (index, entry) in stack.iter().enumerate() {
                    let holds = entry.post.as_ref().is_some_and(|post| {
                        if over_results {
                            post.result.is_some()
                        } else {
                            post.request.is_some()
                        }
                    });
                    let distance = stack.len() - index;
                    if holds && farthest.is_none_or(|(most, _, _)| distance > most) {
                        farthest = Some((distance, forward, index));
                    }
                }
            }
            let Some((_, forward, index)) = farthest else {
                return;
            };
            let stack = if forward {
                &mut self.forward
            } else {
                &mut self.history
            };
            if let Some(post) = stack[index].post.as_mut() {
                if over_results {
                    post.result = None;
                } else {
                    post.request = None;
                }
            }
        }
    }

    /// Shows a kept POST result again. Nothing is sent.
    fn restore_post(&mut self, mut entry: HistoryEntry, how: Direction) {
        let Some(mut post) = entry.post.take() else {
            return;
        };
        let Some(result) = post.result.take() else {
            entry.post = Some(post);
            self.return_to(entry, how);
            return;
        };
        match build_page(result.document) {
            Ok(mut page) => {
                page.security = result.security;
                page.request_method = RequestMethod::Post;
                page.visit_url = entry.url.clone();
                page.post = Some(PagePost {
                    request: post.request.take(),
                    source: post.source.take(),
                    keep: true,
                });
                self.settle_history(how);
                self.replace_page(page, how);
                self.form_editing = None;
                self.scroll_to(entry.scroll_y);
                self.loading = None;
                self.dirty = Dirty::full();
                self.say("POST result shown from memory; nothing was resent");
            }
            // The document went into the failed layout. What is left is the
            // same as a result that was never kept.
            Err(_) => {
                entry.post = Some(post);
                self.return_to(entry, how);
            }
        }
    }

    /// Goes back or forward to an entry that has already left its stack.
    fn return_to(&mut self, mut entry: HistoryEntry, how: Direction) {
        let internal = self.page.request_method == entry.method
            && self.page.document.url().same_document(&entry.url);
        if entry.method != RequestMethod::Post || internal {
            self.request(Navigation {
                url: entry.url,
                restore: entry.scroll_y,
                how,
                bypass_cache: false,
            });
            return;
        }
        let Some(post) = entry.post.as_mut() else {
            self.put_back_entry(Some(entry), how);
            self.say("POST result was not kept and cannot be resent");
            return;
        };
        if post.result.is_some() {
            self.drop_confirmation();
            self.request = Some(Requested::Restore(entry, how));
            return;
        }
        if let Some(request) = post.request.take() {
            let source = post.source.clone();
            let restore = entry.scroll_y;
            self.ask(
                Submission {
                    request,
                    how,
                    restore,
                    source,
                    entry: Some(entry),
                    redirects: 0,
                },
                ConfirmReason::Resend,
            );
            return;
        }
        if let Some(source) = post.source.take() {
            // Neither the result nor the request survived: the form it came
            // from is the one place left to go.
            self.request(Navigation {
                url: source,
                restore: 0,
                how,
                bypass_cache: false,
            });
            return;
        }
        self.put_back_entry(Some(entry), how);
        self.say("POST result was not kept and cannot be resent");
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
        request_method: RequestMethod,
        post: Option<PagePost>,
    ) {
        match build_page(document) {
            Ok(mut page) => {
                page.security = security;
                page.request_method = request_method;
                page.post = post;
                self.settle_history(navigation.how);
                page.visit_url = landed;
                self.replace_page(page, navigation.how);
                self.form_editing = None;
                // Through `scroll_to` rather than assigned, so a remembered
                // position past the end of a page that has since got
                // shorter lands on the last screen instead of below it.
                self.scroll_to(navigation.restore);
                self.loading = None;
                self.message = None;
                self.dirty = Dirty {
                    toolbar: true,
                    address: AddressDamage::default(),
                    focus: [None; 4],
                    control: ControlDamage::default(),
                    popup: None,
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
                self.replace_page(loaded, navigation.how);
                self.form_editing = None;
                self.page.visit_url = navigation.url.clone();
                self.scroll_to(navigation.restore);
                self.loading = None;
                self.message = None;
                self.dirty = Dirty {
                    toolbar: true,
                    address: AddressDamage::default(),
                    focus: [None; 4],
                    control: ControlDamage::default(),
                    popup: None,
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
                self.replace_page(page, how);
                self.form_editing = None;
                self.message = None;
                self.dirty = Dirty {
                    toolbar: true,
                    address: AddressDamage::default(),
                    focus: [None; 4],
                    control: ControlDamage::default(),
                    popup: None,
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
        // The result document itself is attached by `retain_post_result`
        // once the page has actually been replaced; this copies only what
        // survives a fragment navigation within the same result as well.
        let post = self
            .page
            .post
            .as_ref()
            .filter(|_| self.page.request_method == RequestMethod::Post)
            .map(|post| PostEntry {
                request: post
                    .request
                    .as_ref()
                    .and_then(|request| request.try_clone().ok()),
                source: post.source.clone(),
                result: None,
            });
        let entry = HistoryEntry {
            url: self.page.visit_url.clone(),
            scroll_y: self.page.scroll_y,
            method: self.page.request_method,
            post,
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
        self.enforce_post_budget();
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
        self.drop_confirmation();
        let Some(entry) = self.history.pop() else {
            self.say("nothing to go back to");
            return;
        };
        self.return_to(entry, Direction::Back);
    }

    fn go_forward(&mut self) {
        self.drop_confirmation();
        let Some(entry) = self.forward.pop() else {
            self.say("nothing to go forward to");
            return;
        };
        self.return_to(entry, Direction::Forward);
    }

    /// Fetches the address showing again, keeping the reader's place.
    ///
    /// The scroll position is restored because the usual reason to reload
    /// is that the page may have changed under a reader who is partway
    /// down it. On an error page this retries what failed, which works
    /// because an error page's address is the address that failed.
    /// `force` fetches without the cached copy's validator.
    fn reload(&mut self, force: bool) {
        if self.page.request_method == RequestMethod::Post {
            // Never a GET of the same address, and never sent without a yes.
            let post = self.page.post.as_ref();
            let source = post.and_then(|post| post.source.clone());
            let copy = post
                .and_then(|post| post.request.as_ref())
                .map(HttpRequest::try_clone);
            match (copy, source) {
                (Some(Ok(request)), source) => self.ask(
                    Submission {
                        request,
                        how: Direction::Reload,
                        restore: self.page.scroll_y,
                        source,
                        entry: None,
                        redirects: 0,
                    },
                    ConfirmReason::Resend,
                ),
                (_, Some(source)) => self.request(Navigation::fresh(source)),
                (_, None) => self.say("POST result cannot be reloaded: the request was not kept"),
            }
            return;
        }
        let url = self.page.visit_url.clone();
        self.request(Navigation {
            url,
            restore: self.page.scroll_y,
            how: Direction::Reload,
            bypass_cache: force,
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
            let y = self.page.layout.lines().get(line).map_or(0, |line| line.y);
            self.scroll_to(y);
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
        // An open resend question takes every other key, so that nothing
        // typed can send a POST except its own answer.
        if self.confirm.is_some() {
            match key {
                Key::Ascii(b'y' | b'Y' | b'\r' | b'\n') => self.accept_confirmation(),
                Key::Escape | Key::Ascii(b'n' | b'N') => self.cancel_confirmation(),
                _ => {}
            }
            return Action::Continue;
        }
        if self.select_popup.is_some() {
            return self.handle_select_key(key);
        }
        if self.form_editing.is_some() {
            return self.handle_form_editing_key(key);
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
                    let before = self.focused_target();
                    self.page.focus = None;
                    self.dirty.focus_change(before, None);
                    self.dirty.status = true;
                }
                self.clear_message();
            }
            // A plain `q` as well as Ctrl+Q, because CardKB v1.1 has no
            // Ctrl key: on that keyboard this is the only way out, not a
            // fallback. Free to bind because nothing outside the address
            // field takes typed text, and it is what every pager does.
            Key::Ascii(b'q') | Key::Ascii(b'Q') => return Action::Leave,
            Key::ArrowDown => self.scroll_by_pixels(SCROLL_STEP),
            Key::ArrowUp => self.scroll_by_pixels(-SCROLL_STEP),
            Key::PageDown => self.scroll_by_pixels(self.page_step()),
            Key::PageUp => self.scroll_by_pixels(-self.page_step()),
            Key::Home => self.scroll_to(0),
            Key::End => self.scroll_to(self.max_scroll_y()),
            // Space pages down, the way every reader does -- except on a
            // focused checkbox or radio button, which it activates as in
            // every other browser.
            Key::Ascii(b' ') => match self.focused_control() {
                Some(control)
                    if self
                        .page
                        .document
                        .controls()
                        .get(control as usize)
                        .is_some_and(|item| {
                            item.kind.is_checkable() || item.kind == ControlKind::Select
                        }) =>
                {
                    self.activate_control(control)
                }
                _ => self.scroll_by_pixels(self.page_step()),
            },
            Key::Ascii(b'\t') => self.focus_next(),
            Key::Ascii(b'\r') | Key::Ascii(b'\n') => {
                // Enter on a selected link follows it; Enter with nothing
                // selected is "where do you want to go?". That is one key
                // doing two things, but they are never both available: a
                // reader who has not pressed Tab has nothing to follow.
                if let Some(control) = self.focused_control() {
                    self.activate_control(control);
                } else if self.page.focus.is_some() {
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
            Key::Ascii(b'r') | Key::Control(b'r') | Key::Function(5) => self.reload(false),
            // Shift+R: the forced reload, which does not ask the server to
            // confirm the cached copy and so always transfers the page.
            Key::Ascii(b'R') => self.reload(true),
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
        let before = self.address_visual();
        let Some(editing) = self.editing.as_mut() else {
            return Action::Continue;
        };
        // Editing changes only the address rectangle. Buttons and the lock
        // stay in the framebuffer and are not sent to the panel again.
        let mut changed_at = None;
        match key {
            // Escape cancels the edit rather than leaving the browser: one
            // key, and the field being open says which it means.
            Key::Escape => {
                self.editing = None;
                self.clear_message();
                self.dirty.address.full();
            }
            Key::Ascii(b'\r') | Key::Ascii(b'\n') => {
                let text = editing.take_text();
                self.editing = None;
                self.navigate_to_text(&text);
                self.dirty.address.full();
            }
            Key::Ascii(0x08) => {
                changed_at = editing
                    .selection()
                    .map(|selection| selection.start)
                    .or_else(|| {
                        editing.text()[..editing.caret()]
                            .char_indices()
                            .next_back()
                            .map(|(at, _)| at)
                    });
                if !editing.backspace() {
                    changed_at = None;
                }
            }
            // A keyboard that sends DEL for its backspace key is the common
            // case; one that has a separate forward-delete sends
            // `Key::Delete`. Both are handled, and neither guesses.
            Key::Ascii(0x7F) => {
                changed_at = editing
                    .selection()
                    .map(|selection| selection.start)
                    .or_else(|| {
                        editing.text()[..editing.caret()]
                            .char_indices()
                            .next_back()
                            .map(|(at, _)| at)
                    });
                if !editing.backspace() {
                    changed_at = None;
                }
            }
            Key::Delete => {
                changed_at = Some(
                    editing
                        .selection()
                        .map_or(editing.caret(), |selection| selection.start),
                );
                if !editing.delete() {
                    changed_at = None;
                }
            }
            Key::ArrowLeft => editing.move_left(false),
            Key::ArrowRight => editing.move_right(false),
            Key::Home => editing.move_home(false),
            Key::End => editing.move_end(false),
            Key::Control(b'a') => editing.select_all(),
            Key::Ascii(byte) if (0x20..0x7F).contains(&byte) => {
                changed_at = Some(
                    editing
                        .selection()
                        .map_or(editing.caret(), |selection| selection.start),
                );
                if !editing.insert_char(byte as char) {
                    changed_at = None;
                }
            }
            _ => return Action::Continue,
        }
        if self.editing.is_some() {
            let after = self.address_visual();
            self.damage_address_change(before, after, changed_at);
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
        self.editing = Some(TextInput::new(
            text,
            MAX_URL_BYTES,
            TextInputMode::SingleLine,
        ));
        self.page.focus = None;
        self.say("edit the address; Enter goes, Escape cancels");
        self.dirty.address.full();
        self.dirty.viewport = true;
    }

    fn start_form_editing(&mut self) {
        let Some(control) = self.focused_control() else {
            return;
        };
        let Some(definition) = self.page.document.controls().get(control as usize) else {
            return;
        };
        let multiline = definition.kind == ControlKind::Textarea;
        if !(definition.kind == ControlKind::Text || multiline) || definition.disabled {
            self.say("submit is not connected yet");
            return;
        }
        if definition.value_overflow {
            self.say("this textarea's initial text is too long to edit");
            return;
        }
        let value = self
            .page
            .control_values
            .get(control as usize)
            .map(String::as_str)
            .unwrap_or(&definition.initial_value);
        let Ok(value) = memory::string_from(value) else {
            self.say("not enough memory to edit this field");
            return;
        };
        self.form_editing = Some(FormEditing {
            control,
            input: TextInput::new(
                value,
                MAX_INPUT_VALUE_BYTES,
                if multiline {
                    TextInputMode::MultiLine
                } else {
                    TextInputMode::SingleLine
                },
            ),
            top_row: 0,
        });
        self.keep_textarea_caret_visible();
        self.say(if multiline {
            "editing textarea; Enter adds a line, Escape finishes, Tab moves on"
        } else {
            "editing text input; Enter submits, Escape finishes"
        });
        self.dirty.focus_change(
            Some(FocusTarget::Control(control)),
            Some(FocusTarget::Control(control)),
        );
    }

    fn activate_control(&mut self, control: u16) {
        let Some(definition) = self.page.document.controls().get(control as usize) else {
            return;
        };
        match definition.kind {
            ControlKind::Text | ControlKind::Textarea => self.start_form_editing(),
            ControlKind::Submit if !definition.disabled => {
                self.submit_form(control, Some(control as usize))
            }
            ControlKind::Checkbox | ControlKind::Radio if !definition.disabled => {
                self.toggle_control(control)
            }
            ControlKind::Select if !definition.disabled => self.open_select(control),
            _ => self.say("this control cannot be activated"),
        }
    }

    /// Opens a select's list below its box, or above it when there is no
    /// room below, with the (first) selected option highlighted.
    fn open_select(&mut self, control: u16) {
        let Some(definition) = self.page.document.controls().get(control as usize) else {
            return;
        };
        let count = definition.option_count as usize;
        if count == 0 {
            self.say("this list has no options");
            return;
        }
        let first = definition.first_option as usize;
        let multiple = definition.multiple;
        self.scroll_focus_into_view();
        let Some(item) = self
            .page
            .layout
            .controls()
            .iter()
            .find(|item| item.control == control)
            .copied()
        else {
            return;
        };
        let rows = count.min(POPUP_MAX_ROWS);
        let height = popup_height(rows) as u32;
        let width = item.width.max(240).min(PAGE_WIDTH as u16);
        let x = item.x.min(PAGE_WIDTH as u16 - width);
        let view_top = self.page.scroll_y;
        let view_bottom = view_top.saturating_add(VIEWPORT_HEIGHT as u32);
        let below = item.y.saturating_add(item.height as u32);
        let y = if below.saturating_add(height) <= view_bottom {
            below
        } else if item.y >= view_top.saturating_add(height) {
            item.y - height
        } else {
            view_bottom.saturating_sub(height).max(view_top)
        };
        let highlight = (0..count)
            .find(|offset| {
                self.page
                    .option_selected
                    .get(first + offset)
                    .copied()
                    .unwrap_or(false)
            })
            .unwrap_or(0);
        self.select_popup = Some(SelectPopup {
            control,
            highlight,
            top: highlight.saturating_sub(rows - 1),
            rows,
            x,
            y,
            width,
        });
        self.damage_popup();
        self.say(if multiple {
            "Up/Down move, Space or Enter toggles, Escape closes"
        } else {
            "Up/Down move, Enter chooses, Escape closes"
        });
    }

    fn damage_popup(&mut self) {
        let Some(popup) = &self.select_popup else {
            return;
        };
        let Some(rect) = self.viewport_damage(
            MARGIN + popup.x as usize,
            popup.y,
            popup.width as usize,
            popup_height(popup.rows) as u32,
        ) else {
            return;
        };
        self.dirty.popup = Some(match self.dirty.popup {
            Some(old) => union_damage(old, rect),
            None => rect,
        });
    }

    /// Closes the list, repainting what it covered and its select's box.
    fn close_select(&mut self) {
        if self.select_popup.is_none() {
            return;
        }
        self.damage_popup();
        if let Some(popup) = self.select_popup.take() {
            self.dirty
                .focus_change(Some(FocusTarget::Control(popup.control)), None);
        }
        self.clear_message();
    }

    fn handle_select_key(&mut self, key: Key) -> Action {
        let Some(popup) = self.select_popup.as_ref() else {
            return Action::Continue;
        };
        let control = popup.control;
        let rows = popup.rows;
        let highlight = popup.highlight;
        let Some(definition) = self.page.document.controls().get(control as usize) else {
            self.close_select();
            return Action::Continue;
        };
        let count = definition.option_count as usize;
        let multiple = definition.multiple;
        let last = count.saturating_sub(1);
        let target = match key {
            Key::Escape => {
                self.close_select();
                return Action::Continue;
            }
            Key::Ascii(b'\t') => {
                self.close_select();
                self.focus_next();
                return Action::Continue;
            }
            Key::Ascii(b'\r' | b'\n' | b' ') => {
                // A disabled option leaves the list open, so the reason stays
                // on the status line instead of being cleared by the close.
                if self.choose_highlighted() && !multiple {
                    self.close_select();
                }
                return Action::Continue;
            }
            Key::ArrowUp => highlight.saturating_sub(1),
            Key::ArrowDown => (highlight + 1).min(last),
            Key::PageUp => highlight.saturating_sub(rows),
            Key::PageDown => (highlight + rows).min(last),
            Key::Home => 0,
            Key::End => last,
            _ => return Action::Continue,
        };
        self.highlight_option(target);
        Action::Continue
    }

    fn highlight_option(&mut self, target: usize) {
        let Some(popup) = self.select_popup.as_mut() else {
            return;
        };
        if popup.highlight == target {
            return;
        }
        popup.highlight = target;
        popup.top = popup
            .top
            .min(target)
            .max((target + 1).saturating_sub(popup.rows));
        self.damage_popup();
    }

    /// Chooses the highlighted option. Returns whether it was chosen.
    fn choose_highlighted(&mut self) -> bool {
        let Some(popup) = self.select_popup.as_ref() else {
            return false;
        };
        let control = popup.control;
        let Some(first) = self
            .page
            .document
            .controls()
            .get(control as usize)
            .map(|item| item.first_option as usize)
        else {
            return false;
        };
        if form::choose_option(
            &self.page.document,
            &mut self.page.option_selected,
            control as usize,
            first + popup.highlight,
        ) {
            self.damage_popup();
            self.dirty
                .focus_change(Some(FocusTarget::Control(control)), None);
            true
        } else {
            self.say("this option is disabled");
            false
        }
    }

    /// A tap while the list is open: a row chooses it, anywhere else closes
    /// the list and does nothing more.
    fn click_select_popup(&mut self, x: usize, y: usize) {
        let Some(popup) = self.select_popup.as_ref() else {
            return;
        };
        let left = MARGIN + popup.x as usize;
        let screen_top = VIEWPORT_TOP as i64 + popup.y as i64 - self.page.scroll_y as i64 + 1;
        let offset_y = y as i64 - screen_top;
        let row = (offset_y >= 0).then(|| offset_y as usize / POPUP_ROW_HEIGHT);
        let inside = (left..left + popup.width as usize).contains(&x)
            && (VIEWPORT_TOP..VIEWPORT_BOTTOM).contains(&y);
        let multiple = self
            .page
            .document
            .controls()
            .get(popup.control as usize)
            .is_some_and(|item| item.multiple);
        let count = self
            .page
            .document
            .controls()
            .get(popup.control as usize)
            .map_or(0, |item| item.option_count as usize);
        match row.filter(|row| inside && *row < popup.rows) {
            Some(row) if popup.top + row < count => {
                let target = popup.top + row;
                self.highlight_option(target);
                // A disabled option leaves the list open, so the reason stays
                // on the status line instead of being cleared by the close.
                if self.choose_highlighted() && !multiple {
                    self.close_select();
                }
            }
            _ => self.close_select(),
        }
    }

    /// Toggles a checkbox or selects a radio button, repainting only the
    /// controls whose checkedness changed.
    fn toggle_control(&mut self, control: u16) {
        let mut changed = [None; 4];
        let mut overflow = false;
        let activated = form::activate_checkable(
            &self.page.document,
            &mut self.page.control_checked,
            control as usize,
            |id| match changed.iter_mut().find(|slot| slot.is_none()) {
                Some(slot) => *slot = Some(id as u16),
                None => overflow = true,
            },
        );
        if !activated {
            self.say("this control cannot be activated");
            return;
        }
        if overflow {
            self.dirty.viewport = true;
        }
        for id in changed.into_iter().flatten() {
            self.dirty
                .focus_change(Some(FocusTarget::Control(id)), None);
        }
        self.clear_message();
        self.dirty.status = true;
    }

    fn finish_form_editing(&mut self) {
        let Some(mut editing) = self.form_editing.take() else {
            return;
        };
        if let Some(value) = self.page.control_values.get_mut(editing.control as usize) {
            *value = editing.input.take_text();
        }
        self.clear_message();
        self.dirty.focus_change(
            Some(FocusTarget::Control(editing.control)),
            Some(FocusTarget::Control(editing.control)),
        );
    }

    fn submit_form(&mut self, control: u16, activated_submit: Option<usize>) {
        let Some(form_index) = self
            .page
            .document
            .controls()
            .get(control as usize)
            .and_then(|control| control.form)
        else {
            self.say("this control has no form");
            return;
        };
        match form::submit(
            &self.page.document,
            form_index as usize,
            &self.page.control_values,
            &self.page.control_checked,
            &self.page.option_selected,
            activated_submit,
        ) {
            Ok(request) if request.method == RequestMethod::Get => {
                self.request(Navigation::fresh(request.url))
            }
            Ok(request) => self.submit_request(request),
            Err(form::Error::TooLong) => self.say("form result URL is too long"),
            Err(form::Error::OutOfMemory) => self.say("not enough memory to submit form"),
            Err(form::Error::NoSuchForm) => self.say("form no longer exists"),
            Err(form::Error::UnsupportedMethod) => self.say("this form method is unsupported"),
            Err(form::Error::ValueOverflow) => {
                self.say("a textarea's initial text is too long to submit")
            }
        }
    }

    fn handle_form_editing_key(&mut self, key: Key) -> Action {
        if self
            .form_editing
            .as_ref()
            .is_some_and(|editing| editing.input.mode() == TextInputMode::MultiLine)
        {
            return self.handle_textarea_key(key);
        }
        let before = self.form_visual();
        let mut changed_at = None;
        let Some(editing) = self.form_editing.as_mut() else {
            return Action::Continue;
        };
        match key {
            Key::Escape => {
                self.finish_form_editing();
                return Action::Continue;
            }
            Key::Ascii(b'\r') | Key::Ascii(b'\n') => {
                let control = editing.control;
                self.finish_form_editing();
                self.submit_form(control, None);
                return Action::Continue;
            }
            Key::Ascii(b'\t') => {
                self.finish_form_editing();
                self.focus_next();
                return Action::Continue;
            }
            Key::Ascii(0x08) | Key::Ascii(0x7f) => {
                changed_at = editing
                    .input
                    .selection()
                    .map(|selection| selection.start)
                    .or_else(|| {
                        editing.input.text()[..editing.input.caret()]
                            .char_indices()
                            .next_back()
                            .map(|(at, _)| at)
                    });
                if !editing.input.backspace() {
                    changed_at = None;
                }
            }
            Key::Delete => {
                changed_at = Some(
                    editing
                        .input
                        .selection()
                        .map_or(editing.input.caret(), |selection| selection.start),
                );
                if !editing.input.delete() {
                    changed_at = None;
                }
            }
            Key::ArrowLeft => editing.input.move_left(false),
            Key::ArrowRight => editing.input.move_right(false),
            Key::Home => editing.input.move_home(false),
            Key::End => editing.input.move_end(false),
            Key::Control(b'a') => editing.input.select_all(),
            Key::Ascii(byte) if (0x20..0x7f).contains(&byte) => {
                changed_at = Some(
                    editing
                        .input
                        .selection()
                        .map_or(editing.input.caret(), |selection| selection.start),
                );
                if !editing.input.insert_char(byte as char) {
                    changed_at = None;
                }
            }
            _ => return Action::Continue,
        }
        let after = self.form_visual();
        self.damage_form_change(before, after, changed_at);
        Action::Continue
    }

    /// Keys while a textarea is edited. Enter is a line break rather than a
    /// submission, and the arrows move between displayed rows. Every change
    /// repaints the text area of the box: a line break or a wrap moves
    /// everything after it, so a narrower damage rectangle buys little.
    fn handle_textarea_key(&mut self, key: Key) -> Action {
        let Some(control) = self.form_editing.as_ref().map(|editing| editing.control) else {
            return Action::Continue;
        };
        let Some((left, right)) = self.form_text_bounds(control) else {
            return Action::Continue;
        };
        let Some(editing) = self.form_editing.as_mut() else {
            return Action::Continue;
        };
        let measure = |text: &str| crate::font::ui_text_width(text, crate::font::UiTextStyle::BODY);
        match key {
            Key::Escape => {
                self.finish_form_editing();
                return Action::Continue;
            }
            Key::Ascii(b'\t') => {
                self.finish_form_editing();
                self.focus_next();
                return Action::Continue;
            }
            Key::Ascii(b'\r') | Key::Ascii(b'\n') => {
                editing.input.insert_char('\n');
            }
            Key::Ascii(0x08) | Key::Ascii(0x7f) => {
                editing.input.backspace();
            }
            Key::Delete => {
                editing.input.delete();
            }
            Key::ArrowLeft => editing.input.move_left(false),
            Key::ArrowRight => editing.input.move_right(false),
            Key::ArrowUp | Key::ArrowDown => {
                let width = right.saturating_sub(left).saturating_sub(2);
                if let Ok(rows) = text_input::wrap_rows(editing.input.text(), width, measure) {
                    editing
                        .input
                        .move_row(&rows, key == Key::ArrowDown, false, measure);
                }
            }
            Key::Home => editing.input.move_home(false),
            Key::End => editing.input.move_end(false),
            Key::Control(b'a') => editing.input.select_all(),
            Key::Ascii(byte) if (0x20..0x7f).contains(&byte) => {
                editing.input.insert_char(byte as char);
            }
            _ => return Action::Continue,
        }
        self.keep_textarea_caret_visible();
        self.dirty
            .control
            .add(control, left, right, left, right - left);
        Action::Continue
    }

    /// Scrolls a textarea being edited so the caret's row is inside its box.
    fn keep_textarea_caret_visible(&mut self) {
        let Some(control) = self.form_editing.as_ref().map(|editing| editing.control) else {
            return;
        };
        let Some((left, right)) = self.form_text_bounds(control) else {
            return;
        };
        let visible = self
            .page
            .document
            .controls()
            .get(control as usize)
            .map_or(1, |item| usize::from(item.rows.max(1)));
        let Some(editing) = self.form_editing.as_mut() else {
            return;
        };
        if editing.input.mode() != TextInputMode::MultiLine {
            return;
        }
        let width = right.saturating_sub(left).saturating_sub(2);
        let Ok(rows) = text_input::wrap_rows(editing.input.text(), width, |text| {
            crate::font::ui_text_width(text, crate::font::UiTextStyle::BODY)
        }) else {
            return;
        };
        let row = text_input::row_of(&rows, editing.input.caret());
        editing.top_row = editing
            .top_row
            .min(rows.len().saturating_sub(visible))
            .min(row)
            .max((row + 1).saturating_sub(visible));
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
        if self.form_editing.is_some() {
            self.finish_form_editing();
        }
        if self.select_popup.is_some() {
            self.click_select_popup(x, y);
            return Action::Continue;
        }
        if self.confirm.is_some() {
            if y >= VIEWPORT_BOTTOM {
                if (CONFIRM_SEND_LEFT..CONFIRM_SEND_LEFT + CONFIRM_BUTTON_WIDTH).contains(&x) {
                    self.accept_confirmation();
                } else if (CONFIRM_CANCEL_LEFT..CONFIRM_CANCEL_LEFT + CONFIRM_BUTTON_WIDTH)
                    .contains(&x)
                {
                    self.cancel_confirmation();
                }
                return Action::Continue;
            }
            // Anywhere else is a no, and then whatever that tap means.
            self.cancel_confirmation();
        }
        if y < TOOLBAR_HEIGHT {
            return self.click_toolbar(x);
        }
        if y >= VIEWPORT_BOTTOM {
            return Action::Continue;
        }
        if self.editing.is_some() {
            self.editing = None;
            self.clear_message();
            self.dirty.address.full();
        }
        let document_y = self.page.scroll_y + (y - VIEWPORT_TOP) as u32;
        let Some(document_x) = x.checked_sub(MARGIN) else {
            return Action::Continue;
        };
        if let Some(control) = self.page.layout.control_at(document_x as u16, document_y)
            && self
                .page
                .document
                .controls()
                .get(control as usize)
                .is_some_and(|item| !item.disabled)
        {
            let kind = self.page.document.controls()[control as usize].kind;
            let before = self.focused_target();
            self.page.focus = self
                .page
                .order
                .iter()
                .position(|item| *item == FocusTarget::Control(control));
            self.clear_message();
            self.dirty
                .focus_change(before, Some(FocusTarget::Control(control)));
            self.dirty.status = true;
            if matches!(kind, ControlKind::Text | ControlKind::Textarea) {
                self.start_form_editing();
            } else if kind == ControlKind::Submit
                || kind == ControlKind::Select
                || kind.is_checkable()
            {
                self.activate_control(control);
            }
            return Action::Continue;
        }
        if let Some(control) =
            self.page
                .layout
                .label_control_at(&self.page.document, document_x as u16, document_y)
            && self
                .page
                .document
                .controls()
                .get(control as usize)
                .is_some_and(|item| !item.disabled)
        {
            let kind = self.page.document.controls()[control as usize].kind;
            let before = self.focused_target();
            self.page.focus = self
                .page
                .order
                .iter()
                .position(|item| *item == FocusTarget::Control(control));
            self.clear_message();
            self.scroll_focus_into_view();
            self.dirty
                .focus_change(before, Some(FocusTarget::Control(control)));
            self.dirty.status = true;
            if matches!(kind, ControlKind::Text | ControlKind::Textarea) {
                self.start_form_editing();
            } else if kind.is_checkable() || kind == ControlKind::Select {
                // A label activates its checkbox or radio button, which is
                // most of what a label next to one is for.
                self.activate_control(control);
            }
            return Action::Continue;
        }
        match self.page.layout.hit(document_x as u16, document_y) {
            Some(link) => {
                let before = self.focused_target();
                self.page.focus = self
                    .page
                    .order
                    .iter()
                    .position(|item| *item == FocusTarget::Link(link));
                self.clear_message();
                self.dirty
                    .focus_change(before, Some(FocusTarget::Link(link)));
                self.dirty.status = true;
                self.follow_focused();
            }
            None => {
                if self.page.focus.is_some() {
                    let before = self.focused_target();
                    self.page.focus = None;
                    self.clear_message();
                    self.dirty.focus_change(before, None);
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
            Button::Reload => self.reload(false),
        }
        Action::Continue
    }

    /// Empties the address field, leaving it open with the caret at the
    /// start. Only reachable while it is open.
    fn clear_address(&mut self) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        editing.clear();
        self.dirty.address.full();
    }

    fn focus_next(&mut self) {
        if self.page.order.is_empty() {
            self.say("this page has no interactive items");
            return;
        }
        let before = self.focused_target();
        self.page.focus = Some(match self.page.focus {
            Some(position) => (position + 1) % self.page.order.len(),
            None => 0,
        });
        self.clear_message();
        self.scroll_focus_into_view();
        self.dirty.focus_change(before, self.focused_target());
        self.dirty.status = true;
        if self.focused_control().is_some_and(|control| {
            self.page
                .document
                .controls()
                .get(control as usize)
                .is_some_and(|item| matches!(item.kind, ControlKind::Text | ControlKind::Textarea))
        }) {
            self.start_form_editing();
        }
    }

    fn follow_focused(&mut self) {
        if self.focused_control().is_some() {
            self.say("control editing is not connected yet");
            return;
        }
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
        match self.page.order.get(self.page.focus?)? {
            FocusTarget::Link(link) => Some(*link),
            FocusTarget::Control(_) => None,
        }
    }

    fn focused_target(&self) -> Option<FocusTarget> {
        self.page
            .focus
            .and_then(|position| self.page.order.get(position))
            .copied()
    }

    fn focused_control(&self) -> Option<u16> {
        match self.page.order.get(self.page.focus?)? {
            FocusTarget::Control(control) => Some(*control),
            FocusTarget::Link(_) => None,
        }
    }

    // --- scrolling ----------------------------------------------------

    /// The document-space y of the top of the viewport.
    fn top_offset(&self) -> Option<u32> {
        (!self.page.layout.lines().is_empty()).then_some(self.page.scroll_y)
    }

    /// A page turn leaves one normal body row as visual overlap.
    fn page_step(&self) -> i32 {
        (VIEWPORT_HEIGHT as i32 - SCROLL_STEP).max(SCROLL_STEP)
    }

    fn scroll_by_pixels(&mut self, pixels: i32) {
        let target = if pixels < 0 {
            self.page.scroll_y.saturating_sub(pixels.unsigned_abs())
        } else {
            self.page.scroll_y.saturating_add(pixels as u32)
        };
        self.scroll_to(target);
    }

    fn scroll_to(&mut self, y: u32) {
        let y = y.min(self.max_scroll_y());
        if y != self.page.scroll_y {
            // The list is anchored to where its box was on screen.
            self.select_popup = None;
            self.page.scroll_y = y;
            self.dirty.viewport = true;
            self.dirty.toolbar = true;
        }
    }

    fn install_decoded_image(
        &mut self,
        image: usize,
        decoded: Rc<DecodedImage>,
    ) -> Result<(), Error> {
        let reading = self.page.layout.reading_position(self.page.scroll_y);
        // A new layout can move the select box the open list belongs to.
        self.select_popup = None;
        let focused = self
            .page
            .focus
            .and_then(|position| self.page.order.get(position))
            .copied();
        let previous = self
            .page
            .document
            .images()
            .get(image)
            .map(|image| (image.intrinsic_width, image.intrinsic_height));
        self.page
            .document
            .set_image_intrinsic(image, Some((decoded.width.max(1), decoded.height.max(1))));
        let layout = match Layout::build(
            &self.page.document,
            PAGE_WIDTH as u16,
            Metrics {
                glyph_height: CELL_HEIGHT as u16,
                line_gap_percent: crate::browser::layout::LINE_GAP_PERCENT,
            },
        ) {
            Ok(layout) => layout,
            Err(error) => {
                if let Some((width, height)) = previous {
                    self.page
                        .document
                        .set_image_intrinsic(image, width.zip(height));
                }
                return Err(error);
            }
        };
        let order = focus_order(&self.page.document, &layout)?;
        self.page.scroll_y = layout
            .y_of_reading_position(reading)
            .min(layout.height().saturating_sub(VIEWPORT_HEIGHT as u32));
        self.page.layout = layout;
        self.page.order = order;
        self.page.focus =
            focused.and_then(|target| self.page.order.iter().position(|item| *item == target));
        self.page.decoded_images[image] = Some(decoded);
        self.dirty.viewport = true;
        Ok(())
    }

    fn max_scroll_y(&self) -> u32 {
        self.page
            .layout
            .height()
            .saturating_sub(VIEWPORT_HEIGHT as u32)
    }

    fn scroll_focus_into_view(&mut self) {
        let target = match self.page.order.get(self.page.focus.unwrap_or(usize::MAX)) {
            Some(FocusTarget::Link(link)) => {
                let Some(line) = self.page.layout.line_of_link(*link) else {
                    return;
                };
                let line = self.page.layout.lines()[line];
                (line.y, line.height as u32)
            }
            Some(FocusTarget::Control(control)) => {
                let Some(control) = self
                    .page
                    .layout
                    .controls()
                    .iter()
                    .find(|item| item.control == *control)
                else {
                    return;
                };
                (control.y, control.height as u32)
            }
            None => return,
        };
        if target.0 < self.page.scroll_y {
            self.scroll_to(target.0);
            return;
        }
        let viewport_bottom = self.page.scroll_y.saturating_add(VIEWPORT_HEIGHT as u32);
        let target_bottom = target.0.saturating_add(target.1);
        if target_bottom > viewport_bottom {
            self.scroll_to(target_bottom.saturating_sub(VIEWPORT_HEIGHT as u32));
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
            address: AddressDamage::default(),
            focus: [None; 4],
            control: ControlDamage::default(),
            popup: None,
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
        } else if dirty.address.is_dirty() {
            for rect in dirty.address.rects {
                if rect.width != 0 {
                    let old_clip = framebuffer.set_horizontal_clip(rect.x, rect.x + rect.width);
                    self.draw_address_area(framebuffer);
                    framebuffer.set_horizontal_clip(old_clip.0, old_clip.1);
                    ok &= framebuffer.flush_rect(rect.x, FIELD_TOP, rect.width, FIELD_HEIGHT);
                }
            }
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
        } else {
            for target in dirty.focus.into_iter().flatten() {
                ok &= self.draw_focus_damage(framebuffer, service, target);
            }
            if dirty.control.is_dirty() {
                ok &= self.draw_control_damage(framebuffer, service, dirty.control);
            }
            if let Some(rect) = dirty.popup {
                let old_horizontal =
                    framebuffer.set_horizontal_clip(rect.x, rect.x.saturating_add(rect.width));
                let old_vertical =
                    framebuffer.set_vertical_clip(rect.y, rect.y.saturating_add(rect.height));
                self.draw_viewport(framebuffer, service);
                framebuffer.set_vertical_clip(old_vertical.0, old_vertical.1);
                framebuffer.set_horizontal_clip(old_horizontal.0, old_horizontal.1);
                ok &= framebuffer.flush_rect(rect.x, rect.y, rect.width, rect.height);
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

        self.draw_address_area(framebuffer);
    }

    fn draw_address_area(&self, framebuffer: &mut Framebuffer) {
        framebuffer.fill_rect(
            ADDRESS_LEFT.saturating_sub(4),
            FIELD_TOP,
            ADDRESS_RIGHT - ADDRESS_LEFT + 8,
            FIELD_HEIGHT,
            CHROME_BACKGROUND,
        );
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

    fn address_visual(&self) -> Option<AddressVisual> {
        let editing = self.editing.as_ref()?;
        let text_budget = CLEAR_LEFT.saturating_sub(8 + ADDRESS_LEFT);
        let first = editing.visible_start(text_budget.saturating_sub(2), |text| {
            crate::font::ui_text_width(text, crate::font::UiTextStyle::BODY)
        });
        let measure_from_first = |at: usize| {
            crate::font::ui_text_width(
                &editing.text()[first..at.max(first)],
                crate::font::UiTextStyle::BODY,
            )
            .min(text_budget)
        };
        let selection = editing.selection().and_then(|selection| {
            let start = measure_from_first(selection.start);
            let end = measure_from_first(selection.end);
            (start < end).then_some((ADDRESS_LEFT + start, ADDRESS_LEFT + end))
        });
        Some(AddressVisual {
            first,
            caret_x: ADDRESS_LEFT + measure_from_first(editing.caret()),
            selection,
        })
    }

    fn damage_address_change(
        &mut self,
        before: Option<AddressVisual>,
        after: Option<AddressVisual>,
        changed_at: Option<usize>,
    ) {
        let (Some(before), Some(after)) = (before, after) else {
            self.dirty.address.full();
            return;
        };
        if before.first != after.first {
            self.dirty.address.full();
            return;
        }
        if let Some(changed_at) = changed_at {
            if changed_at < after.first {
                self.dirty.address.full();
            } else {
                let x = ADDRESS_LEFT
                    + crate::font::ui_text_width(
                        &self.editing.as_ref().unwrap().text()[after.first..changed_at],
                        crate::font::UiTextStyle::BODY,
                    );
                self.dirty.address.add(x, ADDRESS_RIGHT.saturating_sub(x));
            }
            return;
        }
        self.dirty.address.add(before.caret_x, 2);
        self.dirty.address.add(after.caret_x, 2);
        if let Some((left, right)) = before.selection {
            self.dirty.address.add(left, right - left);
        }
        if let Some((left, right)) = after.selection {
            self.dirty.address.add(left, right - left);
        }
    }

    fn form_text_bounds(&self, control: u16) -> Option<(usize, usize)> {
        let item = self
            .page
            .layout
            .controls()
            .iter()
            .find(|item| item.control == control)?;
        let left = MARGIN + item.x as usize + 6;
        Some((left, left + item.width.saturating_sub(12) as usize))
    }

    fn form_visual(&self) -> Option<AddressVisual> {
        let editing = self.form_editing.as_ref()?;
        let (left, right) = self.form_text_bounds(editing.control)?;
        let budget = right.saturating_sub(left);
        let first = editing
            .input
            .visible_start(budget.saturating_sub(2), |text| {
                crate::font::ui_text_width(text, crate::font::UiTextStyle::BODY)
            });
        let measure_from_first = |at: usize| {
            crate::font::ui_text_width(
                &editing.input.text()[first..at.max(first)],
                crate::font::UiTextStyle::BODY,
            )
            .min(budget)
        };
        let selection = editing.input.selection().and_then(|selection| {
            let start = measure_from_first(selection.start);
            let end = measure_from_first(selection.end);
            (start < end).then_some((left + start, left + end))
        });
        Some(AddressVisual {
            first,
            caret_x: left + measure_from_first(editing.input.caret()),
            selection,
        })
    }

    fn damage_form_change(
        &mut self,
        before: Option<AddressVisual>,
        after: Option<AddressVisual>,
        changed_at: Option<usize>,
    ) {
        let Some(control) = self.form_editing.as_ref().map(|editing| editing.control) else {
            return;
        };
        let Some((left, right)) = self.form_text_bounds(control) else {
            return;
        };
        let (Some(before), Some(after)) = (before, after) else {
            self.dirty
                .control
                .add(control, left, right, left, right - left);
            return;
        };
        if before.first != after.first {
            self.dirty
                .control
                .add(control, left, right, left, right - left);
            return;
        }
        if let Some(changed_at) = changed_at {
            if changed_at < after.first {
                self.dirty
                    .control
                    .add(control, left, right, left, right - left);
            } else {
                let x = left
                    + crate::font::ui_text_width(
                        &self.form_editing.as_ref().unwrap().input.text()[after.first..changed_at],
                        crate::font::UiTextStyle::BODY,
                    );
                self.dirty
                    .control
                    .add(control, left, right, x, right.saturating_sub(x));
            }
            return;
        }
        self.dirty
            .control
            .add(control, left, right, before.caret_x, 2);
        self.dirty
            .control
            .add(control, left, right, after.caret_x, 2);
        if let Some((selection_left, selection_right)) = before.selection {
            self.dirty.control.add(
                control,
                left,
                right,
                selection_left,
                selection_right - selection_left,
            );
        }
        if let Some((selection_left, selection_right)) = after.selection {
            self.dirty.control.add(
                control,
                left,
                right,
                selection_left,
                selection_right - selection_left,
            );
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
        let first = editing.visible_start(text_budget.saturating_sub(2), |text| {
            crate::font::ui_text_width(text, crate::font::UiTextStyle::BODY)
        });
        if let Some(selection) = editing.selection() {
            let visible_start = selection.start.max(first);
            let visible_end = selection.end.max(first);
            if visible_start < visible_end {
                let selection_x = crate::font::ui_text_width(
                    &editing.text()[first..visible_start],
                    crate::font::UiTextStyle::BODY,
                )
                .min(text_budget);
                let selection_width = crate::font::ui_text_width(
                    &editing.text()[visible_start..visible_end],
                    crate::font::UiTextStyle::BODY,
                )
                .min(text_budget.saturating_sub(selection_x));
                framebuffer.fill_rect(
                    ADDRESS_LEFT + selection_x,
                    CHROME_TEXT_Y,
                    selection_width,
                    CELL_HEIGHT * CHROME_SCALE,
                    EDIT_SELECTION,
                );
            }
        }
        draw_clipped(
            framebuffer,
            ADDRESS_LEFT,
            CHROME_TEXT_Y,
            &editing.text()[first..],
            text_budget,
            BLACK,
        );
        let caret_x = crate::font::ui_text_width(
            &editing.text()[first..editing.caret()],
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
        let color = if editing.text().is_empty() {
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
        if let Some(confirmation) = &self.confirm {
            draw_confirmation(framebuffer, confirmation);
            return;
        }
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
        } else if let Some(control) = self.focused_control()
            && let Some(control) = self.page.document.controls().get(control as usize)
        {
            let checked = self
                .page
                .control_checked
                .get(self.focused_control().unwrap_or(u16::MAX) as usize)
                .copied()
                .unwrap_or(false);
            let text = match (control.kind, checked) {
                (ControlKind::Text, _) => "text input",
                (ControlKind::Textarea, _) => "textarea",
                (ControlKind::Select, _) => "list; Enter or Space opens it",
                (ControlKind::Submit, _) => "submit button",
                (ControlKind::Checkbox, true) => "checkbox, checked; Space or Enter toggles",
                (ControlKind::Checkbox, false) => "checkbox, not checked; Space or Enter toggles",
                (ControlKind::Radio, true) => "radio button, selected",
                (ControlKind::Radio, false) => "radio button, not selected; Space or Enter selects",
                _ => "form control",
            };
            draw_clipped(
                framebuffer,
                MARGIN,
                STATUS_TEXT_Y,
                text,
                budget,
                CHROME_TEXT,
            );
        }
    }

    fn viewport_damage(
        &self,
        x: usize,
        document_y: u32,
        width: usize,
        height: u32,
    ) -> Option<ViewportDamage> {
        let document_bottom = document_y.saturating_add(height);
        let viewport_bottom = self.page.scroll_y.saturating_add(VIEWPORT_HEIGHT as u32);
        let visible_top = document_y.max(self.page.scroll_y);
        let visible_bottom = document_bottom.min(viewport_bottom);
        if visible_top >= visible_bottom {
            return None;
        }
        let x = x.min(WIDTH);
        let right = x.saturating_add(width).min(WIDTH);
        (x < right).then_some(ViewportDamage {
            x,
            y: VIEWPORT_TOP + (visible_top - self.page.scroll_y) as usize,
            width: right - x,
            height: (visible_bottom - visible_top) as usize,
        })
    }

    fn focus_damage_rects(&self, target: FocusTarget) -> Result<Vec<ViewportDamage>, Error> {
        let mut rects = Vec::new();
        match target {
            FocusTarget::Control(control) => {
                if let Some(item) = self
                    .page
                    .layout
                    .controls()
                    .iter()
                    .find(|item| item.control == control)
                    && let Some(rect) = self.viewport_damage(
                        MARGIN + item.x as usize,
                        item.y,
                        item.width as usize,
                        item.height as u32,
                    )
                {
                    memory::push(&mut rects, rect)?;
                }
            }
            FocusTarget::Link(link) => {
                for line in self.page.layout.lines() {
                    for piece in self.page.layout.pieces(line) {
                        if piece.link == Some(link)
                            && let Some(rect) = self.viewport_damage(
                                MARGIN + line.x as usize + piece.x as usize,
                                line.y,
                                piece.width as usize
                                    + italic_overhang(piece.style, line.scale as usize),
                                line.height as u32,
                            )
                        {
                            memory::push(&mut rects, rect)?;
                        }
                    }
                }
                for image in self.page.layout.images() {
                    if image.link == Some(link)
                        && let Some(rect) = self.viewport_damage(
                            MARGIN + image.x as usize,
                            image.y,
                            image.width as usize,
                            image.height as u32,
                        )
                    {
                        memory::push(&mut rects, rect)?;
                    }
                }
            }
        }
        Ok(rects)
    }

    /// Reuses the ordinary viewport renderer under a rectangle clip. This
    /// preserves one source of truth for link/control appearance while
    /// avoiding both the full background clear and the full writeback on a
    /// focus move.
    fn draw_focus_damage(
        &mut self,
        framebuffer: &mut Framebuffer,
        service: &mut dyn FnMut(),
        target: FocusTarget,
    ) -> bool {
        let Ok(rects) = self.focus_damage_rects(target) else {
            let height = self.draw_viewport(framebuffer, service);
            return flush_viewport(framebuffer, height, service);
        };
        let mut ok = true;
        for rect in rects {
            let old_horizontal =
                framebuffer.set_horizontal_clip(rect.x, rect.x.saturating_add(rect.width));
            let old_vertical =
                framebuffer.set_vertical_clip(rect.y, rect.y.saturating_add(rect.height));
            self.draw_viewport(framebuffer, service);
            framebuffer.set_vertical_clip(old_vertical.0, old_vertical.1);
            framebuffer.set_horizontal_clip(old_horizontal.0, old_horizontal.1);
            ok &= framebuffer.flush_rect(rect.x, rect.y, rect.width, rect.height);
        }
        ok
    }

    fn draw_control_damage(
        &mut self,
        framebuffer: &mut Framebuffer,
        service: &mut dyn FnMut(),
        damage: ControlDamage,
    ) -> bool {
        let Some(control) = damage.control else {
            return true;
        };
        let Some(item) = self
            .page
            .layout
            .controls()
            .iter()
            .find(|item| item.control == control)
            .copied()
        else {
            return true;
        };
        let Some(vertical) = self.viewport_damage(
            MARGIN + item.x as usize,
            item.y,
            item.width as usize,
            item.height as u32,
        ) else {
            return true;
        };
        let mut ok = true;
        for rect in damage.rects {
            if rect.width == 0 {
                continue;
            }
            let old_horizontal =
                framebuffer.set_horizontal_clip(rect.x, rect.x.saturating_add(rect.width));
            let old_vertical = framebuffer
                .set_vertical_clip(vertical.y, vertical.y.saturating_add(vertical.height));
            self.draw_viewport(framebuffer, service);
            framebuffer.set_vertical_clip(old_vertical.0, old_vertical.1);
            framebuffer.set_horizontal_clip(old_horizontal.0, old_horizontal.1);
            ok &= framebuffer.flush_rect(rect.x, vertical.y, rect.width, vertical.height);
        }
        ok
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
        // A focus-only repaint installs a smaller outer clip before calling
        // this same renderer. Intersect it with the viewport instead of
        // replacing it, so all paint (including the background erase) stays
        // inside the changed object's rectangle.
        let inherited_clip = framebuffer.set_vertical_clip(VIEWPORT_TOP, VIEWPORT_BOTTOM);
        framebuffer.set_vertical_clip(
            inherited_clip.0.max(VIEWPORT_TOP),
            inherited_clip.1.min(VIEWPORT_BOTTOM),
        );
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
            framebuffer.set_vertical_clip(inherited_clip.0, inherited_clip.1);
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
        for control_box in self.page.layout.controls() {
            let bottom = control_box.y.saturating_add(control_box.height as u32);
            let viewport_bottom = top.saturating_add(VIEWPORT_HEIGHT as u32);
            if bottom <= top || control_box.y >= viewport_bottom {
                continue;
            }
            let Some(control) = self
                .page
                .document
                .controls()
                .get(control_box.control as usize)
            else {
                continue;
            };
            let screen_x = MARGIN + control_box.x as usize;
            // Keep the control's original screen position when its top has
            // scrolled above the viewport.  `saturating_sub` on document
            // coordinates would instead pin the whole box to VIEWPORT_TOP,
            // repainting the clipped-away portion over newer content.  A
            // visible control can be above `top` by less than its own bounded
            // height, so this subtraction remains within the chrome above
            // the viewport; the active framebuffer clip then discards it.
            let screen_y = if control_box.y >= top {
                VIEWPORT_TOP + (control_box.y - top) as usize
            } else {
                VIEWPORT_TOP.saturating_sub((top - control_box.y) as usize)
            };
            if control.kind.is_checkable() {
                let checked = self
                    .page
                    .control_checked
                    .get(control_box.control as usize)
                    .copied()
                    .unwrap_or(control.checked);
                let ink = if control.disabled {
                    DISABLED_COLOR
                } else {
                    TEXT_COLOR
                };
                let extent = control_box.width.min(control_box.height) as usize;
                let size = extent.saturating_sub(12);
                let center_x = screen_x + control_box.width as usize / 2;
                let center_y = screen_y + control_box.height as usize / 2;
                let radius = size / 2;
                if self.focused_control() == Some(control_box.control) {
                    // Checkables do not use the line-box-sized control frame:
                    // it reads as a second checkbox, and makes a radio look
                    // square.  Keep the hit box unchanged, but put a two-pixel
                    // focus halo around the actual symbol with a clear gap.
                    let max_focus_radius = extent.saturating_sub(2) / 2;
                    let mut previous_radius = radius;
                    for desired_radius in [radius.saturating_add(3), radius.saturating_add(4)] {
                        let focus_radius = desired_radius.min(max_focus_radius);
                        if focus_radius <= previous_radius {
                            continue;
                        }
                        previous_radius = focus_radius;
                        if control.kind == ControlKind::Radio {
                            framebuffer.draw_circle(center_x, center_y, focus_radius, LINK_COLOR);
                        } else {
                            framebuffer.stroke_rect(
                                center_x.saturating_sub(focus_radius),
                                center_y.saturating_sub(focus_radius),
                                focus_radius.saturating_mul(2),
                                focus_radius.saturating_mul(2),
                                LINK_COLOR,
                            );
                        }
                    }
                }
                if control.kind == ControlKind::Radio {
                    framebuffer.draw_circle(center_x, center_y, radius, ink);
                    if checked {
                        framebuffer.fill_circle(center_x, center_y, size / 4, ink);
                    }
                } else {
                    let left = center_x.saturating_sub(radius);
                    let top = center_y.saturating_sub(radius);
                    framebuffer.stroke_rect(left, top, size, size, ink);
                    if checked {
                        framebuffer.fill_rect(
                            left + 3,
                            top + 3,
                            size.saturating_sub(6),
                            size.saturating_sub(6),
                            ink,
                        );
                    }
                }
                continue;
            }
            let face = if control.kind == ControlKind::Submit {
                if self.focused_control() == Some(control_box.control) {
                    LINK_COLOR
                } else {
                    CHROME_BACKGROUND
                }
            } else {
                EDIT_BACKGROUND
            };
            framebuffer.fill_rect(
                screen_x,
                screen_y,
                control_box.width as usize,
                control_box.height as usize,
                face,
            );
            framebuffer.stroke_rect(
                screen_x,
                screen_y,
                control_box.width as usize,
                control_box.height as usize,
                if self.focused_control() == Some(control_box.control) {
                    LINK_COLOR
                } else if control.disabled {
                    DISABLED_COLOR
                } else {
                    RULE_COLOR
                },
            );
            let text_left = screen_x + 6;
            let text_y = screen_y + 4;
            let text_budget = control_box.width.saturating_sub(12) as usize;
            if control.kind == ControlKind::Select {
                let ink = if control.disabled {
                    DISABLED_COLOR
                } else {
                    TEXT_COLOR
                };
                let first = control.first_option as usize;
                let style = crate::font::UiTextStyle::BODY;
                let end = text_left + text_budget.saturating_sub(18);
                let mut cursor = text_left;
                let mut any = false;
                for (offset, option) in self
                    .page
                    .document
                    .control_options(control)
                    .iter()
                    .enumerate()
                {
                    let selected = self
                        .page
                        .option_selected
                        .get(first + offset)
                        .copied()
                        .unwrap_or(option.selected);
                    if !selected || cursor >= end {
                        continue;
                    }
                    if any {
                        draw_clipped(framebuffer, cursor, text_y, ", ", end - cursor, ink);
                        cursor += crate::font::ui_text_width(", ", style);
                        if cursor >= end {
                            continue;
                        }
                    }
                    draw_clipped(
                        framebuffer,
                        cursor,
                        text_y,
                        &option.label,
                        end - cursor,
                        ink,
                    );
                    cursor += crate::font::ui_text_width(&option.label, style);
                    any = true;
                }
                if !any {
                    draw_clipped(
                        framebuffer,
                        text_left,
                        text_y,
                        "(none)",
                        end - text_left,
                        DISABLED_COLOR,
                    );
                }
                let arrow_x = screen_x + control_box.width as usize - 18;
                let arrow_y = text_y + 5;
                for step in 0..5 {
                    framebuffer.fill_rect(arrow_x + step, arrow_y + step, 10 - 2 * step, 1, ink);
                }
                continue;
            }
            if control.kind == ControlKind::Textarea {
                let editing = self
                    .form_editing
                    .as_ref()
                    .filter(|editing| editing.control == control_box.control);
                let value = match editing {
                    Some(editing) => editing.input.text(),
                    None => self
                        .page
                        .control_values
                        .get(control_box.control as usize)
                        .map(String::as_str)
                        .unwrap_or(&control.initial_value),
                };
                let visible = usize::from(control.rows.max(1));
                draw_textarea(
                    framebuffer,
                    value,
                    editing,
                    visible,
                    text_left,
                    text_y,
                    text_budget,
                    (control_box.height as usize).saturating_sub(8) / visible,
                    if control.disabled {
                        DISABLED_COLOR
                    } else {
                        TEXT_COLOR
                    },
                );
                continue;
            }
            if let Some(editing) = self
                .form_editing
                .as_ref()
                .filter(|editing| editing.control == control_box.control)
            {
                let first = editing
                    .input
                    .visible_start(text_budget.saturating_sub(2), |text| {
                        crate::font::ui_text_width(text, crate::font::UiTextStyle::BODY)
                    });
                if let Some(selection) = editing.input.selection() {
                    let visible_start = selection.start.max(first);
                    let visible_end = selection.end.max(first);
                    if visible_start < visible_end {
                        let selection_x = crate::font::ui_text_width(
                            &editing.input.text()[first..visible_start],
                            crate::font::UiTextStyle::BODY,
                        )
                        .min(text_budget);
                        let selection_width = crate::font::ui_text_width(
                            &editing.input.text()[visible_start..visible_end],
                            crate::font::UiTextStyle::BODY,
                        )
                        .min(text_budget.saturating_sub(selection_x));
                        framebuffer.fill_rect(
                            text_left + selection_x,
                            text_y,
                            selection_width,
                            CELL_HEIGHT,
                            EDIT_SELECTION,
                        );
                    }
                }
                draw_clipped(
                    framebuffer,
                    text_left,
                    text_y,
                    &editing.input.text()[first..],
                    text_budget,
                    TEXT_COLOR,
                );
                let caret_x = crate::font::ui_text_width(
                    &editing.input.text()[first..editing.input.caret()],
                    crate::font::UiTextStyle::BODY,
                );
                framebuffer.fill_rect(text_left + caret_x, text_y, 2, CELL_HEIGHT, EDIT_CARET);
            } else {
                let value = self
                    .page
                    .control_values
                    .get(control_box.control as usize)
                    .map(String::as_str)
                    .unwrap_or(&control.initial_value);
                if control.kind == ControlKind::Submit && control.button_run_count != 0 {
                    let focused = self.focused_control() == Some(control_box.control);
                    let right = text_left.saturating_add(text_budget);
                    let old_clip = framebuffer.set_horizontal_clip(text_left, right);
                    let button_images: Vec<_> = self
                        .page
                        .document
                        .images()
                        .iter()
                        .enumerate()
                        .filter(|(_, image)| image.button == Some(control_box.control))
                        .collect();
                    let mut x = text_left;
                    for run in self.page.document.button_runs(control) {
                        let mut cursor = run.start as usize;
                        for (image_index, image) in &button_images {
                            let offset = image.text_offset as usize;
                            if offset < cursor || offset >= run.end as usize {
                                continue;
                            }
                            if let Some(text) = control.display_label.get(cursor..offset) {
                                let ink = if focused {
                                    WHITE
                                } else if control.disabled {
                                    DISABLED_COLOR
                                } else {
                                    piece_color(run.style, false)
                                };
                                draw_text_run(
                                    framebuffer,
                                    x,
                                    text_y,
                                    text,
                                    1,
                                    ink,
                                    run.style & STYLE_BOLD != 0,
                                    run.style & STYLE_CODE != 0,
                                    run.style & STYLE_ITALIC != 0,
                                );
                                x = x.saturating_add(crate::font::ui_text_width(
                                    text,
                                    crate::font::UiTextStyle::new(
                                        if run.style & STYLE_CODE != 0 {
                                            crate::font::UiFace::Mono
                                        } else {
                                            crate::font::UiFace::Sans
                                        },
                                        16,
                                    ),
                                ));
                            }
                            if let Some(box_) = self
                                .page
                                .layout
                                .images()
                                .iter()
                                .find(|box_| box_.image as usize == *image_index)
                            {
                                x = x.saturating_add(box_.width as usize);
                            }
                            cursor = offset;
                        }
                        if let Some(text) = control.display_label.get(cursor..run.end as usize) {
                            let ink = if focused {
                                WHITE
                            } else if control.disabled {
                                DISABLED_COLOR
                            } else {
                                piece_color(run.style, false)
                            };
                            draw_text_run(
                                framebuffer,
                                x,
                                text_y,
                                text,
                                1,
                                ink,
                                run.style & STYLE_BOLD != 0,
                                run.style & STYLE_CODE != 0,
                                run.style & STYLE_ITALIC != 0,
                            );
                            x = x.saturating_add(crate::font::ui_text_width(
                                text,
                                crate::font::UiTextStyle::new(
                                    if run.style & STYLE_CODE != 0 {
                                        crate::font::UiFace::Mono
                                    } else {
                                        crate::font::UiFace::Sans
                                    },
                                    16,
                                ),
                            ));
                        }
                    }
                    framebuffer.set_horizontal_clip(old_clip.0, old_clip.1);
                    continue;
                }
                let shown = if control.kind == ControlKind::Submit && control.button_element {
                    control.display_label.as_str()
                } else if control.kind == ControlKind::Submit && !control.display_label.is_empty() {
                    control.display_label.as_str()
                } else if control.kind == ControlKind::Submit && value.is_empty() {
                    "Submit"
                } else {
                    value
                };
                draw_clipped(
                    framebuffer,
                    text_left,
                    text_y,
                    shown,
                    text_budget,
                    if self.focused_control() == Some(control_box.control)
                        && control.kind == ControlKind::Submit
                    {
                        WHITE
                    } else if control.disabled {
                        DISABLED_COLOR
                    } else {
                        TEXT_COLOR
                    },
                );
            }
        }
        let focused = self.focused_link();
        for image_box in self.page.layout.images() {
            let bottom = image_box.y.saturating_add(image_box.height as u32);
            let viewport_bottom = top.saturating_add(VIEWPORT_HEIGHT as u32);
            if bottom <= top || image_box.y >= viewport_bottom {
                continue;
            }
            let screen_x = MARGIN + image_box.x as usize;
            let visible_top = image_box.y.max(top);
            let screen_y = VIEWPORT_TOP + (visible_top - top) as usize;
            let visible_height = bottom.min(viewport_bottom) - visible_top;
            let selected = image_box.link.is_some() && image_box.link == focused;
            let in_button = image_box.button.is_some();
            let background = if selected {
                LINK_COLOR
            } else {
                CHROME_BACKGROUND
            };
            let foreground = if selected
                || image_box
                    .button
                    .is_some_and(|control| self.focused_control() == Some(control))
            {
                WHITE
            } else {
                TEXT_COLOR
            };
            if !in_button {
                framebuffer.fill_rect(
                    screen_x,
                    screen_y,
                    image_box.width as usize,
                    visible_height as usize,
                    background,
                );
            }
            let decoded = self
                .page
                .decoded_images
                .get(image_box.image as usize)
                .and_then(Option::as_ref);
            if let Some(decoded) = decoded {
                framebuffer.blit_rgb565_scaled(
                    screen_x,
                    screen_y,
                    decoded.width as usize,
                    decoded.height as usize,
                    image_box.width as usize,
                    image_box.height as usize,
                    &decoded.pixels,
                    (visible_top - image_box.y) as usize,
                );
            }
            if !in_button && image_box.y >= top {
                framebuffer.stroke_rect(
                    screen_x,
                    screen_y,
                    image_box.width as usize,
                    image_box.height as usize,
                    if image_box.link.is_some() {
                        LINK_COLOR
                    } else {
                        RULE_COLOR
                    },
                );
            }
            let alt = self.page.image_failures[image_box.image as usize].unwrap_or_else(|| {
                self.page
                    .document
                    .images()
                    .get(image_box.image as usize)
                    .map(|image| image.alt.as_str())
                    .filter(|alt| !alt.is_empty())
                    .unwrap_or("image pending")
            });
            if decoded.is_none() && image_box.y >= top {
                framebuffer.draw_gui_text_clipped(
                    screen_x.saturating_add(4),
                    screen_y.saturating_add(4),
                    alt,
                    image_box.width.saturating_sub(8) as usize,
                    1,
                    foreground,
                    None,
                );
            }
        }
        let visible = self.page.layout.visible(top, VIEWPORT_HEIGHT as u32);
        for (drawn, line) in self.page.layout.lines()[visible].iter().enumerate() {
            if drawn % LINES_PER_SERVICE == 0 && drawn > 0 {
                service();
            }
            let screen_y = (VIEWPORT_TOP as i64 + line.y as i64 - top as i64) as usize;
            self.draw_line(framebuffer, line, screen_y, focused);
        }
        let popup_bottom = self.draw_select_popup(framebuffer, top);
        // A list reaching below the page's last line must still be erased
        // by the repaint that closes it.
        self.painted_bottom = self.painted_bottom.max(popup_bottom);
        framebuffer.set_vertical_clip(inherited_clip.0, inherited_clip.1);
        height
    }

    /// Draws the open select list on top of the page. Returns its screen
    /// bottom, or zero when none is open.
    fn draw_select_popup(&self, framebuffer: &mut Framebuffer, top: u32) -> usize {
        let Some(popup) = &self.select_popup else {
            return 0;
        };
        let Some(control) = self.page.document.controls().get(popup.control as usize) else {
            return 0;
        };
        let options = self.page.document.control_options(control);
        let first = control.first_option as usize;
        let width = popup.width as usize;
        let height = popup_height(popup.rows);
        let screen_x = MARGIN + popup.x as usize;
        let screen_y = if popup.y >= top {
            VIEWPORT_TOP + (popup.y - top) as usize
        } else {
            VIEWPORT_TOP.saturating_sub((top - popup.y) as usize)
        };
        framebuffer.fill_rect(screen_x, screen_y, width, height, EDIT_BACKGROUND);
        framebuffer.stroke_rect(screen_x, screen_y, width, height, LINK_COLOR);
        for row in 0..popup.rows {
            let index = popup.top + row;
            let Some(option) = options.get(index) else {
                break;
            };
            let y = screen_y + 1 + row * POPUP_ROW_HEIGHT;
            let highlighted = index == popup.highlight;
            if highlighted {
                framebuffer.fill_rect(
                    screen_x + 1,
                    y,
                    width.saturating_sub(2),
                    POPUP_ROW_HEIGHT,
                    LINK_COLOR,
                );
            }
            let ink = if highlighted {
                WHITE
            } else if option.disabled {
                DISABLED_COLOR
            } else {
                TEXT_COLOR
            };
            let selected = self
                .page
                .option_selected
                .get(first + index)
                .copied()
                .unwrap_or(option.selected);
            let mark_x = screen_x + 8;
            let mark_y = y + (POPUP_ROW_HEIGHT - 10) / 2;
            if control.multiple {
                framebuffer.stroke_rect(mark_x, mark_y, 10, 10, ink);
                if selected {
                    framebuffer.fill_rect(mark_x + 2, mark_y + 2, 6, 6, ink);
                }
            } else if selected {
                framebuffer.fill_circle(mark_x + 5, mark_y + 5, 4, ink);
            }
            draw_clipped(
                framebuffer,
                screen_x + 26,
                y + 4,
                &option.label,
                width.saturating_sub(40),
                ink,
            );
        }
        if options.len() > popup.rows {
            let track = height.saturating_sub(2);
            let thumb_y = screen_y + 1 + popup.top * track / options.len();
            let thumb = (popup.rows * track / options.len()).max(4);
            framebuffer.fill_rect(screen_x + width - 6, thumb_y, 4, thumb, RULE_COLOR);
        }
        screen_y + height
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
        VIEWPORT_TOP
            + self
                .page
                .layout
                .height()
                .saturating_sub(top)
                .min(VIEWPORT_HEIGHT as u32) as usize
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
                let focus_width =
                    piece.width as usize + italic_overhang(piece.style, line.scale as usize);
                framebuffer.fill_rect(x, screen_y, focus_width, line.height as usize, LINK_COLOR);
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
                piece.style & STYLE_ITALIC != 0,
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
    // apply at once: code is a different kind of text. Bold and italic are
    // shape changes handled by `draw_text_run`.
    if style & STYLE_CODE != 0 {
        CODE_COLOR
    } else {
        TEXT_COLOR
    }
}

fn italic_overhang(style: u8, scale: usize) -> usize {
    if style & STYLE_ITALIC == 0 {
        return 0;
    }
    tab5_ui_font::oblique_overhang(CELL_HEIGHT.saturating_mul(scale))
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
    italic: bool,
) {
    let face = if mono {
        crate::font::UiFace::Mono
    } else {
        crate::font::UiFace::Sans
    };
    let style = crate::font::UiTextStyle::new(face, if scale >= 2 { 32 } else { 16 });
    let draw = |framebuffer: &mut Framebuffer, x| {
        if italic {
            framebuffer.draw_ui_text_oblique(x, y, text, style, color, None);
        } else {
            framebuffer.draw_ui_text(x, y, text, style, color, None);
        }
    };
    draw(framebuffer, x);
    if bold {
        // Struck twice, one pixel apart. The font has one weight, so bold
        // has to be synthesised or dropped -- and dropping it means `<b>`
        // renders as nothing at all. One physical pixel rather than one
        // glyph pixel (`scale`): at scale 2 a full-cell offset would smear
        // into the next column.
        draw(framebuffer, x + 1);
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

/// A textarea's text: wrapped rows from the edit's scroll position, with the
/// selection and caret when it is being edited.
#[allow(clippy::too_many_arguments)]
fn draw_textarea(
    framebuffer: &mut Framebuffer,
    text: &str,
    editing: Option<&FormEditing>,
    visible: usize,
    left: usize,
    top: usize,
    budget: usize,
    row_height: usize,
    ink: u16,
) {
    let measure = |text: &str| crate::font::ui_text_width(text, crate::font::UiTextStyle::BODY);
    let Ok(rows) = text_input::wrap_rows(text, budget.saturating_sub(2), measure) else {
        return;
    };
    let first = editing.map_or(0, |editing| {
        editing.top_row.min(rows.len().saturating_sub(1))
    });
    let caret = editing.map(|editing| editing.input.caret());
    let caret_row = caret.map(|caret| text_input::row_of(&rows, caret));
    let selection = editing.and_then(|editing| editing.input.selection());
    for (index, row) in rows.iter().enumerate().skip(first).take(visible) {
        let y = top + (index - first) * row_height;
        if let Some(selection) = &selection {
            let start = selection.start.clamp(row.start, row.end);
            let end = selection.end.clamp(row.start, row.end);
            if start < end {
                let x = measure(&text[row.start..start]).min(budget);
                let width = measure(&text[start..end]).min(budget - x);
                framebuffer.fill_rect(left + x, y, width, CELL_HEIGHT, EDIT_SELECTION);
            }
        }
        draw_clipped(framebuffer, left, y, &text[row.clone()], budget, ink);
        if let (Some(caret), Some(caret_row)) = (caret, caret_row)
            && caret_row == index
        {
            let x = measure(&text[row.start..caret.clamp(row.start, row.end)]).min(budget);
            framebuffer.fill_rect(left + x, y, 2, CELL_HEIGHT, EDIT_CARET);
        }
    }
}

/// The resend question and its two buttons, across the status line.
fn draw_confirmation(framebuffer: &mut Framebuffer, confirmation: &Confirmation) {
    let url = &confirmation.submission.request.url;
    let mut text = Summary::new();
    text.push(match confirmation.reason {
        ConfirmReason::Resend => "Resend POST to ",
        ConfirmReason::Unknown => "POST result unknown. Resend to ",
        ConfirmReason::NotSent => "POST was not sent. Send to ",
        ConfirmReason::Redirect => "Server redirects the POST body to ",
    });
    text.push(url.scheme().as_str());
    text.push("://");
    text.push(url.host());
    if !url.has_default_port() {
        text.push(":");
        text.push_usize(url.port() as usize);
    }
    text.push("?");
    draw_clipped(
        framebuffer,
        MARGIN,
        STATUS_TEXT_Y,
        text.as_str(),
        CONFIRM_SEND_LEFT - 8 - MARGIN,
        MESSAGE_COLOR,
    );
    let style = crate::font::UiTextStyle::BODY;
    for (left, label, face, ink) in [
        (
            CONFIRM_SEND_LEFT,
            "Send (y)",
            theme::ACCENT,
            theme::ON_ACCENT,
        ),
        (CONFIRM_CANCEL_LEFT, "Cancel (n)", WHITE, CHROME_TEXT),
    ] {
        framebuffer.fill_rect(
            left,
            VIEWPORT_BOTTOM + 3,
            CONFIRM_BUTTON_WIDTH,
            STATUS_HEIGHT - 6,
            face,
        );
        let width = crate::font::ui_text_width(label, style);
        framebuffer.draw_ui_text(
            left + CONFIRM_BUTTON_WIDTH.saturating_sub(width) / 2,
            STATUS_TEXT_Y,
            label,
            style,
            ink,
            None,
        );
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
    let order = focus_order(&document, &layout)?;
    let visit_url = document.url().clone();
    let decoded_images = core::iter::repeat_with(|| None)
        .take(document.images().len())
        .collect();
    let image_failures = core::iter::repeat_n(None, document.images().len()).collect();
    let mut control_values = Vec::new();
    for control in document.controls() {
        memory::push(
            &mut control_values,
            memory::string_from(&control.initial_value)?,
        )?;
    }
    let mut control_checked = Vec::new();
    for control in document.controls() {
        memory::push(&mut control_checked, control.checked)?;
    }
    let mut option_selected = Vec::new();
    for option in document.options() {
        memory::push(&mut option_selected, option.selected)?;
    }
    Ok(Page {
        document,
        visit_url,
        request_method: RequestMethod::Get,
        security: None,
        error: false,
        layout,
        scroll_y: 0,
        // Nothing is focused until the reader asks: an automatically
        // focused first link looks like something is already selected, and
        // `Enter` would then follow a link nobody chose.
        focus: None,
        order,
        decoded_images,
        image_failures,
        control_values,
        control_checked,
        option_selected,
        post: None,
    })
}

/// Interleaves links and enabled visible controls by their laid-out
/// position.  IDs remain owned by Document/Layout; this is only the small
/// navigation list that Tab walks.
fn focus_order(document: &Document, layout: &Layout) -> Result<Vec<FocusTarget>, Error> {
    let mut positioned: Vec<(u32, u16, usize, FocusTarget)> = Vec::new();
    for (sequence, link) in layout.link_order()?.into_iter().enumerate() {
        let (mut y, x) = layout.position_of_link(link).unwrap_or((0, 0));
        if let Some(line) = layout
            .line_of_link(link)
            .and_then(|line| layout.lines().get(line))
        {
            for control in layout.controls() {
                if control.y < line.y.saturating_add(line.height as u32)
                    && control.y.saturating_add(control.height as u32) > line.y
                {
                    y = y.min(control.y);
                }
            }
        }
        memory::push(&mut positioned, (y, x, sequence, FocusTarget::Link(link)))?;
    }
    let link_count = positioned.len();
    for control_box in layout.controls() {
        let Some(control) = document.controls().get(control_box.control as usize) else {
            continue;
        };
        if control.disabled
            || !matches!(
                control.kind,
                ControlKind::Text
                    | ControlKind::Textarea
                    | ControlKind::Select
                    | ControlKind::Submit
                    | ControlKind::Checkbox
                    | ControlKind::Radio
            )
        {
            continue;
        }
        memory::push(
            &mut positioned,
            (
                control_box.y,
                control_box.x,
                link_count + control_box.control as usize,
                FocusTarget::Control(control_box.control),
            ),
        )?;
    }
    positioned.sort_unstable_by_key(|item| (item.0, item.1, item.2));
    let mut order = Vec::new();
    for (_, _, _, target) in positioned {
        memory::push(&mut order, target)?;
    }
    Ok(order)
}

fn load_builtin(page: &builtin::Page) -> Result<Page, Error> {
    let url = Url::parse(page.url)?;
    let mut parser = Parser::new(url)?;
    page.write(&mut parser)?;
    let mut loaded = build_page(parser.finish()?)?;
    // Built-in pages are `const` references and may be promoted at distinct
    // addresses at different use sites, so identity must be their stable URL.
    if page.url == builtin::IMAGES.url {
        let decoded = decode_builtin_png().map(Rc::new);
        for slot in &mut loaded.decoded_images {
            *slot = decoded.clone();
        }
    }
    Ok(loaded)
}

fn decode_builtin_png() -> Option<DecodedImage> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(&png_chunk_crc(kind, data).to_be_bytes());
    };
    chunk(b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 2, 8, 6, 0, 0, 0]);
    chunk(
        b"IDAT",
        &[
            0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x0c, 0x81, 0x34, 0x08, 0x30, 0x00,
            0x00, 0x48, 0xc9, 0x08, 0xf8,
        ],
    );
    chunk(b"IEND", &[]);
    decode(&bytes).ok()
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
        TableDocument,
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
                Body::TableDocument => write_table(parser),
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

    fn write_table(parser: &mut Parser) -> Result<(), Error> {
        parser.feed(
            b"<title>table acceptance</title><h1>Table acceptance</h1>\
              <p>This page is deliberately taller than the viewport. Scroll through the table and check partially visible text, backgrounds, rules, and links at both edges.</p>\
              <table border='1'><caption>Browser table fixture (border=1)</caption>\
              <thead><tr><th>Row</th><th>ASCII / Japanese</th><th>Long value</th></tr></thead><tbody>",
        )?;
        for row in 1..=18 {
            parser.feed(b"<tr><td>")?;
            feed_decimal(parser, row)?;
            parser.feed(b"</td><td>scroll checkpoint ")?;
            parser.feed("日本語".as_bytes())?;
            parser.feed(b"</td><td>This deliberately long cell wraps inside its column so that each table row has enough height for pixel clipping.</td></tr>")?;
        }
        parser.feed(
            b"<tr><th rowspan='2'>rowspan header</th><td colspan='2'>A colspan cell with a <a href='/'>link back home</a>.</td></tr>\
              <tr><td>left after rowspan</td><td>right after rowspan</td></tr>\
              <tr><td></td><td colspan='2' rowspan='2'>Both spans: this text wraps while occupying two columns and two rows.</td></tr>\
              <tr><td>empty-neighbour</td></tr>\
              <tr><td rowspan='0'>zero means one</td><td colspan='oops'>invalid means one</td><td>omitted end tags\
              </tbody></table>\
              <h2>No border, many narrow columns</h2><table border='0'><tr><th>A</th><th>B</th><th>C</th><th>D</th><th>E</th><th>F</th><th>G</th><th>H</th></tr>\
              <tr><td>alpha wraps</td><td>bravo wraps</td><td>Japanese</td><td>delta</td><td>echo echo</td><td>foxtrot</td><td>golf</td><td>hotel</td></tr></table>\
              <p><a href='/'>Home</a></p>",
        )
    }

    fn feed_decimal(parser: &mut Parser, mut value: usize) -> Result<(), Error> {
        let mut digits = [0u8; 20];
        let mut position = digits.len();
        loop {
            position -= 1;
            digits[position] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                return parser.feed(&digits[position..]);
            }
        }
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
             is no CSS, no JavaScript and no image download or decoding; img \
             elements reserve an outlined region, and an HTTPS connection \
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
             <li><b>Up</b> and <b>Down</b> scroll 20 pixels, <b>Page Up</b> and \
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
             <li><a href=\"/italic\">Italic acceptance: headings and hollow boxes</a></li>\
             <li><a href=\"/table\">Table layout acceptance page</a></li>\
             <li><a href=\"/images\">Image layout acceptance page</a></li>\
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
             <p>Images currently reserve outlined regions rather than being \
             downloaded: <img src=\"x.png\" alt=\"a red square\"> and one \
             without alt text: <img src=\"y.png\">.</p>\
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

    /// Synthetic italic next to upright text, for the paths the oblique
    /// painter takes: A4 Latin and Japanese at body and heading size, and
    /// the fixed-cell hollow boxes drawn for characters no font covers.
    ///
    /// The boxes are sheared band by band under a narrowed vertical clip, so
    /// they are shown at both 16 and 32 pixels, beside A4 glyphs whose slant
    /// they must match, and at the start of a run where a lone combining
    /// mark becomes a box too.
    pub const ITALIC: &Page = &Page {
        url: "http://built-in/italic",
        body: Body::Fixed(
            "<title>italic acceptance</title>\
             <h1>Upright H|l <em>Italic H|l</em></h1>\
             <h1>Box <em>\u{1F600}\u{20BB7}\u{FDFD}</em> end</h1>\
             <h1><em>A\u{1F600}B</em> <em>\u{65E5}\u{20BB7}\u{672C}</em></h1>\
             <h2>\u{65E5}\u{672C}\u{8A9E} <em>\u{65E5}\u{672C}\u{8A9E}\u{306E}\u{659C}\u{4F53}</em></h2>\
             <h3>Small heading <em>italic, double struck \u{1F600}</em></h3>\
             <h2>Body text</h2>\
             <p>Upright: H|l ||||| \u{65E5}\u{672C}\u{8A9E}</p>\
             <p>Italic: <em>H|l ||||| \u{65E5}\u{672C}\u{8A9E}</em></p>\
             <p>Upright boxes: \u{1F600}\u{20BB7}\u{FDFD} | \
             italic boxes: <em>\u{1F600}\u{20BB7}\u{FDFD}</em></p>\
             <p>Mixed: <em>A\u{1F600}B \u{65E5}\u{20BB7}\u{672C} |\u{FDFD}|</em> -- the box slant \
             must continue the letters on either side.</p>\
             <p>Leading combining marks: <em>\u{3099}x</em> <em>\u{301}y</em> -- \
             each starts its run and becomes a slanted box.</p>\
             <p>Combined: <em>\u{304B}\u{3099} e\u{301}</em></p>\
             <p>Bold italic: <b><em>Bold H|l \u{1F600} \u{65E5}\u{672C}</em></b></p>\
             <p>Code italic: <code><em>mono H|l \u{1F600}</em></code></p>\
             <h2>Links</h2>\
             <p>Tab through these and back; no blue or ink may remain at the \
             slanted right edge.</p>\
             <p><a href='/'><em>italic link \u{1F600}</em></a>next \
             <a href='/'><em>\u{20BB7}\u{FDFD}</em></a>|</p>\
             <h1><a href='/'><em>Heading link \u{1F600}</em></a>|</h1>\
             <h2>Table</h2>\
             <table border='1'><tr><th><em>Header \u{1F600}</em></th>\
             <th>Upright \u{1F600}</th></tr>\
             <tr><td><em>Cell H|l \u{20BB7}</em></td><td>Cell H|l \u{20BB7}</td></tr></table>\
             <h2>Wrapping</h2>\
             <p><em>This italic paragraph is long enough to wrap at the right \
             margin, with boxes \u{1F600}\u{20BB7}\u{FDFD} and \u{65E5}\u{672C}\u{8A9E} \
             spread through it, so the slant can be checked against the \
             margin and against the line below it. Keep reading past the \
             edge of the screen \u{1F600} to see the next line start.</em></p>\
             <p><a href='/'>Home</a></p>",
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
        body: Body::TableDocument,
    };

    /// Flash-resident Stage 2 acceptance page. The sources deliberately do
    /// not exist yet: this stage verifies reserved geometry and interaction
    /// before image fetching and decoding are introduced.
    pub const IMAGES: &Page = &Page {
        url: "http://built-in/images",
        body: Body::Fixed(
            "<title>image layout acceptance</title><h1>Image layout acceptance</h1>\
             <p>Each outlined region is a pending image. Their labels and sizes exercise all four width/height cases.</p>\
             <h2>Both dimensions: 320 by 180</h2><img src='/missing-both.png' alt='both 320 x 180' width='320' height='180'>\
             <h2>Width only: 200 by default 90</h2><img src='/missing-width.png' alt='width only 200 x 90' width='200'>\
             <h2>Height only: default 160 by 60</h2><img src='/missing-height.png' alt='height only 160 x 60' height='60'>\
             <h2>No dimensions: default 160 by 90</h2><img src='/missing-default.png' alt='default 160 x 90'>\
             <h2>Too wide: proportional shrink</h2><img src='/missing-wide.png' alt='wide 1280 x 400 shrunk to content width' width='1280' height='400'>\
             <h2>Linked image</h2><p>Tab selects the next outlined region; Enter and touch should return home.</p>\
             <a href='/'><img src='/missing-link.png' alt='linked image back home' width='240' height='80'></a>\
             <h2>Image in a table cell</h2><table border='1'><tr><th>Text cell</th><th>Image cell</th></tr>\
             <tr><td>The row must grow to contain its neighbour.</td><td><img src='/missing-cell.png' alt='cell image' width='300' height='120'></td></tr></table>\
             <p><a href='/'>Home</a></p>",
        ),
    };

    pub const FRAGMENTS: &Page = &Page {
        url: "http://built-in/fragments",
        body: Body::FragmentDocument,
    };

    pub const FORMS: &Page = &Page {
        url: "http://built-in/forms",
        body: Body::Fixed(
            "<title>form layout acceptance</title><h1>Form layout acceptance</h1>\
             <p>Scroll down to bring the controls into the viewport.</p>\
             <p>Before 1: the controls must remain below this paragraph.</p>\
             <p>Before 2: no control border should leak into the status bar.</p>\
             <p>Before 3: scrolling should reveal each control from the bottom edge.</p>\
             <p>Before 4: this deliberately makes the fixture taller than the screen.</p>\
             <p>Before 5: keep scrolling until the Query field appears.</p>\
             <p>Before 6: the visible controls follow this paragraph.</p>\
             <p>Before 7: extra space keeps the controls well below the first viewport.</p>\
             <p>Before 8: every paragraph participates in normal document layout.</p>\
             <p>Before 9: continue scrolling through the upper test region.</p>\
             <p>Before 10: no control pixels should be visible yet.</p>\
             <p>Before 11: the page should move without disturbing browser chrome.</p>\
             <p>Before 12: this line extends the approach to the controls.</p>\
             <p>Before 13: the Query label is still farther down the document.</p>\
             <p>Before 14: partial text lines should clip normally at both edges.</p>\
             <p>Before 15: keep moving toward the form section.</p>\
             <p>Before 16: the control group begins after two more paragraphs.</p>\
             <p>Before 17: this is the penultimate upper spacer paragraph.</p>\
             <p>Before 18: the controls follow immediately after this line.</p>\
             <form action='/forms' method='get'>\
             <p><label for='inline-q'>Inline</label> <input id='inline-q' name='inline' value='same row'> after\
             <select name='inline-select'><option selected>One</option><option>Two</option></select> tail\
             <button name='styled' value='yes'>plain <strong>bold</strong> <em>italic</em> <code>code</code></button>.</p>\
             <p>Button image failure: <button name='image-button' value='tap'>Go <img src='/missing-button.png' alt='missing' width='80' height='40'> now</button> after.</p>\
             <label for='q'>Query</label><input id='q' name='q' value='initial value'>\
             <input type='hidden' name='q' value='hidden duplicate'>\
             <input type='hidden' name='empty' value=''>\
             <input name='off' value='disabled value' disabled>\
             <table border='1'><tr><td><label for='cell-q'>Cell query</label>\
             <input id='cell-q' name='cell' value='inside cell'> after\
             <button name='cell-go' value='yes'><strong>Cell</strong> button</button></td>\
             <td>Neighbour cell</td></tr></table>\
             <label for='date-fallback'>Date fallback</label>\
             <input id='date-fallback' type='date' name='when' value='2026-09-13'>\
             <h2>Checkboxes and radio buttons</h2>\
             <label for='cb-news'>News (checked)</label>\
             <input id='cb-news' type='checkbox' name='topic' value='news' checked>\
             <label for='cb-sport'>Sport</label>\
             <input id='cb-sport' type='checkbox' name='topic' value='sport'>\
             <label for='cb-off'>Disabled but checked</label>\
             <input id='cb-off' type='checkbox' name='topic' value='off' checked disabled>\
             <label for='cb-agree'>No value attribute</label>\
             <input id='cb-agree' type='checkbox' name='agree'>\
             <label for='size-small'>Small (checked first)</label>\
             <input id='size-small' type='radio' name='size' value='small' checked>\
             <label for='size-large'>Large (checked last, wins)</label>\
             <input id='size-large' type='radio' name='size' value='large' checked>\
             <label for='note'>Note (textarea)</label>\
             <textarea id='note' name='note' rows='3'>\nfirst line\nsecond &amp; <b>line</b></textarea>\
             <label for='color'>Color (select)</label>\
             <select id='color' name='color'><option value='red'>Red</option>\
             <option selected>Green</option><option value='blue' disabled>Blue (disabled)</option></select>\
             <label for='tags'>Tags (multiple)</label>\
             <select id='tags' name='tag' multiple><optgroup label='Group'>\
             <option value='a' selected>Alpha</option><option value='b'>Beta</option></optgroup>\
             <optgroup label='Off' disabled><option value='c' selected>Gamma (disabled group)</option>\
             </optgroup><option>Delta &amp; more</option></select>\
             <input type='submit' name='go' value='Search'>\
             <button name='mode' value='advanced'>Apply <strong>changes</strong></button></form>\
             <h2>Textarea scrolling</h2>\
             <form action='/forms' method='get'>\
             <textarea name='long' rows='2'>row 1\nrow 2\nrow 3\nrow 4 is a long line that has to wrap \
             inside the box because it is much wider than three hundred and twenty pixels</textarea>\
             <label for='number'>Number (12 options, list scrolls)</label>\
             <select id='number' name='number'><option>1</option><option>2</option><option>3</option>\
             <option>4</option><option>5</option><option>6</option><option>7</option><option>8</option>\
             <option>9</option><option>10</option><option>11</option><option>12</option></select>\
             <input type='submit' name='go2' value='Send long'></form>\
             <h2>Rejected form</h2>\
             <form action='/forms' method='post'>\
             <input type='submit' name='post' value='Try POST'></form>\
             <p>After 1: this text must start below the Search button.</p>\
             <p>After 2: scroll until the controls leave through the top edge.</p>\
             <p>After 3: partially visible controls must be clipped at that edge.</p>\
             <p>After 4: the toolbar and status bar must remain unchanged.</p>\
             <p>After 5: no stale border should remain after a control disappears.</p>\
             <p>After 6: hidden still occupies no visible row.</p>\
             <p>After 7: disabled remains visibly lighter than Query.</p>\
             <p>After 8: Search keeps its button appearance while scrolling.</p>\
             <p>After 9: all following text remains in normal document flow.</p>\
             <p>After 10: reaching this line confirms the page can scroll far enough.</p>\
             <p>After 11: the controls should now be far above the viewport.</p>\
             <p>After 12: no stale control pixels should remain on this text.</p>\
             <p>After 13: continue through the lower test region.</p>\
             <p>After 14: ordinary paragraphs continue to reserve their line height.</p>\
             <p>After 15: scrolling remains available well beyond the form.</p>\
             <p>After 16: browser chrome should remain stable throughout.</p>\
             <p>After 17: this extra distance exposes delayed redraw artifacts.</p>\
             <p>After 18: text should remain readable after repeated scrolling.</p>\
             <p>After 19: continue toward the bottom of the fixture.</p>\
             <p>After 20: the form remains part of the same document flow.</p>\
             <p>After 21: this line is another full viewport below the controls.</p>\
             <p>After 22: no hidden input row should appear in the document.</p>\
             <p>After 23: continue scrolling to verify the complete range.</p>\
             <p>After 24: the final marker follows this paragraph.</p>\
             <p>End of the long form layout acceptance fixture.</p><p><a href='/'>Home</a></p>",
        ),
    };

    /// The built-in page at `path`, if there is one.
    pub fn by_path(path: &str) -> Option<&'static Page> {
        match path {
            "/" => Some(HOME),
            "/sample" => Some(SAMPLE),
            "/long" => Some(LONG),
            "/wide" => Some(WIDE),
            "/japanese" => Some(JAPANESE),
            "/italic" => Some(ITALIC),
            "/table" => Some(TABLE),
            "/images" => Some(IMAGES),
            "/empty" => Some(EMPTY),
            "/fragments" => Some(FRAGMENTS),
            "/forms" => Some(FORMS),
            _ => None,
        }
    }
}
