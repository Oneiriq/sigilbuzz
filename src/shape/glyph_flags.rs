//! Glyph flags, HarfBuzz's `unsafe_to_break` and `unsafe_to_concat`
//! (`_set_glyph_flags` and its helpers in `hb-buffer.hh`).
//!
//! Shaping marks a range of glyphs whenever their output depends on
//! the glyphs around them: a contextual or pair lookup that matched
//! (unsafe to break), one that looked at glyphs and failed (unsafe to
//! concatenate), a cluster merge a cluster level skipped, a syllable,
//! and so on. [`propagate`] then gives every glyph of a cluster the
//! flags of any of them, as `hb_propagate_flags` does, and drops the
//! concatenation flag unless the buffer asked for it.
//!
//! An unsafe-to-break range is "interior": the glyphs of its smallest
//! cluster are left alone, since breaking before that cluster is
//! still fine. Which glyphs count depends on the cluster level
//! (`_infos_set_glyph_flags`, `_infos_find_min_cluster`).
//!
//! Every call touches at most 255 glyphs: HarfBuzz ignores wider
//! ranges, which keeps the cost of flags linear in the run.

use super::cluster::Clustered;
use crate::buffer::{BufferFlags, ClusterLevel, Glyph, GlyphFlags};
use crate::tables::layout::skip_iter::UnsafeRanges;

/// HarfBuzz's cap on the width of a flag range.
const MAX_RANGE: usize = 255;

/// The flags `unsafe_to_break` sets: unsafe to break is also unsafe
/// to concatenate.
pub(super) const BREAK: GlyphFlags =
    GlyphFlags::UNSAFE_TO_BREAK.union(GlyphFlags::UNSAFE_TO_CONCAT);

/// The flags `unsafe_to_concat` sets.
pub(super) const CONCAT: GlyphFlags = GlyphFlags::UNSAFE_TO_CONCAT;

/// HarfBuzz's `set_cluster`: gives `glyph` `cluster` and, when that
/// changes its cluster, replaces its flags with `flags`.
pub(super) fn set_cluster<T: Clustered>(item: &mut T, cluster: u32, flags: GlyphFlags) {
    if item.cluster() != cluster {
        item.set_flags(flags);
    }
    item.set_cluster(cluster);
}

/// `_infos_find_min_cluster`: the smallest of `cluster` and the
/// clusters of `infos` (at the non-character levels, of its ends).
pub(super) fn find_min_cluster<T: Clustered>(
    infos: &[T],
    cluster: u32,
    level: ClusterLevel,
) -> u32 {
    let (Some(first), Some(last)) = (infos.first(), infos.last()) else {
        return cluster;
    };
    if level == ClusterLevel::Characters {
        infos.iter().fold(cluster, |c, g| c.min(g.cluster()))
    } else {
        cluster.min(first.cluster()).min(last.cluster())
    }
}

/// `_infos_set_glyph_flags`: adds `flags` to the glyphs of `infos`
/// outside the cluster `cluster`, the smallest of the range. At the
/// monotone levels (and at the grapheme level) only the glyphs on the
/// far side of the run of that cluster at one end count.
pub(super) fn set_infos<T: Clustered>(
    infos: &mut [T],
    cluster: u32,
    flags: GlyphFlags,
    level: ClusterLevel,
) {
    let (Some(first), Some(last)) = (infos.first(), infos.last()) else {
        return;
    };
    let (first, last) = (first.cluster(), last.cluster());
    if level == ClusterLevel::Characters || (cluster != first && cluster != last) {
        for g in infos.iter_mut().filter(|g| g.cluster() != cluster) {
            g.add_flags(flags);
        }
        return;
    }
    if cluster == first {
        for g in infos.iter_mut().rev().take_while(|g| g.cluster() != first) {
            g.add_flags(flags);
        }
    } else {
        for g in infos.iter_mut().take_while(|g| g.cluster() != last) {
            g.add_flags(flags);
        }
    }
}

/// True when HarfBuzz ignores a flag range from `start` to `end`: one
/// wider than 255 glyphs, or one whose end comes before its start.
pub(super) const fn too_wide(start: usize, end: usize) -> bool {
    end < start || end - start > MAX_RANGE
}

/// `_set_glyph_flags` on a run without an output buffer: adds `flags`
/// to `glyphs[start..end]`, only outside the range's smallest cluster
/// when `interior`.
pub(super) fn set_flags<T: Clustered>(
    glyphs: &mut [T],
    start: usize,
    end: usize,
    flags: GlyphFlags,
    interior: bool,
    level: ClusterLevel,
) {
    if too_wide(start, end) {
        return;
    }
    let end = end.min(glyphs.len());
    if start >= end || (interior && end - start < 2) {
        return;
    }
    let Some(range) = glyphs.get_mut(start..end) else {
        return;
    };
    if interior {
        let cluster = find_min_cluster(range, u32::MAX, level);
        set_infos(range, cluster, flags, level);
    } else {
        for g in range {
            g.add_flags(flags);
        }
    }
}

/// `hb_buffer_t::unsafe_to_break(start, end)` on a run without an
/// output buffer.
pub(super) fn unsafe_to_break<T: Clustered>(
    glyphs: &mut [T],
    start: usize,
    end: usize,
    level: ClusterLevel,
) {
    set_flags(glyphs, start, end, BREAK, true, level);
}

