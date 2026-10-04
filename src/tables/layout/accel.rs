//! Lookup accelerators: for each lookup, a digest of the glyphs its
//! subtables can start at, so a lookup that cannot apply to a run is
//! passed over without parsing a subtable, as HarfBuzz's
//! `hb_ot_layout_lookup_accelerator_t` does.
//!
//! Every GSUB and GPOS subtable starts by looking the glyph at the
//! cursor up in one Coverage table, its *primary* coverage (the first
//! input coverage of a format 3 context), and does nothing at all when
//! the glyph is not there. A [`Digest`] of that coverage, a small
//! filter with no false negatives, lets the shaper skip the subtable
//! for a glyph it cannot cover, and the union of a lookup's subtable
//! digests lets it skip the whole lookup for a run none of whose glyphs
//! it can cover. Skipping only drops work that would have changed
//! nothing, so the shaped output is the same.
//!
//! A [`crate::Font`] keeps one [`LookupAccels`] per table from its
//! second shaping call on, and each lookup builds its digests the first
//! time it is applied then. In a font's first call, and for a table
//! view without the font's cache, a lookup reads its subtables' primary
//! coverages directly instead ([`DirectAccel`]): a font shaped once,
//! which many callers build per run, would spend more building digests
//! than they save. Building reads each subtable's primary coverage once,
//! under two work budgets: one per lookup, and one per table
//! proportional to the table's size. Past either, a subtable gets a
//! digest that admits every glyph, so a hostile font that points
//! thousands of subtables or lookups at one huge coverage costs no more
//! than reading its table a few times.
//!
//! What the accelerators keep is bounded by the table's size the same
//! way: keeping a subtable's digest costs one entry of the table's
//! budget, and a lookup the budget cannot pay for keeps no digest and
//! admits every glyph. Lookup indices that name one lookup (the same
//! offset in the LookupList) share its accelerator, so a table that
//! lists one lookup of many subtables under many indices keeps its
//! digests once.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::coverage::Coverage;
use super::lookup_list::{Lookup, LookupList};
use super::skip_iter::LayoutTable;
use crate::ot::layout_select::{FeatureMaps, StagePlans};
use crate::sync::OnceBox;

/// Glyph-id bits each of the digest's three masks hashes on.
const SHIFTS: [u32; 3] = [0, 4, 9];

/// A set of glyph ids as three 64-bit masks, HarfBuzz's
/// `hb_set_digest_t`: mask `k` has bit `(g >> SHIFTS[k]) & 63` for every
/// glyph `g` added. [`Digest::may_have`] has false positives but no
/// false negatives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Digest {
    masks: [u64; 3],
}

impl Digest {
    /// The digest of no glyph.
    pub(crate) const EMPTY: Self = Self { masks: [0; 3] };
    /// A digest that admits every glyph.
    pub(crate) const FULL: Self = Self {
        masks: [u64::MAX; 3],
    };

    fn bit(glyph: u16, shift: u32) -> u64 {
        1 << ((u32::from(glyph) >> shift) & 63)
    }

    /// Adds one glyph.
    #[cfg(test)]
    pub(crate) fn add(&mut self, glyph: u16) {
        for (mask, shift) in self.masks.iter_mut().zip(SHIFTS) {
            *mask |= Self::bit(glyph, shift);
        }
    }

    /// Adds every glyph from `first` to `last`, both included. A range
    /// with `first > last` adds nothing.
    pub(crate) fn add_range(&mut self, first: u16, last: u16) {
        if first > last {
            return;
        }
        for (mask, shift) in self.masks.iter_mut().zip(SHIFTS) {
            if (u32::from(last) >> shift) - (u32::from(first) >> shift) >= 63 {
                *mask = u64::MAX;
            } else {
                // The bits from `first`'s to `last`'s, wrapping past
                // bit 63 (HarfBuzz's `add_range`).
                let (a, b) = (Self::bit(first, shift), Self::bit(last, shift));
                *mask |= b
                    .wrapping_add(b.wrapping_sub(a))
                    .wrapping_sub(u64::from(b < a));
            }
        }
    }

    /// Adds every glyph of `other`.
    pub(crate) fn union(&mut self, other: &Self) {
        for (mask, o) in self.masks.iter_mut().zip(other.masks) {
            *mask |= o;
        }
    }

