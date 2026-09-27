//! GSUB lookups for the syllable-based shapers
//! ([`crate::ot::syllabic`]).
//!
//! HarfBuzz applies a lookup at a glyph only when the glyph's mask has
//! one of the bits of the features the lookup belongs to
//! (`apply_forward` and `apply_backward` in `hb-ot-layout-gsubgpos.hh`),
//! and a feature flagged `F_PER_SYLLABLE` only matches glyphs of the
//! cursor's syllable. The shapers keep masks and syllables beside the
//! glyphs, and [`SyllabicGsub::apply_lookup`] takes both as predicates
//! on the glyph and runs one lookup with them: the cursor stops only at
//! glyphs the mask predicate accepts, and with a syllable key each
//! syllable is matched on its own, so no rule reaches into a
//! neighboring syllable. Lookups of every type follow the mask,
//! contextual ones included.

use alloc::vec::Vec;

use super::gsub::{apply_gsub_lookup, GsubCx};
use super::gsub_parsed::{
    apply_parsed_lookup_at, cursor_in_digest, filter_for_lookup, lookup_might_apply,
    parse_lookup_subtables, parsed_has_full_digest, MatchRun, ParsedGsubSubtable,
};
use super::LookupBudget;
use crate::buffer::Glyph;
use crate::tables::gdef::Gdef;
use crate::tables::layout::{Joiners, LayoutTable, MatchContext};
use crate::tables::Gsub;

/// Applies GSUB lookups for one run of a syllable-based shaper, under
/// one [`LookupBudget`] for the whole run.
pub(crate) struct SyllabicGsub<'a> {
    gsub: &'a Gsub<'a>,
    gdef: Option<&'a Gdef<'a>>,
    budget: LookupBudget,
    scratch: Vec<Glyph>,
}

impl<'a> SyllabicGsub<'a> {
    /// A runner for the run `glyphs`, whose budget the length and
    /// clusters of `glyphs` size.
    pub(crate) fn new(gsub: &'a Gsub<'a>, gdef: Option<&'a Gdef<'a>>, glyphs: &[Glyph]) -> Self {
        Self {
            gsub,
            gdef,
            budget: LookupBudget::for_run(glyphs),
            scratch: Vec::new(),
        }
    }

    /// The table the runner applies.
    pub(crate) fn gsub(&self) -> &'a Gsub<'a> {
        self.gsub
    }

    /// Applies lookup `index` to every glyph of `glyphs`, picking
    /// alternate `alternate` (0-based) in alternate substitutions.
    pub(crate) fn apply_lookup_alternate(
        &mut self,
        index: u16,
        alternate: u16,
        glyphs: &mut Vec<Glyph>,
    ) {
        let (gsub, gdef) = (self.gsub, self.gdef);
        let joiners = Joiners::AUTO;
        apply_gsub_lookup(
            gsub,
            index,
            glyphs,
            gdef,
            alternate,
            joiners,
            &mut self.budget,
        );
    }

    /// Applies lookup `index` to `glyphs`, with the feature's joiner
    /// handling. The lookup applies at a glyph only when `applies`
    /// accepts it. With `syllable`, each run of glyphs with the same
    /// key is matched on its own.
    pub(crate) fn apply_lookup(
        &mut self,
        index: u16,
        joiners: Joiners,
        glyphs: &mut Vec<Glyph>,
        syllable: Option<&dyn Fn(&Glyph) -> u32>,
        applies: &dyn Fn(&Glyph) -> bool,
    ) {
        let Some(lookup) = self.gsub.lookup_list().get(index) else {
            return;
        };
        let parsed = parse_lookup_subtables(&lookup, lookup.lookup_type());
        if parsed.is_empty() || glyphs.is_empty() {
            return;
        }
        let cx = GsubCx {
            gsub: self.gsub,
            gdef: self.gdef,
            joiners,
        };
        let mcx = MatchContext::new(
            filter_for_lookup(&lookup, self.gdef),
            LayoutTable::Gsub,
            joiners,
        );
        let reverse = parsed
            .iter()
            .all(|s| matches!(s, ParsedGsubSubtable::ReverseChained(_)));
        let walk = Walk {
            cx: &cx,
            parsed: &parsed,
            mcx: &mcx,
            applies,
            reverse,
        };
        let Some(key) = syllable else {
            walk.run(glyphs, &mut self.budget);
            return;
        };
        let mut out: Vec<Glyph> = Vec::with_capacity(glyphs.len());
        let mut start = 0;
        while start < glyphs.len() {
            let k = key(&glyphs[start]);
            let end = glyphs[start..]
                .iter()
                .position(|g| key(g) != k)
                .map_or(glyphs.len(), |n| start + n);
            self.scratch.clear();
            self.scratch.extend_from_slice(&glyphs[start..end]);
            let first = glyphs[start].cluster;
            let last = glyphs[end - 1].cluster;
            walk.run(&mut self.scratch, &mut self.budget);
            spread_edge_merges(&mut out, &self.scratch, &mut glyphs[end..], first, last);
            out.extend_from_slice(&self.scratch);
            start = end;
        }
        *glyphs = out;
    }
}