/// `hb_buffer_t::unsafe_to_concat(start, end)` on a run without an
/// output buffer.
pub(super) fn unsafe_to_concat<T: Clustered>(glyphs: &mut [T], start: usize, end: usize) {
    set_flags(glyphs, start, end, CONCAT, false, ClusterLevel::Characters);
}

/// `hb_buffer_t::unsafe_to_concat()` with no range: every glyph.
pub(super) fn unsafe_to_concat_all(glyphs: &mut [Glyph]) {
    for g in glyphs {
        g.flags |= CONCAT;
    }
}

/// What the flag calls of one shaping call share: its cluster level,
/// and whether the buffer asked for unsafe-to-concatenate flags (the
/// calls skip them otherwise, as HarfBuzz's `unsafe_to_concat` does)
/// and for safe-to-insert-tatweel flags.
#[derive(Debug, Clone, Copy)]
pub(super) struct FlagCx {
    pub(super) level: ClusterLevel,
    pub(super) concat: bool,
    pub(super) tatweel: bool,
}

impl FlagCx {
    /// Flags for a buffer with `level` and `flags`.
    pub(super) fn new(level: ClusterLevel, flags: BufferFlags) -> Self {
        Self {
            level,
            concat: flags.contains(BufferFlags::PRODUCE_UNSAFE_TO_CONCAT),
            tatweel: flags.contains(BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL),
        }
    }

    /// HarfBuzz's `safe_to_insert_tatweel`: the range may take a
    /// tatweel when the buffer asked for that flag, else it is unsafe to
    /// break.
    pub(super) fn safe_to_insert_tatweel(self, glyphs: &mut [Glyph], start: usize, end: usize) {
        if self.tatweel {
            set_flags(
                glyphs,
                start,
                end,
                GlyphFlags::SAFE_TO_INSERT_TATWEEL,
                true,
                self.level,
            );
        } else {
            unsafe_to_break(glyphs, start, end, self.level);
        }
    }

    /// [`unsafe_to_break`] at this level.
    pub(super) fn unsafe_to_break(self, glyphs: &mut [Glyph], start: usize, end: usize) {
        unsafe_to_break(glyphs, start, end, self.level);
    }

    /// [`unsafe_to_concat`], when the buffer asked for it.
    pub(super) fn unsafe_to_concat(self, glyphs: &mut [Glyph], start: usize, end: usize) {
        if self.concat {
            unsafe_to_concat(glyphs, start, end);
        }
    }

    /// [`unsafe_to_concat_all`], when the buffer asked for it.
    pub(super) fn unsafe_to_concat_all(self, glyphs: &mut [Glyph]) {
        if self.concat {
            unsafe_to_concat_all(glyphs);
        }
    }

    /// The ranges a match over `glyphs` reports, applied as they come.
    pub(super) fn sink(self, glyphs: &mut [Glyph]) -> SliceFlags<'_> {
        SliceFlags { glyphs, cx: self }
    }
}

/// Applies the unsafe ranges a match reports to a run without an
/// output buffer (GPOS), where `_from_outbuffer` ranges are plain ones.
pub(super) struct SliceFlags<'g> {
    glyphs: &'g mut [Glyph],
    cx: FlagCx,
}

impl UnsafeRanges for SliceFlags<'_> {
    fn unsafe_to_break(&mut self, start: usize, end: usize, _from_out: bool) {
        self.cx.unsafe_to_break(self.glyphs, start, end);
    }

    fn unsafe_to_concat(&mut self, start: usize, end: usize, _from_out: bool) {
        self.cx.unsafe_to_concat(self.glyphs, start, end);
    }
}

/// `hb_propagate_flags`: every glyph of a cluster (a run of glyphs
/// sharing one, in output order) gets the union of their flags, less
/// the kinds the buffer did not ask for. Without
/// [`BufferFlags::PRODUCE_UNSAFE_TO_CONCAT`] there is no
/// unsafe-to-concatenate flag. With
/// [`BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL`], a cluster unsafe to
/// break cannot take a tatweel, and one that can is unsafe to break.
pub(super) fn propagate(glyphs: &mut [Glyph], buffer_flags: BufferFlags) {
    let mut keep = GlyphFlags::all();
    if !buffer_flags.contains(BufferFlags::PRODUCE_UNSAFE_TO_CONCAT) {
        keep.remove(GlyphFlags::UNSAFE_TO_CONCAT);
    }
    let tatweel = buffer_flags.contains(BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL);
    let mut start = 0;
    while start < glyphs.len() {
        let cluster = glyphs[start].cluster;
        let end = glyphs[start..]
            .iter()
            .position(|g| g.cluster != cluster)
            .map_or(glyphs.len(), |n| start + n);
        let mut flags = glyphs[start..end]
            .iter()
            .fold(GlyphFlags::empty(), |f, g| f | g.flags);
        if tatweel {
            if flags.contains(GlyphFlags::UNSAFE_TO_BREAK) {
                flags.remove(GlyphFlags::SAFE_TO_INSERT_TATWEEL);
            }
            if flags.contains(GlyphFlags::SAFE_TO_INSERT_TATWEEL) {
                flags |= BREAK;
            }
        }
        flags &= keep;
        for g in &mut glyphs[start..end] {
            g.flags = flags;
        }
        start = end;
    }
}

#[cfg(test)]
mod tests;
