//! GPOS lookup type 5: Mark-to-Ligature Attachment.
//!
//! Attaches a combining mark onto a specific *component* of a
//! preceding ligature glyph. A Latin `ﬁ` ligature with an acute
//! accent should pin the accent above the `i` component, not above
//! the overall glyph box, so the subtable stores one anchor row
//! per component.
//!
//! # Subtable layout
//!
//! ```text
//!   u16      posFormat = 1
//!   Offset16 markCoverageOffset
//!   Offset16 ligatureCoverageOffset
//!   u16      markClassCount
//!   Offset16 markArrayOffset        (same as in type 4)
//!   Offset16 ligatureArrayOffset
//! ```
//!
//! `LigatureArray` (at `ligatureArrayOffset`):
//!
//! ```text
//!   u16       ligatureCount
//!   Offset16  ligatureAttachOffsets[ligatureCount]  (relative to
//!                                                    LigatureArray)
//! ```
//!
//! Each `LigatureAttach` (at `ligatureAttachOffset`):
//!
//! ```text
//!   u16 componentCount
//!   ComponentRecord records[componentCount]:
//!     Offset16 ligatureAnchorOffsets[markClassCount]  (relative to
//!                                                      LigatureAttach;
//!                                                      0 = no anchor)
//! ```
//!
//! # Attachment math
//!
//! Same as mark-to-base. Callers pick which component the mark binds
//! to (usually the component that owns the mark's source codepoint);
//! the anchor pair is then applied exactly as in type 4.

use crate::error::{Error, Result};
use crate::tables::gpos::anchor::Anchor;
use crate::tables::gpos::mark_base::MarkAttachment;
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed Mark-to-Ligature Attachment subtable.
#[derive(Debug, Clone, Copy)]
pub struct MarkLigaPos<'a> {
    mark_coverage: Coverage<'a>,
    liga_coverage: Coverage<'a>,
    mark_class_count: u16,
    mark_array: MarkArray<'a>,
    ligature_array: LigatureArray<'a>,
}

impl<'a> MarkLigaPos<'a> {
    /// Parses a Mark-to-Ligature subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported markLigaPos format",
            });
        }
        let mark_cov_off = r.read_u16()? as usize;
        let liga_cov_off = r.read_u16()? as usize;
        let mark_class_count = r.read_u16()?;
        let mark_array_off = r.read_u16()? as usize;
        let liga_array_off = r.read_u16()? as usize;

        let mark_coverage = Coverage::parse(data.get(mark_cov_off..).ok_or(Error::Malformed {
            offset: mark_cov_off,
            context: "markLigaPos markCoverage offset past end",
        })?)?;
        let liga_coverage = Coverage::parse(data.get(liga_cov_off..).ok_or(Error::Malformed {
            offset: liga_cov_off,
            context: "markLigaPos ligatureCoverage offset past end",
        })?)?;
        let mark_array = MarkArray::parse(data, mark_array_off)?;
        let ligature_array = LigatureArray::parse(data, liga_array_off, mark_class_count)?;

        Ok(Self {
            mark_coverage,
            liga_coverage,
            mark_class_count,
            mark_array,
            ligature_array,
        })
    }

    /// Tries to attach `mark_gid` onto the given `component_index`
    /// of ligature glyph `liga_gid`. Returns the anchor pair when
    /// both glyphs are covered, the component exists, and the
    /// component has an anchor for the mark's class; `None`
    /// otherwise.
    #[must_use]
    pub fn attach(
        &self,
        mark_gid: u16,
        liga_gid: u16,
        component_index: u16,
    ) -> Option<MarkAttachment> {
        let mark_idx = self.mark_coverage.index_of(mark_gid)?;
        let liga_idx = self.liga_coverage.index_of(liga_gid)?;
        let (mark_class, mark_anchor) = self.mark_array.record(mark_idx)?;
        if mark_class >= self.mark_class_count {
            return None;
        }
        let liga_anchor = self
            .ligature_array
            .anchor(liga_idx, component_index, mark_class)?;
        Some(MarkAttachment {
            mark_anchor,
            base_anchor: liga_anchor,
        })
    }
}

