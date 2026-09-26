//! Pre-parsed GSUB subtables: the parse-once cache a lookup's cursor
//! walk reuses, the coverage digests that let it skip positions, the
//! per-cursor dispatch over the cache, and the matching view of the
//! glyph run the drivers keep in sync.

use alloc::vec::Vec;

use super::gsub::{
    apply_gsub_chain_context_at, apply_gsub_context_at, expand_glyph_in_place, substitute_glyph,
    GsubCx,
};
use super::{lig, resolve_extension, LookupBudget};
use crate::buffer::Glyph;
use crate::tables::gdef::Gdef;
use crate::tables::gsub::{
    lookup_type as gsub_lt, Alternate, ChainContextAny, Context as GsubContext, Ligature, Multiple,
    ReverseChain, Single,
};
use crate::tables::layout::{Lookup, MatchContext, MatchFilter, MatchGlyph};

/// One pre-parsed GSUB subtable, ready to drive a cursor walk.
///
/// `apply_gsub_lookup` parses the lookup's subtables once into this
/// enum and reuses the parsed views across every cursor step. Without
/// the cache, ChainContext / Context format-3 parsing allocates three
/// or four `Vec<Coverage>` and a `Vec<SubstLookupRecord>` on every
/// cursor: `O(N * subtables)` allocations for a single feature, the
/// lion's share of the Devanagari regression.
pub(super) enum ParsedGsubSubtable<'a> {
    Single(Single<'a>),
    Multiple(Multiple<'a>),
    Alternate(Alternate<'a>),
    Ligature(Ligature<'a>),
    Context(GsubContext<'a>),
    ChainContext(ChainContextAny<'a>),
    ReverseChained(ReverseChain<'a>),
}

/// Parses the subtables of a single `Lookup`, handling the Extension
/// type-7 unwrap so the caller never sees raw lookup type 7. Returns
/// the parsed list in spec order; subtables that fail to parse are
/// dropped.
pub(super) fn parse_lookup_subtables<'a>(
    lookup: &Lookup<'a>,
    raw_lt: u16,
) -> Vec<ParsedGsubSubtable<'a>> {
    let count = lookup.subtable_count() as usize;
    let mut out: Vec<ParsedGsubSubtable<'a>> = Vec::with_capacity(count);
    for sub_idx in 0..lookup.subtable_count() {
        let Some(bytes) = lookup.subtable_bytes(sub_idx) else {
            continue;
        };
        let (effective_lt, inner_bytes) = if raw_lt == gsub_lt::EXTENSION {
            match resolve_extension(bytes) {
                Some(pair) => pair,
                None => continue,
            }
        } else {
            (raw_lt, bytes)
        };
        let parsed = match effective_lt {
            gsub_lt::SINGLE => Single::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::Single),
            gsub_lt::MULTIPLE => Multiple::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::Multiple),
            gsub_lt::ALTERNATE => Alternate::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::Alternate),
            gsub_lt::LIGATURE => Ligature::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::Ligature),
            gsub_lt::CONTEXT => GsubContext::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::Context),
            gsub_lt::CHAINED_CONTEXT => ChainContextAny::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::ChainContext),
            gsub_lt::REVERSE_CHAINED => ReverseChain::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::ReverseChained),
            _ => None,
        };
        if let Some(p) = parsed {
            out.push(p);
        }
    }
    out
}

/// Returns the "primary" coverage table for a parsed subtable: the
/// coverage on the cursor glyph. Used by the run-level `would_apply`
/// precheck and by the cursor digest in `apply_gsub_lookup`. `None`
/// means the subtable's coverage isn't a single `Coverage` table
/// (chain-context format 1/2, reverse-chain, ...) and the cursor
/// walker has to fall back to per-position dispatch.
fn primary_coverage_of<'a, 'b>(
    sub: &'b ParsedGsubSubtable<'a>,
) -> Option<&'b crate::tables::layout::Coverage<'a>> {
    match sub {
        ParsedGsubSubtable::Single(
            Single::Delta { coverage, .. } | Single::Explicit { coverage, .. },
        ) => Some(coverage),
        ParsedGsubSubtable::Multiple(m) => Some(m.coverage()),
        ParsedGsubSubtable::Alternate(a) => Some(a.coverage()),
        ParsedGsubSubtable::Ligature(l) => Some(l.coverage()),
        ParsedGsubSubtable::ChainContext(ChainContextAny::Format3(c3)) => c3.input_first_coverage(),
        ParsedGsubSubtable::Context(GsubContext::Format3(c3)) => c3.input().first(),
        _ => None,
    }
}

