//! GPOS lookup application.
//!
//! HarfBuzz runs every GPOS feature of a shaping plan in one stage:
//! it collects the lookups of all enabled features (`rvrn`, `abvm`,
//! `blwm`, `mark`, `mkmk`, plus `curs`, `dist` and `kern` for
//! horizontal runs, plus whatever the caller enables, plus the language
//! system's required feature whatever its tag), and applies them once
//! each in lookup-list order ([`stage_lookups`], [`apply_stage`]). A
//! lookup shared by two features runs once, and the font's lookup
//! order, not the feature order, decides what runs first. `rvrn` is
//! there because HarfBuzz enables it for both tables; only GSUB gives
//! it a stage of its own.
//!
//! Each lookup walks the run the way HarfBuzz's `apply_forward` does:
//! at every position the lookup's flags do not skip, the subtables are
//! tried in order and the first one that applies wins, moving the
//! cursor where that subtable says (past a pair's second glyph when
//! the pair positions it, past a context match, or to the next glyph).
//!
//! Iteration inside a lookup (a pair's second glyph, a mark's base, a
//! cursive predecessor, contextual input, backtrack and lookahead)
//! follows HarfBuzz's skipping iterator for GPOS (see
//! [`crate::tables::layout::skip_iter`]): glyphs the lookup flags
//! ignore are passed over, and so are default-ignorable characters
//! (ZWNJ and hidden ones always, ZWJ unless the lookup belongs to
//! `mark` or `mkmk`, which HarfBuzz registers with manual joiners).
//! HarfBuzz's input walks also test each glyph's feature mask. Every
//! GPOS feature here applies to every glyph, as HarfBuzz registers
//! them all as global features, so that test always passes.

use alloc::vec::Vec;

use super::attach::{self, Attach, AttachSubtable, LookupCx};
use super::glyph_flags::FlagCx;
use super::lazy::{LazySubtables, Subtable};
use super::{
    feature_disabled, filter_for_lookup, resolve_extension, Feature, LookupBudget, VarCtx,
    MAX_NESTED_DEPTH,
};
use crate::buffer::Glyph;
use crate::ot::layout_select::PlannedLookup;
use crate::tables::gdef::Gdef;
use crate::tables::gpos::{
    lookup_type as gpos_lt, ChainContextPos, ContextPos, PairPos, SinglePos, ValueRecord,
};
use crate::tables::layout::accel::Accel;
use crate::tables::layout::skip_iter::{apply_nested as apply_records, MaySkip};
use crate::tables::layout::{
    InputMatch, Joiners, LayoutTable, Lookup, MatchContext, MatchGlyph, SequenceLookupRecord,
    SkipRules,
};
use crate::tables::Gpos;

/// GPOS features HarfBuzz enables for every run.
const COMMON_FEATURES: [[u8; 4]; 5] = [*b"rvrn", *b"abvm", *b"blwm", *b"mark", *b"mkmk"];
/// GPOS features HarfBuzz enables for horizontal runs only.
const HORIZONTAL_FEATURES: [[u8; 4]; 3] = [*b"curs", *b"dist", *b"kern"];

/// One lookup scheduled in the positioning stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StageLookup {
    /// Index into the GPOS LookupList.
    pub(super) index: u16,
    /// HarfBuzz's `auto_zwnj` / `auto_zwj`: manual for the lookups of
    /// `mark` and `mkmk`.
    pub(super) joiners: Joiners,
}

/// The lookups of every GPOS feature this run enables, sorted by
/// lookup index and deduplicated. Defaults a caller turned off with a
/// zero-valued [`Feature`] are left out; any other tag the caller
/// turns on joins the stage. `lookups_for` resolves one tag to its
/// lookup indices for the run's script.
///
/// `required` holds the lookups of the language system's required
/// feature. HarfBuzz adds those to the GPOS stage whatever their tag
/// and whether or not the caller disabled it, with automatic joiners.
pub(super) fn stage_lookups(
    features: &[Feature],
    horizontal: bool,
    required: &[u16],
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
    let mut add = |index: u16, joiners: Joiners| match out.iter_mut().find(|l| l.index == index) {
        Some(l) => l.joiners = l.joiners.and(joiners),
        None => out.push(StageLookup { index, joiners }),
    };
    for &index in required {
        add(index, Joiners::AUTO);
    }
    for tag in tags {
        if feature_disabled(features, tag) {
            continue;
        }
        let joiners = if matches!(&tag, b"mark" | b"mkmk") {
            Joiners::MANUAL
        } else {
            Joiners::AUTO
        };
        for index in lookups_for(tag) {
            add(index, joiners);
        }
    }
    out.sort_by_key(|l| l.index);
    out
}

