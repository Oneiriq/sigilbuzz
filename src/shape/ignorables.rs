//! Default-ignorable code points: which characters are hidden, and
//! the passes that hide them.
//!
//! HarfBuzz (`hb-ot-shape.cc`) treats a default-ignorable character
//! like any other while shaping: it maps it through cmap, and GSUB and
//! GPOS see that glyph. After positioning,
//! `hb_ot_zero_width_default_ignorables` zeroes the advance (and the
//! offset along the line) of every default-ignorable glyph GSUB did not
//! substitute, and `hb_ot_hide_default_ignorables` swaps those glyphs
//! for the space glyph, or deletes them when the font has no space.
//! sigilbuzz follows the same order: [`unicode_props`] marks the
//! glyphs at cmap time, GSUB clears the mark on any glyph it
//! substitutes, and [`zero_width`] and [`hide`] run after positioning.
//! The buffer flags `PRESERVE_DEFAULT_IGNORABLES` (keep the glyph and
//! its advance) and `REMOVE_DEFAULT_IGNORABLES` (delete the glyph)
//! change the last two steps exactly as they do in HarfBuzz.

use alloc::vec::Vec;

use super::cluster::merge_clusters;
use crate::buffer::{unicode_prop, BufferFlags, ClusterLevel, Glyph, GlyphFlags};

pub(super) use crate::unicode::is_default_ignorable;

/// The [`Glyph::unicode_props`] bits a glyph mapped from `ch` starts
/// with.
pub(super) fn unicode_props(ch: char) -> u16 {
    let mut props = 0;
    if is_default_ignorable(ch) {
        props |= unicode_prop::DEFAULT_IGNORABLE;
    }
    match ch {
        '\u{200D}' => props |= unicode_prop::JOINER,
        '\u{200C}' => props |= unicode_prop::NON_JOINER,
        _ => {}
    }
    props
}

/// True for a default-ignorable glyph that GSUB has not substituted.
fn is_hidden(glyph: &Glyph) -> bool {
    glyph.unicode_props & unicode_prop::DEFAULT_IGNORABLE != 0
}

/// `hb_ot_zero_width_default_ignorables`: zeroes the advances of every
/// hidden glyph, and its offset along the line. Runs after all
/// positioning and before attachment offsets are resolved. (Current
/// HarfBuzz keeps the cross-stream offset; the older rustybuzz 0.20
/// port still zeroes it.)
pub(super) fn zero_width(glyphs: &mut [Glyph], vertical: bool) {
    for glyph in glyphs.iter_mut().filter(|g| is_hidden(g)) {
        glyph.x_advance = 0;
        glyph.y_advance = 0;
        if vertical {
            glyph.y_offset = 0;
        } else {
            glyph.x_offset = 0;
        }
    }
}

/// True when [`zero_width`] should run for a buffer with `flags`:
/// HarfBuzz skips the zeroing when the ignorables are to be preserved
/// (they keep the font's advance) or removed (they go away anyway).
pub(super) fn zeroes(flags: BufferFlags) -> bool {
    !flags.intersects(
        BufferFlags::PRESERVE_DEFAULT_IGNORABLES | BufferFlags::REMOVE_DEFAULT_IGNORABLES,
    )
}

