//! Full-screen 16 pixel font diagnostic.
//!
//! Three static screens put every case the renderers have to get right next
//! to every other one: half-width and full-width side by side, a combining
//! mark that has to land on the character before it, characters the subset
//! does not cover that have to be boxes rather than blanks, an opaque repaint
//! over wider text that has to leave nothing of it behind, and a paragraph of
//! ordinary Japanese long enough to judge whether the font is actually
//! readable at 16 pixels.
//!
//! It draws once and holds. Nothing here is animated and nothing polls the
//! network: a full screen of text is a few milliseconds of PSRAM writes, far
//! short of the interval that makes the browser service the C6 link mid-draw.
//!
//! The strings are the ones fixed in `docs/FONT_MIGRATION_PLAN.md`, so what is
//! on the panel can be compared against what that document says should be.

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
const ROWS: [(&str, &str); 9] = [
    (
        "ascii  ",
        "!\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~",
    ),
    (
        "kana   ",
        "あいうえお アイウエオ ぁぃぅぇぉ ヴヵヶ がぎぐげご ぱぴぷぺぽ ゛゜ー〜",
    ),
    (
        "kanji  ",
        "日本語表示 東京都渋谷区 髙﨑 灣鬱靄 一二三四五六七八九十百千万",
    ),
    (
        "symbol ",
        "、。・「」『』（）［］｛｝〜―…※＿ ￥＄£ →←↑↓ ①②③ ★☆■□◆ ┌┬┐├┼┤└┴┘",
    ),
    ("hankaku", "ｱｲｳｴｵ ｶﾞｷﾞｸﾞ ﾊﾟﾋﾟﾌﾟ ｰ｡､･｢｣"),
    ("latin  ", "àéîõü ÀÉÎÕÜ ß æ œ Ł ż Ġ ΩΔμ авгд"),
    // Decomposed on purpose: each mark has to ride on the character before it
    // and take no width of its own.
    (
        "combine",
        "か\u{3099} き\u{309A} e\u{301}  (marks ride on the character before)",
    ),
    // This row *starts* with a mark, so there is nothing before it to ride on.
    // It has to become a visible character of its own rather than disappear.
    (
        "orphan ",
        "\u{3099} <- a mark with nothing before it draws U+FFFD",
    ),
    // U+20BB7 and the emoji are outside the BMP, which the source font does
    // not cover; U+FDFD is inside it but outside the subset. All three have to
    // draw a box.
    (
        "missing",
        "\u{20BB7} \u{1F600} \u{FDFD} <- boxes, never blanks",
    ),
];

/// Body text at 1x, which is the size everything except headings uses.
const BODY: &str = "この画面は16ピクセルのビットマップフォントの見え方を確かめるためのものです。\n\
     漢字とかなの混じった長い文が、実機の画面でどのくらい読めるかを見ます。行の\n\
     高さは16ピクセル、英数字とラテン文字は8ピクセル送り、かなと漢字は16ピクセル\n\
     送りです。拡大は整数倍だけで、見出しは2倍の32ピクセルにします。";

