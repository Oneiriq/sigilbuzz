//! Pre-parsed GSUB subtables: the parse-once cache a lookup's cursor
//! walk reuses, the coverage digests that let it skip positions,
//! and the per-cursor dispatch over the cache.

use alloc::vec::Vec;

use super::gsub::{apply_gsub_chain_context_at, apply_gsub_context_at, substitute_glyph, GsubCx};
use super::gsub_buffer::GsubBuffer;
use super::{lig, resolve_extension, LookupBudget};
use crate::buffer::Glyph;
use crate::tables::gdef::Gdef;
use crate::tables::gsub::{
    lookup_type as gsub_lt, Alternate, ChainContextAny, Context as GsubContext, Ligature, Multiple,
    ReverseChain, Single,
};
use crate::tables::layout::{Lookup, MatchContext, MatchFilter};

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
pub(super) fn lookup_might_apply(parsed: &[ParsedGsubSubtable<'_>], run: &[Glyph]) -> bool {
    if run.is_empty() {
        return false;
    }
    parsed.iter().any(|sub| match primary_coverage_of(sub) {
        None => true,
        Some(cov) => run.iter().any(|g| cov.contains(g.glyph_id as u16)),
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

/// Tries the subtables of one lookup at the cursor in order. The first
/// one that applies wins and leaves the cursor where HarfBuzz does:
/// past a single substitution, past a multiple substitution's outputs,
/// past the glyphs a ligature kept inside its match, and at the end
/// of a contextual match. Returns whether one applied.
///
/// `nested` is set when a contextual lookup dispatched this one:
/// reverse chaining substitutions do not apply then, as in HarfBuzz.
/// The cursor glyph is not checked against the lookup's flags here;
/// the top-level walk does that, a nested dispatch does not. Nested
/// lookups and multiple substitutions spend `budget`.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_parsed_lookup_at(
    cx: &GsubCx<'_>,
    parsed: &[ParsedGsubSubtable<'_>],
    mcx: &MatchContext<'_>,
    buf: &mut GsubBuffer,
    depth: u8,
    alternate_index: u16,
    nested: bool,
    budget: &mut LookupBudget,
) -> bool {
    let Some(cur) = buf.cur() else {
        return false;
    };
    let id = cur.glyph_id as u16;
    let at = buf.cursor();
    for sub in parsed {
        let applied = match sub {
            ParsedGsubSubtable::Single(single) => {
                single.apply(id).map(|out| buf.replace_glyph(out))
            }
            ParsedGsubSubtable::Multiple(m) => m
                .apply(id)
                .and_then(|seq| apply_multiple(buf, &seq, budget).then_some(())),
            ParsedGsubSubtable::Alternate(alt) => alt
                .apply(id, alternate_index)
                .map(|out| buf.replace_glyph(out)),
            ParsedGsubSubtable::Ligature(ligature) => {
                let mut ops = buf.take_flag_ops();
                let found = ligature.apply_at_in(&*buf, at, mcx, &mut ops);
                buf.apply_flag_ops(ops);
                found.and_then(|(out, m)| {
                    let positions = m.positions.as_slice();
                    if positions.len() == 1 {
                        // A one-component ligature is a plain
                        // substitution, not a ligation.
                        buf.replace_glyph(out);
                        return Some(());
                    }
                    let classes = mcx.filter().classes();
                    lig::ligate(buf, positions, m.end, out, &classes).then_some(())
                })
            }
            ParsedGsubSubtable::Context(ctx) => {
                apply_gsub_context_at(cx, ctx, mcx, buf, depth + 1, budget).then_some(())
            }
            ParsedGsubSubtable::ChainContext(chain) => {
                apply_gsub_chain_context_at(cx, chain, mcx, buf, depth + 1, budget).then_some(())
            }
            ParsedGsubSubtable::ReverseChained(rc) => {
                if nested {
                    None
                } else {
                    // A reverse chaining subtable reached by a forward
                    // walk substitutes in place, as HarfBuzz's
                    // `replace_glyph_inplace` does.
                    let mut ops = buf.take_flag_ops();
                    let found = rc.apply_at_in(&*buf, at, mcx, &mut ops);
                    buf.apply_flag_ops(ops);
                    found.map(|out| {
                        if let Some(g) = buf.cur_mut() {
                            substitute_glyph(g, out);
                        }
                    })
                }
            }
        };
        if applied.is_some() {
            return true;
        }
    }
    false
}

/// A multiple substitution's sequence at the cursor, HarfBuzz's
/// `Sequence::apply`: one glyph is a plain substitution, more are
/// output in its place, and none deletes the glyph (the spec forbids
/// an empty sequence, but Uniscribe and HarfBuzz accept it, see HarfBuzz
/// issue 253), merging its cluster into a neighbor.
/// Returns false when `budget` has no room for the extra glyphs (see
/// [`LookupBudget`]).
fn apply_multiple(buf: &mut GsubBuffer, seq: &[u16], budget: &mut LookupBudget) -> bool {
    match seq {
        [] => {
            buf.delete_glyph();
            true
        }
        [one] => {
            buf.replace_glyph(*one);
            true
        }
        _ => {
            if !budget.take_growth(seq.len() - 1) {
                return false;
            }
            lig::multiply(buf, seq);
            true
        }
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
