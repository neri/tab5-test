//! Tab5 ST7121-compatible MIPI-DSI output for ESP32-P4 ECO2.
//!
//! Scanout streams an RGB565 double framebuffer from PSRAM through DW-GDMA.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::delay::{delay_ms, delay_us};
use crate::gpio::Pin;
use crate::i2c;
use crate::uart;
use crate::{framebuffer::Framebuffer, gpio, icm, interrupts, psram::Psram};

mod st7121;

const DSI_HOST: usize = 0x500A_0000;
const DSI_BRG: usize = 0x500A_0800;
const DSI_BRG_MEM: usize = 0x5010_5000;
const DW_GDMA: usize = 0x5008_1000;
const HP_SYS_CLKRST: usize = 0x500E_6000;
const PMU: usize = 0x5011_5000;

/// `DSI_BRG_INT_RAW`/`INT_CLR` bit 0: the DPI FIFO ran dry mid-frame. The
/// bridge then drives its underrun output for the remainder of that frame,
/// which the panel shows as a solid light blue flash.
const UNDERRUN_STATUS: u32 = 1 << 0;
const VSYNC_STATUS: u32 = 1 << 1;

/// Frames that underran since `Display::start`. Kept in a static rather than
/// on `Display` so the shell can report it without borrowing the display the
/// frame loop owns.
static UNDERRUNS: AtomicU32 = AtomicU32::new(0);

/// Total frames whose scanout underran since the display started.
pub fn underrun_count() -> u32 {
    UNDERRUNS.load(Ordering::Relaxed)
}

const WIDTH: u32 = 720;
const HEIGHT: u32 = 1280;

/// Vertical timing, in panel lines.
///
/// These are the values the panel was brought up with, and the front porch is
/// **not** a free parameter despite the registers accepting a wide range.
/// Stretching it was tried as a way to widen the blanking interval -- during
/// which scanout reads no PSRAM at all, so the frame loop would have had the
/// memory to itself for longer -- but this panel visibly flickers at anything
/// other than 200, at least down to 37 Hz. It is a panel constraint, not a
/// controller one, so lowering the refresh rate is not a lever available here.
const VSYNC_LINES: u32 = 20;
const VBP_LINES: u32 = 24;
const VFP_LINES: u32 = 200;
const VTOTAL_LINES: u32 = VSYNC_LINES + VBP_LINES + HEIGHT + VFP_LINES;

/// `VID_HLINE_TIME`: one line in lane-byte-clock periods. With two lanes at
/// 960 Mbps the lane byte clock is 120 MHz, so a line is 1375/120 MHz =
/// 11.458 us, and 1524 lines give the panel's 57.3 Hz.
const HLINE_LANE_BYTE_CLOCKS: u32 = 1375;

#[derive(Clone, Copy)]
pub(super) struct InitCommand {
    command: u8,
    data: &'static [u8],
    delay_ms: u32,
}

/// A running DSI/DMA display and its double-buffered framebuffer storage.
///
/// The application owns its event loop and draws through `framebuffer_mut`;
/// this type owns only panel initialization and frame-boundary synchronization.
pub struct Display {
    framebuffer: Framebuffer,
    dma: DmaDisplay,
    sequence: u32,
    /// Whether the previous frame already underran, so a burst spanning
    /// several frames logs one line instead of one per frame.
    underrun_active: bool,
}

impl Display {
    /// Allocates the framebuffer. Callers should prepare it before calling
    /// `start`, because that starts scanout immediately.
    pub fn new(psram: Psram) -> Option<Self> {
        let Some(framebuffer) = Framebuffer::new(psram) else {
            uart::log(b"FB: framebuffer does not fit in mapped PSRAM\r\n");
            return None;
        };
        let address = framebuffer.address()?;
        Some(Self {
            framebuffer,
            dma: DmaDisplay::new(address),
            sequence: 0,
            underrun_active: false,
        })
    }

    pub fn framebuffer_mut(&mut self) -> &mut Framebuffer {
        &mut self.framebuffer
    }

