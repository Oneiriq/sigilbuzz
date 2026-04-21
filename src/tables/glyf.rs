//! `glyf` — TrueType glyph data, bounding-box view only.
//!
//! The full `glyf` table encodes outlines: simple glyphs store
//! contour points, composite glyphs compose other glyphs by
//! reference. sigilbuzz does not rasterize today, so this parser
//! only exposes the ten-byte glyph header — enough to answer the
//! one question text layout engines actually ask of `glyf`: "what
//! is the bounding box of glyph N at native font units?"
//!
//! # Glyph header
//!
//! ```text
//!   i16    numberOfContours   (>=0 simple, -1 composite)
//!   FWord  xMin
//!   FWord  yMin
//!   FWord  xMax
//!   FWord  yMax
//! ```
//!
//! A zero-byte glyph (the two `loca` offsets are equal) has no
//! header and is treated here as "no bounding box" rather than an
//! error. That matches how whitespace glyphs commonly encode.

use crate::error::{Error, Result};
use crate::tables::loca::Loca;
use crate::tables::parse::Reader;

/// Glyph bounding box in font design units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphBounds {
    /// Left edge.
    pub x_min: i16,
    /// Bottom edge.
    pub y_min: i16,
    /// Right edge.
    pub x_max: i16,
    /// Top edge.
    pub y_max: i16,
    /// Number of contours; negative for composite glyphs.
    pub num_contours: i16,
}

/// A borrowed view of the `glyf` table. Parsing is free — accessors
/// slice into the underlying bytes on demand.
#[derive(Debug, Clone, Copy)]
pub struct Glyf<'a> {
    data: &'a [u8],
}

impl<'a> Glyf<'a> {
    /// Wraps the raw `glyf` bytes. No validation up front — the
    /// table is too large and too dense to validate whole-table in
    /// linear time; accessors bound-check each read.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Returns the bounding box for `glyph_id`. The glyph's byte
    /// range comes from `loca`; an empty range means the glyph has
    /// no outline and `None` is returned.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Truncated`] when the range `loca` reports
    /// falls outside the `glyf` bytes.
    pub fn bounds(&self, loca: &Loca<'_>, glyph_id: u16) -> Result<Option<GlyphBounds>> {
        let Some((start, end)) = loca.range(glyph_id) else {
            return Ok(None);
        };
        if start == end {
            return Ok(None);
        }
        let start = start as usize;
        let end = end as usize;
        if end > self.data.len() || start > end {
            return Err(Error::Truncated {
                offset: start,
                context: "glyf range from loca falls outside glyf table",
            });
        }
        let body = &self.data[start..end];
        if body.len() < 10 {
            return Err(Error::Truncated {
                offset: start,
                context: "glyf header shorter than 10 bytes",
            });
        }
        let mut r = Reader::new(body);
        let num_contours = r.read_i16()?;
        let x_min = r.read_i16()?;
        let y_min = r.read_i16()?;
        let x_max = r.read_i16()?;
        let y_max = r.read_i16()?;
        Ok(Some(GlyphBounds {
            x_min,
            y_min,
            x_max,
            y_max,
            num_contours,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::head::IndexToLocFormat;
    use alloc::vec::Vec;

    fn build_header(num_contours: i16, xmin: i16, ymin: i16, xmax: i16, ymax: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&num_contours.to_be_bytes());
        out.extend_from_slice(&xmin.to_be_bytes());
        out.extend_from_slice(&ymin.to_be_bytes());
        out.extend_from_slice(&xmax.to_be_bytes());
        out.extend_from_slice(&ymax.to_be_bytes());
        out
    }

    fn build_loca_short(offsets: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for o in offsets {
            out.extend_from_slice(&o.to_be_bytes());
        }
        out
    }

    #[test]
    fn reads_bounds_from_simple_glyph() {
        // Two glyphs:
        //   glyph 0: no outline (both offsets 0)
        //   glyph 1: contours=1, bbox (10, -200, 500, 1500)
        let g0_body: Vec<u8> = Vec::new();
        let g1_body = build_header(1, 10, -200, 500, 1500);

        let mut glyf = Vec::new();
        glyf.extend_from_slice(&g0_body);
        glyf.extend_from_slice(&g1_body);

        // loca offsets in short form (halved): 0, 0, (g1_body.len()/2).
        let loca_bytes = build_loca_short(&[0, 0, (g1_body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
        let glyf_view = Glyf::new(&glyf);

        assert!(glyf_view.bounds(&loca, 0).unwrap().is_none());
        let b = glyf_view.bounds(&loca, 1).unwrap().unwrap();
        assert_eq!(b.num_contours, 1);
        assert_eq!(b.x_min, 10);
        assert_eq!(b.y_min, -200);
        assert_eq!(b.x_max, 500);
        assert_eq!(b.y_max, 1500);
    }

    #[test]
    fn composite_glyph_reports_negative_contour_count() {
        let body = build_header(-1, 0, 0, 1000, 1000);
        let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&body);
        let b = glyf.bounds(&loca, 0).unwrap().unwrap();
        assert_eq!(b.num_contours, -1);
    }

    #[test]
    fn out_of_range_glyph_yields_none_from_loca() {
        let loca_bytes = build_loca_short(&[0, 10]);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&[0u8; 20]);
        assert!(glyf.bounds(&loca, 7).unwrap().is_none());
    }

    #[test]
    fn rejects_range_past_glyf_end() {
        // loca claims glyph 0 occupies bytes 0..100 but the glyf
        // slice is only 8 bytes long.
        let loca_bytes = build_loca_short(&[0, 50]); // 50 * 2 = 100
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&[0u8; 8]);
        assert!(glyf.bounds(&loca, 0).is_err());
    }

    #[test]
    fn rejects_range_shorter_than_header() {
        // Range is within glyf but only 6 bytes — too short for a
        // 10-byte header.
        let loca_bytes = build_loca_short(&[0, 3]); // 3 * 2 = 6
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
        let glyf = Glyf::new(&[0u8; 10]);
        assert!(glyf.bounds(&loca, 0).is_err());
    }
}
