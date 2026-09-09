//! Normal GUI host: one frame loop, one input owner, one foreground screen.
use super::battery_monitor::BatteryMonitor;
use super::theme::{self, BACKGROUND as WHITE, BUTTON_FACE, TEXT as BLACK};
use super::wifi_manager::{Manager, State};
use crate::framebuffer::{Framebuffer, HEIGHT as SCREEN_HEIGHT, WIDTH};
use tab5_system_ui::{APP, BATTERY, CLOCK, HEIGHT, LAUNCHER, Rect, Snapshot, VOLUME, WIFI};

pub fn wifi_indicator(state: State, addressed: bool) -> u8 {
    match state {
        State::Off | State::LinkDown => 0,
        State::Failed(_) | State::NeedsPassword(_) => 0x80,
        State::Idle | State::Associating { .. } | State::RetryWaiting { .. } => 1,
        State::Online(_) if addressed => 3,
        State::Associated(_)
        | State::RequestingDhcp { .. }
        | State::AssociatedNoLease(_)
        | State::Online(_) => 2,
    }
}

/// I2C scheduling is separate from the pure view. Alternate transaction
/// groups, including recovery after a long foreground pause.
pub struct Indicators {
    pub snapshot: Snapshot,
    next_clock: u64,
    battery_turn: bool,
}
impl Indicators {
    pub fn new(now: u64) -> Self {
        Self {
            snapshot: Snapshot::default(),
            next_clock: now.saturating_add(500),
            battery_turn: true,
        }
    }
    pub fn poll(&mut self, now: u64, wifi: &Manager, battery: &mut BatteryMonitor) -> bool {
        self.snapshot.wifi = wifi_indicator(
            wifi.state(),
            wifi.stack().is_some_and(crate::net::Stack::has_address),
        );
        let mut sampled = false;
        if self.battery_turn || now < self.next_clock {
            sampled = battery.poll(now);
            if sampled {
                self.battery_turn = false;
            }
        }
        if !sampled && now >= self.next_clock {
            self.next_clock = now.saturating_add(1000);
            self.battery_turn = true;
            self.snapshot.clock = crate::rtc::read_status()
                .ok()
                .filter(|s| !s.voltage_low() && !s.stopped())
                .and_then(|_| crate::wall_clock::local_now().ok())
                .map(|c| (c.hour, c.minute));
        }
        self.snapshot.battery = battery
            .sample
            .map(|s| tab5_system_ui::battery_level(s.bus_voltage_mv));
        sampled
    }
}

