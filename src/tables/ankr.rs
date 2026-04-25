//! `ankr` — Apple Anchor Point table.
//!
//! AAT companion to `kerx` format 4 action type 1 (anchor-point
//! kerning). `kerx` carries a state machine whose action records hold
//! `(mark_anchor_index, current_anchor_index)` pairs; `ankr` resolves
//! those indices to concrete `(x, y)` design-unit coordinates so the
//! shaper can apply `mark - current` as the offset that aligns the
//! two glyphs.
//!
//! # Layout
//!
//! ```text
//!   u16 version        (0)
//!   u16 flags          (reserved — sigilbuzz ignores)
//!   u32 lookupTableOff (from start of ankr; AAT lookup: gid → byte
//!                       offset into the anchor-points block)
//!   u32 anchorPointsOff (from start of ankr; the per-glyph anchor
//!                        records live here)
//!
//!   AnchorPoints record (one per glyph that has anchors):
//!     u32 nPoints
//!     i16 x[nPoints]
//!     i16 y[nPoints]   — interleaved as (x0, y0, x1, y1, …)
//! ```
//!
//! The lookup table maps a glyph id to a *byte offset* into the
//! anchor-points block (relative to `anchorPointsOff`). A glyph that
//! falls outside any segment / record yields the AAT
//! `CLASS_OUT_OF_BOUNDS` sentinel and resolves to "no anchor" — same
//! conservative posture sigilbuzz uses for a missing kern rule.
//!
//! Spec:
//! <https://developer.apple.com/fonts/TrueType-Reference-Manual/RM06/Chap6ankr.html>
//!
//! Note Apple's lookup-value semantics here: while `kerx` format-2
//! pre-multiplies its lookup values to yield byte offsets directly,
//! `ankr`'s lookup yields a u16 that's *already a raw byte offset*
//! into the anchor-points block. That's a quirk of the spec — we
//! mirror it without scaling.

use crate::error::{Error, Result};
use crate::tables::layout::state_table::{lookup_class, CLASS_OUT_OF_BOUNDS};

/// A parsed `ankr` table view. Borrows the underlying bytes; lookups
/// slice into them on demand without allocation.
#[derive(Debug, Clone, Copy)]
pub struct Ankr<'a> {
    /// Full table bytes — every recorded offset is relative to byte 0.
    data: &'a [u8],
    /// Slice that starts at the lookup table's origin.
    lookup: &'a [u8],
    /// Slice that starts at the anchor-points block's origin.
    anchor_block: &'a [u8],
}