    /// False only when `glyph` was never added.
    #[inline]
    pub(crate) fn may_have(&self, glyph: u16) -> bool {
        self.masks
            .iter()
            .zip(SHIFTS)
            .all(|(mask, shift)| mask & Self::bit(glyph, shift) != 0)
    }

    /// False only when no glyph was added to both digests.
    #[cfg(test)]
    pub(crate) fn may_intersect(&self, other: &Self) -> bool {
        self.masks
            .iter()
            .zip(other.masks)
            .all(|(mask, o)| mask & o != 0)
    }

    /// The digest of `glyphs`.
    #[cfg(test)]
    pub(crate) fn of(glyphs: impl IntoIterator<Item = u16>) -> Self {
        let mut d = Self::EMPTY;
        for g in glyphs {
            d.add(g);
        }
        d
    }
}

/// Coverage entries one lookup's accelerator may read, at least.
/// Real fonts stay below it: a `vert` substitution of a CJK font covers
/// a few thousand glyphs.
const MIN_COVERAGE_BUDGET: usize = 1 << 14;
/// Coverage entries per subtable the budget grows by.
const COVERAGE_BUDGET_PER_SUBTABLE: usize = 64;

/// Coverage entries all of one table's accelerators may read together:
/// a few times what fits in the table (an entry takes at least two
/// bytes), and at least 64 Ki.
fn table_coverage_budget(table_len: usize) -> usize {
    table_len.saturating_mul(2).saturating_add(1 << 16)
}

/// One lookup's accelerator: the digest of every glyph a subtable of
/// it can start at, and each subtable's own digest, by subtable index.
#[derive(Debug)]
pub(crate) struct LookupAccel {
    digest: Digest,
    subtables: Box<[Digest]>,
}

impl LookupAccel {
    /// An accelerator that keeps no digest and admits every glyph at
    /// every subtable.
    fn admit_all() -> Self {
        Self {
            digest: Digest::FULL,
            subtables: Box::default(),
        }
    }

    /// Builds the accelerator of `lookup`, a lookup of `table`, reading
    /// at most what the lookup's budget and `table_budget`, which the
    /// table's other lookups share, leave.
    ///
    /// Each subtable digest the accelerator keeps costs one entry of
    /// `table_budget` too, taken before any coverage is read, so what a
    /// table's accelerators keep is bounded by the table's size however
    /// many lookups apply. A lookup the budget cannot pay for keeps no
    /// digest and admits every glyph.
    ///
    /// A lookup whose subtables all have the lookup's own digest (a
    /// single subtable, or many that share one coverage) keeps only
    /// that: every walk checks a glyph against the lookup's digest
    /// before it tries a subtable, so theirs would rule nothing more
    /// out.
    pub(crate) fn build(
        table: LayoutTable,
        lookup: &Lookup<'_>,
        table_budget: &AtomicUsize,
    ) -> Self {
        let count = usize::from(lookup.subtable_count());
        if !spend(table_budget, count) {
            return Self::admit_all();
        }
        let mut budget =
            MIN_COVERAGE_BUDGET.max(count.saturating_mul(COVERAGE_BUDGET_PER_SUBTABLE));
        let mut digest = Digest::EMPTY;
        let subtables: Vec<Digest> = (0..lookup.subtable_count())
            .map(|i| {
                let d = lookup
                    .subtable_bytes(i)
                    .and_then(|bytes| primary_coverage(table, lookup.lookup_type(), bytes))
                    .and_then(|cov| coverage_digest(&cov, &mut budget, table_budget))
                    .unwrap_or(Digest::FULL);
                digest.union(&d);
                d
            })
            .collect();
        let subtables = if subtables.iter().all(|d| *d == digest) {
            Box::default()
        } else {
            subtables.into_boxed_slice()
        };
        Self { digest, subtables }
    }

    /// The digest of every glyph a subtable of the lookup can start at.
    #[cfg(test)]
    pub(crate) fn digest(&self) -> &Digest {
        &self.digest
    }

    /// True when subtable `index` may apply at a glyph `glyph`: false
    /// only when the glyph is not in the subtable's primary coverage.
    #[inline]
    pub(crate) fn subtable_may_have(&self, index: usize, glyph: u16) -> bool {
        self.subtables
            .get(index)
            .map_or(true, |d| d.may_have(glyph))
    }
}

