//! Integration tests for embedded bitmap rasterization.
//!
//! Drives [`Rasterizer::rasterize_bitmap_glyph`] against the
//! synthetic CBDT and sbix fixtures already used by the parser tests
//! (`tests/fixtures/cbdt_synthetic.ttf`,
//! `tests/fixtures/sbix_synthetic.ttf`). Both fixtures ship a 1x1
//! transparent RGBA PNG at a 32 ppem strike, small but enough to
//! validate the full pipeline (face -> strike -> PNG decode -> rescale).

mod ebdt;
mod fixtures;
mod pipeline;
mod sbix;
