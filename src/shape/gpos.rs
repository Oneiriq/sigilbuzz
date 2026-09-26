//! GPOS lookup application.
//!
//! HarfBuzz runs every GPOS feature of a shaping plan in one stage:
//! it collects the lookups of all enabled features (`abvm`, `blwm`,
//! `mark`, `mkmk`, plus `curs`, `dist` and `kern` for horizontal runs,
//! plus whatever the caller enables), and applies them once each in
//! lookup-list order ([`stage_lookups`], [`apply_stage`]). A lookup
//! shared by two features runs once, and the font's lookup order,
//! not the feature order, decides what runs first.
//!
//! Each lookup walks the run the way HarfBuzz's `apply_forward` does:
//! at every position the lookup's flags do not skip, the subtables are
//! tried in order and the first one that applies wins, moving the
//! cursor where that subtable says (past a pair's second glyph when
//! the pair positions it, past a context match, or to the next glyph).
//!
//! Iteration inside a lookup (a pair's second glyph, a mark's base, a
//! cursive predecessor) uses [`Skipper`], HarfBuzz's skipping iterator
//! for GPOS: glyphs the lookup flags ignore are passed over, and so are
//! default-ignorable characters (ZWNJ always, ZWJ unless the lookup
//! belongs to `mark` or `mkmk`, which HarfBuzz registers with manual
//! joiners).

use alloc::vec::Vec;

use super::attach::{self, Attach, AttachSubtable, LookupCx};
use super::{
    feature_disabled, filter_for_lookup, resolve_extension, Feature, VarCtx, MAX_NESTED_DEPTH,
};
use crate::buffer::{unicode_prop, Glyph};
use crate::tables::gdef::Gdef;
use crate::tables::gpos::{
    lookup_type as gpos_lt, ChainContextPos, ContextPos, PairPos, SinglePos, ValueRecord,
};
use crate::tables::layout::{Lookup, MatchFilter, SequenceLookupRecord};
use crate::tables::Gpos;

/// GPOS features HarfBuzz enables for every run.
const COMMON_FEATURES: [[u8; 4]; 4] = [*b"abvm", *b"blwm", *b"mark", *b"mkmk"];
/// GPOS features HarfBuzz enables for horizontal runs only.
const HORIZONTAL_FEATURES: [[u8; 4]; 3] = [*b"curs", *b"dist", *b"kern"];

/// One lookup scheduled in the positioning stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StageLookup {
    /// Index into the GPOS LookupList.
    pub(super) index: u16,
    /// HarfBuzz's `auto_zwj`: whether the lookup's iteration skips
    /// ZWJ. False when the lookup belongs to `mark` or `mkmk`.
    pub(super) auto_zwj: bool,
}

/// The lookups of every GPOS feature this run enables, sorted by
/// lookup index and deduplicated. Defaults a caller turned off with a
/// zero-valued [`Feature`] are left out; any other tag the caller
/// turns on joins the stage. `lookups_for` resolves one tag to its
/// lookup indices for the run's script.
pub(super) fn stage_lookups(
    features: &[Feature],
    horizontal: bool,
    mut lookups_for: impl FnMut([u8; 4]) -> Vec<u16>,
) -> Vec<StageLookup> {
    let mut tags: Vec<[u8; 4]> = COMMON_FEATURES.to_vec();
    if horizontal {
        tags.extend_from_slice(&HORIZONTAL_FEATURES);
    }
    for f in features {
        if f.value != 0 && !tags.contains(&f.tag) {
            tags.push(f.tag);
        }
    }
    let mut out: Vec<StageLookup> = Vec::new();
    for tag in tags {
        if feature_disabled(features, tag) {
            continue;
        }
        let auto_zwj = !matches!(&tag, b"mark" | b"mkmk");
        for index in lookups_for(tag) {
            match out.iter_mut().find(|l| l.index == index) {
                Some(l) => l.auto_zwj &= auto_zwj,
                None => out.push(StageLookup { index, auto_zwj }),
            }
        }
    }
    out.sort_by_key(|l| l.index);
    out
}

