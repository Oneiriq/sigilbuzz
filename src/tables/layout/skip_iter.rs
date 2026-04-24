//! `LookupFlag` skip-iterator.
//!
//! OpenType lookup matching runs over the *unfiltered* glyph stream
//! except that each lookup's `LookupFlag` can ask the matcher to
//! pretend certain glyphs are not there. Concretely, a Coverage /
//! ClassDef / glyph-id check during a context or chained-context
//! match walks the stream via a [`SkipIter`] that hops over glyphs
//! whose GDEF properties match the active filter bits:
//!
//! ```text
//!   0x0002  RightToLeft             (cursive direction; not used by this iterator)
//!   0x0002  IgnoreBaseGlyphs        skip GlyphClass::Base
//!   0x0004  IgnoreLigatures         skip GlyphClass::Ligature
//!   0x0008  IgnoreMarks             skip GlyphClass::Mark
//!   0x0010  UseMarkFilteringSet     skip marks not in the named coverage
//!   0xFF00  MarkAttachmentType      (high byte) skip marks whose attach
//!                                   class differs from the specified non-zero value
//! ```
//!
//! sigilbuzz shares one [`MatchFilter`] struct across GSUB and GPOS;
//! the `SkipIter` is just a `(position, direction)` cursor over a
//! `&[u16]` glyph-id slice that hands back only the non-skipped
//! positions. Callers convert back to the absolute glyph position
//! via the iterator's own index — so the `input_len` they report up
//! to the dispatcher covers the full *raw* span of the match,
//! including the skipped glyphs in between.
//!
//! The iterator never allocates and never clones; it stores a
//! borrowed slice plus a borrowed filter.

use crate::tables::gdef::{Gdef, GlyphClass};
use crate::tables::layout::Coverage;

// Public re-export so call sites can write `layout::LOOKUP_FLAG_*`
// without digging into this module.
/// `LookupFlag` — `RightToLeft` bit. Indicates the lookup runs in
/// RTL direction. Only GPOS type 3 (cursive) currently uses it; this
/// module parses but does not act on it.
pub const LOOKUP_FLAG_RIGHT_TO_LEFT: u16 = 0x0001;
/// `LookupFlag` — `IgnoreBaseGlyphs`. Skip glyphs classed
/// [`GlyphClass::Base`] during matching.
pub const LOOKUP_FLAG_IGNORE_BASE_GLYPHS: u16 = 0x0002;
/// `LookupFlag` — `IgnoreLigatures`. Skip glyphs classed
/// [`GlyphClass::Ligature`] during matching.
pub const LOOKUP_FLAG_IGNORE_LIGATURES: u16 = 0x0004;
/// `LookupFlag` — `IgnoreMarks`. Skip glyphs classed
/// [`GlyphClass::Mark`] during matching.
pub const LOOKUP_FLAG_IGNORE_MARKS: u16 = 0x0008;
/// `LookupFlag` — `UseMarkFilteringSet`. When set, the lookup's
/// trailing `markFilteringSet` u16 indexes a `MarkGlyphSetsDef`
/// coverage in GDEF; marks *not* in that coverage are skipped.
pub const LOOKUP_FLAG_USE_MARK_FILTERING_SET: u16 = 0x0010;

/// Mask isolating the `MarkAttachmentType` byte (high byte of the
/// 16-bit flag). Non-zero values restrict marks to those with the
/// same GDEF mark-attachment class.
pub const LOOKUP_FLAG_MARK_ATTACHMENT_TYPE_MASK: u16 = 0xFF00;

/// A decoded `LookupFlag` + the GDEF / mark-filtering-set coverage
/// needed to answer "should the skip-iterator pretend this glyph is
/// not here?" on every step.
///
/// Holds only borrowed data and never allocates. Construct once per
/// lookup via [`MatchFilter::for_lookup`] and hand it to every
/// context/chained-context matcher the lookup triggers — plus every
/// GPOS pair-adjustment walk for the same lookup.
#[derive(Debug, Clone, Copy)]
pub struct MatchFilter<'a> {
    flag: u16,
    mark_attach_type: u8,
    gdef: Option<&'a Gdef<'a>>,
    mark_set: Option<&'a Coverage<'a>>,
}

