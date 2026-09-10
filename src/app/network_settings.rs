//! Event-driven Network settings mini.
//!
//! The host dispatches input and drawing. The manager advances GUI RPC
//! operations and association/DHCP without a screen-owned wait loop.
//!
use alloc::vec::Vec;

use super::theme::{self, TEXT as BLACK};
use crate::framebuffer::{Framebuffer, HEIGHT, WIDTH};
use crate::input::Key;
use crate::wifi;

use super::shell::Line;
use super::wifi_manager::{Association, Failure, Manager, State};

const BACKGROUND: u16 = theme::BACKGROUND;
const HEADER: u16 = theme::HEADER;
const FOOTER: u16 = theme::SUBTLE;
const PANEL: u16 = theme::PANEL;
const SELECTED: u16 = theme::ACCENT;
const MUTED: u16 = theme::MUTED;
const PRIMARY: u16 = theme::ACCENT;
const SUCCESS: u16 = theme::SUCCESS;
const WARNING: u16 = theme::WARNING;
/// The unlit part of a signal bar: mid grey, so the bar's full height stays
/// visible against both the panel and the blue selected row. A bar drawn
/// only where it is lit would make "one bar" and "four bars" the same shape
/// at different heights instead of the same shape differently filled.
const BAR_EMPTY: u16 = theme::INACTIVE;

const HEADER_HEIGHT: usize = 78;
const LIST_TOP: usize = 94;
const LIST_BOTTOM: usize = 604;
const ROW_HEIGHT: usize = 32;
const VISIBLE_ROWS: usize = (LIST_BOTTOM - LIST_TOP) / ROW_HEIGHT;
const FOOTER_TOP: usize = 620;

/// The list's columns, left to right, as x positions inside a row.
///
/// The name comes first because it is the only column the reader is
/// looking for: everything else on the row answers a question they only
/// ask once they have found the network. The signal follows it because it
/// is what decides between two rows with the same name, and the channel,
/// the security and the BSSID count -- the three that are read rarely, and
/// never scanned down the list -- go to the right of both.
///
/// A row spans `ROW_LEFT..ROW_RIGHT`, so every column is a fixed x rather
/// than a position in one packed string. That is what makes a column
/// readable down the list instead of only across one row.
const ROW_LEFT: usize = 24;
const ROW_RIGHT: usize = WIDTH - 24;
/// The tick on the network the board is already on. Half-width, like the
/// ASCII around it: the subset draws U+2713 in an 8 pixel cell.
const MARK_LEFT: usize = 36;
/// The name. It runs to `SIGNAL_LEFT` and cannot overrun it: an SSID is 32
/// bytes, and a glyph is 16 pixels wide only for the code points that take
/// three of them, so the widest one drawable is 32 half-width characters --
/// 256 pixels, less than what is reserved here.
const SSID_LEFT: usize = 60;
/// The signal: the bars, then the number, both inside this column.
const SIGNAL_LEFT: usize = 380;
const SIGNAL_TEXT_LEFT: usize = SIGNAL_LEFT + 40;
const CHANNEL_LEFT: usize = 620;
const AUTH_LEFT: usize = 720;
const COUNT_LEFT: usize = 1000;
/// Where the text in a row sits inside its `ROW_HEIGHT` band.
const ROW_TEXT_OFFSET: usize = 6;
/// The column headings, in the 16 pixels between the header band and the
/// first row.
const COLUMN_LABEL_TOP: usize = HEADER_HEIGHT;

const _: () = assert!(
    ROW_LEFT < MARK_LEFT
        && MARK_LEFT < SSID_LEFT
        && SSID_LEFT < SIGNAL_LEFT
        && SIGNAL_LEFT < SIGNAL_TEXT_LEFT
        && SIGNAL_TEXT_LEFT < CHANNEL_LEFT
        && CHANNEL_LEFT < AUTH_LEFT
        && AUTH_LEFT < COUNT_LEFT
        && COUNT_LEFT < ROW_RIGHT,
    "the list columns are not in left-to-right order inside a row"
);
const _: () = assert!(
    COLUMN_LABEL_TOP + crate::font::HEIGHT <= LIST_TOP,
    "the column headings overlap the first row"
);

struct Network {
    access_point: wifi::station::AccessPoint,
    bssid_count: u32,
}

