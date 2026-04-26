//! sigilbuzz — a modern pure-Rust text shaping engine.
//!
//! # Overview
//!
//! sigilbuzz turns a run of Unicode scalar values into a sequence of
//! positioned glyphs drawn from a chosen font. The pipeline follows the
//! `HarfBuzz` shape:
//!
//! ```text
//!   Blob (raw bytes)  →  Face (parsed SFNT directory)
//!                         │
//!                         ↓
//!                       Font (Face + size)    +    Buffer (text + state)
//!                                       \      │
//!                                        ↓     ↓
//!                                       shape(font, buffer, features)
//!                                                 │
//!                                                 ↓
//!                                            Vec<Glyph>
//! ```
//!
//! See [`Blob`], [`Face`], [`Font`], [`Buffer`], and [`shape`] for the
//! pieces in order.
//!
//! # `no_std`
//!
//! The crate compiles with `--no-default-features` on stable Rust. The
//! default `std` feature enables filesystem helpers and a richer `Error`
//! implementation. Everything on the shaping path works without either.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

extern crate alloc;

mod blob;
mod buffer;
mod error;
mod face;
mod font;
mod shape;

// Public-but-experimental: the OpenType Layout module exposes the
// shape-engine internals (Arabic / Indic / USE / Mongolian / Tibetan
// state machines, feature tag constants). The shaper is a stable API
// via [`shape`]; the per-script machinery here is not — its types and
// signatures will move as the shaping pipeline evolves toward 1.0. The
// module is `pub` so companion crates that experiment with custom
// shapers can still reach it, but it is hidden from rustdoc to signal
// that consumers cannot depend on its shape across releases. See
// `docs/STABILITY.md`.
#[doc(hidden)]
pub mod ot;
pub mod tables;
// Public-but-experimental: the Unicode property tables backing the
// shaper. `unicode::Script` is already consumed by `sigilbuzz-capi`
// for the `hb_script_t` mapping, so the module stays `pub`. The full
// UCD-derived data is still being filled in release-by-release;
// consumers should not pin against the internal shape. Hidden from
// rustdoc until the surface settles. See `docs/STABILITY.md`.
#[doc(hidden)]
pub mod unicode;

pub use blob::Blob;
pub use buffer::{Buffer, Direction, Glyph};
pub use error::{Error, Result};
pub use face::{Face, GlyphBitmapEntry};
pub use font::Font;
pub use shape::{shape, Feature};

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
