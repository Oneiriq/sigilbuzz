//! The GSUB working buffer: HarfBuzz's output-buffer model
//! (`hb_buffer_t`'s `out_info`, `next_glyph`, `replace_glyphs`,
//! `output_glyph`, `move_to` and `sync` in `hb-buffer.hh` and
//! `hb-buffer.cc`).
//!
//! A lookup walks the run once. Glyphs it has passed sit in the
//! output, the ones still to come in the input, and each substitution
//! moves glyphs from the input to the output: a ligature consumes its
//! components and outputs one glyph, a multiple substitution consumes
//! one and outputs several. Nothing after the cursor moves, so a
//! lookup costs time linear in the run however many substitutions it
//! makes. Editing one flat vector in place instead shifts the whole
//! tail on every ligature, which is quadratic on long runs.
//!
//! One vector holds both halves with a gap between them: the output
//! is `buf[..out_len]`, the input `buf[idx..]`, and
//! `buf[out_len..idx]` is free. Consuming a glyph without output widens
//! the gap; outputting more glyphs than consumed narrows it, and when
//! it closes the input tail moves right by at least half the buffer,
//! so the moves cost amortized constant time per output glyph.
//!
//! Matching reads the buffer through its logical index: position `i`
//! is `buf[i]` in the output and `buf[i - out_len + idx]` in the
//! input, so the cursor (HarfBuzz's `buffer->idx`, the first input
//! glyph) is at `out_len`, backtrack walks read the output, and input
//! and lookahead walks read the input. HarfBuzz's `move_to` positions
//! are these logical indices too.

use alloc::vec::Vec;

use super::gsub::substitute_glyph;
use crate::buffer::{ClusterLevel, Glyph};
use crate::tables::layout::skip_iter::{MatchGlyph, MatchSeq};

/// Smallest number of free slots a closed gap reopens with.
const MIN_GAP: usize = 8;

/// The run one GSUB lookup (or one feature's lookups) works on.
#[derive(Debug)]
pub(super) struct GsubBuffer {
    /// Output, gap, then input.
    buf: Vec<Glyph>,
    /// Whether the feature being applied is on at each slot of `buf`,
    /// for features only some glyphs carry. Moves with its glyph.
    mask: Option<Vec<bool>>,
    /// Length of the output.
    out_len: usize,
    /// Start of the input in `buf`: the cursor glyph.
    idx: usize,
    /// Ligature ids (bit `n` for id `n`) glyphs of the run carried when
    /// the pass started or ligatures formed since took.
    lig_ids_used: u8,
    /// The last ligature id handed out once all seven were in use.
    lig_serial: u8,
}

impl GsubBuffer {
    /// A buffer over `glyphs`, with the feature on at the glyphs whose
    /// `mask` entry is true (on at every glyph without a mask; off past
    /// the end of a short mask).
    pub(super) fn new(glyphs: Vec<Glyph>, mask: Option<&[bool]>) -> Self {
        let mask = mask.map(|m| {
            (0..glyphs.len())
                .map(|i| m.get(i).copied().unwrap_or(false))
                .collect()
        });
        Self {
            buf: glyphs,
            mask,
            out_len: 0,
            idx: 0,
            lig_ids_used: 0,
            lig_serial: 0,
        }
    }

    /// The glyphs, once no pass is running.
    pub(super) fn into_glyphs(mut self) -> Vec<Glyph> {
        self.sync();
        self.buf
    }

    /// The glyphs between passes.
    pub(super) fn glyphs(&self) -> &[Glyph] {
        &self.buf
    }

    /// Starts a pass, HarfBuzz's `clear_output`: every glyph is input
    /// and the cursor is on the first.
    pub(super) fn clear_output(&mut self) {
        self.sync();
        self.lig_ids_used = self
            .buf
            .iter()
            .fold(0, |used, g| used | 1 << super::lig::lig_id(g));
    }

    /// Ends a pass, HarfBuzz's `sync`: the rest of the input follows
    /// the output, and the result is the run for the next pass.
    pub(super) fn sync(&mut self) {
        if self.idx > self.out_len {
            self.buf.drain(self.out_len..self.idx);
            if let Some(mask) = &mut self.mask {
                mask.drain(self.out_len..self.idx);
            }
        }
        self.out_len = 0;
        self.idx = 0;
    }

    /// Number of glyphs in the run: output plus input.
    pub(super) fn len(&self) -> usize {
        self.out_len + (self.buf.len() - self.idx)
    }