/// What the list needs to know about the board's own connection, read once
/// per repaint.
///
/// Read from the manager rather than remembered by the screen: the state
/// machine advances every frame while this menu is up, so a connection made
/// here -- or lost here -- has to show without the list being rebuilt.
#[derive(Clone, Copy)]
struct Status {
    enabled: bool,
    /// The SSID the manager is associated to, and whether that association
    /// has reached `Online`. `None` when there is no association at all.
    active: Option<(Association, bool)>,
}

impl Status {
    fn read(manager: &Manager) -> Self {
        let state = manager.state();
        let association = match state {
            State::Associated(association)
            | State::RequestingDhcp { association, .. }
            | State::AssociatedNoLease(association)
            | State::Online(association) => Some(association),
            _ => None,
        };
        Self {
            enabled: manager.is_enabled(),
            active: association.map(|association| (association, matches!(state, State::Online(_)))),
        }
    }

    /// `Some(online)` when `ssid` is the network the board is on.
    ///
    /// Matched on the SSID and not the BSSID because that is what the list
    /// shows: `consolidate_access_points` has already folded every BSSID of
    /// one name into a single row, so the row a roaming client is on is the
    /// name's row whichever radio answered.
    fn marks(&self, ssid: &[u8]) -> Option<bool> {
        let (association, online) = self.active?;
        (!ssid.is_empty() && association.ssid() == ssid).then_some(online)
    }
}

/// Where the menu was opened from.
///
/// Only `Startup` behaves differently -- it is the one entry that returns
/// as soon as the connection is up, because the startup screen has a screen
/// to go to next. The other two are told apart in the log, which is where
/// the question "how did the reader get here" is actually asked.

/// Runs until Escape leaves the screen.
///
/// The manager is borrowed from `app::run`, so a connection made here
/// remains usable by the shell after this function returns.
fn consolidate_access_points(access_points: Vec<wifi::station::AccessPoint>) -> Vec<Network> {
    let mut networks: Vec<Network> = Vec::new();
    for access_point in access_points {
        if let Some(existing) = networks
            .iter_mut()
            .find(|network| network.access_point.ssid() == access_point.ssid())
        {
            existing.bssid_count = existing.bssid_count.saturating_add(1);
            if access_point.rssi > existing.access_point.rssi {
                existing.access_point = access_point;
            }
        } else {
            networks.push(Network {
                access_point,
                bssid_count: 1,
            });
        }
    }
    networks.sort_by(|left, right| right.access_point.rssi.cmp(&left.access_point.rssi));
    networks
}

fn failure_result(failure: Failure) -> ConnectionResult {
    match failure {
        Failure::Disabled => ConnectionResult::error("Wi-Fi is off", "Enable Wi-Fi first"),
        Failure::LinkBringUp => ConnectionResult::error("Link bring-up failed", "See UART log"),
        Failure::StartRpc => ConnectionResult::error("Wi-Fi start RPC failed", "See UART log"),
        Failure::StartStatus(status) => ConnectionResult::status("Wi-Fi start refused", status),
        Failure::ConnectRpc => ConnectionResult::error("Connect RPC failed", "See UART log"),
        Failure::ConnectStatus(status) => ConnectionResult::status("Connect refused", status),
        Failure::Disconnected(reason) => {
            let mut line = Line::new();
            line.push_str("Reason ");
            line.push_u32(reason);
            if let Some(name) = wifi::station::disconnect_reason_name(reason) {
                line.push_str("  ");
                line.push_str(name);
            }
            ConnectionResult {
                title: "Association failed",
                detail: line,
            }
        }
        Failure::AssociationTimedOut => {
            ConnectionResult::error("Association timed out", "No event from the C6")
        }
        Failure::TickUnavailable => {
            ConnectionResult::error("Associated, no IP stack", "Millisecond tick is not running")
        }
        Failure::MacRpc => {
            ConnectionResult::error("Associated, no IP stack", "Station MAC RPC failed")
        }
        Failure::MacStatus(status) => ConnectionResult::status("Station MAC refused", status),
        Failure::LinkLost => ConnectionResult::error("C6 link lost", "Connection did not complete"),
        Failure::ConfigRpc => ConnectionResult::error("Config RPC failed", "See UART log"),
        Failure::ConfigStatus(status) => ConnectionResult::status("Config refused", status),
        Failure::StorageRpc => ConnectionResult::error("Storage RPC failed", "See UART log"),
        Failure::StorageStatus(status) => ConnectionResult::status("Storage refused", status),
        Failure::DisconnectRpc => {
            ConnectionResult::error("Old connection disconnect failed", "See UART log")
        }
        Failure::DisconnectStatus(status) => {
            ConnectionResult::status("Old connection disconnect refused", status)
        }
        Failure::DisconnectTimedOut => ConnectionResult::error(
            "Old connection disconnect timed out",
            "No event from the C6",
        ),
        Failure::ModeRpc => ConnectionResult::error("Wi-Fi mode RPC failed", "See UART log"),
        Failure::ModeStatus(status) => ConnectionResult::status("Wi-Fi mode refused", status),
        Failure::StopRpc => ConnectionResult::error("Wi-Fi stop RPC failed", "See UART log"),
        Failure::StopStatus(status) => ConnectionResult::status("Wi-Fi stop refused", status),
    }
}

