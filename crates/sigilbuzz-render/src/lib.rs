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
//! # Per-Bézier flattening for MSDF
//!
//! [`flatten_grouped`] is a sibling of [`flatten`] that keeps each
//! source curve's chord chunk grouped under a [`FlattenedCurve`]
//! variant. Use this when downstream code needs per-source-Bézier
//! identity (MSDF RGB edge coloring, signed-distance generators,
//! etc.) — it replaces the workaround of calling [`flatten`] one
//! tiny `MoveTo + draw` op pair at a time per Bézier.
//!
//! ```
//! use sigilbuzz_render::{flatten_grouped, FlattenedCurve, Affine, DEFAULT_TOLERANCE};
//! use sigilbuzz::tables::PathOp;
//!
//! let ops = vec![
//!     PathOp::MoveTo { x: 0.0, y: 0.0 },
//!     PathOp::CubicTo { c1x: 50.0, c1y: 100.0, c2x: 100.0, c2y: 100.0, x: 100.0, y: 0.0 },
//!     PathOp::Close,
//! ];
//! let curves = flatten_grouped(ops, &Affine::identity(), DEFAULT_TOLERANCE);
//! assert_eq!(curves.len(), 2); // cubic + close-line
//! match &curves[0] {
//!     FlattenedCurve::Cubic(segs) => assert!(segs.len() > 1),
//!     _ => panic!("expected Cubic"),
//! }
//! ```
//!
//! # Out of scope
//!
//! - sbix `'jp2 '` decoding (deferred — JPEG-2000 is rare in font
//!   embeds and would be its own substantial decoder). sbix `'jpg '`
//!   *is* now decoded via the hand-rolled baseline decoder in
//!   [`decode_jpeg`], and sbix `'tiff'` is decoded via the baseline
//!   subset in [`decode_tiff`] (uncompressed RGB(A) and PackBits;
//!   LZW / JPEG-in-TIFF / tiled / multi-IFD remain unsupported).
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
mod jpeg_decode;
mod pixmap;
mod png_encode;
mod raster;
mod rasterizer;
mod svg;
mod tiff_decode;

pub use affine::Affine;
pub use bitmaps::{decode_ebdt_mono, decode_png, rasterize_bitmap_glyph, rescale_bilinear};
pub use error::RenderError;
pub use flatten::{
    arc_length_cubic, arc_length_cubic_solve_t, arc_length_quad, arc_length_quad_solve_t, flatten,
    flatten_grouped, FlattenedCurve, Segment, DEFAULT_TOLERANCE,
};
pub use jpeg_decode::decode_jpeg;
pub use pixmap::{ColorPixmap, Pixmap};
pub use png_encode::{encode_png, encode_png_alpha};
pub use rasterizer::Rasterizer;
pub use tiff_decode::decode_tiff;

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
