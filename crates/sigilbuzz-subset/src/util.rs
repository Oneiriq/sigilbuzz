//! Small in-place rewrites for tables we mostly pass through, plus the
//! work budget shared by the table walkers.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use sigilbuzz::tables::tag;
use sigilbuzz::tables::variation_store::ItemVariationStore;
use sigilbuzz::Face;

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

/// The deltas of an `ItemVariationStore` at fixed coordinates, each row
/// worked out once.
///
/// Resolving one row walks every region its subtable names, and a
/// small table can point many records (glyphs through an index map,
/// GPOS value records, BASE coordinates) at one large row. Remembering
/// each row's delta keeps the work to one walk per row the store holds.
pub(crate) struct StoreDeltas<'s, 'a> {
    store: &'s ItemVariationStore<'a>,
    coords: &'s [f32],
    memo: RefCell<BTreeMap<(u16, u16), f32>>,
}

impl<'s, 'a> StoreDeltas<'s, 'a> {
    /// The deltas of `store` at `coords`.
    pub(crate) const fn new(store: &'s ItemVariationStore<'a>, coords: &'s [f32]) -> Self {
        Self {
            store,
            coords,
            memo: RefCell::new(BTreeMap::new()),
        }
    }

    /// The delta of row `(outer, inner)`, as
    /// [`ItemVariationStore::delta`] gives it.
    pub(crate) fn get(&self, outer: u16, inner: u16) -> f32 {
        if let Some(&d) = self.memo.borrow().get(&(outer, inner)) {
            return d;
        }
        let d = self.store.delta(outer, inner, self.coords);
        self.memo.borrow_mut().insert((outer, inner), d);
        d
    }

    /// Rows worked out so far.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> usize {
        self.memo.borrow().len()
    }
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