fn failure_line(failure: Failure) -> Line {
    let result = failure_result(failure);
    let mut line = Line::new();
    line.push_str(result.title);
    line.push_str("  ");
    line.push_str(result.detail.as_str());
    line
}

fn keep_visible(selected: usize, count: usize, first: &mut usize) {
    if selected < *first {
        *first = selected;
    } else if selected >= first.saturating_add(VISIBLE_ROWS) {
        *first = selected + 1 - VISIBLE_ROWS;
    }
    *first = (*first).min(count.saturating_sub(VISIBLE_ROWS));
}

fn draw_access_points(
    framebuffer: &mut Framebuffer,
    access_points: &[Network],
    selected: usize,
    first: usize,
    message: Option<&str>,
    status: Status,
) {
    draw_chrome(framebuffer, "Wi-Fi networks");
    let mut count = Line::new();
    count.push_u32(access_points.len() as u32);
    count.push_str(" networks  ");
    count.push_str(if status.enabled { "On" } else { "Off" });
    framebuffer.draw_gui_text(930, 52, count.as_str(), 1, MUTED, None);

    if access_points.is_empty() {
        centred(framebuffer, 285, "No access points found", 2, WARNING);
        centred(framebuffer, 335, "Press R to rescan", 1, BLACK);
    } else {
        draw_column_labels(framebuffer);
    }

    for (visible, network) in access_points
        .iter()
        .skip(first)
        .take(VISIBLE_ROWS)
        .enumerate()
    {
        let index = first + visible;
        let y = LIST_TOP + visible * ROW_HEIGHT;
        let selected_row = index == selected;
        let background = if selected_row { SELECTED } else { PANEL };
        framebuffer.fill_rect(
            ROW_LEFT,
            y,
            ROW_RIGHT - ROW_LEFT,
            ROW_HEIGHT - 3,
            background,
        );
        draw_access_point_row(framebuffer, network, y, status, selected_row);
    }

    framebuffer.draw_gui_text(
        28,
        FOOTER_TOP,
        "Up/Down/Page select   Enter/Touch connect   O off   F forget",
        1,
        PRIMARY,
        None,
    );
    if let Some(message) = message {
        framebuffer.draw_gui_text(28, FOOTER_TOP + 32, message, 1, WARNING, None);
    } else {
        framebuffer.draw_gui_text(
            28,
            FOOTER_TOP + 32,
            "R rescan   Esc exit   Menu connections request DHCP automatically",
            1,
            MUTED,
            None,
        );
    }
    flush(framebuffer, b"WIFI MENU: list flush failed\r\n");
}

/// Names the columns once, above the first row.
///
/// Without these the second column is a row of bars and a negative number,
/// which reads as a measurement of something but does not say of what.
fn draw_column_labels(framebuffer: &mut Framebuffer) {
    for (x, label) in [
        (SSID_LEFT, "Network"),
        (SIGNAL_LEFT, "Signal"),
        (CHANNEL_LEFT, "CH"),
        (AUTH_LEFT, "Security"),
        (COUNT_LEFT, "APs"),
    ] {
        framebuffer.draw_gui_text(x, COLUMN_LABEL_TOP, label, 1, MUTED, None);
    }
}

