//! Touch panel readers for the two controllers Tab5 units have shipped
//! with: the original standalone GT911 and the newer Sitronix ST7121/
//! ST7123, which combine the display driver and touch controller into one
//! chip at I2C address 0x55 (registers per Sitronix's public datasheet /
//! ESPHome's `st7123` touchscreen component).
//!
//! Both share the board I2C bus (SDA31/SCL32) with `lcd.rs`'s PI4IOE1 reset
//! control. Raw touch samples come back in the panel's native 720x1280
//! raster; `native_to_logical` undoes the same CW rotation `framebuffer::
//! native_offset` applies, so callers can use touch points directly as
//! `framebuffer.rs` drawing coordinates.

use crate::gpio::{self, Pin};
use crate::i2c::SoftI2c;
use crate::psram::{HEIGHT as NATIVE_HEIGHT, WIDTH as NATIVE_WIDTH};

const SDA: Pin = Pin::BoardI2cSda;
const SCL: Pin = Pin::BoardI2cScl;

/// Either touch controller Tab5 has shipped with, behind one `poll` API.
pub enum Touch {
    Gt911(Gt911),
    St7123(St7123),
}

/// One active contact in framebuffer logical (landscape) coordinates.
#[derive(Clone, Copy)]
pub struct TouchPoint {
    pub x: usize,
    pub y: usize,
}

impl Touch {
    /// Probes the older standalone GT911 first, then the newer integrated
    /// ST7121/ST7123 display-touch controller. Returns `None` if neither
    /// answers.
    pub fn init() -> Option<Self> {
        if let Some(panel) = Gt911::init() {
            return Some(Touch::Gt911(panel));
        }
        St7123::init().map(Touch::St7123)
    }

    /// Fills `points` with every active contact in `framebuffer.rs`'s logical
    /// (landscape) coordinates and returns how many fit.  A caller can pass
    /// a shorter slice when it only needs the first few contacts.
    pub fn poll_points(&self, points: &mut [TouchPoint]) -> usize {
        match self {
            Touch::Gt911(panel) => panel.poll_points(points),
            Touch::St7123(panel) => panel.poll_points(points),
        }
    }

    /// Returns the first active touch point, or `None` if the panel is idle.
    /// This keeps the paint screen's single-stroke interface while the
    /// `touchtest` command can inspect every contact through `poll_points`.
    pub fn poll(&self) -> Option<(usize, usize)> {
        let mut point = [TouchPoint { x: 0, y: 0 }; 1];
        if self.poll_points(&mut point) == 0 {
            None
        } else {
            Some((point[0].x, point[0].y))
        }
    }

    /// A short controller name for the interactive diagnostic.
    pub fn controller_name(&self) -> &'static str {
        match self {
            Touch::Gt911(_) => "GT911",
            Touch::St7123(_) => "ST7121/ST7123",
        }
    }

    /// Number of contacts the controller is configured to report.
    pub fn max_touches(&self) -> usize {
        match self {
            Touch::Gt911(_) => GT911_MAX_TOUCH_POINTS,
            Touch::St7123(panel) => panel.max_touches,
        }
    }
}

// ---------------------------------------------------------------------
// GT911 (original Tab5 units)
// ---------------------------------------------------------------------

// The GT911's I2C address depends on the state of its INT pin during reset,
// which this firmware never drives; probe both rather than assume one.
const GT911_CANDIDATE_ADDRESSES: [u8; 2] = [0x5D, 0x14];

const GT911_PRODUCT_ID: u16 = 0x8140;
const GT911_X_RESOLUTION: u16 = 0x8146;
const GT911_STATUS: u16 = 0x814E;
const GT911_POINT_1: u16 = 0x8150;
const GT911_TOUCH_STRIDE: usize = 8;
const GT911_MAX_TOUCH_POINTS: usize = 5;

pub struct Gt911 {
    bus: SoftI2c,
    address: u8,
    x_max: u16,
    y_max: u16,
}

impl Gt911 {
    /// Probes both possible GT911 addresses on the board bus and reads its
    /// configured resolution. Returns `None` if neither address answers a
    /// product-ID read, e.g. this is a newer Tab5 with an ST7121/ST7123
    /// panel instead.
    pub fn init() -> Option<Self> {
        let bus = shared_bus();

        for &address in &GT911_CANDIDATE_ADDRESSES {
            let mut product_id = [0u8; 4];
            if !read(&bus, address, GT911_PRODUCT_ID, &mut product_id) || product_id[0] == 0 {
                continue;
            }

            let mut resolution = [0u8; 4];
            let (x_max, y_max) = if read(&bus, address, GT911_X_RESOLUTION, &mut resolution) {
                (
                    u16::from_le_bytes([resolution[0], resolution[1]]),
                    u16::from_le_bytes([resolution[2], resolution[3]]),
                )
            } else {
                (0, 0)
            };
            return Some(Self {
                bus,
                address,
                x_max: fallback_max(x_max, NATIVE_WIDTH),
                y_max: fallback_max(y_max, NATIVE_HEIGHT),
            });
        }
        None
    }

