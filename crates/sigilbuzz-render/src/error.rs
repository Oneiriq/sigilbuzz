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
    /// The font has no SVG document for this glyph id (either the
    /// `SVG ` table is absent or the gid sits outside every record's
    /// range).
    SvgNotFound(u16),
    /// The SVG document for this glyph is gzip-compressed.
    /// `sigilbuzz-render` deliberately does not depend on a gzip
    /// decoder; the consumer is expected to decompress the payload
    /// themselves and feed it through a future bytes-based entry
    /// point. The SVG-in-OT spec allows both plain and gzipped
    /// payloads — Apple Color Emoji and Twitter Color Emoji ship the
    /// plain form, so this is rarer than it sounds.
    SvgGzipped,
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
            Self::SvgNotFound(g) => write!(f, "glyph {g} has no SVG document"),
            Self::SvgGzipped => f.write_str("SVG document is gzip-compressed"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RenderError {}