/// One row: the tick, the name, the signal, and the three columns that are
/// only read once the name has been found.
fn draw_access_point_row(
    framebuffer: &mut Framebuffer,
    network: &Network,
    y: usize,
    status: Status,
    selected: bool,
) {
    let foreground = if selected { theme::ON_ACCENT } else { BLACK };
    let muted = if selected { theme::ON_ACCENT } else { MUTED };
    let access_point = &network.access_point;
    let text_y = y + ROW_TEXT_OFFSET;
    let active = status.marks(access_point.ssid());

    // The connection the board already has. A tick rather than a colour
    // alone, and green only once there is an address: associated without a
    // lease is the state where the row is the right one and the network
    // still does not work, and the browser's bars make the same
    // distinction. Selected rows use white and retain the tick and bold name.
    if let Some(online) = active {
        let colour = if selected {
            theme::ON_ACCENT
        } else if online {
            SUCCESS
        } else {
            WARNING
        };
        framebuffer.draw_gui_text(MARK_LEFT, text_y, "\u{2713}", 1, colour, None);
    }

    // Bold on the row that is connected. The name is what the reader
    // scans, so the weight goes on the name and not on the whole row.
    let bold = active.is_some();
    if access_point.ssid().is_empty() {
        draw_label(
            framebuffer,
            SSID_LEFT,
            text_y,
            "(HIDDEN - UNAVAILABLE)",
            muted,
            false,
        );
    } else {
        draw_ssid(
            framebuffer,
            SSID_LEFT,
            text_y,
            access_point.ssid(),
            bold,
            foreground,
        );
    }

    draw_signal(framebuffer, text_y, access_point.rssi, selected);

    let mut channel = Line::new();
    channel.push_u32(access_point.channel);
    draw_label(
        framebuffer,
        CHANNEL_LEFT,
        text_y,
        channel.as_str(),
        foreground,
        false,
    );

    let mut auth = Line::new();
    match wifi::station::auth_mode_name(access_point.auth_mode) {
        Some(name) => auth.push_str(name),
        None => {
            auth.push_str("Auth ");
            auth.push_u32(access_point.auth_mode as u32);
        }
    }
    draw_label(
        framebuffer,
        AUTH_LEFT,
        text_y,
        auth.as_str(),
        foreground,
        false,
    );

    // Only when there is more than one: a column of `1 AP` down the whole
    // list says nothing and is what the eye has to skip over to find the
    // rows where the number matters.
    if network.bssid_count > 1 {
        let mut count = Line::new();
        count.push_u32(network.bssid_count);
        draw_label(
            framebuffer,
            COUNT_LEFT,
            text_y,
            count.as_str(),
            muted,
            false,
        );
    }
}

/// Draws the name, as text when the bytes are UTF-8 and byte by byte when
/// they are not.
///
/// An SSID is 32 bytes with no declared encoding. Most are UTF-8 and the
/// font covers Japanese, so the common case is drawn as what it says; the
/// rest fall back to `Line::push_ascii`, which substitutes `.` rather than
/// leaving a hole where a byte was.
fn draw_ssid(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    ssid: &[u8],
    bold: bool,
    color: u16,
) {
    let budget = SIGNAL_LEFT.saturating_sub(x + 8);
    match core::str::from_utf8(ssid) {
        Ok(name) => draw_label_clipped(framebuffer, x, y, name, budget, color, bold),
        Err(_) => {
            let mut line = Line::new();
            line.push_ascii(ssid);
            draw_label_clipped(framebuffer, x, y, line.as_str(), budget, color, bold);
        }
    }
}

fn draw_label_clipped(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    text: &str,
    budget: usize,
    colour: u16,
    bold: bool,
) {
    framebuffer.draw_gui_text_clipped(x, y, text, budget, 1, colour, None);
    if bold {
        framebuffer.draw_gui_text_clipped(
            x + 1,
            y,
            text,
            budget.saturating_sub(1),
            1,
            colour,
            None,
        );
    }
}

