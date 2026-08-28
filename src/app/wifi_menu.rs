//! Minimal keyboard-driven Wi-Fi setup screen.
//!
//! Scanning and the short RPC request remain synchronous. Association events
//! and DHCP are advanced once per frame by the connection manager, so input
//! and link servicing continue while the AP or DHCP server is slow.
//!
//! The list is laid out in fixed columns rather than one packed string per
//! row, and the name comes first: it is the only column the reader is
//! looking for, and a column at a fixed x can be read down the list instead
//! of only across one row. The signal follows the name because it is what
//! decides between two rows that carry the same one; the channel, the
//! security and the BSSID count are read rarely and never scanned, so they
//! go to the right of both. The row the board is already on is ticked and
//! its name drawn bold: which network this is on is the first thing anyone
//! opening the screen wants to know, and finding it by reading every row
//! is what a list of thirty networks makes hard.
//!
//! The screen is reached from the shell's `wifi`, from the startup screen,
//! and from the browser's Wi-Fi indicator (`Entry`); only the startup entry
//! behaves differently, returning as soon as the connection is up.

use alloc::vec::Vec;

use crate::framebuffer::{BLACK, Framebuffer, HEIGHT, RED, WHITE, WIDTH};
use crate::input::{InputManager, Key, PrimaryTouch};
use crate::{interrupts, uart, wifi};

use super::shell::Line;
use super::wifi_manager::{Association, Failure, Manager, ProfileChoice, ProfileSaveState, State};

const BACKGROUND: u16 = WHITE;
const HEADER: u16 = 0xE71C;
const FOOTER: u16 = 0xEF7D;
const PANEL: u16 = 0xDEFB;
const SELECTED: u16 = 0xBDF7;
const MUTED: u16 = 0x632C;
const PRIMARY: u16 = 0x0015;
const SUCCESS: u16 = 0x0400;
const WARNING: u16 = 0xA500;
/// The unlit part of a signal bar: mid grey, so the bar's full height stays
/// visible against both the panel and the lighter selected row. A bar drawn
/// only where it is lit would make "one bar" and "four bars" the same shape
/// at different heights instead of the same shape differently filled.
const BAR_EMPTY: u16 = 0x8C51;

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

enum ListAction {
    Exit,
    Rescan,
    Toggle,
    Forget,
    Up,
    Down,
    PageUp,
    PageDown,
    Activate(usize),
}

/// Where the menu was opened from.
///
/// Only `Startup` behaves differently -- it is the one entry that returns
/// as soon as the connection is up, because the startup screen has a screen
/// to go to next. The other two are told apart in the log, which is where
/// the question "how did the reader get here" is actually asked.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Entry {
    Shell,
    Browser,
    Startup,
}

