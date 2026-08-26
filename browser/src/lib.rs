//! The non-UI half of the hypertext viewer: URLs, HTML tokenizing and the
//! document model.
//!
//! This is a separate crate from the firmware for one reason: everything in
//! here is pure, in the sense that it turns bytes into values and never
//! touches a register, a socket or the framebuffer. That makes it the only
//! part of the browser whose behaviour can be pinned down away from the
//! board -- feed a fixture in one byte at a time, compare the result -- and
//! keeping it clear of the firmware's hardware crates is what lets
//! `cargo test` build it for the host while the firmware keeps building for
//! `riscv32imafc`.
//!
//! The firmware re-exports it as `crate::browser` (`src/browser.rs`), so
//! module paths read the same on both sides.
//!
//! What this is *not* is a web browser. See `docs/WEB_BROWSER_PLAN.md`: no
//! CSS, no JavaScript, no images, no TLS. HTML comes in, text and links come
//! out, and anything else is skipped rather than guessed at.
//!
//! `layout` depends on `tab5-font` so that a line is broken by the same
//! advance widths the renderer paints with. That crate is data and a lookup,
//! not hardware, and it builds for the host too.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod document;
pub mod error;
pub mod html;
pub mod layout;
pub mod limits;
pub mod memory;
pub mod url;

pub use error::Error;