/// Reports whether at least one glyph in `run` could trigger any
/// subtable in `parsed`, a fast pre-filter so the cursor walk in
/// `apply_gsub_lookup` skips lookups whose coverage doesn't intersect
/// the run at all. Mirrors HarfBuzz's `would_apply` skip; returns
/// `true` conservatively when a subtable doesn't expose its primary
/// coverage cheaply.
pub(super) fn lookup_might_apply(parsed: &[ParsedGsubSubtable<'_>], run: &[MatchGlyph]) -> bool {
    if run.is_empty() {
        return false;
    }
    parsed.iter().any(|sub| match primary_coverage_of(sub) {
        None => true,
        Some(cov) => run.iter().any(|g| cov.contains(g.id)),
    })
}

/// True when every subtable in `parsed` exposes a single primary
/// coverage we can intersect with the run. When that holds, the
/// cursor walker can use the "digest" path: skip any cursor whose
/// glyph isn't in the union of those coverages, instead of trying
/// every subtable at every cursor.
pub(super) fn parsed_has_full_digest(parsed: &[ParsedGsubSubtable<'_>]) -> bool {
    parsed.iter().all(|s| primary_coverage_of(s).is_some())
}

/// True when glyph `id` is in any of `parsed`'s primary coverages.
/// Caller has already established that every subtable exposes one
/// (`parsed_has_full_digest`).
pub(super) fn cursor_in_digest(parsed: &[ParsedGsubSubtable<'_>], id: u16) -> bool {
    parsed
        .iter()
        .filter_map(primary_coverage_of)
        .any(|cov| cov.contains(id))
}

/// Tries the subtables of one lookup at `at` in order; the first one
/// that applies wins. Returns where the lookup's walk continues when
/// one applied (HarfBuzz leaves the cursor past a single substitution,
/// past a multiple substitution's outputs, past the glyphs a ligature
/// kept inside its match, and at the end of a contextual match), or
/// `None` when none did.
///
/// `nested` is set when a contextual lookup dispatched this one:
/// reverse chaining substitutions do not apply then, as in HarfBuzz.
/// The glyph at `at` is not checked against the lookup's flags here;
/// the top-level walk does that, a nested dispatch does not. Nested
/// lookups and multiple substitutions spend `budget`.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_parsed_lookup_at(
    cx: &GsubCx<'_>,
    parsed: &[ParsedGsubSubtable<'_>],
    mcx: &MatchContext<'_>,
    glyphs: &mut Vec<Glyph>,
    run: &mut MatchRun,
    at: usize,
    depth: u8,
    alternate_index: u16,
    nested: bool,
    budget: &mut LookupBudget,
) -> Option<usize> {
    if at >= glyphs.len() {
        return None;
    }
    let id = run.get(at).id;
    for sub in parsed {
        let next = match sub {
            ParsedGsubSubtable::Single(single) => single.apply(id).map(|out| {
                substitute_glyph(&mut glyphs[at], out);
                run.sync(at, &glyphs[at]);
                at + 1
            }),
            ParsedGsubSubtable::Multiple(m) => m
                .apply(id)
                .and_then(|seq| expand_glyph_in_place(glyphs, at, &seq, budget))
                .map(|n| {
                    run.resync(glyphs);
                    at + n
                }),
            ParsedGsubSubtable::Alternate(alt) => alt.apply(id, alternate_index).map(|out| {
                substitute_glyph(&mut glyphs[at], out);
                run.sync(at, &glyphs[at]);
                at + 1
            }),
            ParsedGsubSubtable::Ligature(ligature) => {
                ligature.apply_at(run.as_slice(), at, mcx).map(|(out, m)| {
                    let positions = m.positions.as_slice();
                    if positions.len() == 1 {
                        // A one-component ligature is a plain
                        // substitution, not a ligation.
                        substitute_glyph(&mut glyphs[at], out);
                    } else {
                        let classes = mcx.filter().classes();
                        let level = cx.gsub.cluster_level();
                        lig::ligate(glyphs, positions, out, &classes, substitute_glyph, level);
                    }
                    run.resync(glyphs);
                    // The components after the first are gone; the
                    // walk resumes after the last one's old place.
                    m.end - (positions.len() - 1)
                })
            }
            ParsedGsubSubtable::Context(ctx) => {
                apply_gsub_context_at(cx, ctx, mcx, glyphs, run, at, depth + 1, budget)
            }
            ParsedGsubSubtable::ChainContext(chain) => {
                apply_gsub_chain_context_at(cx, chain, mcx, glyphs, run, at, depth + 1, budget)
            }
            ParsedGsubSubtable::ReverseChained(rc) => {
                if nested {
                    None
                } else {
                    rc.apply_at(run.as_slice(), at, mcx).map(|out| {
                        substitute_glyph(&mut glyphs[at], out);
                        run.sync(at, &glyphs[at]);
                        at + 1
                    })
                }
            }
        };
        if next.is_some() {
            return next;
        }
    }
    None
}

/// The run as the matching rules see it ([`MatchGlyph`]s), kept in
/// lockstep with the live `Vec<Glyph>` that GSUB drivers mutate. The
/// matchers want a flat slice for backtrack/lookahead/window
/// scanning; rebuilding it per cursor step would be `O(N^2)` for a
/// feature that fires on every glyph. Single substitutions update one
/// slot, ligatures and multiple substitutions resync.
#[derive(Debug)]
pub(super) struct MatchRun {
    glyphs: Vec<MatchGlyph>,
}

impl MatchRun {
    pub(super) fn from_glyphs(glyphs: &[Glyph]) -> Self {
        Self {
            glyphs: glyphs.iter().map(MatchGlyph::from).collect(),
        }
    }

    pub(super) fn as_slice(&self) -> &[MatchGlyph] {
        &self.glyphs
    }

    pub(super) fn get(&self, at: usize) -> MatchGlyph {
        self.glyphs.get(at).copied().unwrap_or_default()
    }

    /// The glyph at `at` changed in place (id and props).
    pub(super) fn sync(&mut self, at: usize, glyph: &Glyph) {
        if let Some(slot) = self.glyphs.get_mut(at) {
            *slot = MatchGlyph::from(glyph);
        }
    }

    /// The run changed length or several glyphs changed.
    pub(super) fn resync(&mut self, glyphs: &[Glyph]) {
        self.glyphs.clear();
        self.glyphs.extend(glyphs.iter().map(MatchGlyph::from));
    }
}

/// Builds a [`MatchFilter`] scoped to one lookup, honoring its
/// `LookupFlag`, the font's glyph classes, and the optional
/// `markFilteringSet` trailer when the font carries one.
pub(super) fn filter_for_lookup<'a>(
    lookup: &Lookup<'a>,
    gdef: Option<&'a Gdef<'a>>,
) -> MatchFilter<'a> {
    MatchFilter::for_lookup(lookup.flag(), gdef, lookup.mark_filtering_set())
}
