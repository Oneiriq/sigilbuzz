//! `Face` accessors for color and bitmap glyph tables (`COLR`,
//! `CPAL`, `CBLC` / `CBDT`, `EBLC` / `EBDT`, `sbix`, `SVG `) and the
//! [`GlyphBitmapEntry`] lookup result.

use super::Face;
use crate::error::{Error, Result};
use crate::tables::{tag, Cbdt, Cblc, Ebdt, EbdtBitmap, Eblc, GlyphBitmap, Sbix, Svg, SvgDocument};

impl<'a> Face<'a> {
    /// Parses the `COLR` table if the font carries one. Color fonts
    /// ship this alongside `CPAL`; monochrome outlines-only fonts
    /// omit both.
    pub fn colr(&self) -> Result<Option<crate::tables::colr::Colr<'a>>> {
        match self.table_bytes(tag::COLR) {
            Ok(bytes) => Ok(Some(crate::tables::colr::Colr::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `CPAL` table if the font carries one.
    pub fn cpal(&self) -> Result<Option<crate::tables::cpal::Cpal<'a>>> {
        match self.table_bytes(tag::CPAL) {
            Ok(bytes) => Ok(Some(crate::tables::cpal::Cpal::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Convenience: resolves the paint subtree for `glyph_id` through
    /// the `COLR` table, returning `Ok(None)` when either the font has
    /// no COLR or the glyph has no color record.
    pub fn colr_paint(&self, glyph_id: u16) -> Result<Option<crate::tables::colr::ColrPaint<'a>>> {
        match self.colr()? {
            Some(colr) => Ok(colr.paint(glyph_id)),
            None => Ok(None),
        }
    }

    /// Parses the `CBLC` table (color bitmap location) if the font
    /// carries one. Always paired with `CBDT` in the wild.
    pub fn cblc(&self) -> Result<Option<Cblc<'a>>> {
        match self.table_bytes(tag::CBLC) {
            Ok(bytes) => Ok(Some(Cblc::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `CBDT` table (color bitmap data) if the font carries
    /// one. Pairs with `CBLC`; consumers usually go through
    /// [`Face::glyph_bitmap`] instead of touching either directly.
    pub fn cbdt(&self) -> Result<Option<Cbdt<'a>>> {
        match self.table_bytes(tag::CBDT) {
            Ok(bytes) => Ok(Some(Cbdt::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `EBLC` table (Microsoft monochrome bitmap location)
    /// if the font carries one. Always paired with `EBDT` in the wild.
    /// Modern color-emoji fonts use CBDT/CBLC instead; EBLC/EBDT
    /// shows up in legacy Asian text fonts and a handful of bitmap-
    /// only display faces.
    pub fn eblc(&self) -> Result<Option<Eblc<'a>>> {
        match self.table_bytes(tag::EBLC) {
            Ok(bytes) => Ok(Some(Eblc::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `EBDT` table (Microsoft monochrome bitmap data) if
    /// the font carries one. Pairs with `EBLC`; consumers usually go
    /// through [`Face::glyph_bitmap`] instead of touching either
    /// directly.
    pub fn ebdt(&self) -> Result<Option<Ebdt<'a>>> {
        match self.table_bytes(tag::EBDT) {
            Ok(bytes) => Ok(Some(Ebdt::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `sbix` table (Apple Standard Bitmap Graphics) if
    /// the font carries one. The strike's per-glyph offsets array is
    /// sized off `maxp.numGlyphs`, so `maxp` must be present.
    pub fn sbix(&self) -> Result<Option<Sbix<'a>>> {
        match self.table_bytes(tag::SBIX) {
            Ok(bytes) => {
                let num_glyphs = self.maxp()?.num_glyphs;
                Ok(Some(Sbix::parse(bytes, num_glyphs)?))
            }
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Parses the `SVG ` table (OpenType SVG) if the font carries
    /// one. Returns `Ok(None)` for fonts without inline SVG glyphs:
    /// most fonts in the wild, including all COLR-only color fonts.
    pub fn svg(&self) -> Result<Option<Svg<'a>>> {
        match self.table_bytes(tag::SVG) {
            Ok(bytes) => Ok(Some(Svg::parse(bytes)?)),
            Err(Error::MissingTable { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Convenience: returns the inline SVG document for `glyph_id`,
    /// or `Ok(None)` when the font has no `SVG ` table or no record
    /// covers the glyph.
    ///
    /// The returned [`SvgDocument`] borrows directly into the font
    /// blob; `data` is either plain SVG XML or a gzip stream. See
    /// the `gzipped` flag. sigilbuzz never decompresses or parses the
    /// SVG itself.
    pub fn svg_document(&self, glyph_id: u16) -> Result<Option<SvgDocument<'a>>> {
        Ok(self.svg()?.and_then(|svg| svg.document_for(glyph_id)))
    }

    /// Returns the bitmap glyph for `glyph_id` at the requested
    /// `ppem`, picking the closest available strike.
    ///
    /// Resolution order:
    /// 1. `CBDT` / `CBLC`: Google color emoji.
    /// 2. `sbix`: Apple color emoji.
    /// 3. `EBDT` / `EBLC`: Microsoft monochrome bitmap embeds.
    ///
    /// CBDT outranks EBDT because a font carrying both (rare but
    /// permitted) is overwhelmingly built around the color table;
    /// the mono path is the legacy fallback.
    ///
    /// Returns `Ok(None)` when no bitmap table is present, or the
    /// available strikes don't cover the glyph (legitimate for glyphs
    /// that have only outline data, e.g. ASCII fallbacks in an emoji
    /// font).
    ///
    /// The returned [`GlyphBitmapEntry`] is a tagged union of CBDT,
    /// sbix, and EBDT payloads. All three ride a `&'a [u8]` that
    /// points back into the original font blob, so cloning is cheap
    /// and there is no allocation on the lookup path. PNG / JPEG /
    /// TIFF / mask decoding is the consumer's job: sigilbuzz exposes
    /// the bytes, not pixels.
    pub fn glyph_bitmap(&self, glyph_id: u16, ppem: u16) -> Result<Option<GlyphBitmapEntry<'a>>> {
        if let Some(cblc) = self.cblc()? {
            if let Some(cbdt) = self.cbdt()? {
                if let Some(size) = cblc.best_strike(glyph_id, ppem) {
                    if let Some(loc) = cblc.locate(&size, glyph_id)? {
                        let bm = cbdt.glyph_bitmap(&loc)?;
                        return Ok(Some(GlyphBitmapEntry::Cbdt {
                            ppem_x: size.ppem_x,
                            ppem_y: size.ppem_y,
                            bitmap: bm,
                        }));
                    }
                }
            }
        }
        if let Some(sbix) = self.sbix()? {
            if let Some(strike) = sbix.best_strike(ppem) {
                if let Some(g) = strike.glyph(glyph_id)? {
                    return Ok(Some(GlyphBitmapEntry::Sbix {
                        ppem: strike.ppem(),
                        ppi: strike.ppi(),
                        glyph: g,
                    }));
                }
            }
        }
        if let Some(eblc) = self.eblc()? {
            if let Some(ebdt) = self.ebdt()? {
                if let Some(size) = eblc.best_strike(glyph_id, ppem) {
                    if let Some(loc) = eblc.locate(&size, glyph_id)? {
                        let bm = ebdt.glyph_bitmap(&loc)?;
                        return Ok(Some(GlyphBitmapEntry::Ebdt {
                            ppem_x: size.ppem_x,
                            ppem_y: size.ppem_y,
                            bitmap: bm,
                        }));
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Tagged result of [`Face::glyph_bitmap`]. Bitmap fonts come in two
/// flavors and the metrics shape differs enough that folding them
/// into one struct loses information; consumers match on the variant
/// they care about.
#[derive(Debug, Clone, Copy)]
pub enum GlyphBitmapEntry<'a> {
    /// Color Bitmap Data (Google CBDT/CBLC). `ppem_x` / `ppem_y` are
    /// the strike's design size; `bitmap.data` is raw payload bytes
    /// (PNG for formats 17/18/19).
    Cbdt {
        /// Strike X resolution (pixels-per-em).
        ppem_x: u8,
        /// Strike Y resolution (pixels-per-em).
        ppem_y: u8,
        /// Parsed CBDT entry (format, metrics, payload bytes).
        bitmap: GlyphBitmap<'a>,
    },
    /// Apple Standard Bitmap Graphics (sbix). `glyph.graphic_type`
    /// indicates the payload format (`'png '`, `'jpg '`, `'tiff'`,
    /// etc.).
    Sbix {
        /// Strike resolution (pixels-per-em).
        ppem: u16,
        /// Strike DPI.
        ppi: u16,
        /// Per-glyph entry (origin offsets, format tag, payload).
        glyph: crate::tables::SbixGlyph<'a>,
    },
    /// Microsoft Embedded Bitmap Data (EBDT/EBLC): monochrome 1bpp
    /// masks. Predecessor to CBDT/CBLC; same indexing model, mask
    /// payload instead of PNG.
    Ebdt {
        /// Strike X resolution (pixels-per-em).
        ppem_x: u8,
        /// Strike Y resolution (pixels-per-em).
        ppem_y: u8,
        /// Parsed EBDT entry (format, metrics, packing, mask bytes).
        bitmap: EbdtBitmap<'a>,
    },
}
