//! GPOS lookup type 6: Mark-to-Mark Attachment.
//!
//! Attaches one combining mark onto another. In the sequence
//! `a + ̂ + ̇` (circumflex then dot-above), the dot-above needs to
//! stack on top of the circumflex, not on the `a`, which is what
//! mark-to-base already handled. This lookup is what drives the
//! stacking.
//!
//! # Subtable layout
//!
//! Identical in shape to mark-to-base, with the "base" renamed to
//! "mark2":
//!
//! ```text
//!   u16      posFormat = 1
//!   Offset16 mark1CoverageOffset
//!   Offset16 mark2CoverageOffset
//!   u16      markClassCount
//!   Offset16 mark1ArrayOffset    (same as MarkArray in type 4)
//!   Offset16 mark2ArrayOffset    (same as BaseArray in type 4)
//! ```
//!
//! `Mark1Array`: one `(markClass, anchorOffset)` record per covered
//! mark1 glyph (the mark being attached).
//!
//! `Mark2Array`: for each covered mark2 glyph, an array of
//! `markClassCount` anchor offsets: the anchors on the *base* mark
//! at which an incoming mark of each class will land.
//!
//! # Attachment math
//!
//! Exactly the same as mark-to-base: the mark2 anchor pins where on
//! the lower mark the upper mark lands, and the mark1 anchor pins
//! where on the upper mark it's grabbed. Callers add the delta to
//! the upper mark's `(x_offset, y_offset)` and typically zero its
//! advance.

use crate::error::{Error, Result};
use crate::tables::gpos::anchor::Anchor;
use crate::tables::gpos::mark_base::MarkAttachment;
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed Mark-to-Mark Attachment subtable.
#[derive(Debug, Clone, Copy)]
pub struct MarkMarkPos<'a> {
    mark1_coverage: Coverage<'a>,
    mark2_coverage: Coverage<'a>,
    mark_class_count: u16,
    mark1_array: Mark1Array<'a>,
    mark2_array: Mark2Array<'a>,
}

impl<'a> MarkMarkPos<'a> {
    /// Parses a Mark-to-Mark subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported markMarkPos format",
            });
        }
        let mark1_cov_off = r.read_u16()? as usize;
        let mark2_cov_off = r.read_u16()? as usize;
        let mark_class_count = r.read_u16()?;
        let mark1_array_off = r.read_u16()? as usize;
        let mark2_array_off = r.read_u16()? as usize;

        let mark1_coverage =
            Coverage::parse(data.get(mark1_cov_off..).ok_or(Error::Malformed {
                offset: mark1_cov_off,
                context: "markMarkPos mark1Coverage offset past end",
            })?)?;
        let mark2_coverage =
            Coverage::parse(data.get(mark2_cov_off..).ok_or(Error::Malformed {
                offset: mark2_cov_off,
                context: "markMarkPos mark2Coverage offset past end",
            })?)?;
        let mark1_array = Mark1Array::parse(data, mark1_array_off)?;
        let mark2_array = Mark2Array::parse(data, mark2_array_off, mark_class_count)?;

        Ok(Self {
            mark1_coverage,
            mark2_coverage,
            mark_class_count,
            mark1_array,
            mark2_array,
        })
    }

    /// True when `glyph_id` is in the mark1 (attaching mark) coverage.
    #[must_use]
    pub fn covers_mark1(&self, glyph_id: u16) -> bool {
        self.mark1_coverage.contains(glyph_id)
    }

    /// Tries to attach the upper mark `mark1_gid` onto the lower mark
    /// `mark2_gid`. Returns the anchor pair when both are covered and
    /// `mark2` has an anchor for `mark1`'s class; `None` otherwise.
    #[must_use]
    pub fn attach(&self, mark1_gid: u16, mark2_gid: u16) -> Option<MarkAttachment> {
        let mark1_idx = self.mark1_coverage.index_of(mark1_gid)?;
        let mark2_idx = self.mark2_coverage.index_of(mark2_gid)?;
        let (mark_class, mark1_anchor) = self.mark1_array.record(mark1_idx)?;
        if mark_class >= self.mark_class_count {
            return None;
        }
        let mark2_anchor = self.mark2_array.anchor(mark2_idx, mark_class)?;
        Some(MarkAttachment {
            mark_anchor: mark1_anchor,
            base_anchor: mark2_anchor,
        })
    }
}

