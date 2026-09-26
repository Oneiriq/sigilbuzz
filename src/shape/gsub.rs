//! The GSUB lookup drivers: the forward cursor walk, its masked and
//! nested variants, contextual dispatch, and the glyph edits every
//! substitution goes through.
//!
//! Matching inside a lookup (ligature components, contextual input,
//! backtrack and lookahead) follows HarfBuzz's skipping iterator, see
//! [`crate::tables::layout::skip_iter`]: glyphs the lookup flags
//! ignore are passed over, and so are default-ignorable characters the
//! rule does not name, per the feature's [`Joiners`].

use alloc::vec::Vec;

use super::gsub_parsed::{
    apply_parsed_lookup_at, cursor_in_digest, filter_for_lookup, lookup_might_apply,
    parse_lookup_subtables, parsed_has_full_digest, MatchRun, ParsedGsubSubtable,
};
use super::{lig, resolve_extension, MAX_NESTED_DEPTH};
use crate::buffer::{unicode_prop, Glyph};
use crate::tables::gdef::Gdef;
use crate::tables::gsub::{
    lookup_type as gsub_lt, ChainContextAny, Context as GsubContext, ReverseChain,
};
use crate::tables::layout::skip_iter::apply_nested;
use crate::tables::layout::{
    InputMatch, Joiners, LayoutTable, Lookup, MatchContext, SequenceLookupRecord,
};
use crate::tables::Gsub;

/// What every GSUB lookup of one pass shares: the table, the font's
/// GDEF, and the joiner handling of the feature the lookups belong to.
pub(super) struct GsubCx<'a> {
    pub(super) gsub: &'a Gsub<'a>,
    pub(super) gdef: Option<&'a Gdef<'a>>,
    pub(super) joiners: Joiners,
}

