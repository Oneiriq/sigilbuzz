//! Small in-place rewrites for tables we mostly pass through, plus the
//! work budget shared by the table walkers.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use sigilbuzz::tables::tag;
use sigilbuzz::tables::variation_store::ItemVariationStore;
use sigilbuzz::Face;

use crate::warnings::Warnings;
use crate::SubsetError;

/// Default number of work units one walk over font data may spend.
///
/// Layout, VARC, and GPOS tables can point many records at the same
/// bytes, so a small table can describe billions of records. Each
/// walker charges a [`WorkBudget`] for the records and bytes it
/// visits, and gives up once the budget runs out.
///
/// The heaviest font under `tests/` spends under a million units, so
/// this leaves more than an order of magnitude of headroom. Lowering it
/// to 2^18 does change the output of the bundled fonts, so it is not a
/// limit real fonts approach.
pub(crate) const WORK_LIMIT: u64 = 1 << 24;

/// A counter of work units left for one walk over font data.
///
/// Uses a [`Cell`] so walkers that only hold a shared reference can
/// still charge it.
pub(crate) struct WorkBudget {
    left: Cell<u64>,
}

impl WorkBudget {
    /// Creates a budget holding `units` work units.
    pub(crate) const fn new(units: u64) -> Self {
        Self {
            left: Cell::new(units),
        }
    }

    /// Spends `units`. Returns false, and empties the budget, when
    /// fewer than `units` remain.
    pub(crate) fn spend(&self, units: usize) -> bool {
        let units = units as u64;
        match self.left.get().checked_sub(units) {
            Some(rest) => {
                self.left.set(rest);
                true
            }
            None => {
                self.left.set(0);
                false
            }
        }
    }

    /// True once a call to [`WorkBudget::spend`] has failed or the
    /// budget has reached zero.
    pub(crate) fn is_spent(&self) -> bool {
        self.left.get() == 0
    }

    /// Refills the budget to `units`.
    pub(crate) fn reset(&self, units: u64) {
        self.left.set(units);
    }
}

/// Work units a [`StoreDeltas`] may spend per byte of its store. A row
/// costs one unit per delta slot, and a store whose subtables do not
/// overlap holds at least a byte per slot of each row, so a real store
/// spends at most twice its size.
const STORE_WORK_PER_BYTE: u64 = 8;

/// The least work a [`StoreDeltas`] may spend, for small stores.
const MIN_STORE_WORK: u64 = 1 << 20;

/// What a [`StoreDeltas`] reports once its budget runs out.
const STORE_OVER_BUDGET: &str = "variation store: row work exceeds its budget";

/// The deltas of an `ItemVariationStore` at fixed coordinates.
///
/// A small table can point many records (glyphs through an index map,
/// GPOS value records and anchors, BASE coordinates, ligature carets)
/// at one large row, and many rows at one region of many axes, so each
/// row and each region is worked out once. Subtable offsets can still
/// alias one large subtable under many outer indices, so every new row
/// also charges a [`WorkBudget`] scaled to the store's size, one unit
/// per delta slot. Past it, rows resolve to zero, which leaves their
/// source values in place, and the run is warned.
pub(crate) struct StoreDeltas<'s, 'a> {
    /// The store, from its first byte to the end of its table.
    bytes: &'a [u8],
    store: ItemVariationStore<'a>,
    coords: &'s [f32],
    /// Each region's scalar at `coords`, by region index.
    regions: RefCell<BTreeMap<u16, Option<f32>>>,
    /// Each resolved row's delta, by `(outer, inner)`.
    memo: RefCell<BTreeMap<(u16, u16), f32>>,
    work: WorkBudget,
    /// Where a spent budget is reported, and against which table.
    report: Option<(&'s Warnings, [u8; 4])>,
    warned: Cell<bool>,
}

impl<'s, 'a> StoreDeltas<'s, 'a> {
    /// The deltas at `coords` of the store that starts at byte 0 of
    /// `bytes`; `None` when the store cannot be read.
    pub(crate) fn new(bytes: &'a [u8], coords: &'s [f32]) -> Option<Self> {
        let store = ItemVariationStore::parse(bytes).ok()?;
        let units = (bytes.len() as u64)
            .saturating_mul(STORE_WORK_PER_BYTE)
            .max(MIN_STORE_WORK);
        Some(Self {
            bytes,
            store,
            coords,
            regions: RefCell::new(BTreeMap::new()),
            memo: RefCell::new(BTreeMap::new()),
            work: WorkBudget::new(units),
            report: None,
            warned: Cell::new(false),
        })
    }