    /// Initializes the panel and starts framebuffer scanout.
    pub fn start(&mut self) -> bool {
        if !init_panel() {
            return false;
        }
        configure_video_dma();
        if !self.dma.initialize() {
            uart::log(b"LCD: DW-GDMA initialization failed\r\n");
            return false;
        }
        let Some(address) = self.framebuffer.address() else {
            return false;
        };
        // Scanout reads must outrank CPU and cache PSRAM traffic before the
        // bridge starts consuming them, otherwise the very first full-frame
        // redraw can already underrun.
        icm::prioritize_display_reads();
        interrupts::install(address);
        unsafe {
            // INT_CLR is 0x54; 0x50 is INT_ENA. Clear both raw indications
            // rather than enabling an interrupt nothing services.
            write(DSI_BRG + 0x54, UNDERRUN_STATUS | VSYNC_STATUS);
        }
        if !self.dma.start() {
            uart::log(b"LCD: DW-GDMA setup failed\r\n");
            return false;
        }
        if !wait_for_bridge_fifo(256) {
            uart::log(b"LCD: Bridge FIFO prefill timeout\r\n");
            self.dma.log_status();
            return false;
        }
        // Start video with the DMA source already armed. No UART writes may
        // be inserted in this register sequence.
        unsafe {
            write(DSI_HOST + 0x38, 0x0000_FF02);
            write(DSI_HOST + 0x34, 0);
            write(DSI_HOST + 0x94, (1 << 0) | (1 << 1));
            write(DSI_BRG + 0x40, 1 | (WIDTH << 4));
            write(DSI_BRG + 0x44, 1);
        }
        sync_video_registers();
        uart::log(b"LCD: DMA 3/3 full-frame interrupt installed\r\n");
        set_backlight(true);
        uart::log(b"LCD: RGB565 framebuffer DMA active\r\n");
        self.sequence = interrupts::frame_sequence();
        true
    }

    /// Waits for the next frame boundary. Returns `None` after a DMA error,
    /// which has already been logged.
    pub fn wait_for_frame(&mut self) -> Option<()> {
        loop {
            let error = interrupts::dma_error();
            if error != 0 {
                uart::log_hex(b"LCD: DMA interrupt error=", error);
                self.dma.log_status();
                return None;
            }
            interrupts::wait_for_interrupt();
            let error = interrupts::dma_error();
            if error != 0 {
                uart::log_hex(b"LCD: DMA interrupt error=", error);
                self.dma.log_status();
                return None;
            }
            let next_sequence = interrupts::frame_sequence();
            if next_sequence != self.sequence {
                self.sequence = next_sequence;
                self.poll_underrun();
                return Some(());
            }
        }
    }

    /// Records a DPI FIFO underrun for the frame that just completed.
    ///
    /// The bridge sets this without any interrupt being enabled, so polling it
    /// once per frame costs one register read and turns the "screen flashed
    /// light blue" report into something the UART log can confirm.
    fn poll_underrun(&mut self) {
        if !take_underrun() {
            self.underrun_active = false;
            return;
        }
        // One line per burst: a starved redraw spans several frames, and
        // logging each of them over USB serial would itself steal bandwidth.
        // The frame number separates a one-off at pipeline start-up (frame 0
        // or 1, harmless) from one caused by a redraw seconds into the session.
        if !self.underrun_active {
            self.underrun_active = true;
            uart::log_hex(b"LCD: DPI FIFO underrun at frame=", self.sequence);
            uart::log_hex(b"LCD: DPI FIFO underruns total=", underrun_count());
        }
    }
}

/// Consumes the bridge's underrun indication, counting it if it was set.
///
/// The indication is a single sticky bit, so it says "at least one underrun
/// since this was last cleared" rather than how many. Anything wanting to
/// count them has to come back often enough that they cannot pile up behind
/// one bit -- the frame loop does that once per frame, and a long-running
/// command that never yields has to call this itself or it will read zero for
/// the whole time it was busy.
pub fn take_underrun() -> bool {
    let underran = unsafe { read(DSI_BRG + 0x58) } & UNDERRUN_STATUS != 0;
    if !underran {
        return false;
    }
    unsafe {
        write(DSI_BRG + 0x54, UNDERRUN_STATUS);
    }
    UNDERRUNS.fetch_add(1, Ordering::Relaxed);
    true
}