    fn poll_points(&self, points: &mut [TouchPoint]) -> usize {
        let mut status = [0u8; 1];
        if !read(&self.bus, self.address, GT911_STATUS, &mut status) {
            return 0;
        }
        let ready = status[0] & 0x80 != 0;
        let point_count = (status[0] as usize & 0x0F).min(GT911_MAX_TOUCH_POINTS);
        let mut found = 0;
        if ready && point_count > 0 {
            let mut data = [0u8; GT911_MAX_TOUCH_POINTS * GT911_TOUCH_STRIDE];
            let len = point_count * GT911_TOUCH_STRIDE;
            if read(&self.bus, self.address, GT911_POINT_1, &mut data[..len]) {
                for point in data[..len].chunks_exact(GT911_TOUCH_STRIDE) {
                    if found == points.len() {
                        break;
                    }
                    let raw_x = u16::from_le_bytes([point[1], point[2]]) as u32;
                    let raw_y = u16::from_le_bytes([point[3], point[4]]) as u32;
                    let (x, y) = native_to_logical(
                        scale(raw_x, self.x_max, NATIVE_WIDTH),
                        scale(raw_y, self.y_max, NATIVE_HEIGHT),
                    );
                    points[found] = TouchPoint { x, y };
                    found += 1;
                }
            }
        }
        if ready {
            // Clears the buffer-ready flag so the next read waits for a
            // fresh sample instead of repeating this one.
            let _ = write(&self.bus, self.address, GT911_STATUS, 0);
        }
        found
    }
}

// ---------------------------------------------------------------------
// ST7121 / ST7123 (current Tab5 units; integrated display + touch)
// ---------------------------------------------------------------------

const ST7123_ADDRESS: u8 = 0x55;
// 16-bit big-endian register addresses, same wire format as GT911's above.
const ST7123_STATUS: u16 = 0x0001; // [7:4] error code, [3:0] device status
const ST7123_MAX_X: u16 = 0x0005; // X res @ +0/+1, Y res @ +2/+3
const ST7123_MAX_TOUCHES_REGISTER: u16 = 0x0009;
const ST7123_ADV_TOUCH_INFO: u16 = 0x0010; // reporting-table header

// The first touch point starts 4 bytes later, at register 0x0014.
const ST7123_STATUS_INIT: u8 = 0x1;
const ST7123_TOUCH_VALID: u8 = 0x80;
const ST7123_COORD_HIGH_MASK: u8 = 0x3F;
const ST7123_HEADER_BYTES: usize = 4;
const ST7123_TOUCH_STRIDE: usize = 7;
// The controller can report up to 10 simultaneous touch points.
const ST7123_MAX_TOUCH_POINTS: usize = 10;

pub struct St7123 {
    bus: SoftI2c,
    x_max: u16,
    y_max: u16,
    /// How many touch slots the controller is configured to report. It only
    /// latches a fresh sample once the host has read its *entire* reporting
    /// table, so every poll must read this many slots. Reading only the first
    /// one left every poll after the first touch reporting that same first
    /// touch again.
    max_touches: usize,
}

impl St7123 {
    /// Reads the status and native resolution of the integrated
    /// display-touch controller. Returns `None` if it doesn't answer, or is
    /// still mid boot (its display init in `lcd.rs` normally finishes well
    /// before a user can type the `paint` command).
    pub fn init() -> Option<Self> {
        let bus = shared_bus();

        let mut status = [0u8; 1];
        if !read(&bus, ST7123_ADDRESS, ST7123_STATUS, &mut status) {
            return None;
        }
        if status[0] & 0x0F == ST7123_STATUS_INIT {
            return None;
        }

        let mut resolution = [0u8; 4];
        let (x_max, y_max) = if read(&bus, ST7123_ADDRESS, ST7123_MAX_X, &mut resolution) {
            (
                (((resolution[0] & ST7123_COORD_HIGH_MASK) as u16) << 8) | resolution[1] as u16,
                (((resolution[2] & ST7123_COORD_HIGH_MASK) as u16) << 8) | resolution[3] as u16,
            )
        } else {
            (0, 0)
        };

        let mut max_touches_byte = [0u8; 1];
        let max_touches = if read(
            &bus,
            ST7123_ADDRESS,
            ST7123_MAX_TOUCHES_REGISTER,
            &mut max_touches_byte,
        ) && max_touches_byte[0] != 0
        {
            (max_touches_byte[0] as usize).min(ST7123_MAX_TOUCH_POINTS)
        } else {
            ST7123_MAX_TOUCH_POINTS
        };

        Some(Self {
            bus,
            x_max: fallback_max(x_max, NATIVE_WIDTH),
            y_max: fallback_max(y_max, NATIVE_HEIGHT),
            max_touches,
        })
    }

