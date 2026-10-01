//! The mask setup, the records, and the reordering of HarfBuzz's USE
//! shaper (`hb-ot-shaper-use.cc`): `setup_rphf_mask`,
//! `setup_topographical_masks`, `record_rphf_use`, `record_pref_use`,
//! and `reorder_syllable_use`.

use alloc::vec::Vec;
use core::ops::Range;

use super::category::{
    FABV, FBLW, FMABV, FMBLW, FMPST, FPST, H, HVM, IS, MABV, MBLW, MPRE, MPST, R, VABV, VBLW,
    VMABV, VMBLW, VMPRE, VMPST, VPRE, VPST,
};
use super::machine::syllable;
use crate::buffer::{ClusterLevel, Glyph};
use crate::ot::syllabic::{merge_clusters, syllable_ranges, GlyphInfo};
use crate::tables::layout::skip_iter::MatchGlyph;

/// HarfBuzz's `setup_rphf_mask`: `rphf` may apply to the first glyph
/// of a syllable that starts with a repha, and to the first three
/// glyphs of any other syllable.
pub(super) fn setup_rphf_mask(info: &mut [GlyphInfo], rphf: u32) {
    for range in syllable_ranges(info) {
        let limit = if info[range.start].category == R {
            1
        } else {
            range.len().min(3)
        };
        for g in &mut info[range.start..range.start + limit] {
            g.mask |= rphf;
        }
    }
}

/// The joining forms of `setup_topographical_masks`, in the order of
/// HarfBuzz's `use_topographical_features`.
const ISOL: usize = 0;
const INIT: usize = 1;
const MEDI: usize = 2;
const FINA: usize = 3;

/// HarfBuzz's `setup_topographical_masks`: each cluster joins the
/// cluster before it, so the clusters of a run of clusters get the
/// `init`, `medi`, and `fina` masks, and a cluster alone gets `isol`.
/// Hieroglyph clusters and characters outside a cluster break the run.
/// `masks` holds the mask of `isol`, `init`, `medi`, and `fina`, zero
/// for a feature the font lacks.
pub(super) fn setup_topographical_masks(info: &mut [GlyphInfo], masks: [u32; 4]) {
    let all = masks.iter().fold(0, |a, &m| a | m);
    if all == 0 {
        return;
    }
    let other = !all;
    let mut last_start = 0;
    let mut last_form: Option<usize> = None;
    for range in syllable_ranges(info) {
        let kind = info[range.start].syllable_type();
        if matches!(kind, syllable::HIEROGLYPH | syllable::NON_CLUSTER) {
            last_form = None;
        } else {
            let join = matches!(last_form, Some(FINA | ISOL));
            if join {
                // The cluster before continues into this one.
                let form = if last_form == Some(FINA) { MEDI } else { INIT };
                for g in &mut info[last_start..range.start] {
                    g.mask = (g.mask & other) | masks[form];
                }
            }
            let form = if join { FINA } else { ISOL };
            for g in &mut info[range.clone()] {
                g.mask = (g.mask & other) | masks[form];
            }
            last_form = Some(form);
        }
        last_start = range.start;
    }
}

/// HarfBuzz's `record_rphf_use`: in each syllable, the first glyph
/// `rphf` substituted among the leading glyphs it could apply to
/// becomes a repha.
pub(super) fn record_rphf(info: &mut [GlyphInfo], rphf: u32) {
    for range in syllable_ranges(info) {
        for g in &mut info[range] {
            if g.mask & rphf == 0 {
                break;
            }
            if g.substituted {
                g.category = R;
                break;
            }
        }
    }
}

/// HarfBuzz's `record_pref_use`: in each syllable, the first glyph
/// `pref` substituted becomes a pre-base vowel sign, which it behaves
/// like.
pub(super) fn record_pref(info: &mut [GlyphInfo]) {
    for range in syllable_ranges(info) {
        if let Some(g) = info[range].iter_mut().find(|g| g.substituted) {
            g.category = VPRE;
        }
    }
}

/// HarfBuzz's `POST_BASE_FLAGS64`: the categories a repha stops in
/// front of.
fn is_post_base(category: u8) -> bool {
    matches!(
        category,
        FABV | FBLW
            | FPST
            | FMABV
            | FMBLW
            | FMPST
            | MABV
            | MBLW
            | MPST
            | MPRE
            | VABV
            | VBLW
            | VPST
            | VPRE
            | VMABV
            | VMBLW
            | VMPST
            | VMPRE
    )
}

/// HarfBuzz's `is_halant_use`: a halant that did not ligate.
fn is_halant(g: &Glyph, info: &GlyphInfo) -> bool {
    matches!(info.category, H | HVM | IS) && !MatchGlyph::from(g).is_ligated()
}