/// Shared inputs of every lookup in the stage.
pub(super) struct GposCx<'a> {
    pub(super) gpos: &'a Gpos<'a>,
    pub(super) gdef: Option<&'a Gdef<'a>>,
    pub(super) var: &'a VarCtx<'a>,
}

/// Applies `lookups` in order across `glyphs`.
pub(super) fn apply_stage(
    cx: &GposCx<'_>,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    lookups: &[StageLookup],
) {
    if glyphs.is_empty() {
        return;
    }
    // Positioning never changes glyph ids, so one id snapshot serves
    // every context match of the stage.
    let ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
    for l in lookups {
        apply_lookup(cx, glyphs, att, &ids, *l);
    }
}

/// HarfBuzz's skipping iterator, for iteration that accepts any glyph
/// (no match function): a glyph is passed over when the lookup flags
/// ignore it, or when it is an unsubstituted default-ignorable
/// character other than a ZWJ the lookup must see.
pub(super) struct Skipper<'f> {
    filter: &'f MatchFilter<'f>,
    ignore_zwj: bool,
}

impl<'f> Skipper<'f> {
    pub(super) const fn new(filter: &'f MatchFilter<'f>, ignore_zwj: bool) -> Self {
        Self { filter, ignore_zwj }
    }

    /// True when iteration passes over `g`.
    pub(super) fn skips(&self, g: &Glyph) -> bool {
        if self.filter.is_skipped(g.glyph_id as u16) {
            return true;
        }
        let props = g.unicode_props;
        props & unicode_prop::DEFAULT_IGNORABLE != 0
            && (self.ignore_zwj || props & unicode_prop::JOINER == 0)
    }

    /// First glyph at or after `from` that iteration stops at.
    pub(super) fn next(&self, glyphs: &[Glyph], from: usize) -> Option<usize> {
        (from..glyphs.len()).find(|&k| !self.skips(&glyphs[k]))
    }

    /// Nearest glyph before `before` that iteration stops at.
    pub(super) fn prev(&self, glyphs: &[Glyph], before: usize) -> Option<usize> {
        (0..before.min(glyphs.len()))
            .rev()
            .find(|&k| !self.skips(&glyphs[k]))
    }
}

/// One parsed GPOS subtable.
enum PosSubtable<'a> {
    /// SinglePos plus the bytes its device offsets are measured from.
    Single(SinglePos<'a>, &'a [u8]),
    Pair(PairPos<'a>),
    Attach(AttachSubtable<'a>),
    Context(ContextPos<'a>),
    Chain(ChainContextPos<'a>),
}

/// Parses a lookup's subtables, unwrapping Extension subtables and
/// dropping the ones that fail to parse.
fn parse_subtables<'a>(lookup: &Lookup<'a>) -> Vec<PosSubtable<'a>> {
    let raw_lt = lookup.lookup_type();
    let mut out = Vec::with_capacity(usize::from(lookup.subtable_count()));
    for sub_idx in 0..lookup.subtable_count() {
        let Some(bytes) = lookup.subtable_bytes(sub_idx) else {
            continue;
        };
        let (lt, inner) = if raw_lt == gpos_lt::EXTENSION {
            match resolve_extension(bytes) {
                Some(pair) => pair,
                None => continue,
            }
        } else {
            (raw_lt, bytes)
        };
        let parsed = match lt {
            gpos_lt::SINGLE_ADJUSTMENT => SinglePos::parse(inner)
                .ok()
                .map(|s| PosSubtable::Single(s, inner)),
            gpos_lt::PAIR_ADJUSTMENT => PairPos::parse(inner).ok().map(PosSubtable::Pair),
            gpos_lt::CONTEXT => ContextPos::parse(inner).ok().map(PosSubtable::Context),
            gpos_lt::CHAINED_CONTEXT => ChainContextPos::parse(inner).ok().map(PosSubtable::Chain),
            lt => AttachSubtable::parse(lt, inner).map(PosSubtable::Attach),
        };
        if let Some(p) = parsed {
            out.push(p);
        }
    }
    out
}

/// Per-lookup state shared by every subtable of one lookup.
struct LookupState<'a> {
    filter: MatchFilter<'a>,
    flag: u16,
    mark_filtering_set: Option<u16>,
    auto_zwj: bool,
}

