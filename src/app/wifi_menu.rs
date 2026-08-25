//! Minimal keyboard-driven Wi-Fi setup screen.
//!
//! This deliberately reuses the existing blocking station and DHCP calls.
//! The first milestone is only the repetitive setup path -- scan, choose,
//! enter a password, associate, obtain a lease -- not the background
//! connection manager planned in `docs/WIFI_REFACTOR_PLAN.md` Stage 3.

use alloc::vec::Vec;

use crate::framebuffer::{BLACK, CYAN, Framebuffer, GREEN, HEIGHT, RED, WHITE, WIDTH, YELLOW};
use crate::input::{InputManager, Key};
use crate::{interrupts, net, tick, uart, wifi};

use super::shell::Line;

const BACKGROUND: u16 = 0x1082;
const HEADER: u16 = 0x0010;
const PANEL: u16 = 0x2104;
const SELECTED: u16 = 0x7D7C;
const MUTED: u16 = 0xAD55;

const HEADER_HEIGHT: usize = 78;
const LIST_TOP: usize = 94;
const LIST_BOTTOM: usize = 604;
const ROW_HEIGHT: usize = 32;
const VISIBLE_ROWS: usize = (LIST_BOTTOM - LIST_TOP) / ROW_HEIGHT;
const FOOTER_TOP: usize = 620;

const CONNECT_TIMEOUT_MS: u32 = 20_000;
const DHCP_TIMEOUT_MS: u64 = 15_000;

