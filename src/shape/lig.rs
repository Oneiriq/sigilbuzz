//! Ligature component bookkeeping, HarfBuzz's `lig_props`, and the
//! two substitutions that change the run's length.
//!
//! GPOS needs to know which ligature component a mark belongs to
//! (mark-to-ligature), whether two marks sit on the same component
//! (mark-to-mark), and which glyphs came out of a multiple
//! substitution (mark-to-base). GSUB records that here, the way
//! HarfBuzz's `ligate_input` and `Sequence::apply` do:
//!
//! - When a ligature forms, the ligature glyph gets a fresh ligature
//!   id and its component count; every mark that sat between the
//!   components gets the same id and the 1-based index of the
//!   component it followed.
//! - When a multiple substitution expands one glyph into several, the
//!   outputs get ligature id 0 and component indices 0, 1, 2, ...,
//!   plus a "multiplied" flag.
//!
//! Both work on the [`GsubBuffer`] at its cursor, moving glyphs from
//! its input to its output as HarfBuzz's output buffer does, so they
//! cost time in the length of the match only.
//!
//! The state rides in the upper bits of [`Glyph::unicode_props`], so
//! it follows each glyph through every reorder and substitution
//! without a parallel array. Bits 8 to 15 hold HarfBuzz's `lig_props`
//! byte verbatim (ligature id in the top three bits, the "is the
//! ligature glyph" flag in bit 4, the component in the low four), bit
//! 7 holds the "multiplied" glyph property, and bit 6 the "ligated"
//! one (any ligature substitution's output); see [`match_prop`], which
//! the matching rules read the same bits through, and
//! [`crate::buffer::unicode_prop`] for the whole layout.
//!
//! Ligation also updates the synthesized glyph class fonts without a
//! GDEF `GlyphClassDef` match against (HarfBuzz's `_set_glyph_class`
//! with a class guess): a real ligature becomes a ligature glyph, while
//! a base or mark that swallowed only marks keeps its class.
//!
//! One difference: HarfBuzz numbers ligature ids from a buffer-wide
//! serial (1 to 7, then wrapping). The GSUB passes here do not share
//! such a counter, so [`GsubBuffer::alloc_lig_id`] picks the smallest
//! id no glyph in the run carries. Ids only ever get compared for
//! equality, so this gives the same answers as HarfBuzz until a run
//! holds more than seven live ligatures, where HarfBuzz's wrapped ids
//! can collide and these cannot until all seven are taken.

use super::gsub_buffer::GsubBuffer;
use crate::buffer::char_class;
use crate::buffer::Glyph;
use crate::tables::layout::skip_iter::{match_prop, GlyphClasses, GlyphKind, MatchGlyph};

fn set_lig_props(g: &mut Glyph, props: u8) {
    g.unicode_props =
        (g.unicode_props & 0x00FF) | (u16::from(props) << match_prop::LIG_PROPS_SHIFT);
}

/// Ligature id: nonzero for a ligature glyph and for the marks that
/// were inside it when it formed.
pub(super) fn lig_id(g: &Glyph) -> u8 {
    MatchGlyph::from(g).lig_id()
}

/// Component index: 1-based position of the ligature component a mark
/// belongs to, or a multiple substitution's 0-based output index. Zero
/// for the ligature glyph itself.
pub(super) fn lig_comp(g: &Glyph) -> u8 {
    MatchGlyph::from(g).lig_comp()
}

/// True when the glyph came out of a multiple substitution.
pub(super) fn is_multiplied(g: &Glyph) -> bool {
    MatchGlyph::from(g).is_multiplied()
}

/// True when a ligature substitution produced the glyph
/// (`_hb_glyph_info_ligated`).
pub(super) fn is_ligated(g: &Glyph) -> bool {
    MatchGlyph::from(g).is_ligated()
}

fn set_for_ligature(g: &mut Glyph, lig_id: u8, num_comps: u8) {
    set_lig_props(
        g,
        (lig_id << 5) | match_prop::IS_LIG_BASE | (num_comps & 0x0F),
    );
}

fn set_for_mark(g: &mut Glyph, lig_id: u8, comp: u8) {
    set_lig_props(g, (lig_id << 5) | (comp & 0x0F));
}

/// Sets the synthesized glyph class bits (`match_prop::SYNTHESIZED_*`).
fn set_synthesized_class(g: &mut Glyph, class: u16) {
    g.unicode_props = (g.unicode_props & !match_prop::SYNTHESIZED_CLASS) | class;
}

