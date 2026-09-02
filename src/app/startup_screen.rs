//! White startup screen and its frame-driven USB/Wi-Fi sub-apps.

use crate::framebuffer::{BLACK, BLUE, Framebuffer, GREEN, HEIGHT, RED, WHITE, WIDTH};
use crate::fs::RamBlockDevice;
use crate::fs::vfs::Vfs;
use crate::input::{InputManager, Key};
use crate::lcd::Display;
use crate::{font, tick, uart, usb};

use super::automount::AutoMount;
use super::wifi_manager::{Failure, Manager as WifiManager, State as WifiState};

pub const STARTUP_CANCEL_HINT_MS: u64 = 5_000;
const USB_DISCOVERY_BUDGET_MS: u64 = 2_000;
const USB_PROBE_INTERVAL_MS: u64 = 100;
const USB_PROBE_CONNECT_WAIT_MS: u32 = 1;
const WIFI_BEGIN_RETRY_MS: u64 = 500;
const WIFI_BEGIN_MAX_ATTEMPTS: u32 = 3;

const ICON_SIZE: usize = 64;
const ICON_PIXELS: usize = ICON_SIZE * ICON_SIZE;
// The icon row is centred three quarters of the way down the screen; the
// title keeps the distance it always had above it.
const ICON_TOP: usize = HEIGHT * 3 / 4 - ICON_SIZE / 2;
const TITLE_Y: usize = HEIGHT / 2 - font::HEIGHT * 2;
const ICON_CELL_WIDTH: usize = 176;
const USB_CELL_LEFT: usize = WIDTH / 2 - ICON_CELL_WIDTH;
const WIFI_CELL_LEFT: usize = WIDTH / 2;
const DETAIL_Y: usize = ICON_TOP + ICON_SIZE + 12;
const CANCEL_Y: usize = HEIGHT - font::HEIGHT * 2;
const CANCEL_TEXT: &str = "ESC  CANCEL STARTUP AND OPEN CONSOLE";

const MUTED: u16 = 0x8410;
const AMBER: u16 = 0xA500;

