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
//!    away, because those are direction-specific but local.
//! 2. **Resolve.** After every positioning pass (GPOS, legacy `kern`,
//!    `kerx`) and the late mark-width zeroing, [`resolve_attachments`]
//!    walks each chain from its root and turns the raw deltas into pen
//!    relative offsets: a child inherits its parent's resolved offset
//!    and, for marks, compensates for the advances between parent and
//!    child. That compensation is where direction matters. Forward runs
//!    (LTR, TTB) subtract the advances of `parent..child`; backward runs
//!    (RTL, BTT) are still in logical order at that point and will be
//!    reversed afterwards, so they add the advances of
//!    `parent+1..=child` instead.
//!
//! Resolving once at the end, with final advances, is what makes the
//! result correct in both directions no matter which positioning ran
//! after the attachment lookup: a kern adjustment or a zeroed mark
//! advance between a base and its mark is always accounted for, and a
//! mark follows its base when the base itself moves (kerning
//! placement, cursive chains).

use alloc::vec::Vec;

use super::gpos::Skipper;
use super::{lig, VarCtx};
use crate::buffer::{Direction, Glyph};
use crate::tables::gdef::{Gdef, GlyphClass};
use crate::tables::gpos::{
    lookup_type as gpos_lt, CursivePos, MarkAttachment, MarkBasePos, MarkLigaPos, MarkMarkPos,
};
use crate::tables::layout::{
    MatchFilter, LOOKUP_FLAG_IGNORE_BASE_GLYPHS, LOOKUP_FLAG_IGNORE_LIGATURES,
    LOOKUP_FLAG_IGNORE_MARKS, LOOKUP_FLAG_RIGHT_TO_LEFT,
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

/// Attachment scratch for one glyph slice: one [`Slot`] per glyph plus
/// the effective run direction (cursive attachment needs it while the
/// lookups run).
pub(super) struct Attach<'s> {
    pub(super) direction: Direction,
    pub(super) slots: &'s mut [Slot],
}

/// Lookup-level inputs shared by every attachment subtable of one
/// lookup.
pub(super) struct LookupCx<'c> {
    pub(super) gdef: Option<&'c Gdef<'c>>,
    pub(super) filter: &'c MatchFilter<'c>,
    /// Raw `LookupFlag`; cursive attachment reads the RightToLeft bit
    /// and mark-to-mark keeps its mark-filtering part.
    pub(super) lookup_flag: u16,
    /// The lookup's mark filtering set index, when it has one.
    pub(super) mark_filtering_set: Option<u16>,
    /// Whether iteration passes over ZWJ (HarfBuzz's `auto_zwj`).
    pub(super) ignore_zwj: bool,
    pub(super) var: &'c VarCtx<'c>,
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
        if cx.filter.is_skipped(glyphs[i].glyph_id as u16) {
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
            let Some(base) = find_base(glyphs, at, cx, |j| {
                accepts_as_base(glyphs, j, cx.gdef) || mbp.covers_base(glyphs[j].glyph_id as u16)
            }) else {
                return false;
            };
            let Some(pair) = mbp.attach(mark_gid, glyphs[base].glyph_id as u16) else {
                return false;
            };
            attach_mark(glyphs, att, at, base, &pair, bytes, cx.var);
            true
        }
        AttachSubtable::MarkLiga(mlp, bytes) => {
            if !mlp.covers_mark(mark_gid) {
                return false;
            }
            let Some(lig) = find_base(glyphs, at, cx, |_| true) else {
                return false;
            };
            let lig_gid = glyphs[lig].glyph_id as u16;
            let Some(comp_count) = mlp.component_count(lig_gid).filter(|&n| n > 0) else {
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
            let flag = cx.lookup_flag
                & !(LOOKUP_FLAG_IGNORE_BASE_GLYPHS
                    | LOOKUP_FLAG_IGNORE_LIGATURES
                    | LOOKUP_FLAG_IGNORE_MARKS);
            let filter = MatchFilter::for_lookup(flag, cx.gdef, cx.mark_filtering_set);
            let Some(prev) = Skipper::new(&filter, cx.ignore_zwj).prev(glyphs, at) else {
                return false;
            };
            if !is_mark(&glyphs[prev], cx.gdef) || !marks_share_a_component(glyphs, at, prev) {
                return false;
            }
            let Some(pair) = mmp.attach(mark_gid, glyphs[prev].glyph_id as u16) else {
                return false;
            };
            attach_mark(glyphs, att, at, prev, &pair, bytes, cx.var);
            true
        }
    }
}

/// True when GDEF classes the glyph as a mark.
fn is_mark(g: &Glyph, gdef: Option<&Gdef<'_>>) -> bool {
    gdef.is_some_and(|d| d.glyph_class(g.glyph_id as u16) == GlyphClass::Mark)
}