/// HarfBuzz's `reorder_syllable_use` for every syllable. In a
/// syllable of a type that reorders, a repha at the start moves toward
/// the end, to just before the first post-base glyph or halant that
/// did not ligate, or to the end. Then each pre-base vowel sign or
/// modifier moves back to the start of the syllable, or to just after
/// the last halant before it that did not ligate. Both merge the
/// clusters they pass at the monotone `level`s. Only the first glyph
/// of a multiple substitution moves back.
///
/// HarfBuzz moves each pre-base glyph on its own, which costs time
/// quadratic in the number of pre-base glyphs one insertion point
/// collects. Here the moves to one insertion point are collected and
/// applied together (see [`move_to_insertion_point`]), so the pass
/// stays linear.
pub(super) fn reorder(glyphs: &mut [Glyph], info: &mut [GlyphInfo], level: ClusterLevel) {
    if glyphs.len() != info.len() {
        return;
    }
    let mut moves: Vec<usize> = Vec::new();
    let mut order: Vec<usize> = Vec::new();
    for range in syllable_ranges(info) {
        let kind = info[range.start].syllable_type();
        if !matches!(
            kind,
            syllable::VIRAMA_TERMINATED
                | syllable::SAKOT_TERMINATED
                | syllable::STANDARD
                | syllable::SYMBOL
                | syllable::BROKEN
        ) {
            continue;
        }
        move_repha(glyphs, info, range.clone(), level);
        let mut j = range.start;
        moves.clear();
        for i in range {
            // Moves only ever touch glyphs before `i`, so this is the
            // glyph's own category whether or not earlier moves ran.
            if is_halant(&glyphs[i], &info[i]) {
                move_to_insertion_point(glyphs, info, j, &moves, level, &mut order);
                moves.clear();
                j = i + 1;
            } else if matches!(info[i].category, VPRE | VMPRE)
                && MatchGlyph::from(&glyphs[i]).lig_comp() == 0
                && j < i
            {
                moves.push(i);
            }
        }
        move_to_insertion_point(glyphs, info, j, &moves, level, &mut order);
    }
}

/// The repha move of `reorder_syllable_use` for the syllable at
/// `range`.
fn move_repha(
    glyphs: &mut [Glyph],
    info: &mut [GlyphInfo],
    range: Range<usize>,
    level: ClusterLevel,
) {
    let (start, end) = (range.start, range.end);
    if end > glyphs.len() || end - start <= 1 || info[start].category != R {
        return;
    }
    for i in start + 1..end {
        let post_base = is_post_base(info[i].category) || is_halant(&glyphs[i], &info[i]);
        if post_base || i == end - 1 {
            let target = if post_base { i - 1 } else { i };
            merge_clusters(glyphs, start, target + 1, level);
            glyphs[start..=target].rotate_left(1);
            info[start..=target].rotate_left(1);
            return;
        }
    }
}

/// Applies the moves one insertion point `j` collected, in one pass:
/// each glyph at `moves` (ascending, all after `j`) moves to `j` in
/// turn, so the last one ends up first, and the glyphs they pass shift
/// right. That is the order moving them one at a time leaves.
///
/// One at a time, each move also merges the clusters from `j` through
/// the moved glyph. The ranges all start at `j` and grow, and the
/// glyphs of a merged range share one cluster, so the moves inside
/// it do not change any cluster. On a run whose clusters rise
/// monotonically, as they do at the monotone levels, merging the
/// widest range once leaves the same clusters as merging each range
/// in turn. At the other levels a merge marks the range unsafe to
/// break, and the widest range marks the same glyphs as the ranges in
/// turn, since the glyphs in each range are the ones it started with.
fn move_to_insertion_point(
    glyphs: &mut [Glyph],
    info: &mut [GlyphInfo],
    j: usize,
    moves: &[usize],
    level: ClusterLevel,
    order: &mut Vec<usize>,
) {
    let (Some(&first), Some(&last)) = (moves.first(), moves.last()) else {
        return;
    };
    let ascending = moves.windows(2).all(|w| w[0] < w[1]);
    if !ascending || first <= j || last >= glyphs.len() || last >= info.len() {
        return;
    }
    merge_clusters(glyphs, j, last + 1, level);
    order.clear();
    order.extend(moves.iter().rev());
    let mut next_move = moves.iter().peekable();
    for k in j..=last {
        if next_move.peek() == Some(&&k) {
            next_move.next();
        } else {
            order.push(k);
        }
    }
    // Ascending moves inside the span make the lengths match.
    if order.len() != last + 1 - j {
        return;
    }
    let moved_glyphs: Vec<Glyph> = order.iter().map(|&k| glyphs[k]).collect();
    let moved_info: Vec<GlyphInfo> = order.iter().map(|&k| info[k]).collect();
    glyphs[j..=last].copy_from_slice(&moved_glyphs);
    info[j..=last].copy_from_slice(&moved_info);
}