/// Runs until Escape leaves the screen.
///
/// The session and stack are borrowed from `app::run`, so a connection made
/// here remains usable by the shell after this function returns.
pub fn run(
    framebuffer: &mut Framebuffer,
    input: &mut InputManager,
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) {
    uart::log(b"WIFI MENU: opened\r\n");
    let mut selected = 0usize;
    let mut first = 0usize;

    'scan: loop {
        let access_points = loop {
            show_progress(
                framebuffer,
                "SCANNING FOR ACCESS POINTS",
                "THIS MAY TAKE A FEW SECONDS",
            );
            match scan(session, stack) {
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
                        match wait_key(input, session, stack) {
                            Some(Key::Ascii(b'r' | b'R')) => break,
                            Some(Key::Escape) | None => return,
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
            draw_access_points(framebuffer, &access_points, selected, first, None);
            let Some(key) = wait_key(input, session, stack) else {
                return;
            };
            match key {
                Key::Escape => return,
                Key::Ascii(b'r' | b'R') => {
                    selected = 0;
                    first = 0;
                    continue 'scan;
                }
                Key::ArrowUp if !access_points.is_empty() => {
                    selected = selected.saturating_sub(1);
                    keep_visible(selected, access_points.len(), &mut first);
                }
                Key::ArrowDown if !access_points.is_empty() => {
                    selected = (selected + 1).min(access_points.len() - 1);
                    keep_visible(selected, access_points.len(), &mut first);
                }
                Key::Ascii(b'\r' | b'\n') if !access_points.is_empty() => {
                    let access_point = &access_points[selected];
                    if access_point.ssid().is_empty() {
                        draw_access_points(
                            framebuffer,
                            &access_points,
                            selected,
                            first,
                            Some("HIDDEN SSIDS CANNOT BE SELECTED IN THIS VERSION"),
                        );
                        let _ = wait_key(input, session, stack);
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
                            session,
                            stack,
                            &ssid[..ssid_length],
                            &mut password,
                        )
                    };

                    let Some(password_length) = password_length else {
                        zeroize(&mut password);
                        continue;
                    };
                    let result = connect_and_configure(
                        framebuffer,
                        session,
                        stack,
                        &ssid[..ssid_length],
                        &password[..password_length],
                    );
                    zeroize(&mut password);
                    show_result(framebuffer, &result);
                    match wait_result_action(input, session, stack) {
                        ResultAction::Exit => return,
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

fn scan(
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) -> Result<Vec<wifi::station::AccessPoint>, MenuError> {
    let rpc = ensure_session(session, stack)?;
    let Some((status, access_points)) = wifi::station::scan(rpc) else {
        return Err(MenuError::ScanRpc);
    };
    if status != 0 {
        return Err(MenuError::ScanStatus(status));
    }
    uart::log_u32(b"WIFI MENU: access points=", access_points.len() as u32);
    Ok(access_points)
}

fn ensure_session<'a>(
    session: &'a mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) -> Result<&'a mut wifi::Rpc, MenuError> {
    if session.as_ref().is_some_and(|rpc| !rpc.is_alive()) {
        *session = None;
        *stack = None;
    }
    if session.is_none() {
        let Some((transport, _)) = wifi::bring_up() else {
            return Err(MenuError::LinkBringUp);
        };
        let mut rpc = wifi::Rpc::new(transport);
        match wifi::station::start(&mut rpc) {
            Some(0) => {}
            Some(status) => return Err(MenuError::StartStatus(status)),
            None => return Err(MenuError::StartRpc),
        }
        *session = Some(rpc);
    }
    session.as_mut().ok_or(MenuError::LinkBringUp)
}

fn edit_password(
    framebuffer: &mut Framebuffer,
    input: &mut InputManager,
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
    ssid: &[u8],
    password: &mut [u8; wifi::station::PASSWORD_MAX_BYTES],
) -> Option<usize> {
    let mut length = 0usize;
    draw_password_screen(framebuffer, ssid, length);
    loop {
        let key = wait_key(input, session, stack)?;
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

fn connect_and_configure(
    framebuffer: &mut Framebuffer,
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
    ssid: &[u8],
    password: &[u8],
) -> ConnectionResult {
    let mut detail = Line::new();
    detail.push_str("SSID ");
    detail.push_ascii(ssid);
    show_progress(framebuffer, "ASSOCIATING", detail.as_str());

    // Once a new association is attempted, an address from the old network
    // must not survive even if this attempt fails halfway through.
    *stack = None;
    let Some(rpc) = session.as_mut() else {
        return ConnectionResult::error("NO C6 SESSION", "RETURN TO THE LIST AND RESCAN");
    };
    match wifi::station::connect(rpc, ssid, password) {
        Some(0) => {}
        Some(status) => {
            return ConnectionResult::status("CONNECT REFUSED", status);
        }
        None => return ConnectionResult::error("CONNECT RPC FAILED", "SEE UART LOG"),
    }

    let connected = match wifi::station::wait_for_connection(rpc, CONNECT_TIMEOUT_MS) {
        wifi::station::Outcome::Connected {
            ssid,
            ssid_length,
            channel,
            ..
        } => {
            let mut line = Line::new();
            line.push_str("SSID ");
            line.push_ascii(&ssid[..ssid_length]);
            line.push_str("  CHANNEL ");
            line.push_u32(channel);
            line
        }
        wifi::station::Outcome::Disconnected { reason } => {
            let mut line = Line::new();
            line.push_str("REASON ");
            line.push_u32(reason);
            if let Some(name) = wifi::station::disconnect_reason_name(reason) {
                line.push_str("  ");
                line.push_str(name);
            }
            return ConnectionResult {
                kind: ResultKind::Error,
                title: "ASSOCIATION FAILED",
                detail: line,
            };
        }
        wifi::station::Outcome::TimedOut => {
            return ConnectionResult::error("ASSOCIATION TIMED OUT", "NO EVENT FROM THE C6");
        }
    };

    if !tick::is_running() {
        return ConnectionResult::error(
            "ASSOCIATED, NO IP STACK",
            "MILLISECOND TICK IS NOT RUNNING",
        );
    }
    let Some((status, mac)) = wifi::rpc::get_mac_address(rpc, wifi::rpc::WIFI_IF_STA) else {
        return ConnectionResult::error("ASSOCIATED, NO IP STACK", "STATION MAC RPC FAILED");
    };
    if status != 0 {
        return ConnectionResult::status("STATION MAC REFUSED", status);
    }

    show_progress(framebuffer, "REQUESTING DHCP LEASE", connected.as_str());
    let mut candidate = net::Stack::new(rpc, mac);
    candidate.start_dhcp();
    let acquired = candidate.pump_until(rpc, DHCP_TIMEOUT_MS, |stack| stack.has_address());
    let alive = rpc.is_alive();

    if !alive {
        *session = None;
        return ConnectionResult::error("C6 LINK LOST", "DHCP DID NOT COMPLETE");
    }

    if acquired {
        let mut line = connected;
        if let Some(config) = candidate.config() {
            line.push_str("  IP ");
            push_ipv4(&mut line, config.address.address());
        }
        *stack = Some(candidate);
        uart::log(b"WIFI MENU: associated and DHCP configured\r\n");
        ConnectionResult {
            kind: ResultKind::Success,
            title: "ONLINE",
            detail: line,
        }
    } else {
        *stack = Some(candidate);
        let mut line = connected;
        line.push_str("  DHCP STILL PENDING");
        ConnectionResult {
            kind: ResultKind::Warning,
            title: "ASSOCIATED, NO LEASE YET",
            detail: line,
        }
    }
}

fn service_link(session: &mut Option<wifi::Rpc>, stack: &mut Option<net::Stack>) {
    if session.as_ref().is_some_and(|rpc| !rpc.is_alive()) {
        *session = None;
        *stack = None;
        return;
    }
    match (session.as_mut(), stack.as_mut()) {
        (Some(rpc), Some(stack)) => {
            stack.poll(rpc);
        }
        (Some(rpc), None) => rpc.discard_station_frames(),
        _ => {}
    }
}

fn wait_key(
    input: &mut InputManager,
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) -> Option<Key> {
    let mut sequence = interrupts::frame_sequence();
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
        service_link(session, stack);
        if let Some(event) = input.poll_key() {
            return Some(event.key);
        }
    }
}

fn wait_result_action(
    input: &mut InputManager,
    session: &mut Option<wifi::Rpc>,
    stack: &mut Option<net::Stack>,
) -> ResultAction {
    loop {
        match wait_key(input, session, stack) {
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
    access_points: &[wifi::station::AccessPoint],
    selected: usize,
    first: usize,
    message: Option<&str>,
) {
    draw_chrome(framebuffer, "WI-FI NETWORKS");
    let mut count = Line::new();
    count.push_u32(access_points.len() as u32);
    count.push_str(" ACCESS POINTS");
    framebuffer.draw_text(930, 30, count.as_str(), 2, MUTED, None);

    if access_points.is_empty() {
        framebuffer.draw_text(350, 285, "NO ACCESS POINTS FOUND", 3, YELLOW, None);
        framebuffer.draw_text(400, 335, "PRESS R TO RESCAN", 2, WHITE, None);
    }

    for (visible, access_point) in access_points
        .iter()
        .skip(first)
        .take(VISIBLE_ROWS)
        .enumerate()
    {
        let index = first + visible;
        let y = LIST_TOP + visible * ROW_HEIGHT;
        let selected_row = index == selected;
        let background = if selected_row { SELECTED } else { PANEL };
        let foreground = if selected_row { BLACK } else { WHITE };
        framebuffer.fill_rect(24, y, WIDTH - 48, ROW_HEIGHT - 3, background);
        let line = access_point_line(access_point);
        framebuffer.draw_text(36, y + 6, line.as_str(), 2, foreground, None);
    }

    framebuffer.draw_text(
        28,
        FOOTER_TOP,
        "UP/DOWN SELECT    ENTER CONNECT    R RESCAN    ESC EXIT",
        2,
        CYAN,
        None,
    );
    if let Some(message) = message {
        framebuffer.draw_text(28, FOOTER_TOP + 32, message, 2, YELLOW, None);
    } else {
        framebuffer.draw_text(
            28,
            FOOTER_TOP + 32,
            "MENU CONNECTIONS REQUEST DHCP AUTOMATICALLY",
            2,
            MUTED,
            None,
        );
    }
    flush(framebuffer, b"WIFI MENU: list flush failed\r\n");
}

fn access_point_line(access_point: &wifi::station::AccessPoint) -> Line {
    let mut line = Line::new();
    if access_point.rssi > -100 {
        line.push_str(" ");
    }
    if access_point.rssi < 0 {
        line.push_str("-");
    }
    line.push_u32(access_point.rssi.unsigned_abs());
    line.push_str(" DBM  CH ");
    line.push_u32(access_point.channel);
    line.push_str("  ");
    match wifi::station::auth_mode_name(access_point.auth_mode) {
        Some(name) => line.push_str(name),
        None => {
            line.push_str("AUTH ");
            line.push_u32(access_point.auth_mode as u32);
        }
    }
    line.push_str("  ");
    if access_point.ssid().is_empty() {
        line.push_str("(HIDDEN - UNAVAILABLE)");
    } else {
        line.push_ascii(access_point.ssid());
    }
    line
}

fn draw_password_screen(framebuffer: &mut Framebuffer, ssid: &[u8], length: usize) {
    draw_chrome(framebuffer, "WI-FI PASSWORD");
    let mut network = Line::new();
    network.push_str("NETWORK  ");
    network.push_ascii(ssid);
    framebuffer.draw_text(90, 150, network.as_str(), 3, WHITE, None);
    framebuffer.draw_text(90, 226, "PASSWORD", 2, MUTED, None);
    framebuffer.fill_rect(90, 258, 900, 64, WHITE);

    let mut masked = [b'*'; wifi::station::PASSWORD_MAX_BYTES];
    let masked_text = core::str::from_utf8(&masked[..length]).unwrap_or("");
    framebuffer.draw_text(106, 276, masked_text, 2, BLACK, Some(WHITE));
    // Do not let the display copy be mistaken for credential storage either.
    zeroize(&mut masked);

    let mut count = Line::new();
    count.push_u32(length as u32);
    count.push_str(" / 64 BYTES");
    framebuffer.draw_text(1010, 279, count.as_str(), 2, CYAN, None);
    framebuffer.draw_text(
        90,
        370,
        "ENTER CONNECT    BACKSPACE DELETE    ESC CANCEL",
        2,
        CYAN,
        None,
    );
    framebuffer.draw_text(
        90,
        414,
        "THE PASSWORD IS NOT WRITTEN TO THE CONSOLE OR UART LOG",
        2,
        MUTED,
        None,
    );
    flush(framebuffer, b"WIFI MENU: password-screen flush failed\r\n");
}

fn draw_chrome(framebuffer: &mut Framebuffer, title: &str) {
    framebuffer.fill(BACKGROUND);
    framebuffer.fill_rect(0, 0, WIDTH, HEADER_HEIGHT, HEADER);
    framebuffer.draw_text(28, 24, title, 3, WHITE, None);
    framebuffer.fill_rect(0, FOOTER_TOP - 16, WIDTH, HEIGHT - (FOOTER_TOP - 16), BLACK);
}

fn show_progress(framebuffer: &mut Framebuffer, title: &str, detail: &str) {
    draw_chrome(framebuffer, "WI-FI SETUP");
    framebuffer.draw_text(90, 235, title, 4, CYAN, None);
    framebuffer.draw_text(90, 318, detail, 2, WHITE, None);
    flush(framebuffer, b"WIFI MENU: progress-screen flush failed\r\n");
}

fn show_error(framebuffer: &mut Framebuffer, title: &str, detail: &Line, instruction: &str) {
    draw_chrome(framebuffer, "WI-FI SETUP");
    framebuffer.draw_text(90, 220, title, 4, RED, None);
    framebuffer.draw_text(90, 304, detail.as_str(), 2, WHITE, None);
    framebuffer.draw_text(90, 382, instruction, 2, CYAN, None);
    flush(framebuffer, b"WIFI MENU: error-screen flush failed\r\n");
}

fn show_result(framebuffer: &mut Framebuffer, result: &ConnectionResult) {
    draw_chrome(framebuffer, "WI-FI SETUP");
    let color = match result.kind {
        ResultKind::Success => GREEN,
        ResultKind::Warning => YELLOW,
        ResultKind::Error => RED,
    };
    framebuffer.draw_text(90, 220, result.title, 4, color, None);
    framebuffer.draw_text(90, 304, result.detail.as_str(), 2, WHITE, None);
    framebuffer.draw_text(
        90,
        382,
        "ENTER AP LIST    R RESCAN    ESC EXIT",
        2,
        CYAN,
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
    LinkBringUp,
    StartRpc,
    StartStatus(i32),
    ScanRpc,
    ScanStatus(i32),
}

impl MenuError {
    fn line(&self) -> Line {
        let mut line = Line::new();
        match *self {
            Self::LinkBringUp => line.push_str("ESP-HOSTED LINK BRING-UP FAILED; SEE UART LOG"),
            Self::StartRpc => line.push_str("WI-FI START RPC FAILED; SEE UART LOG"),
            Self::StartStatus(status) => push_status(&mut line, "WI-FI START", status),
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