    /// The cursor's logical index, HarfBuzz's `out_len` (the backtrack
    /// length).
    pub(super) const fn cursor(&self) -> usize {
        self.out_len
    }

    /// True while there is input left.
    pub(super) fn has_input(&self) -> bool {
        self.idx < self.buf.len()
    }

    /// The slot of `buf` holding logical position `i`.
    fn slot(&self, i: usize) -> usize {
        if i < self.out_len {
            i
        } else {
            i - self.out_len + self.idx
        }
    }

    /// The glyph at logical position `i`.
    pub(super) fn get(&self, i: usize) -> Option<&Glyph> {
        if i >= self.len() {
            return None;
        }
        self.buf.get(self.slot(i))
    }

    /// The glyph at logical position `i`, mutably.
    pub(super) fn get_mut(&mut self, i: usize) -> Option<&mut Glyph> {
        if i >= self.len() {
            return None;
        }
        let slot = self.slot(i);
        self.buf.get_mut(slot)
    }

    /// The cursor glyph, HarfBuzz's `cur()`.
    pub(super) fn cur(&self) -> Option<&Glyph> {
        self.buf.get(self.idx)
    }

    /// The cursor glyph, mutably.
    pub(super) fn cur_mut(&mut self) -> Option<&mut Glyph> {
        self.buf.get_mut(self.idx)
    }

    /// True when the feature is on at the cursor glyph.
    pub(super) fn cur_in_mask(&self) -> bool {
        self.mask
            .as_ref()
            .map_or(true, |m| m.get(self.idx).copied().unwrap_or(false))
    }

    /// True when the feature is on at logical position `i`.
    pub(super) fn in_mask_at(&self, i: usize) -> bool {
        self.mask
            .as_ref()
            .map_or(true, |m| m.get(self.slot(i)).copied().unwrap_or(false))
    }

    /// Copies slot `from` to slot `to`, mask included.
    fn copy_slot(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        if let Some(&g) = self.buf.get(from) {
            if let Some(dst) = self.buf.get_mut(to) {
                *dst = g;
            }
        }
        if let Some(mask) = &mut self.mask {
            if let Some(&m) = mask.get(from) {
                if let Some(dst) = mask.get_mut(to) {
                    *dst = m;
                }
            }
        }
    }

    /// HarfBuzz's `next_glyph`: the cursor glyph moves to the output
    /// unchanged.
    pub(super) fn next_glyph(&mut self) {
        if !self.has_input() {
            return;
        }
        self.copy_slot(self.idx, self.out_len);
        self.out_len += 1;
        self.idx += 1;
    }

    /// HarfBuzz's `next_glyphs`: `n` glyphs move to the output.
    pub(super) fn next_glyphs(&mut self, n: usize) {
        let n = n.min(self.buf.len() - self.idx);
        if self.out_len != self.idx {
            self.buf.copy_within(self.idx..self.idx + n, self.out_len);
            if let Some(mask) = &mut self.mask {
                mask.copy_within(self.idx..self.idx + n, self.out_len);
            }
        }
        self.out_len += n;
        self.idx += n;
    }

    /// HarfBuzz's `skip_glyph`: the cursor glyph is dropped.
    pub(super) fn skip_glyph(&mut self) {
        if self.has_input() {
            self.idx += 1;
        }
    }

    /// HarfBuzz's `delete_glyph`: drops the cursor glyph, and when its
    /// cluster would vanish with it, merges the cluster into a
    /// neighbor: backward into the glyphs output before it at every
    /// level, or forward into the next glyph at the monotone levels.
    pub(super) fn delete_glyph(&mut self, level: ClusterLevel) {
        let Some(cluster) = self.cur().map(|g| g.cluster) else {
            return;
        };
        let next = self.buf.get(self.idx + 1).map(|g| g.cluster);
        let prev = self
            .out_len
            .checked_sub(1)
            .and_then(|i| self.buf.get(i))
            .map(|g| g.cluster);
        if next == Some(cluster) || prev == Some(cluster) {
            // The cluster survives.
        } else if let Some(old) = prev {
            if cluster < old {
                let mut i = self.out_len;
                while let Some(g) = i.checked_sub(1).and_then(|p| self.buf.get_mut(p)) {
                    if g.cluster != old {
                        break;
                    }
                    g.cluster = cluster;
                    i -= 1;
                }
            }
        } else if next.is_some() {
            let at = self.cursor();
            self.merge_clusters(at, at + 2, level);
        }
        self.skip_glyph();
    }

