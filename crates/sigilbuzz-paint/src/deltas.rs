//! Variation deltas for `PaintVar*` fields, `VarColorStop`s, and
//! variable clip boxes.
//!
//! Every variable COLRv1 value names a delta by `varIndexBase + i`,
//! where `i` is the field's position in its record. The delta comes from
//! COLR's own ItemVariationStore, exactly as HarfBuzz reads it:
//!
//! - When the COLR header has a DeltaSetIndexMap, the index is mapped
//!   through it first. The map yields a packed `(outer, inner)` pair; an
//!   index past the end of the map uses the last entry, and a map with
//!   no entries passes the index through unchanged.
//! - Otherwise the index itself is the pair: `outer` in the high 16
//!   bits, `inner` in the low 16 bits.
//!
//! No other table is consulted; GDEF's variation store belongs to GDEF
//! and GPOS. The sentinel `0xFFFFFFFF` means "no delta".
//!
//! The coordinates are rounded to F2DOT14 first, multiples of 1/16384
//! with halves rounded up, the precision HarfBuzz stores a font's
//! coordinates in. Shaping and `Face::glyph_outline_at_coords` round
//! them the same way, so a paint and the outlines it fills vary at the
//! same instance. Coordinates that all round to zero, or none at all,
//! are the default instance and mean "no delta".
//!
//! Both [`crate::evaluate_with`] and [`crate::walk`] read deltas through
//! the same [`Deltas`] value, so they never disagree about units or index
//! mapping.

use alloc::vec::Vec;

use sigilbuzz::tables::colr::{ClipBox, Colr, VarIndexBase};
use sigilbuzz::tables::variation_store::ItemVariationStore;

/// `varIndexBase` value meaning "this record does not vary".
const NO_VARIATION: VarIndexBase = VarIndexBase::MAX;

/// Variation deltas for one evaluation: COLR's item variation store,
/// its optional index map, and the normalized coordinates rounded to
/// F2DOT14.
pub(crate) struct Deltas<'a> {
    store: Option<ItemVariationStore<'a>>,
    map: Option<DeltaSetIndexMap<'a>>,
    coords: Vec<f32>,
}

impl<'a> Deltas<'a> {
    /// Reads the variation store and index map named by `colr`'s header.
    /// An offset that does not lead to a well-formed structure is
    /// treated as absent, which is what HarfBuzz's sanitizer does to it.
    pub(crate) fn new(colr: &Colr<'a>, coords: &[f32]) -> Self {
        let data = colr.data();
        let store = colr
            .var_store_offset()
            .and_then(|off| data.get(off as usize..))
            .and_then(|bytes| ItemVariationStore::parse(bytes).ok());
        let map = colr
            .var_index_map_offset()
            .and_then(|off| DeltaSetIndexMap::parse(data, off as usize));
        Self {
            store,
            map,
            coords: f2dot14_coords(coords),
        }
    }

    /// The raw delta for field `field` of a record whose base index is
    /// `base`, in the field's own units (design units, or F2DOT14 /
    /// Fixed ticks).
    pub(crate) fn raw(&self, base: VarIndexBase, field: u16) -> f32 {
        if base == NO_VARIATION || self.coords.is_empty() {
            return 0.0;
        }
        let Some(store) = self.store.as_ref() else {
            return 0.0;
        };
        let index = base.wrapping_add(u32::from(field));
        let index = self.map.as_ref().map_or(index, |map| map.map(index));
        store.delta((index >> 16) as u16, index as u16, &self.coords)
    }

    /// Delta for an F2DOT14 field, as a fraction: 8192 ticks is 0.5.
    pub(crate) fn f2dot14(&self, base: VarIndexBase, field: u16) -> f32 {
        self.raw(base, field) / 16384.0
    }

    /// Delta for a 16.16 Fixed field, as a fraction.
    pub(crate) fn fixed(&self, base: VarIndexBase, field: u16) -> f32 {
        self.raw(base, field) / 65536.0
    }

