//! GPOS iteration helpers.
//!
//! [`Skipper`] is HarfBuzz's skipping iterator for GPOS: glyphs the
//! lookup flags ignore are passed over, and so are default-ignorable
//! characters (ZWNJ always, ZWJ unless the lookup must see joiners).

use crate::buffer::{unicode_prop, Glyph};
use crate::tables::layout::MatchFilter;

/// HarfBuzz's skipping iterator, for iteration that accepts any glyph
/// (no match function): a glyph is passed over when the lookup flags
/// ignore it, or when it is an unsubstituted default-ignorable
/// character other than a ZWJ the lookup must see.
pub(super) struct Skipper<'f> {
    filter: &'f MatchFilter<'f>,
    ignore_zwj: bool,
}

impl<'f> Skipper<'f> {
    pub(super) const fn new(filter: &'f MatchFilter<'f>, ignore_zwj: bool) -> Self {
        Self { filter, ignore_zwj }
    }

    /// True when iteration passes over `g`.
    pub(super) fn skips(&self, g: &Glyph) -> bool {
        if self.filter.is_skipped(g.glyph_id as u16) {
            return true;
        }
        let props = g.unicode_props;
        props & unicode_prop::DEFAULT_IGNORABLE != 0
            && (self.ignore_zwj || props & unicode_prop::JOINER == 0)
    }

    /// First glyph at or after `from` that iteration stops at.
    pub(super) fn next(&self, glyphs: &[Glyph], from: usize) -> Option<usize> {
        (from..glyphs.len()).find(|&k| !self.skips(&glyphs[k]))
    }

    /// Nearest glyph before `before` that iteration stops at.
    pub(super) fn prev(&self, glyphs: &[Glyph], before: usize) -> Option<usize> {
        (0..before.min(glyphs.len()))
            .rev()
            .find(|&k| !self.skips(&glyphs[k]))
    }
}