// --------------------------------------------------------------------------
// Mark1Array: records for the "upper" mark being attached.
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Mark1Array<'a> {
    data: &'a [u8],
    base: usize,
    records_off: usize,
    mark_count: u16,
}

impl<'a> Mark1Array<'a> {
    fn parse(data: &'a [u8], base: usize) -> Result<Self> {
        if base >= data.len() {
            return Err(Error::Malformed {
                offset: base,
                context: "mark1Array offset past end",
            });
        }
        let mut r = Reader::at(data, base)?;
        let mark_count = r.read_u16()?;
        let records_off = r.position();
        let need = records_off + mark_count as usize * 4;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: records_off,
                context: "mark1Array records shorter than markCount",
            });
        }
        Ok(Self {
            data,
            base,
            records_off,
            mark_count,
        })
    }

    fn record(&self, idx: u16) -> Option<(u16, Anchor)> {
        if idx >= self.mark_count {
            return None;
        }
        let at = self.records_off + idx as usize * 4;
        let mark_class = u16::from_be_bytes([self.data[at], self.data[at + 1]]);
        let anchor_off_rel = u16::from_be_bytes([self.data[at + 2], self.data[at + 3]]) as usize;
        let anchor_off = self.base + anchor_off_rel;
        let anchor = Anchor::parse_at(self.data, anchor_off).ok()?;
        Some((mark_class, anchor))
    }
}

// --------------------------------------------------------------------------
// Mark2Array: anchor rows for the "lower" mark onto which mark1 attaches.
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Mark2Array<'a> {
    data: &'a [u8],
    base: usize,
    records_off: usize,
    mark2_count: u16,
    mark_class_count: u16,
}