    /// Offset and alpha deltas (both F2DOT14 fractions) of a
    /// `VarColorStop` whose `varIndexBase` is `base`.
    pub(crate) fn stop(&self, base: VarIndexBase) -> (f32, f32) {
        (self.f2dot14(base, 0), self.f2dot14(base, 1))
    }

    /// A clip box in design units with its deltas applied. HarfBuzz
    /// rounds each delta to a whole unit (half rounds up) before adding
    /// it; the result is `[x_min, y_min, x_max, y_max]`.
    pub(crate) fn clip_box(&self, clip: ClipBox) -> [i32; 4] {
        let fields = [clip.x_min, clip.y_min, clip.x_max, clip.y_max];
        let base = clip.var_index_base.unwrap_or(NO_VARIATION);
        let mut out = [0i32; 4];
        for (i, (slot, v)) in out.iter_mut().zip(fields).enumerate() {
            let delta = (self.raw(base, i as u16) + 0.5).floor();
            *slot = i32::from(v).saturating_add(delta as i32);
        }
        out
    }
}

/// `coords` rounded to F2DOT14 as HarfBuzz stores a font's coords: each
/// a multiple of 1/16384, rounded halves up (`floor(x * 16384 + 0.5)`),
/// with NaN read as zero. Empty when every coordinate rounds to zero,
/// the default instance. The same rule as the core crate's shaping and
/// outline paths.
fn f2dot14_coords(coords: &[f32]) -> Vec<f32> {
    let rounded: Vec<f32> = coords.iter().map(|&c| round_f2dot14(c)).collect();
    if rounded.iter().all(|&c| c == 0.0) {
        return Vec::new();
    }
    rounded
}

/// One coordinate rounded to a multiple of 1/16384, halves up. Written
/// without `f32::floor`, which `core` lacks before Rust 1.85.
fn round_f2dot14(c: f32) -> f32 {
    if c.is_nan() {
        return 0.0;
    }
    let x = c * 16384.0 + 0.5;
    // Every f32 of magnitude 2^23 or more is a whole number already.
    let floor = if (-8_388_608.0..8_388_608.0).contains(&x) {
        let t = x as i32 as f32;
        if t > x {
            t - 1.0
        } else {
            t
        }
    } else {
        x
    };
    floor / 16384.0
}

/// A borrowed `DeltaSetIndexMap` (format 0 or 1):
///
/// ```text
///   u8   format        // 0: u16 mapCount, 1: u32 mapCount
///   u8   entryFormat   // bits 4-5: bytes per entry - 1; bits 0-3: inner bits - 1
///   u16 / u32 mapCount
///   u8[] entries       // mapCount * bytes per entry, big-endian
/// ```
#[derive(Debug, Clone, Copy)]
struct DeltaSetIndexMap<'a> {
    entries: &'a [u8],
    entry_bytes: usize,
    inner_bits: u32,
    map_count: u32,
}

impl<'a> DeltaSetIndexMap<'a> {
    /// Parses the map at absolute offset `start` of `data`. Returns
    /// `None` for an unknown format or a map that does not fit, which
    /// leaves indices unmapped, as in HarfBuzz.
    fn parse(data: &'a [u8], start: usize) -> Option<Self> {
        let header = data.get(start..)?;
        let (&[format, entry_format], rest) = header.split_first_chunk::<2>()?;
        let (map_count, rest) = match format {
            0 => {
                let (count, rest) = rest.split_first_chunk::<2>()?;
                (u32::from(u16::from_be_bytes(*count)), rest)
            }
            1 => {
                let (count, rest) = rest.split_first_chunk::<4>()?;
                (u32::from_be_bytes(*count), rest)
            }
            _ => return None,
        };
        let entry_bytes = usize::from((entry_format >> 4) & 0x03) + 1;
        let inner_bits = u32::from(entry_format & 0x0F) + 1;
        let len = usize::try_from(map_count).ok()?.checked_mul(entry_bytes)?;
        let entries = rest.get(..len)?;
        Some(Self {
            entries,
            entry_bytes,
            inner_bits,
            map_count,
        })
    }

