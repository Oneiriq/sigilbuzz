//! HarfBuzz's `hb_buffer_t::sort` (`hb-buffer.cc`), which the Myanmar
//! shaper sorts a syllable with: an insertion sort by position, where
//! each glyph that moves first merges the clusters from its new slot
//! to its old one (`merge_clusters (j, i + 1)`).
//!
//! The order is a stable sort. Moving glyphs one by one costs time
//! quadratic in the syllable, so the merges are worked out without the
//! moves.
//!
//! At the monotone cluster levels a merge gives its range the range's
//! smallest cluster, spread over the neighbors that share a cluster
//! with either end. Seen as maximal runs of equal clusters in the
//! current order, that is: every run the range touches takes the
//! smallest cluster among them. A glyph moves inside the runs it just
//! merged, so the runs stay contiguous. While the sort runs, the glyphs
//! before the moving one are sorted, and the ones the move passes are
//! the last of them, so their runs form a stack. Each merge pops runs
//! off it and joins them in a union-find, which keeps the whole sort
//! close to linear. A glyph whose cluster changes loses its flags, as
//! `set_cluster` clears them.
//!
//! At the other levels a merge only marks its range unsafe to break,
//! and HarfBuzz skips ranges wider than 255 glyphs, so only moves past
//! fewer than 255 glyphs matter. Those glyphs are the sorted ones whose
//! position is past the moving glyph's.

use alloc::vec;
use alloc::vec::Vec;

use crate::buffer::{ClusterLevel, Glyph, GlyphFlags};
use crate::ot::syllabic::GlyphInfo;

/// HarfBuzz's cap on the width of a glyph flag range.
const MAX_FLAG_RANGE: usize = 255;

/// Sorts `glyphs[start..end]` and `info[start..end]` stably by
/// [`GlyphInfo::position`], merging clusters as HarfBuzz's insertion
/// sort does at cluster `level`.
pub(super) fn sort_by_position(
    glyphs: &mut [Glyph],
    info: &mut [GlyphInfo],
    start: usize,
    end: usize,
    level: ClusterLevel,
) {
    if end > glyphs.len() || end > info.len() || end < start + 2 {
        return;
    }
    let n = end - start;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&k| info[start + k].position);
    if order.iter().enumerate().all(|(slot, &k)| slot == k) {
        return;
    }
    // Each glyph's position as an index into the positions the
    // syllable uses, smallest first.
    let mut distinct: Vec<u8> = info[start..end].iter().map(|g| g.position).collect();
    distinct.sort_unstable();
    distinct.dedup();
    let ranks: Vec<usize> = info[start..end]
        .iter()
        .map(|g| distinct.partition_point(|&p| p < g.position))
        .collect();
    if level.is_monotone() {
        merge_moves(glyphs, start, end, &ranks, distinct.len());
    } else {
        flag_moves(glyphs, start, &ranks, distinct.len(), level);
    }
    let sorted_glyphs: Vec<Glyph> = order.iter().map(|&k| glyphs[start + k]).collect();
    let sorted_info: Vec<GlyphInfo> = order.iter().map(|&k| info[start + k]).collect();
    glyphs[start..end].copy_from_slice(&sorted_glyphs);
    info[start..end].copy_from_slice(&sorted_info);
}

/// A union-find over runs of glyphs, each set carrying its cluster.
struct Runs {
    parent: Vec<usize>,
    cluster: Vec<u32>,
}

impl Runs {
    fn find(&mut self, mut x: usize) -> usize {
        while let Some(&p) = self.parent.get(x) {
            if p == x {
                break;
            }
            let grand = self.parent.get(p).copied().unwrap_or(p);
            if let Some(slot) = self.parent.get_mut(x) {
                *slot = grand;
            }
            x = p;
        }
        x
    }

    fn value(&mut self, x: usize) -> u32 {
        let root = self.find(x);
        self.cluster.get(root).copied().unwrap_or(0)
    }

    /// Joins the sets of `a` and `b` with cluster `value`, returning
    /// the new root.
    fn join(&mut self, a: usize, b: usize, value: u32) -> usize {
        let (ra, rb) = (self.find(a), self.find(b));
        if let Some(slot) = self.parent.get_mut(rb) {
            *slot = ra;
        }
        if let Some(slot) = self.cluster.get_mut(ra) {
            *slot = value;
        }
        ra
    }
}

/// HarfBuzz's `set_cluster`: a glyph whose cluster changes loses its
/// flags.
fn set_cluster(g: &mut Glyph, cluster: u32) {
    if g.cluster != cluster {
        g.flags = GlyphFlags::empty();
    }
    g.cluster = cluster;
}

