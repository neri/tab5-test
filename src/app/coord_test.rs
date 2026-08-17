//! Full-screen coordinate calibration chart.
//!
//! The chart itself lives in `Framebuffer::draw_coordinate_chart`; this module
//! only owns the mode: paint it once, hold it on the panel, and hand the
//! framebuffer back when a key arrives. Nothing is drawn over the chart -- the
//! exit hint goes to the UART log instead, because every pixel on screen is
//! something to be measured against a ruler.

use crate::framebuffer::Framebuffer;
use crate::input::InputManager;
use crate::uart;

pub fn run(framebuffer: &mut Framebuffer, input: &mut InputManager) {
    framebuffer.draw_coordinate_chart();
    if !framebuffer.flush() {
        uart::log(b"Coord test: flush failed\r\n");
        return;
    }
    uart::log(b"Coord test: chart displayed, press any key to exit\r\n");
    input.wait_for_key();
}