impl StageLookup {
    /// The lookup as a stage plan keeps it.
    pub(super) fn planned(self) -> PlannedLookup {
        PlannedLookup {
            joiners: self.joiners,
            ..PlannedLookup::new(self.index)
        }
    }

    /// The lookup a stage plan kept.
    pub(super) fn from_planned(p: &PlannedLookup) -> Self {
        Self {
            index: p.index,
            joiners: p.joiners,
        }
    }
}

/// Shared inputs of every lookup in the stage.
pub(super) struct GposCx<'a> {
    pub(super) gpos: &'a Gpos<'a>,
    pub(super) gdef: Option<&'a Gdef<'a>>,
    pub(super) var: &'a VarCtx<'a>,
    /// The shaping call's glyph flag settings.
    pub(super) flags: FlagCx,
}

/// Applies `lookups` in order across `glyphs`. Nested lookup calls
/// spend `budget` (see [`LookupBudget`]).
pub(super) fn apply_stage(
    cx: &GposCx<'_>,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    lookups: &[StageLookup],
    budget: &mut LookupBudget,
) {
    if glyphs.is_empty() {
        return;
    }
    // Positioning never changes glyph ids or props, so one snapshot
    // serves every match of the stage.
    let run: Vec<MatchGlyph> = glyphs.iter().map(MatchGlyph::from).collect();
    for l in lookups {
        apply_lookup(cx, glyphs, att, &run, *l, budget);
    }
}

/// HarfBuzz's skipping iterator over the glyphs themselves, for
/// iteration that accepts any glyph (no match function): a glyph is
/// passed over when the walk's rules skip it or it is a default
/// ignorable they let go.
pub(super) struct Skipper<'r> {
    rules: SkipRules<'r>,
}

impl<'r> Skipper<'r> {
    pub(super) const fn new(rules: SkipRules<'r>) -> Self {
        Self { rules }
    }

    /// True when iteration passes over `g`.
    pub(super) fn skips(&self, g: &Glyph) -> bool {
        self.rules.may_skip(MatchGlyph::from(g)) != MaySkip::No
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

impl<'a> Subtable<'a> for PosSubtable<'a> {
    fn parse(lookup: &Lookup<'a>, index: u16) -> Option<Self> {
        parse_subtable(lookup, index)
    }

    fn reads_rule_set_digests(&self) -> bool {
        matches!(
            self,
            Self::Context(ContextPos::Format2(_)) | Self::Chain(ChainContextPos::Format2(_))
        )
    }
}

/// A GPOS lookup's subtables, each parsed the first time a glyph
/// reaches it (see the `lazy` module).
type PosSubtables<'a> = LazySubtables<'a, PosSubtable<'a>>;

/// Parses subtable `sub_idx` of `lookup`, unwrapping an Extension.
/// `None` for a subtable that fails to parse, which the shaper drops.
fn parse_subtable<'a>(lookup: &Lookup<'a>, sub_idx: u16) -> Option<PosSubtable<'a>> {
    let bytes = lookup.subtable_bytes(sub_idx)?;
    let raw_lt = lookup.lookup_type();
    let (lt, inner) = if raw_lt == gpos_lt::EXTENSION {
        resolve_extension(bytes)?
    } else {
        (raw_lt, bytes)
    };
    match lt {
        gpos_lt::SINGLE_ADJUSTMENT => SinglePos::parse(inner)
            .ok()
            .map(|s| PosSubtable::Single(s, inner)),
        gpos_lt::PAIR_ADJUSTMENT => PairPos::parse(inner).ok().map(PosSubtable::Pair),
        gpos_lt::CONTEXT => ContextPos::parse(inner).ok().map(PosSubtable::Context),
        gpos_lt::CHAINED_CONTEXT => ChainContextPos::parse(inner).ok().map(PosSubtable::Chain),
        lt => AttachSubtable::parse(lt, inner).map(PosSubtable::Attach),
    }
}