/// The merges of the sort at the monotone levels. `ranks` holds each
/// glyph's position rank, below `buckets`.
fn merge_moves(glyphs: &mut [Glyph], start: usize, end: usize, ranks: &[usize], buckets: usize) {
    let n = end - start;
    let len = glyphs.len();
    let initial: Vec<u32> = glyphs[start..end].iter().map(|g| g.cluster).collect();
    // run_end[k]: one past the run of equal clusters that holds glyph
    // k in the syllable's original order.
    let mut run_end = vec![0usize; n];
    for k in (0..n).rev() {
        run_end[k] = if initial.get(k + 1) == initial.get(k) {
            run_end.get(k + 1).copied().unwrap_or(k + 1)
        } else {
            k + 1
        };
    }
    // Two more sets: the glyphs before the syllable and after it that
    // joined its runs, `lo..start` and `end..hi`.
    let (before, after) = (n, n + 1);
    let mut runs = Runs {
        parent: (0..n + 2).collect(),
        cluster: initial.clone(),
    };
    runs.cluster.push(initial[0]);
    runs.cluster.push(initial[n - 1]);
    for k in 1..n {
        if initial[k] == initial[k - 1] {
            runs.join(k - 1, k, initial[k]);
        }
    }
    let mut lo = start;
    while lo > 0 && glyphs[lo - 1].cluster == initial[0] {
        lo -= 1;
    }
    if lo < start {
        runs.join(0, before, initial[0]);
    }
    let mut hi = end;
    while hi < len && glyphs[hi].cluster == initial[n - 1] {
        hi += 1;
    }
    if hi > end {
        runs.join(n - 1, after, initial[n - 1]);
    }

    // The runs of the sorted glyphs, as (member, first rank), and how
    // far the last of them reaches into the unsorted glyphs.
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    let mut reach = run_end[0];
    let mut counts = vec![0usize; buckets];
    if let Some(c) = counts.get_mut(ranks[0]) {
        *c += 1;
    }
    let mut members: Vec<usize> = Vec::new();
    for i in 1..n {
        let rank = ranks[i];
        // The slot glyph i moves to: past every sorted glyph whose
        // position is not greater than its own.
        let j: usize = counts.iter().take(rank + 1).sum();
        if let Some(c) = counts.get_mut(rank) {
            *c += 1;
        }
        let in_last_run = reach > i;
        if j >= i {
            if !in_last_run {
                stack.push((i, i));
                reach = run_end[i];
            }
            continue;
        }
        // Merge every run from the one holding slot j to glyph i's.
        members.clear();
        if !in_last_run {
            members.push(i);
            reach = run_end[i];
        }
        let mut first = j;
        while let Some((member, first_rank)) = stack.pop() {
            members.push(member);
            first = first_rank;
            if first_rank <= j {
                break;
            }
        }
        let Some(m) = members.iter().map(|&x| runs.value(x)).min() else {
            continue;
        };
        let mut root = members[0];
        for &x in &members[1..] {
            root = runs.join(root, x, m);
        }
        root = runs.join(root, root, m);
        // A neighbor run with the same cluster joins too.
        if reach < n && initial[reach] == m {
            root = runs.join(root, reach, m);
            reach = run_end[reach];
        }
        if reach >= n && hi < len && glyphs[hi].cluster == m {
            while hi < len && glyphs[hi].cluster == m {
                hi += 1;
            }
            root = runs.join(root, after, m);
        }
        match stack.last().copied() {
            Some((below, below_first)) => {
                if runs.value(below) == m {
                    stack.pop();
                    root = runs.join(root, below, m);
                    first = below_first;
                }
            }
            None => {
                if lo > 0 && glyphs[lo - 1].cluster == m {
                    while lo > 0 && glyphs[lo - 1].cluster == m {
                        lo -= 1;
                    }
                    root = runs.join(root, before, m);
                }
            }
        }
        stack.push((root, first));
    }

    for k in 0..n {
        let v = runs.value(k);
        set_cluster(&mut glyphs[start + k], v);
    }
    let v = runs.value(before);
    for g in &mut glyphs[lo..start] {
        set_cluster(g, v);
    }
    let v = runs.value(after);
    for g in &mut glyphs[end..hi] {
        set_cluster(g, v);
    }
}