    fn poll_points(&self, points: &mut [TouchPoint]) -> usize {
        // Must read the controller's *complete* configured reporting table
        // (see `max_touches` above), otherwise the controller does not latch
        // a fresh sample for the next poll.
        let mut data = [0u8; ST7123_HEADER_BYTES + ST7123_MAX_TOUCH_POINTS * ST7123_TOUCH_STRIDE];
        let len = ST7123_HEADER_BYTES + self.max_touches * ST7123_TOUCH_STRIDE;
        if !read(
            &self.bus,
            ST7123_ADDRESS,
            ST7123_ADV_TOUCH_INFO,
            &mut data[..len],
        ) {
            return 0;
        }
        let mut found = 0;
        for point in data[ST7123_HEADER_BYTES..len].chunks_exact(ST7123_TOUCH_STRIDE) {
            if point[0] & ST7123_TOUCH_VALID == 0 {
                continue;
            }
            if found == points.len() {
                break;
            }
            let raw_x = (((point[0] & ST7123_COORD_HIGH_MASK) as u32) << 8) | point[1] as u32;
            let raw_y = (((point[2] & ST7123_COORD_HIGH_MASK) as u32) << 8) | point[3] as u32;
            let (x, y) = native_to_logical(
                scale(raw_x, self.x_max, NATIVE_WIDTH),
                scale(raw_y, self.y_max, NATIVE_HEIGHT),
            );
            points[found] = TouchPoint { x, y };
            found += 1;
        }
        found
    }
}

// ---------------------------------------------------------------------
// Shared bus plumbing
// ---------------------------------------------------------------------

fn shared_bus() -> SoftI2c {
    let bus = SoftI2c::new(SDA, SCL, 3, 10_000);
    gpio::configure_open_drain(SDA);
    gpio::configure_open_drain(SCL);
    gpio::release(SDA);
    gpio::release(SCL);
    bus
}

/// A zeroed resolution register (unconfigured or unread) falls back to the
/// panel's own native pixel count.
fn fallback_max(reported: u16, native_limit: usize) -> u16 {
    if reported == 0 {
        native_limit as u16
    } else {
        reported
    }
}

fn scale(raw: u32, raw_max: u16, native_limit: usize) -> u32 {
    (raw * native_limit as u32 / raw_max.max(1) as u32).min(native_limit as u32 - 1)
}

/// Inverse of `framebuffer::native_offset`'s CW rotation:
/// logical (x, y) -> native (NATIVE_HEIGHT-1-x, y).
fn native_to_logical(native_x: u32, native_y: u32) -> (usize, usize) {
    let logical_x = (NATIVE_HEIGHT as u32 - 1 - native_y) as usize;
    let logical_y = native_x as usize;
    (logical_x, logical_y)
}

fn read(bus: &SoftI2c, address: u8, register: u16, buffer: &mut [u8]) -> bool {
    if !bus.start() {
        return false;
    }
    let addressed = bus.write_byte(address << 1)
        && bus.write_byte((register >> 8) as u8)
        && bus.write_byte(register as u8)
        && bus.start() // repeated start, switching the transaction to a read
        && bus.write_byte((address << 1) | 1);
    if !addressed {
        bus.stop();
        return false;
    }
    let last = buffer.len().saturating_sub(1);
    for (index, slot) in buffer.iter_mut().enumerate() {
        match bus.read_byte(index != last) {
            Some(byte) => *slot = byte,
            None => {
                bus.stop();
                return false;
            }
        }
    }
    bus.stop();
    true
}

fn write(bus: &SoftI2c, address: u8, register: u16, value: u8) -> bool {
    if !bus.start() {
        return false;
    }
    let ok = bus.write_byte(address << 1)
        && bus.write_byte((register >> 8) as u8)
        && bus.write_byte(register as u8)
        && bus.write_byte(value);
    bus.stop();
    ok
}