impl<'a> Mark2Array<'a> {
    fn parse(data: &'a [u8], base: usize, mark_class_count: u16) -> Result<Self> {
        if base >= data.len() {
            return Err(Error::Malformed {
                offset: base,
                context: "mark2Array offset past end",
            });
        }
        let mut r = Reader::at(data, base)?;
        let mark2_count = r.read_u16()?;
        let records_off = r.position();
        let stride = mark_class_count as usize * 2;
        let need = records_off + mark2_count as usize * stride;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: records_off,
                context: "mark2Array records shorter than mark2Count * markClassCount",
            });
        }
        Ok(Self {
            data,
            base,
            records_off,
            mark2_count,
            mark_class_count,
        })
    }

    fn anchor(&self, mark2_idx: u16, mark_class: u16) -> Option<Anchor> {
        if mark2_idx >= self.mark2_count || mark_class >= self.mark_class_count {
            return None;
        }
        let record_off = self.records_off
            + mark2_idx as usize * self.mark_class_count as usize * 2
            + mark_class as usize * 2;
        let anchor_off_rel =
            u16::from_be_bytes([self.data[record_off], self.data[record_off + 1]]) as usize;
        if anchor_off_rel == 0 {
            return None;
        }
        let anchor_off = self.base + anchor_off_rel;
        Anchor::parse_at(self.data, anchor_off).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_coverage_format1(glyphs: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
        for g in glyphs {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    fn build_anchor(x: i16, y: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&x.to_be_bytes());
        out.extend_from_slice(&y.to_be_bytes());
        out
    }

    #[allow(clippy::type_complexity)]
    fn build_mark_mark_pos(
        mark1_glyphs: &[u16],
        mark2_glyphs: &[u16],
        mark_class_count: u16,
        mark1s: &[(u16, (i16, i16))],
        mark2s: &[Vec<Option<(i16, i16)>>],
    ) -> Vec<u8> {
        assert_eq!(mark1_glyphs.len(), mark1s.len());
        assert_eq!(mark2_glyphs.len(), mark2s.len());

        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        let mark1_cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let mark2_cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&mark_class_count.to_be_bytes());
        let mark1_array_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let mark2_array_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());

        let mark1_array_start = out.len();
        out.extend_from_slice(&(mark1s.len() as u16).to_be_bytes());
        let mark1_records_start = out.len();
        for _ in 0..mark1s.len() {
            out.extend_from_slice(&[0u8; 4]);
        }
        for (i, (mark_class, (x, y))) in mark1s.iter().enumerate() {
            let anchor_start = out.len();
            out.extend_from_slice(&build_anchor(*x, *y));
            let rel = (anchor_start - mark1_array_start) as u16;
            let rec = mark1_records_start + i * 4;
            out[rec..rec + 2].copy_from_slice(&mark_class.to_be_bytes());
            out[rec + 2..rec + 4].copy_from_slice(&rel.to_be_bytes());
        }

        let mark2_array_start = out.len();
        out.extend_from_slice(&(mark2s.len() as u16).to_be_bytes());
        let mark2_records_start = out.len();
        for _ in 0..mark2s.len() {
            for _ in 0..mark_class_count {
                out.extend_from_slice(&[0u8; 2]);
            }
        }
        for (i, row) in mark2s.iter().enumerate() {
            for (c, slot) in row.iter().enumerate() {
                if let Some((x, y)) = slot {
                    let anchor_start = out.len();
                    out.extend_from_slice(&build_anchor(*x, *y));
                    let rel = (anchor_start - mark2_array_start) as u16;
                    let at = mark2_records_start + i * (mark_class_count as usize) * 2 + c * 2;
                    out[at..at + 2].copy_from_slice(&rel.to_be_bytes());
                }
            }
        }

        let mark1_cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(mark1_glyphs));
        let mark2_cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(mark2_glyphs));

        out[mark1_cov_slot..mark1_cov_slot + 2]
            .copy_from_slice(&(mark1_cov_start as u16).to_be_bytes());
        out[mark2_cov_slot..mark2_cov_slot + 2]
            .copy_from_slice(&(mark2_cov_start as u16).to_be_bytes());
        out[mark1_array_slot..mark1_array_slot + 2]
            .copy_from_slice(&(mark1_array_start as u16).to_be_bytes());
        out[mark2_array_slot..mark2_array_slot + 2]
            .copy_from_slice(&(mark2_array_start as u16).to_be_bytes());

        out
    }

    #[test]
    fn stacks_upper_mark_onto_lower_mark() {
        let bytes = build_mark_mark_pos(
            &[30],
            &[20],
            1,
            &[(0, (5, 0))],
            &[alloc::vec![Some((5, 800))]],
        );
        let mmp = MarkMarkPos::parse(&bytes).unwrap();
        let attach = mmp.attach(30, 20).unwrap();
        assert!(mmp.covers_mark1(30) && !mmp.covers_mark1(20));
        assert_eq!((attach.mark_anchor.x, attach.mark_anchor.y), (5, 0));
        assert_eq!((attach.base_anchor.x, attach.base_anchor.y), (5, 800));
    }

    #[test]
    fn returns_none_when_upper_mark_not_covered() {
        let bytes = build_mark_mark_pos(
            &[30],
            &[20],
            1,
            &[(0, (0, 0))],
            &[alloc::vec![Some((0, 0))]],
        );
        let mmp = MarkMarkPos::parse(&bytes).unwrap();
        assert!(mmp.attach(31, 20).is_none());
    }

    #[test]
    fn returns_none_when_mark2_has_null_anchor_for_class() {
        let bytes = build_mark_mark_pos(
            &[30, 31],
            &[20],
            2,
            &[(0, (5, 0)), (1, (7, 0))],
            &[alloc::vec![Some((5, 800)), None]],
        );
        let mmp = MarkMarkPos::parse(&bytes).unwrap();
        assert!(mmp.attach(30, 20).is_some());
        assert!(mmp.attach(31, 20).is_none());
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 10]);
        assert!(matches!(
            MarkMarkPos::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }
}
