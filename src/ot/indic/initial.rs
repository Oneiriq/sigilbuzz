//! HarfBuzz's Indic initial reordering
//! (`initial_reordering_consonant_syllable` in `hb-ot-shaper-indic.cc`):
//! find the base consonant, mark a reph, sort the syllable by position,
//! and set the feature masks.

use alloc::vec::Vec;
use core::ops::Range;

use super::shaper::{is_one_of, Plan, ABVF, BLWF, CONSONANTS, HALF, PREF, PSTF, RPHF};
use super::RephMode;
use crate::buffer::Glyph;
use crate::ot::syllabic::{cat, merge_clusters, pos, GlyphInfo};
use crate::unicode::Script;

/// Categories that attach to the glyph before them when the syllable is
/// sorted.
const MISC_MARKS: [u8; 6] = [cat::ZWJ, cat::ZWNJ, cat::N, cat::RS, cat::CM, cat::H];

/// Reorders the syllable at `range` and sets its masks. Vowel
/// syllables, standalone clusters, and broken clusters (which have a
/// dotted circle by now) all take the consonant syllable path, as in
/// HarfBuzz.
pub(super) fn reorder_syllable(
    plan: &mut Plan<'_>,
    glyphs: &mut [Glyph],
    info: &mut [GlyphInfo],
    range: Range<usize>,
) {
    let (start, end) = (range.start, range.end);
    if start >= end || end > glyphs.len() || end > info.len() {
        return;
    }
    let s = Syl { start, end };
    s.run(plan, glyphs, info);
}

/// One syllable's bounds.
#[derive(Clone, Copy)]
struct Syl {
    start: usize,
    end: usize,
}

