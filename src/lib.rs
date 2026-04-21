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

pub mod ot;
pub mod tables;
pub mod unicode;

pub use blob::Blob;
pub use buffer::{Buffer, Direction, Glyph};
pub use error::{Error, Result};
pub use face::Face;
pub use font::Font;
pub use shape::{shape, Feature};

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
