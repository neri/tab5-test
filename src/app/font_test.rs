//! Full-screen direct-ROM ASCII and A4 UI font diagnostic.
//!
//! The first screen proves that the uncompressed 1bpp DROM font contains only
//! printable ASCII. The next two exercise Latin and Japanese A4 coverage.
//!
//! It draws once and holds. Nothing here is animated and nothing polls the
//! network: a full screen of text is a few milliseconds of PSRAM writes, far
//! short of the interval that makes the browser service the C6 link mid-draw.
//!
//! The labels make the storage source and the deliberate 1-bit/A4 difference
//! visible without needing a serial log beside the panel.

use crate::font;
use crate::framebuffer::{BLACK, BLUE, CYAN, Framebuffer, GREEN, RED, WHITE, YELLOW};
use crate::input::InputManager;
use crate::uart;

/// Left margin, matching the console's.
const MARGIN: usize = 16;
/// Where the samples start: eight half-width cells of label.
const SAMPLE_X: usize = MARGIN + 8 * 8;
/// One 16 pixel line plus 4 pixels of leading.
const LINE: usize = 20;

/// The repertoire rows: a label and the characters it stands for.
const ROWS: [(&str, &str); 5] = [
    (
        "punct  ",
        "!\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~",
    ),
    ("upper  ", "ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
    ("lower  ", "abcdefghijklmnopqrstuvwxyz"),
    ("digits ", "0123456789  0O 1Il 5S 8B"),
    (
        "boxes  ",
        "é ｱ あ 漢 → <- every non-ASCII item is a box here",
    ),
];

/// Body text at 1x, which is the size everything except headings uses.
const BODY: &str = "This page reads the 3,175-byte 1bpp ASCII font directly from ROM.\n\
     It is never copied to PSRAM and it is not LZ4-compressed. All non-ASCII\n\
     text deliberately becomes a visible box on this page. Normal GUI text\n\
     uses the A4 Latin and Japanese font shown on the next page.";

pub fn run(framebuffer: &mut Framebuffer, input: &mut InputManager) {
    #[cfg(feature = "font-drom-direct")]
    uart::log(b"Font test: ASCII=plain DROM, A4=plain DROM direct\r\n");
    #[cfg(not(feature = "font-drom-direct"))]
    uart::log(b"Font test: ASCII=plain DROM, A4=decoded PSRAM\r\n");
    framebuffer.fill(BLACK);
    let started = crate::delay::cycle_count();

    framebuffer.draw_text(MARGIN, 8, "fonttest", 2, WHITE, None);
    framebuffer.draw_text(MARGIN + 160, 16, "plain ROM ASCII only", 1, CYAN, None);

    let mut y = 56;
    for (label, sample) in ROWS {
        framebuffer.draw_text(MARGIN, y, label, 1, YELLOW, None);
        framebuffer.draw_text(SAMPLE_X, y, sample, 1, WHITE, None);
        y += LINE;
    }

    y += 8;
    framebuffer.draw_text(MARGIN, y, "colour ", 1, YELLOW, None);
    let mut x = SAMPLE_X;
    for (color, sample) in [
        (RED, "red "),
        (GREEN, "green "),
        (BLUE, "blue "),
        (CYAN, "cyan "),
        (YELLOW, "yellow"),
    ] {
        // The return value is the advance the text actually took, which is
        // what makes laying pieces out left to right possible without the
        // caller counting characters.
        x += framebuffer.draw_text(x, y, sample, 1, color, None);
    }

    y += LINE;
    framebuffer.draw_text(MARGIN, y, "weight ", 1, YELLOW, None);
    let x = SAMPLE_X;
    let width = framebuffer.draw_text(x, y, "normal  ", 1, WHITE, None);
    // Bold is the same glyph drawn twice, one physical pixel to the right.
    let bold = "bold";
    framebuffer.draw_text(x + width, y, bold, 1, WHITE, None);
    framebuffer.draw_text(x + width + 1, y, bold, 1, WHITE, None);

    y += LINE;
    framebuffer.draw_text(MARGIN, y, "opaque ", 1, YELLOW, None);
    let x = SAMPLE_X;
    // Repaint the same sixteen ASCII cells with narrower ink. Anything left
    // from the first pass means an opaque repaint did not clear its own box.
    framebuffer.draw_text(x, y, "MMMMMMMMMMMMMMMM", 1, RED, Some(BLUE));
    framebuffer.draw_text(x, y, "iiiiiiiiiiiiiiii", 1, WHITE, Some(BLACK));
    framebuffer.draw_text(
        x + 8 * 16 + 16,
        y,
        "<- no red or blue may remain",
        1,
        WHITE,
        None,
    );

    y += LINE + 8;
    framebuffer.draw_text(MARGIN, y, "32px ASCII Heading 0123", 2, WHITE, None);

    y += 32 + 12;
    framebuffer.draw_text(MARGIN, y, BODY, 1, WHITE, None);
    y += font::HEIGHT * 4 + 20;

    framebuffer.draw_text(MARGIN, y, "frame  ", 1, YELLOW, None);
    framebuffer.draw_text(
        SAMPLE_X,
        y,
        "+----------+\n\
         | ROM ASCII|\n\
         +----------+",
        1,
        WHITE,
        None,
    );
    let draw_us = crate::delay::cycle_count().wrapping_sub(started) / 360;
    uart::log_u32(b"Font test: ASCII ROM glyph phase us=", draw_us);
    let footer_width =
        framebuffer.draw_text(MARGIN, 700, "source: ASCII plain DROM", 1, YELLOW, None);
    framebuffer.draw_text(
        MARGIN + footer_width,
        700,
        " - any key: A4 UI fonts",
        1,
        CYAN,
        None,
    );

    if !framebuffer.flush() {
        uart::log(b"Font test: flush failed\r\n");
        return;
    }
    uart::log(b"Font test: ASCII ROM sheet displayed, press any key for A4 UI fonts\r\n");
    input.wait_for_key();
    draw_ui_sheet(framebuffer);
    input.wait_for_key();
    draw_aa_comparison(framebuffer);
    input.wait_for_key();
}

fn draw_ui_sheet(framebuffer: &mut Framebuffer) {
    framebuffer.fill(BLACK);
    let started = crate::delay::cycle_count();
    framebuffer.draw_gui_text(MARGIN, 8, "A4 proportional / monospace", 2, WHITE, None);
    framebuffer.draw_gui_text(
        MARGIN,
        52,
        "Sans 16: AVATAR To Wi-Fi  iIl1Wm  café — € ™",
        1,
        WHITE,
        None,
    );
    framebuffer.draw_ui_text(
        MARGIN,
        80,
        "Mono 16: AVATAR To Wi-Fi  iIl1Wm  00:00 23:59",
        font::UiTextStyle::MONO,
        CYAN,
        None,
    );
    framebuffer.draw_ui_text(
        MARGIN,
        110,
        "Sans 24: proportional anti-aliased text",
        font::UiTextStyle::new(font::UiFace::Sans, 24),
        YELLOW,
        None,
    );
    framebuffer.draw_ui_text(
        MARGIN,
        148,
        "Mono 24: code() 0123456789",
        font::UiTextStyle::new(font::UiFace::Mono, 24),
        GREEN,
        None,
    );
    framebuffer.draw_gui_text(MARGIN, 190, "Sans 32: Heading 0123", 2, WHITE, None);
    framebuffer.draw_ui_text(
        MARGIN,
        232,
        "Mono 32: 0123456789",
        font::UiTextStyle::new(font::UiFace::Mono, 32),
        CYAN,
        None,
    );
    framebuffer.fill_rect(MARGIN, 286, 590, 60, BLUE);
    framebuffer.draw_gui_text(
        MARGIN + 12,
        300,
        "Blue selection: anti-aliased edge",
        1,
        WHITE,
        None,
    );
    framebuffer.fill_rect(MARGIN, 358, 590, 60, 0x0430);
    framebuffer.draw_gui_text(
        MARGIN + 12,
        372,
        "Theme teal: AVATAR To Wi-Fi",
        1,
        YELLOW,
        None,
    );
    framebuffer.draw_gui_text(
        MARGIN,
        438,
        "Mixed: Tab5 Browser 日本語 / e\u{301} combining cluster",
        1,
        WHITE,
        None,
    );
    let width = framebuffer.draw_gui_text(MARGIN, 474, "Bold overstrike: Wi-Fi", 1, WHITE, None);
    framebuffer.draw_gui_text(MARGIN + 1, 474, "Bold overstrike: Wi-Fi", 1, WHITE, None);
    framebuffer.draw_gui_text(
        MARGIN + width + 20,
        474,
        "transparent background",
        1,
        RED,
        None,
    );
    framebuffer.draw_gui_text(
        MARGIN,
        530,
        "日本語16px: 漢字かな交じり文 髙﨑・東京都・ブラウザ表示",
        1,
        WHITE,
        None,
    );
    framebuffer.draw_gui_text(
        MARGIN,
        565,
        "日本語32px（16px A4を2倍描画）",
        2,
        YELLOW,
        None,
    );
    let draw_us = crate::delay::cycle_count().wrapping_sub(started) / 360;
    uart::log_u32(b"Font test: A4 glyph phase us=", draw_us);
    framebuffer.draw_gui_text(MARGIN, 690, "any key: A4 / 1-bit comparison", 1, CYAN, None);
    if !framebuffer.flush() {
        uart::log(b"Font test: A4 sheet flush failed\r\n");
    } else {
        uart::log(b"Font test: A4 sheet displayed, press any key for AA comparison\r\n");
    }
}

const COMPARE_LEFT: usize = 24;
const COMPARE_RIGHT: usize = 656;

fn draw_compare_row(
    framebuffer: &mut Framebuffer,
    y: usize,
    text: &str,
    style: font::UiTextStyle,
    foreground: u16,
    background: Option<u16>,
) {
    framebuffer.draw_ui_text(COMPARE_LEFT, y, text, style, foreground, background);
    framebuffer.draw_ui_text_1bpp(COMPARE_RIGHT, y, text, style, foreground, background);
}

/// Side-by-side AA comparison. Both columns read the same A4 glyph records;
/// only the right column thresholds coverage at 8, so advances, bearings and
/// outline rasterisation cannot accidentally bias the comparison.
fn draw_aa_comparison(framebuffer: &mut Framebuffer) {
    framebuffer.fill(BLACK);
    framebuffer.draw_gui_text(
        MARGIN,
        8,
        "AA test — same glyphs and metrics",
        1,
        WHITE,
        None,
    );
    framebuffer.draw_gui_text(COMPARE_LEFT, 38, "A4 coverage (0..15)", 1, CYAN, None);
    framebuffer.draw_gui_text(COMPARE_RIGHT, 38, "1-bit (A4 >= 8)", 1, YELLOW, None);
    framebuffer.draw_gui_text(1020, 8, font::STORAGE_LABEL, 1, YELLOW, None);
    framebuffer.fill_rect(638, 36, 2, 626, 0x4208);

    draw_compare_row(
        framebuffer,
        74,
        "16px  iIl1|AVATAR|Sphinx|0123456789",
        font::UiTextStyle::new(font::UiFace::Sans, 16),
        WHITE,
        None,
    );
    draw_compare_row(
        framebuffer,
        102,
        "curves: aeocsg  diagonals: AVWXYZ  fine: .,:;'!",
        font::UiTextStyle::new(font::UiFace::Sans, 16),
        WHITE,
        None,
    );
    draw_compare_row(
        framebuffer,
        148,
        "24px iIl1 AVATAR Sphinx 012345",
        font::UiTextStyle::new(font::UiFace::Sans, 24),
        WHITE,
        None,
    );
    draw_compare_row(
        framebuffer,
        184,
        "curves aeocsg / diagonals AVWXYZ",
        font::UiTextStyle::new(font::UiFace::Sans, 24),
        WHITE,
        None,
    );
    draw_compare_row(
        framebuffer,
        240,
        "32px iIl1 AVATAR 0123",
        font::UiTextStyle::new(font::UiFace::Sans, 32),
        WHITE,
        None,
    );
    draw_compare_row(
        framebuffer,
        282,
        "Sphinx aeocsg AVWXYZ",
        font::UiTextStyle::new(font::UiFace::Sans, 32),
        WHITE,
        None,
    );

    // The same edges against dark colour and light backgrounds make lost
    // coverage or colour fringes much easier to see than black alone.
    framebuffer.fill_rect(COMPARE_LEFT, 342, 584, 58, BLUE);
    framebuffer.fill_rect(COMPARE_RIGHT, 342, 584, 58, BLUE);
    draw_compare_row(
        framebuffer,
        357,
        "24px colour: Sphinx AVWXYZ 0123",
        font::UiTextStyle::new(font::UiFace::Sans, 24),
        YELLOW,
        Some(BLUE),
    );
    framebuffer.fill_rect(COMPARE_LEFT, 414, 584, 58, WHITE);
    framebuffer.fill_rect(COMPARE_RIGHT, 414, 584, 58, WHITE);
    draw_compare_row(
        framebuffer,
        429,
        "24px light: Sphinx AVWXYZ 0123",
        font::UiTextStyle::new(font::UiFace::Sans, 24),
        BLACK,
        Some(WHITE),
    );

    draw_compare_row(
        framebuffer,
        500,
        "Mono 16: iIl1|MWMW|00:00|23:59",
        font::UiTextStyle::new(font::UiFace::Mono, 16),
        CYAN,
        None,
    );
    draw_compare_row(
        framebuffer,
        534,
        "日本語16: 漢字かな 髙﨑 東京",
        font::UiTextStyle::new(font::UiFace::Sans, 16),
        GREEN,
        None,
    );
    draw_compare_row(
        framebuffer,
        578,
        "日本語32: 漢字かな",
        font::UiTextStyle::new(font::UiFace::Sans, 32),
        WHITE,
        None,
    );

    framebuffer.draw_gui_text(
        MARGIN,
        674,
        "Compare readability, curves and diagonals at normal viewing distance — any key exits",
        1,
        CYAN,
        None,
    );
    if !framebuffer.flush() {
        uart::log(b"Font test: AA comparison flush failed\r\n");
    } else {
        uart::log(
            b"Font test: AA comparison displayed (A4 left, 1-bit right); press any key to exit\r\n",
        );
    }
}
