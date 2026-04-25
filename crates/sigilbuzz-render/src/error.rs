//! Error type for the rasterizer.

use core::fmt;

/// Errors that can be returned from the rasterizer entry points.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderError {
    /// The requested glyph id was out of range, had no outline, or
    /// the underlying parser refused the data.
    NoOutline(u16),
    /// The COLR table did not carry a v0 record for this glyph.
    NoColrV0(u16),
    /// The COLR table did not carry a v1 paint record for this glyph
    /// (either no v1 extension on the table, or no `BaseGlyphPaintRecord`
    /// for the requested gid).
    ColrV1NotFound(u16),
    /// The font carried no CPAL table, so palette resolution is
    /// impossible.
    NoCpal,
    /// The requested palette index sits outside the CPAL palette
    /// count, or a layer's palette entry is out of range.
    BadPaletteIndex {
        /// Active palette index requested.
        palette: u16,
        /// Entry within that palette.
        entry: u16,
    },
    /// The size in points was non-finite or non-positive.
    BadSize(f32),
    /// `units_per_em` was zero / unparseable.
    BadUpem,
    /// Underlying sigilbuzz parser returned an error while pulling
    /// tables we needed (head, COLR, CPAL).
    Parse(&'static str),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOutline(g) => write!(f, "glyph {g} has no outline"),
            Self::NoColrV0(g) => write!(f, "glyph {g} has no COLRv0 record"),
            Self::ColrV1NotFound(g) => write!(f, "glyph {g} has no COLRv1 paint record"),
            Self::NoCpal => write!(f, "font has no CPAL table"),
            Self::BadPaletteIndex { palette, entry } => {
                write!(f, "palette/entry index out of range: {palette}/{entry}")
            }
            Self::BadSize(s) => write!(f, "bad rasterization size {s}"),
            Self::BadUpem => write!(f, "font has zero or unparseable units_per_em"),
            Self::Parse(msg) => write!(f, "parser error: {msg}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RenderError {}
