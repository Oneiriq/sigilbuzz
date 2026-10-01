//! GPOS attachment: cursive (lookup type 3), mark-to-base (4),
//! mark-to-ligature (5) and mark-to-mark (6).
//!
//! # Model
//!
//! This follows HarfBuzz's two-step model instead of computing final
//! offsets while the lookups run:
//!
//! 1. **Record.** When an attachment lookup fires, the attached glyph
//!    gets the raw anchor delta in its offset (`base_anchor -
//!    mark_anchor` for marks, the cross-stream `entry - exit` delta for
//!    cursive chains) and a [`Slot`] recording what it hangs from: the
//!    attachment kind plus a relative link to the parent glyph. Cursive
//!    attachment also adjusts advances along the main direction right
//!    away, because those are direction-specific but local. A mark
//!    also adds the cross-stream offset its parent has at that moment,
//!    summed over the parent's cursive chain (HarfBuzz's
//!    `resolve_cross_offset`).
//! 2. **Resolve.** After every positioning pass (GPOS, legacy `kern`,
//!    `kerx`) and the late mark-width zeroing, [`resolve_attachments`]
//!    walks each chain from its root and turns the raw deltas into pen
//!    relative offsets: a cursive child inherits its parent's resolved
//!    cross-stream offset, and a mark inherits its parent's resolved
//!    main-direction offset and compensates for the advances between
//!    parent and child. That compensation is where direction matters.
//!    Forward runs (LTR, TTB) subtract the advances of
//!    `parent..child`. Backward runs (RTL, BTT) are still in logical
//!    order at that point and will be reversed afterwards, so they add
//!    the advances of `parent+1..=child` instead.
//!
//! Resolving once at the end, with final advances, is what makes the
//! result correct in both directions no matter which positioning ran
//! after the attachment lookup: a kern adjustment or a zeroed mark
//! advance between a base and its mark is always accounted for, and a
//! mark follows its base along the line when the base itself moves.
//! Across the line a mark keeps the position it got when it attached,
//! as in HarfBuzz 14.5.0, so a later lookup that raises or lowers the
//! base leaves the mark where it was.

use alloc::vec::Vec;

use super::glyph_flags::FlagCx;
use super::gpos::Skipper;
use super::{lig, VarCtx};
use crate::buffer::{Direction, Glyph};
use crate::tables::gpos::{
    lookup_type as gpos_lt, CursivePos, MarkAttachment, MarkBasePos, MarkLigaPos, MarkMarkPos,
};
use crate::tables::layout::{
    GlyphClasses, MatchGlyph, SkipRules, LOOKUP_FLAG_IGNORE_BASE_GLYPHS,
    LOOKUP_FLAG_IGNORE_LIGATURES, LOOKUP_FLAG_IGNORE_MARKS, LOOKUP_FLAG_RIGHT_TO_LEFT,
};

/// How a glyph hangs from its parent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum AttachKind {
    /// Not attached (a chain root, or never touched by an attachment
    /// lookup).
    #[default]
    None,
    /// Mark attachment (mark-to-base / -ligature / -mark): both offset
    /// axes follow the parent, plus the advance compensation.
    Mark,
    /// Cursive attachment: only the cross-stream axis follows the
    /// parent (y for horizontal runs, x for vertical ones).
    Cursive,
}

/// Per-glyph attachment record, the equivalent of HarfBuzz's
/// `attach_type` / `attach_chain` pair.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Slot {
    kind: AttachKind,
    /// Parent index minus this glyph's index; zero when unattached.
    chain: i32,
}

/// Attachment scratch for one glyph slice: one [`Slot`] per glyph, the
/// effective run direction (cursive attachment needs it while the
/// lookups run), and the mark base search cache.
pub(super) struct Attach<'s> {
    pub(super) direction: Direction,
    pub(super) slots: &'s mut [Slot],
    base_cache: BaseCache,
    /// The shaping call's glyph flag settings.
    pub(super) flags: FlagCx,
    /// Cursive links the cross-stream offset walks of mark attachment
    /// may still follow (see [`resolve_cross_offset`]).
    cross_steps_left: usize,
}