    /// HarfBuzz's `replace_glyph` for a GSUB substitution: the cursor
    /// glyph becomes `gid` and moves to the output.
    pub(super) fn replace_glyph(&mut self, gid: u16) {
        if let Some(g) = self.cur_mut() {
            substitute_glyph(g, gid);
        }
        self.next_glyph();
    }

    /// HarfBuzz's `output_glyph` / `output_info`: `glyph` joins the
    /// output ahead of the cursor, with the cursor glyph's mask; the
    /// cursor stays.
    pub(super) fn output_glyph(&mut self, glyph: Glyph) {
        self.ensure_gap(1);
        let on = self.cur_in_mask();
        if let Some(slot) = self.buf.get_mut(self.out_len) {
            *slot = glyph;
        }
        if let Some(slot) = self.mask.as_mut().and_then(|m| m.get_mut(self.out_len)) {
            *slot = on;
        }
        self.out_len += 1;
    }

    /// Makes room for `n` more output glyphs than the gap holds, by
    /// moving the input right by at least half the buffer.
    fn ensure_gap(&mut self, n: usize) {
        let gap = self.idx - self.out_len;
        if gap >= n {
            return;
        }
        let grow = (n - gap).max(self.buf.len() / 2).max(MIN_GAP);
        let filler = core::iter::repeat(Glyph::new(0, 0)).take(grow);
        self.buf.splice(self.idx..self.idx, filler);
        if let Some(mask) = &mut self.mask {
            mask.splice(self.idx..self.idx, core::iter::repeat(false).take(grow));
        }
        self.idx += grow;
    }

    /// HarfBuzz's `move_to`: puts the cursor at logical position `i`,
    /// moving glyphs between the input and the output. Past the end it
    /// stops at the end.
    pub(super) fn move_to(&mut self, i: usize) {
        let i = i.min(self.len());
        if self.out_len < i {
            self.next_glyphs(i - self.out_len);
        } else if self.out_len > i {
            // The output glyphs after `i` go back to the input, in
            // front of the cursor. The gap never makes that overlap.
            let count = self.out_len - i;
            let dst = self.idx - count;
            self.buf.copy_within(i..self.out_len, dst);
            if let Some(mask) = &mut self.mask {
                mask.copy_within(i..self.out_len, dst);
            }
            self.idx = dst;
            self.out_len = i;
        }
    }

    /// A ligature id for a new ligature: the smallest no glyph of the
    /// run had when the pass started and no ligature took since, or,
    /// once all seven are taken, the next in turn as HarfBuzz's
    /// buffer serial would give.
    pub(super) fn alloc_lig_id(&mut self) -> u8 {
        let free = (1..8u8).find(|&id| self.lig_ids_used & (1 << id) == 0);
        let id = free.unwrap_or_else(|| {
            self.lig_serial = self.lig_serial % 7 + 1;
            self.lig_serial
        });
        self.lig_ids_used |= 1 << id;
        id
    }

    /// HarfBuzz's `merge_clusters(start, end)` over input positions
    /// (`start` at or after the cursor): at the monotone levels the
    /// range takes its smallest cluster, spreading to neighbors that
    /// shared a cluster with an end whose cluster changes, into the
    /// output when the range starts at the cursor.
    pub(super) fn merge_clusters(&mut self, start: usize, end: usize, level: ClusterLevel) {
        let len = self.len();
        let end = end.min(len);
        if end < start + 2 || !level.is_monotone() {
            return;
        }
        let cl = |b: &Self, i: usize| b.get(i).map_or(0, |g| g.cluster);
        let Some(cluster) = (start..end).map(|i| cl(self, i)).min() else {
            return;
        };
        let mut end = end;
        if cluster != cl(self, end - 1) {
            while end < len && cl(self, end - 1) == cl(self, end) {
                end += 1;
            }
        }
        let mut start = start;
        if cluster != cl(self, start) {
            while self.cursor() < start && cl(self, start - 1) == cl(self, start) {
                start -= 1;
            }
        }
        let start_cluster = cl(self, start);
        if start == self.cursor() && start_cluster != cluster {
            let mut i = self.out_len;
            while i > 0 && cl(self, i - 1) == start_cluster {
                i -= 1;
                if let Some(g) = self.get_mut(i) {
                    g.cluster = cluster;
                }
            }
        }
        for i in start..end {
            if let Some(g) = self.get_mut(i) {
                g.cluster = cluster;
            }
        }
    }
}

impl MatchSeq for GsubBuffer {
    fn len(&self) -> usize {
        GsubBuffer::len(self)
    }

    fn glyph(&self, i: usize) -> Option<MatchGlyph> {
        self.get(i).map(MatchGlyph::from)
    }