/// A lookup's subtables' primary coverages, read for one application
/// of the lookup without building digests. `None` admits every glyph.
#[derive(Debug)]
pub(crate) struct DirectAccel<'a> {
    /// The coverages of the first [`INLINE_COVERAGES`] subtables, kept
    /// inline so that a small lookup reads its coverages without
    /// allocating.
    first: [Option<Coverage<'a>>; INLINE_COVERAGES],
    /// The coverages of the other subtables.
    rest: Vec<Option<Coverage<'a>>>,
    count: usize,
}

/// Subtables whose coverages a [`DirectAccel`] keeps inline.
const INLINE_COVERAGES: usize = 4;

impl<'a> DirectAccel<'a> {
    fn new(table: LayoutTable, lookup: &Lookup<'a>) -> Self {
        let coverage = |i: u16| {
            let bytes = lookup.subtable_bytes(i)?;
            primary_coverage(table, lookup.lookup_type(), bytes)
        };
        let count = usize::from(lookup.subtable_count());
        let mut first = [None; INLINE_COVERAGES];
        for (i, slot) in (0..lookup.subtable_count()).zip(first.iter_mut()) {
            *slot = coverage(i);
        }
        let rest = (0..lookup.subtable_count())
            .skip(INLINE_COVERAGES)
            .map(coverage)
            .collect();
        Self { first, rest, count }
    }

    /// Subtable `index`'s coverage: `Some(None)` for a subtable without
    /// one sigilbuzz reads, `None` past the last subtable.
    #[inline]
    fn coverage(&self, index: usize) -> Option<&Option<Coverage<'a>>> {
        if index >= self.count {
            return None;
        }
        match index.checked_sub(INLINE_COVERAGES) {
            None => self.first.get(index),
            Some(i) => self.rest.get(i),
        }
    }

    #[inline]
    fn covers(&self, index: usize, glyph: u16) -> bool {
        self.coverage(index)
            .map_or(true, |c| c.as_ref().map_or(true, |c| c.contains(glyph)))
    }

    fn covers_any(&self, glyph: u16) -> bool {
        (0..self.count).any(|i| self.covers(i, glyph))
    }
}

