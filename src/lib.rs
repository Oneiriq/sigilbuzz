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

mod bidi_map;
mod blob;
mod buffer;
mod error;
mod face;
mod font;
mod owned;
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

pub use bidi_map::BidiMap;
pub use blob::Blob;
pub use buffer::{Buffer, Direction, Glyph};
pub use error::{Error, Result};
pub use face::{Face, GlyphBitmapEntry};
pub use font::Font;
pub use owned::OwnedFace;
pub use shape::{shape, Feature};

// --- Curated stable re-exports from `ot::*` --------------------------------
//
// The `ot` module itself stays `#[doc(hidden)]` because most of its
// surface (Arabic / Indic / USE / Mongolian / Tibetan state machines)
// is implementation detail subject to redesign before 1.0. The items
// re-exported here are the subset that downstream consumers writing
// custom shapers or feature pipelines reasonably want as crate-root
// names — see `docs/STABILITY.md`.
//
// Promoted in 0.20.0 (audit follow-up #235):
//   - `ot::feature` — OpenType feature-tag byte-literal constants
//     (LIGA, KERN, CALT, etc.) usable as `Feature::tag` keys.
//   - `ot::arabic::JoiningForm` — the Arabic joining-form enum, the
//     stable output of `ot::arabic::assign_joining_forms`.
pub use ot::arabic::JoiningForm;
pub use ot::feature;

// --- Curated stable re-exports from `unicode::*` ---------------------------
//
// As with `ot`, the `unicode` module remains `#[doc(hidden)]` while
// its core property-classification surface graduates to a stable
// crate-root name. `Script` is re-exported as `UnicodeScript` to
// disambiguate from any future `ot::Script` (script-tag enum); the
// `script_of` and `is_hangul_jamo` helpers are the canonical
// char-to-script entry points.
//
// Promoted in 0.20.0:
//   - `unicode::Script as UnicodeScript` — coarse script bucket
//     consumed by `sigilbuzz-capi` for `hb_script_t` mapping.
//   - `unicode::script_of`, `unicode::is_hangul_jamo` — char
//     classifiers.
//   - `unicode::bidi::BidiInfo` — UAX #9 result type (per-char
//     embedding levels + paragraph direction + L2 reorder).
//   - `unicode::bidi_class::{BidiClass, bidi_class}` — the
//     UCD `Bidi_Class` enum and the char-to-class lookup.
//   - `unicode::joining::JoiningType` — Arabic / Mongolian
//     joining-type enum.
pub use unicode::bidi::BidiInfo;
pub use unicode::bidi_class::{bidi_class, BidiClass};
pub use unicode::joining::JoiningType;
pub use unicode::{is_hangul_jamo, script_of, Script as UnicodeScript};

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
