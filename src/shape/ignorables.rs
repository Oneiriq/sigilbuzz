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

use alloc::vec::Vec;

use super::cluster::merge_clusters;
use crate::buffer::{unicode_prop, ClusterLevel, Glyph};

/// HarfBuzz's `hb_unicode_funcs_t::is_default_ignorable` (in
/// `hb-unicode.hh`): Default_Ignorable_Code_Point, except the Hangul
/// fillers (U+115F, U+1160, U+3164, U+FFA0) and the shorthand format
/// controls (U+1BCA0..U+1BCA3), which fonts draw as regular spacing
/// glyphs.
pub(super) const fn is_default_ignorable(ch: char) -> bool {
    matches!(
        ch as u32,
        0x00AD // SOFT HYPHEN
            | 0x034F // COMBINING GRAPHEME JOINER
            | 0x061C // ARABIC LETTER MARK
            | 0x17B4..=0x17B5 // KHMER VOWEL INHERENT AQ, AA
            | 0x180B..=0x180F // MONGOLIAN FVS1..3, VOWEL SEPARATOR, FVS4
            | 0x200B..=0x200F // ZWSP, ZWNJ, ZWJ, LRM, RLM
            | 0x202A..=0x202E // bidi embeddings and overrides
            | 0x2060..=0x206F // word joiner, invisible operators, isolates
            | 0xFE00..=0xFE0F // variation selectors
            | 0xFEFF // ZERO WIDTH NO-BREAK SPACE
            | 0xFFF0..=0xFFF8 // reserved
            | 0x1D173..=0x1D17A // musical beam and phrase controls
            | 0xE0000..=0xE0FFF // tags and supplementary variation selectors
    )
}

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

/// `hb_ot_hide_default_ignorables`: swaps every hidden glyph for the
/// font's space glyph, or, when the font has none, deletes it and
/// merges its cluster into a neighbor the way HarfBuzz's
/// `delete_glyphs_inplace` does: backward at every cluster `level`,
/// forward (into the next glyph) only at the monotone ones. `glyphs`
/// is in output order.
pub(super) fn hide(glyphs: &mut Vec<Glyph>, space: Option<u32>, level: ClusterLevel) {
    if let Some(space) = space {
        for glyph in glyphs.iter_mut().filter(|g| is_hidden(g)) {
            glyph.glyph_id = space;
        }
        return;
    }
    if !glyphs.iter().any(is_hidden) {
        return;
    }
    let mut kept: Vec<Glyph> = Vec::with_capacity(glyphs.len());
    for i in 0..glyphs.len() {
        let glyph = glyphs[i];
        if !is_hidden(&glyph) {
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
        if let Some(last) = kept.last() {
            // Merge backward: the preceding cluster takes the smaller
            // value.
            let old = last.cluster;
            if cluster < old {
                for g in kept.iter_mut().rev().take_while(|g| g.cluster == old) {
                    g.cluster = cluster;
                }
            }
            continue;
        }
        // Merge forward into the next glyph's cluster (a no-op below
        // the monotone levels). Everything before `i` was deleted, so
        // the merge only changes glyphs still to come.
        merge_clusters(glyphs, i, i + 2, level);
    }
    *glyphs = kept;
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
        hide(&mut glyphs, Some(3), MC);
        let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
        assert_eq!(ids, [1, 3]);
    }

    #[test]
    fn hide_without_a_space_glyph_deletes_and_merges_clusters() {
        // Leading ignorable: merged forward.
        let mut glyphs = alloc::vec![glyph(9, 0, true), glyph(1, 3, false)];
        hide(&mut glyphs, None, MC);
        let got: Vec<(u32, u32)> = glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect();
        assert_eq!(got, [(1, 0)]);
        // Ignorable after a glyph: its larger cluster just goes away.
        let mut glyphs = alloc::vec![glyph(1, 0, false), glyph(9, 1, true), glyph(2, 4, false)];
        hide(&mut glyphs, None, MC);
        let got: Vec<(u32, u32)> = glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect();
        assert_eq!(got, [(1, 0), (2, 4)]);
        // Right-to-left output: the ignorable's smaller cluster merges
        // backward into the glyphs before it.
        let mut glyphs = alloc::vec![glyph(2, 4, false), glyph(5, 4, false), glyph(9, 1, true)];
        hide(&mut glyphs, None, MC);
        let got: Vec<(u32, u32)> = glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect();
        assert_eq!(got, [(2, 1), (5, 1)]);
    }

    fn ids_and_clusters(glyphs: &[Glyph]) -> Vec<(u32, u32)> {
        glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect()
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
            hide(&mut glyphs, None, level);
            assert_eq!(ids_and_clusters(&glyphs), [forward], "{level:?}");
            let mut glyphs = alloc::vec![glyph(2, 4, false), glyph(9, 1, true)];
            hide(&mut glyphs, None, level);
            assert_eq!(ids_and_clusters(&glyphs), [(2, 1)], "{level:?}");
        }
    }
}