/// A lookup's accelerator for one application: its digests when the
/// font has built them, else its subtables' coverages read directly.
/// Either way, a subtable or lookup it rules out for a glyph would not
/// have applied there.
#[derive(Debug)]
pub(crate) enum Accel<'c, 'a> {
    /// The digests the font keeps.
    Built(&'c LookupAccel),
    /// The coverages, read for this application.
    Direct(DirectAccel<'a>),
}

impl Accel<'_, '_> {
    /// False only when no glyph of `glyphs` can start a subtable.
    pub(crate) fn may_apply(&self, glyphs: impl IntoIterator<Item = u16>) -> bool {
        match self {
            Self::Built(a) => glyphs.into_iter().any(|g| a.digest.may_have(g)),
            Self::Direct(d) => glyphs.into_iter().any(|g| d.covers_any(g)),
        }
    }

    /// False only when no subtable can start at `glyph`. Without
    /// digests this searches the coverages until one holds the glyph,
    /// which lets a walk pass over a glyph before it reads the glyph's
    /// properties.
    #[inline]
    pub(crate) fn may_have(&self, glyph: u16) -> bool {
        match self {
            Self::Built(a) => a.digest.may_have(glyph),
            Self::Direct(d) => d.covers_any(glyph),
        }
    }

    /// [`Self::may_have`] where it costs no more than bit tests: without
    /// digests every glyph is admitted, and the subtables' own coverage
    /// checks decide.
    #[inline]
    pub(crate) fn may_have_cheaply(&self, glyph: u16) -> bool {
        match self {
            Self::Built(a) => a.digest.may_have(glyph),
            Self::Direct(_) => true,
        }
    }

    /// False only when subtable `index` cannot start at `glyph`.
    #[cfg(test)]
    pub(crate) fn subtable_may_have(&self, index: usize, glyph: u16) -> bool {
        self.subtable_may_start(false, index, glyph)
    }

    /// False only when subtable `index` cannot start at `glyph`, as a
    /// check ahead of applying it. Without digests, a subtable that is
    /// already `parsed` is let through: it looks the glyph up in its
    /// coverage first thing, so reading the coverage here too would
    /// only search it twice. One not parsed yet is checked, which keeps
    /// it from being parsed for a glyph it cannot cover.
    #[inline]
    pub(crate) fn subtable_may_start(&self, parsed: bool, index: usize, glyph: u16) -> bool {
        match self {
            Self::Built(a) => a.subtable_may_have(index, glyph),
            Self::Direct(d) => parsed || d.covers(index, glyph),
        }
    }
}

/// The accelerators of every lookup of one GSUB or GPOS table, each
/// built the first time it is asked for and then kept.
pub(crate) struct LookupAccels {
    table: LayoutTable,
    lookups: Box<[OnceBox<LookupAccel>]>,
    /// For a table whose LookupList names one lookup by several indices:
    /// for each index, the first index with the same offset, whose
    /// accelerator it shares. Empty when every index has its own offset.
    shared: Box<[u16]>,
    /// Coverage entries the accelerators may still read, and digests
    /// they may still keep.
    budget: AtomicUsize,
}

impl LookupAccels {
    /// Room for the lookups of `lookups`, the LookupList of a `table`
    /// table of `table_len` bytes, none built yet.
    pub(crate) fn new(table: LayoutTable, lookups: &LookupList<'_>, table_len: usize) -> Self {
        Self {
            table,
            lookups: (0..lookups.len()).map(|_| OnceBox::new()).collect(),
            shared: shared_lookups(lookups),
            budget: AtomicUsize::new(table_coverage_budget(table_len)),
        }
    }

    /// The accelerator of `lookup`, lookup `index` of the table, built
    /// now if this is the first time it or another index of the same
    /// lookup is asked for. Past the table's lookups, its coverages are
    /// read for this one use.
    pub(crate) fn get<'a>(&self, index: u16, lookup: &Lookup<'a>) -> Accel<'_, 'a> {
        let index = self
            .shared
            .get(usize::from(index))
            .copied()
            .unwrap_or(index);
        match self.lookups.get(usize::from(index)) {
            Some(slot) => Accel::Built(
                slot.get_or_init(|| LookupAccel::build(self.table, lookup, &self.budget)),
            ),
            None => Accel::Direct(DirectAccel::new(self.table, lookup)),
        }
    }

    /// How many lookup accelerators have been built.
    #[cfg(test)]
    pub(crate) fn built(&self) -> usize {
        self.lookups.iter().filter(|s| s.get().is_some()).count()
    }

    /// Heap bytes the accelerators hold.
    pub(crate) fn heap_bytes(&self) -> usize {
        let slots = self.lookups.len() * core::mem::size_of::<OnceBox<LookupAccel>>()
            + self.shared.len() * 2;
        let built: usize = self
            .lookups
            .iter()
            .filter_map(OnceBox::get)
            .map(|a| {
                core::mem::size_of::<LookupAccel>()
                    + a.subtables.len() * core::mem::size_of::<Digest>()
            })
            .sum();
        slots + built
    }
}

impl core::fmt::Debug for LookupAccels {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LookupAccels")
            .field("table", &self.table)
            .field("lookups", &self.lookups.len())
            .finish_non_exhaustive()
    }
}

/// What a [`crate::Font`] keeps for one GSUB or GPOS table: the lookup
/// accelerators, the resolved language systems, and the merged stage
/// plans.
#[derive(Debug)]
pub(crate) struct LayoutCache {
    pub(crate) accels: LookupAccels,
    pub(crate) maps: FeatureMaps,
    pub(crate) plans: StagePlans,
}

impl LayoutCache {
    /// An empty cache for a `table` table of `table_len` bytes whose
    /// LookupList is `lookups`.
    pub(crate) fn new(table: LayoutTable, lookups: &LookupList<'_>, table_len: usize) -> Self {
        Self {
            accels: LookupAccels::new(table, lookups, table_len),
            maps: FeatureMaps::new(),
            plans: StagePlans::new(),
        }
    }

    /// Heap bytes the cache holds.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.accels.heap_bytes() + self.maps.heap_bytes() + self.plans.heap_bytes()
    }
}

