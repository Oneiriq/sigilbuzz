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
    /// No CBDT/CBLC or sbix strike covers this glyph.
    NoBitmap(u16),
    /// The bitmap embed used an encoding we don't decode (e.g. sbix
    /// `'jpg '` / `'tiff'` / `'dupe'`, or CBDT mask formats 1-9).
    UnsupportedBitmap,
    /// PNG payload failed structural validation (signature, IHDR,
    /// chunk shape) or zlib inflate.
    BadPng(&'static str),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOutline(g) => write!(f, "glyph {g} has no outline"),
            Self::NoColrV0(g) => write!(f, "glyph {g} has no COLRv0 record"),
            Self::NoCpal => write!(f, "font has no CPAL table"),
            Self::BadPaletteIndex { palette, entry } => {
                write!(f, "palette/entry index out of range: {palette}/{entry}")
            }
            Self::BadSize(s) => write!(f, "bad rasterization size {s}"),
            Self::BadUpem => write!(f, "font has zero or unparseable units_per_em"),
            Self::Parse(msg) => write!(f, "parser error: {msg}"),
            Self::NoBitmap(g) => write!(f, "glyph {g} has no embedded bitmap"),
            Self::UnsupportedBitmap => {
                write!(f, "embedded bitmap uses an unsupported payload format")
            }
            Self::BadPng(msg) => write!(f, "PNG decode failed: {msg}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RenderError {}
