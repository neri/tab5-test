//! The 16 pixel bitmap font, re-exported from the `tab5-font` crate.
//!
//! Nothing is implemented here. The glyph data and the `advance` lookup live
//! in `font/src/` as a separate crate for the same reason `browser` does: it
//! builds for the host, so `cargo test` can check coverage, widths and the
//! console's cell mapping without a board. It is also what keeps the firmware
//! and `tab5-browser` measuring text with the same function instead of each
//! keeping its own idea of how wide a character is.
//!
//! This file exists so the crate boundary does not leak into paths: inside
//! the firmware it is `crate::font::advance`, the same as if the data sat
//! under `src/font/`.
//!
//! This replaced a 5x7 ASCII font that every screen used to draw through;
//! `docs/FONT_MIGRATION_PLAN.md` records how and why.

// Deliberately narrow: each name arrives with the code that needs it, so the
// list doubles as a record of how far the migration has got.
pub use tab5_font::{
    Glyph, HEIGHT, MAX_WIDTH, advance, console, glyph_or_replacement, text_width,
};