/// The accelerator of `lookup`: from `cache` when the view has one,
/// built now if need be, else its coverages read for this one use.
pub(crate) fn accel_for<'c, 'a>(
    cache: Option<&'c LayoutCache>,
    table: LayoutTable,
    index: u16,
    lookup: &Lookup<'a>,
) -> Accel<'c, 'a> {
    match cache {
        Some(cache) => cache.accels.get(index, lookup),
        None => Accel::Direct(DirectAccel::new(table, lookup)),
    }
}

/// The digest of `cov`'s glyphs, or `None` when reading them all would
/// overrun `budget` or `table_budget` (the caller then admits every
/// glyph). Spends the entries read from both.
fn coverage_digest(
    cov: &Coverage<'_>,
    budget: &mut usize,
    table_budget: &AtomicUsize,
) -> Option<Digest> {
    let entries = usize::from(cov.len());
    let left = budget.checked_sub(entries)?;
    if !spend(table_budget, entries) {
        return None;
    }
    *budget = left;
    let mut d = Digest::EMPTY;
    cov.for_each_range(|first, last| d.add_range(first, last));
    Some(d)
}

/// Takes `amount` from the shared `budget`, or nothing and false when
/// less is left. A compare-and-swap loop, so two threads building
/// accelerators of one table cannot both take its last entries.
fn spend(budget: &AtomicUsize, amount: usize) -> bool {
    let mut left = budget.load(Ordering::Relaxed);
    loop {
        let Some(rest) = left.checked_sub(amount) else {
            return false;
        };
        match budget.compare_exchange_weak(left, rest, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(now) => left = now,
        }
    }
}

/// For a LookupList that names one lookup by several indices, the
/// first index of each index's offset; empty when every index has its
/// own offset, as in fonts that compilers write. Lookups are usually
/// listed at increasing offsets, which needs no sorting to tell.
fn shared_lookups(lookups: &LookupList<'_>) -> Box<[u16]> {
    let offset = |index: u16| lookups.lookup_offset(index).unwrap_or(0);
    let count = lookups.len();
    if (1..count).all(|i| offset(i - 1) < offset(i)) {
        return Box::default();
    }
    let mut by_offset: Vec<(u16, u16)> = (0..count).map(|i| (offset(i), i)).collect();
    by_offset.sort_unstable();
    let mut first: Vec<u16> = (0..count).collect();
    for pair in by_offset.windows(2) {
        if let [(a, earlier), (b, later)] = *pair {
            if a == b {
                // Sorted by index within one offset, so `earlier`
                // already points at the first index of the offset.
                first[usize::from(later)] = first[usize::from(earlier)];
            }
        }
    }
    if first.iter().enumerate().all(|(i, &f)| usize::from(f) == i) {
        return Box::default();
    }
    first.into_boxed_slice()
}