/// Number of components a glyph stands for, HarfBuzz's
/// `_hb_glyph_info_get_lig_num_comps`: its recorded count when it is
/// a ligature glyph by class and was formed as one, else one.
pub(super) fn num_comps(g: &Glyph, classes: &GlyphClasses<'_>) -> u8 {
    let m = MatchGlyph::from(g);
    if classes.kind(m) == GlyphKind::Ligature && m.is_lig_base() {
        m.lig_props() & 0x0F
    } else {
        1
    }
}

/// The components a later component of a ligature adds, HarfBuzz's
/// `_hb_glyph_info_get_lig_num_comps_in_ligation`: none for a piece
/// after the first of a multiple substitution, which belongs to the
/// same component as the first piece (HarfBuzz issue 4969).
fn num_comps_in_ligation(g: &Glyph, classes: &GlyphClasses<'_>) -> u8 {
    if is_multiplied(g) && lig_comp(g) != 0 {
        0
    } else {
        num_comps(g, classes)
    }
}

/// Renumbers a mark that follows a component: its own component
/// index (or the component's last one) shifted past the components
/// before it.
fn renumber(g: &Glyph, so_far: u32, last: u32) -> u8 {
    let mut this_comp = u32::from(lig_comp(g));
    if this_comp == 0 {
        this_comp = last;
    }
    (so_far.saturating_sub(last) + this_comp.min(last)) as u8
}

/// Performs a ligature substitution at the cursor, HarfBuzz's
/// `ligate_input`: the components at the logical `positions` (the
/// first is the cursor, the rest ascending, the last before
/// `match_end`) become one `lig_gid` glyph, and the glyphs between them
/// follow it.
///
/// - A ligature whose later components are all marks does not count
///   as a ligature for component tracking: a base plus marks keeps
///   behaving like the base, and a run of marks keeps its marks'
///   existing ids.
/// - Otherwise the ligature gets a new id and its total component
///   count, and every glyph between the components (the marks the
///   lookup skipped) gets that id and the index of the component it
///   followed, counted across components that were ligatures
///   themselves. Its synthesized class becomes ligature, and a
///   ligature whose first component was a nonspacing mark is not one
///   any more.
/// - Marks right after the last component that belonged to an
///   earlier ligature are renumbered into this one.
///
/// At the monotone cluster levels the matched span shares one
/// cluster, the smallest in it (`ligate_input` calls its buffer's
/// `merge_clusters`); at the others the ligature keeps its first
/// component's cluster and the glyphs between keep theirs.
///
/// `classes` says what counts as a base glyph and a mark. Returns
/// false, leaving the buffer untouched, for positions that do not
/// fit that description.
pub(super) fn ligate(
    buf: &mut GsubBuffer,
    positions: &[usize],
    match_end: usize,
    lig_gid: u16,
    classes: &GlyphClasses<'_>,
) -> bool {
    let cursor = buf.cursor();
    let (Some(&first_pos), Some(&last)) = (positions.first(), positions.last()) else {
        return false;
    };
    let ascending = positions.windows(2).all(|w| w[0] < w[1]);
    if first_pos != cursor || !ascending || last >= match_end || match_end > buf.len() {
        return false;
    }
    let kind = |b: &GsubBuffer, p: usize| {
        b.get(p).map_or(GlyphKind::Unclassified, |g| {
            classes.kind(MatchGlyph::from(g))
        })
    };
    let comps = |b: &GsubBuffer, p: usize, first: bool| {
        b.get(p).map_or(1, |g| {
            u32::from(if first {
                num_comps(g, classes)
            } else {
                num_comps_in_ligation(g, classes)
            })
        })
    };
    let total_comps: u32 = comps(buf, cursor, true)
        + positions[1..]
            .iter()
            .map(|&p| comps(buf, p, false))
            .sum::<u32>();

    buf.merge_clusters(cursor, match_end);

    let mut is_mark_ligature = kind(buf, cursor) == GlyphKind::Mark;
    let mut is_base_ligature = kind(buf, cursor) == GlyphKind::Base;
    if positions[1..]
        .iter()
        .any(|&p| kind(buf, p) != GlyphKind::Mark)
    {
        is_mark_ligature = false;
        is_base_ligature = false;
    }
    let is_ligature = !is_base_ligature && !is_mark_ligature;
    let new_id = if is_ligature { buf.alloc_lig_id() } else { 0 };

    let Some(first) = buf.cur().copied() else {
        return false;
    };
    let mut last_lig_id = lig_id(&first);
    let mut last_num_comps = u32::from(num_comps(&first, classes));
    let mut comps_so_far = last_num_comps;

    if let Some(lig) = buf.cur_mut() {
        if is_ligature {
            set_for_ligature(lig, new_id, total_comps.min(15) as u8);
            set_synthesized_class(lig, match_prop::SYNTHESIZED_LIGATURE);
            if lig.char_class & char_class::NONSPACING_MARK != 0 {
                // HarfBuzz makes it General_Category Lo, which is no
                // mark and has no combining class.
                lig.char_class &= !(char_class::MARK | char_class::NONSPACING_MARK);
                lig.combining_class = 0;
            }
        }
        lig.unicode_props = (lig.unicode_props & !match_prop::MULTIPLIED) | match_prop::LIGATED;
    }
    buf.replace_glyph(lig_gid);

    // Later components keep their logical positions as long as every
    // glyph consumed before them is output again. Each component
    // dropped moves the ones after it one place closer.
    for (dropped, &pos) in positions[1..].iter().enumerate() {
        let pos = pos - dropped;
        while buf.cursor() < pos && buf.has_input() {
            if is_ligature {
                if let Some(g) = buf.cur_mut() {
                    let comp = renumber(g, comps_so_far, last_num_comps);
                    set_for_mark(g, new_id, comp);
                }
            }
            buf.next_glyph();
        }
        if let Some(component) = buf.cur() {
            last_lig_id = lig_id(component);
            last_num_comps = u32::from(num_comps_in_ligation(component, classes));
            comps_so_far += last_num_comps;
        }
        buf.skip_glyph();
    }

    if !is_mark_ligature && last_lig_id != 0 {
        // Re-adjust the components of the marks that follow.
        let mut i = buf.cursor();
        while let Some(g) = buf.get_mut(i) {
            if lig_id(g) != last_lig_id || lig_comp(g) == 0 {
                break;
            }
            let comp = renumber(g, comps_so_far, last_num_comps);
            set_for_mark(g, new_id, comp);
            i += 1;
        }
    }
    true
}

