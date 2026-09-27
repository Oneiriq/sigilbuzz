//! HarfBuzz's Indic final reordering
//! (`final_reordering_syllable_indic` in `hb-ot-shaper-indic.cc`),
//! after the basic features: find the base again, move pre-base matras
//! next to it, move the reph to its script's position, move a
//! pre-base-reordering consonant, and mark a word-initial matra for
//! `init`.

use core::ops::Range;

use super::shaper::{is_one_of, ligated_and_didnt_multiply, Plan, CONSONANTS, INIT, PREF};
use super::RephPosition;
use crate::buffer::Glyph;
use crate::ot::syllabic::{cat, merge_clusters, pos, GlyphInfo};
use crate::tables::layout::skip_iter::match_prop;
use crate::unicode::Script;

/// Final reordering of the syllable at `range`.
pub(super) fn reorder_syllable(
    plan: &Plan<'_>,
    glyphs: &mut [Glyph],
    info: &mut [GlyphInfo],
    range: Range<usize>,
) {
    let (start, end) = (range.start, range.end);
    if start >= end || end > glyphs.len() || end > info.len() {
        return;
    }
    let f = Final {
        plan,
        start,
        end,
        level: plan.level,
    };
    f.run(glyphs, info);
}

struct Final<'p, 'a> {
    plan: &'p Plan<'a>,
    start: usize,
    end: usize,
    level: crate::buffer::ClusterLevel,
}

fn is_halant(glyphs: &[Glyph], info: &[GlyphInfo], i: usize) -> bool {
    is_one_of(&glyphs[i], &info[i], &[cat::H])
}

fn is_joiner(glyphs: &[Glyph], info: &[GlyphInfo], i: usize) -> bool {
    is_one_of(&glyphs[i], &info[i], &[cat::ZWJ, cat::ZWNJ])
}

fn is_consonant(glyphs: &[Glyph], info: &[GlyphInfo], i: usize) -> bool {
    is_one_of(&glyphs[i], &info[i], &CONSONANTS)
}

/// Moves `glyphs[from]` to `to` (`from < to`), shifting the ones
/// between back by one.
fn move_forward(glyphs: &mut [Glyph], info: &mut [GlyphInfo], from: usize, to: usize) {
    glyphs[from..=to].rotate_left(1);
    info[from..=to].rotate_left(1);
}

/// Moves the left matras in `lo..hi` after the other glyphs there,
/// keeping both groups in order. Returns where the matras start.
fn partition_matras(glyphs: &mut [Glyph], info: &mut [GlyphInfo], lo: usize, hi: usize) -> usize {
    let is_matra = |g: &GlyphInfo| g.position == pos::PRE_M;
    let pairs: alloc::vec::Vec<(Glyph, GlyphInfo)> = glyphs[lo..hi]
        .iter()
        .copied()
        .zip(info[lo..hi].iter().copied())
        .collect();
    let rest = pairs.iter().filter(|(_, i)| !is_matra(i));
    let matras = pairs.iter().filter(|(_, i)| is_matra(i));
    let first = lo + rest.clone().count();
    for (k, &(g, i)) in rest.chain(matras).enumerate() {
        glyphs[lo + k] = g;
        info[lo + k] = i;
    }
    first
}

/// Moves `glyphs[from]` to `to` (`to < from`), shifting the ones
/// between forward by one.
fn move_back(glyphs: &mut [Glyph], info: &mut [GlyphInfo], from: usize, to: usize) {
    glyphs[to..=from].rotate_right(1);
    info[to..=from].rotate_right(1);
}