/// Per-lookup state shared by every subtable of one lookup: its flags,
/// its feature's joiner handling, and its index in the LookupList.
struct LookupState<'a> {
    mcx: MatchContext<'a>,
    index: u16,
}

impl<'a> LookupState<'a> {
    fn new(lookup: &Lookup<'a>, gdef: Option<&'a Gdef<'a>>, joiners: Joiners, index: u16) -> Self {
        Self {
            mcx: MatchContext::new(filter_for_lookup(lookup, gdef), LayoutTable::Gpos, joiners),
            index,
        }
    }
}

/// Walks the run once with one lookup, HarfBuzz's `apply_forward`.
fn apply_lookup(
    cx: &GposCx<'_>,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    run: &[MatchGlyph],
    stage: StageLookup,
    budget: &mut LookupBudget,
) {
    let Some(lookup) = cx.gpos.lookup_list().get(stage.index) else {
        return;
    };
    // A lookup none of whose subtables can start at a glyph of the run
    // changes nothing, and HarfBuzz skips it; positioning never changes
    // glyph ids, so the run's ids decide for the whole walk.
    let accel = cx.gpos.lookup_accel(stage.index, &lookup);
    if !accel.may_apply(run.iter().map(|g| g.id)) {
        return;
    }
    let mut subtables = LazySubtables::new(lookup);
    let state = LookupState::new(&lookup, cx.gdef, stage.joiners, stage.index);
    // Each lookup starts with no remembered mark base, as in HarfBuzz.
    att.reset_base_cache();
    let mut i = 0;
    while i < glyphs.len() {
        // A glyph the lookup flags skip, or one no subtable can start
        // at (every subtable first looks it up in its coverage), is
        // passed over.
        if run
            .get(i)
            .is_some_and(|&g| !accel.may_have_cheaply(g.id) || state.mcx.filter().is_skipped(g))
        {
            i += 1;
            continue;
        }
        let at = i;
        i = match apply_subtables_at(
            cx,
            &mut subtables,
            &accel,
            &state,
            glyphs,
            att,
            run,
            at,
            0,
            budget,
        ) {
            Some(next) => next.max(at + 1),
            None => at + 1,
        };
    }
}

/// Applies one lookup at position `at` only, for a contextual
/// lookup's nested records. As in HarfBuzz the glyph's properties are
/// not checked against the nested lookup's flags; its subtables just
/// try to apply, with the outer feature's joiner handling. Every call
/// spends one unit of `budget` and does nothing once the budget is
/// gone.
#[allow(clippy::too_many_arguments)]
fn apply_lookup_at(
    cx: &GposCx<'_>,
    lookup_index: u16,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    run: &[MatchGlyph],
    at: usize,
    joiners: Joiners,
    depth: u8,
    budget: &mut LookupBudget,
) {
    if depth >= MAX_NESTED_DEPTH || at >= glyphs.len() || !budget.take_nested_op() {
        return;
    }
    let Some(lookup) = cx.gpos.lookup_list().get(lookup_index) else {
        return;
    };
    let accel = cx.gpos.lookup_accel(lookup_index, &lookup);
    // No subtable can start at the glyph: nothing to parse or apply.
    if !accel.may_have_cheaply(glyphs[at].glyph_id as u16) {
        return;
    }
    let mut subtables = LazySubtables::new(lookup);
    let state = LookupState::new(&lookup, cx.gdef, joiners, lookup_index);
    let (state, accel) = (&state, &accel);
    apply_subtables_at(
        cx,
        &mut subtables,
        accel,
        state,
        glyphs,
        att,
        run,
        at,
        depth,
        budget,
    );
}

