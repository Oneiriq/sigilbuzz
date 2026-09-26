//! The GSUB lookup drivers: the forward cursor walk, its masked and
//! nested variants, contextual dispatch, and the glyph edits every
//! substitution goes through.

use alloc::vec::Vec;

use super::gsub_parsed::{
    apply_parsed_lookup_at, cursor_in_digest, filter_for_lookup, lookup_might_apply,
    parse_lookup_subtables, parsed_has_full_digest, GlyphIds,
};
use super::{lig, resolve_extension, MAX_NESTED_DEPTH};
use crate::buffer::{unicode_prop, Glyph};
use crate::tables::gdef::Gdef;
use crate::tables::gsub::{
    lookup_type as gsub_lt, Alternate, ChainContextAny, Context as GsubContext, Ligature, Multiple,
    ReverseChain, Single,
};
use crate::tables::layout::{MatchFilter, SequenceLookupRecord};
use crate::tables::Gsub;

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
) {
    let lookup_list = gsub.lookup_list();
    let Some(lookup) = lookup_list.get(lookup_idx) else {
        return;
    };
    let raw_lt = lookup.lookup_type();
    let effective_lt = if raw_lt == gsub_lt::EXTENSION {
        lookup
            .subtable_bytes(0)
            .and_then(resolve_extension)
            .map_or(raw_lt, |(inner, _)| inner)
    } else {
        raw_lt
    };

    // Chained-context inside a positional feature ignores the mask:
    // the rule itself encodes positional intent via its input coverage
    // (post-positional glyph ids tagged init/medi/fina/...). Defer to
    // the unmasked driver so the cursor walk + first-subtable-wins
    // semantics still apply.
    if effective_lt == gsub_lt::CHAINED_CONTEXT || effective_lt == gsub_lt::CONTEXT {
        apply_gsub_lookup(gsub, lookup_idx, glyphs, gdef, 0);
        return;
    }

    let parsed = parse_lookup_subtables(&lookup, raw_lt);
    if parsed.is_empty() {
        return;
    }
    let filter = filter_for_lookup(&lookup, gdef);
    let mut ids = GlyphIds::from_glyphs(glyphs);
    let mut i = 0;
    while i < glyphs.len() {
        if !mask.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }
        let consumed =
            apply_parsed_lookup_at(gsub, &parsed, &filter, glyphs, &mut ids, gdef, i, 0, 0);
        if consumed > 0 {
            i += consumed;
        } else {
            i += 1;
        }
    }
}