impl Final<'_, '_> {
    fn run(&self, glyphs: &mut [Glyph], info: &mut [GlyphInfo]) {
        let (start, end) = (self.start, self.end);
        let script = self.plan.script();

        // Ligations and multiple substitutions may have lost a halant's
        // category. Recover it for the virama glyph.
        if let Some(virama) = self.plan.virama_glyph {
            for i in start..end {
                let m = crate::tables::layout::skip_iter::MatchGlyph::from(&glyphs[i]);
                if glyphs[i].glyph_id == u32::from(virama) && m.is_ligated() && m.is_multiplied() {
                    info[i].category = cat::H;
                    glyphs[i].unicode_props &= !(match_prop::LIGATED | match_prop::MULTIPLIED);
                }
            }
        }

        let mut try_pref = self.plan.has_pref;
        let mut base = self.find_base(glyphs, info, &mut try_pref);

        base = self.reorder_matras(glyphs, info, base);
        base = self.reorder_reph(glyphs, info, base);

        // Pre-base-reordering consonants.
        if try_pref && base + 1 < end {
            if let Some(i) = (base + 1..end).find(|&i| info[i].mask & PREF != 0) {
                // Only a glyph `pref` formed moves.
                if ligated_and_didnt_multiply(&glyphs[i]) {
                    let mut new_pos = base;
                    if script != Script::Malayalam && script != Script::Tamil {
                        while new_pos > start
                            && !is_one_of(
                                &glyphs[new_pos - 1],
                                &info[new_pos - 1],
                                &[cat::M, cat::MPST, cat::H],
                            )
                        {
                            new_pos -= 1;
                        }
                    }
                    if new_pos > start
                        && is_halant(glyphs, info, new_pos - 1)
                        && new_pos < end
                        && is_joiner(glyphs, info, new_pos)
                    {
                        new_pos += 1;
                    }
                    let old_pos = i;
                    if new_pos <= old_pos {
                        merge_clusters(glyphs, new_pos, old_pos + 1, self.level);
                        move_back(glyphs, info, old_pos, new_pos);
                    }
                }
            }
        }

        // `init` on a left matra that starts a word.
        if info[start].position == pos::PRE_M && (start == 0 || !info[start - 1].word_char) {
            info[start].mask |= INIT;
        }
    }

    /// Finds the base again after the basic features.
    fn find_base(&self, glyphs: &[Glyph], info: &mut [GlyphInfo], try_pref: &mut bool) -> usize {
        let (start, end) = (self.start, self.end);
        let script = self.plan.script();
        let mut base = start;
        while base < end {
            if info[base].position >= pos::BASE_C {
                if *try_pref && base + 1 < end {
                    if let Some(i) = (base + 1..end).find(|&i| info[i].mask & PREF != 0) {
                        if !(info[i].substituted && ligated_and_didnt_multiply(&glyphs[i])) {
                            // A `pref` candidate that formed nothing:
                            // the base is around here.
                            base = i;
                            while base < end && is_halant(glyphs, info, base) {
                                base += 1;
                            }
                            if base < end {
                                info[base].position = pos::BASE_C;
                            }
                            *try_pref = false;
                        }
                    }
                    if base == end {
                        break;
                    }
                }
                // Malayalam skips over unformed below (not post) forms.
                if script == Script::Malayalam {
                    let mut i = base + 1;
                    while i < end {
                        while i < end && is_joiner(glyphs, info, i) {
                            i += 1;
                        }
                        if i == end || !is_halant(glyphs, info, i) {
                            break;
                        }
                        i += 1;
                        while i < end && is_joiner(glyphs, info, i) {
                            i += 1;
                        }
                        if i < end
                            && is_consonant(glyphs, info, i)
                            && info[i].position == pos::BELOW_C
                        {
                            base = i;
                            info[base].position = pos::BASE_C;
                        }
                        i += 1;
                    }
                }
                if start < base && base < end && info[base].position > pos::BASE_C {
                    base -= 1;
                }
                break;
            }
            base += 1;
        }
        if base == end && start < base && is_one_of(&glyphs[base - 1], &info[base - 1], &[cat::ZWJ])
        {
            base -= 1;
        }
        if base < end {
            while start < base && is_one_of(&glyphs[base], &info[base], &[cat::N, cat::H]) {
                base -= 1;
            }
        }
        base
    }

