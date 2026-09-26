//! GSUB lookup type 4: Ligature Substitution.
//!
//! Replaces a sequence of input glyphs with a single output glyph.
//! The canonical use case is `fi` -> `ﬁ`, but the lookup is general:
//! any font-declared `(first, ...components) -> ligatureGlyph` rule
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
use crate::tables::layout::skip_iter::{match_input, InputMatch, MatchContext, MatchGlyph};
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

    /// Coverage table for the run-level "would_apply" precheck.
    #[must_use]
    pub const fn coverage(&self) -> &Coverage<'a> {
        &self.coverage
    }

    /// Tries to match a ligature starting at `glyphs[0]` with plain
    /// matching (no lookup flags, no default ignorables). Returns the
    /// output glyph id and the number of input glyphs consumed when a
    /// ligature fires; `None` when no rule in this subtable matches.
    /// The shaper uses [`Ligature::apply_at`].
    ///
    /// Search order within a LigatureSet matches the spec: first
    /// match wins, so longer-first ordering in the font wins over
    /// shorter alternatives.
    #[must_use]
    pub fn apply(&self, glyphs: &[u16]) -> Option<(u16, usize)> {
        let run: alloc::vec::Vec<MatchGlyph> = glyphs.iter().map(|&g| MatchGlyph::new(g)).collect();
        self.apply_at(&run, 0, &MatchContext::plain())
            .map(|(out, m)| (out, m.end))
    }

    /// Tries the ligatures of `glyphs[at]`'s LigatureSet in order and
    /// returns the first that matches: its output glyph and the
    /// matched input (component positions, the first at `at`), found
    /// with the input walk of `cx` (HarfBuzz's `LigatureSet::apply`
    /// and `match_input`). A one-component ligature matches its first
    /// glyph alone.
    #[must_use]
    pub fn apply_at(
        &self,
        glyphs: &[MatchGlyph],
        at: usize,
        cx: &MatchContext<'_>,
    ) -> Option<(u16, InputMatch)> {
        let first = glyphs.get(at)?.id;
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

        (0..usize::from(lig_count)).find_map(|i| {
            let off_off = 2 + i * 2;
            let lig_off = u16::from_be_bytes([set_bytes[off_off], set_bytes[off_off + 1]]) as usize;
            match_ligature(set_bytes.get(lig_off..)?, glyphs, at, cx)
        })
    }
}