// The user-supplied 240 px PNGs are kept beside these generated masks as
// 64 px review assets. The firmware needs no PNG decoder: it links only the
// resized alpha planes and colours them for the current startup phase.
const USB_ICON_ALPHA: &[u8; ICON_PIXELS] = include_bytes!("../../assets/startup/usb-64-alpha.bin");
const WIFI_ICON_ALPHA: &[u8; ICON_PIXELS] =
    include_bytes!("../../assets/startup/wifi-64-alpha.bin");

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Phase {
    Pending,
    Running,
    Succeeded,
    Warning,
    Failed,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Visual {
    phase: Phase,
    detail: &'static str,
}

impl Visual {
    const fn pending() -> Self {
        Self {
            phase: Phase::Pending,
            detail: "WAITING",
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum InitialRoute {
    Browser,
    WifiMenu,
    Console,
}

pub fn draw_initial(framebuffer: &mut Framebuffer) -> bool {
    framebuffer.fill(WHITE);
    centred(framebuffer, TITLE_Y, "パソコンを起動しています…", 2, BLACK);
    draw_cell(framebuffer, USB_CELL_LEFT, Visual::pending(), true);
    draw_cell(framebuffer, WIFI_CELL_LEFT, Visual::pending(), false);
    framebuffer.flush()
}

pub fn run(
    display: &mut Display,
    input: &mut InputManager,
    auto_mount: &mut AutoMount,
    vfs: &mut Vfs,
    mut ram_disk: Option<&mut RamBlockDevice>,
    wifi: &mut WifiManager,
    screen_shown_ms: u64,
) -> InitialRoute {
    let started_ms = tick::now_ms();
    let mut screen = Screen::new();
    let mut usb_app = UsbStartup::new(started_ms);
    let mut wifi_app = WifiStartup::new();
    wifi.begin_startup_retry_policy();

    // Publish the running state before either synchronous low-level step is
    // attempted. A connected USB device can take noticeably longer to
    // enumerate than an empty root-port probe.
    let framebuffer = display.framebuffer_mut();
    screen.update_usb(
        framebuffer,
        Visual {
            phase: Phase::Running,
            detail: "LOOKING FOR DEVICES",
        },
    );
    screen.update_wifi(
        framebuffer,
        Visual {
            phase: Phase::Running,
            detail: "STARTING RADIO",
        },
    );

    loop {
        if display
            .wait_for_frame_with(|| input.service_fast())
            .is_none()
        {
            usb_app.cancel(input);
            wifi.finish_startup_retry_policy();
            return InitialRoute::Console;
        }

        let framebuffer = display.framebuffer_mut();
        if !screen.cancel_visible
            && tick::now_ms().saturating_sub(screen_shown_ms) >= STARTUP_CANCEL_HINT_MS
        {
            screen.show_cancel(framebuffer);
        }

        while let Some(event) = input.poll_key() {
            if screen.cancel_visible && event.key == Key::Escape {
                uart::log(b"STARTUP: cancelled by Escape\r\n");
                usb_app.cancel(input);
                wifi.finish_startup_retry_policy();
                return InitialRoute::Console;
            }
        }

        let usb_visual = usb_app.poll(framebuffer, input, auto_mount, vfs, ram_disk.as_deref_mut());
        let wifi_visual = wifi_app.poll(wifi);
        screen.update_usb(framebuffer, usb_visual);
        screen.update_wifi(framebuffer, wifi_visual);

        if usb_app.is_done() && wifi_app.is_done() {
            wifi.finish_startup_retry_policy();
            return if wifi_app.is_online() {
                InitialRoute::Browser
            } else {
                InitialRoute::WifiMenu
            };
        }
    }
}

struct Screen {
    usb: Visual,
    wifi: Visual,
    cancel_visible: bool,
}

impl Screen {
    const fn new() -> Self {
        Self {
            usb: Visual::pending(),
            wifi: Visual::pending(),
            cancel_visible: false,
        }
    }

    fn update_usb(&mut self, framebuffer: &mut Framebuffer, visual: Visual) {
        if self.usb == visual {
            return;
        }
        self.usb = visual;
        draw_cell(framebuffer, USB_CELL_LEFT, visual, true);
        flush_cell(
            framebuffer,
            USB_CELL_LEFT,
            b"STARTUP: USB icon flush failed\r\n",
        );
    }

    fn update_wifi(&mut self, framebuffer: &mut Framebuffer, visual: Visual) {
        if self.wifi == visual {
            return;
        }
        self.wifi = visual;
        draw_cell(framebuffer, WIFI_CELL_LEFT, visual, false);
        flush_cell(
            framebuffer,
            WIFI_CELL_LEFT,
            b"STARTUP: Wi-Fi icon flush failed\r\n",
        );
    }

    fn show_cancel(&mut self, framebuffer: &mut Framebuffer) {
        self.cancel_visible = true;
        framebuffer.fill_rect(0, CANCEL_Y, WIDTH, font::HEIGHT, WHITE);
        centred(framebuffer, CANCEL_Y, CANCEL_TEXT, 1, MUTED);
        if !framebuffer.flush_rect(0, CANCEL_Y, WIDTH, font::HEIGHT) {
            uart::log(b"STARTUP: cancel hint flush failed\r\n");
        }
    }
}

struct UsbStartup {
    started_ms: u64,
    next_probe_ms: u64,
    scan_finished: bool,
    finalized: bool,
    enumeration_warning: bool,
    mount_warning: bool,
    done: bool,
}

impl UsbStartup {
    const fn new(started_ms: u64) -> Self {
        Self {
            started_ms,
            next_probe_ms: started_ms,
            scan_finished: false,
            finalized: false,
            enumeration_warning: false,
            mount_warning: false,
            done: false,
        }
    }

    fn poll(
        &mut self,
        framebuffer: &mut Framebuffer,
        input: &mut InputManager,
        auto_mount: &mut AutoMount,
        vfs: &mut Vfs,
        ram_disk: Option<&mut RamBlockDevice>,
    ) -> Visual {
        if self.done {
            return self.service_after_scan(framebuffer, input, auto_mount, vfs, ram_disk);
        }

        let now = tick::now_ms();
        if !self.scan_finished && now >= self.next_probe_ms {
            let previous_wait = usb::set_connect_wait_ms(USB_PROBE_CONNECT_WAIT_MS);
            input.usb_host_mut().rescan(usb::RescanReason::Manual);
            usb::set_connect_wait_ms(previous_wait);

            let (connected, enabled) = input
                .usb_host_mut()
                .last_probe()
                .map(|port| (port.connected, port.enabled))
                .unwrap_or((false, false));
            let enumerated =
                connected && enabled && input.usb_host_mut().bus_devices().next().is_some();
            let budget_expired =
                tick::now_ms().saturating_sub(self.started_ms) >= USB_DISCOVERY_BUDGET_MS;
            if enumerated {
                self.scan_finished = true;
                self.enumeration_warning = false;
            } else if budget_expired {
                self.scan_finished = true;
                self.enumeration_warning = connected;
            } else {
                self.next_probe_ms = tick::now_ms().saturating_add(USB_PROBE_INTERVAL_MS);
            }
        }

        if !self.scan_finished {
            return Visual {
                phase: Phase::Running,
                detail: "LOOKING FOR DEVICES",
            };
        }

        if !self.finalized {
            input.finish_boot_usb_scan(self.started_ms);
            self.finalized = true;
        }

        self.service_after_scan(framebuffer, input, auto_mount, vfs, ram_disk)
    }

    /// Keeps the ordinary USB lifecycle alive after the boot verdict.
    ///
    /// A warning is a startup result, not permission to stop handling the
    /// bus. Physical connection events are therefore consumed immediately,
    /// and a failed initial enumeration enters the normal fallback-rescan
    /// path while Wi-Fi may still be keeping the splash screen open.
    fn service_after_scan(
        &mut self,
        framebuffer: &mut Framebuffer,
        input: &mut InputManager,
        auto_mount: &mut AutoMount,
        vfs: &mut Vfs,
        ram_disk: Option<&mut RamBlockDevice>,
    ) -> Visual {
        input.service();
        let connected = input
            .usb_host_mut()
            .last_probe()
            .is_some_and(|port| port.connected);
        if input.usb_host_mut().bus_devices().next().is_some() {
            self.enumeration_warning = false;
        } else if connected {
            self.enumeration_warning = true;
        } else {
            self.enumeration_warning = false;
        }
        self.mount_warning |=
            auto_mount.service_silent(framebuffer, vfs, ram_disk, input.usb_host_mut());
        if auto_mount.has_pending() {
            self.done = false;
            return Visual {
                phase: Phase::Running,
                detail: "MOUNTING STORAGE",
            };
        }

        self.done = true;
        self.terminal_visual()
    }

    fn cancel(&mut self, input: &mut InputManager) {
        if !self.finalized {
            input.finish_boot_usb_scan(self.started_ms);
            self.finalized = true;
        }
        self.done = true;
    }

    const fn terminal_visual(&self) -> Visual {
        if self.enumeration_warning || self.mount_warning {
            Visual {
                phase: Phase::Warning,
                detail: "ENUMERATION WARNING",
            }
        } else {
            Visual {
                phase: Phase::Succeeded,
                detail: "READY",
            }
        }
    }

    const fn is_done(&self) -> bool {
        self.done
    }
}

struct WifiStartup {
    begin_attempts: u32,
    next_begin_ms: u64,
    begun: bool,
    done: bool,
    online: bool,
    failed: bool,
}

impl WifiStartup {
    const fn new() -> Self {
        Self {
            begin_attempts: 0,
            next_begin_ms: 0,
            begun: false,
            done: false,
            online: false,
            failed: false,
        }
    }

    fn poll(&mut self, manager: &mut WifiManager) -> Visual {
        if self.done {
            if !self.online {
                return self.terminal_visual();
            }

            // USB mounting can outlive Wi-Fi setup. Keep the connection
            // manager alive until the whole splash screen has finished, and
            // withdraw the successful result if the link drops meanwhile.
            manager.service();
            if matches!(manager.state(), WifiState::Online(_)) {
                return self.terminal_visual();
            }
            self.done = false;
            self.online = false;
        }

        let now = tick::now_ms();
        if !self.begun && now >= self.next_begin_ms {
            self.begin_attempts = self.begin_attempts.saturating_add(1);
            match manager.begin_startup_auto_connect() {
                Ok(true) => {
                    uart::log(b"WIFI: saved profile auto-connect started\r\n");
                    self.begun = true;
                }
                Ok(false) => {
                    if manager.is_enabled() {
                        uart::log(b"WIFI: no saved profile\r\n");
                    } else {
                        uart::log(b"WIFI: persistent OFF; C6 powered down\r\n");
                    }
                    self.done = true;
                    return self.terminal_visual();
                }
                Err(failure)
                    if begin_failure_retryable(failure)
                        && self.begin_attempts < WIFI_BEGIN_MAX_ATTEMPTS =>
                {
                    uart::log(b"WIFI: startup probe failed; retrying\r\n");
                    self.next_begin_ms = now.saturating_add(WIFI_BEGIN_RETRY_MS);
                }
                Err(_) => {
                    uart::log(b"WIFI: saved profile probe failed\r\n");
                    self.failed = true;
                    self.done = true;
                    return self.terminal_visual();
                }
            }
        }

        if !self.begun {
            return Visual {
                phase: Phase::Running,
                detail: "STARTING RADIO",
            };
        }

        manager.service();
        match manager.state() {
            WifiState::Online(_) => {
                self.online = true;
                self.done = true;
            }
            WifiState::Off | WifiState::Idle | WifiState::AssociatedNoLease(_) => {
                self.done = true;
            }
            WifiState::NeedsPassword(_) | WifiState::Failed(_) => {
                self.failed = true;
                self.done = true;
            }
            _ => {}
        }

        if self.done {
            self.terminal_visual()
        } else {
            running_wifi_visual(manager.state())
        }
    }

    const fn terminal_visual(&self) -> Visual {
        if self.online {
            Visual {
                phase: Phase::Succeeded,
                detail: "ONLINE",
            }
        } else if self.failed {
            Visual {
                phase: Phase::Failed,
                detail: "SETUP REQUIRED",
            }
        } else {
            Visual {
                phase: Phase::Warning,
                detail: "SETUP REQUIRED",
            }
        }
    }

    const fn is_done(&self) -> bool {
        self.done
    }

    const fn is_online(&self) -> bool {
        self.online
    }
}

fn running_wifi_visual(state: WifiState) -> Visual {
    let detail = match state {
        WifiState::LinkDown => "STARTING RADIO",
        WifiState::Associating { .. } => "CONNECTING",
        WifiState::RetryWaiting { .. } => "RETRYING",
        WifiState::Associated(_) => "CONNECTED",
        WifiState::RequestingDhcp { .. } => "WAITING FOR DHCP",
        _ => "INITIALIZING",
    };
    Visual {
        phase: Phase::Running,
        detail,
    }
}

const fn begin_failure_retryable(failure: Failure) -> bool {
    matches!(
        failure,
        Failure::LinkBringUp
            | Failure::StartRpc
            | Failure::ConfigRpc
            | Failure::ModeRpc
            | Failure::LinkLost
    )
}

fn draw_cell(framebuffer: &mut Framebuffer, cell_left: usize, visual: Visual, usb_icon: bool) {
    framebuffer.fill_rect(
        cell_left,
        ICON_TOP - 8,
        ICON_CELL_WIDTH,
        DETAIL_Y + font::HEIGHT - (ICON_TOP - 8),
        WHITE,
    );
    let icon_left = cell_left + (ICON_CELL_WIDTH - ICON_SIZE) / 2;
    let alpha = if usb_icon {
        USB_ICON_ALPHA
    } else {
        WIFI_ICON_ALPHA
    };
    draw_bitmap_icon(framebuffer, icon_left, ICON_TOP, visual.phase, alpha);
    centred_in(
        framebuffer,
        cell_left,
        ICON_CELL_WIDTH,
        DETAIL_Y,
        visual.detail,
        1,
        phase_color(visual.phase),
    );
}

fn draw_bitmap_icon(
    framebuffer: &mut Framebuffer,
    left: usize,
    top: usize,
    phase: Phase,
    alpha: &[u8; ICON_PIXELS],
) {
    let foreground = icon_color(phase);
    for (index, &coverage) in alpha.iter().enumerate() {
        if coverage == 0 {
            continue;
        }
        framebuffer.draw_pixel(
            left + index % ICON_SIZE,
            top + index / ICON_SIZE,
            blend_over_white(foreground, coverage),
        );
    }
    draw_marker(framebuffer, left + 47, top + 44, phase);
}

fn blend_over_white(color: u16, alpha: u8) -> u16 {
    let alpha = alpha as u32;
    let inverse = 255 - alpha;
    let red = (((color >> 11) & 0x1f) as u32 * alpha + 31 * inverse + 127) / 255;
    let green = (((color >> 5) & 0x3f) as u32 * alpha + 63 * inverse + 127) / 255;
    let blue = ((color & 0x1f) as u32 * alpha + 31 * inverse + 127) / 255;
    ((red << 11) | (green << 5) | blue) as u16
}

fn draw_marker(framebuffer: &mut Framebuffer, left: usize, top: usize, phase: Phase) {
    match phase {
        Phase::Pending => {}
        Phase::Running => framebuffer.fill_rect(left, top, 12, 12, BLUE),
        Phase::Succeeded => {
            framebuffer.draw_line(left, top + 6, left + 4, top + 11, GREEN);
            framebuffer.draw_line(left + 4, top + 11, left + 13, top, GREEN);
            framebuffer.draw_line(left, top + 5, left + 4, top + 10, GREEN);
        }
        Phase::Warning => {
            framebuffer.fill_rect(left, top, 14, 14, AMBER);
            framebuffer.draw_text(left + 3, top, "!", 1, WHITE, None);
        }
        Phase::Failed => {
            framebuffer.fill_rect(left, top, 14, 14, RED);
            framebuffer.draw_text(left + 3, top, "!", 1, WHITE, None);
        }
    }
}

const fn icon_color(phase: Phase) -> u16 {
    match phase {
        Phase::Pending => 0xBDF7,
        _ => BLACK,
    }
}

const fn phase_color(phase: Phase) -> u16 {
    match phase {
        Phase::Pending => MUTED,
        Phase::Running => BLUE,
        Phase::Succeeded => 0x0400,
        Phase::Warning => AMBER,
        Phase::Failed => RED,
    }
}

fn flush_cell(framebuffer: &Framebuffer, left: usize, failure: &[u8]) {
    if !framebuffer.flush_rect(
        left,
        ICON_TOP - 8,
        ICON_CELL_WIDTH,
        DETAIL_Y + font::HEIGHT - (ICON_TOP - 8),
    ) {
        uart::log(failure);
    }
}

fn centred(framebuffer: &mut Framebuffer, y: usize, text: &str, scale: usize, color: u16) {
    centred_in(framebuffer, 0, WIDTH, y, text, scale, color);
}

fn centred_in(
    framebuffer: &mut Framebuffer,
    left: usize,
    width: usize,
    y: usize,
    text: &str,
    scale: usize,
    color: u16,
) {
    let drawn = font::text_width(text) * scale;
    let x = left + width.saturating_sub(drawn) / 2;
    framebuffer.draw_text(x, y, text, scale, color, None);
}
