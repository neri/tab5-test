//! Full-screen 16 pixel font diagnostic.
//!
//! One static screen that puts every case the renderer has to get right next
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
    ("missing", "\u{20BB7} \u{1F600} \u{FDFD} <- boxes, never blanks"),
];

/// Body text at 1x, which is the size everything except headings uses.
const BODY: &str =
    "この画面は16ピクセルのビットマップフォントの見え方を確かめるためのものです。\n\
     漢字とかなの混じった長い文が、実機の画面でどのくらい読めるかを見ます。行の\n\
     高さは16ピクセル、英数字とラテン文字は8ピクセル送り、かなと漢字は16ピクセル\n\
     送りです。拡大は整数倍だけで、見出しは2倍の32ピクセルにします。";

pub fn run(framebuffer: &mut Framebuffer, input: &mut InputManager) {
    framebuffer.fill(BLACK);

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
    framebuffer.draw_text(x + 8 * 16 + 16, y, "<- no red or blue may remain", 1, WHITE, None);

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

    framebuffer.draw_text(MARGIN, 700, "any key exits", 1, CYAN, None);

    if !framebuffer.flush() {
        uart::log(b"Font test: flush failed\r\n");
        return;
    }
    uart::log(b"Font test: sheet displayed, press any key to exit\r\n");
    input.wait_for_key();
}
