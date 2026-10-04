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

/// The slot of a subtable not parsed yet.
const UNPARSED: u32 = 0;
/// The slot of a subtable that does not parse (the shaper drops those).
const FAILED: u32 = u32::MAX;

/// A kind of lookup subtable that [`LazySubtables`] parses on demand.
pub(super) trait Subtable<'a>: Sized {
    /// Subtable `index` of `lookup`, `None` when it does not parse.
    fn parse(lookup: &Lookup<'a>, index: u16) -> Option<Self>;

    /// True for a class-based context subtable, the one kind whose
    /// matching reads
    /// [`crate::tables::layout::MatchContext::rule_set_digests`].
    fn reads_rule_set_digests(&self) -> bool;
}

/// The subtables of one lookup, each parsed the first time it is asked
/// for.
pub(super) struct LazySubtables<'a, T> {
    lookup: Lookup<'a>,
    /// For each of the first [`INLINE_SLOTS`] subtable indices,
    /// [`UNPARSED`], [`FAILED`], or one more than the subtable's
    /// position in `parsed`.
    first: [u32; INLINE_SLOTS],
    /// The same for the other subtables.
    rest: Vec<u32>,
    /// The subtables that parsed so far.
    parsed: Vec<T>,
    /// [`Self::has_cache`] has counted the subtables before this index,
    /// stopping one past the [`SUBTABLE_CACHES`]th that parses.
    counted: u16,
    /// How many of the subtables before `counted` parse: at most
    /// [`SUBTABLE_CACHES`].
    counted_parsing: usize,
    /// Subtable reads, for the test that bounds the work per glyph.
    #[cfg(test)]
    reads: usize,
}

