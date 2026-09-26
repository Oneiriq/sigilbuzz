//! GPOS lookup type 4: Mark-to-Base Attachment.
//!
//! Attaches a combining mark (grave, acute, tilde, diaeresis, ...)
//! to the preceding base glyph. The feature that drives this at
//! shaping time is `mark`, and every serious Latin font ships it;
//! the difference between "café" rendered right and rendered wrong
//! is whether this lookup fires.
//!
//! # Subtable layout
//!
//! ```text
//!   u16      posFormat = 1
//!   Offset16 markCoverageOffset
//!   Offset16 baseCoverageOffset
//!   u16      markClassCount
//!   Offset16 markArrayOffset
//!   Offset16 baseArrayOffset
//! ```
//!
//! `MarkArray` (at `markArrayOffset`):
//!
//! ```text
//!   u16 markCount
//!   MarkRecord records[markCount]:
//!     u16       markClass
//!     Offset16  markAnchorOffset   (relative to MarkArray)
//! ```
//!
//! `BaseArray` (at `baseArrayOffset`):
//!
//! ```text
//!   u16 baseCount
//!   BaseRecord records[baseCount]:
//!     Offset16 baseAnchorOffsets[markClassCount]  (relative to
//!                                                  BaseArray; 0 =
//!                                                  no anchor for
//!                                                  that class)
//! ```
//!
//! # Attachment math
//!
//! Given a base glyph `B` at pen position `P` with advance `a`,
//! and a mark glyph `M` immediately after:
//!
//! ```text
//!   base_anchor_world_pos = P + base_anchor.(x,y)
//!   mark_natural_world_pos = P + a + mark_anchor.(x,y)
//!   delta = base_anchor_world_pos - mark_natural_world_pos
//!         = (base_anchor.x - mark_anchor.x - a, base_anchor.y - mark_anchor.y)
//! ```
//!
//! That is the left-to-right picture. The shaper follows HarfBuzz:
//! it stores `base_anchor - mark_anchor` on the mark, links the mark
//! to its base, and only after all positioning resolves the link,
//! subtracting the advances from the base up to the mark for forward
//! runs, or adding the advances after the base through the mark for
//! backward (RTL, BTT) runs, which are reversed afterwards. Mark
//! advances are zeroed by the shaper's mark-width pass, not here.
//! Format 3 anchors can carry variation deltas; see
//! [`Anchor::resolve`].

use crate::error::{Error, Result};
use crate::tables::gpos::anchor::Anchor;
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed Mark-to-Base Attachment subtable.
#[derive(Debug, Clone, Copy)]
pub struct MarkBasePos<'a> {
    mark_coverage: Coverage<'a>,
    base_coverage: Coverage<'a>,
    mark_class_count: u16,
    mark_array: MarkArray<'a>,
    base_array: BaseArray<'a>,
}

impl<'a> MarkBasePos<'a> {
    /// Parses a Mark-to-Base subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported markBasePos format",
            });
        }
        let mark_cov_off = r.read_u16()? as usize;
        let base_cov_off = r.read_u16()? as usize;
        let mark_class_count = r.read_u16()?;
        let mark_array_off = r.read_u16()? as usize;
        let base_array_off = r.read_u16()? as usize;

        let mark_coverage = Coverage::parse(data.get(mark_cov_off..).ok_or(Error::Malformed {
            offset: mark_cov_off,
            context: "markBasePos markCoverage offset past end",
        })?)?;
        let base_coverage = Coverage::parse(data.get(base_cov_off..).ok_or(Error::Malformed {
            offset: base_cov_off,
            context: "markBasePos baseCoverage offset past end",
        })?)?;
        let mark_array = MarkArray::parse(data, mark_array_off)?;
        let base_array = BaseArray::parse(data, base_array_off, mark_class_count)?;

        Ok(Self {
            mark_coverage,
            base_coverage,
            mark_class_count,
            mark_array,
            base_array,
        })
    }

    /// Tries to attach `mark_gid` onto `base_gid`. Returns the pair
    /// of anchors when both glyphs are covered and the base has an
    /// anchor for the mark's class; `None` otherwise (including
    /// when either coverage fails or the relevant base anchor is
    /// null).
    #[must_use]
    pub fn attach(&self, mark_gid: u16, base_gid: u16) -> Option<MarkAttachment> {
        let mark_idx = self.mark_coverage.index_of(mark_gid)?;
        let base_idx = self.base_coverage.index_of(base_gid)?;
        let (mark_class, mark_anchor) = self.mark_array.record(mark_idx)?;
        if mark_class >= self.mark_class_count {
            return None;
        }
        let base_anchor = self.base_array.anchor(base_idx, mark_class)?;
        Some(MarkAttachment {
            mark_anchor,
            base_anchor,
        })
    }
}

