//! `sigilbuzz-paint` — COLRv1 paint evaluator.
//!
//! sigilbuzz parses the COLRv1 paint tree as a borrowed enum
//! ([`sigilbuzz::tables::colr::ColrPaint`]); this companion crate walks
//! that tree and emits a flat stream of [`DrawCmd`]s a renderer can
//! turn into pixels. The walker:
//!
//! - composes nested affine transforms into a single 2x3 matrix per
//!   leaf,
//! - resolves [`sigilbuzz::tables::colr::ColorLine`] stops against the
//!   active [`sigilbuzz::tables::cpal::Cpal`] palette, applying the
//!   per-stop alpha,
//! - emits [`DrawCmd::PushLayer`] / [`DrawCmd::PopLayer`] pairs around
//!   `PaintComposite` children so the consumer can drive
//!   blend-mode-aware compositing,
//! - recurses through [`sigilbuzz::tables::colr::ColrPaint::ColrGlyph`]
//!   references with a visited-set so cyclic DAGs terminate.
//!
//! The walker never panics on malformed input. A bad sub-offset, an
//! unknown paint format, or a cycle truncates the [`DrawCmd`] stream;
//! it never produces a partially-constructed paint.
//!
//! ```no_run
//! use sigilbuzz::Face;
//! use sigilbuzz_paint::{evaluate, DrawCmd};
//!
//! # fn demo(face: &Face<'_>) {
//! let cmds: Vec<DrawCmd> = evaluate(face, 42);
//! for cmd in &cmds {
//!     match cmd {
//!         DrawCmd::FillGlyph { gid, transform, paint } => {
//!             // hand off to your rasteriser
//!             let _ = (gid, transform, paint);
//!         }
//!         DrawCmd::PushLayer { composite_mode } => {
//!             let _ = composite_mode;
//!         }
//!         DrawCmd::PopLayer => {}
//!     }
//! }
//! # }
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod color;
mod eval;
mod gradient;
mod transform;

pub use color::Color;
pub use eval::{evaluate, evaluate_at_coords, DrawCmd, GlyphId, PaintSource};
pub use gradient::{ColorStop, Extend, Gradient, GradientKind};
pub use sigilbuzz::tables::colr::CompositeMode;
pub use transform::Transform2D;