    /// The same deltas, reporting a spent budget to `warnings` against
    /// `table`.
    pub(crate) fn reporting(self, warnings: &'s Warnings, table: [u8; 4]) -> Self {
        Self {
            report: Some((warnings, table)),
            ..self
        }
    }

    /// The delta of row `(outer, inner)`, as
    /// [`ItemVariationStore::delta`] gives it; zero once the budget is
    /// spent.
    pub(crate) fn get(&self, outer: u16, inner: u16) -> f32 {
        if let Some(&d) = self.memo.borrow().get(&(outer, inner)) {
            return d;
        }
        match self.resolve(outer, inner) {
            Some(d) => {
                self.memo.borrow_mut().insert((outer, inner), d);
                d
            }
            None => {
                if let Some((warnings, table)) = self.report.filter(|_| !self.warned.get()) {
                    warnings.push(
                        table,
                        0,
                        STORE_OVER_BUDGET,
                        "the deltas of the rows past it",
                    );
                }
                self.warned.set(true);
                0.0
            }
        }
    }

    /// Works out row `(outer, inner)` the way
    /// [`ItemVariationStore::delta`] does, summing in slot order, with
    /// each region's scalar from the cache. A row the store cannot hold
    /// is zero; `None` when the budget runs out.
    fn resolve(&self, outer: u16, inner: u16) -> Option<f32> {
        let b = self.bytes;
        if outer >= self.store.subtable_count() {
            return Some(0.0);
        }
        // The parse read every subtable offset.
        let Some(off) = be_u32(b, 8 + 4 * usize::from(outer)) else {
            return Some(0.0);
        };
        let off = off as usize;
        let (Some(item_count), Some(word_raw), Some(slots)) = (
            be_u16(b, off),
            be_u16(b, off.saturating_add(2)),
            be_u16(b, off.saturating_add(4)),
        ) else {
            return Some(0.0);
        };
        if !self.work.spend(usize::from(slots) + 1) {
            return None;
        }
        let long_words = word_raw & 0x8000 != 0;
        let word_count = word_raw & 0x7FFF;
        if word_count > slots || inner >= item_count {
            return Some(0.0);
        }
        let (wide, narrow) = if long_words { (4, 2) } else { (2, 1) };
        let row_size = usize::from(word_count) * wide + usize::from(slots - word_count) * narrow;
        let indexes = off + 6;
        let rows = indexes + 2 * usize::from(slots);
        let end = usize::from(item_count)
            .checked_mul(row_size)
            .and_then(|n| n.checked_add(rows));
        if end.map_or(true, |end| b.len() < end) {
            return Some(0.0);
        }
        let mut cursor = rows + usize::from(inner) * row_size;
        let mut out: f32 = 0.0;
        for slot in 0..slots {
            let (value, size) = match (slot < word_count, long_words) {
                (true, true) => (be_u32(b, cursor).map(|v| v as i32), 4),
                (true, false) | (false, true) => {
                    (be_u16(b, cursor).map(|v| i32::from(v as i16)), 2)
                }
                (false, false) => (b.get(cursor).map(|&v| i32::from(v as i8)), 1),
            };
            cursor += size;
            let (Some(value), Some(region)) = (value, be_u16(b, indexes + 2 * usize::from(slot)))
            else {
                return Some(0.0);
            };
            if let Some(scalar) = self.region(region) {
                out += scalar * value as f32;
            }
        }
        Some(out)
    }

    /// The scalar of region `index` at the coordinates, worked out once.
    fn region(&self, index: u16) -> Option<f32> {
        if let Some(&s) = self.regions.borrow().get(&index) {
            return s;
        }
        let s = self.store.region_scalar(index, self.coords);
        self.regions.borrow_mut().insert(index, s);
        s
    }

    /// Rows worked out so far.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> usize {
        self.memo.borrow().len()
    }

    /// Regions worked out so far.
    #[cfg(test)]
    pub(crate) fn regions(&self) -> usize {
        self.regions.borrow().len()
    }