/// Tries each subtable at `at` in order. Returns where the cursor goes
/// next when one applies, `None` when none does. A subtable `accel`
/// shows cannot cover the glyph at `at` is passed over unparsed: it
/// starts by looking the glyph up in that coverage, so it would not
/// have applied.
#[allow(clippy::too_many_arguments)]
fn apply_subtables_at(
    cx: &GposCx<'_>,
    subtables: &mut PosSubtables<'_>,
    accel: &Accel<'_, '_>,
    state: &LookupState<'_>,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    run: &[MatchGlyph],
    at: usize,
    depth: u8,
    budget: &mut LookupBudget,
) -> Option<usize> {
    let horizontal = att.direction.is_horizontal();
    let id = glyphs.get(at)?.glyph_id as u16;
    // The walk only stops at glyphs the lookup's digest admits, so a
    // lookup without digests of its own subtables admits all of them.
    let every = accel.subtables_rule_out_nothing_more();
    for index in 0..subtables.len() {
        let admits = |parsed| every || accel.subtable_may_start(parsed, usize::from(index), id);
        let Some((sub, digests)) = subtables.get_admitted(index, admits) else {
            continue;
        };
        let mcx = &state.mcx.with_rule_set_digests(digests);
        let next = match sub {
            PosSubtable::Single(sp, base) => {
                let glyph = glyphs.get_mut(at)?;
                sp.adjustment(glyph.glyph_id as u16).map(|v| {
                    apply_value(glyph, &v, base, cx.var, horizontal);
                    at + 1
                })
            }
            PosSubtable::Pair(pp) => {
                apply_pair(pp, state, glyphs, at, cx.var, cx.flags, horizontal)
            }
            PosSubtable::Attach(sub) => {
                let lcx = LookupCx::new(mcx.input(), cx.var, state.index);
                attach::apply_at(sub, glyphs, att, &lcx, at).then_some(at + 1)
            }
            PosSubtable::Context(ctx) => {
                let found = match ctx {
                    ContextPos::Format1(c) => {
                        c.matches_in(run, at, mcx, &mut cx.flags.sink(glyphs))
                    }
                    ContextPos::Format2(c) => {
                        c.matches_in(run, at, mcx, &mut cx.flags.sink(glyphs))
                    }
                    ContextPos::Format3(c) => c
                        .matches_in(run, at, mcx, &mut cx.flags.sink(glyphs))
                        .map(|m| (m, c.lookups())),
                };
                // A rule set that does not match leaves the later
                // subtables their turn.
                found.map(|(m, records)| {
                    apply_nested(cx, state, glyphs, att, run, m, records, depth, budget)
                })
            }
            PosSubtable::Chain(chain) => {
                let found = match chain {
                    ChainContextPos::Format1(c) => {
                        c.matches_in(run, at, mcx, &mut cx.flags.sink(glyphs))
                    }
                    ChainContextPos::Format2(c) => {
                        c.matches_in(run, at, mcx, &mut cx.flags.sink(glyphs))
                    }
                    ChainContextPos::Format3(c) => c
                        .matches_in(run, at, mcx, &mut cx.flags.sink(glyphs))
                        .map(|m| (m, c.lookups())),
                };
                // A rule set that does not match leaves the later
                // subtables their turn.
                found.map(|(m, records)| {
                    apply_nested(cx, state, glyphs, att, run, m, records, depth, budget)
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
///
/// As in HarfBuzz, the first glyph's coverage is checked before the
/// search: a long run of glyphs the walk skips would otherwise be
/// scanned again from every glyph in it.
fn apply_pair(
    pp: &PairPos<'_>,
    state: &LookupState<'_>,
    glyphs: &mut [Glyph],
    at: usize,
    var: &VarCtx<'_>,
    flags: FlagCx,
    horizontal: bool,
) -> Option<usize> {
    let first = glyphs.get(at)?.glyph_id as u16;
    if !pp.covers(first) {
        return None;
    }
    let Some(j) = Skipper::new(state.mcx.input()).next(glyphs, at + 1) else {
        flags.unsafe_to_concat(glyphs, at, glyphs.len());
        return None;
    };
    let second = glyphs[j].glyph_id as u16;
    let Some((v1, v2, base)) = pp.lookup_with_device_base(first, second) else {
        flags.unsafe_to_concat(glyphs, at, j + 1);
        return None;
    };
    let (left, right) = glyphs.split_at_mut(j);
    apply_value(&mut left[at], &v1, base, var, horizontal);
    apply_value(&mut right[0], &v2, base, var, horizontal);
    // HarfBuzz's PairSet::apply and PairPosFormat2::apply: a pair that
    // moved something is unsafe to break, any other is unsafe to
    // concatenate, and with a second value record the glyph after the
    // pair joins the range (HarfBuzz issue 3824).
    if moves(&v1, var, horizontal) || moves(&v2, var, horizontal) {
        flags.unsafe_to_break(glyphs, at, j + 1);
    } else {
        flags.unsafe_to_concat(glyphs, at, j + 1);
    }
    if pp.value_format2() != 0 {
        flags.unsafe_to_break(glyphs, at, j + 2);
        Some(j + 1)
    } else {
        Some(j)
    }
}

/// HarfBuzz's `ValueFormat::apply_value` result: whether the record
/// holds a nonzero value it reads (the advance of the run's axis
/// only), or a device offset it reads, which it does only for a
/// variable font at non-default coordinates here.
fn moves(v: &ValueRecord, var: &VarCtx<'_>, horizontal: bool) -> bool {
    let advance = if horizontal { v.x_advance } else { v.y_advance };
    let advance_device = if horizontal {
        v.x_advance_device_off
    } else {
        v.y_advance_device_off
    };
    v.x_placement != 0
        || v.y_placement != 0
        || advance != 0
        || (var.is_active()
            && (v.x_placement_device_off != 0
                || v.y_placement_device_off != 0
                || advance_device != 0))
}

/// Dispatches a contextual match's nested lookup records at the
/// matched input positions (HarfBuzz's `apply_lookup`; positioning
/// never changes the run's length). Returns where the walk continues:
/// the end of the match. Records past the end of `budget` do not run.
#[allow(clippy::too_many_arguments)]
fn apply_nested(
    cx: &GposCx<'_>,
    state: &LookupState<'_>,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    run: &[MatchGlyph],
    mut m: InputMatch,
    records: &[SequenceLookupRecord],
    depth: u8,
    budget: &mut LookupBudget,
) -> usize {
    let joiners = state.mcx.joiners();
    let len = glyphs.len();
    apply_records(&mut m.positions, m.end, len, records, |lookup, at| {
        if budget.exhausted() {
            return None;
        }
        apply_lookup_at(cx, lookup, glyphs, att, run, at, joiners, depth + 1, budget);
        Some(0)
    })
}

/// Applies one ValueRecord to `glyph`, HarfBuzz's
/// `ValueFormat::apply_value`: placements always apply; the x advance
/// only in horizontal runs and the y advance only in vertical ones,
/// negated there because font space grows upward while vertical
/// advances run downward. Device / VariationIndex deltas follow the
/// same rules; `base` is the table their offsets are measured from.
///
/// The sums saturate: a font that stacks thousands of adjustments on
/// one glyph pins the position at the `i32` bounds.
pub(super) fn apply_value(
    glyph: &mut Glyph,
    v: &ValueRecord,
    base: &[u8],
    var: &VarCtx<'_>,
    horizontal: bool,
) {
    let delta =
        |value: i16, device: u16| i32::from(value).saturating_add(var.resolve(base, device));
    glyph.x_offset = glyph
        .x_offset
        .saturating_add(delta(v.x_placement, v.x_placement_device_off));
    glyph.y_offset = glyph
        .y_offset
        .saturating_add(delta(v.y_placement, v.y_placement_device_off));
    if horizontal {
        glyph.x_advance = glyph
            .x_advance
            .saturating_add(delta(v.x_advance, v.x_advance_device_off));
    } else {
        glyph.y_advance = glyph
            .y_advance
            .saturating_sub(delta(v.y_advance, v.y_advance_device_off));
    }
}

#[cfg(test)]
mod tests;