/// The signal column: four ascending bars, then the RSSI itself.
///
/// Both, because they answer different questions. The bars are what makes
/// two rows comparable at a glance; the number is what makes one row
/// comparable with the same network yesterday, and is the only form the
/// UART log and the `wifi scan` output share.
fn draw_signal(framebuffer: &mut Framebuffer, y: usize, rssi: i32, selected: bool) {
    const BAR_WIDTH: usize = 5;
    const BAR_GAP: usize = 2;
    const BARS: usize = 4;
    const TALLEST: usize = 16;

    let lit = signal_bars(rssi);
    // The glyph box is 16 pixels tall and the bars are drawn inside it, so
    // the tallest bar and a capital letter end on the same row.
    let baseline = y + TALLEST;
    for index in 0..BARS {
        let height = 4 + index * 4;
        let colour = if index < lit {
            if selected { theme::ON_ACCENT } else { PRIMARY }
        } else {
            BAR_EMPTY
        };
        framebuffer.fill_rect(
            SIGNAL_LEFT + index * (BAR_WIDTH + BAR_GAP),
            baseline - height,
            BAR_WIDTH,
            height,
            colour,
        );
    }

    let mut line = Line::new();
    if rssi < 0 {
        line.push_str("-");
    }
    line.push_u32(rssi.unsigned_abs());
    line.push_str(" DBM");
    draw_label(
        framebuffer,
        SIGNAL_TEXT_LEFT,
        y,
        line.as_str(),
        if selected { theme::ON_ACCENT } else { MUTED },
        false,
    );
}

/// How many of the four bars an RSSI lights.
///
/// The thresholds are the ones the usual client-side tables use: -55 dBm
/// and better is as good as it gets in a room, -67 is what voice and video
/// want, -75 still carries a page, and below -85 an association is a
/// coin toss. Nothing here reads them back, so they only have to be
/// monotonic and to put the strong and the hopeless at opposite ends.
fn signal_bars(rssi: i32) -> usize {
    if rssi >= -55 {
        4
    } else if rssi >= -67 {
        3
    } else if rssi >= -75 {
        2
    } else if rssi >= -85 {
        1
    } else {
        0
    }
}

/// Draws `text`, struck twice one pixel apart when `bold`.
///
/// The 16 pixel font has one weight, so weight is synthesised rather than
/// selected -- the same thing the browser does for `<b>`. One physical
/// pixel thickens every vertical stem, which at this size is most of what
/// a bold face is.
fn draw_label(
    framebuffer: &mut Framebuffer,
    x: usize,
    y: usize,
    text: &str,
    colour: u16,
    bold: bool,
) {
    framebuffer.draw_gui_text(x, y, text, 1, colour, None);
    if bold {
        framebuffer.draw_gui_text(x + 1, y, text, 1, colour, None);
    }
}

fn draw_password_screen(framebuffer: &mut Framebuffer, ssid: &[u8], length: usize) {
    draw_chrome(framebuffer, "Wi-Fi password");
    let mut network = Line::new();
    network.push_str("Network  ");
    network.push_ascii(ssid);
    framebuffer.draw_gui_text(90, 150, network.as_str(), 2, BLACK, None);
    framebuffer.draw_gui_text(90, 226, "Password", 1, MUTED, None);
    framebuffer.fill_rect(90, 258, 900, 64, BLACK);
    framebuffer.fill_rect(92, 260, 896, 60, BACKGROUND);

    let mut masked = [b'*'; wifi::station::PASSWORD_MAX_BYTES];
    let masked_text = core::str::from_utf8(&masked[..length]).unwrap_or("");
    framebuffer.draw_gui_text(106, 276, masked_text, 1, BLACK, Some(BACKGROUND));
    // Do not let the display copy be mistaken for credential storage either.
    zeroize(&mut masked);

    let mut count = Line::new();
    count.push_u32(length as u32);
    count.push_str(" / 64 bytes");
    framebuffer.draw_gui_text(1010, 279, count.as_str(), 1, PRIMARY, None);
    framebuffer.draw_gui_text(
        90,
        370,
        "Enter connect    Backspace delete    Esc cancel",
        1,
        PRIMARY,
        None,
    );
    framebuffer.draw_gui_text(
        90,
        414,
        "The password is not written to the Console or UART log",
        1,
        MUTED,
        None,
    );
    flush(framebuffer, b"WIFI MENU: password-screen flush failed\r\n");
}

