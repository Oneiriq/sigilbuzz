//! sigilbuzz-gpu: CPU-side outline encoder for GPU rasterization.
//!
//! This crate consumes the per-glyph outlines exposed by
//! [`sigilbuzz::Face::glyph_outline`] and packs them into a flat,
//! GPU-friendly representation following Eric Lengyel's *Slug*
//! algorithm (Loop-Blinn family).
//!
//! # Pipeline
//!
//! ```text
//!   Face                     SlugGlyph
//!     |                       +--------------------------+
//!     | glyph_outline(gid)    | bbox: Bbox               |
//!     v                       | bands: Vec<Band>         |
//!   Outline (PathOps)         | segments: Vec<QuadSeg>   |
//!     |                       +--------------------------+
//!     | flatten cubics
//!     v
//!   QuadPath
//!     |
//!     | band-decompose (N bands tiling the bbox y-range)
//!     v
//!   SlugGlyph
//! ```
//!
//! The output buffers (`bands`, `segments`) are designed so a consumer
//! can upload them as SSBOs / texture buffers and run a fragment
//! shader that walks the band's segment list to compute coverage.
//! The shader side is out of scope: sigilbuzz-gpu only
//! produces the encoded data.
//!
//! # Quick start
//!
//! ```no_run
//! use sigilbuzz::{Blob, Face};
//! use sigilbuzz_gpu::{encode_glyph, SlugOptions};
//!
//! let blob = Blob::from_path("./MyFont.ttf").unwrap();
//! let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
//! let glyph = encode_glyph(&face, 42, &SlugOptions::default()).unwrap();
//! // upload glyph.bands and glyph.segments to the GPU.
//! # let _ = glyph;
//! ```
//!
//! # No-std
//!
//! The encoder needs only `alloc`. The crate still depends on
//! `sigilbuzz` with its default `std` feature, so even with
//! `--no-default-features` it needs a target with `std`.
//!
//! # References
//!
//! - Eric Lengyel, "GPU-Centered Font Rendering Directly from Glyph
//!   Outlines", Journal of Computer Graphics Techniques (JCGT), 2017.
//! - Charles Loop, Jim Blinn, "Resolution Independent Curve Rendering
//!   using Programmable Graphics Hardware", SIGGRAPH 2005.
//! - Thomas Sederberg, *Computer Aided Geometric Design*, section 5.4: the
//!   third-difference error bound used by the cubic flattening pass.
//!
//! The companion shader-side reference is HarfBuzz's `hb_gpu`; we
//! omit a default shader here so consumers can target
//! whatever graphics API (Metal, Vulkan, WebGPU, ...) suits them.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

extern crate alloc;

mod encoder;
mod flatten;
mod types;

pub use encoder::{encode_glyph, encode_glyph_at_coords, SlugOptions};
pub use types::{Band, Bbox, QuadSegment, SlugGlyph, Vec2};

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
