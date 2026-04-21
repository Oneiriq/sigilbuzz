//! GSUB lookup type 4 — Ligature Substitution.
//!
//! Replaces a sequence of input glyphs with a single output glyph.
//! The canonical use case is `fi` → `ﬁ`, but the lookup is general:
//! any font-declared `(first, ...components) → ligatureGlyph` rule
//! fires when the input matches in order.
//!
//! # Subtable layout
//!
//! ```text
//!   u16      substFormat       = 1
//!   Offset16 coverageOffset    (first component only)
//!   u16      ligatureSetCount
//!   Offset16 ligatureSetOffsets[ligatureSetCount]
//! ```
//!
//! The coverage table lists only the *first* component of each
//! ligature. Every covered glyph has its own `LigatureSet`:
//!
//! ```text
//!   LigatureSet:
//!     u16       ligatureCount
//!     Offset16  ligatureOffsets[ligatureCount]
//!
//!   Ligature:
//!     u16       ligatureGlyph        (output)
//!     u16       componentCount       (including first)
//!     u16       componentGlyphIDs[componentCount - 1]
//! ```
//!
//! # Application
//!
//! At a glyph position `i` where `glyphs[i]` is covered:
//! - Look up the LigatureSet for that glyph.
//! - Walk its ligatures in order. A ligature with components
//!   `[c1, c2, ...]` matches when `glyphs[i+1..]` start with
//!   `c2, c3, ...`.
//! - On the first match, consume `componentCount` glyphs and emit
//!   the ligature glyph in their place. Stop scanning this
//!   ligature set.

use crate::error::{Error, Result};
use crate::tables::layout::Coverage;
use crate::tables::parse::Reader;

/// A parsed Ligature Substitution subtable.
#[derive(Debug, Clone, Copy)]
pub struct Ligature<'a> {
    data: &'a [u8],
    coverage: Coverage<'a>,
    set_offsets_off: usize,
    set_count: u16,
}

impl<'a> Ligature<'a> {
    /// Parses a Ligature Substitution subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported ligature substitution format",
            });
        }
        let coverage_off = r.read_u16()? as usize;
        let set_count = r.read_u16()?;
        let set_offsets_off = r.position();

        let need = set_offsets_off + set_count as usize * 2;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: set_offsets_off,
                context: "ligature set offsets shorter than count",
            });
        }

        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "ligature coverage offset past end",
        })?)?;

        Ok(Self {
            data,
            coverage,
            set_offsets_off,
            set_count,
        })
    }

    /// Tries to match a ligature starting at `glyphs[0]`. Returns the
    /// output glyph id and the number of input glyphs consumed when a
    /// ligature fires; `None` when no rule in this subtable matches.
    ///
    /// Search order within a LigatureSet matches the spec: first
    /// match wins, so longer-first ordering in the font wins over
    /// shorter alternatives.
    #[must_use]
    pub fn apply(&self, glyphs: &[u16]) -> Option<(u16, usize)> {
        let first = *glyphs.first()?;
        let cov_index = self.coverage.index_of(first)?;
        if cov_index >= self.set_count {
            return None;
        }
        let set_off_off = self.set_offsets_off + cov_index as usize * 2;
        let set_off =
            u16::from_be_bytes([self.data[set_off_off], self.data[set_off_off + 1]]) as usize;
        let set_bytes = self.data.get(set_off..)?;

        let mut r = Reader::new(set_bytes);
        let lig_count = r.read_u16().ok()?;
        if set_bytes.len() < 2 + lig_count as usize * 2 {
            return None;
        }

        for i in 0..lig_count {
            let off_off = 2 + i as usize * 2;
            let lig_off = u16::from_be_bytes([set_bytes[off_off], set_bytes[off_off + 1]]) as usize;
            let Some(lig_bytes) = set_bytes.get(lig_off..) else {
                continue;
            };
            let Some((out, consumed)) = try_match_ligature(lig_bytes, glyphs) else {
                continue;
            };
            return Some((out, consumed));
        }
        None
    }
}

