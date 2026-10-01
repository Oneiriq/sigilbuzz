//! The GSUB lookup drivers: the forward cursor walk, its masked and
//! nested variants, contextual dispatch, and the glyph edits every
//! substitution goes through.
//!
//! A lookup walks the run the way HarfBuzz's `apply_forward` does,
//! through a [`GsubBuffer`]: glyphs the cursor passes move to the
//! output, substitutions consume input and produce output, and a
//! contextual rule's nested lookups move the cursor around inside the
//! match (`move_to`). Every lookup is one linear pass.
//!
//! Matching inside a lookup (ligature components, contextual input,
//! backtrack and lookahead) follows HarfBuzz's skipping iterator, see
//! [`crate::tables::layout::skip_iter`]: glyphs the lookup flags
//! ignore are passed over, and so are default-ignorable characters the
//! rule does not name, per the feature's joiner handling
//! ([`Joiners`](crate::tables::layout::Joiners)). A feature HarfBuzz
//! registers per syllable only matches glyphs of the cursor's
//! syllable.

use alloc::vec::Vec;

use super::gsub_buffer::GsubBuffer;
use super::gsub_parsed::{
    apply_parsed_lookup_at, cursor_in_digest, filter_for_lookup, lookup_might_apply,
    parse_lookup_subtables, parsed_has_full_digest, ParsedGsubSubtable,
};
use super::joiners::FeatureFlags;
use super::{resolve_extension, LookupBudget, MAX_NESTED_DEPTH};
use crate::buffer::{unicode_prop, Glyph};
use crate::tables::gdef::Gdef;
use crate::tables::gsub::{
    lookup_type as gsub_lt, ChainContextAny, Context as GsubContext, ReverseChain,
};
use crate::tables::layout::skip_iter::apply_nested;
use crate::tables::layout::{
    InputMatch, LayoutTable, Lookup, MatchContext, MatchGlyph, SequenceLookupRecord,
};
use crate::tables::Gsub;

/// What every GSUB lookup of one pass shares: the table, the font's
/// GDEF, and the flags of the feature the lookups belong to.
pub(super) struct GsubCx<'a> {
    pub(super) gsub: &'a Gsub<'a>,
    pub(super) gdef: Option<&'a Gdef<'a>>,
    pub(super) flags: FeatureFlags,
}