/// Applies a single GSUB lookup by index. Mirrors HarfBuzz's
/// `apply_forward`: walks the glyph run cursor-by-cursor, and at each
/// cursor tries the lookup's subtables in spec order, taking the
/// first subtable that matches and advancing the cursor past the
/// consumed input window. The previous implementation walked each
/// subtable across the whole run independently. That re-fired later
/// subtables on positions that an earlier one had already matched
/// (with `SubstCount=0`, common in Amiri's `rlig`), producing
/// glyph-id divergences from rustybuzz on Allah / bism-Allah and
/// other Quranic-grade vocalized forms (issue #21).
///
/// Reverse-chained lookups (type 8) iterate right-to-left and are
/// not a per-cursor "first match" thing. They substitute coverage-
/// matched glyphs in place, with each lookup's subtables walking the
/// frozen-prefix snapshot. Detected via lookup type and dispatched
/// separately.
pub(super) fn apply_gsub_lookup(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    alternate_index: u16,
) {
    let lookup_list = gsub.lookup_list();
    let Some(lookup) = lookup_list.get(lookup_idx) else {
        return;
    };
    let raw_lt = lookup.lookup_type();
    let effective_lt = if raw_lt == gsub_lt::EXTENSION {
        // Peek at the first subtable to see what the extension wraps;
        // a lookup's subtables all share a type so this is sufficient.
        lookup
            .subtable_bytes(0)
            .and_then(resolve_extension)
            .map_or(raw_lt, |(inner, _)| inner)
    } else {
        raw_lt
    };

    if effective_lt == gsub_lt::REVERSE_CHAINED {
        // Reverse chain walks right-to-left and is in-place; keep the
        // existing per-subtable driver since cursor semantics differ
        // from forward lookups.
        for sub_idx in 0..lookup.subtable_count() {
            let Some(bytes) = lookup.subtable_bytes(sub_idx) else {
                continue;
            };
            let inner_bytes = if raw_lt == gsub_lt::EXTENSION {
                match resolve_extension(bytes) {
                    Some((_, inner)) => inner,
                    None => continue,
                }
            } else {
                bytes
            };
            let Ok(rc) = ReverseChain::parse(inner_bytes) else {
                continue;
            };
            apply_reverse_chain_subtable(&rc, glyphs);
        }
        return;
    }

    // Pre-parse subtables once so the cursor walk below doesn't
    // re-parse them at every position. ChainContextAny / Context /
    // Ligature parsers each allocate three or four `Vec`s for their
    // coverage / substitution arrays; doing that per cursor on a 80-
    // glyph Devanagari run is what made the bench look like a
    // quadratic explosion.
    let parsed = parse_lookup_subtables(&lookup, raw_lt);
    if parsed.is_empty() {
        return;
    }
    let filter = filter_for_lookup(&lookup, gdef);

    // Build the shadow glyph-id buffer once; the per-subtable
    // matchers read from it and `apply_parsed_lookup_at` keeps it in
    // sync with `glyphs` after each substitution.
    let mut ids = GlyphIds::from_glyphs(glyphs);

    // Run-level "would_apply" precheck. If no glyph in the run can
    // possibly trigger any subtable's primary coverage, the cursor
    // walk has nothing to do. Skip it. Saves the per-cursor coverage
    // probe on lookups that target glyph subsets the run never
    // contains (very common: every Indic feature dispatched against
    // a run that doesn't carry that feature's anchor consonants).
    if !lookup_might_apply(&parsed, ids.as_slice()) {
        return;
    }

    // Forward cursor walk: cursor visits only positions whose glyph
    // is in the lookup's primary coverage union. HarfBuzz calls
    // this the "digest" walk. Falls back to visiting every position
    // when at least one subtable's primary coverage isn't a single
    // `Coverage` table (chain-context format 1/2, reverse-chain).
    //
    // At each visited cursor, try every subtable in order; the first
    // one that matches consumes input and the cursor skips past it.
    // A subtable that matches but produces zero substitutions (common
    // in Amiri rlig: a context with `SubstCount=0` is intentionally a
    // "no-op match" that blocks later subtables at this cursor) still
    // advances the cursor by its input length.
    let use_digest = parsed_has_full_digest(&parsed);
    let mut i = 0;
    while i < glyphs.len() {
        if use_digest {
            let id = ids.as_slice()[i];
            if !cursor_in_digest(&parsed, id) {
                i += 1;
                continue;
            }
        }
        let consumed = apply_parsed_lookup_at(
            gsub,
            &parsed,
            &filter,
            glyphs,
            &mut ids,
            gdef,
            i,
            0,
            alternate_index,
        );
        if consumed > 0 {
            i += consumed;
        } else {
            i += 1;
        }
    }
}