/// The coverage the subtable at `bytes`, of lookup type `lookup_type`
/// in `table`, looks the cursor glyph up in before anything else, or
/// `None` when it cannot be read or the subtable has none sigilbuzz
/// reads that way (the caller then admits every glyph). Extension
/// subtables are looked through.
fn primary_coverage(table: LayoutTable, lookup_type: u16, bytes: &[u8]) -> Option<Coverage<'_>> {
    let read = |at: usize| -> Option<u16> {
        Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
    };
    let format = read(0)?;
    let (extension, context, chain) = match table {
        LayoutTable::Gsub => (7, 5, 6),
        LayoutTable::Gpos => (9, 7, 8),
    };
    let offset = match (table, lookup_type) {
        (_, t) if t == extension => {
            if format != 1 {
                return None;
            }
            let inner = read(2)?;
            if inner == extension {
                return None;
            }
            let off = u32::from_be_bytes([
                *bytes.get(4)?,
                *bytes.get(5)?,
                *bytes.get(6)?,
                *bytes.get(7)?,
            ]);
            let inner_bytes = bytes.get(usize::try_from(off).ok()?..)?;
            return primary_coverage(table, inner, inner_bytes);
        }
        (_, t) if t == context => match format {
            1 | 2 => read(2)?,
            // ContextFormat3: glyphCount, seqLookupCount, then the
            // input coverages.
            3 if read(2)? > 0 => read(6)?,
            _ => return None,
        },
        (_, t) if t == chain => match format {
            1 | 2 => read(2)?,
            // ChainContextFormat3: the backtrack coverages, then the
            // input ones.
            3 => {
                let backtrack = usize::from(read(2)?);
                let input_at = 4 + 2 * backtrack;
                if read(input_at)? == 0 {
                    return None;
                }
                read(input_at + 2)?
            }
            _ => return None,
        },
        // Single (formats 1 and 2), Multiple, Alternate, Ligature and
        // ReverseChainSingle substitution.
        (LayoutTable::Gsub, 1) if matches!(format, 1 | 2) => read(2)?,
        (LayoutTable::Gsub, 2 | 3 | 4 | 8) if format == 1 => read(2)?,
        // Single and pair adjustment (formats 1 and 2), cursive
        // attachment, and the mark attachments' mark coverage.
        (LayoutTable::Gpos, 1 | 2) if matches!(format, 1 | 2) => read(2)?,
        (LayoutTable::Gpos, 3..=6) if format == 1 => read(2)?,
        _ => return None,
    };
    Coverage::parse(bytes.get(usize::from(offset)..)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn digests_have_no_false_negatives() {
        let mut d = Digest::EMPTY;
        assert!(!d.may_have(5));
        for g in [0, 5, 63, 64, 1000, 4095, 65_535] {
            d.add(g);
        }
        for g in [0, 5, 63, 64, 1000, 4095, 65_535] {
            assert!(d.may_have(g), "{g}");
        }
        let mut r = Digest::EMPTY;
        r.add_range(60, 70);
        for g in 60..=70 {
            assert!(r.may_have(g), "{g}");
        }
        // Wrapping past bit 63 of every mask.
        let mut r = Digest::EMPTY;
        r.add_range(1000, 1100);
        for g in 1000..=1100 {
            assert!(r.may_have(g), "{g}");
        }
        let mut w = Digest::EMPTY;
        w.add_range(0, 65_535);
        assert_eq!(w, Digest::FULL);
        let mut e = Digest::EMPTY;
        e.add_range(9, 3);
        assert_eq!(e, Digest::EMPTY);
        assert!(d.may_intersect(&Digest::of([5])));
        assert!(!Digest::of([1]).may_intersect(&Digest::of([2])));
        assert!(!Digest::EMPTY.may_intersect(&Digest::FULL));
    }

    #[test]
    fn exhaustive_ranges_match_the_glyphs_they_cover() {
        for (first, last) in [(0u16, 0u16), (3, 130), (500, 520), (63, 64), (4000, 9000)] {
            let mut r = Digest::EMPTY;
            r.add_range(first, last);
            let mut each = Digest::EMPTY;
            for g in first..=last {
                each.add(g);
            }
            // The range sets exactly the bits its glyphs set.
            assert_eq!(r, each, "{first}..={last}");
        }
    }

    fn push16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_be_bytes());
    }

    /// A LookupList of one lookup of `lookup_type` whose subtables are
    /// `subtables`.
    fn lookup_list(lookup_type: u16, subtables: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![];
        push16(&mut out, 1);
        push16(&mut out, 4);
        push16(&mut out, lookup_type);
        push16(&mut out, 0);
        push16(&mut out, subtables.len() as u16);
        let mut at = 6 + 2 * subtables.len();
        for s in subtables {
            push16(&mut out, at as u16);
            at += s.len();
        }
        for s in subtables {
            out.extend_from_slice(s);
        }
        out
    }

    /// A SingleSubst format 1 over `glyphs` (coverage format 1).
    fn single(glyphs: &[u16]) -> Vec<u8> {
        let mut out = vec![];
        push16(&mut out, 1);
        push16(&mut out, 6);
        push16(&mut out, 1);
        push16(&mut out, 1);
        push16(&mut out, glyphs.len() as u16);
        for &g in glyphs {
            push16(&mut out, g);
        }
        out
    }

    #[test]
    fn subtable_digests_follow_their_coverage() {
        let data = lookup_list(1, &[single(&[10, 20]), single(&[700]), vec![0, 9]]);
        let list = LookupList::parse(&data).unwrap();
        let lookup = list.get(0).unwrap();
        let accel = LookupAccel::build(LayoutTable::Gsub, &lookup, &AtomicUsize::new(usize::MAX));
        assert!(accel.subtable_may_have(0, 10));
        assert!(accel.subtable_may_have(0, 20));
        assert!(!accel.subtable_may_have(0, 700));
        assert!(accel.subtable_may_have(1, 700));
        assert!(!accel.subtable_may_have(1, 10));
        // An unknown format admits every glyph.
        assert!(accel.subtable_may_have(2, 12_345));
        assert!(accel.subtable_may_have(3, 12_345));
        assert!(accel.digest().may_have(700));
        assert_eq!(*accel.digest(), Digest::FULL);
        let data = lookup_list(1, &[single(&[10, 20]), single(&[700])]);
        let list = LookupList::parse(&data).unwrap();
        let accel = LookupAccel::build(
            LayoutTable::Gsub,
            &list.get(0).unwrap(),
            &AtomicUsize::new(usize::MAX),
        );
        assert!(!accel.digest().may_have(11));
        assert!(!accel.digest().may_have(5000));
    }

    #[test]
    fn subtables_with_the_lookup_digest_keep_none_of_their_own() {
        for subs in [vec![single(&[10, 20])], vec![single(&[10, 20]); 3]] {
            let data = lookup_list(1, &subs);
            let list = LookupList::parse(&data).unwrap();
            let accel = LookupAccel::build(
                LayoutTable::Gsub,
                &list.get(0).unwrap(),
                &AtomicUsize::new(usize::MAX),
            );
            assert!(accel.subtables.is_empty());
            assert_eq!(*accel.digest(), Digest::of([10, 20]));
            assert!(accel.subtable_may_have(subs.len() - 1, 20));
        }
    }

    #[test]
    fn a_shared_huge_coverage_admits_everything_past_the_budget() {
        // Many subtables share one big coverage: past the budget the
        // rest admit every glyph instead of reading it again.
        let glyphs: Vec<u16> = (0..4000).map(|g| g * 2).collect();
        let sub = single(&glyphs);
        let subs: Vec<Vec<u8>> = (0..10).map(|_| sub.clone()).collect();
        let data = lookup_list(1, &subs);
        let list = LookupList::parse(&data).unwrap();
        let accel = LookupAccel::build(
            LayoutTable::Gsub,
            &list.get(0).unwrap(),
            &AtomicUsize::new(usize::MAX),
        );
        assert!(!accel.subtable_may_have(0, 65_001));
        assert!(accel.subtable_may_have(9, 65_001));
    }

    #[test]
    fn a_table_budget_bounds_reads_across_lookups() {
        // Every lookup reads the same 4000-entry coverage; once the
        // table's budget is spent, later lookups admit every glyph.
        let glyphs: Vec<u16> = (0..4000).map(|g| g * 2).collect();
        let data = lookup_list(1, &[single(&glyphs)]);
        let list = LookupList::parse(&data).unwrap();
        let lookup = list.get(0).unwrap();
        let budget = AtomicUsize::new(10_000);
        let first = LookupAccel::build(LayoutTable::Gsub, &lookup, &budget);
        let second = LookupAccel::build(LayoutTable::Gsub, &lookup, &budget);
        let third = LookupAccel::build(LayoutTable::Gsub, &lookup, &budget);
        // One subtable each, so each keeps only the lookup's digest.
        assert!(!first.digest().may_have(65_001));
        assert!(!second.digest().may_have(65_001));
        assert!(third.digest().may_have(65_001));
        // Three digests kept, two coverages read.
        assert_eq!(budget.load(Ordering::Relaxed), 2000 - 3);
        assert_eq!(table_coverage_budget(1000), 2000 + (1 << 16));
    }

    #[test]
    fn kept_digests_are_paid_from_the_table_budget() {
        // 500 subtables of one-glyph coverages: keeping their digests
        // costs 500 entries, and reading their coverages 500 more.
        let subs: Vec<Vec<u8>> = (0..500).map(|g| single(&[g])).collect();
        let data = lookup_list(1, &subs);
        let list = LookupList::parse(&data).unwrap();
        let lookup = list.get(0).unwrap();
        let budget = AtomicUsize::new(1499);
        let kept = LookupAccel::build(LayoutTable::Gsub, &lookup, &budget);
        assert_eq!(kept.subtables.len(), 500);
        assert!(kept.subtable_may_have(7, 7));
        assert!(!kept.subtable_may_have(7, 8));
        assert_eq!(budget.load(Ordering::Relaxed), 499);
        // What is left cannot pay for 500 digests: the next build keeps
        // none, reads nothing, and admits every glyph at every subtable.
        let none = LookupAccel::build(LayoutTable::Gsub, &lookup, &budget);
        assert!(none.subtables.is_empty());
        assert_eq!(*none.digest(), Digest::FULL);
        assert!(none.subtable_may_have(7, 8));
        assert_eq!(budget.load(Ordering::Relaxed), 499);
    }

    /// A LookupList whose index `i` names lookup `targets[i]` of
    /// `lookups`, each a lookup of single substitutions `subtables`.
    fn list_of(targets: &[usize], lookups: &[&[Vec<u8>]]) -> Vec<u8> {
        // Each lookup as `lookup_list` lays it out, past the list's
        // count and offset.
        let bodies: Vec<Vec<u8>> = lookups
            .iter()
            .map(|subtables| lookup_list(1, subtables)[4..].to_vec())
            .collect();
        let mut at = 2 + 2 * targets.len();
        let mut starts = vec![];
        for body in &bodies {
            starts.push(at);
            at += body.len();
        }
        let mut out = vec![];
        push16(&mut out, targets.len() as u16);
        for &t in targets {
            push16(&mut out, starts[t] as u16);
        }
        for body in &bodies {
            out.extend_from_slice(body);
        }
        out
    }

    #[test]
    fn indices_of_one_lookup_share_its_accelerator() {
        let a = [single(&[10])];
        let b = [single(&[20]), single(&[30])];
        // Indices 0, 2 and 3 name lookup a, index 1 names lookup b.
        let data = list_of(&[0, 1, 0, 0], &[&a, &b]);
        let list = LookupList::parse(&data).unwrap();
        assert_eq!(&*shared_lookups(&list), &[0, 1, 0, 0]);
        let accels = LookupAccels::new(LayoutTable::Gsub, &list, data.len());
        let built = |i: u16| match accels.get(i, &list.get(i).unwrap()) {
            Accel::Built(a) => core::ptr::from_ref(a),
            Accel::Direct(_) => panic!("lookup {i} has no slot"),
        };
        let first = built(2);
        assert_eq!(built(0), first);
        assert_eq!(built(3), first);
        assert_ne!(built(1), first);
        assert_eq!(accels.built(), 2);
        // Lists that name each lookup once share nothing and keep no
        // map, at increasing offsets or not.
        for targets in [[0, 1], [1, 0]] {
            let data = list_of(&targets, &[&a, &b]);
            assert!(shared_lookups(&LookupList::parse(&data).unwrap()).is_empty());
        }
    }

    #[test]
    fn accelerators_are_built_once_and_agree_with_the_coverages() {
        let data = lookup_list(1, &[single(&[10]), single(&[3000])]);
        let list = LookupList::parse(&data).unwrap();
        let lookup = list.get(0).unwrap();
        let accels = LookupAccels::new(LayoutTable::Gsub, &list, data.len());
        assert_eq!(accels.built(), 0);
        let first = accel_for(None, LayoutTable::Gsub, 0, &lookup);
        assert!(matches!(first, Accel::Direct(_)));
        let second = accels.get(0, &lookup);
        assert!(matches!(second, Accel::Built(_)));
        assert!(matches!(accels.get(0, &lookup), Accel::Built(_)));
        assert_eq!(accels.built(), 1);
        // Both answer alike.
        for a in [&first, &second] {
            assert!(a.may_apply([1, 10]));
            assert!(!a.may_apply([1, 11]));
            assert!(a.subtable_may_have(0, 10));
            assert!(!a.subtable_may_have(0, 3000));
            assert!(a.subtable_may_have(1, 3000));
            assert!(!a.subtable_may_have(1, 10));
            assert!(a.subtable_may_have(5, 10));
        }
        assert!(!second.may_have(11));
        // Past the slots, the coverages are read for one use.
        assert!(matches!(accels.get(7, &lookup), Accel::Direct(_)));
        assert_eq!(accels.built(), 1);
        assert!(accels.heap_bytes() > 0);
    }
}