// --------------------------------------------------------------------------
// MarkArray: identical in wire format to the one in mark_base.
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
// LigatureArray / LigatureAttach: per-component anchor rows.
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct LigatureArray<'a> {
    data: &'a [u8],
    base: usize,
    attach_offsets_off: usize,
    ligature_count: u16,
    mark_class_count: u16,
}

impl<'a> LigatureArray<'a> {
    fn parse(data: &'a [u8], base: usize, mark_class_count: u16) -> Result<Self> {
        if base >= data.len() {
            return Err(Error::Malformed {
                offset: base,
                context: "ligatureArray offset past end",
            });
        }
        let mut r = Reader::at(data, base)?;
        let ligature_count = r.read_u16()?;
        let attach_offsets_off = r.position();
        let need = attach_offsets_off + ligature_count as usize * 2;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: attach_offsets_off,
                context: "ligatureArray attach offsets shorter than ligatureCount",
            });
        }
        Ok(Self {
            data,
            base,
            attach_offsets_off,
            ligature_count,
            mark_class_count,
        })
    }

    fn anchor(&self, liga_idx: u16, component_index: u16, mark_class: u16) -> Option<Anchor> {
        if liga_idx >= self.ligature_count || mark_class >= self.mark_class_count {
            return None;
        }
        let off_slot = self.attach_offsets_off + liga_idx as usize * 2;
        let attach_off_rel =
            u16::from_be_bytes([self.data[off_slot], self.data[off_slot + 1]]) as usize;
        let attach_base = self.base + attach_off_rel;
        if attach_base >= self.data.len() {
            return None;
        }
        let mut r = Reader::at(self.data, attach_base).ok()?;
        let component_count = r.read_u16().ok()?;
        if component_index >= component_count {
            return None;
        }
        let records_off = r.position();
        let stride = self.mark_class_count as usize * 2;
        let need = records_off + component_count as usize * stride;
        if self.data.len() < need {
            return None;
        }
        let anchor_slot = records_off
            + component_index as usize * self.mark_class_count as usize * 2
            + mark_class as usize * 2;
        let anchor_off_rel =
            u16::from_be_bytes([self.data[anchor_slot], self.data[anchor_slot + 1]]) as usize;
        if anchor_off_rel == 0 {
            return None;
        }
        let anchor_off = attach_base + anchor_off_rel;
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

    /// Builds a MarkLigaPos subtable. `ligatures` is one entry per
    /// ligature glyph, each a list of components, each component a
    /// slice of `markClassCount` anchor slots (None = null anchor).
    #[allow(clippy::type_complexity)]
    fn build_mark_liga_pos(
        mark_glyphs: &[u16],
        liga_glyphs: &[u16],
        mark_class_count: u16,
        marks: &[(u16, (i16, i16))],
        ligatures: &[Vec<Vec<Option<(i16, i16)>>>],
    ) -> Vec<u8> {
        assert_eq!(mark_glyphs.len(), marks.len());
        assert_eq!(liga_glyphs.len(), ligatures.len());

        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        let mark_cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let liga_cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&mark_class_count.to_be_bytes());
        let mark_array_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let liga_array_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());

        // MarkArray.
        let mark_array_start = out.len();
        out.extend_from_slice(&(marks.len() as u16).to_be_bytes());
        let mark_records_start = out.len();
        for _ in 0..marks.len() {
            out.extend_from_slice(&[0u8; 4]);
        }
        for (i, (mark_class, (x, y))) in marks.iter().enumerate() {
            let anchor_start = out.len();
            out.extend_from_slice(&build_anchor(*x, *y));
            let rel = (anchor_start - mark_array_start) as u16;
            let rec = mark_records_start + i * 4;
            out[rec..rec + 2].copy_from_slice(&mark_class.to_be_bytes());
            out[rec + 2..rec + 4].copy_from_slice(&rel.to_be_bytes());
        }

        // LigatureArray: header + attach-offset slots.
        let liga_array_start = out.len();
        out.extend_from_slice(&(ligatures.len() as u16).to_be_bytes());
        let attach_slots_start = out.len();
        for _ in 0..ligatures.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        // One LigatureAttach per ligature.
        for (i, components) in ligatures.iter().enumerate() {
            let attach_start = out.len();
            out.extend_from_slice(&(components.len() as u16).to_be_bytes());
            let comp_records_start = out.len();
            for _ in 0..components.len() {
                for _ in 0..mark_class_count {
                    out.extend_from_slice(&[0u8; 2]);
                }
            }
            for (c_i, comp) in components.iter().enumerate() {
                for (cls, slot) in comp.iter().enumerate() {
                    if let Some((x, y)) = slot {
                        let anchor_start = out.len();
                        out.extend_from_slice(&build_anchor(*x, *y));
                        let rel = (anchor_start - attach_start) as u16;
                        let at =
                            comp_records_start + c_i * (mark_class_count as usize) * 2 + cls * 2;
                        out[at..at + 2].copy_from_slice(&rel.to_be_bytes());
                    }
                }
            }
            let rel = (attach_start - liga_array_start) as u16;
            let slot = attach_slots_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
        }

        // Coverages appended last; patch offsets.
        let mark_cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(mark_glyphs));
        let liga_cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(liga_glyphs));

        out[mark_cov_slot..mark_cov_slot + 2]
            .copy_from_slice(&(mark_cov_start as u16).to_be_bytes());
        out[liga_cov_slot..liga_cov_slot + 2]
            .copy_from_slice(&(liga_cov_start as u16).to_be_bytes());
        out[mark_array_slot..mark_array_slot + 2]
            .copy_from_slice(&(mark_array_start as u16).to_be_bytes());
        out[liga_array_slot..liga_array_slot + 2]
            .copy_from_slice(&(liga_array_start as u16).to_be_bytes());

        out
    }

    #[test]
    fn attaches_mark_to_chosen_component() {
        // One ligature glyph with two components; class 0 mark
        // attaches at (100, 600) on component 0 and (400, 600) on
        // component 1.
        let bytes = build_mark_liga_pos(
            &[30],
            &[50],
            1,
            &[(0, (5, 0))],
            &alloc::vec![alloc::vec![
                alloc::vec![Some((100, 600))],
                alloc::vec![Some((400, 600))],
            ]],
        );
        let mlp = MarkLigaPos::parse(&bytes).unwrap();
        let a0 = mlp.attach(30, 50, 0).unwrap();
        let a1 = mlp.attach(30, 50, 1).unwrap();
        assert_eq!(a0.base_anchor, Anchor { x: 100, y: 600 });
        assert_eq!(a1.base_anchor, Anchor { x: 400, y: 600 });
    }

    #[test]
    fn null_anchor_for_component_returns_none() {
        let bytes = build_mark_liga_pos(
            &[30],
            &[50],
            1,
            &[(0, (0, 0))],
            &alloc::vec![alloc::vec![
                alloc::vec![Some((100, 600))],
                alloc::vec![None]
            ]],
        );
        let mlp = MarkLigaPos::parse(&bytes).unwrap();
        assert!(mlp.attach(30, 50, 0).is_some());
        assert!(mlp.attach(30, 50, 1).is_none());
    }

    #[test]
    fn component_index_past_end_returns_none() {
        let bytes = build_mark_liga_pos(
            &[30],
            &[50],
            1,
            &[(0, (0, 0))],
            &alloc::vec![alloc::vec![alloc::vec![Some((100, 600))]]],
        );
        let mlp = MarkLigaPos::parse(&bytes).unwrap();
        assert!(mlp.attach(30, 50, 5).is_none());
    }

    #[test]
    fn uncovered_mark_or_ligature_returns_none() {
        let bytes = build_mark_liga_pos(
            &[30],
            &[50],
            1,
            &[(0, (0, 0))],
            &alloc::vec![alloc::vec![alloc::vec![Some((100, 600))]]],
        );
        let mlp = MarkLigaPos::parse(&bytes).unwrap();
        assert!(mlp.attach(99, 50, 0).is_none());
        assert!(mlp.attach(30, 99, 0).is_none());
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 10]);
        assert!(matches!(
            MarkLigaPos::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }
}