impl<'a> Ankr<'a> {
    /// Parses an `ankr` table. Returns `Err` for unsupported versions
    /// or out-of-range offsets so callers can fall back gracefully.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < 12 {
            return Err(Error::Truncated {
                offset: 0,
                context: "ankr header",
            });
        }
        let version = u16::from_be_bytes([data[0], data[1]]);
        if version != 0 {
            return Err(Error::Unsupported {
                context: "ankr version != 0",
            });
        }
        // bytes 2..4 = flags (reserved).
        let lookup_off = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let anchor_off = u32::from_be_bytes([data[8], data[9], data[10], data[11]]) as usize;
        if lookup_off >= data.len() || anchor_off >= data.len() {
            return Err(Error::Malformed {
                offset: lookup_off.max(anchor_off),
                context: "ankr offsets outside table",
            });
        }
        Ok(Self {
            data,
            lookup: &data[lookup_off..],
            anchor_block: &data[anchor_off..],
        })
    }

    /// Resolves the `(x, y)` anchor at `anchor_index` for `gid`.
    ///
    /// Returns `None` when:
    /// - the lookup table has no entry for `gid`,
    /// - the resolved anchor-block offset is out of range,
    /// - `anchor_index` is past the per-glyph anchor count, or
    /// - any of the byte slices are truncated.
    ///
    /// A `None` is the conservative "drop the kern silently" signal
    /// for `kerx` format 4 type 1; the caller should not error.
    #[must_use]
    pub fn anchor_for(&self, gid: u16, anchor_index: u16) -> Option<(i16, i16)> {
        // AAT lookup yields either a u16 byte offset into the anchor-
        // points block or `CLASS_OUT_OF_BOUNDS` (= the AAT sentinel
        // class id 1) when the glyph isn't covered. The sentinel
        // numerically falls inside any non-trivial anchor block, so we
        // must explicitly translate it to "no anchor" before the
        // offset arithmetic.
        let raw = lookup_class(self.lookup, gid, 0).ok()?;
        if raw == CLASS_OUT_OF_BOUNDS {
            return None;
        }
        let block_off = usize::from(raw);
        let block = self.anchor_block.get(block_off..)?;
        if block.len() < 4 {
            return None;
        }
        let n_points = u32::from_be_bytes([block[0], block[1], block[2], block[3]]);
        if u32::from(anchor_index) >= n_points {
            return None;
        }
        let rec_off = 4 + (anchor_index as usize) * 4;
        let rec = block.get(rec_off..rec_off + 4)?;
        let x = i16::from_be_bytes([rec[0], rec[1]]);
        let y = i16::from_be_bytes([rec[2], rec[3]]);
        Some((x, y))
    }

    /// Raw table bytes — exposed for tests / debug tooling.
    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Builds a minimal `ankr` blob with one anchor record at lookup
    /// offset `0`. Glyphs that resolve through an AAT format-6 lookup
    /// to `0` get the supplied `(x, y)` pairs.
    fn build_ankr(pairs: &[(u16, u16)], anchor_records: &[Vec<(i16, i16)>]) -> Vec<u8> {
        // Header is 12 bytes. Lookup table follows; anchor block after.
        let header_len = 12;
        // Format 6 lookup body: 12 + 4 * nUnits.
        let lookup_units: Vec<u8> = {
            let mut v = Vec::new();
            v.extend_from_slice(&6u16.to_be_bytes()); // format
            v.extend_from_slice(&4u16.to_be_bytes()); // unitSize
            v.extend_from_slice(&(pairs.len() as u16).to_be_bytes()); // nUnits
            v.extend_from_slice(&0u16.to_be_bytes()); // searchRange
            v.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
            v.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
            for &(g, val) in pairs {
                v.extend_from_slice(&g.to_be_bytes());
                v.extend_from_slice(&val.to_be_bytes());
            }
            v
        };
        let lookup_off = header_len;
        let mut anchor_off = lookup_off + lookup_units.len();
        // Pad to 4-byte alignment for the anchor block (matches Apple
        // tooling and keeps tests readable).
        while anchor_off % 4 != 0 {
            anchor_off += 1;
        }

        let mut anchor_bytes: Vec<u8> = Vec::new();
        for rec in anchor_records {
            anchor_bytes.extend_from_slice(&(rec.len() as u32).to_be_bytes());
            for &(x, y) in rec {
                anchor_bytes.extend_from_slice(&x.to_be_bytes());
                anchor_bytes.extend_from_slice(&y.to_be_bytes());
            }
        }

        let mut out = Vec::new();
        out.extend_from_slice(&0u16.to_be_bytes()); // version
        out.extend_from_slice(&0u16.to_be_bytes()); // flags
        out.extend_from_slice(&(lookup_off as u32).to_be_bytes());
        out.extend_from_slice(&(anchor_off as u32).to_be_bytes());
        out.extend_from_slice(&lookup_units);
        while out.len() < anchor_off {
            out.push(0);
        }
        out.extend_from_slice(&anchor_bytes);
        out
    }

    #[test]
    fn rejects_version_other_than_zero() {
        let mut bytes = vec![0u8; 12];
        bytes[0..2].copy_from_slice(&1u16.to_be_bytes()); // version = 1
        assert!(Ankr::parse(&bytes).is_err());
    }

    #[test]
    fn rejects_truncated_header() {
        let bytes = vec![0u8; 8];
        assert!(Ankr::parse(&bytes).is_err());
    }

    #[test]
    fn anchor_for_returns_recorded_pair() {
        // Glyph 5 → block offset 0; block has two anchors at (10, 20)
        // and (-30, 40).
        let bytes = build_ankr(&[(5, 0)], &[vec![(10, 20), (-30, 40)]]);
        let ankr = Ankr::parse(&bytes).unwrap();
        assert_eq!(ankr.anchor_for(5, 0), Some((10, 20)));
        assert_eq!(ankr.anchor_for(5, 1), Some((-30, 40)));
        // Anchor index past nPoints → None.
        assert_eq!(ankr.anchor_for(5, 2), None);
        // Glyph the lookup doesn't cover → None.
        assert_eq!(ankr.anchor_for(99, 0), None);
    }

    #[test]
    fn multiple_glyphs_with_distinct_blocks() {
        // Glyph 5 → block offset 0; glyph 7 → block offset
        // (4 + 1*4 = 8) — past the first single-anchor block.
        let bytes = build_ankr(&[(5, 0), (7, 8)], &[vec![(1, 2)], vec![(3, 4), (5, 6)]]);
        let ankr = Ankr::parse(&bytes).unwrap();
        assert_eq!(ankr.anchor_for(5, 0), Some((1, 2)));
        assert_eq!(ankr.anchor_for(7, 0), Some((3, 4)));
        assert_eq!(ankr.anchor_for(7, 1), Some((5, 6)));
    }

    #[test]
    fn out_of_range_lookup_returns_none() {
        // Glyph 5 → block offset 0; gid 99 falls through the format-6
        // search and yields CLASS_OUT_OF_BOUNDS, which translates to a
        // huge block offset and the bounds check rejects it.
        let bytes = build_ankr(&[(5, 0)], &[vec![(1, 2)]]);
        let ankr = Ankr::parse(&bytes).unwrap();
        assert_eq!(ankr.anchor_for(99, 0), None);
    }
}