/// Gives the `index`-th output of a multiple substitution of `source`
/// its props, as `Sequence::apply` and `_set_glyph_class` do: it is
/// flagged multiplied, numbered as component `index` unless the source
/// belonged to a ligature, and synthesized as a base glyph when the
/// source was a synthesized ligature.
fn set_multiplied(g: &mut Glyph, source: &Glyph, index: usize) {
    g.unicode_props |= match_prop::MULTIPLIED;
    if lig_id(source) == 0 {
        set_for_mark(g, 0, index as u8);
    }
    if MatchGlyph::from(source).synthesized_kind() == GlyphKind::Ligature {
        set_synthesized_class(g, 0);
    }
}

/// Performs a multiple substitution of two or more glyphs at the
/// cursor (HarfBuzz's `Sequence::apply`): the cursor glyph is replaced
/// by `seq`, each output copying its state and recorded as above, and
/// the cursor moves past the outputs.
pub(super) fn multiply(buf: &mut GsubBuffer, seq: &[u16]) {
    let Some(source) = buf.cur().copied() else {
        return;
    };
    for (i, &gid) in seq.iter().enumerate() {
        let mut g = source;
        super::gsub::substitute_glyph(&mut g, gid);
        set_multiplied(&mut g, &source, i);
        buf.output_glyph(g);
    }
    buf.skip_glyph();
}

/// Ligates `positions` of a plain glyph run, the first one standing
/// for the cursor, for tests that set up GPOS input.
#[cfg(test)]
pub(super) fn ligate_glyphs(
    glyphs: &mut alloc::vec::Vec<Glyph>,
    positions: &[usize],
    lig_gid: u16,
    classes: &GlyphClasses<'_>,
    level: crate::buffer::ClusterLevel,
) -> bool {
    let (Some(&first), Some(&last)) = (positions.first(), positions.last()) else {
        return false;
    };
    let mut buf = GsubBuffer::new(core::mem::take(glyphs), None, level, false);
    buf.clear_output();
    buf.next_glyphs(first);
    let done = ligate(&mut buf, positions, last + 1, lig_gid, classes);
    *glyphs = buf.into_glyphs();
    done
}

/// Records a multiple substitution that turned `glyphs[at]` into the
/// `outputs` glyphs now at `glyphs[at..at + outputs]`, for tests.
#[cfg(test)]
pub(super) fn record_multiple(glyphs: &mut [Glyph], at: usize, outputs: usize) {
    let Some(run) = at
        .checked_add(outputs)
        .and_then(|end| glyphs.get_mut(at..end))
    else {
        return;
    };
    let Some(source) = run.first().copied() else {
        return;
    };
    if outputs < 2 {
        return;
    }
    for (i, g) in run.iter_mut().enumerate() {
        set_multiplied(g, &source, i);
    }
}

#[cfg(test)]
mod tests;