    /// Maps a flat index to the packed `outer << 16 | inner` the
    /// variation store reads.
    fn map(&self, index: u32) -> u32 {
        if self.map_count == 0 {
            return index;
        }
        let last = usize::try_from(index.min(self.map_count - 1)).ok();
        // `parse` sized `entries` for `map_count` entries, so the lookup
        // only fails on a broken invariant. The index then stays unmapped.
        let Some(entry) = last.and_then(|i| {
            let at = i.checked_mul(self.entry_bytes)?;
            self.entries.get(at..at.checked_add(self.entry_bytes)?)
        }) else {
            return index;
        };
        let raw = entry.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b));
        let outer = raw >> self.inner_bits;
        let inner = raw & ((1u32 << self.inner_bits) - 1);
        (outer << 16) | inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Format `format` map with 1-byte entries and 4 inner bits.
    fn map_bytes(format: u8, entries: &[u8]) -> Vec<u8> {
        let mut out = alloc::vec![format, 0x03];
        if format == 0 {
            out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        } else {
            out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        }
        out.extend_from_slice(entries);
        out
    }

    #[test]
    fn coords_round_to_f2dot14_halves_up() {
        // A 16.16 coord `k / 65536` lands on `((k + 2) >> 2) / 16384`, as
        // HarfBuzz rounds it.
        for k in -70_000i32..=70_000 {
            let got = f2dot14_coords(&[k as f32 / 65536.0, 1.0]);
            assert_eq!(got[0], ((k + 2) >> 2) as f32 / 16384.0, "k = {k}");
        }
        assert!(f2dot14_coords(&[1.0 / 65536.0, -2.0 / 65536.0]).is_empty());
        assert!(f2dot14_coords(&[f32::NAN, 0.0]).is_empty());
        assert!(f2dot14_coords(&[]).is_empty());
        assert_eq!(f2dot14_coords(&[f32::NAN, 0.5]), [0.0, 0.5]);
        // Out-of-range and huge values keep their size.
        assert_eq!(f2dot14_coords(&[3.0, -1e30]), [3.0, -1e30]);
        assert_eq!(f2dot14_coords(&[f32::INFINITY]), [f32::INFINITY]);
    }

    #[test]
    fn map_splits_entries_into_outer_and_inner() {
        for format in [0, 1] {
            let bytes = map_bytes(format, &[0x12, 0x3F]);
            let map = DeltaSetIndexMap::parse(&bytes, 0).expect("parses");
            assert_eq!(map.map(0), (1 << 16) | 2);
            assert_eq!(map.map(1), (3 << 16) | 0xF);
            // Past the end: the last entry.
            assert_eq!(map.map(7), (3 << 16) | 0xF);
        }
    }

    #[test]
    fn empty_map_passes_indices_through() {
        let bytes = map_bytes(0, &[]);
        let map = DeltaSetIndexMap::parse(&bytes, 0).expect("parses");
        assert_eq!(map.map(0x0002_0003), 0x0002_0003);
    }

    #[test]
    fn wide_entries_and_unknown_formats() {
        // Two-byte entries, 16 inner bits: 0x0102 -> outer 0, inner 0x102.
        let bytes = [0u8, 0x1F, 0, 1, 0x01, 0x02];
        let map = DeltaSetIndexMap::parse(&bytes, 0).expect("parses");
        assert_eq!(map.map(0), 0x0102);
        assert!(DeltaSetIndexMap::parse(&[2, 0, 0, 0], 0).is_none());
        // Entries that run past the data.
        assert!(DeltaSetIndexMap::parse(&[0, 0, 0, 5, 1], 0).is_none());
        assert!(DeltaSetIndexMap::parse(&[1, 0, 0], 0).is_none());
    }
}