/// Stops framebuffer scanout left running by a previous boot, and puts the
/// interconnect arbitration back to its power-on state.
///
/// `startup::reboot` resets only the HP CPU core. Everything else keeps
/// running, so DW-GDMA streams the old framebuffer out of PSRAM straight
/// through the next boot's `psram::init` -- which resets the MSPI controller
/// and re-tunes the DQS phase underneath it. That was always wrong; it only
/// became visible once `icm` started giving this master priority over the CPU,
/// because then the CPU's own PSRAM accesses lose to it outright and bring-up
/// either hangs or leaves PSRAM returning corrupt data.
///
/// Call this before touching PSRAM on the way up, and again before triggering
/// a reboot so the bootloader's own flash reads are not competing with a DMA
/// master this firmware promoted.
pub fn quiesce_dma() {
    unsafe {
        // Registers of a block that was never clocked out of reset must not be
        // read: on ECO2 that bus access does not return. On a cold boot that
        // is also proof no transfer can be in flight, so there is nothing to
        // stop.
        let bus_clock = read(HP_SYS_CLKRST + 0x14) & (1 << 13) != 0;
        let register_clock = read(HP_SYS_CLKRST + 0x18) & (1 << 5) != 0;
        let held_in_reset = read(HP_SYS_CLKRST + 0xC0) & (1 << 21) != 0;
        if !bus_clock || !register_clock || held_in_reset {
            icm::restore_default_priority();
            return;
        }

        if read(DW_GDMA + 0x18) & 1 != 0 {
            // Clearing CHEN0's enable bit does not reliably stop a channel
            // mid-transfer. Request the abort through CHEN1 and wait for it,
            // as dw_gdma_ll_channel_abort does, so no AXI read is left in
            // flight against the PSRAM controller that is about to be reset.
            write(DW_GDMA + 0x1C, 0x101);
            let mut timeout = 1_000_000;
            while read(DW_GDMA + 0x1C) & 0x101 != 0 {
                if timeout == 0 {
                    uart::log(b"LCD: stale DMA channel abort timed out\r\n");
                    break;
                }
                timeout -= 1;
            }
        }

        // Then hold the block in reset briefly so nothing can re-arm it.
        modify(HP_SYS_CLKRST + 0xC0, 1 << 21, 1 << 21);
        modify(HP_SYS_CLKRST + 0xC0, 1 << 21, 0);
    }
    icm::restore_default_priority();
}

fn init_panel() -> bool {
    if !reset_lcd_panel() {
        uart::log(b"LCD: PI4IOE1 reset control failed\r\n");
        return false;
    }
    enable_dphy_ldo();
    enable_dsi_clock();

    if !init_phy() {
        uart::log(b"LCD: D-PHY lock timeout\r\n");
        unsafe {
            uart::log_hex(b"LCD: PHY status=", read(DSI_HOST + 0xB0));
            uart::log_hex(b"LCD: LDO_VO3=", read(PMU + 0x1C0));
            uart::log_hex(b"LCD: LDO_VO3_ANA=", read(PMU + 0x1C4));
            uart::log_hex(b"LCD: ref_clk_ctrl=", read(HP_SYS_CLKRST + 0x2C));
        }
        return false;
    }
    uart::log(b"LCD: D-PHY 4/4 ready\r\n");

    // The official ST7121 driver performs a software reset even when the Tab5
    // board-level reset has already been pulsed through the I/O expander.
    if !dcs_write(0x01, &[]) {
        uart::log(b"LCD: ST7121 software reset failed\r\n");
        return false;
    }
    delay_ms(120);

    for init in st7121::INIT {
        if !dcs_write(init.command, init.data) {
            uart::log(b"LCD: DCS FIFO timeout\r\n");
            uart::log_hex(b"LCD: failed command=", init.command as u32);
            unsafe {
                uart::log_hex(b"LCD: packet status=", read(DSI_HOST + 0x74));
                uart::log_hex(b"LCD: host int_st0=", read(DSI_HOST + 0xBC));
                uart::log_hex(b"LCD: host int_st1=", read(DSI_HOST + 0xC0));
                uart::log_hex(b"LCD: PHY status=", read(DSI_HOST + 0xB0));
            }
            return false;
        }
        // M5Stack's ST7121 driver leaves at least 5 ms after every command.
        delay_ms(init.delay_ms + 5);
    }
    // Commands are deliberately queued back-to-back, as ESP-IDF does.  Wait
    // only once before changing the Host from command to video mode.
    if !wait_for(DSI_HOST + 0x74, (1 << 0) | (1 << 2), (1 << 0) | (1 << 2)) {
        uart::log(b"LCD: final DCS drain timeout\r\n");
        unsafe {
            uart::log_hex(b"LCD: packet status=", read(DSI_HOST + 0x74));
        }
        return false;
    }
    uart::log(b"LCD: DCS init complete\r\n");
    true
}

