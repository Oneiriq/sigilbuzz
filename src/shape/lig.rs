//! Ligature component bookkeeping, HarfBuzz's `lig_props`.
//!
//! GPOS needs to know which ligature component a mark belongs to
//! (mark-to-ligature), whether two marks sit on the same component
//! (mark-to-mark), and which glyphs came out of a multiple
//! substitution (mark-to-base). GSUB records that here, the way
//! HarfBuzz's `ligate_input` and `MultipleSubst` do:
//!
//! - When a ligature forms, the ligature glyph gets a fresh ligature
//!   id and its component count; every mark that sat between the
//!   components gets the same id and the 1-based index of the
//!   component it followed.
//! - When a multiple substitution expands one glyph into several, the
//!   outputs get ligature id 0 and component indices 0, 1, 2, ...,
//!   plus a "multiplied" flag.
//!
//! The state rides in the upper bits of [`Glyph::unicode_props`], so
//! it follows each glyph through every reorder and substitution
//! without a parallel array. Bits 8 to 15 hold HarfBuzz's `lig_props`
//! byte verbatim (ligature id in the top three bits, the "is the
//! ligature glyph" flag in bit 4, the component in the low four) and
//! bit 7 holds the "multiplied" glyph property.
//!
//! One deliberate difference: HarfBuzz numbers ligature ids from a
//! buffer-wide serial (1 to 7, then wrapping). The GSUB drivers here
//! do not share such a counter, so [`alloc_lig_id`] picks the smallest
//! id no glyph in the run carries. Ids only ever get compared for
//! equality, so this gives the same answers as HarfBuzz until a run
//! holds more than seven live ligatures, where HarfBuzz's wrapped ids
//! can collide and these cannot (as long as a free id exists).

use crate::buffer::Glyph;
use crate::tables::gdef::{Gdef, GlyphClass};

/// Bit 7 of `unicode_props`: the glyph came out of a multiple
/// substitution (HarfBuzz's `HB_OT_LAYOUT_GLYPH_PROPS_MULTIPLIED`).
const MULTIPLIED: u16 = 1 << 7;
/// Shift of the `lig_props` byte inside `unicode_props`.
const LIG_PROPS_SHIFT: u32 = 8;
/// `lig_props` flag: this glyph is the ligature itself, and the low
/// four bits count its components.
const IS_LIG_BASE: u8 = 0x10;

fn lig_props(g: &Glyph) -> u8 {
    (g.unicode_props >> LIG_PROPS_SHIFT) as u8
}

fn set_lig_props(g: &mut Glyph, props: u8) {
    g.unicode_props = (g.unicode_props & 0x00FF) | (u16::from(props) << LIG_PROPS_SHIFT);
}

/// Ligature id: nonzero for a ligature glyph and for the marks that
/// were inside it when it formed.
pub(super) fn lig_id(g: &Glyph) -> u8 {
    lig_props(g) >> 5
}

/// True for the glyph a ligature substitution produced.
fn is_lig_base(g: &Glyph) -> bool {
    lig_props(g) & IS_LIG_BASE != 0
}

/// Component index: 1-based position of the ligature component a mark
/// belongs to, or a multiple substitution's 0-based output index. Zero
/// for the ligature glyph itself.
pub(super) fn lig_comp(g: &Glyph) -> u8 {
    if is_lig_base(g) {
        0
    } else {
        lig_props(g) & 0x0F
    }
}

/// True when the glyph came out of a multiple substitution.
pub(super) fn is_multiplied(g: &Glyph) -> bool {
    g.unicode_props & MULTIPLIED != 0
}

fn set_for_ligature(g: &mut Glyph, lig_id: u8, num_comps: u8) {
    set_lig_props(g, (lig_id << 5) | IS_LIG_BASE | (num_comps & 0x0F));
}

fn set_for_mark(g: &mut Glyph, lig_id: u8, comp: u8) {
    set_lig_props(g, (lig_id << 5) | (comp & 0x0F));
}

fn is_mark(g: &Glyph, gdef: Option<&Gdef<'_>>) -> bool {
    gdef.is_some_and(|d| d.glyph_class(g.glyph_id as u16) == GlyphClass::Mark)
}