impl<'a> LookupState<'a> {
    fn new(lookup: &Lookup<'a>, gdef: Option<&'a Gdef<'a>>, auto_zwj: bool) -> Self {
        Self {
            filter: filter_for_lookup(lookup, gdef),
            flag: lookup.flag(),
            mark_filtering_set: lookup.mark_filtering_set(),
            auto_zwj,
        }
    }
}

/// Walks the run once with one lookup, HarfBuzz's `apply_forward`.
fn apply_lookup(
    cx: &GposCx<'_>,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    ids: &[u16],
    stage: StageLookup,
) {
    let Some(lookup) = cx.gpos.lookup_list().get(stage.index) else {
        return;
    };
    let subtables = parse_subtables(&lookup);
    if subtables.is_empty() {
        return;
    }
    let state = LookupState::new(&lookup, cx.gdef, stage.auto_zwj);
    let mut i = 0;
    while i < glyphs.len() {
        if state.filter.is_skipped(glyphs[i].glyph_id as u16) {
            i += 1;
            continue;
        }
        i = match apply_subtables_at(cx, &subtables, &state, glyphs, att, ids, i, 0) {
            Some(next) => next.max(i + 1),
            None => i + 1,
        };
    }
}

/// Applies one lookup at position `at` only, for a contextual
/// lookup's nested records. As in HarfBuzz the glyph's properties are
/// not checked against the nested lookup's flags; its subtables just
/// try to apply.
#[allow(clippy::too_many_arguments)]
fn apply_lookup_at(
    cx: &GposCx<'_>,
    lookup_index: u16,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    ids: &[u16],
    at: usize,
    auto_zwj: bool,
    depth: u8,
) {
    if depth >= MAX_NESTED_DEPTH || at >= glyphs.len() {
        return;
    }
    let Some(lookup) = cx.gpos.lookup_list().get(lookup_index) else {
        return;
    };
    let subtables = parse_subtables(&lookup);
    let state = LookupState::new(&lookup, cx.gdef, auto_zwj);
    apply_subtables_at(cx, &subtables, &state, glyphs, att, ids, at, depth);
}