pub fn run(framebuffer: &mut Framebuffer, input: &mut InputManager) {
    #[cfg(feature = "font-drom-direct")]
    uart::log(b"Font test: source=plain DROM direct\r\n");
    #[cfg(not(feature = "font-drom-direct"))]
    uart::log(b"Font test: source=decoded PSRAM\r\n");
    framebuffer.fill(BLACK);
    let started = crate::delay::cycle_count();

    framebuffer.draw_text(MARGIN, 8, "fonttest", 2, WHITE, None);
    framebuffer.draw_text(MARGIN + 160, 16, "16px glyph renderer", 1, CYAN, None);

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
        (RED, "赤 red "),
        (GREEN, "緑 green "),
        (BLUE, "青 blue "),
        (CYAN, "水 cyan "),
        (YELLOW, "黄 yellow"),
    ] {
        // The return value is the advance the text actually took, which is
        // what makes laying pieces out left to right possible without the
        // caller counting characters.
        x += framebuffer.draw_text(x, y, sample, 1, color, None);
    }

    y += LINE;
    framebuffer.draw_text(MARGIN, y, "weight ", 1, YELLOW, None);
    let x = SAMPLE_X;
    let width = framebuffer.draw_text(x, y, "通常 normal  ", 1, WHITE, None);
    // Bold is the same glyph drawn twice, one physical pixel to the right.
    let bold = "太字 bold";
    framebuffer.draw_text(x + width, y, bold, 1, WHITE, None);
    framebuffer.draw_text(x + width + 1, y, bold, 1, WHITE, None);

    y += LINE;
    framebuffer.draw_text(MARGIN, y, "opaque ", 1, YELLOW, None);
    let x = SAMPLE_X;
    // Sixteen half-width cells of solid ink, then exactly the same 128 pixel
    // box repainted with eight full-width characters. Anything left of the
    // first row means an opaque repaint is not covering its own box.
    framebuffer.draw_text(x, y, "MMMMMMMMMMMMMMMM", 1, RED, Some(BLUE));
    framebuffer.draw_text(x, y, "あいうえおかきく", 1, WHITE, Some(BLACK));
    framebuffer.draw_text(
        x + 8 * 16 + 16,
        y,
        "<- no red or blue may remain",
        1,
        WHITE,
        None,
    );

    y += LINE + 8;
    framebuffer.draw_text(MARGIN, y, "見出し 32px Heading 0123", 2, WHITE, None);

    y += 32 + 12;
    framebuffer.draw_text(MARGIN, y, BODY, 1, WHITE, None);
    y += font::HEIGHT * 4 + 20;

    // Drawn as one string, so the newlines exercise the renderer's own line
    // pitch rather than a loop's. Box drawing joins only if that pitch is
    // exactly the glyph height and the advance is exactly the glyph width: a
    // pixel either way shows up as a broken corner or a doubled rule.
    framebuffer.draw_text(MARGIN, y, "frame  ", 1, YELLOW, None);
    framebuffer.draw_text(
        SAMPLE_X,
        y,
        "\u{250C}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}\n\
         \u{2502} joined  \u{2502}\n\
         \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}",
        1,
        WHITE,
        None,
    );
    // Eight full blocks have to be one solid 64 pixel bar with no seams.
    framebuffer.draw_text(
        SAMPLE_X + 8 * 16,
        y + font::HEIGHT,
        "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588} <- solid, no seams",
        1,
        WHITE,
        None,
    );

    let draw_us = crate::delay::cycle_count().wrapping_sub(started) / 360;
    uart::log_u32(b"Font test: legacy glyph phase us=", draw_us);
    let footer_width = framebuffer.draw_text(MARGIN, 700, "source: ", 1, CYAN, None);
    let footer_width = footer_width
        + framebuffer.draw_text(
            MARGIN + footer_width,
            700,
            font::STORAGE_LABEL,
            1,
            YELLOW,
            None,
        );
    framebuffer.draw_text(
        MARGIN + footer_width,
        700,
        " — any key: A4 UI fonts",
        1,
        CYAN,
        None,
    );

    if !framebuffer.flush() {
        uart::log(b"Font test: flush failed\r\n");
        return;
    }
    uart::log(b"Font test: legacy sheet displayed, press any key for A4 UI fonts\r\n");
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
        "Mixed: Tab5 Browser 日本語 / e\u{301} legacy cluster",
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
        "Mono 24: iIl1|MWMW|012345",
        font::UiTextStyle::new(font::UiFace::Mono, 24),
        GREEN,
        None,
    );
    draw_compare_row(
        framebuffer,
        578,
        "Mono 32: iIl1|MW|0123",
        font::UiTextStyle::new(font::UiFace::Mono, 32),
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