/// Draws `text` centred inside `left..left + width`.
///
/// Placed from the text's own measured width rather than from a counted cell
/// count: half the strings on these screens are meant to sit in the middle of
/// something, and a fixed x is only ever right for one string at one size.
fn centred_in(
    framebuffer: &mut Framebuffer,
    left: usize,
    width: usize,
    y: usize,
    text: &str,
    scale: usize,
    color: u16,
) {
    let style =
        crate::font::UiTextStyle::new(crate::font::UiFace::Sans, if scale >= 2 { 32 } else { 16 });
    let drawn = crate::font::ui_text_width(text, style);
    let x = left + width.saturating_sub(drawn) / 2;
    framebuffer.draw_gui_text(x, y, text, scale, color, None);
}

/// Centred across the whole screen.
fn centred(framebuffer: &mut Framebuffer, y: usize, text: &str, scale: usize, color: u16) {
    centred_in(framebuffer, 0, WIDTH, y, text, scale, color);
}

fn draw_chrome(framebuffer: &mut Framebuffer, title: &str) {
    framebuffer.fill_rect(0, 48, WIDTH, HEIGHT - 48, BACKGROUND);
    framebuffer.fill_rect(0, 48, WIDTH, HEADER_HEIGHT - 48, HEADER);
    framebuffer.draw_gui_text(28, 52, title, 1, BLACK, None);
    framebuffer.fill_rect(
        0,
        FOOTER_TOP - 16,
        WIDTH,
        HEIGHT - (FOOTER_TOP - 16),
        FOOTER,
    );
}

fn show_progress(framebuffer: &mut Framebuffer, title: &str, detail: &str) {
    draw_chrome(framebuffer, "Wi-Fi setup");
    framebuffer.draw_gui_text(90, 235, title, 2, PRIMARY, None);
    framebuffer.draw_gui_text(90, 318, detail, 1, BLACK, None);
    flush(framebuffer, b"WIFI MENU: progress-screen flush failed\r\n");
}

fn flush(_framebuffer: &Framebuffer, _failure: &[u8]) { /* Host flushes content. */
}