/// Anchor pair produced by a successful mark-to-base lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkAttachment {
    /// Anchor on the mark glyph (in mark-local design units).
    pub mark_anchor: Anchor,
    /// Anchor on the base glyph (in base-local design units).
    pub base_anchor: Anchor,
}

// --------------------------------------------------------------------------
// MarkArray
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct MarkArray<'a> {
    data: &'a [u8],
    base: usize,
    records_off: usize,
    mark_count: u16,
}

impl<'a> MarkArray<'a> {
    fn parse(data: &'a [u8], base: usize) -> Result<Self> {
        if base >= data.len() {
            return Err(Error::Malformed {
                offset: base,
                context: "markArray offset past end",
            });
        }
        let mut r = Reader::at(data, base)?;
        let mark_count = r.read_u16()?;
        let records_off = r.position();
        let need = records_off + mark_count as usize * 4;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: records_off,
                context: "markArray records shorter than markCount",
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
// BaseArray
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct BaseArray<'a> {
    data: &'a [u8],
    base: usize,
    records_off: usize,
    base_count: u16,
    mark_class_count: u16,
}

impl<'a> BaseArray<'a> {
    fn parse(data: &'a [u8], base: usize, mark_class_count: u16) -> Result<Self> {
        if base >= data.len() {
            return Err(Error::Malformed {
                offset: base,
                context: "baseArray offset past end",
            });
        }
        let mut r = Reader::at(data, base)?;
        let base_count = r.read_u16()?;
        let records_off = r.position();
        let stride = mark_class_count as usize * 2;
        let need = records_off + base_count as usize * stride;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: records_off,
                context: "baseArray records shorter than baseCount * markClassCount",
            });
        }
        Ok(Self {
            data,
            base,
            records_off,
            base_count,
            mark_class_count,
        })
    }

    fn anchor(&self, base_idx: u16, mark_class: u16) -> Option<Anchor> {
        if base_idx >= self.base_count || mark_class >= self.mark_class_count {
            return None;
        }
        let record_off = self.records_off
            + base_idx as usize * self.mark_class_count as usize * 2
            + mark_class as usize * 2;
        let anchor_off_rel =
            u16::from_be_bytes([self.data[record_off], self.data[record_off + 1]]) as usize;
        if anchor_off_rel == 0 {
            // Spec: a null offset means this base has no anchor for
            // this mark class. Not an error: the mark simply does
            // not attach through this base.
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
        out.extend_from_slice(&1u16.to_be_bytes()); // format 1
        out.extend_from_slice(&x.to_be_bytes());
        out.extend_from_slice(&y.to_be_bytes());
        out
    }

    /// Assembles a MarkBasePos format-1 subtable from a Rust-level
    /// description. Each mark has `(mark_class, (x, y))`. Each base
    /// has a slice of `(x, y)` pairs, one entry per mark class; a
    /// `None` position slot stands in for a null (absent) anchor.
    #[allow(clippy::type_complexity)]
    fn build_mark_base_pos(
        mark_glyphs: &[u16],
        base_glyphs: &[u16],
        mark_class_count: u16,
        marks: &[(u16, (i16, i16))],
        bases: &[Vec<Option<(i16, i16)>>],
    ) -> Vec<u8> {
        assert_eq!(mark_glyphs.len(), marks.len());
        assert_eq!(base_glyphs.len(), bases.len());

        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
        let mark_cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let base_cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&mark_class_count.to_be_bytes());
        let mark_array_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let base_array_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());

        // MarkArray: header + records + anchors.
        let mark_array_start = out.len();
        out.extend_from_slice(&(marks.len() as u16).to_be_bytes()); // markCount
        let mark_records_start = out.len();
        for _ in 0..marks.len() {
            out.extend_from_slice(&[0u8; 4]); // markClass + anchorOffset placeholder
        }
        for (i, (mark_class, (x, y))) in marks.iter().enumerate() {
            let anchor_start = out.len();
            out.extend_from_slice(&build_anchor(*x, *y));
            let rel = (anchor_start - mark_array_start) as u16;
            let rec = mark_records_start + i * 4;
            out[rec..rec + 2].copy_from_slice(&mark_class.to_be_bytes());
            out[rec + 2..rec + 4].copy_from_slice(&rel.to_be_bytes());
        }

        // BaseArray: header + records (mark_class_count offsets
        // each) + anchors.
        let base_array_start = out.len();
        out.extend_from_slice(&(bases.len() as u16).to_be_bytes()); // baseCount
        let base_records_start = out.len();
        for _ in 0..bases.len() {
            for _ in 0..mark_class_count {
                out.extend_from_slice(&[0u8; 2]);
            }
        }
        for (i, base_row) in bases.iter().enumerate() {
            for (c, slot) in base_row.iter().enumerate() {
                if let Some((x, y)) = slot {
                    let anchor_start = out.len();
                    out.extend_from_slice(&build_anchor(*x, *y));
                    let rel = (anchor_start - base_array_start) as u16;
                    let at = base_records_start + i * (mark_class_count as usize) * 2 + c * 2;
                    out[at..at + 2].copy_from_slice(&rel.to_be_bytes());
                }
                // else: leave zero, meaning null anchor.
            }
        }

        // Coverages appended last; patch offsets.
        let mark_cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(mark_glyphs));
        let base_cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(base_glyphs));

        out[mark_cov_slot..mark_cov_slot + 2]
            .copy_from_slice(&(mark_cov_start as u16).to_be_bytes());
        out[base_cov_slot..base_cov_slot + 2]
            .copy_from_slice(&(base_cov_start as u16).to_be_bytes());
        out[mark_array_slot..mark_array_slot + 2]
            .copy_from_slice(&(mark_array_start as u16).to_be_bytes());
        out[base_array_slot..base_array_slot + 2]
            .copy_from_slice(&(base_array_start as u16).to_be_bytes());

        out
    }

    #[test]
    fn attaches_mark_to_base_via_class_anchors() {
        // One mark class. Base 'e' (glyph 5) has anchor at (250, 500),
        // mark 'acute' (glyph 20) has anchor at (10, 0). attach()
        // yields both anchors; the caller computes the mark offset.
        let bytes = build_mark_base_pos(
            &[20],                            // mark glyphs
            &[5],                             // base glyphs
            1,                                // mark class count
            &[(0, (10, 0))],                  // mark records
            &[alloc::vec![Some((250, 500))]], // base rows
        );
        let mbp = MarkBasePos::parse(&bytes).unwrap();
        let attach = mbp.attach(20, 5).unwrap();
        assert_eq!((attach.mark_anchor.x, attach.mark_anchor.y), (10, 0));
        assert_eq!((attach.base_anchor.x, attach.base_anchor.y), (250, 500));
    }

    #[test]
    fn returns_none_when_mark_not_covered() {
        let bytes =
            build_mark_base_pos(&[20], &[5], 1, &[(0, (0, 0))], &[alloc::vec![Some((0, 0))]]);
        let mbp = MarkBasePos::parse(&bytes).unwrap();
        assert!(mbp.attach(21, 5).is_none());
    }

    #[test]
    fn returns_none_when_base_has_null_anchor_for_class() {
        // One base, two mark classes, but the base only has an
        // anchor for class 0. A mark of class 1 cannot attach.
        let bytes = build_mark_base_pos(
            &[20, 21],
            &[5],
            2,
            &[(0, (10, 0)), (1, (12, 0))],
            &[alloc::vec![Some((250, 500)), None]],
        );
        let mbp = MarkBasePos::parse(&bytes).unwrap();
        assert!(mbp.attach(20, 5).is_some()); // class-0 mark hits
        assert!(mbp.attach(21, 5).is_none()); // class-1 mark misses
    }

    #[test]
    fn separate_class_anchors_select_the_right_one() {
        // Base has distinct anchors for two mark classes.
        let bytes = build_mark_base_pos(
            &[20, 21],
            &[5],
            2,
            &[(0, (10, 0)), (1, (30, 0))],
            &[alloc::vec![Some((250, 500)), Some((260, 600))]],
        );
        let mbp = MarkBasePos::parse(&bytes).unwrap();
        let a0 = mbp.attach(20, 5).unwrap();
        let a1 = mbp.attach(21, 5).unwrap();
        assert_eq!(a0.base_anchor.y, 500);
        assert_eq!(a1.base_anchor.y, 600);
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 10]);
        assert!(matches!(
            MarkBasePos::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }
}