/// Applies a nested GSUB lookup at one specific position in the
/// run. Returns the number of glyphs the nested lookup consumed
/// (1 for single substitution, N for ligature, 0 when the lookup
/// did not fire). Called from inside the context/chain-context
/// subtable drivers.
///
/// `depth` is the recursion depth: the caller passes `0` for its
/// first invocation and each recursive edge increments by one; we
/// bail out at [`MAX_NESTED_DEPTH`] so a pathological font loop
/// cannot overflow the stack.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn apply_gsub_lookup_at(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    ids: &mut GlyphIds,
    gdef: Option<&Gdef<'_>>,
    at: usize,
    depth: u8,
    alternate_index: u16,
) -> usize {
    if depth >= MAX_NESTED_DEPTH {
        return 0;
    }
    if at >= glyphs.len() {
        return 0;
    }
    let lookup_list = gsub.lookup_list();
    let Some(lookup) = lookup_list.get(lookup_idx) else {
        return 0;
    };
    let filter = filter_for_lookup(&lookup, gdef);
    let raw_lt = lookup.lookup_type();
    for sub_idx in 0..lookup.subtable_count() {
        let Some(bytes) = lookup.subtable_bytes(sub_idx) else {
            continue;
        };
        let (effective_lt, inner_bytes) = if raw_lt == gsub_lt::EXTENSION {
            match resolve_extension(bytes) {
                Some((inner_type, inner)) => (inner_type, inner),
                None => continue,
            }
        } else {
            (raw_lt, bytes)
        };

        match effective_lt {
            gsub_lt::SINGLE => {
                let Ok(single) = Single::parse(inner_bytes) else {
                    continue;
                };
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
            gsub_lt::MULTIPLE => {
                let Ok(m) = Multiple::parse(inner_bytes) else {
                    continue;
                };
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
            gsub_lt::ALTERNATE => {
                let Ok(alt) = Alternate::parse(inner_bytes) else {
                    continue;
                };
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
            gsub_lt::LIGATURE => {
                let Ok(ligature) = Ligature::parse(inner_bytes) else {
                    continue;
                };
                if let Some((out, positions)) =
                    ligature.apply_filtered(&ids.as_slice()[at..], &filter)
                {
                    let level = gsub.cluster_level();
                    lig::ligate(glyphs, at, &positions, out, gdef, substitute_glyph, level);
                    let span = positions.last().copied().map_or(0, |p| p + 1);
                    ids.resync(glyphs);
                    return span;
                }
            }
            gsub_lt::CONTEXT => {
                let Ok(ctx) = GsubContext::parse(inner_bytes) else {
                    continue;
                };
                let ran =
                    apply_gsub_context_at(gsub, &ctx, glyphs, ids, gdef, &filter, at, depth + 1);
                if ran > 0 {
                    return ran;
                }
            }
            gsub_lt::CHAINED_CONTEXT => {
                let Ok(chain) = ChainContextAny::parse(inner_bytes) else {
                    continue;
                };
                let ran = apply_gsub_chain_context_at(
                    gsub,
                    &chain,
                    glyphs,
                    ids,
                    gdef,
                    &filter,
                    at,
                    depth + 1,
                );
                if ran > 0 {
                    return ran;
                }
            }
            gsub_lt::REVERSE_CHAINED => {
                let Ok(rc) = ReverseChain::parse(inner_bytes) else {
                    continue;
                };
                if let Some(out) = rc.apply(ids.as_slice(), at) {
                    substitute_glyph(&mut glyphs[at], out);
                    ids.set(at, out);
                    return 1;
                }
            }
            _ => {}
        }
    }
    0
}

/// Nested dispatch for a GSUB contextual subtable at position `at`.
/// Mirrors the chain-context driver but without backtrack/lookahead
/// so the lookup fires on the input window alone.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_gsub_context_at(
    gsub: &Gsub<'_>,
    ctx: &GsubContext<'_>,
    glyphs: &mut Vec<Glyph>,
    ids: &mut GlyphIds,
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    at: usize,
    depth: u8,
) -> usize {
    // Match against the shadow `ids` slice: no per-cursor allocation.
    // Records that need to outlive the match call get cloned into a
    // small heap buffer so we can release the borrow on `ids` before
    // dispatching nested lookups (which mutate `ids` via the glyphs
    // it tracks).
    let (input_len, lookups): (usize, Vec<SequenceLookupRecord>) = {
        let id_slice = ids.as_slice();
        match ctx {
            GsubContext::Format1(c) => {
                let Some((n, lks)) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                (n, lks.to_vec())
            }
            GsubContext::Format2(c) => {
                let Some((n, lks)) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                (n, lks.to_vec())
            }
            GsubContext::Format3(c) => {
                let Some(n) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                (n, c.lookups().to_vec())
            }
        }
    };
    apply_nested_gsub_lookups(gsub, glyphs, ids, gdef, filter, at, depth, &lookups);
    input_len.max(1)
}

/// Nested dispatch for a GSUB chained-context subtable at position
/// `at`.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_gsub_chain_context_at(
    gsub: &Gsub<'_>,
    chain: &ChainContextAny<'_>,
    glyphs: &mut Vec<Glyph>,
    ids: &mut GlyphIds,
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    at: usize,
    depth: u8,
) -> usize {
    let (input_len, lookups): (usize, Vec<SequenceLookupRecord>) = {
        let id_slice = ids.as_slice();
        match chain {
            ChainContextAny::Format1(c) => {
                let Some((n, lks)) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                (n, lks.to_vec())
            }
            ChainContextAny::Format2(c) => {
                let Some((n, lks)) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                (n, lks.to_vec())
            }
            ChainContextAny::Format3(c) => {
                let Some(n) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                let lks = c
                    .substitutions()
                    .iter()
                    .map(|r| SequenceLookupRecord {
                        sequence_index: r.sequence_index,
                        lookup_list_index: r.lookup_list_index,
                    })
                    .collect();
                (n, lks)
            }
        }
    };
    apply_nested_gsub_lookups(gsub, glyphs, ids, gdef, filter, at, depth, &lookups);
    input_len.max(1)
}

/// Translates a list of sequence-lookup records against the current
/// input window and dispatches each nested lookup at the matching
/// absolute glyph position. `sequence_index` counts *unfiltered*
/// input positions, so we walk the skip-iterator `seq_idx` times
/// from `at` to find the corresponding raw index. Marks (or other
/// skipped glyphs) between matched components never appear in the
/// sequence-index space.
#[allow(clippy::too_many_arguments)]
fn apply_nested_gsub_lookups(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    ids: &mut GlyphIds,
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    at: usize,
    depth: u8,
    lookups: &[SequenceLookupRecord],
) {
    for rec in lookups {
        let seq = rec.sequence_index as usize;
        let pos = if seq == 0 {
            at
        } else {
            // Walk `seq` unfiltered steps forward from `at` over the
            // shadow id buffer. Bailing out of the walk has to happen
            // outside the inner loop so we can `return` from the outer
            // function (rather than break out of just the seq walk).
            let id_slice = ids.as_slice();
            let mut cursor = at + 1;
            let mut walked = at;
            let mut found_all = true;
            for _ in 0..seq {
                if let Some(p) = filter.next_unskipped(id_slice, cursor) {
                    walked = p;
                    cursor = p + 1;
                } else {
                    found_all = false;
                    break;
                }
            }
            if !found_all {
                return;
            }
            walked
        };
        // Nested alternate lookups always pick index 0: feature
        // value-based selection is a top-level concept and does not
        // propagate into a recursed lookup.
        apply_gsub_lookup_at(
            gsub,
            rec.lookup_list_index,
            glyphs,
            ids,
            gdef,
            pos,
            depth,
            0,
        );
    }
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

/// Reverse chained single substitution (GSUB type 8). Walks the run
/// right-to-left so a match earlier in the run does not see a
/// substituted glyph later in the run (the spec requires this).
fn apply_reverse_chain_subtable(rc: &ReverseChain<'_>, glyphs: &mut [Glyph]) {
    if glyphs.is_empty() {
        return;
    }
    // Snapshot once: type 8 only ever produces one glyph per hit so
    // we can mutate the live stream after computing the substitute
    // against the frozen prefix/suffix. Walking right-to-left means
    // the "input" glyph for position `i` uses the current state of
    // positions < i (untouched so far) and of positions > i (snapshot).
    let mut ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
    for i in (0..glyphs.len()).rev() {
        if let Some(out) = rc.apply(&ids, i) {
            substitute_glyph(&mut glyphs[i], out);
            ids[i] = out;
        }
    }
}