/// The glyph a mark at `at` attaches to: the nearest earlier glyph the
/// skipping iterator stops at when it ignores marks (and, like every
/// GPOS iteration, default-ignorable characters), whatever the
/// lookup's own flags say. `accept` can turn a candidate down, which
/// passes over it like a mark.
fn find_base(
    glyphs: &[Glyph],
    at: usize,
    cx: &LookupCx<'_>,
    accept: impl Fn(usize) -> bool,
) -> Option<usize> {
    let filter = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, cx.gdef, None);
    let skipper = Skipper::new(&filter, cx.ignore_zwj);
    (0..at)
        .rev()
        .find(|&j| !skipper.skips(&glyphs[j]) && accept(j))
}

/// HarfBuzz's mark-to-base `accept`: of the glyphs a multiple
/// substitution produced, a mark only attaches to the first one
/// (issue 740), unless a mark separates it from its predecessor
/// (issue 1020).
fn accepts_as_base(glyphs: &[Glyph], j: usize, gdef: Option<&Gdef<'_>>) -> bool {
    let g = &glyphs[j];
    if !lig::is_multiplied(g) || lig::lig_comp(g) == 0 || j == 0 {
        return true;
    }
    let prev = &glyphs[j - 1];
    is_mark(prev, gdef)
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

/// Records a mark attachment of `mark` onto `parent`: the mark's offset
/// becomes the raw anchor delta (replacing any earlier placement, as in
/// HarfBuzz) and its slot links to the parent for the resolve pass.
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
    let g = &mut glyphs[mark];
    g.x_offset = base_x.saturating_sub(mark_x);
    g.y_offset = base_y.saturating_sub(mark_y);
    att.slots[mark] = Slot {
        kind: AttachKind::Mark,
        chain: parent as i32 - mark as i32,
    };
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
    let Some(i) = Skipper::new(cx.filter, cx.ignore_zwj).prev(glyphs, j) else {
        return false;
    };
    let Some(exit) = cp.exit(glyphs[i].glyph_id as u16) else {
        return false;
    };
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
    if cx.lookup_flag & LOOKUP_FLAG_RIGHT_TO_LEFT == 0 {
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

/// Final attachment pass: converts every recorded chain into pen
/// relative offsets, parents before children. Must run after all
/// advance changes (GPOS, legacy kern, kerx, mark-width zeroing) and
/// before a backward run is reversed; `direction` is the run's
/// effective direction.
///
/// Equivalent to HarfBuzz's `propagate_attachment_offsets` over the
/// whole buffer. The walk is iterative, so arbitrarily long cursive
/// chains cannot overflow the stack, and each link is consumed once,
/// so malformed cycles terminate.
pub(super) fn resolve_attachments(glyphs: &mut [Glyph], slots: &mut [Slot], direction: Direction) {
    let len = glyphs.len().min(slots.len());
    let mut path: Vec<(usize, usize, AttachKind)> = Vec::new();
    for start in 0..len {
        if slots[start].chain == 0 {
            continue;
        }
        path.clear();
        let mut cur = start;
        loop {
            let slot = slots[cur];
            if slot.chain == 0 {
                break;
            }
            slots[cur].chain = 0;
            let Some(parent) = linked_index(cur, slot.chain, len) else {
                break;
            };
            path.push((cur, parent, slot.kind));
            cur = parent;
        }
        for &(child, parent, kind) in path.iter().rev() {
            propagate(glyphs, child, parent, kind, direction);
        }
    }
}

/// Folds the resolved position of `parent` into `child`.
fn propagate(
    glyphs: &mut [Glyph],
    child: usize,
    parent: usize,
    kind: AttachKind,
    direction: Direction,
) {
    let (px, py) = (glyphs[parent].x_offset, glyphs[parent].y_offset);
    match kind {
        AttachKind::None => {}
        AttachKind::Cursive => {
            let g = &mut glyphs[child];
            if direction.is_horizontal() {
                g.y_offset = g.y_offset.saturating_add(py);
            } else {
                g.x_offset = g.x_offset.saturating_add(px);
            }
        }
        AttachKind::Mark => {
            let (mut dx, mut dy) = (px, py);
            // Marks only ever attach backwards in logical order.
            if parent < child {
                if direction.is_forward() {
                    // The pen reaches the mark after passing the
                    // parent and everything up to the mark.
                    for g in &glyphs[parent..child] {
                        dx = dx.saturating_sub(g.x_advance);
                        dy = dy.saturating_sub(g.y_advance);
                    }
                } else {
                    // After the final reversal the mark precedes the
                    // parent: the pen reaches the parent after the
                    // mark and everything between the two.
                    for g in &glyphs[parent + 1..=child] {
                        dx = dx.saturating_add(g.x_advance);
                        dy = dy.saturating_add(g.y_advance);
                    }
                }
            }
            let g = &mut glyphs[child];
            g.x_offset = g.x_offset.saturating_add(dx);
            g.y_offset = g.y_offset.saturating_add(dy);
        }
    }
}

#[cfg(test)]
mod tests;