impl<'a> MatchFilter<'a> {
    /// An "accept every glyph" filter. Useful for call sites that do
    /// not yet have access to the lookup's flag, and for fallbacks
    /// when GDEF is missing from the font.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            flag: 0,
            mark_attach_type: 0,
            gdef: None,
            mark_set: None,
        }
    }

    /// Builds a filter from the `LookupFlag` bits, the font's GDEF,
    /// and the optional `markFilteringSet` trailer on the lookup
    /// header. Passing `gdef = None` collapses every class-based
    /// skip into a no-op (which matches the spec: GDEF is optional).
    ///
    /// `mark_filtering_set_index` is the u16 that sits after the
    /// subtable offsets when `LOOKUP_FLAG_USE_MARK_FILTERING_SET` is
    /// on — read it once from the `Lookup` header and pass it here.
    #[must_use]
    pub fn for_lookup(
        flag: u16,
        gdef: Option<&'a Gdef<'a>>,
        mark_filtering_set_index: Option<u16>,
    ) -> Self {
        let mark_attach_type = ((flag & LOOKUP_FLAG_MARK_ATTACHMENT_TYPE_MASK) >> 8) as u8;
        let mark_set = if flag & LOOKUP_FLAG_USE_MARK_FILTERING_SET != 0 {
            match (gdef, mark_filtering_set_index) {
                (Some(g), Some(idx)) => g.mark_filtering_set(idx),
                _ => None,
            }
        } else {
            None
        };
        Self {
            flag,
            mark_attach_type,
            gdef,
            mark_set,
        }
    }

    /// `true` when the filter is a pass-through — every glyph is
    /// accepted. Hot-path dispatchers take a short-circuit to avoid
    /// the per-step class query when the flag is zero.
    #[must_use]
    pub const fn is_pass_through(&self) -> bool {
        self.flag == 0 && self.mark_attach_type == 0
    }

    /// `true` when the matcher should pretend `glyph_id` is not
    /// present at its position. Every bit of the flag is checked in
    /// spec order; encountering any skip reason short-circuits.
    #[must_use]
    pub fn is_skipped(&self, glyph_id: u16) -> bool {
        if self.is_pass_through() {
            return false;
        }
        let Some(gdef) = self.gdef else {
            // Without GDEF we cannot classify glyphs; treat every
            // glyph as a base (matching the spec default). That
            // means IgnoreBaseGlyphs/Ligatures/Marks all degrade to
            // "skip nothing" — identical to the pass-through branch.
            return false;
        };
        let class = gdef.glyph_class(glyph_id);
        if class == GlyphClass::Base && (self.flag & LOOKUP_FLAG_IGNORE_BASE_GLYPHS) != 0 {
            return true;
        }
        if class == GlyphClass::Ligature && (self.flag & LOOKUP_FLAG_IGNORE_LIGATURES) != 0 {
            return true;
        }
        if class == GlyphClass::Mark {
            if (self.flag & LOOKUP_FLAG_IGNORE_MARKS) != 0 {
                return true;
            }
            // MarkAttachmentType: non-zero means "restrict to marks
            // of this attachment class" — other marks are skipped.
            if self.mark_attach_type != 0 {
                let attach = gdef.mark_attach_class(glyph_id) as u8;
                if attach != self.mark_attach_type {
                    return true;
                }
            }
            // UseMarkFilteringSet: a mark not listed in the
            // coverage is skipped. When the flag is set but the
            // coverage is missing (e.g. malformed font) every mark
            // is skipped — the conservative choice HarfBuzz makes.
            if (self.flag & LOOKUP_FLAG_USE_MARK_FILTERING_SET) != 0 {
                match self.mark_set {
                    Some(cov) => {
                        if !cov.contains(glyph_id) {
                            return true;
                        }
                    }
                    None => return true,
                }
            }
        }
        false
    }

    /// Convenience wrapper: walks `glyphs[start..]` forward and
    /// returns the first index that is *not* skipped. `None` if the
    /// tail is empty or every remaining glyph is filtered out.
    #[must_use]
    pub fn next_unskipped(&self, glyphs: &[u16], start: usize) -> Option<usize> {
        for (rel, &g) in glyphs.iter().enumerate().skip(start) {
            if !self.is_skipped(g) {
                return Some(rel);
            }
        }
        None
    }

    /// Convenience wrapper: walks `glyphs[..end]` backward and
    /// returns the first index (closer to `end-1`) that is not
    /// skipped. `None` if the prefix is empty or every preceding
    /// glyph is filtered out.
    #[must_use]
    pub fn prev_unskipped(&self, glyphs: &[u16], end: usize) -> Option<usize> {
        (0..end).rev().find(|&i| !self.is_skipped(glyphs[i]))
    }
}