pub struct View {
    painted: Option<Snapshot>,
    slowest_us: [u32; 3],
}
impl View {
    pub fn new() -> Self {
        Self {
            painted: None,
            slowest_us: [0; 3],
        }
    }
    pub fn invalidate(&mut self) {
        self.painted = None;
    }
    pub fn dirty(&self, s: Snapshot) -> bool {
        s.dirty(self.painted).into_iter().any(|v| v)
    }
    pub fn draw(&mut self, fb: &mut Framebuffer, s: Snapshot, launcher: bool) -> bool {
        let dirty = s.dirty(self.painted);
        let mut ok = true;
        if self.painted.is_none() {
            for r in [LAUNCHER, VOLUME] {
                fb.fill_rect(r.x, 0, r.width, HEIGHT, BUTTON_FACE);
            }
            for y in [16, 23, 30] {
                fb.fill_rect(17, y, 18, 2, if launcher { theme::ACCENT } else { BLACK });
            }
            ok &= fb.flush_rect(LAUNCHER.x, 0, LAUNCHER.width, HEIGHT);
            ok &= fb.flush_rect(VOLUME.x, 0, VOLUME.width, HEIGHT);
        }
        for (index, r) in [WIFI, BATTERY, CLOCK].into_iter().enumerate() {
            if !dirty[index] {
                continue;
            }
            let started = crate::delay::cycle_count();
            fb.fill_rect(r.x, 0, r.width, HEIGHT, BUTTON_FACE);
            match index {
                0 => {
                    for i in 0..3 {
                        let h = 6 + i * 5;
                        fb.fill_rect(
                            r.x + 12 + i * 7,
                            32 - h,
                            5,
                            h,
                            if i < (s.wifi & 3) as usize {
                                BLACK
                            } else {
                                theme::INACTIVE
                            },
                        );
                    }
                    if s.wifi & 0x80 != 0 {
                        fb.draw_text(r.x + 34, 16, "!", 1, theme::ERROR, None);
                    }
                }
                1 => {
                    fb.stroke_rect(r.x + 7, 14, 28, 20, BLACK);
                    fb.fill_rect(r.x + 35, 20, 3, 8, BLACK);
                    if let Some(level) = s.battery {
                        if level > 0 {
                            fb.fill_rect(r.x + 10, 17, level as usize * 5, 14, theme::SUCCESS);
                        }
                    } else {
                        fb.draw_text(r.x + 17, 16, "?", 1, BLACK, None);
                    }
                }
                _ => {
                    let mut text = *b"--:--";
                    if let Some((h, m)) = s.clock {
                        text = [
                            b'0' + h / 10,
                            b'0' + h % 10,
                            b':',
                            b'0' + m / 10,
                            b'0' + m % 10,
                        ];
                    }
                    let text = core::str::from_utf8(&text).unwrap_or("--:--");
                    let style = crate::font::UiTextStyle::MONO;
                    let text_width = crate::font::ui_text_width(text, style);
                    fb.draw_ui_text(
                        r.x + r.width.saturating_sub(text_width) / 2,
                        16,
                        text,
                        style,
                        BLACK,
                        None,
                    );
                }
            }
            ok &= fb.flush_rect(r.x, 0, r.width, HEIGHT);
            let us = crate::delay::cycle_count().wrapping_sub(started) / 360;
            if us > self.slowest_us[index] {
                self.slowest_us[index] = us;
                let label: &[u8] = match index {
                    0 => b"SYSTEM BAR: Wi-Fi slot max draw/flush us=",
                    1 => b"SYSTEM BAR: battery slot max draw/flush us=",
                    _ => b"SYSTEM BAR: clock slot max draw/flush us=",
                };
                crate::uart::log_u32(label, us);
            }
        }
        if ok {
            self.painted = Some(s);
        }
        ok
    }
}

mod host {
    use super::super::{
        browser::Browser,
        network_settings,
        pointer::{CURSOR_DRAWN_HEIGHT, CURSOR_DRAWN_WIDTH, Cursor, flush_union},
    };
    use super::*;
    use crate::browser::url::Url;
    use crate::fs::{RamBlockDevice, vfs::Vfs};
    use crate::input::{InputManager, Key, PrimaryTouch};
    use crate::usb::MOUSE_BUTTON_LEFT;
    use crate::{interrupts, tick, uart};
    use tab5_system_ui::{Gesture, SystemAction, Timer};