impl<'a> GsubCx<'a> {
    /// The matching context of one lookup.
    fn match_cx(&self, lookup: &Lookup<'a>) -> MatchContext<'a> {
        MatchContext::new(
            filter_for_lookup(lookup, self.gdef),
            LayoutTable::Gsub,
            self.joiners,
        )
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

/// Applies a single GSUB lookup only at positions where `mask[i]`
/// is true. Used by the Arabic positional pass: `isol` at positions
/// tagged `Isol`, `init` at `Init`, and so on, and by the Indic
/// shaper for `half`/`pref`/`pres` gating.
///
/// Per-glyph lookup types (SINGLE / MULTIPLE / ALTERNATE / LIGATURE)
/// only fire when the mask at the cursor position is true.
/// Chained-context lookups inside a positional feature run over the
/// full glyph stream. The rules' coverage already encodes their
/// positional intent.
///
/// Like [`apply_gsub_lookup`], this walks the cursor once and tries
/// the lookup's subtables in spec order, taking the first match.
pub(super) fn apply_gsub_lookup_masked(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    mask: &[bool],
    joiners: Joiners,
) {
    let Some(lookup) = gsub.lookup_list().get(lookup_idx) else {
        return;
    };
    // Chained-context inside a positional feature ignores the mask:
    // the rule itself encodes positional intent via its input coverage
    // (post-positional glyph ids tagged init/medi/fina/...). Defer to
    // the unmasked driver so the cursor walk + first-subtable-wins
    // semantics still apply.
    let lt = effective_type(&lookup);
    if lt == gsub_lt::CHAINED_CONTEXT || lt == gsub_lt::CONTEXT {
        apply_gsub_lookup(gsub, lookup_idx, glyphs, gdef, 0, joiners);
        return;
    }

    let parsed = parse_lookup_subtables(&lookup, lookup.lookup_type());
    if parsed.is_empty() {
        return;
    }
    let cx = GsubCx {
        gsub,
        gdef,
        joiners,
    };
    let mcx = cx.match_cx(&lookup);
    let mut run = MatchRun::from_glyphs(glyphs);
    let mut i = 0;
    while i < glyphs.len() {
        if !mask.get(i).copied().unwrap_or(false) || mcx.filter().is_skipped(run.get(i)) {
            i += 1;
            continue;
        }
        i = apply_parsed_lookup_at(&cx, &parsed, &mcx, glyphs, &mut run, i, 0, 0, false)
            .map_or(i + 1, |next| next.max(i + 1));
    }
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
/// `joiners` is the ZWJ/ZWNJ handling of the feature the lookup
/// belongs to.
pub(super) fn apply_gsub_lookup(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    alternate_index: u16,
    joiners: Joiners,
) {
    let Some(lookup) = gsub.lookup_list().get(lookup_idx) else {
        return;
    };
    // Pre-parse subtables once so the cursor walk below doesn't
    // re-parse them at every position. ChainContextAny / Context /
    // Ligature parsers each allocate three or four `Vec`s for their
    // coverage / substitution arrays; doing that per cursor on a 80-
    // glyph Devanagari run is what made the bench look like a
    // quadratic explosion.
    let parsed = parse_lookup_subtables(&lookup, lookup.lookup_type());
    if parsed.is_empty() {
        return;
    }
    let cx = GsubCx {
        gsub,
        gdef,
        joiners,
    };
    let mcx = cx.match_cx(&lookup);

    if effective_type(&lookup) == gsub_lt::REVERSE_CHAINED {
        apply_reverse_chain(&parsed, glyphs, &mcx);
        return;
    }

    let mut run = MatchRun::from_glyphs(glyphs);
    // Run-level "would_apply" precheck. If no glyph in the run can
    // possibly trigger any subtable's primary coverage, the cursor
    // walk has nothing to do.
    if !lookup_might_apply(&parsed, run.as_slice()) {
        return;
    }

    // Forward cursor walk: with the "digest" path the cursor only
    // stops at glyphs in the union of the subtables' primary
    // coverages. It falls back to visiting every position when a
    // subtable's primary coverage isn't a single `Coverage` table
    // (chain-context format 1/2).
    //
    // A subtable that matches but produces zero substitutions (common
    // in Amiri rlig: a context with `SubstCount=0` is intentionally a
    // "no-op match" that blocks later subtables at this cursor) still
    // moves the cursor past its input.
    let use_digest = parsed_has_full_digest(&parsed);
    let mut i = 0;
    while i < glyphs.len() {
        let g = run.get(i);
        if (use_digest && !cursor_in_digest(&parsed, g.id)) || mcx.filter().is_skipped(g) {
            i += 1;
            continue;
        }
        i = apply_parsed_lookup_at(
            &cx,
            &parsed,
            &mcx,
            glyphs,
            &mut run,
            i,
            0,
            alternate_index,
            false,
        )
        .map_or(i + 1, |next| next.max(i + 1));
    }
}

/// Applies a nested GSUB lookup at one position, for a contextual
/// rule's lookup records. Returns `None` when the lookup did not
/// apply. As in HarfBuzz's `recurse`, the nested lookup brings its own
/// flags but keeps the outer feature's joiner handling, and the glyph
/// at `at` is not checked against its flags.
///
/// `depth` is the recursion depth; we bail out at
/// [`MAX_NESTED_DEPTH`] so a pathological font loop cannot overflow
/// the stack.
fn apply_gsub_lookup_at(
    cx: &GsubCx<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    run: &mut MatchRun,
    at: usize,
    depth: u8,
) -> Option<()> {
    if depth >= MAX_NESTED_DEPTH || at >= glyphs.len() {
        return None;
    }
    let lookup = cx.gsub.lookup_list().get(lookup_idx)?;
    let parsed = parse_lookup_subtables(&lookup, lookup.lookup_type());
    let mcx = cx.match_cx(&lookup);
    // Nested alternate lookups always pick index 0: feature
    // value-based selection is a top-level concept and does not
    // propagate into a recursed lookup.
    apply_parsed_lookup_at(cx, &parsed, &mcx, glyphs, run, at, depth, 0, true).map(|_| ())
}

/// A GSUB contextual subtable at `at`: matches the rule and runs its
/// nested lookups. Returns where the walk continues (the end of the
/// match, moved by any length change the nested lookups made).
pub(super) fn apply_gsub_context_at(
    cx: &GsubCx<'_>,
    ctx: &GsubContext<'_>,
    mcx: &MatchContext<'_>,
    glyphs: &mut Vec<Glyph>,
    run: &mut MatchRun,
    at: usize,
    depth: u8,
) -> Option<usize> {
    let (m, records) = match ctx {
        GsubContext::Format1(c) => c.matches(run.as_slice(), at, mcx)?,
        GsubContext::Format2(c) => c.matches(run.as_slice(), at, mcx)?,
        GsubContext::Format3(c) => (c.matches(run.as_slice(), at, mcx)?, c.lookups()),
    };
    Some(apply_nested_gsub_lookups(
        cx, glyphs, run, m, records, depth,
    ))
}

/// A GSUB chained-context subtable at `at`, like
/// [`apply_gsub_context_at`].
pub(super) fn apply_gsub_chain_context_at(
    cx: &GsubCx<'_>,
    chain: &ChainContextAny<'_>,
    mcx: &MatchContext<'_>,
    glyphs: &mut Vec<Glyph>,
    run: &mut MatchRun,
    at: usize,
    depth: u8,
) -> Option<usize> {
    let (m, records) = match chain {
        ChainContextAny::Format1(c) => c.matches(run.as_slice(), at, mcx)?,
        ChainContextAny::Format2(c) => c.matches(run.as_slice(), at, mcx)?,
        ChainContextAny::Format3(c) => {
            let m = c.matches(run.as_slice(), at, mcx)?;
            let records: Vec<SequenceLookupRecord> = c
                .substitutions()
                .iter()
                .map(|r| SequenceLookupRecord {
                    sequence_index: r.sequence_index,
                    lookup_list_index: r.lookup_list_index,
                })
                .collect();
            return Some(apply_nested_gsub_lookups(
                cx, glyphs, run, m, &records, depth,
            ));
        }
    };
    Some(apply_nested_gsub_lookups(
        cx, glyphs, run, m, records, depth,
    ))
}

/// Runs a matched rule's lookup records at their match positions,
/// HarfBuzz's `apply_lookup` (see [`apply_nested`]). A record's
/// sequence index picks one of the matched input glyphs, so glyphs
/// the rule skipped never count, and the positions follow the length
/// changes earlier records make. Returns where the match now ends.
fn apply_nested_gsub_lookups(
    cx: &GsubCx<'_>,
    glyphs: &mut Vec<Glyph>,
    run: &mut MatchRun,
    mut m: InputMatch,
    records: &[SequenceLookupRecord],
    depth: u8,
) -> usize {
    let run_len = glyphs.len();
    apply_nested(&mut m.positions, m.end, run_len, records, |lookup, at| {
        let before = glyphs.len() as isize;
        apply_gsub_lookup_at(cx, lookup, glyphs, run, at, depth)?;
        Some(glyphs.len() as isize - before)
    })
}

/// Replaces `glyphs[at]` with the given sequence in place. Cluster
/// is copied from the original glyph so every expanded sub-glyph
/// still points back to its source codepoint. Returns the output
/// length when `seq` is non-empty, `None` on a zero-length sequence
/// (which the spec forbids but we treat as a safe no-op).
pub(super) fn expand_glyph_in_place(
    glyphs: &mut Vec<Glyph>,
    at: usize,
    seq: &[u16],
) -> Option<usize> {
    if seq.is_empty() {
        return None;
    }
    let source_cluster = glyphs[at].cluster;
    // Inherit the source glyph's shaper-internal state so Indic
    // `indic_position` and unicode-property bits survive a
    // multiple-sub split. Rustybuzz does the same via its info mask.
    let source_pos = glyphs[at].indic_position;
    substitute_glyph(&mut glyphs[at], seq[0]);
    let source_props = glyphs[at].unicode_props;
    for (i, &out_gid) in seq.iter().enumerate().skip(1) {
        let mut g = Glyph::new(u32::from(out_gid), source_cluster);
        g.unicode_props = source_props;
        g.indic_position = source_pos;
        glyphs.insert(at + i, g);
    }
    // Component numbering for GPOS mark attachment (see `lig`).
    lig::record_multiple(glyphs, at, seq.len());
    Some(seq.len())
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
    glyphs: &mut [Glyph],
    mcx: &MatchContext<'_>,
) {
    let subtables: Vec<&ReverseChain<'_>> = parsed
        .iter()
        .filter_map(|s| match s {
            ParsedGsubSubtable::ReverseChained(rc) => Some(rc),
            _ => None,
        })
        .collect();
    let mut run = MatchRun::from_glyphs(glyphs);
    for i in (0..glyphs.len()).rev() {
        if mcx.filter().is_skipped(run.get(i)) {
            continue;
        }
        let substitute = subtables
            .iter()
            .find_map(|rc| rc.apply_at(run.as_slice(), i, mcx));
        if let Some(out) = substitute {
            substitute_glyph(&mut glyphs[i], out);
            run.sync(i, &glyphs[i]);
        }
    }
}