/// Carries cluster merges past a syllable's edges. HarfBuzz's
/// `merge_clusters` extends a merge over neighbors that share a
/// cluster with the range's first or last glyph, and a per-syllable
/// lookup does not stop that at the syllable. When the syllable's
/// first glyph (with cluster `first` before the lookup) or last glyph
/// (`last`) now has a smaller cluster, the neighbors before it in
/// `before`, or after it in `after`, that shared the old cluster take
/// the new one.
fn spread_edge_merges(
    before: &mut [Glyph],
    syllable: &[Glyph],
    after: &mut [Glyph],
    first: u32,
    last: u32,
) {
    if let Some(new) = syllable.first().map(|g| g.cluster).filter(|&c| c < first) {
        for g in before.iter_mut().rev() {
            if g.cluster != first {
                break;
            }
            g.cluster = new;
        }
    }
    if let Some(new) = syllable.last().map(|g| g.cluster).filter(|&c| c < last) {
        for g in after.iter_mut() {
            if g.cluster != last {
                break;
            }
            g.cluster = new;
        }
    }
}

/// One lookup's walk over one run of glyphs.
struct Walk<'w, 'a> {
    cx: &'w GsubCx<'a>,
    parsed: &'w [ParsedGsubSubtable<'a>],
    mcx: &'w MatchContext<'a>,
    applies: &'w dyn Fn(&Glyph) -> bool,
    reverse: bool,
}

impl Walk<'_, '_> {
    /// HarfBuzz's `apply_forward`, or `apply_backward` for a reverse
    /// chaining lookup, with the mask check at the cursor.
    fn run(&self, glyphs: &mut Vec<Glyph>, budget: &mut LookupBudget) {
        let mut run = MatchRun::from_glyphs(glyphs);
        if !lookup_might_apply(self.parsed, run.as_slice()) {
            return;
        }
        if self.reverse {
            for i in (0..glyphs.len()).rev() {
                if self.skips(glyphs, &run, i, false) {
                    continue;
                }
                let at = i;
                let _ = apply_parsed_lookup_at(
                    self.cx,
                    self.parsed,
                    self.mcx,
                    glyphs,
                    &mut run,
                    at,
                    0,
                    0,
                    false,
                    budget,
                );
            }
            return;
        }
        let digest = parsed_has_full_digest(self.parsed);
        let mut i = 0;
        while i < glyphs.len() {
            if self.skips(glyphs, &run, i, digest) {
                i += 1;
                continue;
            }
            i = apply_parsed_lookup_at(
                self.cx,
                self.parsed,
                self.mcx,
                glyphs,
                &mut run,
                i,
                0,
                0,
                false,
                budget,
            )
            .map_or(i + 1, |next| next.max(i + 1));
        }
    }

    /// Whether the cursor passes over glyph `i`: the mask does not
    /// cover it, the lookup flags skip it, or (with `digest`) no
    /// subtable covers it.
    fn skips(&self, glyphs: &[Glyph], run: &MatchRun, i: usize, digest: bool) -> bool {
        let g = run.get(i);
        !glyphs.get(i).is_some_and(|glyph| (self.applies)(glyph))
            || (digest && !cursor_in_digest(self.parsed, g.id))
            || self.mcx.filter().is_skipped(g)
    }
}