/// `hb_ot_hide_default_ignorables`, for a buffer with `flags` and
/// cluster `level`. `glyphs` is in output order.
///
/// - With [`BufferFlags::PRESERVE_DEFAULT_IGNORABLES`], nothing
///   happens: the glyphs keep the font's glyph and advance.
/// - Otherwise every hidden glyph becomes the font's space glyph,
///   unless [`BufferFlags::REMOVE_DEFAULT_IGNORABLES`] is set or the
///   font has no space; then the glyph is deleted and its cluster
///   merged into a neighbor, as HarfBuzz's `delete_glyphs_inplace`
///   does: backward at every level, forward (into the next glyph)
///   only at the monotone levels.
pub(super) fn hide(
    glyphs: &mut Vec<Glyph>,
    space: Option<u32>,
    flags: BufferFlags,
    level: ClusterLevel,
) {
    if flags.contains(BufferFlags::PRESERVE_DEFAULT_IGNORABLES) {
        return;
    }
    if let Some(space) = space.filter(|_| !flags.contains(BufferFlags::REMOVE_DEFAULT_IGNORABLES)) {
        for glyph in glyphs.iter_mut().filter(|g| is_hidden(g)) {
            glyph.glyph_id = space;
        }
        return;
    }
    if !glyphs.iter().any(is_hidden) {
        return;
    }
    let mut kept: Vec<Glyph> = Vec::with_capacity(glyphs.len());
    // The trailing run of `kept` glyphs that share one cluster starts
    // at `run_start` and has the cluster `run_cluster`. A backward merge
    // only lowers `run_cluster`, and the value is written into the run
    // once, when the run ends. Rewriting the run on every merge would
    // make a long run of ignorables after one big cluster quadratic.
    let mut run_start = 0;
    let mut run_cluster: Option<u32> = None;
    // The flags of the deleted glyph that last lowered `run_cluster`:
    // HarfBuzz's `set_cluster` gives them to every glyph whose cluster
    // the merge changes.
    let mut run_flags: Option<GlyphFlags> = None;
    for i in 0..glyphs.len() {
        let glyph = glyphs[i];
        if !is_hidden(&glyph) {
            if run_cluster != Some(glyph.cluster) {
                write_cluster(&mut kept, run_start, run_cluster, run_flags);
                run_start = kept.len();
                run_cluster = Some(glyph.cluster);
                run_flags = None;
            }
            kept.push(glyph);
            continue;
        }
        let cluster = glyph.cluster;
        if glyphs
            .get(i + 1)
            .is_some_and(|next| next.cluster == cluster)
        {
            // The cluster survives in the next glyph.
            continue;
        }
        if let Some(old) = run_cluster {
            // Merge backward: the preceding cluster takes the smaller
            // value. A run before it that already has that value now
            // continues into it.
            if cluster < old {
                run_cluster = Some(cluster);
                run_flags = Some(glyph.flags);
                while run_start > 0 && kept[run_start - 1].cluster == cluster {
                    run_start -= 1;
                }
            }
            continue;
        }
        // Merge forward into the next glyph's cluster (a no-op below
        // the monotone levels). Everything before `i` was deleted, so
        // the merge only changes glyphs still to come.
        merge_clusters(glyphs, i, i + 2, level);
    }
    write_cluster(&mut kept, run_start, run_cluster, run_flags);
    *glyphs = kept;
}