/// Matches one Ligature table at `glyphs[at]`: its tail components
/// against the glyphs the input walk stops at.
fn match_ligature(
    lig_bytes: &[u8],
    glyphs: &[MatchGlyph],
    at: usize,
    cx: &MatchContext<'_>,
) -> Option<(u16, InputMatch)> {
    let mut r = Reader::new(lig_bytes);
    let ligature_glyph = r.read_u16().ok()?;
    let component_count = r.read_u16().ok()?;
    if component_count == 0 {
        return None;
    }
    let tail = usize::from(component_count) - 1;
    let tail_bytes = r.read_bytes(tail * 2).ok()?;
    let component = |k: usize| u16::from_be_bytes([tail_bytes[k * 2], tail_bytes[k * 2 + 1]]);
    let m = match_input(glyphs, at, tail, cx, |k, g| g == component(k))?;
    Some((ligature_glyph, m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::layout::skip_iter::{Joiners, LayoutTable, MatchFilter};
    use alloc::vec::Vec;

    /// A run of glyphs with no props.
    fn run(ids: &[u16]) -> Vec<MatchGlyph> {
        ids.iter().map(|&id| MatchGlyph::new(id)).collect()
    }

    /// Applies `lig` at the start of `ids` with plain matching: the
    /// output glyph and where the match ends.
    fn apply(lig: &Ligature<'_>, ids: &[u16]) -> Option<(u16, usize)> {
        lig.apply_at(&run(ids), 0, &MatchContext::plain())
            .map(|(out, m)| (out, m.end))
    }

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
        // Covered first glyph: 10 ('f'). Ligature: 10 + 20 -> 100.
        let bytes = build_subtable(&[(10, alloc::vec![(100, alloc::vec![20])])]);
        let lig = Ligature::parse(&bytes).unwrap();
        let out = apply(&lig, &[10, 20, 30]).unwrap();
        assert_eq!(out, (100, 2));
    }

    #[test]
    fn three_component_ligature_fires() {
        // 10 + 20 + 30 -> 500
        let bytes = build_subtable(&[(10, alloc::vec![(500, alloc::vec![20, 30])])]);
        let lig = Ligature::parse(&bytes).unwrap();
        assert_eq!(apply(&lig, &[10, 20, 30, 99]).unwrap(), (500, 3));
    }

    #[test]
    fn non_matching_sequence_returns_none() {
        let bytes = build_subtable(&[(10, alloc::vec![(100, alloc::vec![20])])]);
        let lig = Ligature::parse(&bytes).unwrap();
        // First glyph not covered.
        assert!(apply(&lig, &[99, 20]).is_none());
        // Covered first glyph but wrong second.
        assert!(apply(&lig, &[10, 99]).is_none());
        // Covered but not enough glyphs left.
        assert!(apply(&lig, &[10]).is_none());
    }

    #[test]
    fn first_match_wins_within_a_ligature_set() {
        // First ligature: 10 + 20 -> 100.
        // Second ligature: 10 + 20 + 30 -> 999. Will never fire because
        // the shorter one is listed first and wins on (10, 20, 30).
        let bytes = build_subtable(&[(
            10,
            alloc::vec![(100, alloc::vec![20]), (999, alloc::vec![20, 30])],
        )]);
        let lig = Ligature::parse(&bytes).unwrap();
        assert_eq!(apply(&lig, &[10, 20, 30]).unwrap(), (100, 2));
    }

    #[test]
    fn longer_ligature_fires_when_listed_first() {
        let bytes = build_subtable(&[(
            10,
            alloc::vec![(999, alloc::vec![20, 30]), (100, alloc::vec![20])],
        )]);
        let lig = Ligature::parse(&bytes).unwrap();
        assert_eq!(apply(&lig, &[10, 20, 30]).unwrap(), (999, 3));
        // On (10, 20) the second (shorter) ligature still matches.
        assert_eq!(apply(&lig, &[10, 20]).unwrap(), (100, 2));
    }

    #[test]
    fn multiple_ligature_sets_share_subtable() {
        // Covered first glyphs: 10 and 40.
        let bytes = build_subtable(&[
            (10, alloc::vec![(100, alloc::vec![20])]),
            (40, alloc::vec![(200, alloc::vec![50])]),
        ]);
        let lig = Ligature::parse(&bytes).unwrap();
        assert_eq!(apply(&lig, &[10, 20]).unwrap(), (100, 2));
        assert_eq!(apply(&lig, &[40, 50]).unwrap(), (200, 2));
        assert!(apply(&lig, &[10, 50]).is_none());
        assert!(apply(&lig, &[40, 20]).is_none());
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

    #[test]
    fn filter_aware_apply_hops_marks_in_input_window() {
        use crate::tables::gdef::Gdef;
        use crate::tables::layout::skip_iter::LOOKUP_FLAG_IGNORE_MARKS;

        // Ligature: 10 + 20 -> 100. Input stream carries a mark glyph
        // 99 between 10 and 20; with IgnoreMarks the match still fires.
        let bytes = build_subtable(&[(10, alloc::vec![(100, alloc::vec![20])])]);
        let lig = Ligature::parse(&bytes).unwrap();

        // Build a GDEF that classifies 10 and 20 as bases and 99 as a mark.
        let mut cd = alloc::vec::Vec::new();
        cd.extend_from_slice(&2u16.to_be_bytes()); // class def format 2
        cd.extend_from_slice(&3u16.to_be_bytes()); // range count
        for (gid, cls) in [(10u16, 1u16), (20, 1), (99, 3)] {
            cd.extend_from_slice(&gid.to_be_bytes());
            cd.extend_from_slice(&gid.to_be_bytes());
            cd.extend_from_slice(&cls.to_be_bytes());
        }
        let mut gdef_bytes = alloc::vec::Vec::new();
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // major
        gdef_bytes.extend_from_slice(&0u16.to_be_bytes()); // minor
        gdef_bytes.extend_from_slice(&12u16.to_be_bytes()); // glyphClassDefOff
        gdef_bytes.extend_from_slice(&[0u8; 6]);
        gdef_bytes.extend_from_slice(&cd);
        let gdef = Gdef::parse(&gdef_bytes).unwrap();
        let filter = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);

        // Plain apply sees the mark blocking the second component.
        assert!(apply(&lig, &[10, 99, 20]).is_none());

        // Filter-aware apply matches across the mark.
        let cx = MatchContext::new(filter, LayoutTable::Gsub, Joiners::AUTO);
        let (out, m) = lig.apply_at(&run(&[10, 99, 20]), 0, &cx).unwrap();
        assert_eq!(out, 100);
        assert_eq!(m.positions.as_slice(), [0, 2]);
    }

    #[test]
    fn default_ignorables_inside_the_input_follow_the_joiner_rules() {
        use crate::tables::layout::skip_iter::match_prop;
        // f + i -> fi, with a ZWJ or a ZWNJ between them.
        let bytes = build_subtable(&[(10, alloc::vec![(100, alloc::vec![20])])]);
        let lig = Ligature::parse(&bytes).unwrap();
        let joiner = |extra| MatchGlyph::with_props(3, match_prop::DEFAULT_IGNORABLE | extra);
        let zwj = [
            MatchGlyph::new(10),
            joiner(match_prop::ZWJ),
            MatchGlyph::new(20),
        ];
        let zwnj = [
            MatchGlyph::new(10),
            joiner(match_prop::ZWNJ),
            MatchGlyph::new(20),
        ];
        let with = |j| MatchContext::new(MatchFilter::none(), LayoutTable::Gsub, j);
        // ZWJ does not break a ligature...
        let (_, m) = lig.apply_at(&zwj, 0, &with(Joiners::AUTO)).unwrap();
        assert_eq!((m.positions.as_slice(), m.end), (&[0, 2][..], 3));
        // ...unless the feature handles joiners itself.
        assert!(lig.apply_at(&zwj, 0, &with(Joiners::MANUAL_ZWJ)).is_none());
        // ZWNJ always does.
        assert!(lig.apply_at(&zwnj, 0, &with(Joiners::AUTO)).is_none());
    }
}
