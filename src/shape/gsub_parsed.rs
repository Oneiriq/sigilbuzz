//! Pre-parsed GSUB subtables: the parse-once cache a lookup's cursor
//! walk reuses, the coverage digests that let it skip positions, the
//! per-cursor dispatch over the cache, and the glyph-id shadow buffer
//! the drivers keep in sync.

use alloc::vec::Vec;

use super::gsub::{
    apply_gsub_chain_context_at, apply_gsub_context_at, expand_glyph_in_place, substitute_glyph,
};
use super::{lig, resolve_extension};
use crate::buffer::Glyph;
use crate::tables::gdef::Gdef;
use crate::tables::gsub::{
    lookup_type as gsub_lt, Alternate, ChainContextAny, Context as GsubContext, Ligature, Multiple,
    ReverseChain, Single,
};
use crate::tables::layout::{Lookup, MatchFilter};
use crate::tables::Gsub;

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
/// silently dropped, matching the per-cursor behavior the inline
/// `apply_gsub_lookup_at` walker had before the cache was introduced.
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

/// Reports whether at least one glyph in `ids` could trigger any
/// subtable in `parsed`, a fast pre-filter so the cursor walk in
/// `apply_gsub_lookup` skips lookups whose coverage doesn't intersect
/// the run at all. Mirrors HarfBuzz's `would_apply` skip; returns
/// `true` conservatively when a subtable doesn't expose its primary
/// coverage cheaply.
pub(super) fn lookup_might_apply(parsed: &[ParsedGsubSubtable<'_>], ids: &[u16]) -> bool {
    if ids.is_empty() {
        return false;
    }
    for sub in parsed {
        match primary_coverage_of(sub) {
            None => return true,
            Some(cov) => {
                for &id in ids {
                    if cov.contains(id) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// True when every subtable in `parsed` exposes a single primary
/// coverage we can intersect with the run. When that holds, the
/// cursor walker can use the "digest" path: skip any cursor whose
/// glyph isn't in the union of those coverages, instead of trying
/// every subtable at every cursor.
pub(super) fn parsed_has_full_digest(parsed: &[ParsedGsubSubtable<'_>]) -> bool {
    parsed.iter().all(|s| primary_coverage_of(s).is_some())
}

/// True when `glyphs[i]` is in any of `parsed`'s primary coverages.
/// Caller has already established that every subtable exposes one
/// (`parsed_has_full_digest`). Falling out of the digest path back to
/// the per-position walker happens at the caller level.
pub(super) fn cursor_in_digest(parsed: &[ParsedGsubSubtable<'_>], id: u16) -> bool {
    for sub in parsed {
        if let Some(cov) = primary_coverage_of(sub) {
            if cov.contains(id) {
                return true;
            }
        }
    }
    false
}

/// Cursor-position dispatch over a pre-parsed subtable list. Mirrors
/// the inner loop of `apply_gsub_lookup_at` but without the
/// per-cursor parse cost. Returns the input span the matching subtable
/// consumed (1 for Single/Alternate, N for Ligature, the input window
/// length for Context / Chain / Reverse), or 0 when no subtable fired.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_parsed_lookup_at(
    gsub: &Gsub<'_>,
    parsed: &[ParsedGsubSubtable<'_>],
    filter: &MatchFilter<'_>,
    glyphs: &mut Vec<Glyph>,
    ids: &mut GlyphIds,
    gdef: Option<&Gdef<'_>>,
    at: usize,
    depth: u8,
    alternate_index: u16,
) -> usize {
    if at >= glyphs.len() {
        return 0;
    }
    for sub in parsed {
        match sub {
            ParsedGsubSubtable::Single(single) => {
                let id = glyphs[at].glyph_id as u16;
                if filter.is_skipped(id) {
                    continue;
                }
                if let Some(out) = single.apply(id) {
                    substitute_glyph(&mut glyphs[at], out);
                    ids.set(at, out);
                    return 1;
                }
            }
            ParsedGsubSubtable::Multiple(m) => {
                let id = glyphs[at].glyph_id as u16;
                if filter.is_skipped(id) {
                    continue;
                }
                if let Some(seq) = m.apply(id) {
                    if let Some(n) = expand_glyph_in_place(glyphs, at, &seq) {
                        ids.resync(glyphs);
                        return n;
                    }
                }
            }
            ParsedGsubSubtable::Alternate(alt) => {
                let id = glyphs[at].glyph_id as u16;
                if filter.is_skipped(id) {
                    continue;
                }
                if let Some(out) = alt.apply(id, alternate_index) {
                    substitute_glyph(&mut glyphs[at], out);
                    ids.set(at, out);
                    return 1;
                }
            }
            ParsedGsubSubtable::Ligature(ligature) => {
                if let Some((out, positions)) =
                    ligature.apply_filtered(&ids.as_slice()[at..], filter)
                {
                    let level = gsub.cluster_level();
                    lig::ligate(glyphs, at, &positions, out, gdef, substitute_glyph, level);
                    ids.resync(glyphs);
                    // Ligature emits 1 glyph from N matched components.
                    // The cursor must advance past the ligature output
                    // *and* any skipped (filtered) glyphs that survived
                    // inside the matched window: in HarfBuzz's
                    // input/output buffer model that is `idx + span` in
                    // INPUT space; in our in-place model the buffer
                    // already shrunk by `(positions.len() - 1)` glyphs,
                    // so the equivalent NEW-buffer advance is
                    // `span - (positions.len() - 1)` = `1 + skipped`.
                    //
                    // Returning the raw input span over-advances by the
                    // number of consumed components, which silently skips
                    // the next-letter slot, visible as Mongolian's calt
                    // marker-pass leaking marker glyphs on 3+ letter
                    // chains (#118).
                    let span = positions.last().copied().map_or(0, |p| p + 1);
                    let advance = 1 + span.saturating_sub(positions.len());
                    return advance;
                }
            }
            ParsedGsubSubtable::Context(ctx) => {
                let ran =
                    apply_gsub_context_at(gsub, ctx, glyphs, ids, gdef, filter, at, depth + 1);
                if ran > 0 {
                    return ran;
                }
            }
            ParsedGsubSubtable::ChainContext(chain) => {
                let ran = apply_gsub_chain_context_at(
                    gsub,
                    chain,
                    glyphs,
                    ids,
                    gdef,
                    filter,
                    at,
                    depth + 1,
                );
                if ran > 0 {
                    return ran;
                }
            }
            ParsedGsubSubtable::ReverseChained(rc) => {
                if let Some(out) = rc.apply(ids.as_slice(), at) {
                    substitute_glyph(&mut glyphs[at], out);
                    ids.set(at, out);
                    return 1;
                }
            }
        }
    }
    0
}

/// Mirror buffer of `glyph_id`s, kept in lockstep with the live
/// `Vec<Glyph>` that GSUB drivers mutate. The matchers for context /
/// chained-context / reverse-chain / ligature subtables all want a
/// flat `&[u16]` for backtrack/lookahead/window scanning; before this
/// shadow buffer existed every cursor step rebuilt that slice via
/// `glyphs.iter().map(...).collect()`, which is `O(N²)` for a feature
/// that fires on every glyph. We keep the shadow in sync manually
/// after each substitution: Single/Alternate touch one slot,
/// Ligature/Multiple change length and trigger a full resync.
#[derive(Debug)]
pub(super) struct GlyphIds {
    ids: Vec<u16>,
}

impl GlyphIds {
    pub(super) fn from_glyphs(glyphs: &[Glyph]) -> Self {
        let mut ids = Vec::with_capacity(glyphs.len());
        for g in glyphs {
            ids.push(g.glyph_id as u16);
        }
        Self { ids }
    }

    pub(super) fn as_slice(&self) -> &[u16] {
        &self.ids
    }

    /// Single-slot update; the glyph at `at` gained a new id but the
    /// stream length is unchanged. Caller has already written to the
    /// `Glyph` struct.
    pub(super) fn set(&mut self, at: usize, gid: u16) {
        if at < self.ids.len() {
            self.ids[at] = gid;
        }
    }

    /// Length-changing substitution (ligature drain, multiple-sub
    /// expansion). Cheaper than maintaining diff edits inside every
    /// driver. These substitutions are far less common than context
    /// matches anyway.
    pub(super) fn resync(&mut self, glyphs: &[Glyph]) {
        self.ids.clear();
        for g in glyphs {
            self.ids.push(g.glyph_id as u16);
        }
    }
}

/// Builds a [`MatchFilter`] scoped to one lookup, honoring its
/// `LookupFlag`, GDEF-backed glyph classes, and the optional
/// `markFilteringSet` trailer when the font carries one.
pub(super) fn filter_for_lookup<'a>(
    lookup: &Lookup<'a>,
    gdef: Option<&'a Gdef<'a>>,
) -> MatchFilter<'a> {
    MatchFilter::for_lookup(lookup.flag(), gdef, lookup.mark_filtering_set())
}