/// Forward/backward skip-iterator over a glyph-id slice. Each call
/// to [`SkipIter::next`] advances past zero or more filtered glyphs
/// and returns the next unfiltered `(index, glyph_id)` pair.
/// [`SkipIter::prev`] walks the other direction. Both methods leave
/// the cursor positioned *after* the returned index on the direction
/// of travel, so chaining works.
///
/// The iterator never allocates and holds only three borrowed
/// references: the glyph slice, the filter, and the current cursor
/// as a `usize`.
#[derive(Debug)]
pub struct SkipIter<'g, 'f> {
    glyphs: &'g [u16],
    filter: &'f MatchFilter<'f>,
    /// Cursor in the forward direction: the next position `next()`
    /// will inspect. `prev()` looks at `cursor - 1` and decrements.
    cursor: usize,
}

impl<'g, 'f> SkipIter<'g, 'f> {
    /// Builds a new iterator starting at `start` on `glyphs` with
    /// `filter` controlling which glyphs are skipped.
    #[must_use]
    pub const fn new(glyphs: &'g [u16], filter: &'f MatchFilter<'f>, start: usize) -> Self {
        Self {
            glyphs,
            filter,
            cursor: start,
        }
    }

    /// Resets the cursor to `pos`. Cheap — the iterator caches
    /// nothing beyond the integer.
    pub fn reset(&mut self, pos: usize) {
        self.cursor = pos;
    }

    /// Current cursor position (the next forward step inspects here).
    #[must_use]
    pub const fn position(&self) -> usize {
        self.cursor
    }

    /// Returns the next unfiltered glyph at or after the current
    /// cursor, advancing the cursor past it. `None` when the tail
    /// contains only filtered glyphs.
    ///
    /// Named `next_glyph` (rather than `next`) so it does not shadow
    /// the [`Iterator::next`] trait method; sigilbuzz never wraps the
    /// iterator in a trait object so a free method is easier to
    /// reason about than an iterator impl.
    pub fn next_glyph(&mut self) -> Option<(usize, u16)> {
        while self.cursor < self.glyphs.len() {
            let i = self.cursor;
            let g = self.glyphs[i];
            self.cursor += 1;
            if !self.filter.is_skipped(g) {
                return Some((i, g));
            }
        }
        None
    }

    /// Returns the next unfiltered glyph strictly before the current
    /// cursor, decrementing the cursor past it. `None` when the
    /// prefix contains only filtered glyphs.
    pub fn prev_glyph(&mut self) -> Option<(usize, u16)> {
        while self.cursor > 0 {
            self.cursor -= 1;
            let i = self.cursor;
            let g = self.glyphs[i];
            if !self.filter.is_skipped(g) {
                return Some((i, g));
            }
        }
        None
    }

