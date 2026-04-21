//! OpenType `ClassDef` tables.
//!
//! `ClassDef` assigns a class value (a small integer) to every glyph
//! relevant to a feature. It is the mechanism behind class-based
//! GPOS pair kerning (where thousands of real glyph pairs collapse
//! into a handful of class-vs-class rules), GDEF glyph classes
//! (base / ligature / mark / component), and a few GSUB constructs.
//!
//! A glyph that is not explicitly assigned is implicitly in class 0.
//! Callers never need to distinguish "explicit 0" from "unassigned"
//! because the spec gives them the same semantic meaning.
//!
//! # Formats
//!
//! ## Format 1 — dense
//!
//! ```text
//!   u16 classFormat = 1
//!   u16 startGlyphID
//!   u16 glyphCount
//!   u16 classValueArray[glyphCount]
//! ```
//!
//! Looks up glyph `g` as `classValueArray[g - startGlyphID]` when
//! `g` falls in the declared range; everything else is class 0.
//!
//! ## Format 2 — range-based
//!
//! ```text
//!   u16 classFormat = 2
//!   u16 classRangeCount
//!   ClassRangeRecord records[classRangeCount]:
//!     u16 startGlyphID
//!     u16 endGlyphID
//!     u16 class
//! ```
//!
//! Ranges are sorted by `startGlyphID` and non-overlapping. Binary
//! search locates the range containing a given glyph; glyphs outside
//! any range are class 0.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// A parsed `ClassDef` table.
#[derive(Debug, Clone, Copy)]
pub struct ClassDef<'a> {
    inner: Inner<'a>,
}

#[derive(Debug, Clone, Copy)]
enum Inner<'a> {
    Format1 {
        data: &'a [u8],
        start_glyph_id: u16,
        glyph_count: u16,
        values_off: usize,
    },
    Format2 {
        data: &'a [u8],
        range_count: u16,
        ranges_off: usize,
    },
}