/// HarfBuzz's base-glyph property: GDEF class 1 exactly (unlisted
/// glyphs are not bases), or any non-mark when the font has no class
/// definitions to consult.
fn is_base_glyph(g: &Glyph, gdef: Option<&Gdef<'_>>) -> bool {
    match gdef.and_then(|d| d.raw_glyph_class(g.glyph_id as u16)) {
        Some(class) => class == 1,
        None => true,
    }
}

/// Number of components a glyph stands for: its recorded count when
/// it is a ligature glyph (GDEF class ligature, or no class data at
/// all), else one.
fn num_comps(g: &Glyph, gdef: Option<&Gdef<'_>>) -> u8 {
    let ligature_class = match gdef.and_then(|d| d.raw_glyph_class(g.glyph_id as u16)) {
        Some(class) => class == 2,
        None => true,
    };
    if ligature_class && is_lig_base(g) {
        lig_props(g) & 0x0F
    } else {
        1
    }
}

/// Smallest ligature id no glyph of `glyphs` carries; see the module
/// docs. With all seven taken, reuses the id of the ligature glyph
/// furthest before `at`, which is the least likely to still matter.
fn alloc_lig_id(glyphs: &[Glyph], at: usize) -> u8 {
    let mut used = [false; 8];
    for g in glyphs {
        used[usize::from(lig_id(g))] = true;
    }
    if let Some(free) = (1..8u8).find(|&id| !used[usize::from(id)]) {
        return free;
    }
    glyphs[..at]
        .iter()
        .find(|g| is_lig_base(g))
        .map_or(1, lig_id)
}

/// Records a ligature substitution and performs it: `glyphs[at]`
/// becomes `lig_gid` and the other matched components (at
/// `at + positions[1..]`, `positions[0]` being 0) are removed, while
/// the glyphs between them stay. Mirrors HarfBuzz's `ligate_input`:
///
/// - A ligature whose later components are all marks does not count
///   as a ligature for component tracking: a base plus marks keeps
///   behaving like the base, and a run of marks keeps its marks'
///   existing ids.
/// - Otherwise the ligature gets a new id and its total component
///   count, and every glyph between the components (the marks the
///   lookup skipped) gets that id and the index of the component it
///   followed, counted across components that were ligatures
///   themselves.
/// - Marks right after the last component that belonged to an
///   earlier ligature are renumbered into this one.
///
/// `substitute` writes the new glyph id; the caller passes its own
/// helper so the GSUB bookkeeping it already does (default-ignorable
/// flags) stays in one place.
pub(super) fn ligate(
    glyphs: &mut alloc::vec::Vec<Glyph>,
    at: usize,
    positions: &[usize],
    lig_gid: u16,
    gdef: Option<&Gdef<'_>>,
    substitute: fn(&mut Glyph, u16),
) {
    let Some(&last_rel) = positions.last() else {
        return;
    };
    if at + last_rel >= glyphs.len() {
        return;
    }
    // The matched span shares one cluster, the smallest in it, as in
    // HarfBuzz's ligate_input. The first component is not always the
    // smallest: text shaped in reversed grapheme order (see
    // `native_direction`) runs its clusters downward.
    super::merge_clusters(glyphs, at, at + last_rel + 1);
    let first = glyphs[at];
    let mut is_mark_ligature = is_mark(&first, gdef);
    let mut is_base_ligature = is_base_glyph(&first, gdef);
    if positions[1..]
        .iter()
        .any(|&rel| !is_mark(&glyphs[at + rel], gdef))
    {
        is_mark_ligature = false;
        is_base_ligature = false;
    }
    let is_ligature = !is_base_ligature && !is_mark_ligature;
    let total_comps: u32 = positions
        .iter()
        .map(|&rel| u32::from(num_comps(&glyphs[at + rel], gdef)))
        .sum();
    let new_id = if is_ligature {
        alloc_lig_id(glyphs, at)
    } else {
        0
    };

    let mut last_lig_id = lig_id(&first);
    let mut last_num_comps = u32::from(num_comps(&first, gdef));
    let mut comps_so_far = last_num_comps;
    // Renumbers a mark that follows a component: its own component
    // index (or the component's last one) shifted past the components
    // before it.
    let renumber = |g: &Glyph, so_far: u32, last: u32| -> u8 {
        let mut this_comp = u32::from(lig_comp(g));
        if this_comp == 0 {
            this_comp = last;
        }
        (so_far - last + this_comp.min(last)) as u8
    };
    for pair in positions.windows(2) {
        if is_ligature {
            for rel in pair[0] + 1..pair[1] {
                let comp = renumber(&glyphs[at + rel], comps_so_far, last_num_comps);
                set_for_mark(&mut glyphs[at + rel], new_id, comp);
            }
        }
        let component = &glyphs[at + pair[1]];
        last_lig_id = lig_id(component);
        last_num_comps = u32::from(num_comps(component, gdef));
        comps_so_far += last_num_comps;
    }
    if !is_mark_ligature && last_lig_id != 0 {
        for g in &mut glyphs[at + last_rel + 1..] {
            if lig_id(g) != last_lig_id || lig_comp(g) == 0 {
                break;
            }
            let comp = renumber(g, comps_so_far, last_num_comps);
            set_for_mark(g, new_id, comp);
        }
    }

    let lig = &mut glyphs[at];
    substitute(lig, lig_gid);
    lig.unicode_props &= !MULTIPLIED;
    if is_ligature {
        set_for_ligature(lig, new_id, total_comps.min(15) as u8);
    }
    // Remove the components back to front so earlier indices hold.
    for &rel in positions[1..].iter().rev() {
        glyphs.remove(at + rel);
    }
}

