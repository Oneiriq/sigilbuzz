//! HarfBuzz's `initial_reordering_consonant_syllable` for Myanmar
//! (`hb-ot-shaper-myanmar.cc`), which follows Microsoft's Myanmar
//! shaping rules: find the base, give every glyph a position, sort the
//! syllable by position, and flip a run of pre-base vowels.

use super::sort::sort_by_position;
use crate::buffer::{ClusterLevel, Glyph};
use crate::ot::syllabic::cat::{
    A, AS, C, CS, DOTTEDCIRCLE, H, MR, PLACEHOLDER, RA, V, VBLW, VPRE, VS,
};
use crate::ot::syllabic::pos::{AFTER_MAIN, AFTER_SUB, BASE_C, BEFORE_SUB, BELOW_C, PRE_C, PRE_M};
use crate::ot::syllabic::GlyphInfo;
use crate::tables::layout::skip_iter::MatchGlyph;

/// HarfBuzz's `is_consonant_myanmar`: a consonant, an independent
/// vowel, or a placeholder that did not ligate.
fn is_consonant(g: &Glyph, info: &GlyphInfo) -> bool {
    !MatchGlyph::from(g).is_ligated()
        && matches!(info.category, C | CS | RA | V | PLACEHOLDER | DOTTEDCIRCLE)
}

/// Reorders the consonant syllable or broken cluster at
/// `start..end`. A kinzi (ra, asat, virama) at the start goes after the
/// base, glyphs before the base and a medial ra before it, pre-base
/// vowels to the front, and a below-base vowel's marks around it.
pub(super) fn reorder_consonant_syllable(
    glyphs: &mut [Glyph],
    info: &mut [GlyphInfo],
    start: usize,
    end: usize,
    level: ClusterLevel,
) {
    if end > glyphs.len() || end > info.len() || start >= end {
        return;
    }
    // A kinzi at the start counts as a reph. The base is the first
    // consonant after it, or the start when there is none.
    let has_reph = end - start >= 3
        && info[start].category == RA
        && info[start + 1].category == AS
        && info[start + 2].category == H;
    let limit = if has_reph { start + 3 } else { start };
    let base = (limit..end)
        .find(|&i| is_consonant(&glyphs[i], &info[i]))
        .unwrap_or(start);

    let mut i = start;
    while i < limit {
        info[i].position = AFTER_MAIN;
        i += 1;
    }
    while i < base {
        info[i].position = PRE_C;
        i += 1;
    }
    if i < end {
        info[i].position = BASE_C;
        i += 1;
    }
    let mut pos = AFTER_MAIN;
    while i < end {
        let category = info[i].category;
        info[i].position = if category == MR {
            // Pre-base reordering.
            PRE_C
        } else if category == VPRE {
            // Left matra.
            PRE_M
        } else if category == VS {
            i.checked_sub(1)
                .and_then(|k| info.get(k))
                .map_or(pos, |g| g.position)
        } else if pos == AFTER_MAIN && category == VBLW {
            pos = BELOW_C;
            pos
        } else if pos == BELOW_C && category == A {
            BEFORE_SUB
        } else if pos == BELOW_C && category != VBLW {
            pos = AFTER_SUB;
            pos
        } else {
            pos
        };
        i += 1;
    }

    sort_by_position(glyphs, info, start, end, level);

    // Flip the left matras, then flip each one back with what follows
    // it (https://github.com/harfbuzz/harfbuzz/issues/3863).
    let mut first_left_matra = end;
    let mut last_left_matra = end;
    for (k, g) in info.iter().enumerate().take(end).skip(start) {
        if g.position == PRE_M {
            if first_left_matra == end {
                first_left_matra = k;
            }
            last_left_matra = k;
        }
    }
    if first_left_matra < last_left_matra {
        glyphs[first_left_matra..=last_left_matra].reverse();
        info[first_left_matra..=last_left_matra].reverse();
        let mut k = first_left_matra;
        for j in first_left_matra..=last_left_matra {
            if info[j].category == VPRE {
                glyphs[k..=j].reverse();
                info[k..=j].reverse();
                k = j + 1;
            }
        }
    }
}
