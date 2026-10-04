//! Parsed GSUB subtables: parsing one subtable, and the per-cursor
//! dispatch over a lookup's subtables, which parses each the first
//! time a glyph its accelerator admits reaches it (see the `lazy`
//! module).

use super::gsub::{apply_gsub_chain_context_at, apply_gsub_context_at, GsubCx};
use super::gsub_buffer::GsubBuffer;
use super::lazy::{LazySubtables, Subtable};
use super::{lig, resolve_extension, LookupBudget};
use crate::tables::gdef::Gdef;
use crate::tables::gsub::{
    lookup_type as gsub_lt, Alternate, ChainContextAny, Context as GsubContext, Ligature, Multiple,
    ReverseChain, Single,
};
use crate::tables::layout::accel::Accel;
use crate::tables::layout::{Lookup, MatchContext, MatchFilter};

/// One parsed GSUB subtable, ready to drive a cursor walk.
///
/// A lookup's subtables are parsed at most once per application and
/// reused across every cursor step. Without that, ChainContext /
/// Context format-3 parsing allocates three or four `Vec<Coverage>`
/// and a `Vec<SubstLookupRecord>` on every cursor: `O(N * subtables)`
/// allocations for a single feature, the lion's share of the
/// Devanagari regression.
pub(super) enum ParsedGsubSubtable<'a> {
    Single(Single<'a>),
    Multiple(Multiple<'a>),
    Alternate(Alternate<'a>),
    Ligature(Ligature<'a>),
    Context(GsubContext<'a>),
    ChainContext(ChainContextAny<'a>),
    ReverseChained(ReverseChain<'a>),
}

impl<'a> Subtable<'a> for ParsedGsubSubtable<'a> {
    fn parse(lookup: &Lookup<'a>, index: u16) -> Option<Self> {
        parse_subtable(lookup, index)
    }

    fn reads_rule_set_digests(&self) -> bool {
        matches!(
            self,
            Self::Context(GsubContext::Format2(_))
                | Self::ChainContext(ChainContextAny::Format2(_))
        )
    }
}

/// A GSUB lookup's subtables, parsed on demand.
pub(super) type GsubSubtables<'a> = LazySubtables<'a, ParsedGsubSubtable<'a>>;

/// The subtables of `lookup`, none parsed yet.
pub(super) fn lazy_subtables(lookup: Lookup<'_>) -> GsubSubtables<'_> {
    LazySubtables::new(lookup)
}

/// Parses subtable `sub_idx` of `lookup`, looking through an Extension
/// (type 7) wrapper, so the caller never sees raw lookup type 7.
/// `None` for a subtable that fails to parse, which the shaper drops.
fn parse_subtable<'a>(lookup: &Lookup<'a>, sub_idx: u16) -> Option<ParsedGsubSubtable<'a>> {
    let bytes = lookup.subtable_bytes(sub_idx)?;
    let raw_lt = lookup.lookup_type();
    let (effective_lt, inner_bytes) = if raw_lt == gsub_lt::EXTENSION {
        resolve_extension(bytes)?
    } else {
        (raw_lt, bytes)
    };
    match effective_lt {
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
    }
}

/// Tries the subtables of one lookup at the cursor in order. The first
/// one that applies wins and leaves the cursor where HarfBuzz does:
/// past a single substitution, past a multiple substitution's outputs,
/// past the glyphs a ligature kept inside its match, and at the end
/// of a contextual match. Returns whether one applied.
///
/// A subtable `accel` shows cannot cover the cursor glyph is passed
/// over unparsed: it starts by looking the glyph up in that coverage,
/// so it would not have applied.
///
/// `nested` is set when a contextual lookup dispatched this one:
/// reverse chaining substitutions do not apply then, as in HarfBuzz.
/// The cursor glyph is not checked against the lookup's flags here;
/// the top-level walk does that, a nested dispatch does not. Nested
/// lookups and multiple substitutions spend `budget`.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_parsed_lookup_at(
    cx: &GsubCx<'_>,
    subtables: &mut GsubSubtables<'_>,
    accel: &Accel<'_, '_>,
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
    for index in 0..subtables.len() {
        let admits = |parsed| accel.subtable_may_start(parsed, usize::from(index), id);
        let Some((sub, digests)) = subtables.get_admitted(index, admits) else {
            continue;
        };
        let mcx = &mcx.with_rule_set_digests(digests);
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
                    found.map(|out| buf.replace_glyph_at(at, out))
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