    /// Moves pre-base matras next to the base, after the last
    /// standalone halant. Returns the base's new index.
    fn reorder_matras(&self, glyphs: &mut [Glyph], info: &mut [GlyphInfo], base: usize) -> usize {
        let (start, end) = (self.start, self.end);
        let script = self.plan.script();
        let mut base = base;
        if !(start + 1 < end && start < base) {
            return base;
        }
        // Having lost track of the base, go before the last glyph.
        let mut new_pos = if base == end { base - 2 } else { base - 1 };
        if script != Script::Malayalam && script != Script::Tamil {
            loop {
                while new_pos > start
                    && !is_one_of(
                        &glyphs[new_pos],
                        &info[new_pos],
                        &[cat::M, cat::MPST, cat::H],
                    )
                {
                    new_pos -= 1;
                }
                // Only a halant that does not belong to the matra
                // itself counts.
                if is_halant(glyphs, info, new_pos) && info[new_pos].position != pos::PRE_M {
                    // A ZWJ after the halant keeps the matra from
                    // moving after it: keep searching. A ZWNJ ends the
                    // syllable already.
                    if new_pos + 1 < end
                        && info[new_pos + 1].category == cat::ZWJ
                        && new_pos > start
                    {
                        new_pos -= 1;
                        continue;
                    }
                } else {
                    new_pos = start;
                }
                break;
            }
        }

        if start < new_pos && info[new_pos].position != pos::PRE_M {
            // Move each matra before `new_pos` to it, the last one
            // first. Each move shifts the glyphs it passes, so many
            // matras far from `new_pos` would cost time quadratic in
            // the syllable. Past a linear budget the rest move in one
            // stable partition, which leaves the same order, followed
            // by one merge over the range the single merges would have
            // covered.
            let mut budget = 8 * (end - start);
            let mut i = new_pos;
            while i > start {
                if info[i - 1].position == pos::PRE_M {
                    let old_pos = i - 1;
                    let cost = new_pos - old_pos;
                    if cost > budget {
                        let first = partition_matras(glyphs, info, start, new_pos + 1);
                        merge_clusters(glyphs, first, end.min(base + 1), self.level);
                        break;
                    }
                    budget -= cost;
                    if old_pos < base && base <= new_pos {
                        base -= 1;
                    }
                    move_forward(glyphs, info, old_pos, new_pos);
                    merge_clusters(glyphs, new_pos, end.min(base + 1), self.level);
                    new_pos -= 1;
                }
                i -= 1;
            }
        } else if let Some(i) = (start..base).find(|&i| info[i].position == pos::PRE_M) {
            merge_clusters(glyphs, i, end.min(base + 1), self.level);
        }
        base
    }

    /// Moves the reph to its script's position, when the Ra,H formed a
    /// reph (or an encoded repha did not ligate). Returns the base's
    /// new index.
    fn reorder_reph(&self, glyphs: &mut [Glyph], info: &mut [GlyphInfo], base: usize) -> usize {
        let (start, end) = (self.start, self.end);
        let mut base = base;
        let formed =
            (info[start].category == cat::REPHA) ^ ligated_and_didnt_multiply(&glyphs[start]);
        if !(start + 1 < end && info[start].position == pos::RA_TO_BECOME_REPH && formed) {
            return base;
        }
        let new_reph_pos = self.reph_target(glyphs, info, base);
        merge_clusters(glyphs, start, new_reph_pos + 1, self.level);
        move_forward(glyphs, info, start, new_reph_pos);
        if start < base && base <= new_reph_pos {
            base -= 1;
        }
        base
    }

    /// Where the reph goes: steps 2 to 6 of the reph rules.
    fn reph_target(&self, glyphs: &[Glyph], info: &[GlyphInfo], base: usize) -> usize {
        let (start, end) = (self.start, self.end);
        let reph_pos = self.plan.config.reph_pos;
        // After the first explicit halant between the reph and the
        // main consonant, and after a joiner that follows it.
        let after_halant = || {
            let mut p = start + 1;
            while p < base && !is_halant(glyphs, info, p) {
                p += 1;
            }
            if p < base && is_halant(glyphs, info, p) {
                if p + 1 < base && is_joiner(glyphs, info, p + 1) {
                    p += 1;
                }
                return Some(p);
            }
            None
        };
        if reph_pos != RephPosition::AfterPost {
            // Step 2.
            if let Some(p) = after_halant() {
                return p;
            }
            // Step 3: after the main consonant.
            if reph_pos == RephPosition::AfterMain {
                let mut p = base;
                while p + 1 < end && info[p + 1].position <= pos::AFTER_MAIN {
                    p += 1;
                }
                if p < end {
                    return p;
                }
            }
            // Step 4: before the first post-base form.
            if reph_pos == RephPosition::AfterSub {
                let mut p = base;
                while p + 1 < end
                    && !matches!(
                        info[p + 1].position,
                        pos::POST_C | pos::AFTER_POST | pos::SMVD
                    )
                {
                    p += 1;
                }
                if p < end {
                    return p;
                }
            }
        }
        // Step 5.
        if let Some(p) = after_halant() {
            return p;
        }
        // Step 6: the end of the syllable, before trailing modifiers.
        let mut p = end - 1;
        while p > start && info[p].position == pos::SMVD {
            p -= 1;
        }
        // After a matra,halant, go before the halant so the reph can
        // interact with the matra.
        if is_halant(glyphs, info, p) {
            let mut i = base + 1;
            while i < p {
                if matches!(info[i].category, cat::M | cat::MPST) {
                    p -= 1;
                }
                i += 1;
            }
        }
        p
    }
}