    /// Returns the next unfiltered glyph at or after the current
    /// cursor *without* advancing. Useful for matchers that want to
    /// test whether the stream still has content before committing
    /// to a step.
    #[must_use]
    pub fn peek(&self) -> Option<(usize, u16)> {
        for i in self.cursor..self.glyphs.len() {
            let g = self.glyphs[i];
            if !self.filter.is_skipped(g) {
                return Some((i, g));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    // Builds a GDEF-like structure by constructing a full v1.0 header
    // pointing at a synthetic glyph class def. Glyphs are tagged per
    // `classes` (1 = base, 2 = ligature, 3 = mark).
    fn make_gdef(classes: &[(u16, u16)]) -> Vec<u8> {
        // Class def format 1: start glyph + count + classes[].
        // Callers here pass explicit (gid, class) pairs for clarity,
        // so translate into the dense format the ClassDef parser
        // expects.
        //
        // Simpler path: use format 2 (range records) with one record
        // per pair.
        let mut cd = Vec::new();
        cd.extend_from_slice(&2u16.to_be_bytes());
        cd.extend_from_slice(&(classes.len() as u16).to_be_bytes());
        for (gid, cls) in classes {
            cd.extend_from_slice(&gid.to_be_bytes());
            cd.extend_from_slice(&gid.to_be_bytes());
            cd.extend_from_slice(&cls.to_be_bytes());
        }

        let mut gdef = Vec::new();
        gdef.extend_from_slice(&1u16.to_be_bytes()); // major
        gdef.extend_from_slice(&0u16.to_be_bytes()); // minor
        gdef.extend_from_slice(&12u16.to_be_bytes()); // glyphClassDefOff
        gdef.extend_from_slice(&0u16.to_be_bytes()); // attachListOff
        gdef.extend_from_slice(&0u16.to_be_bytes()); // ligCaretListOff
        gdef.extend_from_slice(&0u16.to_be_bytes()); // markAttachClassDefOff
        gdef.extend_from_slice(&cd);
        gdef
    }

    #[test]
    fn pass_through_filter_accepts_every_glyph() {
        let f = MatchFilter::none();
        let glyphs = [10u16, 11, 12, 13];
        let mut it = SkipIter::new(&glyphs, &f, 0);
        let seen: Vec<_> = core::iter::from_fn(|| it.next_glyph()).collect();
        assert_eq!(seen, [(0, 10), (1, 11), (2, 12), (3, 13)]);
    }

    #[test]
    fn ignore_marks_hops_past_mark_glyphs() {
        // 10=base, 11=mark, 12=base, 13=mark, 14=base.
        let bytes = make_gdef(&[(10, 1), (11, 3), (12, 1), (13, 3), (14, 1)]);
        let gdef = Gdef::parse(&bytes).unwrap();
        let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);
        assert!(!f.is_pass_through());
        let glyphs = [10u16, 11, 12, 13, 14];
        let mut it = SkipIter::new(&glyphs, &f, 0);
        let seen: Vec<_> = core::iter::from_fn(|| it.next_glyph()).collect();
        assert_eq!(seen, [(0, 10), (2, 12), (4, 14)]);
    }

    #[test]
    fn ignore_bases_skips_base_glyphs_only() {
        let bytes = make_gdef(&[(10, 1), (11, 3), (12, 1), (13, 2)]);
        let gdef = Gdef::parse(&bytes).unwrap();
        let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_BASE_GLYPHS, Some(&gdef), None);
        let glyphs = [10u16, 11, 12, 13];
        let mut it = SkipIter::new(&glyphs, &f, 0);
        let seen: Vec<_> = core::iter::from_fn(|| it.next_glyph()).collect();
        // 10 base skipped; 11 mark kept; 12 base skipped; 13 liga kept.
        assert_eq!(seen, [(1, 11), (3, 13)]);
    }

    #[test]
    fn ignore_ligatures_skips_only_liga_class() {
        let bytes = make_gdef(&[(10, 1), (11, 2), (12, 3), (13, 2)]);
        let gdef = Gdef::parse(&bytes).unwrap();
        let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_LIGATURES, Some(&gdef), None);
        let glyphs = [10u16, 11, 12, 13];
        let mut it = SkipIter::new(&glyphs, &f, 0);
        let seen: Vec<_> = core::iter::from_fn(|| it.next_glyph()).collect();
        assert_eq!(seen, [(0, 10), (2, 12)]);
    }

