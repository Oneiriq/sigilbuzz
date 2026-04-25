//! `sigilbuzz-render` — software CPU rasterizer for sigilbuzz.
//!
//! Turns glyph outlines from [`sigilbuzz::Face::glyph_outline`] /
//! [`sigilbuzz::Face::glyph_outline_at_coords`] into 8-bit alpha
//! [`Pixmap`]s, and composes COLRv0 layered colour glyphs against a
//! CPAL palette into RGBA [`ColorPixmap`]s. Pure Rust, no runtime
//! deps; the rasterizer is a clean-room non-zero-winding trapezoid
//! scanline algorithm with 256-level anti-aliasing.
//!
//! ```text
//!   Face                           Pixmap
//!     │                             ┌──────────────┐
//!     │ glyph_outline(gid)          │ width: u32   │
//!     ▼                             │ height: u32  │
//!   Outline (PathOps)               │ data: Vec<u8>│  (alpha)
//!     │                             └──────────────┘
//!     │ flatten curves
//!     ▼
//!   Edges per scanline
//!     │
//!     │ trapezoid scan + winding
//!     ▼
//!   Pixmap (8-bit alpha)
//! ```
//!
//! # Quick start
//!
//! ```no_run
//! use sigilbuzz::{Blob, Face};
//! use sigilbuzz_render::Rasterizer;
//!
//! let blob = Blob::from_path("./MyFont.ttf").unwrap();
//! let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
//! let rast = Rasterizer::new();
//! let pix = rast.rasterize_glyph(&face, 42, 48.0, &[]).unwrap();
//! let _ = pix.data;
//! ```
//!
//! # Out of scope
//!
//! - SVG-in-OT, CBDT/CBLC, EBDT/EBLC, sbix bitmap embeds.
//! - Subpixel text positioning beyond what the trapezoid rasterizer
//!   naturally provides.
//! - Hinting.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

extern crate alloc;

mod affine;
mod colrv1;
mod error;
mod flatten;
mod pixmap;
mod raster;
mod rasterizer;

pub use affine::Affine;
pub use error::RenderError;
pub use pixmap::{ColorPixmap, Pixmap};
pub use rasterizer::Rasterizer;

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
