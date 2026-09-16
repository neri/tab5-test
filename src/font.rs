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
//! `docs/plans/archive/FONT_MIGRATION_PLAN.md` records how and why.

// Deliberately narrow: each name arrives with the code that needs it, so the
// list doubles as a record of how far the migration has got.
#[cfg(not(feature = "font-drom-direct"))]
use alloc::vec::Vec;

pub use tab5_font::{Glyph, HEIGHT, MAX_WIDTH, console, glyph_or_replacement, text_width};

pub use tab5_ui_font::{Face as UiFace, Glyph as UiGlyph, TextStyle as UiTextStyle};

pub fn ui_text_width(text: &str, style: UiTextStyle) -> usize {
    tab5_ui_font::text_width(text, style)
}

pub const STORAGE_LABEL: &str = if cfg!(feature = "font-drom-direct") {
    "DROM direct"
} else {
    "PSRAM decoded"
};

#[cfg(not(feature = "font-drom-direct"))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PsramInstallError {
    OutOfMemory,
    Ui(tab5_ui_font::InstallError),
}

#[cfg(not(feature = "font-drom-direct"))]
pub struct PsramFontStorage {
    pub ui_bytes: usize,
    pub compressed_bytes: usize,
    pub ui_address: usize,
}

#[cfg(not(feature = "font-drom-direct"))]
fn permanent_buffer(length: usize) -> Result<&'static mut [u8], PsramInstallError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(length)
        .map_err(|_| PsramInstallError::OutOfMemory)?;
    buffer.resize(length, 0);
    Ok(buffer.leak())
}

/// Expands the A4 LZ4 DROM font blob into permanent decoded PSRAM.
/// Must be called after the global PSRAM allocator is ready and before UI.
#[cfg(not(feature = "font-drom-direct"))]
pub fn install_psram() -> Result<PsramFontStorage, PsramInstallError> {
    let ui = permanent_buffer(tab5_ui_font::STORAGE_BYTES)?;
    tab5_ui_font::install_psram(ui).map_err(PsramInstallError::Ui)?;
    Ok(PsramFontStorage {
        ui_bytes: tab5_ui_font::STORAGE_BYTES,
        compressed_bytes: tab5_ui_font::COMPRESSED_BYTES,
        ui_address: tab5_ui_font::psram_address().unwrap(),
    })
}