fn zeroize(bytes: &mut [u8]) {
    for byte in bytes {
        // A normal fill may be optimized away once the local buffer is dead.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
}

fn push_status(line: &mut Line, operation: &str, status: i32) {
    line.push_str(operation);
    line.push_str(" slave status 0x");
    line.push_hex(status as u32, 8);
}

struct ConnectionResult {
    title: &'static str,
    detail: Line,
}

impl ConnectionResult {
    fn error(title: &'static str, detail: &'static str) -> Self {
        let mut line = Line::new();
        line.push_str(detail);
        Self {
            title,
            detail: line,
        }
    }

    fn status(title: &'static str, status: i32) -> Self {
        let mut line = Line::new();
        push_status(&mut line, "Request", status);
        Self {
            title,
            detail: line,
        }
    }
}

/// Event-driven mini; the manager owns any ongoing radio operation.
pub struct Screen {
    pending: Option<u32>,
    last_save: super::wifi_manager::ProfileSaveState,
    access_points: Vec<Network>,
    selected: usize,
    first: usize,
    password: [u8; wifi::station::PASSWORD_MAX_BYTES],
    length: usize,
    mode: Mode,
    message: Option<Line>,
    pub dirty: bool,
    last_state: State,
}
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    List,
    Password,
    ConfirmForget,
    Busy,
}
impl Screen {
    pub fn editing(&self) -> bool {
        self.mode == Mode::Password || self.mode == Mode::ConfirmForget
    }
    pub fn new(manager: &mut Manager) -> Self {
        let mut screen = Self {
            pending: None,
            last_save: manager.profile_save_state(),
            access_points: Vec::new(),
            selected: 0,
            first: 0,
            password: [0; wifi::station::PASSWORD_MAX_BYTES],
            length: 0,
            mode: Mode::List,
            message: None,
            dirty: true,
            last_state: manager.state(),
        };
        if manager.is_enabled() {
            screen.rescan(manager);
        }
        screen
    }
    fn rescan(&mut self, manager: &mut Manager) {
        self.message = None;
        match manager.gui_scan() {
            Ok(token) => {
                self.pending = Some(token);
                self.mode = Mode::Busy;
            }
            Err(e) => self.message = Some(failure_line(e)),
        }
        self.dirty = true;
    }
    pub fn tick(&mut self, manager: &mut Manager) {
        if let Some(result) = self.pending.and_then(|token| manager.gui_result(token)) {
            self.pending = None;
            self.mode = Mode::List;
            match result {
                Ok(Some(points)) => {
                    self.access_points = consolidate_access_points(points);
                    self.selected = 0;
                    self.first = 0;
                }
                Ok(None) => {
                    self.message = None;
                }
                Err(e) => self.message = Some(failure_line(e)),
            }
            self.dirty = true;
        }
        if self.last_state != manager.state() {
            self.last_state = manager.state();
            self.message = match self.last_state {
                State::Failed(failure) => Some(failure_line(failure)),
                State::NeedsPassword(reason) => Some(failure_line(Failure::Disconnected(reason))),
                State::AssociatedNoLease(_) => {
                    let mut l = Line::new();
                    l.push_str("DHCP lease unavailable; client continues waiting");
                    Some(l)
                }
                State::Online(_) => {
                    let mut l = Line::new();
                    l.push_str("Online ");
                    if let Some(config) = manager.stack().and_then(|s| s.config()) {
                        for (i, byte) in config.address.address().octets().iter().enumerate() {
                            if i != 0 {
                                l.push_str(".");
                            }
                            l.push_u32(*byte as u32);
                        }
                    }
                    Some(l)
                }
                _ => None,
            };
            self.dirty = true;
        }
        if self.last_save != manager.profile_save_state() {
            self.last_save = manager.profile_save_state();
            self.dirty = true;
            if self.last_save == super::wifi_manager::ProfileSaveState::Failed {
                let mut line = Line::new();
                line.push_str("Profile save failed; this connection is not saved");
                self.message = Some(line);
            }
        }
    }
    pub fn key(&mut self, key: Key, manager: &mut Manager) -> bool {
        self.dirty = true;
        if self.mode == Mode::Password {
            match key {
                Key::Escape => {
                    zeroize(&mut self.password);
                    self.length = 0;
                    self.mode = Mode::List;
                }
                Key::Ascii(b'\r' | b'\n') => self.connect(manager),
                Key::Ascii(8 | 127) => {
                    self.length = self.length.saturating_sub(1);
                    self.password[self.length] = 0;
                }
                Key::Ascii(b) if (32..127).contains(&b) && self.length < self.password.len() => {
                    self.password[self.length] = b;
                    self.length += 1;
                }
                _ => {}
            }
            return false;
        }
        if self.mode == Mode::ConfirmForget {
            if matches!(key, Key::Ascii(b'y' | b'Y' | b'\r' | b'\n')) {
                self.operation(manager.gui_forget());
            } else {
                self.mode = Mode::List;
            }
            return false;
        }
        if key == Key::Escape {
            return true;
        }
        if self.mode == Mode::Busy {
            return false;
        }
        match key {
            Key::ArrowUp => self.selected = self.selected.saturating_sub(1),
            Key::ArrowDown => {
                self.selected = (self.selected + 1).min(self.access_points.len().saturating_sub(1))
            }
            Key::PageUp => self.selected = self.selected.saturating_sub(VISIBLE_ROWS),
            Key::PageDown => {
                self.selected =
                    (self.selected + VISIBLE_ROWS).min(self.access_points.len().saturating_sub(1))
            }
            Key::Ascii(b'r' | b'R') => self.rescan(manager),
            Key::Ascii(b'o' | b'O') => self.operation(manager.gui_enable(!manager.is_enabled())),
            Key::Ascii(b'f' | b'F') => self.mode = Mode::ConfirmForget,
            Key::Ascii(b'\r' | b'\n') => self.activate(manager),
            _ => {}
        }
        keep_visible(self.selected, self.access_points.len(), &mut self.first);
        false
    }
    fn operation(&mut self, result: Result<u32, Failure>) {
        match result {
            Ok(token) => {
                self.pending = Some(token);
                self.mode = Mode::Busy;
                self.message = None;
            }
            Err(e) => {
                self.mode = Mode::List;
                self.message = Some(failure_line(e));
            }
        }
    }
    fn activate(&mut self, manager: &mut Manager) {
        let Some(network) = self.access_points.get(self.selected) else {
            return;
        };
        if network.access_point.ssid().is_empty() {
            return;
        }
        self.length = 0;
        zeroize(&mut self.password);
        if network.access_point.auth_mode == 0 {
            self.connect(manager);
        } else {
            self.mode = Mode::Password;
        }
    }
    fn connect(&mut self, manager: &mut Manager) {
        if let Some(network) = self.access_points.get(self.selected) {
            let result =
                manager.gui_connect(network.access_point.ssid(), &self.password[..self.length]);
            self.operation(result);
        }
        zeroize(&mut self.password);
        self.length = 0;
    }
    pub fn click(&mut self, x: usize, y: usize, manager: &mut Manager) -> bool {
        if (668..712).contains(&y) && (24..1256).contains(&x) {
            let count = self.footer_labels().len();
            let step = 1232 / count;
            if (x - 24) % step >= step - 16 {
                return false;
            }
            let index = (x - 24) / step;
            let key = match self.mode {
                Mode::List => match index {
                    0 => Key::Ascii(b'r'),
                    1 => Key::Ascii(b'o'),
                    2 => Key::Ascii(b'f'),
                    _ => Key::Escape,
                },
                Mode::Password => match index {
                    0 => Key::Ascii(b'\r'),
                    1 => Key::Ascii(8),
                    2 => {
                        zeroize(&mut self.password);
                        self.length = 0;
                        self.dirty = true;
                        return false;
                    }
                    _ => Key::Escape,
                },
                Mode::ConfirmForget => {
                    if index == 0 {
                        Key::Ascii(b'y')
                    } else {
                        Key::Escape
                    }
                }
                Mode::Busy => Key::Escape,
            };
            return self.key(key, manager);
        }
        if self.mode == Mode::List && (610..642).contains(&y) && (24..1256).contains(&x) {
            return self.key(
                if x < WIDTH / 2 {
                    Key::PageUp
                } else {
                    Key::PageDown
                },
                manager,
            );
        }
        if self.mode == Mode::List
            && (ROW_LEFT..ROW_RIGHT).contains(&x)
            && (LIST_TOP..LIST_BOTTOM).contains(&y)
        {
            let index = self.first + (y - LIST_TOP) / ROW_HEIGHT;
            if index < self.access_points.len() {
                self.selected = index;
                self.activate(manager);
                self.dirty = true;
            }
        }
        false
    }
    fn footer_labels(&self) -> &'static [&'static str] {
        match self.mode {
            Mode::List => &["R Rescan", "O On/Off", "F Forget", "Esc Back"],
            Mode::Password => &["Enter Connect", "Backspace", "Clear", "Esc Cancel"],
            Mode::ConfirmForget => &["Yes, forget saved profile", "Cancel"],
            Mode::Busy => &["Esc Back"],
        }
    }
    pub fn draw(&mut self, fb: &mut Framebuffer, manager: &Manager) {
        match self.mode {
            Mode::List => draw_access_points(
                fb,
                &self.access_points,
                self.selected,
                self.first,
                self.message.as_ref().map(Line::as_str),
                Status::read(manager),
            ),
            Mode::Password => draw_password_screen(
                fb,
                self.access_points[self.selected].access_point.ssid(),
                self.length,
            ),
            Mode::ConfirmForget => show_progress(
                fb,
                "Forget saved profile?",
                "Y / Enter confirm   any other key cancel",
            ),
            Mode::Busy => show_progress(
                fb,
                "Wi-Fi operation in progress",
                "System bar and Esc remain available",
            ),
        }
        if self.mode == Mode::List {
            fb.fill_rect(0, 610, WIDTH, 34, FOOTER);
            for (i, label) in ["Page up", "Page down"].iter().enumerate() {
                fb.fill_rect(24 + i * 616, 610, 600, 32, HEADER);
                fb.draw_gui_text(40 + i * 616, 618, label, 1, BLACK, None);
            }
        }
        let labels = self.footer_labels();
        let step = 1232 / labels.len();
        for (i, label) in labels.iter().enumerate() {
            fb.fill_rect(24 + i * step, 668, step - 16, 44, HEADER);
            draw_label(fb, 40 + i * step, 680, label, BLACK, *label == "Esc Back");
        }
        self.dirty = false;
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        zeroize(&mut self.password);
    }
}
