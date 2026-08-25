//! The hypertext viewer's non-UI layers, re-exported from the
//! `tab5-browser` crate.
//!
//! Nothing is implemented here. URL parsing, HTML tokenizing and the
//! document model live in `browser/src/` as a separate crate with no
//! dependencies, for one reason: that crate builds for the host, so its
//! behaviour can be pinned down by `cargo test` instead of only by looking
//! at the panel. This binary cannot -- `main.rs` and every module beside it
//! is RISC-V.
//!
//! This file exists so the split does not leak into paths. Inside the
//! firmware the modules are `crate::browser::url` and friends, the same as
//! if they sat under `src/browser/`, and a call site does not have to know
//! which side of the crate boundary something is on.
//!
//! The parts that cannot be pure stay outside it and stay here in `src/`:
//! `net::http` does the fetching, `app::browser` owns the screen.

// `html` is not among these. The tokenizer is reachable only through
// `document::Parser`, which is the pairing that has any meaning: a token
// stream with no document builder behind it is not something the firmware
// has a use for, and re-exporting it would invite one.
pub use tab5_browser::{document, error, layout, limits, memory, url};