impl Syl {
    fn run(self, plan: &mut Plan<'_>, glyphs: &mut [Glyph], info: &mut [GlyphInfo]) {
        let Self { start, end } = self;
        let one_of = |glyphs: &[Glyph], info: &[GlyphInfo], i: usize, cats: &[u8]| {
            is_one_of(&glyphs[i], &info[i], cats)
        };
        let is_consonant =
            |glyphs: &[Glyph], info: &[GlyphInfo], i: usize| one_of(glyphs, info, i, &CONSONANTS);
        let is_joiner = |glyphs: &[Glyph], info: &[GlyphInfo], i: usize| {
            one_of(glyphs, info, i, &[cat::ZWJ, cat::ZWNJ])
        };

        // For compatibility with legacy usage in Kannada, Ra,H,ZWJ must
        // behave like Ra,ZWJ,H.
        if plan.script() == Script::Kannada
            && start + 3 <= end
            && one_of(glyphs, info, start, &[cat::RA])
            && one_of(glyphs, info, start + 1, &[cat::H])
            && one_of(glyphs, info, start + 2, &[cat::ZWJ])
        {
            merge_clusters(glyphs, start + 1, start + 3, plan.level);
            glyphs.swap(start + 1, start + 2);
            info.swap(start + 1, start + 2);
        }

        // 1. Find the base consonant.
        let mut base = end;
        let mut has_reph = false;
        let mut limit = start;
        let mode = plan.config.reph_mode;
        if plan.has_rphf
            && start + 3 <= end
            && ((mode == RephMode::Implicit && !is_joiner(glyphs, info, start + 2))
                || (mode == RephMode::Explicit && info[start + 2].category == cat::ZWJ))
        {
            let ra_h = [glyphs[start].glyph_id, glyphs[start + 1].glyph_id];
            let ra_h_zwj = [ra_h[0], ra_h[1], glyphs[start + 2].glyph_id];
            if plan.would_substitute(*b"rphf", &ra_h)
                || (mode == RephMode::Explicit && plan.would_substitute(*b"rphf", &ra_h_zwj))
            {
                limit += 2;
                while limit < end && is_joiner(glyphs, info, limit) {
                    limit += 1;
                }
                base = start;
                has_reph = true;
            }
        } else if mode == RephMode::LogRepha && info[start].category == cat::REPHA {
            limit += 1;
            while limit < end && is_joiner(glyphs, info, limit) {
                limit += 1;
            }
            base = start;
            has_reph = true;
        }

        // Starting from the end of the syllable, move backwards until
        // a consonant without a below-base or post-base form, or the
        // first consonant.
        let mut i = end;
        let mut seen_below = false;
        loop {
            i -= 1;
            if is_consonant(glyphs, info, i) {
                let p = info[i].position;
                if p != pos::BELOW_C && (p != pos::POST_C || seen_below) {
                    base = i;
                    break;
                }
                if p == pos::BELOW_C {
                    seen_below = true;
                }
                base = i;
            } else if start < i && info[i].category == cat::ZWJ && info[i - 1].category == cat::H {
                // A ZWJ after a halant stops the search and asks for an
                // explicit half form. A ZWJ before a halant asks for a
                // subjoined form, so the search goes on.
                break;
            }
            if i <= limit {
                break;
            }
        }
        // Without another consonant, the Ra is the base after all.
        if has_reph && base == start && limit - base <= 2 {
            has_reph = false;
        }

        // Reorder characters.
        for g in &mut info[start..base.min(end)] {
            g.position = g.position.min(pos::PRE_C);
        }
        if base < end {
            info[base].position = pos::BASE_C;
        }
        if has_reph {
            info[start].position = pos::RA_TO_BECOME_REPH;
        }

        // Old-spec fonts: the first post-base halant moves after the
        // last consonant (not in Kannada when a halant already ends
        // the syllable).
        if plan.is_old_spec {
            let no_double = plan.script() == Script::Kannada;
            if let Some(i) = (base + 1..end).find(|&i| info[i].category == cat::H) {
                let mut j = end - 1;
                while j > i {
                    if is_consonant(glyphs, info, j) || (no_double && info[j].category == cat::H) {
                        break;
                    }
                    j -= 1;
                }
                if info[j].category != cat::H && j > i {
                    glyphs[i..=j].rotate_left(1);
                    info[i..=j].rotate_left(1);
                }
            }
        }

        // Misc marks take the position of the glyph before them.
        let mut last_pos = pos::START;
        for i in start..end {
            let category = info[i].category;
            if MISC_MARKS.contains(&category) {
                info[i].position = last_pos;
                if category == cat::H && info[i].position == pos::PRE_M {
                    // A halant does not move with a left matra.
                    if let Some(j) = (start + 1..=i)
                        .rev()
                        .find(|&j| info[j - 1].position != pos::PRE_M)
                    {
                        info[i].position = info[j - 1].position;
                    }
                }
            } else if info[i].position != pos::SMVD {
                if category == cat::MPST && i > start && info[i - 1].category == cat::SM {
                    info[i - 1].position = info[i].position;
                }
                last_pos = info[i].position;
            }
        }
        // Post-base consonants own what comes before them since the
        // last consonant or matra.
        let mut last = base;
        for i in base.saturating_add(1)..end {
            if is_consonant(glyphs, info, i) {
                for j in last + 1..i {
                    if info[j].position < pos::SMVD {
                        info[j].position = info[i].position;
                    }
                }
                last = i;
            } else if matches!(info[i].category, cat::M | cat::MPST) {
                last = i;
            }
        }

        let base = self.sort(plan, glyphs, info);
        self.set_masks(plan, glyphs, info, base);
    }