/// Attempts to match one Ligature record. Returns
/// `Some((ligature_glyph, consumed))` on a match where `consumed` is
/// the total number of input glyphs the ligature eats (including the
/// first, coverage-matched one).
fn try_match_ligature(lig_bytes: &[u8], glyphs: &[u16]) -> Option<(u16, usize)> {
    let mut r = Reader::new(lig_bytes);
    let ligature_glyph = r.read_u16().ok()?;
    let component_count = r.read_u16().ok()?;
    if component_count == 0 || (component_count as usize) > glyphs.len() {
        return None;
    }
    // Tail components: componentCount - 1 trailing u16s.
    let tail = component_count as usize - 1;
    let tail_bytes = r.read_bytes(tail * 2).ok()?;
    for i in 0..tail {
        let expected = u16::from_be_bytes([tail_bytes[i * 2], tail_bytes[i * 2 + 1]]);
        if glyphs[i + 1] != expected {
            return None;
        }
    }
    Some((ligature_glyph, component_count as usize))
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

    fn build_ligature(ligature_glyph: u16, tail_components: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&ligature_glyph.to_be_bytes());
        out.extend_from_slice(&((tail_components.len() + 1) as u16).to_be_bytes());
        for c in tail_components {
            out.extend_from_slice(&c.to_be_bytes());
        }
        out
    }

    fn build_ligature_set(ligatures: &[(u16, Vec<u16>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(ligatures.len() as u16).to_be_bytes());
        let offsets_start = out.len();
        for _ in 0..ligatures.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        for (i, (lg, tail)) in ligatures.iter().enumerate() {
            let body_start = out.len();
            out.extend_from_slice(&build_ligature(*lg, tail));
            let slot = offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
        }
        out
    }

    /// Builds a type-4 subtable mapping each covered glyph to one or
    /// more ligatures in its LigatureSet.
    #[allow(clippy::type_complexity)]
    fn build_subtable(sets: &[(u16, Vec<(u16, Vec<u16>)>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let cov_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // coverage offset placeholder
        out.extend_from_slice(&(sets.len() as u16).to_be_bytes());
        let set_offsets_start = out.len();
        for _ in 0..sets.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        // LigatureSet bodies.
        for (i, (_first, ligs)) in sets.iter().enumerate() {
            let body_start = out.len();
            out.extend_from_slice(&build_ligature_set(ligs));
            let slot = set_offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
        }
        // Coverage table appended, its offset patched in.
        let cov_start = out.len();
        let first_glyphs: Vec<u16> = sets.iter().map(|(f, _)| *f).collect();
        out.extend_from_slice(&build_coverage_format1(&first_glyphs));
        out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn fi_ligature_fires_on_f_then_i() {
        // Covered first glyph: 10 ('f'). Ligature: 10 + 20 → 100.
        let bytes = build_subtable(&[(10, alloc::vec![(100, alloc::vec![20])])]);
        let lig = Ligature::parse(&bytes).unwrap();
        let out = lig.apply(&[10, 20, 30]).unwrap();
        assert_eq!(out, (100, 2));
    }

    #[test]
    fn three_component_ligature_fires() {
        // 10 + 20 + 30 → 500
        let bytes = build_subtable(&[(10, alloc::vec![(500, alloc::vec![20, 30])])]);
        let lig = Ligature::parse(&bytes).unwrap();
        assert_eq!(lig.apply(&[10, 20, 30, 99]).unwrap(), (500, 3));
    }

    #[test]
    fn non_matching_sequence_returns_none() {
        let bytes = build_subtable(&[(10, alloc::vec![(100, alloc::vec![20])])]);
        let lig = Ligature::parse(&bytes).unwrap();
        // First glyph not covered.
        assert!(lig.apply(&[99, 20]).is_none());
        // Covered first glyph but wrong second.
        assert!(lig.apply(&[10, 99]).is_none());
        // Covered but not enough glyphs left.
        assert!(lig.apply(&[10]).is_none());
    }

    #[test]
    fn first_match_wins_within_a_ligature_set() {
        // First ligature: 10 + 20 → 100.
        // Second ligature: 10 + 20 + 30 → 999. Will never fire because
        // the shorter one is listed first and wins on (10, 20, 30).
        let bytes = build_subtable(&[(
            10,
            alloc::vec![(100, alloc::vec![20]), (999, alloc::vec![20, 30])],
        )]);
        let lig = Ligature::parse(&bytes).unwrap();
        assert_eq!(lig.apply(&[10, 20, 30]).unwrap(), (100, 2));
    }

    #[test]
    fn longer_ligature_fires_when_listed_first() {
        let bytes = build_subtable(&[(
            10,
            alloc::vec![(999, alloc::vec![20, 30]), (100, alloc::vec![20])],
        )]);
        let lig = Ligature::parse(&bytes).unwrap();
        assert_eq!(lig.apply(&[10, 20, 30]).unwrap(), (999, 3));
        // On (10, 20) the second (shorter) ligature still matches.
        assert_eq!(lig.apply(&[10, 20]).unwrap(), (100, 2));
    }

    #[test]
    fn multiple_ligature_sets_share_subtable() {
        // Covered first glyphs: 10 and 40.
        let bytes = build_subtable(&[
            (10, alloc::vec![(100, alloc::vec![20])]),
            (40, alloc::vec![(200, alloc::vec![50])]),
        ]);
        let lig = Ligature::parse(&bytes).unwrap();
        assert_eq!(lig.apply(&[10, 20]).unwrap(), (100, 2));
        assert_eq!(lig.apply(&[40, 50]).unwrap(), (200, 2));
        assert!(lig.apply(&[10, 50]).is_none());
        assert!(lig.apply(&[40, 20]).is_none());
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 4]);
        assert!(matches!(
            Ligature::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        let bytes = [0u8; 3];
        assert!(Ligature::parse(&bytes).is_err());
    }
}