    #[test]
    fn prev_walks_backwards_and_skips() {
        let bytes = make_gdef(&[(10, 1), (11, 3), (12, 1), (13, 3), (14, 1)]);
        let gdef = Gdef::parse(&bytes).unwrap();
        let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);
        let glyphs = [10u16, 11, 12, 13, 14];
        // Start "after" the last glyph so prev() returns the last
        // unfiltered position first.
        let mut it = SkipIter::new(&glyphs, &f, glyphs.len());
        assert_eq!(it.prev_glyph(), Some((4, 14)));
        assert_eq!(it.prev_glyph(), Some((2, 12)));
        assert_eq!(it.prev_glyph(), Some((0, 10)));
        assert_eq!(it.prev_glyph(), None);
    }

    #[test]
    fn peek_does_not_advance_cursor() {
        let bytes = make_gdef(&[(10, 3), (11, 1)]);
        let gdef = Gdef::parse(&bytes).unwrap();
        let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);
        let glyphs = [10u16, 11];
        let mut it = SkipIter::new(&glyphs, &f, 0);
        assert_eq!(it.peek(), Some((1, 11)));
        assert_eq!(it.peek(), Some((1, 11)));
        assert_eq!(it.next_glyph(), Some((1, 11)));
        assert_eq!(it.next_glyph(), None);
    }

    #[test]
    fn reset_restores_iteration_from_new_position() {
        let glyphs = [5u16, 6, 7, 8];
        let f = MatchFilter::none();
        let mut it = SkipIter::new(&glyphs, &f, 0);
        assert_eq!(it.next_glyph(), Some((0, 5)));
        assert_eq!(it.next_glyph(), Some((1, 6)));
        it.reset(3);
        assert_eq!(it.next_glyph(), Some((3, 8)));
        assert_eq!(it.next_glyph(), None);
    }

    #[test]
    fn mark_attachment_type_restricts_marks_to_matching_class() {
        // Build a v1.2 GDEF with glyph class def + mark attach class
        // def. Marks 11 and 12 have attach classes 1 and 2; a flag
        // with MarkAttachmentType=1 skips mark 12 but keeps 11.
        let mut gdef_bytes = Vec::new();
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // major
        gdef_bytes.extend_from_slice(&2u16.to_be_bytes()); // minor
        let header_len = 14u16;
        // Placeholders for 5 offset16 fields.
        gdef_bytes.extend_from_slice(&[0u8; 10]);
        let gc_slot = 4usize;
        let mac_slot = 10usize;
        let mgs_slot = 12usize;
        debug_assert_eq!(gdef_bytes.len(), header_len as usize);
        let _ = mgs_slot;

        let gc_off = gdef_bytes.len() as u16;
        gdef_bytes[gc_slot..gc_slot + 2].copy_from_slice(&gc_off.to_be_bytes());
        // GlyphClassDef format 1 starting at gid 10: 10=base, 11=mark, 12=mark.
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // format
        gdef_bytes.extend_from_slice(&10u16.to_be_bytes()); // start
        gdef_bytes.extend_from_slice(&3u16.to_be_bytes()); // count
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // 10 → base
        gdef_bytes.extend_from_slice(&3u16.to_be_bytes()); // 11 → mark
        gdef_bytes.extend_from_slice(&3u16.to_be_bytes()); // 12 → mark

        let mac_off = gdef_bytes.len() as u16;
        gdef_bytes[mac_slot..mac_slot + 2].copy_from_slice(&mac_off.to_be_bytes());
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // format
        gdef_bytes.extend_from_slice(&11u16.to_be_bytes()); // start
        gdef_bytes.extend_from_slice(&2u16.to_be_bytes()); // count
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // 11 → attach 1
        gdef_bytes.extend_from_slice(&2u16.to_be_bytes()); // 12 → attach 2

        let gdef = Gdef::parse(&gdef_bytes).unwrap();
        // Flag with MarkAttachmentType=1 in the high byte. Low byte zero.
        let flag = 0x0100u16;
        let f = MatchFilter::for_lookup(flag, Some(&gdef), None);
        assert!(!f.is_skipped(10)); // base untouched by attach type
        assert!(!f.is_skipped(11)); // mark attach 1 matches, not skipped
        assert!(f.is_skipped(12)); // mark attach 2 mismatches, skipped
    }

    #[test]
    fn use_mark_filtering_set_skips_marks_outside_the_coverage() {
        // v1.2 GDEF with mark glyph sets = [cov{11}]. Flag 0x0010 with
        // index 0 → only mark 11 survives; mark 12 is skipped.
        let mut gdef_bytes = Vec::new();
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // major
        gdef_bytes.extend_from_slice(&2u16.to_be_bytes()); // minor
        gdef_bytes.extend_from_slice(&[0u8; 10]);
        let gc_slot = 4usize;
        let mgs_slot = 12usize;

        let gc_off = gdef_bytes.len() as u16;
        gdef_bytes[gc_slot..gc_slot + 2].copy_from_slice(&gc_off.to_be_bytes());
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // format 1
        gdef_bytes.extend_from_slice(&10u16.to_be_bytes()); // start
        gdef_bytes.extend_from_slice(&3u16.to_be_bytes()); // count
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // 10 → base
        gdef_bytes.extend_from_slice(&3u16.to_be_bytes()); // 11 → mark
        gdef_bytes.extend_from_slice(&3u16.to_be_bytes()); // 12 → mark

        let mgs_sub_off = gdef_bytes.len();
        gdef_bytes[mgs_slot..mgs_slot + 2].copy_from_slice(&(mgs_sub_off as u16).to_be_bytes());
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // format
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // count
        let off32_slot = gdef_bytes.len();
        gdef_bytes.extend_from_slice(&0u32.to_be_bytes()); // coverage[0] offset
        let cov_rel = (gdef_bytes.len() - mgs_sub_off) as u32;
        gdef_bytes[off32_slot..off32_slot + 4].copy_from_slice(&cov_rel.to_be_bytes());
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // coverage format 1
        gdef_bytes.extend_from_slice(&1u16.to_be_bytes()); // glyphCount
        gdef_bytes.extend_from_slice(&11u16.to_be_bytes()); // glyph 11 only

        let gdef = Gdef::parse(&gdef_bytes).unwrap();
        let f = MatchFilter::for_lookup(LOOKUP_FLAG_USE_MARK_FILTERING_SET, Some(&gdef), Some(0));
        assert!(!f.is_skipped(10)); // base
        assert!(!f.is_skipped(11)); // mark in set
        assert!(f.is_skipped(12)); // mark not in set
    }

    #[test]
    fn missing_gdef_neutralises_flag() {
        let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, None, None);
        // Without GDEF every glyph reads as base → nothing is skipped
        // even though the flag says "ignore marks".
        assert!(!f.is_skipped(99));
    }

    #[test]
    fn next_unskipped_and_prev_unskipped_helpers_agree_with_iterator() {
        let bytes = make_gdef(&[(10, 1), (11, 3), (12, 1)]);
        let gdef = Gdef::parse(&bytes).unwrap();
        let f = MatchFilter::for_lookup(LOOKUP_FLAG_IGNORE_MARKS, Some(&gdef), None);
        let glyphs = [10u16, 11, 12];
        assert_eq!(f.next_unskipped(&glyphs, 0), Some(0));
        assert_eq!(f.next_unskipped(&glyphs, 1), Some(2));
        assert_eq!(f.next_unskipped(&glyphs, 3), None);
        assert_eq!(f.prev_unskipped(&glyphs, 3), Some(2));
        assert_eq!(f.prev_unskipped(&glyphs, 2), Some(0));
        assert_eq!(f.prev_unskipped(&glyphs, 1), Some(0));
        assert_eq!(f.prev_unskipped(&glyphs, 0), None);
    }

}