    /// Sorts the syllable by position (a stable sort), flips a run of
    /// left matras back, and merges the clusters of the glyphs that
    /// moved across the base. Returns the new base.
    fn sort(self, plan: &Plan<'_>, glyphs: &mut [Glyph], info: &mut [GlyphInfo]) -> usize {
        let Self { start, end } = self;
        let len = end - start;
        let mut order: Vec<usize> = (0..len).collect();
        order.sort_by_key(|&k| info[start + k].position);
        let sorted_glyphs: Vec<Glyph> = order.iter().map(|&k| glyphs[start + k]).collect();
        let sorted_info: Vec<GlyphInfo> = order.iter().map(|&k| info[start + k]).collect();
        glyphs[start..end].copy_from_slice(&sorted_glyphs);
        info[start..end].copy_from_slice(&sorted_info);

        // Find the base again, and flip a left matra sequence.
        let mut first_left_matra = end;
        let mut last_left_matra = end;
        let mut base = end;
        for (i, g) in info.iter().enumerate().take(end).skip(start) {
            if g.position == pos::BASE_C {
                base = i;
                break;
            } else if g.position == pos::PRE_M {
                if first_left_matra == end {
                    first_left_matra = i;
                }
                last_left_matra = i;
            }
        }
        if first_left_matra < last_left_matra {
            let range = first_left_matra..last_left_matra + 1;
            glyphs[range.clone()].reverse();
            info[range.clone()].reverse();
            order[range.start - start..range.end - start].reverse();
            // Reverse nuktas and the like back.
            let mut i = first_left_matra;
            for j in first_left_matra..=last_left_matra {
                if matches!(info[j].category, cat::M | cat::MPST) {
                    glyphs[i..=j].reverse();
                    info[i..=j].reverse();
                    order[i - start..=j - start].reverse();
                    i = j + 1;
                }
            }
        }

        // Glyphs that moved across the base share clusters with it.
        if plan.is_old_spec || len > 127 {
            merge_clusters(glyphs, base, end, plan.level);
        } else {
            let mut visited = alloc::vec![false; len];
            for i in base..end {
                if visited[i - start] {
                    continue;
                }
                let (mut min, mut max) = (i, i);
                let mut j = start + order[i - start];
                let mut steps = 0;
                while j != i && steps < len {
                    min = min.min(j);
                    max = max.max(j);
                    visited[j - start] = true;
                    j = start + order[j - start];
                    steps += 1;
                }
                merge_clusters(glyphs, base.max(min), max + 1, plan.level);
            }
        }
        base
    }

    /// Sets the feature masks of the sorted syllable with base `base`.
    fn set_masks(self, plan: &Plan<'_>, glyphs: &[Glyph], info: &mut [GlyphInfo], base: usize) {
        let Self { start, end } = self;
        for g in &mut info[start..end] {
            if g.position != pos::RA_TO_BECOME_REPH {
                break;
            }
            g.mask |= RPHF;
        }
        let mut pre = HALF;
        if !plan.is_old_spec && !plan.blwf_post_only {
            pre |= BLWF;
        }
        for g in &mut info[start..base.min(end)] {
            g.mask |= pre;
        }
        for g in &mut info[base.saturating_add(1).min(end)..end] {
            g.mask |= BLWF | ABVF | PSTF;
        }

        if plan.is_old_spec && plan.script() == Script::Devanagari {
            // Old-spec eyelash Ra: `blwf` also applies to Ra,H before
            // the base, unless a ZWJ asks for the eyelash form.
            for i in start..base.saturating_sub(1) {
                if info[i].category == cat::RA
                    && info[i + 1].category == cat::H
                    && (i + 2 == base || info[i + 2].category != cat::ZWJ)
                {
                    info[i].mask |= BLWF;
                    info[i + 1].mask |= BLWF;
                }
            }
        }

        if plan.has_pref && base + 2 < end {
            // Find a Halant,Ra pair for pre-base reordering.
            for i in base + 1..end - 1 {
                let pair = [glyphs[i].glyph_id, glyphs[i + 1].glyph_id];
                if plan.would_substitute(*b"pref", &pair) {
                    info[i].mask |= PREF;
                    info[i + 1].mask |= PREF;
                    break;
                }
            }
        }

        // A ZWNJ turns `half` off on the glyphs before it, back to and
        // including the previous consonant (or the syllable's first
        // glyph). ZWJ and ZWNJ block `cjct` by being there. HarfBuzz
        // walks back from each ZWNJ, which a long run of them makes
        // quadratic. One pass from the end marks the same glyphs.
        let mut pending = false;
        for j in (start..end).rev() {
            if pending {
                info[j].mask &= !HALF;
            }
            if is_one_of(&glyphs[j], &info[j], &CONSONANTS) {
                pending = false;
            }
            if j > start
                && info[j].category == cat::ZWNJ
                && is_one_of(&glyphs[j], &info[j], &[cat::ZWJ, cat::ZWNJ])
            {
                pending = true;
            }
        }
    }
}
