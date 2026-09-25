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
    /// No CBDT/CBLC or sbix strike covers this glyph.
    NoBitmap(u16),
    /// The bitmap embed used an encoding we don't decode (e.g. sbix
    /// `'jpg '` / `'tiff'` / `'dupe'`, or CBDT mask formats 1-9).
    UnsupportedBitmap,
    /// A bitmap embed parsed structurally but could not be decoded,
    /// used by the EBDT composite (formats 8 / 9) recursion guard for
    /// cycles, self-references, and out-of-range component glyph ids.
    /// The static string identifies which guard tripped.
    BitmapDecodeFailed(&'static str),
    /// PNG payload failed structural validation (signature, IHDR,
    /// chunk shape) or zlib inflate.
    BadPng(&'static str),
    /// JPEG payload failed structural validation (SOI, marker shape,
    /// SOF0, DQT, DHT, SOS, entropy stream) or used an unsupported
    /// feature (progressive scan, arithmetic coding, 16-bit
    /// precision, JPEG2000 / TIFF, restart markers, etc.). The
    /// static string identifies the specific failure.
    BadJpeg(&'static str),
    /// TIFF payload failed structural validation (header magic, byte
    /// order mark, IFD shape, strip offsets, length mismatch). The
    /// static string identifies the specific failure. Well-formed but
    /// out-of-scope features (LZW / JPEG-in-TIFF / tiled / planar /
    /// non-RGB photometric / non-8-bit) surface as
    /// [`RenderError::UnsupportedBitmap`] instead.
    BadTiff(&'static str),
    /// The font has no SVG document for this glyph id (either the
    /// `SVG ` table is absent or the gid sits outside every record's
    /// range).
    SvgNotFound(u16),
    /// The SVG document for this glyph is gzip-compressed.
    /// `sigilbuzz-render` does not depend on a gzip
    /// decoder; the consumer is expected to decompress the payload
    /// themselves and feed it through a future bytes-based entry
    /// point. The SVG-in-OT spec allows both plain and gzipped
    /// payloads. Apple Color Emoji and Twitter Color Emoji ship the
    /// plain form, so this is rarer than it sounds.
    SvgGzipped,
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
            Self::NoBitmap(g) => write!(f, "glyph {g} has no embedded bitmap"),
            Self::UnsupportedBitmap => {
                write!(f, "embedded bitmap uses an unsupported payload format")
            }
            Self::BitmapDecodeFailed(msg) => write!(f, "bitmap decode failed: {msg}"),
            Self::BadPng(msg) => write!(f, "PNG decode failed: {msg}"),
            Self::BadJpeg(msg) => write!(f, "JPEG decode failed: {msg}"),
            Self::BadTiff(msg) => write!(f, "TIFF decode failed: {msg}"),
            Self::SvgNotFound(g) => write!(f, "glyph {g} has no SVG document"),
            Self::SvgGzipped => f.write_str("SVG document is gzip-compressed"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RenderError {}