/// The merges of the sort at the other levels, which only mark ranges
/// unsafe to break.
fn flag_moves(
    glyphs: &mut [Glyph],
    start: usize,
    ranks: &[usize],
    buckets: usize,
    level: ClusterLevel,
) {
    // The sorted glyphs of each position, in their original order.
    let mut sorted: Vec<Vec<usize>> = vec![Vec::new(); buckets];
    let mut range: Vec<usize> = Vec::new();
    let mut scratch: Vec<Glyph> = Vec::new();
    for (i, &rank) in ranks.iter().enumerate() {
        let later = sorted.get(rank + 1..).unwrap_or_default();
        let passed: usize = later.iter().map(Vec::len).sum();
        if passed > 0 && passed < MAX_FLAG_RANGE {
            range.clear();
            range.extend(later.iter().flatten().copied());
            range.push(i);
            scratch.clear();
            scratch.extend(range.iter().map(|&k| glyphs[start + k]));
            let len = scratch.len();
            crate::shape::unsafe_to_break(&mut scratch, 0, len, level);
            for (&k, g) in range.iter().zip(&scratch) {
                glyphs[start + k].flags = g.flags;
            }
        }
        if let Some(list) = sorted.get_mut(rank) {
            list.push(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HarfBuzz's insertion sort, one move at a time.
    fn reference(
        glyphs: &mut [Glyph],
        info: &mut [GlyphInfo],
        start: usize,
        end: usize,
        level: ClusterLevel,
    ) {
        for i in start + 1..end {
            let mut j = i;
            while j > start && info[j - 1].position > info[i].position {
                j -= 1;
            }
            if i == j {
                continue;
            }
            crate::shape::merge_clusters(glyphs, j, i + 1, level);
            glyphs[j..=i].rotate_right(1);
            info[j..=i].rotate_right(1);
        }
    }

    fn state(glyphs: &[Glyph]) -> Vec<(u32, u32, u32)> {
        glyphs
            .iter()
            .map(|g| (g.glyph_id, g.cluster, g.flags.bits()))
            .collect()
    }

    /// Glyph `k` of `clusters` gets glyph id `k` and position
    /// `positions[k]`, and the syllable is `start..end`.
    fn check(clusters: &[u32], positions: &[u8], start: usize, end: usize) {
        for level in [
            ClusterLevel::MonotoneGraphemes,
            ClusterLevel::MonotoneCharacters,
            ClusterLevel::Characters,
            ClusterLevel::Graphemes,
        ] {
            let mut glyphs: Vec<Glyph> = clusters
                .iter()
                .enumerate()
                .map(|(k, &c)| {
                    let mut g = Glyph::new(k as u32, c);
                    g.flags = GlyphFlags::from_bits_truncate(k as u32 % 2);
                    g
                })
                .collect();
            let mut info: Vec<GlyphInfo> = positions
                .iter()
                .map(|&position| GlyphInfo {
                    position,
                    ..GlyphInfo::default()
                })
                .collect();
            let (mut g2, mut i2) = (glyphs.clone(), info.clone());
            reference(&mut g2, &mut i2, start, end, level);
            sort_by_position(&mut glyphs, &mut info, start, end, level);
            assert_eq!(
                state(&glyphs),
                state(&g2),
                "{clusters:?} {positions:?} at {level:?}"
            );
        }
    }

    #[test]
    fn merges_match_the_insertion_sort() {
        // A pre-base vowel past a base and two signs.
        check(&[0, 3, 6, 9], &[4, 5, 5, 2], 0, 4);
        // Medial ra and a pre-base vowel, in one grapheme.
        check(&[0, 0, 0, 0], &[4, 3, 5, 2], 0, 4);
        // Neighbors that share a cluster with the syllable's ends.
        check(&[1, 4, 4, 6, 8, 8, 8, 9], &[0, 4, 5, 3, 5, 2, 0, 0], 1, 6);
        check(&[2, 2, 2, 5, 7, 7], &[0, 4, 5, 5, 2, 0], 2, 5);
        // Clusters out of order, as right-to-left text leaves them.
        check(&[9, 9, 7, 5, 5, 3, 2], &[4, 5, 5, 3, 2, 8, 7], 0, 7);
        check(&[5, 5, 9, 3, 3, 7, 1], &[0, 4, 9, 5, 2, 2, 0], 1, 6);
        check(&[3, 8, 2, 8, 2, 1, 6], &[4, 8, 7, 9, 2, 5, 3], 0, 7);
    }

    #[test]
    fn merges_match_the_insertion_sort_on_generated_syllables() {
        // A small linear congruential generator keeps the cases fixed.
        let mut seed = 0x2545_F491_u32;
        let mut next = |m: u32| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (seed >> 16) % m
        };
        for _ in 0..2_000 {
            let n = 2 + next(9) as usize;
            let mut clusters = Vec::new();
            let mut c = 0;
            for _ in 0..n + 2 {
                c += next(3);
                clusters.push(if next(5) == 0 { next(12) } else { c });
            }
            let positions: Vec<u8> = (0..n + 2).map(|_| 2 + next(8) as u8).collect();
            check(&clusters, &positions, 1, n + 1);
        }
    }

    #[test]
    fn long_runs_sort_in_linear_time() {
        // A base, 20,000 asats, and 20,000 pre-base vowels: each vowel
        // moves past every asat.
        const N: usize = 20_000;
        let mut positions = vec![4u8];
        positions.extend(core::iter::repeat(5).take(N));
        positions.extend(core::iter::repeat(2).take(N));
        for level in [ClusterLevel::MonotoneCharacters, ClusterLevel::Characters] {
            let mut glyphs: Vec<Glyph> = (0..positions.len())
                .map(|k| Glyph::new(k as u32, k as u32))
                .collect();
            let mut info: Vec<GlyphInfo> = positions
                .iter()
                .map(|&position| GlyphInfo {
                    position,
                    ..GlyphInfo::default()
                })
                .collect();
            let end = glyphs.len();
            sort_by_position(&mut glyphs, &mut info, 0, end, level);
            assert_eq!(glyphs[0].glyph_id as usize, N + 1);
            assert_eq!(glyphs[N].glyph_id, 0);
        }
    }
}
