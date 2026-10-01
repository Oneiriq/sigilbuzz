//! Pair kerning from the legacy `kern` table and AAT `kerx`.
//!
//! Both follow HarfBuzz's `hb_kern_machine_t` (hb-kern.hh), which the
//! OpenType `kern` formats and the `kerx` pair formats (0, 2, 6)
//! share:
//!
//! - Pairs are found with a skipping iterator that ignores marks (and
//!   default-ignorable characters), so a mark between two letters
//!   does not break their kerning.
//! - A pair's value `kern` is split: `kern >> 1` goes on the first
//!   glyph's advance, the rest (`kern - (kern >> 1)`) on the second
//!   glyph's advance and also on its offset, so the second glyph keeps
//!   its place relative to the pen while the gap closes around it.
//!   The split rounds toward negative infinity: -41 puts -21 on the
//!   first glyph and -20 on the second.
//! - Subtables apply one at a time, each splitting its own value.
//! - Backward runs are kerned in visual order: kern tables list pairs
//!   left to right, so an RTL run is reversed for the pass and put
//!   back afterwards.

use super::glyph_flags::FlagCx;
use super::gpos::Skipper;
use crate::buffer::{Direction, Glyph};
use crate::error::Result;
use crate::face::Face;
use crate::tables::gdef::Gdef;
use crate::tables::layout::{
    Joiners, LayoutTable, MatchContext, MatchFilter, LOOKUP_FLAG_IGNORE_MARKS,
};
use crate::tables::{KernTable, Kerx};

/// Runs the kern machine for one pair subtable over `glyphs`, which
/// are in the order the subtable expects (visual for kern tables).
fn kern_pairs(
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    horizontal: bool,
    flags: FlagCx,
    kern: impl Fn(u16, u16) -> i32,
) {
    // `hb_kern_machine_t::kern` marks the whole run unsafe to
    // concatenate, and each pair it kerns unsafe to break.
    flags.unsafe_to_concat_all(glyphs);
    let filter = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, gdef, None);
    let skipper = Skipper::new(MatchContext::new(filter, LayoutTable::Gpos, Joiners::AUTO).input());
    let mut i = 0;
    while i < glyphs.len() {
        // No glyph after `i` stops the iterator, so none after any
        // later start does either: the pass is over. Scanning again
        // from every later glyph would make a long run of marks
        // quadratic.
        let Some(j) = skipper.next(glyphs, i + 1) else {
            break;
        };
        let (left, right) = glyphs.split_at_mut(j);
        let (Some(first_glyph), Some(second_glyph)) = (left.get_mut(i), right.first_mut()) else {
            break;
        };
        let value = kern(first_glyph.glyph_id as u16, second_glyph.glyph_id as u16);
        if value != 0 {
            // The sums saturate, like every other positioning pass.
            let first = value >> 1;
            let second = value - first;
            if horizontal {
                first_glyph.x_advance = first_glyph.x_advance.saturating_add(first);
                second_glyph.x_advance = second_glyph.x_advance.saturating_add(second);
                second_glyph.x_offset = second_glyph.x_offset.saturating_add(second);
            } else {
                first_glyph.y_advance = first_glyph.y_advance.saturating_add(first);
                second_glyph.y_advance = second_glyph.y_advance.saturating_add(second);
                second_glyph.y_offset = second_glyph.y_offset.saturating_add(second);
            }
            flags.unsafe_to_break(glyphs, i, j + 1);
        }
        i = j;
    }
}

/// Runs `apply` with `glyphs` in visual order, restoring logical
/// order afterwards.
fn in_visual_order(glyphs: &mut [Glyph], direction: Direction, apply: impl FnOnce(&mut [Glyph])) {
    let backward = !direction.is_forward();
    if backward {
        glyphs.reverse();
    }
    apply(glyphs);
    if backward {
        glyphs.reverse();
    }
}

/// Legacy `kern` table kerning. Only the horizontal subtables are
/// parsed, and HarfBuzz applies those to horizontal runs only.
pub(super) fn apply_kern_table(
    kern: &KernTable<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    direction: Direction,
    flags: FlagCx,
) {
    if !direction.is_horizontal() {
        return;
    }
    in_visual_order(glyphs, direction, |glyphs| {
        for s in 0..kern.subtable_count() {
            kern_pairs(glyphs, gdef, true, flags, |l, r| {
                i32::from(kern.subtable_kern(s, l, r))
            });
        }
    });
}