/// Records a multiple substitution that turned one glyph into the
/// `outputs` glyphs now sitting at `glyphs[at..at + outputs]`, as
/// HarfBuzz's `MultipleSubst` does: each output is flagged multiplied
/// and, unless the source belonged to a ligature, numbered as
/// component 0, 1, 2, ... A one-glyph sequence is a plain
/// substitution and records nothing.
pub(super) fn record_multiple(glyphs: &mut [Glyph], at: usize, outputs: usize) {
    if outputs < 2 || at + outputs > glyphs.len() {
        return;
    }
    let keep_ligature = lig_id(&glyphs[at]) != 0;
    for (i, g) in glyphs[at..at + outputs].iter_mut().enumerate() {
        g.unicode_props |= MULTIPLIED;
        if !keep_ligature {
            set_for_mark(g, 0, i as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// GDEF v1.0 whose class def lists glyphs 1..=9: 1-4 bases,
    /// 5-7 marks, 8-9 ligatures.
    fn gdef_bytes() -> Vec<u8> {
        let classes = [1u16, 1, 1, 1, 3, 3, 3, 2, 2];
        let mut out = vec![0, 1, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0];
        out.extend_from_slice(&1u16.to_be_bytes()); // ClassDef format 1
        out.extend_from_slice(&1u16.to_be_bytes()); // start glyph
        out.extend_from_slice(&(classes.len() as u16).to_be_bytes());
        for c in classes {
            out.extend_from_slice(&c.to_be_bytes());
        }
        out
    }

    fn run(ids: &[u32]) -> Vec<Glyph> {
        ids.iter()
            .enumerate()
            .map(|(i, &g)| Glyph::new(g, i as u32))
            .collect()
    }

    fn plain(g: &mut Glyph, gid: u16) {
        g.glyph_id = u32::from(gid);
    }

    fn props(glyphs: &[Glyph]) -> Vec<(u32, u8, u8)> {
        glyphs
            .iter()
            .map(|g| (g.glyph_id, lig_id(g), lig_comp(g)))
            .collect()
    }

    #[test]
    fn marks_between_components_take_their_component_index() {
        let bytes = gdef_bytes();
        let gdef = Gdef::parse(&bytes).unwrap();
        // base(1) mark(5) base(2) mark(6) base(3) mark(7): ligate the
        // three bases, skipping the marks between them.
        let mut glyphs = run(&[1, 5, 2, 6, 3, 7]);
        ligate(&mut glyphs, 0, &[0, 2, 4], 8, Some(&gdef), plain);
        assert_eq!(
            props(&glyphs),
            [(8, 1, 0), (5, 1, 1), (6, 1, 2), (7, 0, 0)],
            "the trailing mark never joined a ligature, so it keeps id 0"
        );
        assert_eq!(num_comps(&glyphs[0], Some(&gdef)), 3);
    }

    #[test]
    fn base_plus_marks_is_not_a_ligature() {
        let bytes = gdef_bytes();
        let gdef = Gdef::parse(&bytes).unwrap();
        let mut glyphs = run(&[1, 5, 6]);
        ligate(&mut glyphs, 0, &[0, 1, 2], 4, Some(&gdef), plain);
        assert_eq!(props(&glyphs), [(4, 0, 0)]);
        assert_eq!(num_comps(&glyphs[0], Some(&gdef)), 1);
    }

    #[test]
    fn marks_after_the_last_component_stay_out_of_a_plain_ligature() {
        let bytes = gdef_bytes();
        let gdef = Gdef::parse(&bytes).unwrap();
        let mut glyphs = run(&[1, 2, 5]);
        ligate(&mut glyphs, 0, &[0, 1], 8, Some(&gdef), plain);
        assert_eq!(props(&glyphs), [(8, 1, 0), (5, 0, 0)]);
    }

    #[test]
    fn nested_ligature_renumbers_the_inner_ligatures_marks() {
        let bytes = gdef_bytes();
        let gdef = Gdef::parse(&bytes).unwrap();
        // Ligate 1+2 around mark 5 into lig 8 (after base 3), then
        // ligate 3 with lig 8: the mark sat on component 1 of the
        // two-component ligature, which is component 2 of the new
        // three-component one.
        let mut glyphs = run(&[3, 1, 5, 2]);
        ligate(&mut glyphs, 1, &[0, 2], 8, Some(&gdef), plain);
        assert_eq!(props(&glyphs), [(3, 0, 0), (8, 1, 0), (5, 1, 1)]);
        ligate(&mut glyphs, 0, &[0, 1], 9, Some(&gdef), plain);
        assert_eq!(props(&glyphs), [(9, 2, 0), (5, 2, 2)]);
        assert_eq!(num_comps(&glyphs[0], Some(&gdef)), 3);
    }

    #[test]
    fn nested_ligature_keeps_an_inner_mark_on_its_component() {
        let bytes = gdef_bytes();
        let gdef = Gdef::parse(&bytes).unwrap();
        let mut glyphs = run(&[1, 5, 2, 3]);
        ligate(&mut glyphs, 0, &[0, 2], 8, Some(&gdef), plain);
        ligate(&mut glyphs, 0, &[0, 2], 9, Some(&gdef), plain);
        assert_eq!(props(&glyphs), [(9, 2, 0), (5, 2, 1)]);
        assert_eq!(num_comps(&glyphs[0], Some(&gdef)), 3);
    }

    #[test]
    fn ids_avoid_ligatures_already_in_the_run() {
        let bytes = gdef_bytes();
        let gdef = Gdef::parse(&bytes).unwrap();
        let mut glyphs = run(&[1, 2, 3, 4]);
        ligate(&mut glyphs, 0, &[0, 1], 8, Some(&gdef), plain);
        ligate(&mut glyphs, 1, &[0, 1], 9, Some(&gdef), plain);
        assert_eq!(props(&glyphs), [(8, 1, 0), (9, 2, 0)]);
    }

    #[test]
    fn multiple_substitution_numbers_its_outputs() {
        let mut glyphs = run(&[1, 2, 3]);
        record_multiple(&mut glyphs, 0, 3);
        assert_eq!(props(&glyphs), [(1, 0, 0), (2, 0, 1), (3, 0, 2)]);
        assert!(glyphs.iter().all(is_multiplied));
        // A ligature glyph formed from multiplied glyphs is not
        // multiplied any more.
        let bytes = gdef_bytes();
        let gdef = Gdef::parse(&bytes).unwrap();
        ligate(&mut glyphs, 0, &[0, 1], 8, Some(&gdef), plain);
        assert!(!is_multiplied(&glyphs[0]));
        assert!(is_multiplied(&glyphs[1]));
    }

    #[test]
    fn single_output_multiple_substitution_records_nothing() {
        let mut glyphs = run(&[1]);
        record_multiple(&mut glyphs, 0, 1);
        assert_eq!(glyphs[0].unicode_props, 0);
    }

    #[test]
    fn low_unicode_bits_survive_the_bookkeeping() {
        let mut g = Glyph::new(1, 0);
        g.unicode_props = 0b101;
        set_for_mark(&mut g, 3, 2);
        assert_eq!(g.unicode_props & 0x7F, 0b101);
        assert_eq!((lig_id(&g), lig_comp(&g)), (3, 2));
    }
}