/// Tries each subtable at `at` in order. Returns where the cursor goes
/// next when one applies, `None` when none does.
#[allow(clippy::too_many_arguments)]
fn apply_subtables_at(
    cx: &GposCx<'_>,
    subtables: &[PosSubtable<'_>],
    state: &LookupState<'_>,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    ids: &[u16],
    at: usize,
    depth: u8,
) -> Option<usize> {
    let horizontal = att.direction.is_horizontal();
    for sub in subtables {
        let next = match sub {
            PosSubtable::Single(sp, base) => sp.adjustment(glyphs[at].glyph_id as u16).map(|v| {
                apply_value(&mut glyphs[at], &v, base, cx.var, horizontal);
                at + 1
            }),
            PosSubtable::Pair(pp) => apply_pair(pp, state, glyphs, at, cx.var, horizontal),
            PosSubtable::Attach(sub) => {
                let lcx = LookupCx {
                    gdef: cx.gdef,
                    filter: &state.filter,
                    lookup_flag: state.flag,
                    mark_filtering_set: state.mark_filtering_set,
                    ignore_zwj: state.auto_zwj,
                    var: cx.var,
                };
                attach::apply_at(sub, glyphs, att, &lcx, at).then_some(at + 1)
            }
            PosSubtable::Context(ctx) => {
                let matched = match ctx {
                    ContextPos::Format1(c) => c
                        .matches_filtered(ids, at, &state.filter)
                        .map(|(n, l)| (n, l.to_vec())),
                    ContextPos::Format2(c) => c
                        .matches_filtered(ids, at, &state.filter)
                        .map(|(n, l)| (n, l.to_vec())),
                    ContextPos::Format3(c) => c
                        .matches_filtered(ids, at, &state.filter)
                        .map(|n| (n, c.lookups().to_vec())),
                };
                matched.map(|(len, records)| {
                    apply_nested(cx, state, glyphs, att, ids, at, depth, &records);
                    at + len.max(1)
                })
            }
            PosSubtable::Chain(chain) => {
                let matched = match chain {
                    ChainContextPos::Format1(c) => c
                        .matches_filtered(ids, at, &state.filter)
                        .map(|(n, l)| (n, l.to_vec())),
                    ChainContextPos::Format2(c) => c
                        .matches_filtered(ids, at, &state.filter)
                        .map(|(n, l)| (n, l.to_vec())),
                    ChainContextPos::Format3(c) => c
                        .matches_filtered(ids, at, &state.filter)
                        .map(|n| (n, c.lookups().to_vec())),
                };
                matched.map(|(len, records)| {
                    apply_nested(cx, state, glyphs, att, ids, at, depth, &records);
                    at + len.max(1)
                })
            }
        };
        if next.is_some() {
            return next;
        }
    }
    None
}

/// PairPos at `at`: finds the second glyph with the skipping iterator
/// and applies the pair's records. HarfBuzz leaves the cursor on the
/// second glyph so it can start the next pair, unless the subtable's
/// `valueFormat2` is nonzero, in which case it moves past it.
fn apply_pair(
    pp: &PairPos<'_>,
    state: &LookupState<'_>,
    glyphs: &mut [Glyph],
    at: usize,
    var: &VarCtx<'_>,
    horizontal: bool,
) -> Option<usize> {
    let skipper = Skipper::new(&state.filter, state.auto_zwj);
    let j = skipper.next(glyphs, at + 1)?;
    let first = glyphs[at].glyph_id as u16;
    let second = glyphs[j].glyph_id as u16;
    let (v1, v2, base) = pp.lookup_with_device_base(first, second)?;
    let (left, right) = glyphs.split_at_mut(j);
    apply_value(&mut left[at], &v1, base, var, horizontal);
    apply_value(&mut right[0], &v2, base, var, horizontal);
    Some(if pp.value_format2() != 0 { j + 1 } else { j })
}

/// Dispatches a contextual match's nested lookup records. A record's
/// sequence index counts the positions the lookup flags keep, so it
/// is walked with the lookup's filter from the match start.
#[allow(clippy::too_many_arguments)]
fn apply_nested(
    cx: &GposCx<'_>,
    state: &LookupState<'_>,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    ids: &[u16],
    at: usize,
    depth: u8,
    records: &[SequenceLookupRecord],
) {
    for rec in records {
        let mut pos = at;
        for _ in 0..rec.sequence_index {
            match state.filter.next_unskipped(ids, pos + 1) {
                Some(p) => pos = p,
                None => return,
            }
        }
        apply_lookup_at(
            cx,
            rec.lookup_list_index,
            glyphs,
            att,
            ids,
            pos,
            state.auto_zwj,
            depth + 1,
        );
    }
}

/// Applies one ValueRecord to `glyph`, HarfBuzz's
/// `ValueFormat::apply_value`: placements always apply; the x advance
/// only in horizontal runs and the y advance only in vertical ones,
/// negated there because font space grows upward while vertical
/// advances run downward. Device / VariationIndex deltas follow the
/// same rules; `base` is the table their offsets are measured from.
pub(super) fn apply_value(
    glyph: &mut Glyph,
    v: &ValueRecord,
    base: &[u8],
    var: &VarCtx<'_>,
    horizontal: bool,
) {
    glyph.x_offset += i32::from(v.x_placement) + var.resolve(base, v.x_placement_device_off);
    glyph.y_offset += i32::from(v.y_placement) + var.resolve(base, v.y_placement_device_off);
    if horizontal {
        glyph.x_advance += i32::from(v.x_advance) + var.resolve(base, v.x_advance_device_off);
    } else {
        glyph.y_advance -= i32::from(v.y_advance) + var.resolve(base, v.y_advance_device_off);
    }
}

#[cfg(test)]
mod tests;
