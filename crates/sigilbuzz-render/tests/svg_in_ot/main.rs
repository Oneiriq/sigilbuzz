//! SVG-in-OT rasterization end-to-end.
//!
//! Builds a synthetic SFNT in memory carrying just enough tables for
//! `Face::svg_document` plus an `SVG ` table holding a hand-written
//! XML document. The rasterizer should:
//!
//! - Decode the document, walk `<path>` / `<g>` geometry, fill it via
//!   the existing trapezoid rasterizer.
//! - Honor `<g transform="scale(...)">` by scaling the rendered
//!   bitmap accordingly.
//! - Handle Bezier curves through the same flatten step the outline
//!   path uses.
//! - Surface `RenderError::SvgNotFound` for gids without records.

mod adversarial;
mod basic;
mod filters;
mod fonts;
mod masks;
mod paint;
mod placement;
mod shapes;
mod textpath;