    use tab5_system_ui::{LaunchChoice, MiniId, ReturnTo, Screen};
    const ITEMS: [&str; 4] = ["Browser", "Console", "デスクトップ", "Power..."];
    const POWER_ITEMS: [&str; 3] = ["Reboot", "Shutdown", "Back"];
    fn menu_items(power: bool) -> &'static [&'static str] {
        if power { &POWER_ITEMS } else { &ITEMS }
    }
    const ROW_LEFT: usize = 8;
    const ROW_TOP: usize = HEIGHT + 8;
    const ROW_WIDTH: usize = 400;
    const ROW_HEIGHT: usize = 56;

    pub fn run(
        fb: &mut Framebuffer,
        input: &mut InputManager,
        wifi: &mut Manager,
        vfs: &mut Vfs,
        mut ram: Option<&mut RamBlockDevice>,
        start: Option<Url>,
        desktop: bool,
        battery: &mut BatteryMonitor,
        automount: &mut super::super::automount::AutoMount,
    ) -> super::super::FrontRoute {
        let Some(mut browser) = Browser::new(start, wifi) else {
            return super::super::FrontRoute::Console;
        };
        #[cfg(feature = "system-bar-static")]
        {
            fb.fill(WHITE);
            fb.fill_rect(APP.x, 0, APP.width, HEIGHT, BUTTON_FACE);
            fb.draw_gui_text(
                APP.x + 12,
                16,
                "System bar geometry / press any key to leave",
                1,
                BLACK,
                None,
            );
            let mut view = View::new();
            if !view.draw(
                fb,
                Snapshot {
                    wifi: 3,
                    battery: Some(3),
                    clock: Some((12, 34)),
                },
                false,
            ) || !fb.flush()
            {
                return super::super::FrontRoute::Console;
            }
            loop {
                interrupts::wait_for_interrupt();
                input.service_fast();
                wifi.service_io();
                if input.poll_key().is_some() {
                    return super::super::FrontRoute::Console;
                }
                if interrupts::dma_error() != 0 {
                    return super::super::FrontRoute::Console;
                }
            }
        }
        debug_assert!(tab5_system_ui::AppClass::Normal.has_bar());
        wifi.enter_gui();
        let mut screen = if desktop {
            Screen::Desktop
        } else {
            Screen::Browser
        };
        let mut normal_screen = screen;
        let mut exit_route = super::super::FrontRoute::Console;
        let mut network: Option<network_settings::Screen> = None;
        let mut bar = View::new();
        let mut indicators = Indicators::new(tick::now_ms());
        let mut timer = Timer::new(tick::now_ms(), 17, false);
        if desktop {
            browser.suspend(wifi, vfs);
        }
        let mut cursor = Cursor::new(WIDTH / 2, SCREEN_HEIGHT / 2);
        let mut pointer_visible = false;
        let mut gesture = Gesture::default();
        let mut press: Option<(usize, usize, bool)> = None;
        let mut touch_down = false;
        let mut content_dirty = true;
        let mut full = true;
        let mut slowest_handler = 0;
        let mut sequence = interrupts::frame_sequence();
        let mut topology = input.usb_host_mut().topology_epoch();
        input.cancel_primary_touch();
        loop {
            if interrupts::dma_error() != 0 {
                uart::log(b"SYSTEM BAR: DMA error\r\n");
                break;
            }
            interrupts::wait_for_interrupt();
            input.service_fast();
            let next = interrupts::frame_sequence();
            if sequence == next {
                wifi.service_io();
                continue;
            }
            sequence = next;
            let handler_started = tick::now_ms();
            input.service();
            wifi.service();
            // Auto-mount notices have no console target while GUI owns the display.
            automount.service_silent(fb, vfs, ram.as_deref_mut(), input.usb_host_mut());
            let now = tick::now_ms();
            let sampled = indicators.poll(now, wifi, battery);
            let epoch = input.usb_host_mut().topology_epoch();
            if epoch != topology {
                topology = epoch;
                gesture.cancel();
                press = None;
            }
            let mut transition: Option<Screen> = None;
            let mut leave = false;
            // Bounded input batch. A transition ends dispatch of the old batch.
            for _ in 0..16 {
                let Some(event) = input.poll_key() else {
                    break;
                };
                let key = event.key;
                // F3 works during editing; plain M is the CardKB entry outside
                // text fields. Existing browser shortcuts keep their meaning.
                let editing = match screen {
                    Screen::Browser => browser.editing(),
                    Screen::Mini(MiniId::Network) => network.as_ref().is_some_and(|n| n.editing()),
                    _ => false,
                };
                if key == Key::Function(3) || (!editing && matches!(key, Key::Ascii(b'm' | b'M'))) {
                    if !matches!(screen, Screen::Launcher { .. }) {
                        let return_to = ReturnTo::from_screen(screen);
                        transition = Some(Screen::Launcher {
                            power: false,
                            selected: 0,
                            return_to,
                        });
                    }
                    break;
                }
                match &mut screen {
                    Screen::Desktop => {}
                    Screen::Browser => {
                        if browser.key(key, wifi, vfs) {
                            transition = Some(Screen::Desktop);
                        }
                    }
                    Screen::Mini(MiniId::Network) => {
                        if network.as_mut().is_some_and(|n| n.key(key, wifi)) {
                            transition = Some(normal_screen);
                        }
                    }
                    Screen::Mini(MiniId::Battery) => {
                        if key == Key::Escape {
                            transition = Some(normal_screen);
                        }
                    }
                    Screen::Launcher {
                        power,
                        selected,
                        return_to,
                    } => {
                        match key {
                            Key::ArrowUp | Key::PageUp => *selected = selected.saturating_sub(1),
                            Key::ArrowDown | Key::PageDown => {
                                *selected = (*selected + 1).min(menu_items(*power).len() - 1)
                            }
                            Key::Escape => {
                                if *power {
                                    choose(
                                        2,
                                        true,
                                        *return_to,
                                        &mut transition,
                                        &mut leave,
                                        &mut exit_route,
                                    );
                                } else {
                                    transition = Some(return_to.screen());
                                }
                            }
                            Key::Ascii(b'\r' | b'\n') => choose(
                                *selected,
                                *power,
                                *return_to,
                                &mut transition,
                                &mut leave,
                                &mut exit_route,
                            ),
                            _ => {}
                        }
                        content_dirty = true;
                    }
                }
                if leave || transition.is_some() {
                    break;
                }
            }
            let touch = input.poll_primary_touch();
            let motion = input.poll_mouse();
            let mut target = (cursor.x, cursor.y);
            let mut clicked = None;
            if transition.is_none() && !leave {
                match touch {
                    PrimaryTouch::Pressed(p) => {
                        touch_down = true;
                        target = (p.x, p.y);
                        press = Some((p.x, p.y, true));
                        if p.y < HEIGHT {
                            gesture.press(if matches!(screen, Screen::Browser) {
                                target_rect(p.x, &browser)
                            } else {
                                system_target(p.x)
                            });
                        }
                    }
                    PrimaryTouch::Moved(p) => {
                        target = (p.x, p.y);
                        gesture.move_to(p.x, p.y);
                    }
                    PrimaryTouch::Released => {
                        if let Some((x, y, true)) = press.take() {
                            if y >= HEIGHT || gesture.release(cursor.x, cursor.y) {
                                clicked = Some((x, y));
                            }
                        }
                        touch_down = false;
                    }
                    PrimaryTouch::Idle => {}
                }
                if !touch_down && !matches!(touch, PrimaryTouch::Released) {
                    if let Some(m) = motion {
                        target = cursor.moved_to(m.dx, m.dy);
                        if m.pressed & MOUSE_BUTTON_LEFT != 0 {
                            press = Some((target.0, target.1, false));
                            if target.1 < HEIGHT {
                                gesture.press(if matches!(screen, Screen::Browser) {
                                    target_rect(target.0, &browser)
                                } else {
                                    system_target(target.0)
                                });
                            }
                        }
                        gesture.move_to(target.0, target.1);
                        if m.released & MOUSE_BUTTON_LEFT != 0 {
                            if let Some((x, y, false)) = press.take() {
                                if y >= HEIGHT || gesture.release(target.0, target.1) {
                                    clicked = Some((x, y));
                                }
                            }
                        }
                        if m.wheel != 0 && target.1 >= HEIGHT && matches!(screen, Screen::Browser) {
                            browser.wheel(m.wheel);
                        }
                    }
                }
                if let Some((x, y)) = clicked {
                    match tab5_system_ui::hit(x, y) {
                        Some(SystemAction::Launcher) => {
                            if let Screen::Launcher { return_to, .. } = screen {
                                transition = Some(return_to.screen());
                            } else {
                                let return_to = ReturnTo::from_screen(screen);
                                transition = Some(Screen::Launcher {
                                    power: false,
                                    selected: 0,
                                    return_to,
                                });
                            }
                        }
                        Some(SystemAction::Wifi)
                            if !matches!(screen, Screen::Mini(MiniId::Network)) =>
                        {
                            transition = Some(Screen::Mini(MiniId::Network))
                        }
                        Some(SystemAction::Battery)
                            if !matches!(screen, Screen::Mini(MiniId::Battery)) =>
                        {
                            transition = Some(Screen::Mini(MiniId::Battery))
                        }
                        Some(_) => {}
                        None => match &mut screen {
                            Screen::Browser if y >= HEIGHT || APP.contains(x, y) => {
                                if browser.click(x, y, wifi, vfs) {
                                    transition = Some(Screen::Desktop);
                                }
                            }
                            Screen::Mini(_) if APP.contains(x, y) => {
                                transition = Some(normal_screen)
                            }
                            Screen::Mini(MiniId::Network) if y >= HEIGHT => {
                                if let Some(n) = &mut network {
                                    if n.click(x, y, wifi) {
                                        transition = Some(normal_screen);
                                    }
                                }
                            }
                            Screen::Launcher {
                                return_to, power, ..
                            } if x >= ROW_LEFT
                                && x < ROW_LEFT + ROW_WIDTH
                                && y >= ROW_TOP
                                && y < ROW_TOP + menu_items(*power).len() * ROW_HEIGHT
                                && (y - ROW_TOP) % ROW_HEIGHT < ROW_HEIGHT - 8 =>
                            {
                                choose(
                                    (y - ROW_TOP) / ROW_HEIGHT,
                                    *power,
                                    *return_to,
                                    &mut transition,
                                    &mut leave,
                                    &mut exit_route,
                                )
                            }
                            _ => {}
                        },
                    }
                }
            }
            if leave {
                break;
            }
            if let Some(next) = transition {
                cursor.hide(fb);
                if matches!(screen, Screen::Browser) {
                    browser.suspend(wifi, vfs);
                    timer.suspend();
                }
                if matches!(next, Screen::Browser) {
                    timer.resume(now);
                }
                if matches!(next, Screen::Desktop | Screen::Browser) {
                    network = None;
                }
                if matches!(next, Screen::Mini(MiniId::Network)) && network.is_none() {
                    network = Some(network_settings::Screen::new(wifi));
                }
                if matches!(next, Screen::Mini(MiniId::Battery)) {
                    network = None;
                }
                if matches!(next, Screen::Browser | Screen::Desktop) {
                    normal_screen = next;
                }
                screen = next;
                gesture.cancel();
                press = None;
                touch_down = false;
                input.discard_queued_keys();
                input.cancel_primary_touch();
                content_dirty = true;
                full = true;
                bar.invalidate();
            }
            match screen {
                Screen::Browser => {
                    if timer.fire(now) {
                        browser.tick(input, wifi, vfs, ram.as_deref_mut());
                        timer.consume(tick::now_ms());
                    }
                    content_dirty |= browser.dirty();
                }
                Screen::Mini(MiniId::Network) => {
                    if let Some(n) = &mut network {
                        n.tick(wifi);
                        content_dirty |= n.dirty;
                    }
                }
                Screen::Mini(MiniId::Battery) => content_dirty |= sampled,
                _ => {}
            }
            let elapsed = tick::now_ms().saturating_sub(handler_started);
            if elapsed > slowest_handler {
                slowest_handler = elapsed;
                uart::log_u32(b"SYSTEM BAR: max service/handler ms=", elapsed as u32);
            }
            let show_pointer = input.has_mouse() || touch_down;
            if !full
                && !content_dirty
                && !bar.dirty(indicators.snapshot)
                && show_pointer == pointer_visible
                && target == (cursor.x, cursor.y)
            {
                continue;
            }
            let previous = (cursor.x, cursor.y);
            cursor.hide(fb);
            if content_dirty || full {
                match screen {
                    Screen::Desktop => super::super::desktop::draw(fb),
                    Screen::Browser => {
                        if !browser.draw(fb, wifi, full) {
                            uart::log(b"SYSTEM BAR: Browser flush failed\r\n");
                            break;
                        }
                    }
                    Screen::Mini(MiniId::Network) => {
                        if let Some(n) = &mut network {
                            n.draw(fb, wifi);
                        }
                    }
                    Screen::Mini(MiniId::Battery) => {
                        debug_assert!(tab5_system_ui::AppClass::Mini.has_bar());
                        super::super::battery::draw_details(fb, battery)
                    }
                    Screen::Launcher {
                        selected, power, ..
                    } => {
                        fb.fill_rect(0, HEIGHT, WIDTH, SCREEN_HEIGHT - HEIGHT, WHITE);
                        for (i, label) in menu_items(power).iter().enumerate() {
                            fb.fill_rect(
                                ROW_LEFT,
                                ROW_TOP + i * ROW_HEIGHT,
                                ROW_WIDTH,
                                ROW_HEIGHT - 8,
                                if i == selected {
                                    theme::ACCENT
                                } else {
                                    theme::SUBTLE
                                },
                            );
                            fb.draw_gui_text(
                                ROW_LEFT + 16,
                                ROW_TOP + i * ROW_HEIGHT + 8,
                                label,
                                2,
                                if i == selected {
                                    theme::ON_ACCENT
                                } else {
                                    BLACK
                                },
                                None,
                            );
                        }
                    }
                }
                if !matches!(screen, Screen::Browser) {
                    if full {
                        fb.fill_rect(APP.x, 0, APP.width, HEIGHT, BUTTON_FACE);
                        let title = match screen {
                            Screen::Desktop => "デスクトップ",
                            Screen::Mini(MiniId::Network) => "< Back   Network settings",
                            Screen::Mini(MiniId::Battery) => "< Back   Battery details",
                            Screen::Launcher { power: true, .. } => {
                                "Power / Escape returns to Launcher"
                            }
                            _ => "Launcher / M or F3   Arrows select, Enter opens, Escape returns",
                        };
                        fb.draw_gui_text_clipped(
                            APP.x + 12,
                            16,
                            title,
                            APP.width.saturating_sub(24),
                            1,
                            BLACK,
                            None,
                        );
                        if matches!(screen, Screen::Mini(_)) {
                            // Synthesize bold with the same one-pixel overstrike as Browser.
                            fb.draw_gui_text(APP.x + 13, 16, "< Back", 1, BLACK, None);
                        }
                        if !fb.flush_rect(APP.x, 0, APP.width, HEIGHT) {
                            uart::log(b"SYSTEM BAR: title flush failed\r\n");
                            break;
                        }
                    }
                    if !fb.flush_rect(0, HEIGHT, WIDTH, SCREEN_HEIGHT - HEIGHT) {
                        uart::log(b"SYSTEM BAR: content flush failed\r\n");
                        break;
                    }
                }
            }

            if !bar.draw(
                fb,
                indicators.snapshot,
                matches!(screen, Screen::Launcher { .. }),
            ) {
                uart::log(b"SYSTEM BAR: slot flush failed\r\n");
                break;
            }
            cursor.move_to(target.0, target.1);
            if show_pointer {
                cursor.show(fb);
            }
            pointer_visible = show_pointer;
            flush_union(
                fb,
                previous,
                (cursor.x, cursor.y),
                CURSOR_DRAWN_WIDTH,
                CURSOR_DRAWN_HEIGHT,
            );
            full = false;
            content_dirty = false;
        }
        cursor.hide(fb);
        browser.close(wifi, vfs);
        input.discard_queued_keys();
        wifi.leave_gui();
        input.cancel_primary_touch();
        exit_route
    }
    fn system_target(x: usize) -> Rect {
        for r in [LAUNCHER, WIFI, BATTERY, VOLUME, CLOCK] {
            if r.contains(x, 0) {
                return r;
            }
        }
        APP
    }
    fn target_rect(x: usize, browser: &Browser) -> Rect {
        for r in [LAUNCHER, WIFI, BATTERY, VOLUME, CLOCK] {
            if r.contains(x, 0) {
                return r;
            }
        }
        browser.bar_target(x)
    }
    fn choose(
        index: usize,
        power: bool,
        return_to: ReturnTo,
        transition: &mut Option<Screen>,
        leave: &mut bool,
        exit_route: &mut super::super::FrontRoute,
    ) {
        match if power {
            tab5_system_ui::power_launch(index, return_to)
        } else {
            tab5_system_ui::launch(index, return_to)
        } {
            LaunchChoice::Screen(screen) => *transition = Some(screen),
            LaunchChoice::Console => *leave = true,
            LaunchChoice::Power(action) => {
                *exit_route = super::super::FrontRoute::Power(action);
                *leave = true;
            }
        }
    }
}
pub use host::run;