fn reset_lcd_panel() -> bool {
    // Tab5's LCD reset is PI4IOE1 P4, not an ESP32-P4 GPIO. Use a small
    // open-drain software-I2C master on the board bus (SDA31/SCL32), avoiding
    // any dependency on an ECO2-aware peripheral HAL.
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

fn pi4ioe1_write(register: u8, value: u8) -> bool {
    i2c::board_bus().write(0x43, &[register, value]).is_ok()
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
    unsafe {
        write(LDO_VO3, 0x4020_0180);
        write(LDO_VO3_ANA, (9 << 28) | (6 << 23));
    }
    delay_ms(10);
}

fn enable_dsi_clock() {
    unsafe {
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
}

fn init_phy() -> bool {
    unsafe {
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
    }

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
    if !wait_for(
        DSI_HOST + 0xB0,
        (1 << 2) | (1 << 4) | (1 << 7),
        (1 << 2) | (1 << 4) | (1 << 7),
    ) {
        uart::log(b"LCD: D-PHY lane stop-state wait failed\r\n");
        return false;
    }

    unsafe {
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
    }
    true
}

fn phy_write(address: u8, value: u8) {
    unsafe {
        write(DSI_HOST + 0xB4, 0);
        write(DSI_HOST + 0xB8, (1 << 16) | address as u32);
        write(DSI_HOST + 0xB4, 1 << 1);
        write(DSI_HOST + 0xB4, 0);
        write(DSI_HOST + 0xB8, value as u32);
        write(DSI_HOST + 0xB4, 1 << 1);
        write(DSI_HOST + 0xB4, 0);
    }
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
        unsafe {
            write(DSI_HOST + 0x70, first_word);
        }

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
            unsafe {
                write(DSI_HOST + 0x70, word);
            }
        }
        if !wait_for(DSI_HOST + 0x74, 1 << 1, 0) {
            return false;
        }
        unsafe {
            write(DSI_HOST + 0x6C, 0x39 | ((size as u32) << 8));
        }
    } else {
        let parameter = data.first().copied().unwrap_or(0);
        let data_type = if size == 2 { 0x15 } else { 0x05 };
        if !wait_for(DSI_HOST + 0x74, 1 << 1, 0) {
            return false;
        }
        unsafe {
            write(
                DSI_HOST + 0x6C,
                data_type | ((command as u32) << 8) | ((parameter as u32) << 16),
            );
        }
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
    unsafe {
        write(DSI_HOST + 0x6C, 0x37 | (3 << 8));

        // Receive on VC0 and permit a Bus Turn-Around response.
        modify(DSI_HOST + 0x30, 0x3, 0);
        modify(DSI_HOST + 0x2C, 1 << 2, 1 << 2);
    }
    if !wait_for(DSI_HOST + 0x74, 1 << 1, 0) {
        return None;
    }
    unsafe {
        write(DSI_HOST + 0x6C, 0x06 | (0x04 << 8));
    }

    // The command cannot leave the Host until its BTA phase has completed.
    if !wait_for(DSI_HOST + 0x74, 1 << 0, 1 << 0) || !wait_for(DSI_HOST + 0x74, 1 << 4, 0) {
        unsafe {
            modify(DSI_HOST + 0x2C, 1 << 2, 0);
        }
        return None;
    }
    unsafe {
        let value = read(DSI_HOST + 0x70);
        modify(DSI_HOST + 0x2C, 1 << 2, 0);
        Some([value as u8, (value >> 8) as u8, (value >> 16) as u8])
    }
}

fn configure_video_dma() {
    unsafe {
        // The BSP requests 70 MHz from PLL_F240M. Its integer divider is 3, so
        // the real DPI clock is 80 MHz; host timing is nevertheless calculated
        // from the requested 70 MHz and the bridge front porch compensates the
        // delta.
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
        write(DSI_HOST + 0x50, HLINE_LANE_BYTE_CLOCKS);
        write(DSI_HOST + 0x54, VSYNC_LINES);
        write(DSI_HOST + 0x58, VBP_LINES);
        write(DSI_HOST + 0x5C, VFP_LINES);
        write(DSI_HOST + 0x60, HEIGHT);

        write(DSI_BRG, 1); // force bridge register clock
        write(DSI_BRG + 0x08, 256); // 256 64-bit words per requested burst
        write(DSI_BRG + 0x0C, (1 << 31) | (WIDTH * HEIGHT * 16 / 64));
        write(DSI_BRG + 0x18, 2); // RGB565 input and output
        write(DSI_BRG + 0x30, VTOTAL_LINES | (HEIGHT << 16));
        write(DSI_BRG + 0x34, VBP_LINES | (VSYNC_LINES << 16));
        write(DSI_BRG + 0x38, 917 | (WIDTH << 16));
        write(DSI_BRG + 0x3C, 40 | (2 << 16));
        write(DSI_BRG + 0x40, WIDTH << 4); // enable after DMA starts
        // DMA_FLOW_CTRL is at 0x88 and RAW_BUF_ALMOST_EMPTY_THRD at 0x8C.
        // These two were previously written one register low, which left the
        // almost-empty threshold at its 512-word reset value: the bridge then
        // asked GDMA for the next burst only once the FIFO was half empty,
        // roughly halving the slack it has to absorb a PSRAM stall.
        write(DSI_BRG + 0x88, 1 << 4); // DMAC as flow controller, one block/frame
        write(DSI_BRG + 0x8C, 768); // refill request at 1024-word FIFO minus 256-word burst
        write(DSI_BRG + 0x04, 1);
        write(DSI_BRG + 0x44, 1);
    }
}

struct DmaDisplay {
    address: u32,
    initialized: bool,
}

impl DmaDisplay {
    fn new(address: u32) -> Self {
        Self {
            address,
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
        unsafe {
            modify(HP_SYS_CLKRST + 0x14, 1 << 13, 1 << 13);
            modify(HP_SYS_CLKRST + 0x18, 1 << 5, 1 << 5);
            modify(HP_SYS_CLKRST + 0xC0, 1 << 21, 1 << 21);
            modify(HP_SYS_CLKRST + 0xC0, 1 << 21, 0);
            write(DW_GDMA + 0x58, 1);
        }
        let mut timeout = 1_000_000;
        while unsafe { read(DW_GDMA + 0x58) } & 1 != 0 {
            if timeout == 0 {
                return false;
            }
            timeout -= 1;
        }
        unsafe {
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
        }
        self.initialized = true;
        true
    }

    fn start(&mut self) -> bool {
        if !self.initialize() {
            return false;
        }

        if unsafe { read(DW_GDMA + 0x18) } & 1 != 0 {
            return false;
        }

        unsafe {
            let channel = DW_GDMA + 0x100;
            write(channel, self.address);
            write(channel + 0x98, u32::MAX);
            write(DW_GDMA + 0x18, 0x101);
        }
        true
    }

    fn log_status(&self) {
        unsafe {
            uart::log_hex(b"LCD: GDMA CFG=", read(DW_GDMA + 0x10));
            uart::log_hex(b"LCD: GDMA CHEN=", read(DW_GDMA + 0x18));
            uart::log_hex(b"LCD: GDMA INT=", read(DW_GDMA + 0x188));
            uart::log_hex(b"LCD: GDMA STATUS=", read(DW_GDMA + 0x130));
            uart::log_hex(b"LCD: bridge FIFO=", read(DSI_BRG + 0x14));
            uart::log_hex(b"LCD: bridge raw int=", read(DSI_BRG + 0x54));
        }
    }
}

/// Tab5 backlight is GPIO22. GPIO matrix reset state selects GPIO output.
/// Exposed for the shell's `backlight` command as well as the two internal
/// bring-up call sites below.
pub fn set_backlight(on: bool) {
    gpio::enable_output(Pin::Backlight);
    if on {
        gpio::set_high(Pin::Backlight);
    } else {
        gpio::set_low(Pin::Backlight);
    }
}

fn sync_video_registers() {
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack));
        // Read-back drains posted peripheral writes without producing UART noise.
        let _ = read(DSI_HOST + 0x38);
        let _ = read(DSI_BRG + 0x40);
        let _ = read(DSI_BRG + 0x44);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}

fn wait_for_bridge_fifo(minimum_words: u32) -> bool {
    for _ in 0..5_000_000 {
        if unsafe { read(DSI_BRG + 0x14) } & 0x3FFF >= minimum_words {
            return true;
        }
    }
    false
}

fn wait_for(address: usize, mask: u32, expected: u32) -> bool {
    for _ in 0..20_000_000 {
        if unsafe { read(address) } & mask == expected {
            return true;
        }
    }
    false
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn write(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn modify(address: usize, mask: u32, value: u32) {
    unsafe {
        write(address, (read(address) & !mask) | (value & mask));
    }
}
