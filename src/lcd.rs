//! Tab5 ST7121-compatible MIPI-DSI output for ESP32-P4 ECO2.
//!
//! The normal path streams an RGB565 double framebuffer from PSRAM through
//! DW-GDMA. The DSI Host's Video Pattern Generator remains available as a
//! fallback diagnostic when framebuffer setup fails.

use crate::uart;
use crate::{cardkb::CardKb, console::Update, framebuffer::DoubleBuffer, interrupts, psram::Psram};

mod st7121;

const DSI_HOST: usize = 0x500A_0000;
const DSI_BRG: usize = 0x500A_0800;
const DSI_BRG_MEM: usize = 0x5010_5000;
const DW_GDMA: usize = 0x5008_1000;
const HP_SYS_CLKRST: usize = 0x500E_6000;
const PMU: usize = 0x5011_5000;
const GPIO: usize = 0x500E_0000;
const IO_MUX: usize = 0x500E_1000;

const WIDTH: u32 = 720;
const HEIGHT: u32 = 1280;

#[derive(Clone, Copy)]
pub(super) struct InitCommand {
    command: u8,
    data: &'static [u8],
    delay_ms: u32,
}

// M5Stack's ST7123 sequence for the Tab5. The final sleep-out delay is
// necessary before the host changes from command mode to continuous video.
#[allow(dead_code)]
const ST7123_INIT: &[InitCommand] = &[
    InitCommand {
        command: 0x60,
        data: &[0x71, 0x23, 0xA2],
        delay_ms: 0,
    },
    InitCommand {
        command: 0x60,
        data: &[0x71, 0x23, 0xA3],
        delay_ms: 0,
    },
    InitCommand {
        command: 0x60,
        data: &[0x71, 0x23, 0xA4],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xA4,
        data: &[0x31],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xD7,
        data: &[0x10, 0x0A, 0x10, 0x2A, 0x80, 0x80],
        delay_ms: 0,
    },
    InitCommand {
        command: 0x90,
        data: &[0x71, 0x23, 0x5A, 0x20, 0x24, 0x09, 0x09],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xA3,
        data: &[
            0x80, 0x01, 0x88, 0x30, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46, 0x00, 0x00, 0x1E,
            0x5C, 0x1E, 0x80, 0x00, 0x4F, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46, 0x00, 0x00,
            0x1E, 0x5C, 0x1E, 0x80, 0x00, 0x6F, 0x58, 0x00, 0x00, 0x00, 0xFF,
        ],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xA6,
        data: &[
            0x03, 0x00, 0x24, 0x55, 0x36, 0x00, 0x39, 0x00, 0x6E, 0x6E, 0x91, 0xFF, 0x00, 0x24,
            0x55, 0x38, 0x00, 0x37, 0x00, 0x6E, 0x6E, 0x91, 0xFF, 0x00, 0x24, 0x11, 0x00, 0x00,
            0x00, 0x00, 0x6E, 0x6E, 0x91, 0xFF, 0x00, 0xEC, 0x11, 0x00, 0x03, 0x00, 0x03, 0x6E,
            0x6E, 0xFF, 0xFF, 0x00, 0x08, 0x80, 0x08, 0x80, 0x06, 0x00, 0x00, 0x00, 0x00,
        ],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xA7,
        data: &[
            0x19, 0x19, 0x80, 0x64, 0x40, 0x07, 0x16, 0x40, 0x00, 0x44, 0x03, 0x6E, 0x6E, 0x91,
            0xFF, 0x08, 0x80, 0x64, 0x40, 0x25, 0x34, 0x40, 0x00, 0x02, 0x01, 0x6E, 0x6E, 0x91,
            0xFF, 0x08, 0x80, 0x64, 0x40, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x6E, 0x6E, 0x91,
            0xFF, 0x08, 0x80, 0x64, 0x40, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x6E, 0x6E, 0x84,
            0xFF, 0x08, 0x80, 0x44,
        ],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xAC,
        data: &[
            0x03, 0x19, 0x19, 0x18, 0x18, 0x06, 0x13, 0x13, 0x11, 0x11, 0x08, 0x08, 0x0A, 0x0A,
            0x1C, 0x1C, 0x07, 0x07, 0x00, 0x00, 0x02, 0x02, 0x01, 0x19, 0x19, 0x18, 0x18, 0x06,
            0x12, 0x12, 0x10, 0x10, 0x09, 0x09, 0x0B, 0x0B, 0x1C, 0x1C, 0x07, 0x07, 0x03, 0x03,
            0x01, 0x01,
        ],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xAD,
        data: &[
            0xF0, 0x00, 0x46, 0x00, 0x03, 0x50, 0x50, 0xFF, 0xFF, 0xF0, 0x40, 0x06, 0x01, 0x07,
            0x42, 0x42, 0xFF, 0xFF, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF,
        ],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xAE,
        data: &[0xFE, 0x3F, 0x3F, 0xFE, 0x3F, 0x3F, 0x00],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xB2,
        data: &[
            0x15, 0x19, 0x05, 0x23, 0x49, 0xAF, 0x03, 0x2E, 0x5C, 0xD2, 0xFF, 0x10, 0x20, 0xFD,
            0x20, 0xC0, 0x00,
        ],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xE8,
        data: &[
            0x20, 0x6F, 0x04, 0x97, 0x97, 0x3E, 0x04, 0xDC, 0xDC, 0x3E, 0x06, 0xFA, 0x26, 0x3E,
        ],
        delay_ms: 0,
    },
    InitCommand {
        command: 0x75,
        data: &[0x03, 0x04],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xE7,
        data: &[
            0x3B, 0x00, 0x00, 0x7C, 0xA1, 0x8C, 0x20, 0x1A, 0xF0, 0xB1, 0x50, 0x00, 0x50, 0xB1,
            0x50, 0xB1, 0x50, 0xD8, 0x00, 0x55, 0x00, 0xB1, 0x00, 0x45, 0xC9, 0x6A, 0xFF, 0x5A,
            0xD8, 0x18, 0x88, 0x15, 0xB1, 0x01, 0x01, 0x77,
        ],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xEA,
        data: &[0x13, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x2C],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xB0,
        data: &[0x22, 0x43, 0x11, 0x61, 0x25, 0x43, 0x43],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xB7,
        data: &[0x00, 0x00, 0x73, 0x73],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xBF,
        data: &[0xA6, 0xAA],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xA9,
        data: &[0x00, 0x00, 0x73, 0xFF, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xC8,
        data: &[
            0x00, 0x00, 0x10, 0x1F, 0x36, 0x00, 0x5D, 0x04, 0x9D, 0x05, 0x10, 0xF2, 0x06, 0x60,
            0x03, 0x11, 0xAD, 0x00, 0xEF, 0x01, 0x22, 0x2E, 0x0E, 0x74, 0x08, 0x32, 0xDC, 0x09,
            0x33, 0x0F, 0xF3, 0x77, 0x0D, 0xB0, 0xDC, 0x03, 0xFF,
        ],
        delay_ms: 0,
    },
    InitCommand {
        command: 0xC9,
        data: &[
            0x00, 0x00, 0x10, 0x1F, 0x36, 0x00, 0x5D, 0x04, 0x9D, 0x05, 0x10, 0xF2, 0x06, 0x60,
            0x03, 0x11, 0xAD, 0x00, 0xEF, 0x01, 0x22, 0x2E, 0x0E, 0x74, 0x08, 0x32, 0xDC, 0x09,
            0x33, 0x0F, 0xF3, 0x77, 0x0D, 0xB0, 0xDC, 0x03, 0xFF,
        ],
        delay_ms: 0,
    },
    InitCommand {
        command: 0x36,
        data: &[0x03],
        delay_ms: 0,
    },
    InitCommand {
        command: 0x11,
        data: &[0x00],
        delay_ms: 120,
    },
    InitCommand {
        command: 0x29,
        data: &[0x00],
        delay_ms: 0,
    },
    InitCommand {
        command: 0x35,
        data: &[0x00],
        delay_ms: 120,
    },
];

/// Starts the DSI Host's vertical colour-bar generator.
///
/// A failure returns to the caller after writing a diagnostic to USB; it never
/// spins forever waiting for a PHY that did not lock.
pub fn start_pattern() -> bool {
    uart::log(b"LCD: enabling DSI test pattern...\r\n");
    if !init_panel() {
        return false;
    }
    // Match ESP-IDF's lifecycle: create/configure the normal DPI bridge first,
    // then temporarily replace its output with the Host VPG. The later DMA
    // transition only disables VPG and re-enables this already-clean bridge.
    configure_video_dma();
    start_video_pattern();
    backlight_on();
    uart::log(b"LCD: ST7121 VPG vertical colour bars active\r\n");
    true
}

/// Runs the CardKB input echo console over the double-buffered DMA display.
///
/// Each keystroke is rendered into the inactive buffer, cache-synchronised,
/// then selected at the next full-frame DMA completion. This never modifies a
/// buffer while the display engine can read from it.
pub fn run_console(psram: Psram) {
    let Some(mut framebuffers) = DoubleBuffer::new(psram) else {
        uart::log(b"FB: two buffers do not fit in mapped PSRAM\r\n");
        return;
    };
    // One foreground hart owns the singleton for the lifetime of the app.
    let console = unsafe { crate::console::singleton() };
    uart::log(b"Console: buffer 0 render begin\r\n");
    console.render(&mut framebuffers, 0);
    uart::log(b"Console: buffer 0 render done\r\n");
    uart::log(b"Console: buffer 1 render begin\r\n");
    console.render(&mut framebuffers, 1);
    uart::log(b"Console: buffer 1 render done\r\n");
    uart::log(b"Console: flush 0 begin\r\n");
    if !framebuffers.flush(0) {
        uart::log(b"FB: cache sync failed\r\n");
        return;
    }
    uart::log(b"Console: flush 0 done\r\n");
    uart::log(b"Console: flush 1 begin\r\n");
    if !framebuffers.flush(1) {
        uart::log(b"FB: cache sync failed\r\n");
        return;
    }
    uart::log(b"Console: flush 1 done\r\n");
    uart::log(b"Console: buffers ready\r\n");
    let Some(fb0) = framebuffers.address(0) else {
        return;
    };
    let Some(fb1) = framebuffers.address(1) else {
        return;
    };

    uart::log(b"LCD: enabling PSRAM/DW-GDMA video...\r\n");
    if !init_panel() {
        return;
    }
    configure_video_dma();
    uart::log(b"LCD: DMA 1/3 bridge configured\r\n");
    let mut dma = DmaDisplay::new(fb0, fb1);
    if !dma.initialize() {
        uart::log(b"LCD: DW-GDMA initialization failed\r\n");
        return;
    }
    interrupts::install(fb0, fb1);
    write(DSI_BRG + 0x50, 1); // clear a stale FIFO-underrun indication
    if !dma.start(0) {
        uart::log(b"LCD: DW-GDMA setup failed\r\n");
        return;
    }
    if !wait_for_bridge_fifo(256) {
        uart::log(b"LCD: Bridge FIFO prefill timeout\r\n");
        dma.log_status();
        return;
    }
    // Start video for the first time with the DMA source already armed. This
    // has no VPG source-mux transition and mirrors dpi_panel_init(). No UART
    // writes may be inserted in this sequence.
    write(DSI_HOST + 0x38, 0x0000_FF02);
    write(DSI_HOST + 0x34, 0);
    write(DSI_HOST + 0x94, (1 << 0) | (1 << 1));
    write(DSI_BRG + 0x40, 1 | (WIDTH << 4));
    write(DSI_BRG + 0x44, 1);
    sync_video_registers();

    uart::log(b"LCD: DMA 2/3 first frame armed\r\n");
    uart::log(b"LCD: DMA 3/3 full-frame interrupt installed\r\n");
    backlight_on();
    uart::log(b"LCD: RGB565 framebuffer DMA active\r\n");

    let mut sequence = interrupts::frame_sequence();
    let mut keyboard = CardKb::init();
    if keyboard.is_some() {
        uart::log(b"CardKB: ready\r\n");
    } else {
        uart::log(b"CardKB: absent\r\n");
    }
    let mut reconnect_frames = 0u32;
    loop {
        // An error IRQ can arrive while a key update is running. Check before
        // WFI as well as after it so a status already acknowledged by the ISR
        // cannot leave the foreground asleep forever.
        let error = interrupts::dma_error();
        if error != 0 {
            uart::log_hex(b"LCD: DMA interrupt error=", error);
            dma.log_status();
            return;
        }
        interrupts::wait_for_interrupt();
        let error = interrupts::dma_error();
        if error != 0 {
            uart::log_hex(b"LCD: DMA interrupt error=", error);
            dma.log_status();
            return;
        }
        let next_sequence = interrupts::frame_sequence();
        if next_sequence == sequence {
            continue;
        }
        sequence = next_sequence;
        // Keep the CPU's view of the displayed side in sync with the ISR.
        // Rendering is always directed to the opposite side below.
        let displayed = interrupts::active_framebuffer();

        if keyboard.is_none() {
            reconnect_frames += 1;
            if reconnect_frames == 60 {
                reconnect_frames = 0;
                keyboard = CardKb::init();
                if keyboard.is_some() {
                    uart::log(b"CardKB: connected\r\n");
                }
            }
            continue;
        }

        if let Some(byte) = keyboard.as_mut().and_then(CardKb::poll) {
            uart::log_hex(b"CardKB: key=", byte as u32);
            match console.push(byte) {
                Update::None => {}
                Update::Cell { column, row } => {
                    // Both sides start identical. Update the inactive side
                    // first, then the displayed side. Only the native span
                    // covering this cell is written back, so GDMA retains
                    // almost all PSRAM bandwidth and visible tearing is at
                    // most one small glyph.
                    let back_buffer = displayed ^ 1;
                    console.render_cell(&mut framebuffers, back_buffer, column, row);
                    if !console.flush_cell(&framebuffers, back_buffer, column, row) {
                        uart::log(b"Console: cell flush failed\r\n");
                        continue;
                    }
                    console.render_cell(&mut framebuffers, displayed, column, row);
                    if !console.flush_cell(&framebuffers, displayed, column, row) {
                        uart::log(b"Console: cell flush failed\r\n");
                        continue;
                    }
                    uart::log(b"Console: cell updated\r\n");
                }
                Update::Full => {
                    // Scrolling is infrequent. Keep both sides coherent so
                    // subsequent cell updates can remain incremental.
                    uart::log(b"Console: full redraw\r\n");
                    for index in [displayed ^ 1, displayed] {
                        console.render(&mut framebuffers, index);
                        if !framebuffers.flush(index) {
                            uart::log(b"Console: flush failed\r\n");
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn init_panel() -> bool {
    if !reset_lcd_panel() {
        uart::log(b"LCD: PI4IOE1 reset control failed\r\n");
        return false;
    }
    uart::log(b"LCD: panel reset released\r\n");
    uart::log(b"LCD: D-PHY 1/4 powering LDO_VO3\r\n");
    enable_dphy_ldo();
    uart::log(b"LCD: D-PHY 2/4 enabling clocks\r\n");
    enable_dsi_clock();

    uart::log(b"LCD: D-PHY 3/4 starting PLL\r\n");
    if !init_phy() {
        uart::log(b"LCD: D-PHY lock timeout\r\n");
        uart::log_hex(b"LCD: PHY status=", read(DSI_HOST + 0xB0));
        uart::log_hex(b"LCD: LDO_VO3=", read(PMU + 0x1C0));
        uart::log_hex(b"LCD: LDO_VO3_ANA=", read(PMU + 0x1C4));
        uart::log_hex(b"LCD: ref_clk_ctrl=", read(HP_SYS_CLKRST + 0x2C));
        return false;
    }
    uart::log(b"LCD: D-PHY 4/4 ready\r\n");

    // The official ST7121 driver performs a software reset even when the Tab5
    // board-level reset has already been pulsed through the I/O expander.
    uart::log(b"LCD: DCS software reset\r\n");
    if !dcs_write(0x01, &[]) {
        uart::log(b"LCD: ST7121 software reset failed\r\n");
        return false;
    }
    delay_ms(120);

    uart::log(b"LCD: DCS ST7121 init sequence\r\n");
    for init in st7121::INIT {
        if !dcs_write(init.command, init.data) {
            uart::log(b"LCD: DCS FIFO timeout\r\n");
            uart::log_hex(b"LCD: failed command=", init.command as u32);
            uart::log_hex(b"LCD: packet status=", read(DSI_HOST + 0x74));
            uart::log_hex(b"LCD: host int_st0=", read(DSI_HOST + 0xBC));
            uart::log_hex(b"LCD: host int_st1=", read(DSI_HOST + 0xC0));
            uart::log_hex(b"LCD: PHY status=", read(DSI_HOST + 0xB0));
            return false;
        }
        // M5Stack's ST7121 driver leaves at least 5 ms after every command.
        delay_ms(init.delay_ms + 5);
    }
    // Commands are deliberately queued back-to-back, as ESP-IDF does.  Wait
    // only once before changing the Host from command to video mode.
    if !wait_for(DSI_HOST + 0x74, (1 << 0) | (1 << 2), (1 << 0) | (1 << 2)) {
        uart::log(b"LCD: final DCS drain timeout\r\n");
        uart::log_hex(b"LCD: packet status=", read(DSI_HOST + 0x74));
        return false;
    }
    uart::log(b"LCD: DCS init complete\r\n");
    true
}

fn reset_lcd_panel() -> bool {
    // Tab5's LCD reset is PI4IOE1 P4, not an ESP32-P4 GPIO. Use a small
    // open-drain software-I2C master on the board bus (SDA31/SCL32), avoiding
    // any dependency on an ECO2-aware peripheral HAL.
    configure_i2c_gpio(31, IO_MUX + 0x80, GPIO + 0x5D4);
    configure_i2c_gpio(32, IO_MUX + 0x84, GPIO + 0x5D8);
    gpio_release(31);
    gpio_release(32);
    delay_us(20);

    // Recover a transaction that a reset may have interrupted.
    for _ in 0..9 {
        gpio_low(32);
        delay_us(3);
        gpio_release(32);
        delay_us(3);
    }
    i2c_stop();

    const SETUP: &[(u8, u8)] = &[
        (0x01, 0xFF), // chip reset
        (0x03, 0x7F), // P0..P6 outputs
        (0x07, 0x00), // output high-impedance disabled
        (0x0D, 0x7F), // pull-up selection
        (0x0B, 0x7F), // pulls enabled
    ];
    for &(register, value) in SETUP {
        if !pi4ioe1_write(register, value) {
            return false;
        }
        delay_us(100);
    }

    // Preserve M5Stack's defaults for the other outputs. Pulse LCD_RST (P4)
    // low, then release it exactly as in the successful ESP-IDF control app.
    if !pi4ioe1_write(0x05, 0x66) {
        return false;
    }
    delay_ms(100);
    if !pi4ioe1_write(0x05, 0x76) {
        return false;
    }
    delay_ms(100);
    true
}

fn configure_i2c_gpio(pin: u32, iomux: usize, matrix_out: usize) {
    // GPIO matrix output, output-enable controlled by GPIO_ENABLE.
    write(matrix_out, 256 | (1 << 10));
    // GPIO function, input enabled, pull-up enabled, medium drive strength.
    modify(
        iomux,
        (1 << 7) | (1 << 8) | (1 << 9) | (0x3 << 10) | (0x7 << 12),
        (1 << 8) | (1 << 9) | (2 << 10) | (1 << 12),
    );
    gpio_low(pin);
    gpio_release(pin);
}

fn pi4ioe1_write(register: u8, value: u8) -> bool {
    if !i2c_start() {
        return false;
    }
    let acknowledged =
        i2c_write_byte(0x43 << 1) && i2c_write_byte(register) && i2c_write_byte(value);
    i2c_stop();
    acknowledged
}

fn i2c_start() -> bool {
    gpio_release(31);
    gpio_release(32);
    delay_us(3);
    if !gpio_level(31) || !raise_scl() {
        return false;
    }
    gpio_low(31);
    delay_us(3);
    gpio_low(32);
    true
}

fn i2c_stop() {
    gpio_low(31);
    delay_us(3);
    let _ = raise_scl();
    delay_us(3);
    gpio_release(31);
    delay_us(3);
}

fn i2c_write_byte(byte: u8) -> bool {
    for bit in (0..8).rev() {
        gpio_low(32);
        if byte & (1 << bit) != 0 {
            gpio_release(31);
        } else {
            gpio_low(31);
        }
        delay_us(2);
        if !raise_scl() {
            return false;
        }
        delay_us(3);
    }

    gpio_low(32);
    gpio_release(31);
    delay_us(2);
    if !raise_scl() {
        return false;
    }
    let acknowledged = !gpio_level(31);
    delay_us(2);
    gpio_low(32);
    acknowledged
}

fn raise_scl() -> bool {
    gpio_release(32);
    for _ in 0..10_000 {
        if gpio_level(32) {
            return true;
        }
    }
    false
}

fn gpio_low(pin: u32) {
    if pin < 32 {
        write(GPIO + 0x0C, 1u32 << pin);
        write(GPIO + 0x24, 1u32 << pin);
    } else {
        let bit = 1u32 << (pin - 32);
        write(GPIO + 0x18, bit);
        write(GPIO + 0x30, bit);
    }
}

fn gpio_release(pin: u32) {
    if pin < 32 {
        write(GPIO + 0x28, 1u32 << pin);
    } else {
        write(GPIO + 0x34, 1u32 << (pin - 32));
    }
}

fn gpio_level(pin: u32) -> bool {
    if pin < 32 {
        read(GPIO + 0x3C) & (1u32 << pin) != 0
    } else {
        read(GPIO + 0x40) & (1u32 << (pin - 32)) != 0
    }
}

fn enable_dphy_ldo() {
    // Tab5 routes VDD_MIPI_DPHY to LDO_VO3. ESP-IDF's calibrated value is
    // normally obtained from eFuse; 2.5 V maps exactly to dref=9, mul=6 with
    // the nominal ECO2 calibration model.
    // ESP-IDF maps LDO channel 3 to ext_ldo[1], which is P0_0P2A.
    // P0_0P3A (offset 0x1C8) is a different physical regulator.
    const LDO_VO3: usize = PMU + 0x1C0;
    const LDO_VO3_ANA: usize = PMU + 0x1C4;
    // These are the complete ESP-IDF register values, not a read-modify-write
    // approximation: force_tieh_sel, target0 and target1 are significant.
    write(LDO_VO3, 0x4020_0180);
    write(LDO_VO3_ANA, (9 << 28) | (6 << 23));
    delay_ms(10);
}

fn enable_dsi_clock() {
    // Enable the DSI register clock and release the bridge reset.
    modify(HP_SYS_CLKRST + 0x18, 1 << 12, 1 << 12);
    modify(HP_SYS_CLKRST + 0xC0, 1 << 26, 1 << 26);
    modify(HP_SYS_CLKRST + 0xC0, 1 << 26, 0);

    // PLL_F20M is the default D-PHY reference clock. Enable configuration and
    // reference clocks; source-select 0 is PLL_F20M.
    // `esp_clk_tree_enable_src(PLL_F20M)` enables this gate in ESP-IDF.
    modify(HP_SYS_CLKRST + 0x28, 0xFF << 16, 23 << 16);
    modify(HP_SYS_CLKRST + 0x2C, 1 << 8, 1 << 8);
    modify(HP_SYS_CLKRST + 0x38, 0x3 << 30, 0);
    modify(
        HP_SYS_CLKRST + 0x3C,
        (1 << 0) | (1 << 1),
        (1 << 0) | (1 << 1),
    );
}

fn init_phy() -> bool {
    // 2 lanes, host and PHY on. Keep clock-lane enable and PLL force clear
    // while releasing PHY reset; this order is required by the D-PHY state
    // machine and mirrors ESP-IDF's mipi_dsi_hal_init().
    modify(DSI_HOST + 0xA4, 0x3, 1);
    modify(DSI_HOST + 0x04, 1, 1);
    modify(DSI_HOST + 0xA0, 0xF, 1);
    modify(DSI_HOST + 0xA0, 1 << 1, 0);
    modify(DSI_HOST + 0xA0, 1 << 1, 1 << 1);
    modify(DSI_HOST + 0xA0, (1 << 2) | (1 << 3), (1 << 2) | (1 << 3));

    // The command FIFOs are drained through the internal DSI bridge as well.
    // It must leave its own reset *before* panel DCS commands are queued.
    // EN.bit0 is the bridge output enable; EN.bit1 is its software reset.
    // Keep output disabled here, reset the bridge, then release it.
    write(DSI_BRG + 0x00, 1);
    modify(DSI_BRG + 0x04, 1 << 1, 1 << 1);
    modify(DSI_BRG + 0x04, 1 << 1, 0);

    // 20 MHz reference, M=48, N=1 gives a 960 Mbps lane rate. The Tab5 BSP
    // requests 965 Mbps for both ST7121 and ST7123 panel revisions.
    phy_write(0x44, 0x34); // 950..999 Mbps range selector
    phy_write(0x19, 0x30);
    phy_write(0x17, 0x00);
    phy_write(0x18, 0x0F);
    phy_write(0x18, 0x81);

    if !wait_for(DSI_HOST + 0xB0, 1, 1) {
        uart::log(b"LCD: D-PHY PLL lock wait failed\r\n");
        return false;
    }
    uart::log_hex(b"LCD: D-PHY PLL locked, status=", read(DSI_HOST + 0xB0));
    if !wait_for(
        DSI_HOST + 0xB0,
        (1 << 2) | (1 << 4) | (1 << 7),
        (1 << 2) | (1 << 4) | (1 << 7),
    ) {
        uart::log(b"LCD: D-PHY lane stop-state wait failed\r\n");
        return false;
    }

    // Command mode with LP commands, then the timings used by ESP-IDF.
    modify(DSI_HOST + 0x34, 1, 1);
    write(DSI_HOST + 0x94, 0);
    write(DSI_HOST + 0x9C, (50 << 16) | 104);
    write(DSI_HOST + 0x98, (46 << 16) | 128);
    write(DSI_HOST + 0x2C, (1 << 0) | (1 << 3) | (1 << 4));
    write(DSI_HOST + 0x08, (12 << 8) | 6);
    modify(DSI_HOST + 0xA4, 0xFF << 8, 0x3F << 8);
    // All generic and DCS packet forms use low-power mode.  Do not request an
    // ACK for each write: an uninitialised ST712x may not answer it, which
    // leaves this Host revision's command scheduler permanently occupied.
    // The generic settings are also required by this Host's internal command
    // FIFO, even though the panel packets below are DCS.
    write(DSI_HOST + 0x68, (0x7F << 8) | (0xF << 16) | (1 << 24));
    true
}

fn phy_write(address: u8, value: u8) {
    write(DSI_HOST + 0xB4, 0);
    write(DSI_HOST + 0xB8, (1 << 16) | address as u32);
    write(DSI_HOST + 0xB4, 1 << 1);
    write(DSI_HOST + 0xB4, 0);
    write(DSI_HOST + 0xB8, value as u32);
    write(DSI_HOST + 0xB4, 1 << 1);
    write(DSI_HOST + 0xB4, 0);
}

fn dcs_write(command: u8, data: &[u8]) -> bool {
    let size = data.len() + 1;
    if size > 2 {
        let mut first_word = command as u32;
        let first_data_bytes = data.len().min(3);
        for index in 0..first_data_bytes {
            first_word |= (data[index] as u32) << (8 * (index + 1));
        }
        if !wait_for(DSI_HOST + 0x74, 1 << 3, 0) {
            return false;
        }
        write(DSI_HOST + 0x70, first_word);

        let mut index = first_data_bytes;
        while index < data.len() {
            let mut word = 0;
            for byte_index in 0..4 {
                if index == data.len() {
                    break;
                }
                word |= (data[index] as u32) << (8 * byte_index);
                index += 1;
            }
            if !wait_for(DSI_HOST + 0x74, 1 << 3, 0) {
                return false;
            }
            write(DSI_HOST + 0x70, word);
        }
        if !wait_for(DSI_HOST + 0x74, 1 << 1, 0) {
            return false;
        }
        write(DSI_HOST + 0x6C, 0x39 | ((size as u32) << 8));
    } else {
        let parameter = data.first().copied().unwrap_or(0);
        let data_type = if size == 2 { 0x15 } else { 0x05 };
        if !wait_for(DSI_HOST + 0x74, 1 << 1, 0) {
            return false;
        }
        write(
            DSI_HOST + 0x6C,
            data_type | ((command as u32) << 8) | ((parameter as u32) << 16),
        );
    }

    true
}

/// Reads the standard three-byte DCS Display ID (command 0x04).
///
/// This is a physical-link diagnostic: unlike a write FIFO becoming empty, a
/// returned byte proves that the panel drove the DSI lanes during BTA.
#[allow(dead_code)]
fn read_display_id() -> Option<[u8; 3]> {
    // SET_MAXIMUM_RETURN_PACKET_SIZE, requesting three bytes.
    if !wait_for(DSI_HOST + 0x74, 1 << 1, 0) {
        return None;
    }
    write(DSI_HOST + 0x6C, 0x37 | (3 << 8));

    // Receive on VC0 and permit a Bus Turn-Around response.
    modify(DSI_HOST + 0x30, 0x3, 0);
    modify(DSI_HOST + 0x2C, 1 << 2, 1 << 2);
    if !wait_for(DSI_HOST + 0x74, 1 << 1, 0) {
        return None;
    }
    write(DSI_HOST + 0x6C, 0x06 | (0x04 << 8));

    // The command cannot leave the Host until its BTA phase has completed.
    if !wait_for(DSI_HOST + 0x74, 1 << 0, 1 << 0) || !wait_for(DSI_HOST + 0x74, 1 << 4, 0) {
        modify(DSI_HOST + 0x2C, 1 << 2, 0);
        return None;
    }
    let value = read(DSI_HOST + 0x70);
    modify(DSI_HOST + 0x2C, 1 << 2, 0);
    Some([value as u8, (value >> 8) as u8, (value >> 16) as u8])
}

fn start_video_pattern() {
    // The BSP requests 70 MHz from PLL_F240M. Its integer divider is 3, so the
    // real DPI clock is 80 MHz; host timing is nevertheless calculated from
    // the requested 70 MHz and the bridge front porch compensates the delta.
    modify(
        HP_SYS_CLKRST + 0x3C,
        (0x3 << 5) | (1 << 7) | (0xFF << 8),
        (1 << 5) | (1 << 7) | (2 << 8),
    );

    write(DSI_HOST + 0x0C, 0);
    write(DSI_HOST + 0x10, 0); // RGB565 configuration 1
    write(DSI_HOST + 0x14, 0);
    write(DSI_HOST + 0x3C, WIDTH);
    write(DSI_HOST + 0x40, 0);
    write(DSI_HOST + 0x44, 0);
    write(DSI_HOST + 0x48, 3);
    write(DSI_HOST + 0x4C, 69);
    write(DSI_HOST + 0x50, 1375);
    write(DSI_HOST + 0x54, 20);
    write(DSI_HOST + 0x58, 24);
    write(DSI_HOST + 0x5C, 200);
    write(DSI_HOST + 0x60, HEIGHT);

    // Start from a complete normal DPI-bridge configuration. ESP-IDF's VPG
    // helper does this first, then disables only the bridge's pixel output;
    // omitting these bridge timing registers leaves the Host VPG black.
    write(DSI_BRG + 0x00, 1);
    write(DSI_BRG + 0x18, 2); // RGB565 raw input type
    write(DSI_BRG + 0x30, 1524 | (HEIGHT << 16)); // vtotal, vdisplay
    write(DSI_BRG + 0x34, 24 | (20 << 16)); // vback porch, vsync
    write(DSI_BRG + 0x38, 917 | (WIDTH << 16)); // compensated htotal, hdisplay
    write(DSI_BRG + 0x3C, 40 | (2 << 16)); // hback porch, hsync
    write(DSI_BRG + 0x40, 1 | (WIDTH << 4)); // DPI on, underrun discard
    write(DSI_BRG + 0x04, 1);
    write(DSI_BRG + 0x44, 1);

    // Mirror esp_lcd_dpi_panel_set_pattern(): stop bridge pixels, commit,
    // then let the Host's VPG generate vertical bars instead of DMA input.
    write(DSI_BRG + 0x40, WIDTH << 4);
    write(DSI_BRG + 0x44, 1);
    // Burst-with-sync-pulses, LP porches, frame ACK, and VPG vertical bars.
    write(DSI_HOST + 0x38, 0x0001_FF02);

    write(DSI_HOST + 0x34, 0);
    write(DSI_HOST + 0x94, (1 << 0) | (1 << 1));
}

fn configure_video_dma() {
    // Same 80 MHz real DPI clock and compensated timings as the known-good
    // VPG path, but leave the Host pattern generator disabled.
    modify(
        HP_SYS_CLKRST + 0x3C,
        (0x3 << 5) | (1 << 7) | (0xFF << 8),
        (1 << 5) | (1 << 7) | (2 << 8),
    );

    write(DSI_HOST + 0x0C, 0);
    write(DSI_HOST + 0x10, 0);
    write(DSI_HOST + 0x14, 0);
    write(DSI_HOST + 0x3C, WIDTH);
    write(DSI_HOST + 0x40, 0);
    write(DSI_HOST + 0x44, 0);
    write(DSI_HOST + 0x48, 3);
    write(DSI_HOST + 0x4C, 69);
    write(DSI_HOST + 0x50, 1375);
    write(DSI_HOST + 0x54, 20);
    write(DSI_HOST + 0x58, 24);
    write(DSI_HOST + 0x5C, 200);
    write(DSI_HOST + 0x60, HEIGHT);

    write(DSI_BRG, 1); // force bridge register clock
    write(DSI_BRG + 0x08, 256); // 256 64-bit words per requested burst
    write(DSI_BRG + 0x0C, (1 << 31) | (WIDTH * HEIGHT * 16 / 64));
    write(DSI_BRG + 0x18, 2); // RGB565 input and output
    write(DSI_BRG + 0x30, 1524 | (HEIGHT << 16));
    write(DSI_BRG + 0x34, 24 | (20 << 16));
    write(DSI_BRG + 0x38, 917 | (WIDTH << 16));
    write(DSI_BRG + 0x3C, 40 | (2 << 16));
    write(DSI_BRG + 0x40, WIDTH << 4); // enable after DMA starts
    write(DSI_BRG + 0x84, 1 << 4); // DMA flow controller, one block/frame
    write(DSI_BRG + 0x88, 768); // 1024-word FIFO minus 256-word burst
    write(DSI_BRG + 0x04, 1);
    write(DSI_BRG + 0x44, 1);
}

struct DmaDisplay {
    addresses: [u32; 2],
    initialized: bool,
}

impl DmaDisplay {
    fn new(fb0: u32, fb1: u32) -> Self {
        Self {
            addresses: [fb0, fb1],
            initialized: false,
        }
    }

    fn initialize(&mut self) -> bool {
        // Enable/reset the independent DW-GDMA block. Unlike MSPI, resetting
        // this peripheral cannot disturb the flash XIP path. Do not read a
        // GDMA register before these clock gates are open: on ECO2 that bus
        // access does not return.
        if self.initialized {
            return true;
        }
        modify(HP_SYS_CLKRST + 0x14, 1 << 13, 1 << 13);
        modify(HP_SYS_CLKRST + 0x18, 1 << 5, 1 << 5);
        modify(HP_SYS_CLKRST + 0xC0, 1 << 21, 1 << 21);
        modify(HP_SYS_CLKRST + 0xC0, 1 << 21, 0);
        write(DW_GDMA + 0x58, 1);
        let mut timeout = 1_000_000;
        while read(DW_GDMA + 0x58) & 1 != 0 {
            if timeout == 0 {
                return false;
            }
            timeout -= 1;
        }
        // Match dw_gdma_hal_init(): bit 0 enables the controller and bit 1
        // enables global interrupt/status generation. We poll the raw channel
        // status, but the hardware HAL still enables both bits.
        write(DW_GDMA + 0x10, 3);

        let channel = DW_GDMA + 0x100;
        write(channel + 0x08, DSI_BRG_MEM as u32);
        write(channel + 0x10, WIDTH * HEIGHT * 2 / 8 - 1);
        // memory master 1 -> DSI master 0, increment -> fixed, 64-bit,
        // source burst 512 items, destination burst 256 items.
        write(
            channel + 0x18,
            1 | (1 << 6) | (3 << 8) | (3 << 11) | (8 << 14) | (7 << 18),
        );
        write(
            channel + 0x1C,
            (1 << 6) | (16 << 7) | (1 << 15) | (16 << 16),
        );
        write(channel + 0x20, 0); // contiguous source and destination
        write(
            channel + 0x24,
            1 | (1 << 17) | ((5 - 1) << 23) | ((2 - 1) << 27),
        );
        write(channel + 0x80, u32::MAX); // generate status events
        self.initialized = true;
        uart::log(b"LCD: DMA controller initialized\r\n");
        true
    }

    fn start(&mut self, framebuffer: usize) -> bool {
        if framebuffer >= self.addresses.len() || !self.initialize() {
            return false;
        }

        if read(DW_GDMA + 0x18) & 1 != 0 {
            return false;
        }

        let channel = DW_GDMA + 0x100;
        write(channel, self.addresses[framebuffer]);
        write(channel + 0x98, u32::MAX);
        write(DW_GDMA + 0x18, 0x101);
        true
    }

    fn log_status(&self) {
        uart::log_hex(b"LCD: GDMA CFG=", read(DW_GDMA + 0x10));
        uart::log_hex(b"LCD: GDMA CHEN=", read(DW_GDMA + 0x18));
        uart::log_hex(b"LCD: GDMA INT=", read(DW_GDMA + 0x188));
        uart::log_hex(b"LCD: GDMA STATUS=", read(DW_GDMA + 0x130));
        uart::log_hex(b"LCD: bridge FIFO=", read(DSI_BRG + 0x14));
        uart::log_hex(b"LCD: bridge raw int=", read(DSI_BRG + 0x54));
    }
}

fn backlight_on() {
    // Tab5 backlight is GPIO22. GPIO matrix reset state selects GPIO output.
    write(GPIO + 0x24, 1 << 22); // GPIO_ENABLE_W1TS
    write(GPIO + 0x08, 1 << 22); // GPIO_OUT_W1TS
}

fn sync_video_registers() {
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
    // Read-back drains posted peripheral writes without producing UART noise.
    let _ = read(DSI_HOST + 0x38);
    let _ = read(DSI_BRG + 0x40);
    let _ = read(DSI_BRG + 0x44);
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}

fn wait_for_bridge_fifo(minimum_words: u32) -> bool {
    for _ in 0..5_000_000 {
        if read(DSI_BRG + 0x14) & 0x3FFF >= minimum_words {
            return true;
        }
    }
    false
}

fn wait_for(address: usize, mask: u32, expected: u32) -> bool {
    for _ in 0..20_000_000 {
        if read(address) & mask == expected {
            return true;
        }
    }
    false
}

fn delay_ms(milliseconds: u32) {
    // The ESP32-P4 bootloader leaves the HP core at its 400 MHz default.  A
    // cycle-counter delay is therefore stable enough for panel reset/sleep
    // timings, unlike the former loop-count approximation.
    const CPU_CYCLES_PER_MS: u32 = 400_000;
    let start = cycle_count();
    let cycles = milliseconds.saturating_mul(CPU_CYCLES_PER_MS);
    while cycle_count().wrapping_sub(start) < cycles {
        core::hint::spin_loop();
    }
}

fn delay_us(microseconds: u32) {
    const CPU_CYCLES_PER_US: u32 = 400;
    let start = cycle_count();
    let cycles = microseconds.saturating_mul(CPU_CYCLES_PER_US);
    while cycle_count().wrapping_sub(start) < cycles {
        core::hint::spin_loop();
    }
}

#[inline(always)]
fn cycle_count() -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("rdcycle {value}", value = out(reg) value, options(nomem, nostack));
    }
    value
}

fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}

fn write(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) }
}

fn modify(address: usize, mask: u32, value: u32) {
    write(address, (read(address) & !mask) | (value & mask));
}
