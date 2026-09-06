//! Empty normal GUI surface. The shared host owns all input and the bar.
use crate::framebuffer::{Framebuffer, HEIGHT, WIDTH};

pub fn draw(fb: &mut Framebuffer) {
    fb.fill_rect(
        0,
        tab5_system_ui::HEIGHT,
        WIDTH,
        HEIGHT - tab5_system_ui::HEIGHT,
        super::theme::DESKTOP_BACKGROUND,
    );
}