impl<'a, T: Subtable<'a>> LazySubtables<'a, T> {
    /// The subtables of `lookup`, none parsed yet.
    pub(super) fn new(lookup: Lookup<'a>) -> Self {
        let count = usize::from(lookup.subtable_count());
        Self {
            first: [UNPARSED; INLINE_SLOTS],
            rest: alloc::vec![UNPARSED; count.saturating_sub(INLINE_SLOTS)],
            lookup,
            parsed: Vec::new(),
            counted: 0,
            counted_parsing: 0,
            #[cfg(test)]
            reads: 0,
        }
    }

    /// The slot of subtable `index`, `None` past the last subtable.
    #[inline]
    fn slot(&self, index: u16) -> Option<u32> {
        if index >= self.len() {
            return None;
        }
        let i = usize::from(index);
        match i.checked_sub(INLINE_SLOTS) {
            None => self.first.get(i).copied(),
            Some(i) => self.rest.get(i).copied(),
        }
    }

    fn set_slot(&mut self, index: u16, slot: u32) {
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
        self.slot(index).is_some_and(|s| s != UNPARSED)
    }

    /// Subtable `index`, parsed now if it was not yet; `None` when it
    /// does not parse.
    #[inline]
    pub(super) fn get(&mut self, index: u16) -> Option<&T> {
        #[cfg(test)]
        {
            self.reads += 1;
        }
        let slot = self.slot(index)?;
        let at = self.position(index, slot)?;
        self.parsed.get(at)
    }

    /// Where subtable `index`, whose slot is `slot`, is in `parsed`,
    /// parsing it now if it was not yet; `None` when it does not parse.
    #[inline]
    fn position(&mut self, index: u16, slot: u32) -> Option<usize> {
        match slot {
            UNPARSED => self.parse(index),
            FAILED => None,
            slot => Some(slot as usize - 1),
        }
    }

    /// Parses subtable `index`, which is not parsed yet, and returns
    /// where it is in `parsed`, `None` when it does not parse. Only the
    /// subtables that parse take room in `parsed`.
    fn parse(&mut self, index: u16) -> Option<usize> {
        let Some(parsed) = T::parse(&self.lookup, index) else {
            self.set_slot(index, FAILED);
            return None;
        };
        if self.parsed.len() == INLINE_SLOTS {
            // A walk that has parsed this many subtables tends to parse
            // most of them: room for all of them at once, as parsing
            // every subtable up front took, costs less than copying the
            // parsed ones at each doubling.
            self.parsed
                .reserve_exact(usize::from(self.len()) - INLINE_SLOTS);
        }
        self.parsed.push(parsed);
        // At most one entry per subtable, and there are at most
        // u16::MAX subtables, so the slot is neither UNPARSED nor FAILED.
        let at = self.parsed.len() - 1;
        self.set_slot(index, at as u32 + 1);
        Some(at)
    }

    /// Subtable `index` if `admits` lets it through, told whether the
    /// subtable is parsed already, parsed now if it was not yet, and
    /// whether HarfBuzz gives it a cache, as [`Self::has_cache`] tells
    /// for a subtable that reads one. `None` when `admits` turns it
    /// away, which parses nothing, or when it does not parse. The walk
    /// over a lookup's subtables asks this of each at every glyph, with
    /// one look at the subtable's slot.
    #[inline]
    pub(super) fn get_admitted(
        &mut self,
        index: u16,
        admits: impl FnOnce(bool) -> bool,
    ) -> Option<(&T, bool)> {
        #[cfg(test)]
        {
            self.reads += 1;
        }
        let slot = self.slot(index)?;
        if !admits(slot != UNPARSED) {
            return None;
        }
        let at = self.position(index, slot)?;
        let digests = usize::from(index) < SUBTABLE_CACHES
            || !self.parsed.get(at)?.reads_rule_set_digests()
            || self.has_cache(index);
        self.parsed.get(at).map(|t| (t, digests))
    }

    /// Whether subtable `index`, one that parses, is among the first
    /// [`SUBTABLE_CACHES`] subtables of the lookup that parse, which
    /// HarfBuzz gives a cache (see
    /// [`crate::tables::layout::MatchContext::with_rule_set_digests`]).
    /// Parses the subtables before it when that takes counting.
    ///
    /// The count is kept across calls, so over one application of the
    /// lookup each subtable is counted at most once, however many
    /// glyphs reach however many subtables: once the
    /// [`SUBTABLE_CACHES`]th subtable that parses is found, every later
    /// one is known to have no cache.
    pub(super) fn has_cache(&mut self, index: u16) -> bool {
        if usize::from(index) < SUBTABLE_CACHES {
            return true;
        }
        while self.counted < index && self.counted_parsing < SUBTABLE_CACHES {
            if self.get(self.counted).is_some() {
                self.counted_parsing += 1;
            }
            self.counted += 1;
        }
        // Either fewer than SUBTABLE_CACHES of the subtables before
        // `counted`, which has reached `index`, parse, or `counted` is
        // one past the SUBTABLE_CACHES-th that parses and only the
        // subtables before it have a cache.
        self.counted_parsing < SUBTABLE_CACHES || index < self.counted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::layout::LookupList;
    use alloc::vec;

    /// A LookupList of one lookup of `count` one-byte subtables, of
    /// which every third fails to parse.
    fn lookup_list(count: u16) -> Vec<u8> {
        let mut out = vec![];
        for v in [1, 4, 1, 0, count] {
            out.extend_from_slice(&u16::to_be_bytes(v));
        }
        let body = 6 + 2 * usize::from(count);
        for i in 0..usize::from(count) {
            out.extend_from_slice(&u16::try_from(body + i).unwrap().to_be_bytes());
        }
        out.extend((0..count).map(|i| u8::from(i % 3 != 0)));
        out
    }

    /// A one-byte subtable that parses when its byte is 1, and reads
    /// rule set digests.
    struct Byte;

    impl<'a> Subtable<'a> for Byte {
        fn parse(lookup: &Lookup<'a>, index: u16) -> Option<Self> {
            let byte = lookup.subtable_bytes(index)?.first().copied();
            (byte == Some(1)).then_some(Byte)
        }

        fn reads_rule_set_digests(&self) -> bool {
            true
        }
    }

    #[test]
    fn the_cache_rank_counts_the_subtables_that_parse_in_any_order() {
        let count = 40;
        let data = lookup_list(count);
        let list = LookupList::parse(&data).unwrap();
        // Whether fewer than SUBTABLE_CACHES subtables before `index`
        // parse, counted afresh.
        let scan = |index: u16| (0..index).filter(|i| i % 3 != 0).count() < SUBTABLE_CACHES;
        let orders: [Vec<u16>; 3] = [
            (0..count).collect(),
            (0..count).rev().collect(),
            (0..count).map(|i| (i * 17) % count).collect(),
        ];
        for order in orders {
            let mut subtables = LazySubtables::<Byte>::new(list.get(0).unwrap());
            for &i in order.iter().chain(&order) {
                let got = subtables
                    .get_admitted(i, |_| true)
                    .map(|(_, digests)| digests);
                assert_eq!(got, (i % 3 != 0).then(|| scan(i)), "subtable {i}");
            }
        }
    }

    #[test]
    fn the_cache_rank_costs_constant_work_per_subtable_visit() {
        // Every glyph reaches every subtable of a lookup of many
        // class-based context subtables, and each visit past the first
        // SUBTABLE_CACHES asks for the cache rank. Counting the
        // subtables before each one made a glyph's walk quadratic in
        // their number.
        let count = 6000;
        let data = lookup_list(count);
        let list = LookupList::parse(&data).unwrap();
        let mut subtables = LazySubtables::<Byte>::new(list.get(0).unwrap());
        let glyphs = 4;
        for _ in 0..glyphs {
            for i in 0..count {
                let _ = subtables.get_admitted(i, |_| true);
            }
        }
        let visits = glyphs * usize::from(count);
        // One read a visit, plus counting each subtable at most once.
        let bound = visits + usize::from(count);
        assert!(
            subtables.reads <= bound,
            "{} subtable reads for {visits} visits, at most {bound}",
            subtables.reads
        );
    }
}
