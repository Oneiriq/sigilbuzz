//! A lookup's subtables, each parsed the first time a glyph reaches
//! it.
//!
//! Parsing a contextual subtable allocates its rule arrays, and a
//! lookup can have hundreds of them (Noto Sans KR's `ccmp` has 268),
//! of which a short run reaches one or two. The lookup accelerators
//! (see [`crate::tables::layout::accel`]) tell which subtables a glyph
//! can reach; this parses only those, once per lookup application.

use alloc::vec::Vec;

use crate::tables::layout::skip_iter::SUBTABLE_CACHES;
use crate::tables::layout::Lookup;

/// Subtables whose slots a [`LazySubtables`] keeps inline, so that a
/// small lookup is applied without allocating them.
const INLINE_SLOTS: usize = 8;

/// The subtables of one lookup, parsed on demand with `parse`.
pub(super) struct LazySubtables<'a, T> {
    lookup: Lookup<'a>,
    /// For each of the first [`INLINE_SLOTS`] subtable indices, 0
    /// before it is parsed, else one more than its position in
    /// `parsed`.
    first: [u16; INLINE_SLOTS],
    /// The same for the other subtables.
    rest: Vec<u16>,
    /// The subtables parsed so far, `None` for one that failed to
    /// parse (the shaper drops those).
    parsed: Vec<Option<T>>,
    parse: fn(&Lookup<'a>, u16) -> Option<T>,
}

impl<'a, T> LazySubtables<'a, T> {
    /// The subtables of `lookup`, none parsed yet.
    pub(super) fn new(lookup: Lookup<'a>, parse: fn(&Lookup<'a>, u16) -> Option<T>) -> Self {
        let count = usize::from(lookup.subtable_count());
        Self {
            first: [0; INLINE_SLOTS],
            rest: alloc::vec![0; count.saturating_sub(INLINE_SLOTS)],
            lookup,
            parsed: Vec::new(),
            parse,
        }
    }

    /// The slot of subtable `index`, `None` past the last subtable.
    #[inline]
    fn slot(&self, index: u16) -> Option<u16> {
        if index >= self.len() {
            return None;
        }
        let i = usize::from(index);
        match i.checked_sub(INLINE_SLOTS) {
            None => self.first.get(i).copied(),
            Some(i) => self.rest.get(i).copied(),
        }
    }

    fn set_slot(&mut self, index: u16, slot: u16) {
        let i = usize::from(index);
        let cell = match i.checked_sub(INLINE_SLOTS) {
            None => self.first.get_mut(i),
            Some(i) => self.rest.get_mut(i),
        };
        if let Some(cell) = cell {
            *cell = slot;
        }
    }

    /// Number of subtables, parsed or not.
    pub(super) fn len(&self) -> u16 {
        self.lookup.subtable_count()
    }

    /// True once subtable `index` has been parsed (whether or not it
    /// parsed).
    #[inline]
    pub(super) fn is_parsed(&self, index: u16) -> bool {
        self.slot(index).is_some_and(|s| s != 0)
    }

    /// Subtable `index`, parsed now if it was not yet; `None` when it
    /// does not parse.
    #[inline]
    pub(super) fn get(&mut self, index: u16) -> Option<&T> {
        let slot = self.slot(index)?;
        let at = if slot == 0 {
            let parsed = (self.parse)(&self.lookup, index);
            self.parsed.push(parsed);
            // At most one entry per subtable, so the position fits.
            let at = self.parsed.len() - 1;
            self.set_slot(index, (at + 1) as u16);
            at
        } else {
            usize::from(slot - 1)
        };
        self.parsed.get(at)?.as_ref()
    }

    /// Subtable `index`, parsed now if it was not yet, and whether
    /// HarfBuzz gives it a cache, as [`Self::has_cache`] tells for a
    /// subtable `reads_digests` says reads one; `None` when it does not
    /// parse.
    #[inline]
    pub(super) fn get_with_digests(
        &mut self,
        index: u16,
        reads_digests: fn(&T) -> bool,
    ) -> Option<(&T, bool)> {
        if usize::from(index) >= SUBTABLE_CACHES
            && reads_digests(self.get(index)?)
            && !self.has_cache(index)
        {
            return self.get(index).map(|t| (t, false));
        }
        self.get(index).map(|t| (t, true))
    }

    /// Whether subtable `index`, one that parses, is among the first
    /// [`SUBTABLE_CACHES`] subtables of the lookup that parse, which
    /// HarfBuzz gives a cache (see
    /// [`crate::tables::layout::MatchContext::with_rule_set_digests`]).
    /// Parses the subtables before it when that takes counting.
    pub(super) fn has_cache(&mut self, index: u16) -> bool {
        if usize::from(index) < SUBTABLE_CACHES {
            return true;
        }
        let failed = (0..index).filter(|&i| self.get(i).is_none()).count();
        usize::from(index) - failed < SUBTABLE_CACHES
    }
}
