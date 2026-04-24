//! sigilbuzz-gpu — CPU-side outline encoder for GPU rasterisation.
//!
//! This crate consumes the per-glyph outlines exposed by
//! [`sigilbuzz::Face::glyph_outline`] and packs them into a flat,
//! GPU-friendly representation following Eric Lengyel's *Slug*
//! algorithm (Loop-Blinn family).
//!
//! Subsequent commits add the cubic-to-quadratic flattening pass
//! and the band decomposition driver. This commit lays down the
//! plain-old-data surface (`Vec2`, `Bbox`, `QuadSegment`, `Band`,
//! `SlugGlyph`).
//!
//! # No-std
//!
//! `sigilbuzz-gpu` builds with `--no-default-features`. It still uses
//! `alloc::vec::Vec`; the encoder does not require `std`.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

extern crate alloc;

mod types;

pub use types::{Band, Bbox, QuadSegment, SlugGlyph, Vec2};

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