/// Gives every glyph of `kept[start..]` the cluster `cluster`, when
/// there is one, and `flags` to those whose cluster that changes.
fn write_cluster(
    kept: &mut [Glyph],
    start: usize,
    cluster: Option<u32>,
    flags: Option<GlyphFlags>,
) {
    let Some(cluster) = cluster else {
        return;
    };
    for g in kept.get_mut(start..).unwrap_or_default() {
        if let Some(flags) = flags.filter(|_| g.cluster != cluster) {
            g.flags = flags;
        }
        g.cluster = cluster;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MC: ClusterLevel = ClusterLevel::MonotoneCharacters;

    fn glyph(id: u32, cluster: u32, ignorable: bool) -> Glyph {
        let mut g = Glyph::new(id, cluster);
        g.x_advance = 500;
        g.x_offset = 7;
        g.y_offset = 3;
        if ignorable {
            g.unicode_props = unicode_prop::DEFAULT_IGNORABLE;
        }
        g
    }

    #[test]
    fn default_ignorable_set_matches_harfbuzz() {
        for ch in [
            '\u{00AD}',
            '\u{034F}',
            '\u{061C}',
            '\u{17B4}',
            '\u{180E}',
            '\u{200B}',
            '\u{200D}',
            '\u{202E}',
            '\u{2065}',
            '\u{206F}',
            '\u{FE0F}',
            '\u{FEFF}',
            '\u{FFF8}',
            '\u{1D17A}',
            '\u{E0001}',
            '\u{E01EF}',
            '\u{E0FFF}',
        ] {
            assert!(is_default_ignorable(ch), "{ch:?}");
        }
        // Default_Ignorable_Code_Point, but drawn by fonts: HarfBuzz's
        // exceptions.
        for ch in [
            '\u{115F}',
            '\u{1160}',
            '\u{3164}',
            '\u{FFA0}',
            '\u{1BCA0}',
            '\u{1BCA3}',
            'a',
            ' ',
            '\u{00A0}',
            '\u{2010}',
            '\u{FFF9}',
            '\u{E1000}',
        ] {
            assert!(!is_default_ignorable(ch), "{ch:?}");
        }
    }

    #[test]
    fn joiners_carry_their_own_bits() {
        let zwj = unicode_props('\u{200D}');
        assert_eq!(zwj, unicode_prop::DEFAULT_IGNORABLE | unicode_prop::JOINER);
        let zwnj = unicode_props('\u{200C}');
        assert_eq!(
            zwnj,
            unicode_prop::DEFAULT_IGNORABLE | unicode_prop::NON_JOINER
        );
        assert_eq!(unicode_props('a'), 0);
    }

    #[test]
    fn zero_width_clears_advance_and_the_offset_along_the_line() {
        let mut glyphs = [glyph(1, 0, false), glyph(2, 1, true)];
        zero_width(&mut glyphs, false);
        assert_eq!(glyphs[0].x_advance, 500);
        assert_eq!(
            (glyphs[1].x_advance, glyphs[1].x_offset, glyphs[1].y_offset),
            (0, 0, 3)
        );
        let mut vertical = [glyph(2, 1, true)];
        vertical[0].y_advance = -900;
        zero_width(&mut vertical, true);
        assert_eq!(
            (
                vertical[0].y_advance,
                vertical[0].x_offset,
                vertical[0].y_offset
            ),
            (0, 7, 0)
        );
    }

    #[test]
    fn hide_swaps_in_the_space_glyph() {
        let mut glyphs = alloc::vec![glyph(1, 0, false), glyph(9, 1, true)];
        hide(&mut glyphs, Some(3), BufferFlags::DEFAULT, MC);
        let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
        assert_eq!(ids, [1, 3]);
    }

    #[test]
    fn hide_without_a_space_glyph_deletes_and_merges_clusters() {
        // Leading ignorable: merged forward.
        let mut glyphs = alloc::vec![glyph(9, 0, true), glyph(1, 3, false)];
        hide(&mut glyphs, None, BufferFlags::DEFAULT, MC);
        let got: Vec<(u32, u32)> = glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect();
        assert_eq!(got, [(1, 0)]);
        // Ignorable after a glyph: its larger cluster just goes away.
        let mut glyphs = alloc::vec![glyph(1, 0, false), glyph(9, 1, true), glyph(2, 4, false)];
        hide(&mut glyphs, None, BufferFlags::DEFAULT, MC);
        let got: Vec<(u32, u32)> = glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect();
        assert_eq!(got, [(1, 0), (2, 4)]);
        // Right-to-left output: the ignorable's smaller cluster merges
        // backward into the glyphs before it.
        let mut glyphs = alloc::vec![glyph(2, 4, false), glyph(5, 4, false), glyph(9, 1, true)];
        hide(&mut glyphs, None, BufferFlags::DEFAULT, MC);
        let got: Vec<(u32, u32)> = glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect();
        assert_eq!(got, [(2, 1), (5, 1)]);
    }

    fn ids_and_clusters(glyphs: &[Glyph]) -> Vec<(u32, u32)> {
        glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect()
    }

    #[test]
    fn preserve_keeps_the_real_glyph_and_skips_zeroing() {
        let flags = BufferFlags::PRESERVE_DEFAULT_IGNORABLES;
        assert!(!zeroes(flags));
        // Preserve wins over remove, as in HarfBuzz.
        assert!(!zeroes(flags | BufferFlags::REMOVE_DEFAULT_IGNORABLES));
        let mut glyphs = alloc::vec![glyph(1, 0, false), glyph(9, 1, true)];
        hide(
            &mut glyphs,
            Some(3),
            flags | BufferFlags::REMOVE_DEFAULT_IGNORABLES,
            MC,
        );
        assert_eq!(ids_and_clusters(&glyphs), [(1, 0), (9, 1)]);
        assert_eq!(glyphs[1].x_advance, 500);
    }

    #[test]
    fn remove_deletes_even_with_a_space_glyph() {
        assert!(!zeroes(BufferFlags::REMOVE_DEFAULT_IGNORABLES));
        assert!(zeroes(BufferFlags::BOT | BufferFlags::EOT));
        let mut glyphs = alloc::vec![glyph(1, 0, false), glyph(9, 1, true), glyph(2, 4, false)];
        hide(
            &mut glyphs,
            Some(3),
            BufferFlags::REMOVE_DEFAULT_IGNORABLES,
            MC,
        );
        assert_eq!(ids_and_clusters(&glyphs), [(1, 0), (2, 4)]);
    }

    #[test]
    fn forward_merge_needs_a_monotone_level_but_backward_does_not() {
        // HarfBuzz's delete_glyphs_inplace merges a leading deleted
        // cluster forward with merge_clusters (monotone levels only),
        // but merges backward unconditionally.
        for (level, forward) in [
            (ClusterLevel::MonotoneGraphemes, (1, 0)),
            (ClusterLevel::MonotoneCharacters, (1, 0)),
            (ClusterLevel::Characters, (1, 3)),
            (ClusterLevel::Graphemes, (1, 3)),
        ] {
            let mut glyphs = alloc::vec![glyph(9, 0, true), glyph(1, 3, false)];
            hide(&mut glyphs, None, BufferFlags::DEFAULT, level);
            assert_eq!(ids_and_clusters(&glyphs), [forward], "{level:?}");
            let mut glyphs = alloc::vec![glyph(2, 4, false), glyph(9, 1, true)];
            hide(&mut glyphs, None, BufferFlags::DEFAULT, level);
            assert_eq!(ids_and_clusters(&glyphs), [(2, 1)], "{level:?}");
        }
    }

    /// Backward merges keep the behavior of rewriting the trailing run
    /// on every merge: a lowered run joins an earlier run with the same
    /// cluster, and a later merge lowers both.
    #[test]
    fn repeated_backward_merges_lower_every_run_they_reach() {
        let mut glyphs = alloc::vec![
            glyph(1, 5, false),
            glyph(2, 7, false),
            glyph(9, 5, true),
            glyph(9, 3, true),
            glyph(3, 8, false),
        ];
        hide(&mut glyphs, None, BufferFlags::DEFAULT, MC);
        let got: Vec<(u32, u32)> = glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect();
        assert_eq!(got, [(1, 3), (2, 3), (3, 8)]);
    }

    /// A long right-to-left run of ignorables after one big cluster
    /// merges in one pass. Rewriting the cluster on every merge made
    /// this quadratic.
    #[test]
    fn long_backward_merge_run_stays_linear() {
        let n = 200_000u32;
        let mut glyphs: Vec<Glyph> = (0..n).map(|_| glyph(1, n + 1, false)).collect();
        glyphs.extend((0..n).rev().map(|c| glyph(9, c, true)));
        hide(&mut glyphs, None, BufferFlags::DEFAULT, MC);
        assert_eq!(glyphs.len(), n as usize);
        assert!(glyphs.iter().all(|g| g.cluster == 0));
    }
}
