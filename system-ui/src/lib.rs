#![no_std]
//! Hardware-independent geometry, gesture ownership and timer lifetimes.

pub const HEIGHT: usize = 48;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub width: usize,
}
impl Rect {
    pub const fn contains(self, x: usize, y: usize) -> bool {
        y < HEIGHT && x >= self.x && x < self.x + self.width
    }
}
pub const LAUNCHER: Rect = Rect { x: 0, width: 52 };
pub const APP: Rect = Rect { x: 52, width: 1004 };
pub const WIFI: Rect = Rect {
    x: APP.x + APP.width,
    width: 48,
};
pub const BATTERY: Rect = Rect {
    x: WIFI.x + WIFI.width,
    width: 48,
};
pub const VOLUME: Rect = Rect {
    x: BATTERY.x + BATTERY.width,
    width: 48,
};
pub const CLOCK: Rect = Rect {
    x: VOLUME.x + VOLUME.width,
    width: 80,
};
const _: () = {
    assert!(APP.width > 0);
    assert!(CLOCK.x + CLOCK.width == 1280);
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppClass {
    Normal,
    Mini,
    Exclusive,
    Console,
}
impl AppClass {
    pub const fn has_bar(self) -> bool {
        matches!(self, Self::Normal | Self::Mini)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemAction {
    Launcher,
    Wifi,
    Battery,
}
pub fn hit(x: usize, y: usize) -> Option<SystemAction> {
    if LAUNCHER.contains(x, y) {
        Some(SystemAction::Launcher)
    } else if WIFI.contains(x, y) {
        Some(SystemAction::Wifi)
    } else if BATTERY.contains(x, y) {
        Some(SystemAction::Battery)
    } else {
        None
    }
}

/// One pointer owns a press until release. Leaving a target cancels it,
/// even if the pointer subsequently returns. Inert bar space is consumed.
#[derive(Default)]
pub struct Gesture {
    target: Option<Rect>,
    cancelled: bool,
    active: bool,
}
impl Gesture {
    pub fn press(&mut self, target: Rect) {
        self.target = Some(target);
        self.cancelled = false;
        self.active = true;
    }
    pub fn move_to(&mut self, x: usize, y: usize) {
        if self.active && self.target.is_some_and(|r| !r.contains(x, y)) {
            self.cancelled = true;
        }
    }
    pub fn release(&mut self, x: usize, y: usize) -> bool {
        self.move_to(x, y);
        let accept = self.active && !self.cancelled;
        self.cancel();
        accept
    }
    pub fn cancel(&mut self) {
        self.active = false;
        self.target = None;
        self.cancelled = false;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub wifi: u8,
    pub battery: Option<u8>,
    pub clock: Option<(u8, u8)>,
}
impl Snapshot {
    pub fn dirty(self, old: Option<Self>) -> [bool; 3] {
        old.map_or([true; 3], |old| {
            [
                self.wifi != old.wifi,
                self.battery != old.battery,
                self.clock != old.clock,
            ]
        })
    }
}
pub fn voltage_percent(mv: u32) -> u32 {
    mv.saturating_sub(6000)
        .saturating_mul(100)
        .checked_div(2230)
        .unwrap_or(0)
        .min(100)
}
pub fn battery_level(mv: u32) -> u8 {
    ((voltage_percent(mv) + 24) / 25) as u8
}

/// A timer has a single outstanding delivery, acknowledged after the handler.
/// No queued event can outlive this value (screen destruction drops it).
pub struct Timer {
    period: u64,
    next: u64,
    fixed: bool,
    pending: bool,
    stopped: bool,
}
impl Timer {
    pub fn new(now: u64, period: u64, fixed: bool) -> Self {
        assert!(period > 0);
        Self {
            period,
            next: now.saturating_add(period),
            fixed,
            pending: false,
            stopped: false,
        }
    }
    pub fn fire(&mut self, now: u64) -> bool {
        if self.stopped || self.pending || now < self.next {
            return false;
        }
        self.pending = true;
        true
    }
    pub fn consume(&mut self, now: u64) {
        if !self.pending {
            return;
        }
        self.pending = false;
        self.next = if self.fixed {
            self.next.saturating_add(
                (now.saturating_sub(self.next) / self.period + 1).saturating_mul(self.period),
            )
        } else {
            now.saturating_add(self.period)
        };
    }
    pub fn suspend(&mut self) {
        self.stopped = true;
    }
    pub fn resume(&mut self, now: u64) {
        self.stopped = false;
        self.pending = false;
        if self.fixed {
            if now >= self.next {
                self.next = self.next.saturating_add(
                    (now.saturating_sub(self.next) / self.period + 1).saturating_mul(self.period),
                );
            }
        } else {
            self.next = now.saturating_add(self.period);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries() {
        for x in 0..1280 {
            assert_eq!(hit(x, 48), None);
        }
        assert_eq!(hit(51, 47), Some(SystemAction::Launcher));
        assert_eq!(hit(52, 0), None);
        assert_eq!(hit(1055, 0), None);
        assert_eq!(hit(1056, 47), Some(SystemAction::Wifi));
        assert_eq!(hit(1103, 0), Some(SystemAction::Wifi));
        assert_eq!(hit(1104, 0), Some(SystemAction::Battery));
        assert_eq!(hit(1151, 0), Some(SystemAction::Battery));
        assert_eq!(hit(1152, 0), None);
        assert_eq!(hit(1279, 0), None);
        assert_eq!(hit(1280, 0), None);
    }
    #[test]
    fn drag_out_is_permanent() {
        let mut g = Gesture::default();
        g.press(WIFI);
        g.move_to(1056, 48);
        g.move_to(1056, 0);
        assert!(!g.release(1056, 0));
        g.press(WIFI);
        assert!(g.release(1103, 47));
        g.press(WIFI);
        g.cancel();
        assert!(!g.release(1056, 0));
    }
    #[test]
    fn voltage_and_dirty() {
        assert_eq!(voltage_percent(0), 0);
        assert_eq!(voltage_percent(6000), 0);
        assert_eq!(voltage_percent(7115), 50);
        assert_eq!(voltage_percent(8230), 100);
        assert_eq!(voltage_percent(36000), 100);
        let a = Snapshot {
            clock: Some((12, 34)),
            ..Snapshot::default()
        };
        assert_eq!(a.dirty(Some(a)), [false; 3]);
        assert_eq!(
            Snapshot {
                clock: Some((12, 35)),
                ..a
            }
            .dirty(Some(a)),
            [false, false, true]
        );
        assert_eq!(
            Snapshot {
                battery: Some(0),
                ..a
            }
            .dirty(Some(a)),
            [false, true, false]
        );
    }
    #[test]
    fn timer_coalesces_and_resumes() {
        let mut t = Timer::new(0, 100, true);
        assert!(!t.fire(99));
        assert!(t.fire(100));
        assert!(!t.fire(500));
        t.consume(550);
        assert!(!t.fire(599));
        assert!(t.fire(600));
        t.consume(600);
        t.suspend();
        assert!(!t.fire(1100));
        t.resume(1150);
        assert!(!t.fire(1199));
        assert!(t.fire(1200));
        let mut t = Timer::new(0, 100, false);
        t.suspend();
        t.resume(1150);
        assert!(!t.fire(1200));
        assert!(t.fire(1250));
    }
    #[test]
    fn classes() {
        assert!(AppClass::Normal.has_bar());
        assert!(AppClass::Mini.has_bar());
        assert!(!AppClass::Exclusive.has_bar());
        assert!(!AppClass::Console.has_bar());
    }
}

/// Browser targets use the same app rectangle as its drawing constants.
pub const BUTTON_WIDTH: usize = 52;
pub const ICON_LEFT: usize = APP.x + 3 * BUTTON_WIDTH + 12;
pub const ADDRESS_LEFT: usize = ICON_LEFT + 24 + 8;
pub const ADDRESS_RIGHT: usize = APP.x + APP.width - 8;
pub const CLEAR_LEFT: usize = ADDRESS_RIGHT - 32;
pub const ADDRESS_CELLS: usize = (CLEAR_LEFT - 8 - ADDRESS_LEFT) / 8;
const _: () = {
    assert!(ADDRESS_CELLS >= 64);
    assert!(ADDRESS_RIGHT + 4 <= WIFI.x);
};
pub fn browser_target(x: usize, editing: bool) -> Rect {
    for i in 0..3 {
        let r = Rect {
            x: APP.x + i * BUTTON_WIDTH,
            width: BUTTON_WIDTH,
        };
        if r.contains(x, 0) {
            return r;
        }
    }
    let lock = Rect {
        x: ICON_LEFT,
        width: 24,
    };
    if lock.contains(x, 0) {
        return lock;
    }
    let clear = Rect {
        x: CLEAR_LEFT,
        width: 32,
    };
    if editing && clear.contains(x, 0) {
        return clear;
    }
    let address = Rect {
        x: ADDRESS_LEFT,
        width: if editing { CLEAR_LEFT } else { ADDRESS_RIGHT } - ADDRESS_LEFT,
    };
    if address.contains(x, 0) {
        return address;
    }
    APP
}

#[cfg(test)]
mod browser_tests {
    use super::*;
    #[test]
    fn app_gesture_does_not_cross_buttons_or_system_slots() {
        assert_eq!(ADDRESS_CELLS, 94);
        for x in [52, 103, 104, 155, 156, 207, 220, 243, 252, 1015, 1016, 1047] {
            let r = browser_target(x, true);
            assert!(APP.contains(r.x, 0));
            assert!(r.x + r.width <= WIFI.x);
            let mut g = Gesture::default();
            g.press(r);
            assert!(!g.release(WIFI.x, 0));
        }
        let mut g = Gesture::default();
        g.press(browser_target(103, false));
        assert!(!g.release(104, 0));
        assert_eq!(browser_target(1016, false), browser_target(252, false));
        assert_ne!(browser_target(1016, true), browser_target(252, true));
    }
    #[test]
    fn independent_indicator_changes_and_unknown() {
        let s = Snapshot {
            wifi: 3,
            battery: Some(4),
            clock: Some((23, 59)),
        };
        assert_eq!(
            Snapshot { wifi: 0x80, ..s }.dirty(Some(s)),
            [true, false, false]
        );
        assert_eq!(
            Snapshot { battery: None, ..s }.dirty(Some(s)),
            [false, true, false]
        );
        assert_eq!(
            Snapshot { clock: None, ..s }.dirty(Some(s)),
            [false, false, true]
        );
        assert_eq!(
            Snapshot {
                clock: Some((0, 0)),
                ..s
            }
            .dirty(Some(s)),
            [false, false, true]
        );
    }
    #[test]
    fn timer_pending_does_not_leak_into_replacement() {
        let mut old = Timer::new(0, 10, true);
        assert!(old.fire(10));
        old.suspend();
        assert!(!old.fire(100));
        let mut replacement = Timer::new(100, 10, true);
        assert!(!replacement.fire(109));
        assert!(replacement.fire(110));
        replacement.consume(1000);
        assert!(!replacement.fire(1000));
        assert!(replacement.fire(1010));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MiniId {
    Network,
    Battery,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnTo {
    Desktop,
    Browser,
    Mini(MiniId),
}
impl ReturnTo {
    pub fn screen(self) -> Screen {
        match self {
            Self::Desktop => Screen::Desktop,
            Self::Browser => Screen::Browser,
            Self::Mini(id) => Screen::Mini(id),
        }
    }
    pub fn from_screen(screen: Screen) -> Self {
        match screen {
            Screen::Desktop => Self::Desktop,
            Screen::Browser => Self::Browser,
            Screen::Mini(id) => Self::Mini(id),
            Screen::Launcher { return_to, .. } => return_to,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Desktop,
    Browser,
    Launcher {
        power: bool,
        selected: usize,
        return_to: ReturnTo,
    },
    Mini(MiniId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    Reboot,
    Shutdown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchChoice {
    Screen(Screen),
    Console,
    Power(PowerAction),
}
pub fn launch(index: usize, return_to: ReturnTo) -> LaunchChoice {
    LaunchChoice::Screen(match index {
        0 => Screen::Browser,
        1 => return LaunchChoice::Console,
        2 => Screen::Desktop,
        3 => Screen::Launcher {
            power: true,
            selected: 0,
            return_to,
        },
        _ => return_to.screen(),
    })
}
pub fn power_launch(index: usize, return_to: ReturnTo) -> LaunchChoice {
    match index {
        0 => LaunchChoice::Power(PowerAction::Reboot),
        1 => LaunchChoice::Power(PowerAction::Shutdown),
        _ => LaunchChoice::Screen(Screen::Launcher {
            power: false,
            selected: 3,
            return_to,
        }),
    }
}
#[cfg(test)]
mod route_tests {
    use super::*;
    #[test]
    fn launcher_and_power_preserve_each_origin() {
        for origin in [
            ReturnTo::Desktop,
            ReturnTo::Browser,
            ReturnTo::Mini(MiniId::Network),
            ReturnTo::Mini(MiniId::Battery),
        ] {
            assert_eq!(
                launch(usize::MAX, origin),
                LaunchChoice::Screen(origin.screen())
            );
            assert_eq!(launch(2, origin), LaunchChoice::Screen(Screen::Desktop));
            assert_eq!(launch(1, origin), LaunchChoice::Console);
            assert_eq!(launch(0, origin), LaunchChoice::Screen(Screen::Browser));
            assert_eq!(
                launch(3, origin),
                LaunchChoice::Screen(Screen::Launcher {
                    power: true,
                    selected: 0,
                    return_to: origin
                })
            );
            assert_eq!(
                power_launch(2, origin),
                LaunchChoice::Screen(Screen::Launcher {
                    power: false,
                    selected: 3,
                    return_to: origin
                })
            );
        }
        assert_eq!(
            power_launch(0, ReturnTo::Desktop),
            LaunchChoice::Power(PowerAction::Reboot)
        );
        assert_eq!(
            power_launch(1, ReturnTo::Desktop),
            LaunchChoice::Power(PowerAction::Shutdown)
        );
    }
}