impl<'a> ClassDef<'a> {
    /// Parses a `ClassDef` table from its raw bytes.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        match r.read_u16()? {
            1 => {
                let start_glyph_id = r.read_u16()?;
                let glyph_count = r.read_u16()?;
                let values_off = r.position();
                let need = values_off + glyph_count as usize * 2;
                if data.len() < need {
                    return Err(Error::Truncated {
                        offset: values_off,
                        context: "classDef format 1 values shorter than glyphCount",
                    });
                }
                Ok(Self {
                    inner: Inner::Format1 {
                        data,
                        start_glyph_id,
                        glyph_count,
                        values_off,
                    },
                })
            }
            2 => {
                let range_count = r.read_u16()?;
                let ranges_off = r.position();
                let need = ranges_off + range_count as usize * 6;
                if data.len() < need {
                    return Err(Error::Truncated {
                        offset: ranges_off,
                        context: "classDef format 2 ranges shorter than rangeCount",
                    });
                }
                Ok(Self {
                    inner: Inner::Format2 {
                        data,
                        range_count,
                        ranges_off,
                    },
                })
            }
            _ => Err(Error::Malformed {
                offset: 0,
                context: "unsupported classDef format",
            }),
        }
    }

    /// Class value assigned to `glyph_id`. Returns `0` for glyphs
    /// that are not listed — both formats treat unlisted glyphs as
    /// the default class.
    #[must_use]
    pub fn class_of(&self, glyph_id: u16) -> u16 {
        match self.inner {
            Inner::Format1 {
                data,
                start_glyph_id,
                glyph_count,
                values_off,
            } => {
                if glyph_id < start_glyph_id {
                    return 0;
                }
                let offset = glyph_id - start_glyph_id;
                if offset >= glyph_count {
                    return 0;
                }
                let at = values_off + offset as usize * 2;
                u16::from_be_bytes([data[at], data[at + 1]])
            }
            Inner::Format2 {
                data,
                range_count,
                ranges_off,
            } => {
                if range_count == 0 {
                    return 0;
                }
                let mut lo: u16 = 0;
                let mut hi: u16 = range_count;
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let off = ranges_off + mid as usize * 6;
                    let start = u16::from_be_bytes([data[off], data[off + 1]]);
                    let end = u16::from_be_bytes([data[off + 2], data[off + 3]]);
                    if glyph_id < start {
                        hi = mid;
                    } else if glyph_id > end {
                        lo = mid + 1;
                    } else {
                        return u16::from_be_bytes([data[off + 4], data[off + 5]]);
                    }
                }
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_format1(start: u16, values: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&(values.len() as u16).to_be_bytes());
        for v in values {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    fn build_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
        for (start, end, class) in ranges {
            out.extend_from_slice(&start.to_be_bytes());
            out.extend_from_slice(&end.to_be_bytes());
            out.extend_from_slice(&class.to_be_bytes());
        }
        out
    }

    #[test]
    fn format1_maps_glyphs_to_declared_classes() {
        // startGlyphID = 10; glyphs 10..=13 map to classes 1, 2, 1, 3.
        let bytes = build_format1(10, &[1, 2, 1, 3]);
        let cd = ClassDef::parse(&bytes).unwrap();
        assert_eq!(cd.class_of(10), 1);
        assert_eq!(cd.class_of(11), 2);
        assert_eq!(cd.class_of(12), 1);
        assert_eq!(cd.class_of(13), 3);
    }

    #[test]
    fn format1_returns_zero_for_glyphs_below_or_above_range() {
        let bytes = build_format1(10, &[1, 2, 3]);
        let cd = ClassDef::parse(&bytes).unwrap();
        assert_eq!(cd.class_of(0), 0);
        assert_eq!(cd.class_of(9), 0);
        assert_eq!(cd.class_of(13), 0);
        assert_eq!(cd.class_of(999), 0);
    }

    #[test]
    fn format2_maps_ranges_to_classes() {
        // Range 10..=19 -> class 1; range 100..=200 -> class 7.
        let bytes = build_format2(&[(10, 19, 1), (100, 200, 7)]);
        let cd = ClassDef::parse(&bytes).unwrap();
        assert_eq!(cd.class_of(10), 1);
        assert_eq!(cd.class_of(15), 1);
        assert_eq!(cd.class_of(19), 1);
        assert_eq!(cd.class_of(100), 7);
        assert_eq!(cd.class_of(200), 7);
    }

    #[test]
    fn format2_returns_zero_in_gaps() {
        let bytes = build_format2(&[(10, 19, 1), (100, 200, 7)]);
        let cd = ClassDef::parse(&bytes).unwrap();
        assert_eq!(cd.class_of(9), 0);
        assert_eq!(cd.class_of(20), 0);
        assert_eq!(cd.class_of(99), 0);
        assert_eq!(cd.class_of(201), 0);
    }

    #[test]
    fn empty_format2_is_all_class_zero() {
        let bytes = build_format2(&[]);
        let cd = ClassDef::parse(&bytes).unwrap();
        assert_eq!(cd.class_of(0), 0);
        assert_eq!(cd.class_of(1000), 0);
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = alloc::vec![0u8, 5]; // format = 5
        bytes.extend_from_slice(&0u16.to_be_bytes());
        assert!(matches!(
            ClassDef::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_truncated_format1_body() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes()); // startGlyphID
        bytes.extend_from_slice(&3u16.to_be_bytes()); // glyphCount=3
        bytes.extend_from_slice(&1u16.to_be_bytes()); // only one value
        assert!(matches!(
            ClassDef::parse(&bytes),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_truncated_format2_body() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&2u16.to_be_bytes()); // rangeCount=2
        bytes.extend_from_slice(&[0u8; 6]); // only one range
        assert!(matches!(
            ClassDef::parse(&bytes),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(ClassDef::parse(&[0]).is_err());
    }
}