    /// Work units left.
    #[cfg(test)]
    pub(crate) fn work_left(&self) -> u64 {
        self.work.left.get()
    }
}

/// The big-endian `u16` at byte `off` of `b`.
fn be_u16(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..)
        .and_then(<[u8]>::first_chunk::<2>)
        .map(|v| u16::from_be_bytes(*v))
}

/// The big-endian `u32` at byte `off` of `b`.
fn be_u32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..)
        .and_then(<[u8]>::first_chunk::<4>)
        .map(|v| u32::from_be_bytes(*v))
}

/// Patches `head.indexToLocFormat` (offset 50: 0=short, 1=long).
pub fn write_index_to_loc_format(head: &mut [u8], long: bool) {
    if head.len() < 52 {
        return;
    }
    let val: i16 = if long { 1 } else { 0 };
    head[50..52].copy_from_slice(&val.to_be_bytes());
}

/// Byte offset of `hhea.numberOfHMetrics` and of
/// `vhea.numberOfLongVerMetrics`, where the parsers read them.
const METRICS_COUNT_OFFSET: usize = 34;

/// Patches `hhea.numberOfHMetrics` at byte 34. A table padded past its
/// 36 bytes keeps its tail.
pub fn write_hhea_metrics_count(hhea: &mut [u8], n: u16) -> Result<(), SubsetError> {
    write_metrics_count(hhea, n).ok_or(SubsetError::Unsupported("hhea too short to patch"))
}

/// Patches `vhea.numberOfLongVerMetrics` at byte 34 (vhea v1.0 and
/// v1.1 share the byte layout of `hhea`). A table padded past its 36
/// bytes keeps its tail.
pub fn write_vhea_metrics_count(vhea: &mut [u8], n: u16) -> Result<(), SubsetError> {
    write_metrics_count(vhea, n).ok_or(SubsetError::Unsupported("vhea too short to patch"))
}

/// Writes `n` at [`METRICS_COUNT_OFFSET`]; `None` when the table is too
/// short to hold it.
fn write_metrics_count(table: &mut [u8], n: u16) -> Option<()> {
    let field = table
        .get_mut(METRICS_COUNT_OFFSET..)?
        .first_chunk_mut::<2>()?;
    *field = n.to_be_bytes();
    Some(())
}

/// Patches `maxp.numGlyphs` (offset 4..6).
pub fn write_maxp_num_glyphs(maxp: &mut [u8], n: u16) -> Result<(), SubsetError> {
    if maxp.len() < 6 {
        return Err(SubsetError::Unsupported("maxp too short to patch"));
    }
    maxp[4..6].copy_from_slice(&n.to_be_bytes());
    Ok(())
}

/// Builds a `post` format-3 table. Format 3 carries no glyph names
/// at all: the only payload is the 32-byte header. We pull
/// `italicAngle` / `underlinePosition` / `underlineThickness` /
/// `isFixedPitch` from the source font when present so kerning-
/// adjacent renderers that consult these still get sane values.
pub fn synthesize_post_format_3(face: &Face<'_>) -> Result<Vec<u8>, SubsetError> {
    let mut out = Vec::with_capacity(32);
    // version: 3.0 (0x00030000)
    out.extend_from_slice(&0x0003_0000u32.to_be_bytes());

    // Default values when the source has no post.
    let mut italic_angle: u32 = 0; // Fixed16.16
    let mut underline_position: i16 = 0;
    let mut underline_thickness: i16 = 0;
    let mut is_fixed_pitch: u32 = 0;
    let mut min_mem_t42: u32 = 0;
    let mut max_mem_t42: u32 = 0;
    let mut min_mem_t1: u32 = 0;
    let mut max_mem_t1: u32 = 0;

    if let Ok(post) = face.table_bytes(tag::POST) {
        if post.len() >= 32 {
            italic_angle = u32::from_be_bytes([post[4], post[5], post[6], post[7]]);
            underline_position = i16::from_be_bytes([post[8], post[9]]);
            underline_thickness = i16::from_be_bytes([post[10], post[11]]);
            is_fixed_pitch = u32::from_be_bytes([post[12], post[13], post[14], post[15]]);
            min_mem_t42 = u32::from_be_bytes([post[16], post[17], post[18], post[19]]);
            max_mem_t42 = u32::from_be_bytes([post[20], post[21], post[22], post[23]]);
            min_mem_t1 = u32::from_be_bytes([post[24], post[25], post[26], post[27]]);
            max_mem_t1 = u32::from_be_bytes([post[28], post[29], post[30], post[31]]);
        }
    }

    out.extend_from_slice(&italic_angle.to_be_bytes());
    out.extend_from_slice(&underline_position.to_be_bytes());
    out.extend_from_slice(&underline_thickness.to_be_bytes());
    out.extend_from_slice(&is_fixed_pitch.to_be_bytes());
    out.extend_from_slice(&min_mem_t42.to_be_bytes());
    out.extend_from_slice(&max_mem_t42.to_be_bytes());
    out.extend_from_slice(&min_mem_t1.to_be_bytes());
    out.extend_from_slice(&max_mem_t1.to_be_bytes());

    debug_assert_eq!(out.len(), 32);
    Ok(out)
}