    fn in_mask(&self, i: usize) -> bool {
        self.in_mask_at(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn run(ids: &[u32]) -> Vec<Glyph> {
        ids.iter()
            .enumerate()
            .map(|(i, &g)| Glyph::new(g, i as u32))
            .collect()
    }

    fn ids(b: &GsubBuffer) -> Vec<u32> {
        (0..b.len())
            .filter_map(|i| b.get(i))
            .map(|g| g.glyph_id)
            .collect()
    }

    #[test]
    fn output_and_input_halves_read_as_one_run() {
        let mut b = GsubBuffer::new(run(&[1, 2, 3, 4]), None);
        b.clear_output();
        b.next_glyph();
        b.skip_glyph();
        assert_eq!((b.cursor(), b.len()), (1, 3));
        assert_eq!(ids(&b), [1, 3, 4]);
        b.output_glyph(Glyph::new(9, 1));
        b.output_glyph(Glyph::new(8, 1));
        assert_eq!(ids(&b), [1, 9, 8, 3, 4]);
        assert_eq!(b.cursor(), 3);
        b.sync();
        assert_eq!(
            b.glyphs().iter().map(|g| g.glyph_id).collect::<Vec<_>>(),
            [1, 9, 8, 3, 4]
        );
    }

    #[test]
    fn move_to_rewinds_and_advances_through_the_output() {
        let mut b = GsubBuffer::new(run(&[1, 2, 3, 4, 5]), None);
        b.clear_output();
        b.next_glyphs(4);
        b.move_to(1);
        assert_eq!((b.cursor(), ids(&b)), (1, vec![1, 2, 3, 4, 5]));
        b.replace_glyph(7);
        b.move_to(4);
        assert_eq!((b.cursor(), ids(&b)), (4, vec![1, 7, 3, 4, 5]));
        b.move_to(usize::MAX);
        assert_eq!(b.cursor(), 5);
        assert!(!b.has_input());
    }

    #[test]
    fn the_mask_moves_with_its_glyph() {
        let mask = [true, false, true];
        let mut b = GsubBuffer::new(run(&[1, 2, 3]), Some(&mask));
        b.clear_output();
        b.skip_glyph();
        assert!(!b.cur_in_mask());
        b.output_glyph(Glyph::new(5, 1));
        b.next_glyph();
        assert!(b.cur_in_mask());
        assert_eq!(
            (0..b.len()).map(|i| b.in_mask_at(i)).collect::<Vec<_>>(),
            [false, false, true]
        );
    }

    #[test]
    fn a_long_expansion_moves_the_tail_a_few_times_only() {
        let mut b = GsubBuffer::new(run(&[1; 1000]), None);
        b.clear_output();
        let mut grows = 0;
        while b.has_input() {
            for gid in [2, 3] {
                if b.idx == b.out_len {
                    grows += 1;
                }
                b.output_glyph(Glyph::new(gid, 0));
            }
            b.skip_glyph();
        }
        b.sync();
        assert_eq!(b.glyphs().len(), 2000);
        assert!(grows < 20, "{grows} gap refills");
    }

    #[test]
    fn ligature_ids_avoid_live_ones_then_cycle() {
        let mut glyphs = run(&[1, 2]);
        glyphs[0].unicode_props = 2 << 13;
        let mut b = GsubBuffer::new(glyphs, None);
        b.clear_output();
        let got: Vec<u8> = (0..9).map(|_| b.alloc_lig_id()).collect();
        assert_eq!(got, [1, 3, 4, 5, 6, 7, 1, 2, 3]);
    }

    #[test]
    fn merging_at_the_cursor_reaches_into_the_output() {
        // Clusters 0 5 5 | 5 2: the input range [3, 5) merges to 2 and
        // the output glyphs sharing cluster 5 follow.
        let glyphs: Vec<Glyph> = [0, 5, 5, 5, 2].iter().map(|&c| Glyph::new(1, c)).collect();
        let mut b = GsubBuffer::new(glyphs, None);
        b.clear_output();
        b.next_glyphs(3);
        b.merge_clusters(3, 5, ClusterLevel::MonotoneCharacters);
        let clusters: Vec<u32> = (0..5).filter_map(|i| b.get(i)).map(|g| g.cluster).collect();
        assert_eq!(clusters, [0, 2, 2, 2, 2]);
        let mut b = GsubBuffer::new(run(&[1, 1]), None);
        b.merge_clusters(0, 2, ClusterLevel::Characters);
        assert_eq!(b.get(1).map(|g| g.cluster), Some(1));
    }
}