impl<'s> Attach<'s> {
    /// Scratch for a run in `direction` with one slot per glyph.
    pub(super) fn new(direction: Direction, slots: &'s mut [Slot], flags: FlagCx) -> Self {
        // HarfBuzz walks the whole cursive chain for every mark, which
        // a long chain of marked glyphs turns into quadratic work. The
        // walks share a budget as large as the nested lookup budget,
        // which no real text comes near.
        let cross_steps_left = slots
            .len()
            .saturating_mul(super::NESTED_OPS_PER_GLYPH)
            .max(super::NESTED_OPS_MIN);
        Self {
            direction,
            slots,
            base_cache: BaseCache::default(),
            flags,
            cross_steps_left,
        }
    }

    /// Forgets the remembered mark base. The GPOS stage calls this at
    /// the start of every lookup.
    pub(super) fn reset_base_cache(&mut self) {
        self.base_cache = BaseCache::default();
    }
}

/// HarfBuzz's `last_base` / `last_base_until` pair for mark-to-base and
/// mark-to-ligature: the base the last search found and the position
/// that search started from. The base is the nearest glyph before
/// `until` that the search accepts, or `None` when there is none, so a
/// later search only has to look at the glyphs from `until` on. A run
/// of marks after one base then costs one step per mark instead of a
/// walk back to the base for every mark, which a long mark run would
/// turn into quadratic work.
#[derive(Debug, Clone, Copy, Default)]
struct BaseCache {
    base: Option<usize>,
    until: usize,
    /// The lookup that filled the cache. A nested lookup with a
    /// different index starts over, since its flags and coverage can
    /// accept other glyphs.
    lookup: Option<u16>,
}

/// Lookup-level inputs shared by every attachment subtable of one
/// lookup.
pub(super) struct LookupCx<'c> {
    /// The lookup's GPOS input walk: its flags, its glyph classes, and
    /// its feature's joiner handling.
    rules: SkipRules<'c>,
    pub(super) var: &'c VarCtx<'c>,
    /// The lookup's index in the LookupList, which keys the mark base
    /// search cache.
    lookup_index: u16,
}

impl<'c> LookupCx<'c> {
    /// Inputs for lookup `lookup_index`, whose input walk is `rules`.
    pub(super) const fn new(rules: SkipRules<'c>, var: &'c VarCtx<'c>, lookup_index: u16) -> Self {
        Self {
            rules,
            var,
            lookup_index,
        }
    }

    /// Raw `LookupFlag`; cursive attachment reads the RightToLeft bit
    /// and mark-to-mark keeps its mark-filtering part.
    fn lookup_flag(&self) -> u16 {
        self.rules.filter().flag()
    }

    /// Where the lookup's glyph classes come from.
    fn classes(&self) -> GlyphClasses<'c> {
        self.rules.filter().classes()
    }

    /// The input walk with the lookup flags replaced by `flag`, as
    /// HarfBuzz's `set_lookup_props` does for the base searches.
    fn walk_with_flag(&self, flag: u16) -> Skipper<'c> {
        let filter = self.rules.filter().with_flag(flag);
        Skipper::new(self.rules.with_filter(filter))
    }
}

/// One parsed attachment subtable plus the bytes its anchors resolve
/// device offsets against.
pub(super) enum AttachSubtable<'a> {
    Cursive(CursivePos<'a>),
    MarkBase(MarkBasePos<'a>, &'a [u8]),
    MarkLiga(MarkLigaPos<'a>, &'a [u8]),
    MarkMark(MarkMarkPos<'a>, &'a [u8]),
}

