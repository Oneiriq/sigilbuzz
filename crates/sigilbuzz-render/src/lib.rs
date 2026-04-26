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
//! # PNG round-trip
//!
//! [`encode_png`] / [`encode_png_alpha`] turn a pixmap back into a
//! self-contained PNG byte stream that round-trips through
//! [`decode_png`].
//!
//! ```
//! use sigilbuzz_render::{ColorPixmap, decode_png, encode_png};
//!
//! let mut p = ColorPixmap::new(2, 2);
//! p.data = vec![
//!     255, 0, 0, 255,  0, 255, 0, 255,
//!     0, 0, 255, 255,  255, 255, 255, 255,
//! ];
//! let bytes = encode_png(&p);
//! let back = decode_png(&bytes).unwrap();
//! assert_eq!(back, p);
//! ```
//!
//! # Out of scope
//!
//! - EBDT/EBLC mono bitmap embeds (deferred — modern bitmap fonts
//!   carry CBDT or sbix instead).
//! - Subpixel text positioning beyond what the trapezoid rasterizer
//!   naturally provides.
//! - Hinting.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

extern crate alloc;

mod affine;
mod bitmaps;
mod colrv1;
mod error;
mod flatten;
mod pixmap;
mod png_encode;
mod raster;
mod rasterizer;
mod svg;

pub use affine::Affine;
pub use bitmaps::{decode_png, rasterize_bitmap_glyph, rescale_bilinear};
pub use error::RenderError;
pub use flatten::{flatten, Segment, DEFAULT_TOLERANCE};
pub use pixmap::{ColorPixmap, Pixmap};
pub use png_encode::{encode_png, encode_png_alpha};
pub use rasterizer::Rasterizer;

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