impl Entry {
    fn name(self) -> &'static [u8] {
        match self {
            Entry::Shell => b"shell",
            Entry::Browser => b"browser",
            Entry::Startup => b"startup",
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Outcome {
    Online,
    Cancelled,
}

/// Runs until Escape leaves the screen.
///
/// The manager is borrowed from `app::run`, so a connection made here
/// remains usable by the shell after this function returns.
pub fn run(
    framebuffer: &mut Framebuffer,
    input: &mut InputManager,
    manager: &mut Manager,
    entry: Entry,
) -> Outcome {
    uart::log(b"WIFI MENU: opened from ");
    uart::log(entry.name());
    uart::log(b"\r\n");
    input.reset_primary_touch();
    let mut selected = 0usize;
    let mut first = 0usize;

    'scan: loop {
        if !manager.is_enabled() {
            draw_off_screen(framebuffer, manager.has_saved_profile(), None);
            loop {
                match wait_off_action(input, manager) {
                    Some(ListAction::Toggle) => {
                        show_progress(framebuffer, "ENABLING WI-FI", "RESTORING C6 RADIO STATE");
                        match manager.set_enabled(true) {
                            Ok(true) => {
                                show_progress(
                                    framebuffer,
                                    "WI-FI ENABLED",
                                    "AUTO-CONNECT STARTED - ESC CANCELS",
                                );
                                if entry == Entry::Startup {
                                    loop {
                                        if matches!(manager.state(), State::Online(_)) {
                                            return Outcome::Online;
                                        }
                                        if matches!(
                                            manager.state(),
                                            State::Off
                                                | State::Idle
                                                | State::NeedsPassword(_)
                                                | State::AssociatedNoLease(_)
                                                | State::Failed(_)
                                        ) {
                                            continue 'scan;
                                        }
                                        let Some((key, _)) = wait_input_frame(input, manager)
                                        else {
                                            return Outcome::Cancelled;
                                        };
                                        if matches!(key, Some(Key::Escape)) {
                                            return Outcome::Cancelled;
                                        }
                                    }
                                }
                                let _ = wait_key(input, manager);
                                return Outcome::Cancelled;
                            }
                            Ok(false) => continue 'scan,
                            Err(failure) => draw_off_screen(
                                framebuffer,
                                manager.has_saved_profile(),
                                Some(failure_line(failure).as_str()),
                            ),
                        }
                    }
                    Some(ListAction::Forget) => {
                        if confirm_forget(framebuffer, input, manager)
                            && let Err(failure) = manager.forget_saved_profile()
                        {
                            let line = failure_line(failure);
                            draw_off_screen(
                                framebuffer,
                                manager.has_saved_profile(),
                                Some(line.as_str()),
                            );
                        } else {
                            draw_off_screen(framebuffer, manager.has_saved_profile(), None);
                        }
                    }
                    Some(ListAction::Exit) | None => return Outcome::Cancelled,
                    _ => {}
                }
            }
        }

        let access_points = loop {
            show_progress(
                framebuffer,
                "SCANNING FOR ACCESS POINTS",
                "THIS MAY TAKE A FEW SECONDS",
            );
            match scan(manager) {
                Ok(access_points) => break access_points,
                Err(error) => {
                    uart::log(b"WIFI MENU: scan path failed\r\n");
                    show_error(
                        framebuffer,
                        "SCAN FAILED",
                        &error.line(),
                        "R RETRY    ESC EXIT",
                    );
                    loop {
                        match wait_key(input, manager) {
                            Some(Key::Ascii(b'r' | b'R')) => break,
                            Some(Key::Escape) | None => return Outcome::Cancelled,
                            _ => {}
                        }
                    }
                    continue;
                }
            }
        };

        selected = selected.min(access_points.len().saturating_sub(1));
        keep_visible(selected, access_points.len(), &mut first);

        loop {
            draw_access_points(
                framebuffer,
                &access_points,
                selected,
                first,
                None,
                Status::read(manager),
            );
            let Some(action) =
                wait_list_action(input, manager, selected, first, access_points.len())
            else {
                return Outcome::Cancelled;
            };
            match action {
                ListAction::Exit => return Outcome::Cancelled,
                ListAction::Rescan => {
                    selected = 0;
                    first = 0;
                    continue 'scan;
                }
                ListAction::Toggle => {
                    show_progress(
                        framebuffer,
                        "DISABLING WI-FI",
                        "LEAVING AP AND POWERING DOWN C6",
                    );
                    let message = manager.set_enabled(false).err().map(failure_line);
                    draw_off_screen(
                        framebuffer,
                        manager.has_saved_profile(),
                        message.as_ref().map(Line::as_str),
                    );
                    continue 'scan;
                }
                ListAction::Forget => {
                    if confirm_forget(framebuffer, input, manager) {
                        let message = manager.forget_saved_profile().err().map(failure_line);
                        draw_access_points(
                            framebuffer,
                            &access_points,
                            selected,
                            first,
                            message
                                .as_ref()
                                .map(Line::as_str)
                                .or(Some("SAVED PROFILE DELETED")),
                            Status::read(manager),
                        );
                        let _ = wait_key(input, manager);
                    }
                }
                ListAction::Up if !access_points.is_empty() => {
                    selected = selected.saturating_sub(1);
                    keep_visible(selected, access_points.len(), &mut first);
                }
                ListAction::Down if !access_points.is_empty() => {
                    selected = (selected + 1).min(access_points.len() - 1);
                    keep_visible(selected, access_points.len(), &mut first);
                }
                ListAction::PageUp if !access_points.is_empty() => {
                    selected = selected.saturating_sub(VISIBLE_ROWS);
                    keep_visible(selected, access_points.len(), &mut first);
                }
                ListAction::PageDown if !access_points.is_empty() => {
                    selected = (selected + VISIBLE_ROWS).min(access_points.len() - 1);
                    keep_visible(selected, access_points.len(), &mut first);
                }
                ListAction::Activate(index) if index < access_points.len() => {
                    selected = index;
                    keep_visible(selected, access_points.len(), &mut first);
                    let access_point = &access_points[selected].access_point;
                    if access_point.ssid().is_empty() {
                        draw_access_points(
                            framebuffer,
                            &access_points,
                            selected,
                            first,
                            Some("HIDDEN SSIDS CANNOT BE SELECTED IN THIS VERSION"),
                            Status::read(manager),
                        );
                        let _ = wait_key(input, manager);
                        continue;
                    }

                    let mut ssid = [0u8; wifi::station::SSID_MAX_BYTES];
                    let ssid_length = access_point.ssid().len();
                    ssid[..ssid_length].copy_from_slice(access_point.ssid());
                    let auth_mode = access_point.auth_mode;
                    let mut password = [0u8; wifi::station::PASSWORD_MAX_BYTES];
                    let password_length = if auth_mode == 0 {
                        Some(0)
                    } else {
                        edit_password(
                            framebuffer,
                            input,
                            manager,
                            &ssid[..ssid_length],
                            &mut password,
                        )
                    };

                    let Some(password_length) = password_length else {
                        zeroize(&mut password);
                        continue;
                    };
                    let Some(profile) =
                        choose_profile(framebuffer, input, manager, &ssid[..ssid_length])
                    else {
                        zeroize(&mut password);
                        continue;
                    };
                    let result = connect_and_configure(
                        framebuffer,
                        input,
                        manager,
                        &ssid[..ssid_length],
                        &mut password[..password_length],
                        profile,
                    );
                    zeroize(&mut password);
                    let Some(result) = result else {
                        return Outcome::Cancelled;
                    };
                    if entry == Entry::Startup && matches!(manager.state(), State::Online(_)) {
                        return Outcome::Online;
                    }
                    show_result(framebuffer, &result);
                    match wait_result_action(input, manager) {
                        ResultAction::Exit => return Outcome::Cancelled,
                        ResultAction::Rescan => {
                            selected = 0;
                            first = 0;
                            continue 'scan;
                        }
                        ResultAction::List => {}
                    }
                }
                _ => {}
            }
        }
    }
}

fn scan(manager: &mut Manager) -> Result<Vec<Network>, MenuError> {
    let rpc = manager.ensure_station().map_err(MenuError::Manager)?;
    let Some((status, access_points)) = wifi::station::scan(rpc) else {
        return Err(MenuError::ScanRpc);
    };
    if status != 0 {
        return Err(MenuError::ScanStatus(status));
    }
    uart::log_u32(b"WIFI MENU: access points=", access_points.len() as u32);
    Ok(consolidate_access_points(access_points))
}

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

fn edit_password(
    framebuffer: &mut Framebuffer,
    input: &mut InputManager,
    manager: &mut Manager,
    ssid: &[u8],
    password: &mut [u8; wifi::station::PASSWORD_MAX_BYTES],
) -> Option<usize> {
    let mut length = 0usize;
    draw_password_screen(framebuffer, ssid, length);
    loop {
        let key = wait_key(input, manager)?;
        let changed = match key {
            Key::Escape => return None,
            Key::Ascii(b'\r' | b'\n') => return Some(length),
            Key::Ascii(0x08 | 0x7F) | Key::Delete if length != 0 => {
                length -= 1;
                password[length] = 0;
                true
            }
            Key::Ascii(byte) if (0x20..=0x7E).contains(&byte) && length < password.len() => {
                password[length] = byte;
                length += 1;
                true
            }
            _ => false,
        };
        if changed {
            draw_password_screen(framebuffer, ssid, length);
        }
    }
}

fn choose_profile(
    framebuffer: &mut Framebuffer,
    input: &mut InputManager,
    manager: &mut Manager,
    ssid: &[u8],
) -> Option<ProfileChoice> {
    let mut choice = ProfileChoice::SaveAndAutoConnect;
    loop {
        draw_profile_choice(framebuffer, ssid, choice);
        match wait_key(input, manager)? {
            Key::Escape => return None,
            Key::ArrowUp | Key::ArrowDown => {
                choice = match choice {
                    ProfileChoice::SaveAndAutoConnect => ProfileChoice::ConnectOnce,
                    ProfileChoice::ConnectOnce => ProfileChoice::SaveAndAutoConnect,
                };
            }
            Key::Ascii(b's' | b'S') => return Some(ProfileChoice::SaveAndAutoConnect),
            Key::Ascii(b'o' | b'O') => return Some(ProfileChoice::ConnectOnce),
            Key::Ascii(b'\r' | b'\n') => return Some(choice),
            _ => {}
        }
    }
}

fn connect_and_configure(
    framebuffer: &mut Framebuffer,
    input: &mut InputManager,
    manager: &mut Manager,
    ssid: &[u8],
    password: &mut [u8],
    profile: ProfileChoice,
) -> Option<ConnectionResult> {
    let mut detail = Line::new();
    detail.push_str("SSID ");
    detail.push_ascii(ssid);
    show_progress(framebuffer, "ASSOCIATING", detail.as_str());

    let started = manager.begin_menu_connect(ssid, password, profile);
    // The manager has its own fixed retry buffer now. Erase the UI's copy
    // before waiting for a station event; the manager erases its copy on
    // success or a non-retryable failure.
    zeroize(password);
    if let Err(failure) = started {
        return Some(failure_result(failure));
    }

    let mut sequence = interrupts::frame_sequence();
    let mut dhcp_screen_drawn = false;
    let mut shown_attempt = 0u32;
    let mut shown_retry: Option<(u32, u32)> = None;
    loop {
        if interrupts::dma_error() != 0 {
            uart::log(b"WIFI MENU: DMA interrupt error\r\n");
            return None;
        }
        interrupts::wait_for_interrupt();
        let next_sequence = interrupts::frame_sequence();
        if next_sequence == sequence {
            input.service_fast();
            continue;
        }
        sequence = next_sequence;
        input.service();
        manager.service();

        while let Some(event) = input.poll_key() {
            if event.key == Key::Escape {
                // The manager owns the in-flight request, so leaving the
                // screen does not lose or duplicate it.
                return None;
            }
        }

        match manager.state() {
            State::Associating { attempt, .. } => {
                if attempt != shown_attempt {
                    let mut title = Line::new();
                    title.push_str("ASSOCIATING - ATTEMPT ");
                    title.push_u32(attempt);
                    show_progress(framebuffer, title.as_str(), detail.as_str());
                    shown_attempt = attempt;
                    shown_retry = None;
                }
            }
            State::RetryWaiting {
                next_attempt,
                generation,
                failure,
                ..
            } => {
                if shown_retry != Some((generation, next_attempt)) {
                    let mut retry = failure_line(failure);
                    retry.push_str("  NEXT ATTEMPT ");
                    retry.push_u32(next_attempt);
                    show_progress(framebuffer, "RETRY WAITING", retry.as_str());
                    shown_retry = Some((generation, next_attempt));
                }
            }
            State::NeedsPassword(reason) => {
                let mut line = failure_line(Failure::Disconnected(reason));
                line.push_str("  SELECT THE AP TO TRY AGAIN");
                return Some(ConnectionResult {
                    kind: ResultKind::Error,
                    title: "PASSWORD REQUIRED",
                    detail: line,
                });
            }
            State::RequestingDhcp { association, .. } => {
                if !dhcp_screen_drawn {
                    show_progress(
                        framebuffer,
                        "REQUESTING DHCP LEASE",
                        association_line(&association).as_str(),
                    );
                    dhcp_screen_drawn = true;
                }
            }
            State::Online(association) => {
                let mut line = association_line(&association);
                if let Some(config) = manager.stack().and_then(|stack| stack.config()) {
                    line.push_str("  IP ");
                    push_ipv4(&mut line, config.address.address());
                }
                uart::log(b"WIFI MENU: associated and DHCP configured\r\n");
                let (kind, title) = match manager.profile_save_state() {
                    ProfileSaveState::Saved => (ResultKind::Success, "ONLINE - PROFILE SAVED"),
                    ProfileSaveState::Failed => {
                        line.push_str("  PROFILE SAVE FAILED");
                        (ResultKind::Warning, "ONLINE - SAVE FAILED")
                    }
                    ProfileSaveState::NotRequested | ProfileSaveState::Pending => {
                        (ResultKind::Success, "ONLINE")
                    }
                };
                return Some(ConnectionResult {
                    kind,
                    title,
                    detail: line,
                });
            }
            State::AssociatedNoLease(association) => {
                let mut line = association_line(&association);
                line.push_str("  DHCP STILL PENDING");
                return Some(ConnectionResult {
                    kind: ResultKind::Warning,
                    title: "ASSOCIATED, NO LEASE YET",
                    detail: line,
                });
            }
            State::Failed(failure) => return Some(failure_result(failure)),
            State::Associated(association) => {
                return Some(ConnectionResult {
                    kind: ResultKind::Warning,
                    title: "ASSOCIATED, IP UNCONFIGURED",
                    detail: association_line(&association),
                });
            }
            State::Off => {
                return Some(ConnectionResult::error(
                    "WI-FI IS OFF",
                    "ENABLE WI-FI BEFORE CONNECTING",
                ));
            }
            State::LinkDown | State::Idle => {
                return Some(ConnectionResult::error(
                    "CONNECTION STOPPED",
                    "THE CONNECTION MANAGER RETURNED IDLE",
                ));
            }
        }
    }
}

fn association_line(association: &super::wifi_manager::Association) -> Line {
    let mut line = Line::new();
    line.push_str("SSID ");
    line.push_ascii(association.ssid());
    line.push_str("  CHANNEL ");
    line.push_u32(association.channel);
    line
}

fn failure_result(failure: Failure) -> ConnectionResult {
    match failure {
        Failure::Disabled => ConnectionResult::error("WI-FI IS OFF", "ENABLE WI-FI FIRST"),
        Failure::LinkBringUp => ConnectionResult::error("LINK BRING-UP FAILED", "SEE UART LOG"),
        Failure::StartRpc => ConnectionResult::error("WI-FI START RPC FAILED", "SEE UART LOG"),
        Failure::StartStatus(status) => ConnectionResult::status("WI-FI START REFUSED", status),
        Failure::ConnectRpc => ConnectionResult::error("CONNECT RPC FAILED", "SEE UART LOG"),
        Failure::ConnectStatus(status) => ConnectionResult::status("CONNECT REFUSED", status),
        Failure::Disconnected(reason) => {
            let mut line = Line::new();
            line.push_str("REASON ");
            line.push_u32(reason);
            if let Some(name) = wifi::station::disconnect_reason_name(reason) {
                line.push_str("  ");
                line.push_str(name);
            }
            ConnectionResult {
                kind: ResultKind::Error,
                title: "ASSOCIATION FAILED",
                detail: line,
            }
        }
        Failure::AssociationTimedOut => {
            ConnectionResult::error("ASSOCIATION TIMED OUT", "NO EVENT FROM THE C6")
        }
        Failure::TickUnavailable => {
            ConnectionResult::error("ASSOCIATED, NO IP STACK", "MILLISECOND TICK IS NOT RUNNING")
        }
        Failure::MacRpc => {
            ConnectionResult::error("ASSOCIATED, NO IP STACK", "STATION MAC RPC FAILED")
        }
        Failure::MacStatus(status) => ConnectionResult::status("STATION MAC REFUSED", status),
        Failure::LinkLost => ConnectionResult::error("C6 LINK LOST", "CONNECTION DID NOT COMPLETE"),
        Failure::ConfigRpc => ConnectionResult::error("CONFIG RPC FAILED", "SEE UART LOG"),
        Failure::ConfigStatus(status) => ConnectionResult::status("CONFIG REFUSED", status),
        Failure::StorageRpc => ConnectionResult::error("STORAGE RPC FAILED", "SEE UART LOG"),
        Failure::StorageStatus(status) => ConnectionResult::status("STORAGE REFUSED", status),
        Failure::DisconnectRpc => {
            ConnectionResult::error("OLD CONNECTION DISCONNECT FAILED", "SEE UART LOG")
        }
        Failure::DisconnectStatus(status) => {
            ConnectionResult::status("OLD CONNECTION DISCONNECT REFUSED", status)
        }
        Failure::DisconnectTimedOut => ConnectionResult::error(
            "OLD CONNECTION DISCONNECT TIMED OUT",
            "NO EVENT FROM THE C6",
        ),
        Failure::ModeRpc => ConnectionResult::error("WI-FI MODE RPC FAILED", "SEE UART LOG"),
        Failure::ModeStatus(status) => ConnectionResult::status("WI-FI MODE REFUSED", status),
        Failure::StopRpc => ConnectionResult::error("WI-FI STOP RPC FAILED", "SEE UART LOG"),
        Failure::StopStatus(status) => ConnectionResult::status("WI-FI STOP REFUSED", status),
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

fn wait_key(input: &mut InputManager, manager: &mut Manager) -> Option<Key> {
    loop {
        let (key, _) = wait_input_frame(input, manager)?;
        if key.is_some() {
            return key;
        }
    }
}

fn wait_input_frame(
    input: &mut InputManager,
    manager: &mut Manager,
) -> Option<(Option<Key>, PrimaryTouch)> {
    let sequence = interrupts::frame_sequence();
    loop {
        if interrupts::dma_error() != 0 {
            uart::log(b"WIFI MENU: DMA interrupt error\r\n");
            return None;
        }
        interrupts::wait_for_interrupt();
        let next_sequence = interrupts::frame_sequence();
        if next_sequence == sequence {
            input.service_fast();
            continue;
        }
        input.service();
        manager.service();
        return Some((
            input.poll_key().map(|event| event.key),
            input.poll_primary_touch(),
        ));
    }
}

fn wait_list_action(
    input: &mut InputManager,
    manager: &mut Manager,
    selected: usize,
    first: usize,
    count: usize,
) -> Option<ListAction> {
    loop {
        let (key, touch) = wait_input_frame(input, manager)?;
        let keyboard = match key {
            Some(Key::Escape) => Some(ListAction::Exit),
            Some(Key::Ascii(b'r' | b'R')) => Some(ListAction::Rescan),
            Some(Key::Ascii(b'o' | b'O')) => Some(ListAction::Toggle),
            Some(Key::Ascii(b'f' | b'F')) => Some(ListAction::Forget),
            Some(Key::ArrowUp) => Some(ListAction::Up),
            Some(Key::ArrowDown) => Some(ListAction::Down),
            Some(Key::PageUp) => Some(ListAction::PageUp),
            Some(Key::PageDown) => Some(ListAction::PageDown),
            Some(Key::Ascii(b'\r' | b'\n')) => {
                Some(ListAction::Activate(selected.min(count.saturating_sub(1))))
            }
            _ => None,
        };
        if keyboard.is_some() {
            return keyboard;
        }
        if let PrimaryTouch::Pressed(point) = touch {
            if point.x >= 24 && point.x < WIDTH - 24 && (LIST_TOP..LIST_BOTTOM).contains(&point.y) {
                let index = first + (point.y - LIST_TOP) / ROW_HEIGHT;
                if index < count {
                    return Some(ListAction::Activate(index));
                }
            }
            if point.y >= FOOTER_TOP {
                return Some(match point.x {
                    0..=239 => ListAction::Toggle,
                    240..=479 => ListAction::Forget,
                    480..=799 => ListAction::Rescan,
                    _ => ListAction::Exit,
                });
            }
        }
    }
}

fn wait_off_action(input: &mut InputManager, manager: &mut Manager) -> Option<ListAction> {
    loop {
        let (key, touch) = wait_input_frame(input, manager)?;
        match key {
            Some(Key::Escape) => return Some(ListAction::Exit),
            Some(Key::Ascii(b'o' | b'O')) => return Some(ListAction::Toggle),
            Some(Key::Ascii(b'f' | b'F')) => return Some(ListAction::Forget),
            _ => {}
        }
        if let PrimaryTouch::Pressed(point) = touch {
            if (220..=590).contains(&point.x) && (290..=390).contains(&point.y) {
                return Some(ListAction::Toggle);
            }
            if (690..=1060).contains(&point.x) && (290..=390).contains(&point.y) {
                return Some(ListAction::Forget);
            }
            if point.y >= FOOTER_TOP {
                return Some(ListAction::Exit);
            }
        }
    }
}

fn wait_result_action(input: &mut InputManager, manager: &mut Manager) -> ResultAction {
    loop {
        match wait_key(input, manager) {
            Some(Key::Escape) | None => return ResultAction::Exit,
            Some(Key::Ascii(b'r' | b'R')) => return ResultAction::Rescan,
            Some(Key::Ascii(b'\r' | b'\n')) => return ResultAction::List,
            _ => {}
        }
    }
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
    draw_chrome(framebuffer, "WI-FI NETWORKS");
    let mut count = Line::new();
    count.push_u32(access_points.len() as u32);
    count.push_str(" NETWORKS  ");
    count.push_str(if status.enabled { "ON" } else { "OFF" });
    framebuffer.draw_text(930, 30, count.as_str(), 1, MUTED, None);

    if access_points.is_empty() {
        centred(framebuffer, 285, "NO ACCESS POINTS FOUND", 2, WARNING);
        centred(framebuffer, 335, "PRESS R TO RESCAN", 1, BLACK);
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
        draw_access_point_row(framebuffer, network, y, status);
    }

    framebuffer.draw_text(
        28,
        FOOTER_TOP,
        "UP/DOWN/PAGE SELECT   ENTER/TOUCH CONNECT   O OFF   F FORGET",
        1,
        PRIMARY,
        None,
    );
    if let Some(message) = message {
        framebuffer.draw_text(28, FOOTER_TOP + 32, message, 1, WARNING, None);
    } else {
        framebuffer.draw_text(
            28,
            FOOTER_TOP + 32,
            "R RESCAN   ESC EXIT   MENU CONNECTIONS REQUEST DHCP AUTOMATICALLY",
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
        (SSID_LEFT, "NETWORK"),
        (SIGNAL_LEFT, "SIGNAL"),
        (CHANNEL_LEFT, "CH"),
        (AUTH_LEFT, "SECURITY"),
        (COUNT_LEFT, "APS"),
    ] {
        framebuffer.draw_text(x, COLUMN_LABEL_TOP, label, 1, MUTED, None);
    }
}

/// One row: the tick, the name, the signal, and the three columns that are
/// only read once the name has been found.
fn draw_access_point_row(
    framebuffer: &mut Framebuffer,
    network: &Network,
    y: usize,
    status: Status,
) {
    let access_point = &network.access_point;
    let text_y = y + ROW_TEXT_OFFSET;
    let active = status.marks(access_point.ssid());

    // The connection the board already has. A tick rather than a colour
    // alone, and green only once there is an address: associated without a
    // lease is the state where the row is the right one and the network
    // still does not work, and the browser's bars make the same
    // distinction.
    if let Some(online) = active {
        let colour = if online { SUCCESS } else { WARNING };
        framebuffer.draw_text(MARK_LEFT, text_y, "\u{2713}", 1, colour, None);
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
            MUTED,
            false,
        );
    } else {
        draw_ssid(framebuffer, SSID_LEFT, text_y, access_point.ssid(), bold);
    }

    draw_signal(framebuffer, text_y, access_point.rssi);

    let mut channel = Line::new();
    channel.push_u32(access_point.channel);
    draw_label(
        framebuffer,
        CHANNEL_LEFT,
        text_y,
        channel.as_str(),
        BLACK,
        false,
    );

    let mut auth = Line::new();
    match wifi::station::auth_mode_name(access_point.auth_mode) {
        Some(name) => auth.push_str(name),
        None => {
            auth.push_str("AUTH ");
            auth.push_u32(access_point.auth_mode as u32);
        }
    }
    draw_label(framebuffer, AUTH_LEFT, text_y, auth.as_str(), BLACK, false);

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
            MUTED,
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
fn draw_ssid(framebuffer: &mut Framebuffer, x: usize, y: usize, ssid: &[u8], bold: bool) {
    match core::str::from_utf8(ssid) {
        Ok(name) => draw_label(framebuffer, x, y, name, BLACK, bold),
        Err(_) => {
            let mut line = Line::new();
            line.push_ascii(ssid);
            draw_label(framebuffer, x, y, line.as_str(), BLACK, bold);
        }
    }
}

/// The signal column: four ascending bars, then the RSSI itself.
///
/// Both, because they answer different questions. The bars are what makes
/// two rows comparable at a glance; the number is what makes one row
/// comparable with the same network yesterday, and is the only form the
/// UART log and the `wifiscan` output share.
fn draw_signal(framebuffer: &mut Framebuffer, y: usize, rssi: i32) {
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
        let colour = if index < lit { PRIMARY } else { BAR_EMPTY };
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
        MUTED,
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
    framebuffer.draw_text(x, y, text, 1, colour, None);
    if bold {
        framebuffer.draw_text(x + 1, y, text, 1, colour, None);
    }
}

fn draw_password_screen(framebuffer: &mut Framebuffer, ssid: &[u8], length: usize) {
    draw_chrome(framebuffer, "WI-FI PASSWORD");
    let mut network = Line::new();
    network.push_str("NETWORK  ");
    network.push_ascii(ssid);
    framebuffer.draw_text(90, 150, network.as_str(), 2, BLACK, None);
    framebuffer.draw_text(90, 226, "PASSWORD", 1, MUTED, None);
    framebuffer.fill_rect(90, 258, 900, 64, BLACK);
    framebuffer.fill_rect(92, 260, 896, 60, WHITE);

    let mut masked = [b'*'; wifi::station::PASSWORD_MAX_BYTES];
    let masked_text = core::str::from_utf8(&masked[..length]).unwrap_or("");
    framebuffer.draw_text(106, 276, masked_text, 1, BLACK, Some(WHITE));
    // Do not let the display copy be mistaken for credential storage either.
    zeroize(&mut masked);

    let mut count = Line::new();
    count.push_u32(length as u32);
    count.push_str(" / 64 BYTES");
    framebuffer.draw_text(1010, 279, count.as_str(), 1, PRIMARY, None);
    framebuffer.draw_text(
        90,
        370,
        "ENTER CONNECT    BACKSPACE DELETE    ESC CANCEL",
        1,
        PRIMARY,
        None,
    );
    framebuffer.draw_text(
        90,
        414,
        "THE PASSWORD IS NOT WRITTEN TO THE CONSOLE OR UART LOG",
        1,
        MUTED,
        None,
    );
    flush(framebuffer, b"WIFI MENU: password-screen flush failed\r\n");
}

fn draw_profile_choice(framebuffer: &mut Framebuffer, ssid: &[u8], choice: ProfileChoice) {
    draw_chrome(framebuffer, "WI-FI PROFILE");
    let mut network = Line::new();
    network.push_str("NETWORK  ");
    network.push_ascii(ssid);
    framebuffer.draw_text(90, 140, network.as_str(), 2, BLACK, None);

    let save_selected = choice == ProfileChoice::SaveAndAutoConnect;
    framebuffer.fill_rect(
        90,
        230,
        1050,
        62,
        if save_selected { SELECTED } else { PANEL },
    );
    framebuffer.draw_text(110, 248, "SAVE AND AUTO-CONNECT", 2, BLACK, None);
    framebuffer.fill_rect(
        90,
        312,
        1050,
        62,
        if save_selected { PANEL } else { SELECTED },
    );
    framebuffer.draw_text(110, 330, "CONNECT ONCE", 2, BLACK, None);
    framebuffer.draw_text(
        90,
        420,
        "UP/DOWN SELECT    ENTER CONFIRM    ESC CANCEL",
        1,
        PRIMARY,
        None,
    );
    framebuffer.draw_text(
        90,
        464,
        "THE PROFILE IS SAVED ONLY AFTER ASSOCIATION SUCCEEDS",
        1,
        MUTED,
        None,
    );
    flush(framebuffer, b"WIFI MENU: profile-screen flush failed\r\n");
}

fn draw_off_screen(framebuffer: &mut Framebuffer, saved_profile: bool, message: Option<&str>) {
    draw_chrome(framebuffer, "WI-FI NETWORKS");
    framebuffer.draw_text(1030, 30, "OFF", 1, WARNING, None);
    framebuffer.draw_text(500, 155, "WI-FI IS OFF", 2, WARNING, None);
    framebuffer.draw_text(
        405,
        220,
        if saved_profile {
            "SAVED PROFILE: YES"
        } else {
            "SAVED PROFILE: NO"
        },
        1,
        BLACK,
        None,
    );
    framebuffer.fill_rect(220, 290, 370, 100, SELECTED);
    framebuffer.draw_text(328, 326, "O  TURN ON", 2, BLACK, None);
    framebuffer.fill_rect(690, 290, 370, 100, PANEL);
    framebuffer.draw_text(760, 326, "F  FORGET PROFILE", 2, BLACK, None);
    framebuffer.draw_text(
        430,
        FOOTER_TOP,
        "TOUCH A BUTTON OR PRESS O/F",
        1,
        PRIMARY,
        None,
    );
    framebuffer.draw_text(550, FOOTER_TOP + 32, "ESC EXIT", 1, MUTED, None);
    if let Some(message) = message {
        framebuffer.draw_text(90, 450, message, 1, RED, None);
    }
    flush(framebuffer, b"WIFI MENU: off-screen flush failed\r\n");
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
    let drawn = crate::font::text_width(text) * scale;
    let x = left + width.saturating_sub(drawn) / 2;
    framebuffer.draw_text(x, y, text, scale, color, None);
}

/// Centred across the whole screen.
fn centred(framebuffer: &mut Framebuffer, y: usize, text: &str, scale: usize, color: u16) {
    centred_in(framebuffer, 0, WIDTH, y, text, scale, color);
}

fn confirm_forget(
    framebuffer: &mut Framebuffer,
    input: &mut InputManager,
    manager: &mut Manager,
) -> bool {
    draw_chrome(framebuffer, "FORGET WI-FI PROFILE?");
    centred(
        framebuffer,
        180,
        "THE SAVED SSID AND PASSWORD WILL BE DELETED",
        1,
        WARNING,
    );
    framebuffer.fill_rect(100, 300, 470, 90, RED);
    centred_in(framebuffer, 100, 470, 332, "Y  FORGET", 2, WHITE);
    framebuffer.fill_rect(700, 300, 470, 90, PANEL);
    centred_in(framebuffer, 700, 470, 332, "N  CANCEL", 2, BLACK);
    centred(
        framebuffer,
        FOOTER_TOP,
        "TOUCH A BUTTON OR PRESS Y/N",
        1,
        PRIMARY,
    );
    flush(framebuffer, b"WIFI MENU: forget-confirm flush failed\r\n");

    loop {
        let Some((key, touch)) = wait_input_frame(input, manager) else {
            return false;
        };
        match key {
            Some(Key::Ascii(b'y' | b'Y')) => return true,
            Some(Key::Ascii(b'n' | b'N')) | Some(Key::Escape) => return false,
            _ => {}
        }
        if let PrimaryTouch::Pressed(point) = touch {
            if (100..=570).contains(&point.x) && (300..=390).contains(&point.y) {
                return true;
            }
            if (700..=1170).contains(&point.x) && (300..=390).contains(&point.y) {
                return false;
            }
        }
    }
}

fn draw_chrome(framebuffer: &mut Framebuffer, title: &str) {
    framebuffer.fill(BACKGROUND);
    framebuffer.fill_rect(0, 0, WIDTH, HEADER_HEIGHT, HEADER);
    framebuffer.draw_text(28, 24, title, 2, BLACK, None);
    framebuffer.fill_rect(
        0,
        FOOTER_TOP - 16,
        WIDTH,
        HEIGHT - (FOOTER_TOP - 16),
        FOOTER,
    );
}

fn show_progress(framebuffer: &mut Framebuffer, title: &str, detail: &str) {
    draw_chrome(framebuffer, "WI-FI SETUP");
    framebuffer.draw_text(90, 235, title, 2, PRIMARY, None);
    framebuffer.draw_text(90, 318, detail, 1, BLACK, None);
    flush(framebuffer, b"WIFI MENU: progress-screen flush failed\r\n");
}

fn show_error(framebuffer: &mut Framebuffer, title: &str, detail: &Line, instruction: &str) {
    draw_chrome(framebuffer, "WI-FI SETUP");
    framebuffer.draw_text(90, 220, title, 2, RED, None);
    framebuffer.draw_text(90, 304, detail.as_str(), 1, BLACK, None);
    framebuffer.draw_text(90, 382, instruction, 1, PRIMARY, None);
    flush(framebuffer, b"WIFI MENU: error-screen flush failed\r\n");
}

fn show_result(framebuffer: &mut Framebuffer, result: &ConnectionResult) {
    draw_chrome(framebuffer, "WI-FI SETUP");
    let color = match result.kind {
        ResultKind::Success => SUCCESS,
        ResultKind::Warning => WARNING,
        ResultKind::Error => RED,
    };
    framebuffer.draw_text(90, 220, result.title, 2, color, None);
    framebuffer.draw_text(90, 304, result.detail.as_str(), 1, BLACK, None);
    framebuffer.draw_text(
        90,
        382,
        "ENTER AP LIST    R RESCAN    ESC EXIT",
        1,
        PRIMARY,
        None,
    );
    flush(framebuffer, b"WIFI MENU: result-screen flush failed\r\n");
}

fn flush(framebuffer: &Framebuffer, failure: &[u8]) {
    if !framebuffer.flush() {
        uart::log(failure);
    }
}

fn push_ipv4(line: &mut Line, address: smoltcp::wire::Ipv4Address) {
    for (index, octet) in address.octets().iter().enumerate() {
        if index != 0 {
            line.push_str(".");
        }
        line.push_u32(*octet as u32);
    }
}

fn zeroize(bytes: &mut [u8]) {
    for byte in bytes {
        // A normal fill may be optimized away once the local buffer is dead.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
}

enum MenuError {
    Manager(Failure),
    ScanRpc,
    ScanStatus(i32),
}

impl MenuError {
    fn line(&self) -> Line {
        let mut line = Line::new();
        match *self {
            Self::Manager(Failure::LinkBringUp) => {
                line.push_str("ESP-HOSTED LINK BRING-UP FAILED; SEE UART LOG")
            }
            Self::Manager(Failure::StartRpc) => {
                line.push_str("WI-FI START RPC FAILED; SEE UART LOG")
            }
            Self::Manager(Failure::StartStatus(status)) => {
                push_status(&mut line, "WI-FI START", status)
            }
            Self::Manager(_) => line.push_str("WI-FI MANAGER FAILED; SEE UART LOG"),
            Self::ScanRpc => line.push_str("SCAN RPC FAILED; SEE UART LOG"),
            Self::ScanStatus(status) => push_status(&mut line, "SCAN", status),
        }
        line
    }
}

fn push_status(line: &mut Line, operation: &str, status: i32) {
    line.push_str(operation);
    line.push_str(" SLAVE STATUS 0X");
    line.push_hex(status as u32, 8);
}

struct ConnectionResult {
    kind: ResultKind,
    title: &'static str,
    detail: Line,
}

impl ConnectionResult {
    fn error(title: &'static str, detail: &'static str) -> Self {
        let mut line = Line::new();
        line.push_str(detail);
        Self {
            kind: ResultKind::Error,
            title,
            detail: line,
        }
    }

    fn status(title: &'static str, status: i32) -> Self {
        let mut line = Line::new();
        push_status(&mut line, "REQUEST", status);
        Self {
            kind: ResultKind::Error,
            title,
            detail: line,
        }
    }
}

enum ResultKind {
    Success,
    Warning,
    Error,
}

enum ResultAction {
    Exit,
    Rescan,
    List,
}