impl<'a> AttachSubtable<'a> {
    /// Parses `bytes` as an attachment subtable of `lookup_type`
    /// (already unwrapped from any Extension). `None` for other lookup
    /// types and for subtables that fail to parse, which the caller
    /// skips exactly like the other GPOS subtable kinds.
    pub(super) fn parse(lookup_type: u16, bytes: &'a [u8]) -> Option<Self> {
        match lookup_type {
            gpos_lt::CURSIVE_ATTACHMENT => CursivePos::parse(bytes).ok().map(Self::Cursive),
            gpos_lt::MARK_TO_BASE => MarkBasePos::parse(bytes)
                .ok()
                .map(|s| Self::MarkBase(s, bytes)),
            gpos_lt::MARK_TO_LIGATURE => MarkLigaPos::parse(bytes)
                .ok()
                .map(|s| Self::MarkLiga(s, bytes)),
            gpos_lt::MARK_TO_MARK => MarkMarkPos::parse(bytes)
                .ok()
                .map(|s| Self::MarkMark(s, bytes)),
            _ => None,
        }
    }
}

/// Fresh attachment slots for a run of `len` glyphs.
pub(super) fn new_slots(len: usize) -> Vec<Slot> {
    alloc::vec![Slot::default(); len]
}

/// Applies one attachment lookup across `glyphs`, HarfBuzz style: the
/// run is walked position by position, and at each position the
/// lookup's subtables are tried in order until one attaches. Glyphs
/// the lookup flags skip are left alone. The shaper walks lookups in
/// `gpos`; this standalone walker drives the unit tests.
#[cfg(test)]
pub(super) fn apply_lookup(
    subtables: &[AttachSubtable<'_>],
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    cx: &LookupCx<'_>,
) {
    for i in 0..glyphs.len() {
        if cx.rules.filter().is_skipped(MatchGlyph::from(&glyphs[i])) {
            continue;
        }
        for sub in subtables {
            if apply_at(sub, glyphs, att, cx, i) {
                break;
            }
        }
    }
}

/// Tries one attachment subtable on the glyph at `at`. Returns `true`
/// when it attached, so the caller stops trying later subtables.
///
/// As in HarfBuzz, the glyph at `at` only has to be in the subtable's
/// mark (or cursive) coverage; its GDEF class is not checked.
pub(super) fn apply_at(
    sub: &AttachSubtable<'_>,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    cx: &LookupCx<'_>,
    at: usize,
) -> bool {
    if at >= glyphs.len() || at >= att.slots.len() {
        return false;
    }
    let mark_gid = glyphs[at].glyph_id as u16;
    match sub {
        AttachSubtable::Cursive(cp) => apply_cursive(cp, glyphs, att, cx, at),
        AttachSubtable::MarkBase(mbp, bytes) => {
            if !mbp.covers_mark(mark_gid) {
                return false;
            }
            // HarfBuzz issue 4124: a glyph the multiple-substitution
            // rule rejects still serves as the base when the subtable
            // covers it.
            let classes = cx.classes();
            let Some(base) = find_base(glyphs, at, cx, &mut att.base_cache, |j| {
                accepts_as_base(glyphs, j, &classes) || mbp.covers_base(glyphs[j].glyph_id as u16)
            }) else {
                att.flags.unsafe_to_concat(glyphs, 0, at + 1);
                return false;
            };
            let base_gid = glyphs[base].glyph_id as u16;
            if !mbp.covers_base(base_gid) {
                att.flags.unsafe_to_concat(glyphs, base, at + 1);
                return false;
            }
            let Some(pair) = mbp.attach(mark_gid, base_gid) else {
                return false;
            };
            att.flags.unsafe_to_break(glyphs, base, at + 1);
            attach_mark(glyphs, att, at, base, &pair, bytes, cx.var);
            true
        }
        AttachSubtable::MarkLiga(mlp, bytes) => {
            if !mlp.covers_mark(mark_gid) {
                return false;
            }
            let Some(lig) = find_base(glyphs, at, cx, &mut att.base_cache, |_| true) else {
                att.flags.unsafe_to_concat(glyphs, 0, at + 1);
                return false;
            };
            let lig_gid = glyphs[lig].glyph_id as u16;
            let Some(comp_count) = mlp.component_count(lig_gid).filter(|&n| n > 0) else {
                att.flags.unsafe_to_concat(glyphs, lig, at + 1);
                return false;
            };
            // A mark that was inside this ligature when it formed
            // carries the ligature's id and the component it followed;
            // any other mark goes on the last component.
            let lig_id = lig::lig_id(&glyphs[lig]);
            let mark_comp = u16::from(lig::lig_comp(&glyphs[at]));
            let same_ligature = lig_id != 0 && lig_id == lig::lig_id(&glyphs[at]) && mark_comp > 0;
            let component = if same_ligature {
                mark_comp.min(comp_count)
            } else {
                comp_count
            } - 1;
            let Some(pair) = mlp.attach(mark_gid, lig_gid, component) else {
                return false;
            };
            att.flags.unsafe_to_break(glyphs, lig, at + 1);
            attach_mark(glyphs, att, at, lig, &pair, bytes, cx.var);
            true
        }
        AttachSubtable::MarkMark(mmp, bytes) => {
            if !mmp.covers_mark1(mark_gid) {
                return false;
            }
            // The previous glyph the lookup's mark filtering keeps,
            // with the ignore-base / -ligature / -mark flags dropped:
            // it must be a mark, or there is nothing to stack on.
            let flag = cx.lookup_flag()
                & !(LOOKUP_FLAG_IGNORE_BASE_GLYPHS
                    | LOOKUP_FLAG_IGNORE_LIGATURES
                    | LOOKUP_FLAG_IGNORE_MARKS);
            let Some(prev) = cx.walk_with_flag(flag).prev(glyphs, at) else {
                att.flags.unsafe_to_concat(glyphs, 0, at + 1);
                return false;
            };
            let prev_gid = glyphs[prev].glyph_id as u16;
            if !is_mark(&glyphs[prev], &cx.classes())
                || !marks_share_a_component(glyphs, at, prev)
                || !mmp.covers_mark2(prev_gid)
            {
                att.flags.unsafe_to_concat(glyphs, prev, at + 1);
                return false;
            }
            let Some(pair) = mmp.attach(mark_gid, prev_gid) else {
                return false;
            };
            att.flags.unsafe_to_break(glyphs, prev, at + 1);
            attach_mark(glyphs, att, at, prev, &pair, bytes, cx.var);
            true
        }
    }
}

/// True when the glyph is a mark, by GDEF or, for fonts without GDEF
/// glyph classes, by its synthesized class.
fn is_mark(g: &Glyph, classes: &GlyphClasses<'_>) -> bool {
    classes.is_mark(MatchGlyph::from(g))
}

/// The glyph a mark at `at` attaches to: the nearest earlier glyph the
/// skipping iterator stops at when it ignores marks (and, like every
/// GPOS iteration, default-ignorable characters), whatever the
/// lookup's own flags say. `accept` can turn a candidate down, which
/// passes over it like a mark.
///
/// `cache` carries the previous search of the same lookup forward, as
/// HarfBuzz's `last_base` does, so only the glyphs after the previous
/// start are examined.
fn find_base(
    glyphs: &[Glyph],
    at: usize,
    cx: &LookupCx<'_>,
    cache: &mut BaseCache,
    accept: impl Fn(usize) -> bool,
) -> Option<usize> {
    if cache.lookup != Some(cx.lookup_index) {
        *cache = BaseCache {
            lookup: Some(cx.lookup_index),
            ..BaseCache::default()
        };
    }
    if at < cache.until {
        // Nothing between the cached base and `until` is accepted, so
        // the cached answer holds for `at` too unless `at` is at or
        // before that base.
        match cache.base {
            Some(base) if base >= at => {
                cache.base = None;
                cache.until = 0;
            }
            base => return base,
        }
    }
    let skipper = cx.walk_with_flag(LOOKUP_FLAG_IGNORE_MARKS);
    let end = at.min(glyphs.len());
    if let Some(found) = (cache.until..end)
        .rev()
        .find(|&j| !skipper.skips(&glyphs[j]) && accept(j))
    {
        cache.base = Some(found);
    }
    cache.until = at;
    cache.base
}

/// HarfBuzz's mark-to-base `accept`: of the glyphs a multiple
/// substitution produced, a mark only attaches to the first one
/// (issue 740), unless a mark separates it from its predecessor
/// (issue 1020).
fn accepts_as_base(glyphs: &[Glyph], j: usize, classes: &GlyphClasses<'_>) -> bool {
    let g = &glyphs[j];
    if !lig::is_multiplied(g) || lig::lig_comp(g) == 0 || j == 0 {
        return true;
    }
    let prev = &glyphs[j - 1];
    is_mark(prev, classes)
        || !lig::is_multiplied(prev)
        || lig::lig_id(g) != lig::lig_id(prev)
        || lig::lig_comp(g) != lig::lig_comp(prev) + 1
}

/// HarfBuzz's mark-to-mark ligature check: two marks stack when they
/// belong to the same base, or to the same component of the same
/// ligature, or when one of them is itself a ligature of marks.
fn marks_share_a_component(glyphs: &[Glyph], mark1: usize, mark2: usize) -> bool {
    let (id1, id2) = (lig::lig_id(&glyphs[mark1]), lig::lig_id(&glyphs[mark2]));
    let (comp1, comp2) = (lig::lig_comp(&glyphs[mark1]), lig::lig_comp(&glyphs[mark2]));
    if id1 == id2 {
        id1 == 0 || comp1 == comp2
    } else {
        (id1 > 0 && comp1 == 0) || (id2 > 0 && comp2 == 0)
    }
}

/// Records a mark attachment of `mark` onto `parent`, as HarfBuzz's
/// `MarkArray::apply` (`OT/Layout/GPOS/MarkArray.hh`) does: the mark's
/// offset becomes the raw anchor delta (replacing any earlier
/// placement) plus, across the line, the parent's cross-stream offset
/// as it stands now, and its slot links to the parent for the resolve
/// pass.
fn attach_mark(
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    mark: usize,
    parent: usize,
    pair: &MarkAttachment,
    bytes: &[u8],
    var: &VarCtx<'_>,
) {
    let (mark_x, mark_y) = pair.mark_anchor.resolve(bytes, var.store, var.coords);
    let (base_x, base_y) = pair.base_anchor.resolve(bytes, var.store, var.coords);
    let base_offset = resolve_cross_offset(glyphs, att, parent);
    let g = &mut glyphs[mark];
    g.x_offset = base_x.saturating_sub(mark_x);
    g.y_offset = base_y.saturating_sub(mark_y);
    if att.direction.is_horizontal() {
        g.y_offset = g.y_offset.saturating_add(base_offset);
    } else {
        g.x_offset = g.x_offset.saturating_add(base_offset);
    }
    att.slots[mark] = Slot {
        kind: AttachKind::Mark,
        chain: parent as i32 - mark as i32,
    };
}

/// HarfBuzz's `resolve_cross_offset` (`OT/Layout/GPOS/MarkArray.hh`):
/// the cross-stream offset (y in horizontal runs, x in vertical ones)
/// of the glyph at `at` plus those of the cursive parents it hangs
/// from, as they stand now. The walk stops at the first glyph without
/// a cursive link, and once the shared step budget is spent.
fn resolve_cross_offset(glyphs: &[Glyph], att: &mut Attach<'_>, at: usize) -> i32 {
    let horizontal = att.direction.is_horizontal();
    let cross = |g: &Glyph| if horizontal { g.y_offset } else { g.x_offset };
    let len = glyphs.len().min(att.slots.len());
    let mut offset = glyphs.get(at).map_or(0, cross);
    let mut cur = at;
    while let Some(&slot) = att.slots.get(cur) {
        if slot.kind != AttachKind::Cursive || att.cross_steps_left == 0 {
            break;
        }
        let Some(parent) = linked_index(cur, slot.chain, len).filter(|_| slot.chain != 0) else {
            break;
        };
        att.cross_steps_left -= 1;
        cur = parent;
        offset = offset.saturating_add(glyphs.get(cur).map_or(0, cross));
    }
    offset
}

/// Cursive attachment of the glyph at `j` (entry) to the previous
/// glyph the lookup does not skip (exit). Mirrors HarfBuzz's
/// `CursivePosFormat1::apply`: the main-direction advances are fixed
/// up immediately (entry and exit swap roles between forward and
/// backward runs), and the cross-stream offset is recorded on the
/// child of the pair, which is the logically earlier glyph when the
/// lookup has the RightToLeft flag and the later one otherwise.
fn apply_cursive(
    cp: &CursivePos<'_>,
    glyphs: &mut [Glyph],
    att: &mut Attach<'_>,
    cx: &LookupCx<'_>,
    j: usize,
) -> bool {
    let Some(entry) = cp.entry(glyphs[j].glyph_id as u16) else {
        return false;
    };
    let Some(i) = Skipper::new(cx.rules).prev(glyphs, j) else {
        att.flags.unsafe_to_concat(glyphs, 0, j + 1);
        return false;
    };
    let Some(exit) = cp.exit(glyphs[i].glyph_id as u16) else {
        att.flags.unsafe_to_concat(glyphs, i, j + 1);
        return false;
    };
    att.flags.unsafe_to_break(glyphs, i, j + 1);
    let (exit_x, exit_y) = exit.resolve(cp.data(), cx.var.store, cx.var.coords);
    let (entry_x, entry_y) = entry.resolve(cp.data(), cx.var.store, cx.var.coords);

    // Main-direction adjustment.
    match att.direction {
        Direction::Ltr => {
            glyphs[i].x_advance = exit_x.saturating_add(glyphs[i].x_offset);
            let d = entry_x.saturating_add(glyphs[j].x_offset);
            glyphs[j].x_advance = glyphs[j].x_advance.saturating_sub(d);
            glyphs[j].x_offset = glyphs[j].x_offset.saturating_sub(d);
        }
        Direction::Rtl => {
            let d = exit_x.saturating_add(glyphs[i].x_offset);
            glyphs[i].x_advance = glyphs[i].x_advance.saturating_sub(d);
            glyphs[i].x_offset = glyphs[i].x_offset.saturating_sub(d);
            glyphs[j].x_advance = entry_x.saturating_add(glyphs[j].x_offset);
        }
        Direction::Ttb => {
            glyphs[i].y_advance = exit_y.saturating_add(glyphs[i].y_offset);
            let d = entry_y.saturating_add(glyphs[j].y_offset);
            glyphs[j].y_advance = glyphs[j].y_advance.saturating_sub(d);
            glyphs[j].y_offset = glyphs[j].y_offset.saturating_sub(d);
        }
        Direction::Btt => {
            let d = exit_y.saturating_add(glyphs[i].y_offset);
            glyphs[i].y_advance = glyphs[i].y_advance.saturating_sub(d);
            glyphs[i].y_offset = glyphs[i].y_offset.saturating_sub(d);
            // HarfBuzz sets the plain entry y here, without folding in
            // the existing offset like the other three directions do.
            glyphs[j].y_advance = entry_y;
        }
    }

    // Cross-direction adjustment: the child aligns itself against its
    // parent; the chain root stays on the baseline.
    let (mut child, mut parent) = (i, j);
    let (mut x_offset, mut y_offset) = (
        entry_x.saturating_sub(exit_x),
        entry_y.saturating_sub(exit_y),
    );
    if cx.lookup_flag() & LOOKUP_FLAG_RIGHT_TO_LEFT == 0 {
        core::mem::swap(&mut child, &mut parent);
        x_offset = x_offset.saturating_neg();
        y_offset = y_offset.saturating_neg();
    }
    let horizontal = att.direction.is_horizontal();
    // A child already chained elsewhere hands its old tree over to
    // the new parent by reversing the old links.
    reverse_cursive_minor_offset(glyphs, att.slots, child, parent, horizontal);

    let chain = parent as i32 - child as i32;
    att.slots[child] = Slot {
        kind: AttachKind::Cursive,
        chain,
    };
    if horizontal {
        glyphs[child].y_offset = y_offset;
    } else {
        glyphs[child].x_offset = x_offset;
    }
    // A parent that was attached to this child gets separated, so the
    // two never form a two-glyph cycle.
    if att.slots[parent].chain == -chain {
        att.slots[parent].chain = 0;
        if horizontal {
            glyphs[parent].y_offset = 0;
        } else {
            glyphs[parent].x_offset = 0;
        }
    }
    true
}

/// Walks the cursive chain hanging off `start` and reverses every link
/// on it (stopping at `new_parent`), negating the cross-stream offsets
/// so the old subtree now hangs from `start`'s new parent. Iterative
/// port of HarfBuzz's recursive `reverse_cursive_minor_offset`: links
/// are collected first and rewritten from the far end back, which is
/// the order the recursion unwinds in.
fn reverse_cursive_minor_offset(
    glyphs: &mut [Glyph],
    slots: &mut [Slot],
    start: usize,
    new_parent: usize,
    horizontal: bool,
) {
    let mut links: Vec<(usize, usize, Slot)> = Vec::new();
    let mut cur = start;
    loop {
        let slot = slots[cur];
        if slot.chain == 0 || slot.kind != AttachKind::Cursive {
            break;
        }
        slots[cur].chain = 0;
        let Some(next) = linked_index(cur, slot.chain, slots.len()) else {
            break;
        };
        if next == new_parent {
            break;
        }
        links.push((cur, next, slot));
        cur = next;
    }
    for &(from, to, slot) in links.iter().rev() {
        if horizontal {
            glyphs[to].y_offset = glyphs[from].y_offset.saturating_neg();
        } else {
            glyphs[to].x_offset = glyphs[from].x_offset.saturating_neg();
        }
        slots[to] = Slot {
            kind: slot.kind,
            chain: -slot.chain,
        };
    }
}

/// `index + chain` when it lands inside `0..len`.
fn linked_index(index: usize, chain: i32, len: usize) -> Option<usize> {
    let target = index as i64 + i64::from(chain);
    if (0..len as i64).contains(&target) {
        Some(target as usize)
    } else {
        None
    }
}

/// How many links one walk of [`resolve_attachments`] follows from its
/// starting glyph, HarfBuzz's `HB_MAX_NESTING_LEVEL`.
const MAX_CHAIN_DEPTH: usize = 64;

/// Final attachment pass: converts every recorded chain into pen
/// relative offsets, parents before children. Must run after all
/// advance changes (GPOS, legacy kern, kerx, mark-width zeroing) and
/// before a backward run is reversed; `direction` is the run's
/// effective direction.
///
/// Port of HarfBuzz's `GPOS::position_finish_offsets` and
/// `propagate_attachment_offsets` (`OT/Layout/GPOS/GPOS.hh`): glyphs
/// are visited from the start of the run in forward directions and
/// from its end in backward ones (HarfBuzz issue 5514), and each visit
/// resolves the glyph's ancestors first, up to [`MAX_CHAIN_DEPTH`]
/// links away. The walk is iterative, so long cursive chains cannot
/// overflow the stack, and each link is consumed once, so malformed
/// cycles terminate.
pub(super) fn resolve_attachments(glyphs: &mut [Glyph], slots: &mut [Slot], direction: Direction) {
    let len = glyphs.len().min(slots.len());
    // Running advance sums, so the compensation for the glyphs between
    // a mark and its parent costs one subtraction instead of a walk
    // over them. Advances do not change while attachments resolve.
    let advances = AdvanceSums::new(glyphs.get(..len).unwrap_or_default());
    let mut path: Vec<(usize, usize, AttachKind)> = Vec::new();
    let forward = direction.is_forward();
    for n in 0..len {
        let start = if forward { n } else { len - 1 - n };
        if slots[start].chain == 0 {
            continue;
        }
        path.clear();
        let mut cur = start;
        let mut depth_left = MAX_CHAIN_DEPTH;
        loop {
            let slot = slots[cur];
            slots[cur].chain = 0;
            let Some(parent) = linked_index(cur, slot.chain, len) else {
                break;
            };
            if depth_left == 0 {
                break;
            }
            path.push((cur, parent, slot.kind));
            if slots[parent].chain == 0 {
                break;
            }
            depth_left -= 1;
            cur = parent;
        }
        for &(child, parent, kind) in path.iter().rev() {
            propagate(glyphs, &advances, child, parent, kind, direction);
        }
    }
}

/// Prefix sums of the x and y advances of a run: `x[k]` is the sum of
/// the x advances of glyphs `0..k`. Kept in `i64` so no partial sum
/// can overflow.
struct AdvanceSums {
    x: Vec<i64>,
    y: Vec<i64>,
}

impl AdvanceSums {
    fn new(glyphs: &[Glyph]) -> Self {
        let mut x = Vec::with_capacity(glyphs.len() + 1);
        let mut y = Vec::with_capacity(glyphs.len() + 1);
        let (mut sx, mut sy) = (0i64, 0i64);
        x.push(0);
        y.push(0);
        for g in glyphs {
            sx += i64::from(g.x_advance);
            sy += i64::from(g.y_advance);
            x.push(sx);
            y.push(sy);
        }
        Self { x, y }
    }

    /// Sum of the advances of glyphs `start..end`, or zero when the
    /// range is empty or out of bounds.
    fn between(&self, start: usize, end: usize) -> (i64, i64) {
        match (
            self.x.get(start),
            self.x.get(end),
            self.y.get(start),
            self.y.get(end),
        ) {
            (Some(x0), Some(x1), Some(y0), Some(y1)) if start <= end => (x1 - x0, y1 - y0),
            _ => (0, 0),
        }
    }
}

/// Clamps an `i64` position into the `i32` range.
fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Folds the resolved position of `parent` into `child`: a cursive
/// child takes the parent's cross-stream offset, a mark the parent's
/// main-direction offset plus the advance compensation. The mark got
/// its cross-stream share when it attached.
fn propagate(
    glyphs: &mut [Glyph],
    advances: &AdvanceSums,
    child: usize,
    parent: usize,
    kind: AttachKind,
    direction: Direction,
) {
    let Some((px, py)) = glyphs.get(parent).map(|g| (g.x_offset, g.y_offset)) else {
        return;
    };
    let Some(g) = glyphs.get_mut(child) else {
        return;
    };
    match kind {
        AttachKind::None => {}
        AttachKind::Cursive => {
            if direction.is_horizontal() {
                g.y_offset = g.y_offset.saturating_add(py);
            } else {
                g.x_offset = g.x_offset.saturating_add(px);
            }
        }
        AttachKind::Mark => {
            let (mut dx, mut dy) = if direction.is_horizontal() {
                (i64::from(px), 0)
            } else {
                (0, i64::from(py))
            };
            // Marks only ever attach backwards in logical order.
            if parent < child {
                if direction.is_forward() {
                    // The pen reaches the mark after passing the
                    // parent and everything up to the mark.
                    let (ax, ay) = advances.between(parent, child);
                    dx -= ax;
                    dy -= ay;
                } else {
                    // After the final reversal the mark precedes the
                    // parent: the pen reaches the parent after the
                    // mark and everything between the two.
                    let (ax, ay) = advances.between(parent + 1, child + 1);
                    dx += ax;
                    dy += ay;
                }
            }
            g.x_offset = clamp_i32(i64::from(g.x_offset) + dx);
            g.y_offset = clamp_i32(i64::from(g.y_offset) + dy);
        }
    }
}

#[cfg(test)]
mod tests;