impl<'a> GsubCx<'a> {
    /// The matching context of one lookup.
    fn match_cx(&self, lookup: &Lookup<'a>) -> MatchContext<'a> {
        MatchContext::new(
            filter_for_lookup(lookup, self.gdef),
            LayoutTable::Gsub,
            self.flags.joiners,
        )
        .with_per_syllable(self.flags.per_syllable)
    }
}

/// The lookup's type, looking through an Extension wrapper (a lookup's
/// subtables all share one type, so the first one decides).
fn effective_type(lookup: &Lookup<'_>) -> u16 {
    let raw = lookup.lookup_type();
    if raw == gsub_lt::EXTENSION {
        lookup
            .subtable_bytes(0)
            .and_then(resolve_extension)
            .map_or(raw, |(inner, _)| inner)
    } else {
        raw
    }
}

/// Applies a feature's GSUB lookups, in order, only where `mask`
/// turns the feature on: HarfBuzz's per-glyph feature mask, for the
/// features the shapers give to some glyphs only (the Arabic and
/// Mongolian positional forms, Indic `half`, `rtlm` on mirrored
/// characters). `mask[i]` belongs to `glyphs[i]` and moves with it
/// through every substitution, as the mask bits of HarfBuzz's glyph
/// info do, so the later lookups of the feature still find it.
///
/// As in HarfBuzz, a lookup only starts at a glyph the feature is on
/// at (`apply_forward` and `apply_backward` test `lookup_mask`), and
/// every other input glyph a rule matches (ligature components,
/// contextual input) must have the feature on too (the skipping
/// iterator's `may_match`). Backtrack and lookahead glyphs need not.
pub(super) fn apply_gsub_lookups_masked(
    gsub: &Gsub<'_>,
    lookups: &[u16],
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    mask: &[bool],
    flags: FeatureFlags,
    budget: &mut LookupBudget,
) {
    let cx = GsubCx { gsub, gdef, flags };
    let mut buf = GsubBuffer::new(
        core::mem::take(glyphs),
        Some(mask),
        gsub.cluster_level(),
        gsub.unsafe_to_concat(),
    );
    for &index in lookups {
        if let Some(lookup) = gsub.lookup_list().get(index) {
            apply_lookup_to_buffer(&cx, &lookup, &mut buf, 0, budget);
        }
    }
    *glyphs = buf.into_glyphs();
}

/// One lookup of a GSUB stage: its index, the flags of the features
/// that share it, the alternate an AlternateSubst picks, and whether
/// it only applies where the stage's mask is on (a lookup no global
/// feature of the stage reaches).
#[derive(Debug, Clone, Copy)]
pub(super) struct StageLookup {
    pub(super) index: u16,
    pub(super) flags: FeatureFlags,
    pub(super) alternate: u16,
    pub(super) masked: bool,
}

/// Applies the lookups of one GSUB stage in order, HarfBuzz's
/// `hb_ot_map_t::apply` over a stage: each lookup once, the masked ones
/// only on the glyphs `mask` marks. The mask moves with its glyphs
/// through every lookup of the stage.
pub(super) fn apply_gsub_stage(
    gsub: &Gsub<'_>,
    lookups: &[StageLookup],
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    mask: Option<&[bool]>,
    budget: &mut LookupBudget,
) {
    let mut buf = GsubBuffer::new(
        core::mem::take(glyphs),
        mask,
        gsub.cluster_level(),
        gsub.unsafe_to_concat(),
    );
    for l in lookups {
        if let Some(lookup) = gsub.lookup_list().get(l.index) {
            let cx = GsubCx {
                gsub,
                gdef,
                flags: l.flags,
            };
            buf.set_mask_active(l.masked);
            apply_lookup_to_buffer(&cx, &lookup, &mut buf, l.alternate, budget);
        }
    }
    *glyphs = buf.into_glyphs();
}

/// Applies a single GSUB lookup by index. Mirrors HarfBuzz's
/// `apply_forward`: walks the glyph run cursor-by-cursor, and at each
/// cursor whose glyph the lookup flags keep tries the lookup's
/// subtables in spec order, taking the first subtable that matches
/// and moving the cursor where that subtable leaves it. Trying each
/// subtable across the whole run independently instead would re-fire
/// later subtables on positions an earlier one already matched (with
/// `SubstCount=0`, common in Amiri's `rlig`), producing glyph-id
/// divergences from rustybuzz on Allah / bism-Allah and other
/// Quranic-grade vocalized forms (issue #21).
///
/// Reverse-chained lookups (type 8) walk right to left instead, as in
/// HarfBuzz's `apply_backward`.
///
/// `flags` are the joiner handling and per-syllable setting of the
/// feature the lookup belongs to. Nested lookups and multiple
/// substitutions spend `budget` (see [`LookupBudget`]).
pub(super) fn apply_gsub_lookup(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    alternate_index: u16,
    flags: FeatureFlags,
    budget: &mut LookupBudget,
) {
    let Some(lookup) = gsub.lookup_list().get(lookup_idx) else {
        return;
    };
    let cx = GsubCx { gsub, gdef, flags };
    let mut buf = GsubBuffer::new(
        core::mem::take(glyphs),
        None,
        gsub.cluster_level(),
        gsub.unsafe_to_concat(),
    );
    apply_lookup_to_buffer(&cx, &lookup, &mut buf, alternate_index, budget);
    *glyphs = buf.into_glyphs();
}

/// One pass of `lookup` over `buf`, HarfBuzz's
/// `apply_string`: `apply_forward`, whose cursor stops at each glyph
/// the feature is on at and the lookup flags keep, and moves on by one
/// wherever no subtable applies, or `apply_backward` for a reverse
/// chaining lookup.
fn apply_lookup_to_buffer(
    cx: &GsubCx<'_>,
    lookup: &Lookup<'_>,
    buf: &mut GsubBuffer,
    alternate_index: u16,
    budget: &mut LookupBudget,
) {
    // Pre-parse subtables once so the cursor walk below doesn't
    // re-parse them at every position. ChainContextAny / Context /
    // Ligature parsers each allocate three or four `Vec`s for their
    // coverage / substitution arrays; doing that per cursor on a 80-
    // glyph Devanagari run is what made the bench look like a
    // quadratic explosion.
    let parsed = parse_lookup_subtables(lookup, lookup.lookup_type());
    // Run-level "would_apply" precheck. If no glyph in the run can
    // possibly trigger any subtable's primary coverage, the cursor
    // walk has nothing to do.
    if parsed.is_empty() || !lookup_might_apply(&parsed, buf.glyphs()) {
        return;
    }
    let mcx = cx.match_cx(lookup);
    if effective_type(lookup) == gsub_lt::REVERSE_CHAINED {
        apply_reverse_chain(&parsed, buf, &mcx);
        return;
    }
    // With the "digest" path the cursor only stops at glyphs in the
    // union of the subtables' primary coverages. It falls back to
    // visiting every position when a subtable's primary coverage
    // isn't a single `Coverage` table (chain-context format 1/2).
    let use_digest = parsed_has_full_digest(&parsed);
    buf.clear_output();
    while let Some(&g) = buf.cur() {
        let m = MatchGlyph::from(&g);
        if (use_digest && !cursor_in_digest(&parsed, m.id))
            || !buf.cur_in_mask()
            || mcx.filter().is_skipped(m)
        {
            buf.next_glyph();
            continue;
        }
        let (cursor, len) = (buf.cursor(), buf.len());
        let applied =
            apply_parsed_lookup_at(cx, &parsed, &mcx, buf, 0, alternate_index, false, budget);
        // A subtable that matches but produces zero substitutions
        // (common in Amiri rlig: a context with `SubstCount=0` is a
        // "no-op match" that blocks later subtables at this cursor)
        // still moves the cursor past its input. A walk that neither
        // moved nor shrank the run steps on, so it always ends.
        if !applied || (buf.cursor() <= cursor && buf.len() >= len) {
            buf.next_glyph();
        }
    }
    buf.sync();
}

/// Applies a nested GSUB lookup at the cursor, for a contextual rule's
/// lookup records. Returns `None` when the lookup did not apply. As
/// in HarfBuzz's `recurse`, the nested lookup brings its own flags but
/// keeps the outer feature's joiner handling, and the cursor glyph is
/// not checked against its flags.
///
/// `depth` is the recursion depth; we bail out at
/// [`MAX_NESTED_DEPTH`] so a pathological font loop cannot overflow
/// the stack. Every call also spends one unit of `budget` and does
/// nothing once the budget is gone.
fn apply_gsub_lookup_at(
    cx: &GsubCx<'_>,
    lookup_idx: u16,
    buf: &mut GsubBuffer,
    depth: u8,
    budget: &mut LookupBudget,
) -> Option<()> {
    if depth >= MAX_NESTED_DEPTH || !buf.has_input() || !budget.take_nested_op() {
        return None;
    }
    let lookup = cx.gsub.lookup_list().get(lookup_idx)?;
    let parsed = parse_lookup_subtables(&lookup, lookup.lookup_type());
    let mcx = cx.match_cx(&lookup);
    // Nested alternate lookups always pick index 0: feature
    // value-based selection is a top-level concept and does not
    // propagate into a recursed lookup.
    apply_parsed_lookup_at(cx, &parsed, &mcx, buf, depth, 0, true, budget).then_some(())
}

/// A GSUB contextual subtable at the cursor: matches the rule and runs
/// its nested lookups, which spend `budget`, leaving the cursor at the
/// end of the match. Returns whether a rule matched.
pub(super) fn apply_gsub_context_at(
    cx: &GsubCx<'_>,
    ctx: &GsubContext<'_>,
    mcx: &MatchContext<'_>,
    buf: &mut GsubBuffer,
    depth: u8,
    budget: &mut LookupBudget,
) -> bool {
    let at = buf.cursor();
    let mut ops = buf.take_flag_ops();
    let found = match ctx {
        GsubContext::Format1(c) => c.matches_in(&*buf, at, mcx, &mut ops),
        GsubContext::Format2(c) => c.matches_in(&*buf, at, mcx, &mut ops),
        GsubContext::Format3(c) => c
            .matches_in(&*buf, at, mcx, &mut ops)
            .map(|m| (m, c.lookups())),
    };
    buf.apply_flag_ops(ops);
    let Some((m, records)) = found else {
        return false;
    };
    apply_nested_gsub_lookups(cx, buf, m, records, depth, budget);
    true
}

/// A GSUB chained-context subtable at the cursor, like
/// [`apply_gsub_context_at`].
pub(super) fn apply_gsub_chain_context_at(
    cx: &GsubCx<'_>,
    chain: &ChainContextAny<'_>,
    mcx: &MatchContext<'_>,
    buf: &mut GsubBuffer,
    depth: u8,
    budget: &mut LookupBudget,
) -> bool {
    let at = buf.cursor();
    let mut ops = buf.take_flag_ops();
    let found = match chain {
        ChainContextAny::Format1(c) => c.matches_in(&*buf, at, mcx, &mut ops),
        ChainContextAny::Format2(c) => c.matches_in(&*buf, at, mcx, &mut ops),
        ChainContextAny::Format3(c) => {
            let found = c.matches_in(&*buf, at, mcx, &mut ops);
            buf.apply_flag_ops(ops);
            let Some(m) = found else {
                return false;
            };
            let records: Vec<SequenceLookupRecord> = c
                .substitutions()
                .iter()
                .map(|r| SequenceLookupRecord {
                    sequence_index: r.sequence_index,
                    lookup_list_index: r.lookup_list_index,
                })
                .collect();
            apply_nested_gsub_lookups(cx, buf, m, &records, depth, budget);
            return true;
        }
    };
    buf.apply_flag_ops(ops);
    let Some((m, records)) = found else {
        return false;
    };
    apply_nested_gsub_lookups(cx, buf, m, records, depth, budget);
    true
}

/// Runs a matched rule's lookup records at their match positions,
/// HarfBuzz's `apply_lookup` (see [`apply_nested`]): the cursor moves
/// to each record's glyph, the nested lookup applies there, and the
/// positions follow the length changes it makes. A record's sequence
/// index picks one of the matched input glyphs, so glyphs the rule
/// skipped never count. The cursor ends at the end of the match.
/// Records past the end of `budget` do not run.
fn apply_nested_gsub_lookups(
    cx: &GsubCx<'_>,
    buf: &mut GsubBuffer,
    mut m: InputMatch,
    records: &[SequenceLookupRecord],
    depth: u8,
    budget: &mut LookupBudget,
) {
    let run_len = buf.len();
    let end = apply_nested(&mut m.positions, m.end, run_len, records, |lookup, at| {
        if budget.exhausted() {
            return None;
        }
        buf.move_to(at);
        let before = buf.len() as isize;
        apply_gsub_lookup_at(cx, lookup, buf, depth, budget)?;
        Some(buf.len() as isize - before)
    });
    buf.move_to(end);
}

/// Writes a GSUB substitution result into `glyph`.
///
/// Besides swapping the glyph id, this clears
/// [`unicode_prop::DEFAULT_IGNORABLE`]: HarfBuzz stops hiding a
/// default-ignorable glyph once GSUB has substituted it, because the
/// font asked to draw something in its place. Every GSUB write site
/// goes through here so the zero-advance pass in [`shape`](super::shape) can trust
/// the bit.
pub(super) fn substitute_glyph(glyph: &mut Glyph, gid: u16) {
    glyph.glyph_id = u32::from(gid);
    glyph.unicode_props &= !unicode_prop::DEFAULT_IGNORABLE;
}

/// Reverse chained single substitution (GSUB type 8), HarfBuzz's
/// `apply_backward`: the cursor walks the run right to left, and at
/// every glyph the lookup flags keep, the first subtable whose context
/// matches substitutes it. Glyphs after the cursor are already
/// substituted, which is what the lookahead sees.
fn apply_reverse_chain(
    parsed: &[ParsedGsubSubtable<'_>],
    buf: &mut GsubBuffer,
    mcx: &MatchContext<'_>,
) {
    let subtables: Vec<&ReverseChain<'_>> = parsed
        .iter()
        .filter_map(|s| match s {
            ParsedGsubSubtable::ReverseChained(rc) => Some(rc),
            _ => None,
        })
        .collect();
    if subtables.is_empty() {
        return;
    }
    buf.sync();
    for i in (0..buf.len()).rev() {
        let Some(g) = buf.get(i) else {
            continue;
        };
        if !buf.in_mask_at(i) || mcx.filter().is_skipped(MatchGlyph::from(g)) {
            continue;
        }
        let mut ops = buf.take_flag_ops();
        let substitute = subtables
            .iter()
            .find_map(|rc| rc.apply_at_in(&*buf, i, mcx, &mut ops));
        buf.apply_flag_ops(ops);
        if let (Some(out), Some(glyph)) = (substitute, buf.get_mut(i)) {
            substitute_glyph(glyph, out);
        }
    }
}