/// AAT `kerx` kerning: the pair subtables through the kern machine,
/// then the state-machine subtables (a popped value moves both the
/// glyph's advance and its offset, as in HarfBuzz), then the
/// control-point subtables. Only horizontal subtables are parsed, so
/// vertical runs are left alone.
pub(super) fn apply_kerx_table(
    face: &Face<'_>,
    kerx: &Kerx<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    direction: Direction,
    flags: FlagCx,
) -> Result<()> {
    if !direction.is_horizontal() || glyphs.is_empty() {
        return Ok(());
    }
    let mut result = Ok(());
    in_visual_order(glyphs, direction, |glyphs| {
        for s in 0..kerx.subtable_count() {
            if kerx.subtable_pair_kern(s, 0, 0).is_none() {
                continue;
            }
            kern_pairs(glyphs, gdef, true, flags, |l, r| {
                i32::from(kerx.subtable_pair_kern(s, l, r).unwrap_or(0))
            });
        }
        if kerx.has_state_machine() {
            let ids: alloc::vec::Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
            kerx.apply_state_machines(&ids, |idx, delta| {
                if let Some(g) = glyphs.get_mut(idx) {
                    g.x_advance = g.x_advance.saturating_add(i32::from(delta));
                    g.x_offset = g.x_offset.saturating_add(i32::from(delta));
                }
            });
        }
        if kerx.has_format4() {
            result = super::apply_kerx_format4(face, kerx, glyphs);
        }
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const NO_FLAGS: FlagCx = FlagCx {
        level: crate::buffer::ClusterLevel::MonotoneCharacters,
        concat: false,
        tatweel: false,
    };

    fn run(ids: &[u32]) -> Vec<Glyph> {
        ids.iter()
            .map(|&g| {
                let mut glyph = Glyph::new(g, 0);
                glyph.x_advance = 500;
                glyph
            })
            .collect()
    }

    fn table(pairs: &[(u16, u16, i32)]) -> impl Fn(u16, u16) -> i32 + '_ {
        move |l, r| {
            pairs
                .iter()
                .find(|p| p.0 == l && p.1 == r)
                .map_or(0, |p| p.2)
        }
    }

    #[test]
    fn value_splits_with_the_second_half_moving_the_second_glyph() {
        let mut glyphs = run(&[1, 2]);
        kern_pairs(&mut glyphs, None, true, NO_FLAGS, table(&[(1, 2, -41)]));
        // -41 >> 1 = -21 on A; -20 on V's advance and offset.
        assert_eq!(glyphs[0].x_advance, 479);
        assert_eq!((glyphs[1].x_advance, glyphs[1].x_offset), (480, -20));

        let mut glyphs = run(&[1, 2]);
        kern_pairs(&mut glyphs, None, true, NO_FLAGS, table(&[(1, 2, 41)]));
        assert_eq!(glyphs[0].x_advance, 520);
        assert_eq!((glyphs[1].x_advance, glyphs[1].x_offset), (521, 21));
    }

    #[test]
    fn vertical_split_uses_y() {
        let mut glyphs = run(&[1, 2]);
        kern_pairs(&mut glyphs, None, false, NO_FLAGS, table(&[(1, 2, -40)]));
        assert_eq!(glyphs[0].y_advance, -20);
        assert_eq!((glyphs[1].y_advance, glyphs[1].y_offset), (-20, -20));
        assert!(glyphs.iter().all(|g| g.x_advance == 500 && g.x_offset == 0));
    }

    #[test]
    fn every_glyph_can_start_the_next_pair() {
        let mut glyphs = run(&[1, 2, 3]);
        kern_pairs(
            &mut glyphs,
            None,
            true,
            NO_FLAGS,
            table(&[(1, 2, -10), (2, 3, -20)]),
        );
        assert_eq!(
            glyphs.iter().map(|g| g.x_advance).collect::<Vec<_>>(),
            [495, 495 - 10, 490]
        );
    }

    #[test]
    fn default_ignorables_do_not_break_a_pair() {
        let mut glyphs = run(&[1, 9, 2]);
        glyphs[1].unicode_props =
            crate::buffer::unicode_prop::DEFAULT_IGNORABLE | crate::buffer::unicode_prop::JOINER;
        kern_pairs(&mut glyphs, None, true, NO_FLAGS, table(&[(1, 2, -40)]));
        assert_eq!(glyphs[0].x_advance, 480);
        assert_eq!(glyphs[1].x_advance, 500);
        assert_eq!((glyphs[2].x_advance, glyphs[2].x_offset), (480, -20));
    }

    #[test]
    fn backward_runs_kern_visual_pairs() {
        // Logical [1, 2] in an RTL run is visually [2, 1]: the kern
        // table's (2, 1) pair applies, (1, 2) does not.
        let mut glyphs = run(&[1, 2]);
        in_visual_order(&mut glyphs, Direction::Rtl, |g| {
            kern_pairs(g, None, true, NO_FLAGS, table(&[(1, 2, -100), (2, 1, -40)]));
        });
        // Visual first glyph (logical 1, id 2) takes -20; the visual
        // second (logical 0, id 1) takes -20 plus the offset.
        assert_eq!(glyphs[1].x_advance, 480);
        assert_eq!((glyphs[0].x_advance, glyphs[0].x_offset), (480, -20));
    }
}