/// Rounds to the nearest integer, halves up (toward positive infinity):
/// `floor(v + 0.5)`, the rounding HarfBuzz's instancer (its own
/// `roundf`) and fontTools (`otRound`) both use. Saturates at the
/// `i32` range; NaN becomes zero.
pub(crate) fn round_half_up(v: f32) -> i32 {
    let x = v + 0.5;
    // `as` truncates toward zero and saturates; step down once for a
    // negative value with a fraction to get the floor.
    #[allow(clippy::cast_possible_truncation)]
    let t = x as i32;
    if (t as f32) > x {
        t.saturating_sub(1)
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ItemVariationStore over `axes` axes with `regions` regions
    /// (region `r` peaks at 1.0 on axis `r % axes` alone) and `outers`
    /// subtable offsets that all point at one subtable of `rows` rows
    /// over `slots` slots (slot `s` names region `s % regions`). The
    /// first `words` slots are wide, and `long` widens every slot.
    fn build_store(
        axes: u16,
        regions: u16,
        outers: u16,
        slots: u16,
        rows: u16,
        words: u16,
        long: bool,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        let region_off = 8 + 4 * usize::from(outers);
        out.extend_from_slice(&(region_off as u32).to_be_bytes());
        out.extend_from_slice(&outers.to_be_bytes());
        let sub_off = region_off + 4 + 6 * usize::from(axes) * usize::from(regions);
        for _ in 0..outers {
            out.extend_from_slice(&(sub_off as u32).to_be_bytes());
        }
        out.extend_from_slice(&axes.to_be_bytes());
        out.extend_from_slice(&regions.to_be_bytes());
        for r in 0..regions {
            for a in 0..axes {
                let peak: i16 = if a == r % axes { 16384 } else { 0 };
                for v in [0, peak, peak] {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
        }
        out.extend_from_slice(&rows.to_be_bytes());
        let flag = if long { 0x8000 } else { 0 };
        out.extend_from_slice(&(flag | words).to_be_bytes());
        out.extend_from_slice(&slots.to_be_bytes());
        for s in 0..slots {
            out.extend_from_slice(&(s % regions).to_be_bytes());
        }
        for row in 0..rows {
            for s in 0..slots {
                let v = (i32::from(row) * 3 - i32::from(s) * 5 + 1) % 100;
                match (s < words, long) {
                    (true, true) => out.extend_from_slice(&(v * 1000).to_be_bytes()),
                    (true, false) | (false, true) => {
                        out.extend_from_slice(&((v * 100) as i16).to_be_bytes());
                    }
                    (false, false) => out.push(v as i8 as u8),
                }
            }
        }
        out
    }

    #[test]
    fn store_deltas_match_the_core_store() {
        let coords = [0.5, 0.25, 0.75];
        for (words, long) in [(0, false), (2, false), (0, true), (2, true)] {
            let bytes = build_store(3, 4, 2, 5, 6, words, long);
            let core = ItemVariationStore::parse(&bytes).unwrap();
            let deltas = StoreDeltas::new(&bytes, &coords).unwrap();
            for outer in 0..3 {
                for inner in 0..8 {
                    assert_eq!(
                        deltas.get(outer, inner).to_bits(),
                        core.delta(outer, inner, &coords).to_bits(),
                        "({outer}, {inner}) with {words} words, long {long}"
                    );
                }
            }
        }
    }

    #[test]
    fn store_deltas_evaluate_each_region_once() {
        // 4,000 rows over one region of 512 axes: the region is worked
        // out once, and each row costs its one slot plus one.
        let coords = alloc::vec![0.5; 512];
        let bytes = build_store(512, 1, 1, 1, 4000, 0, false);
        let core = ItemVariationStore::parse(&bytes).unwrap();
        let deltas = StoreDeltas::new(&bytes, &coords).unwrap();
        let budget = deltas.work_left();
        for inner in 0..4000 {
            assert_eq!(deltas.get(0, inner), core.delta(0, inner, &coords));
        }
        assert_eq!(deltas.regions(), 1);
        assert_eq!(deltas.rows(), 4000);
        assert_eq!(budget - deltas.work_left(), 4000 * 2);
    }

    #[test]
    fn store_deltas_stop_at_the_budget_on_aliased_subtables() {
        // 4,096 outer indices alias one subtable of 1,000 slots, so no
        // two keys share a row yet every key walks 1,000 slots. The
        // walk stops once it has spent its budget; the rows past it
        // read zero, and the run is warned once.
        let bytes = build_store(1, 1, 4096, 1000, 2, 0, false);
        let core = ItemVariationStore::parse(&bytes).unwrap();
        let warnings = Warnings::default();
        let deltas = StoreDeltas::new(&bytes, &[1.0])
            .unwrap()
            .reporting(&warnings, *b"GPOS");
        let budget = deltas.work_left();
        assert_eq!(budget, MIN_STORE_WORK, "a small store gets the floor");
        let affordable = budget / 1001;
        for outer in 0..4096u16 {
            let d = deltas.get(outer, 0);
            if u64::from(outer) < affordable {
                assert_eq!(d, core.delta(outer, 0, &[1.0]));
                assert_ne!(d, 0.0);
            } else {
                assert_eq!(d, 0.0, "outer {outer} is past the budget");
            }
        }
        assert_eq!(deltas.rows() as u64, affordable);
        assert_eq!(deltas.work_left(), 0);
        let warned = warnings.into_sorted();
        assert_eq!(warned.len(), 1);
        assert_eq!(warned[0].context, STORE_OVER_BUDGET);
    }

    #[test]
    fn write_loc_format_short_long() {
        let mut head = alloc::vec![0u8; 54];
        write_index_to_loc_format(&mut head, true);
        assert_eq!(head[50], 0);
        assert_eq!(head[51], 1);
        write_index_to_loc_format(&mut head, false);
        assert_eq!(head[50], 0);
        assert_eq!(head[51], 0);
    }

    #[test]
    fn write_hhea_count_patches_tail() {
        let mut hhea = alloc::vec![0u8; 36];
        write_hhea_metrics_count(&mut hhea, 42).unwrap();
        assert_eq!(&hhea[34..36], &42u16.to_be_bytes());
    }

    #[test]
    fn metrics_counts_are_patched_at_byte_34_of_a_padded_table() {
        // A table longer than 36 bytes keeps its count at byte 34,
        // where the parsers read it. Patching the last two bytes
        // instead left the count stale and wrote over the padding.
        for write in [write_hhea_metrics_count, write_vhea_metrics_count] {
            let mut table = alloc::vec![0xAAu8; 40];
            write(&mut table, 0x0102).unwrap();
            assert_eq!(&table[34..36], &[0x01, 0x02]);
            assert_eq!(&table[36..], &[0xAA; 4], "the tail is untouched");
            assert!(write(&mut alloc::vec![0u8; 35], 1).is_err());
        }
    }

    #[test]
    fn write_maxp_glyphs_patches_offset_4() {
        let mut maxp = alloc::vec![0u8; 6];
        write_maxp_num_glyphs(&mut maxp, 9000).unwrap();
        assert_eq!(&maxp[4..6], &9000u16.to_be_bytes());
    }

    #[test]
    fn work_budget_empties_when_overspent() {
        let budget = WorkBudget::new(10);
        assert!(budget.spend(4));
        assert!(!budget.is_spent());
        assert!(budget.spend(6));
        assert!(budget.is_spent());
        let budget = WorkBudget::new(10);
        assert!(!budget.spend(11));
        assert!(budget.is_spent());
        budget.reset(3);
        assert!(budget.spend(3));
    }
}
