//! GPOS lookup type 2: Pair Adjustment.
//!
//! Pair adjustment is the mechanism that delivers kerning. Given two
//! adjacent glyphs `(first, second)`, the lookup returns a pair of
//! [`ValueRecord`]s whose `x_advance` on the first record is what
//! every text renderer adds to `first`'s advance to
//! tighten or loosen the pair.
//!
//! Two subtable formats exist:
//!
//! - **Format 1**: one explicit entry per pair. Fast to look up;
//!   used by small fonts or by fonts that hand-tune unusual pairs.
//! - **Format 2**: class-based. Every glyph is assigned a first
//!   class and a second class via [`ClassDef`] tables, and the
//!   adjustment for a pair is a single lookup into a `class1 *
//!   class2` matrix. Used by every serious Latin font because it
//!   compresses thousands of individual pair rules into a small
//!   grid.

use crate::error::{Error, Result};
use crate::tables::gpos::value_record::ValueRecord;
use crate::tables::layout::{ClassDef, Coverage};
use crate::tables::parse::Reader;

/// A parsed Pair Adjustment subtable, either format 1 or format 2.
#[derive(Debug, Clone)]
pub enum PairPos<'a> {
    /// Explicit per-pair entries.
    Format1(PairPosFormat1<'a>),
    /// Class-based matrix.
    Format2(PairPosFormat2<'a>),
}

impl<'a> PairPos<'a> {
    /// Parses a Pair Adjustment subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        match format {
            1 => PairPosFormat1::parse(data).map(PairPos::Format1),
            2 => PairPosFormat2::parse(data).map(PairPos::Format2),
            _ => Err(Error::Malformed {
                offset: 0,
                context: "unsupported GPOS pairPos format",
            }),
        }
    }

    /// Looks up an adjustment for the pair `(first, second)`.
    ///
    /// Returns the pair of `ValueRecord`s (which the shaper applies
    /// to the two glyphs) or `None` if the pair is not covered. A
    /// pair with all-zero fields is returned as `Some`. The lookup
    /// hit but happened to have no delta, which is still different
    /// from "no rule."
    pub fn lookup(&self, first: u16, second: u16) -> Option<(ValueRecord, ValueRecord)> {
        self.lookup_with_device_base(first, second)
            .map(|(v1, v2, _)| (v1, v2))
    }

    /// Like [`PairPos::lookup`], plus the bytes the two records'
    /// Device / VariationIndex offsets are measured from. The OpenType
    /// spec (and HarfBuzz) root them at the PairSet table for format 1
    /// and at the PairPos subtable for format 2, so a format 1 record
    /// resolved against the subtable would read the wrong table.
    pub fn lookup_with_device_base(
        &self,
        first: u16,
        second: u16,
    ) -> Option<(ValueRecord, ValueRecord, &'a [u8])> {
        match self {
            PairPos::Format1(f) => f.lookup(first, second),
            PairPos::Format2(f) => f.lookup(first, second).map(|(v1, v2)| (v1, v2, f.data)),
        }
    }

    /// True when `first` is in the subtable's coverage, so a pair
    /// starting with it can match. HarfBuzz checks this before it
    /// looks for the second glyph.
    #[must_use]
    pub(crate) fn covers(&self, first: u16) -> bool {
        match self {
            PairPos::Format1(f) => f.coverage.contains(first),
            PairPos::Format2(f) => f.coverage.contains(first),
        }
    }

    /// The subtable's `valueFormat2`. HarfBuzz moves past the second
    /// glyph of a pair it positioned exactly when this is nonzero,
    /// whatever the record's values are.
    #[must_use]
    pub fn value_format2(&self) -> u16 {
        match self {
            PairPos::Format1(f) => f.value_format2,
            PairPos::Format2(f) => f.value_format2,
        }
    }
}

// ---------------------------------------------------------------------------
// Format 1
// ---------------------------------------------------------------------------

/// Pair Adjustment, format 1. Each covered first-glyph is attached
/// to a `PairSet` listing the specific second glyphs and their
/// adjustments.
#[derive(Debug, Clone, Copy)]
pub struct PairPosFormat1<'a> {
    data: &'a [u8],
    coverage: Coverage<'a>,
    value_format1: u16,
    value_format2: u16,
    pair_set_offsets_off: usize,
    pair_set_count: u16,
}

impl<'a> PairPosFormat1<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "pairPos format1 tag mismatch",
            });
        }
        let coverage_off = r.read_u16()? as usize;
        let value_format1 = r.read_u16()?;
        let value_format2 = r.read_u16()?;
        let pair_set_count = r.read_u16()?;
        let pair_set_offsets_off = r.position();

        let need = pair_set_offsets_off + pair_set_count as usize * 2;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: pair_set_offsets_off,
                context: "pairPos format1 pair set offsets shorter than count",
            });
        }

        let coverage_bytes = data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "pairPos format1 coverage offset past end",
        })?;
        let coverage = Coverage::parse(coverage_bytes)?;

        Ok(Self {
            data,
            coverage,
            value_format1,
            value_format2,
            pair_set_offsets_off,
            pair_set_count,
        })
    }

    fn pair_set_offset(&self, i: u16) -> Option<u16> {
        let off = self.pair_set_offsets_off + i as usize * 2;
        Reader::at(self.data, off).ok()?.read_u16().ok()
    }

    /// Returns the pair's records and the PairSet bytes their Device
    /// offsets are measured from.
    fn lookup(&self, first: u16, second: u16) -> Option<(ValueRecord, ValueRecord, &'a [u8])> {
        let cov = self.coverage.index_of(first)?;
        if cov >= self.pair_set_count {
            return None;
        }
        let set_off = self.pair_set_offset(cov)? as usize;
        let set_bytes = self.data.get(set_off..)?;

        let mut r = Reader::new(set_bytes);
        let pair_count = r.read_u16().ok()?;
        let record_size =
            2 + ValueRecord::size(self.value_format1) + ValueRecord::size(self.value_format2);

        // Binary search over the PairSet: secondGlyph ids are sorted.
        let mut lo: u16 = 0;
        let mut hi: u16 = pair_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry_off = 2 + mid as usize * record_size;
            let at = set_bytes.get(entry_off..entry_off + 2)?;
            let candidate = u16::from_be_bytes([at[0], at[1]]);
            match candidate.cmp(&second) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => {
                    let value_off = entry_off + 2;
                    let mut rr = Reader::at(set_bytes, value_off).ok()?;
                    let v1 = ValueRecord::parse(&mut rr, self.value_format1).ok()?;
                    let v2 = ValueRecord::parse(&mut rr, self.value_format2).ok()?;
                    // Device offsets in a PairValueRecord are measured
                    // from the PairSet.
                    return Some((v1, v2, set_bytes));
                }
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Format 2
// ---------------------------------------------------------------------------

/// Pair Adjustment, format 2. A class1 x class2 grid of
/// `ValueRecord` pairs. Every glyph that the coverage table
/// mentions gets its class1 from `classDef1`; the second glyph's
/// class is looked up in `classDef2` unconditionally (coverage
/// governs only the *first* glyph).
#[derive(Debug, Clone, Copy)]
pub struct PairPosFormat2<'a> {
    data: &'a [u8],
    coverage: Coverage<'a>,
    value_format1: u16,
    value_format2: u16,
    class_def1: ClassDef<'a>,
    class_def2: ClassDef<'a>,
    class1_count: u16,
    class2_count: u16,
    class1_records_off: usize,
    record_stride: usize,
}

impl<'a> PairPosFormat2<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let format = r.read_u16()?;
        if format != 2 {
            return Err(Error::Malformed {
                offset: 0,
                context: "pairPos format2 tag mismatch",
            });
        }
        let coverage_off = r.read_u16()? as usize;
        let value_format1 = r.read_u16()?;
        let value_format2 = r.read_u16()?;
        let class_def1_off = r.read_u16()? as usize;
        let class_def2_off = r.read_u16()? as usize;
        let class1_count = r.read_u16()?;
        let class2_count = r.read_u16()?;
        let class1_records_off = r.position();

        let value_record_pair = ValueRecord::size(value_format1) + ValueRecord::size(value_format2);
        // class1Count * class2Count * 32 bytes can overflow a 32-bit
        // usize, so every step is checked.
        let need = usize::from(class1_count)
            .checked_mul(usize::from(class2_count))
            .and_then(|cells| cells.checked_mul(value_record_pair))
            .and_then(|len| class1_records_off.checked_add(len));
        if !need.is_some_and(|need| need <= data.len()) {
            return Err(Error::Truncated {
                offset: class1_records_off,
                context: "pairPos format2 class records shorter than declared",
            });
        }

        let coverage = Coverage::parse(data.get(coverage_off..).ok_or(Error::Malformed {
            offset: coverage_off,
            context: "pairPos format2 coverage offset past end",
        })?)?;
        let class_def1 = ClassDef::parse_at(
            data,
            class_def1_off,
            "pairPos format2 classDef1 offset past end",
        )?;
        let class_def2 = ClassDef::parse_at(
            data,
            class_def2_off,
            "pairPos format2 classDef2 offset past end",
        )?;

        Ok(Self {
            data,
            coverage,
            value_format1,
            value_format2,
            class_def1,
            class_def2,
            class1_count,
            class2_count,
            class1_records_off,
            record_stride: value_record_pair,
        })
    }

    fn lookup(&self, first: u16, second: u16) -> Option<(ValueRecord, ValueRecord)> {
        // Coverage gates whether the lookup fires at all; the class
        // of the first glyph is taken from classDef1 independently.
        self.coverage.index_of(first)?;
        let class1 = self.class_def1.class_of(first);
        let class2 = self.class_def2.class_of(second);
        if class1 >= self.class1_count || class2 >= self.class2_count {
            return None;
        }

        let class1_record_stride = self.class2_count as usize * self.record_stride;
        let entry_off = self.class1_records_off
            + class1 as usize * class1_record_stride
            + class2 as usize * self.record_stride;

        let mut r = Reader::at(self.data, entry_off).ok()?;
        let v1 = ValueRecord::parse(&mut r, self.value_format1).ok()?;
        let v2 = ValueRecord::parse(&mut r, self.value_format2).ok()?;
        Some((v1, v2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::gpos::value_record::{X_ADVANCE, Y_PLACEMENT};
    use alloc::vec::Vec;

    fn build_coverage_format1(glyphs: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
        for g in glyphs {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    fn build_class_def_format1(start: u16, values: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&(values.len() as u16).to_be_bytes());
        for v in values {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// Builds a Pair Adjustment format 1 subtable.
    ///
    /// `pairs` is indexed by first-glyph coverage index. Each inner
    /// slice is (secondGlyph, v1_x_advance, v2_x_advance). The
    /// value format is fixed to just X_ADVANCE for both records to
    /// keep the fixture readable.
    fn build_pair_pos_format1(covered: &[u16], pairs: &[&[(u16, i16, i16)]]) -> Vec<u8> {
        assert_eq!(covered.len(), pairs.len());
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // posFormat
                                                    // coverageOffset placeholder, filled in later
        let cov_off_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&X_ADVANCE.to_be_bytes()); // valueFormat1
        out.extend_from_slice(&X_ADVANCE.to_be_bytes()); // valueFormat2
        out.extend_from_slice(&(pairs.len() as u16).to_be_bytes()); // pairSetCount
        let pair_set_offsets_start = out.len();
        for _ in 0..pairs.len() {
            out.extend_from_slice(&[0u8; 2]);
        }
        // Now write each pair set, patching its offset.
        for (i, entries) in pairs.iter().enumerate() {
            let set_start = out.len();
            out.extend_from_slice(&(entries.len() as u16).to_be_bytes()); // pairValueCount
            for (second, v1, v2) in *entries {
                out.extend_from_slice(&second.to_be_bytes());
                out.extend_from_slice(&v1.to_be_bytes());
                out.extend_from_slice(&v2.to_be_bytes());
            }
            let slot = pair_set_offsets_start + i * 2;
            out[slot..slot + 2].copy_from_slice(&(set_start as u16).to_be_bytes());
        }
        // Finally, coverage table appended and its offset patched in.
        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_off_slot..cov_off_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
        out
    }

    #[test]
    fn format1_returns_adjustment_for_known_pair() {
        // Coverage: first glyphs 10, 20.
        // Pair set 0 (glyph 10): (second=15, v1=-30, v2=0), (second=25, v1=+5, v2=0)
        // Pair set 1 (glyph 20): (second=5, v1=-50, v2=0)
        let bytes =
            build_pair_pos_format1(&[10, 20], &[&[(15, -30, 0), (25, 5, 0)], &[(5, -50, 0)]]);
        let pp = PairPos::parse(&bytes).unwrap();
        let (v1, v2) = pp.lookup(10, 15).unwrap();
        assert_eq!(v1.x_advance, -30);
        assert_eq!(v2.x_advance, 0);
        let (v1b, _) = pp.lookup(10, 25).unwrap();
        assert_eq!(v1b.x_advance, 5);
        let (v1c, _) = pp.lookup(20, 5).unwrap();
        assert_eq!(v1c.x_advance, -50);
    }

    #[test]
    fn device_base_is_the_pair_set_for_format1_and_the_subtable_for_format2() {
        let bytes = build_pair_pos_format1(&[10, 20], &[&[(15, -30, 0)], &[(5, -50, 0)]]);
        let pp = PairPos::parse(&bytes).unwrap();
        // Pair set 1 starts where pairSetOffsets[1] points.
        let set1 = u16::from_be_bytes([bytes[12], bytes[13]]) as usize;
        let (_, _, base) = pp.lookup_with_device_base(20, 5).unwrap();
        assert_eq!(base, &bytes[set1..]);
        assert_eq!(pp.value_format2(), X_ADVANCE);

        let class_def1 = build_class_def_format1(10, &[1]);
        let class_def2 = build_class_def_format1(20, &[1]);
        let bytes2 = build_pair_pos_format2(&[10], &class_def1, &class_def2, &[&[0, 0], &[0, -25]]);
        let pp2 = PairPos::parse(&bytes2).unwrap();
        let (_, _, base2) = pp2.lookup_with_device_base(10, 20).unwrap();
        assert_eq!(base2, &bytes2[..]);
        assert_eq!(pp2.value_format2(), 0);
    }

    #[test]
    fn format1_unknown_first_glyph_returns_none() {
        let bytes = build_pair_pos_format1(&[10], &[&[(15, -30, 0)]]);
        let pp = PairPos::parse(&bytes).unwrap();
        assert!(pp.lookup(11, 15).is_none());
    }

    #[test]
    fn format1_unknown_second_glyph_returns_none() {
        let bytes = build_pair_pos_format1(&[10], &[&[(15, -30, 0)]]);
        let pp = PairPos::parse(&bytes).unwrap();
        assert!(pp.lookup(10, 16).is_none());
    }

    #[test]
    fn format1_device_base_is_the_pair_set() {
        // Device offsets in format 1 are measured from the PairSet,
        // so the base must start at the second PairSet's count word.
        let bytes =
            build_pair_pos_format1(&[10, 20], &[&[(15, -30, 0), (25, 5, 0)], &[(5, -50, 0)]]);
        let pp = PairPos::parse(&bytes).unwrap();
        let (_, _, base) = pp.lookup_with_device_base(20, 5).unwrap();
        let set_off = usize::from(u16::from_be_bytes([bytes[12], bytes[13]]));
        assert_eq!(base, &bytes[set_off..]);
        assert_eq!(&base[..2], &1u16.to_be_bytes(), "pairValueCount of set 1");
    }

    /// Builds a Pair Adjustment format 2 subtable with a single
    /// X_ADVANCE value field on valueFormat1 and no value on
    /// valueFormat2. `matrix[c1][c2]` is the x_advance for the
    /// (class1, class2) cell.
    fn build_pair_pos_format2(
        covered: &[u16],
        class_def1: &[u8],
        class_def2: &[u8],
        matrix: &[&[i16]], // matrix[c1][c2]
    ) -> Vec<u8> {
        let class1_count = matrix.len() as u16;
        let class2_count = matrix.first().map_or(0, |row| row.len()) as u16;

        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // posFormat
        let cov_off_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset
        out.extend_from_slice(&X_ADVANCE.to_be_bytes()); // valueFormat1 = X_ADVANCE only
        out.extend_from_slice(&0u16.to_be_bytes()); // valueFormat2 = nothing
        let cd1_off_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // classDef1Offset
        let cd2_off_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // classDef2Offset
        out.extend_from_slice(&class1_count.to_be_bytes());
        out.extend_from_slice(&class2_count.to_be_bytes());
        for row in matrix {
            for cell in *row {
                out.extend_from_slice(&cell.to_be_bytes());
            }
        }

        let cov_start = out.len();
        out.extend_from_slice(&build_coverage_format1(covered));
        out[cov_off_slot..cov_off_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());

        let cd1_start = out.len();
        out.extend_from_slice(class_def1);
        out[cd1_off_slot..cd1_off_slot + 2].copy_from_slice(&(cd1_start as u16).to_be_bytes());

        let cd2_start = out.len();
        out.extend_from_slice(class_def2);
        out[cd2_off_slot..cd2_off_slot + 2].copy_from_slice(&(cd2_start as u16).to_be_bytes());

        out
    }

    #[test]
    fn format2_class_matrix_lookup_returns_expected_delta() {
        // Coverage: glyphs 10, 11. classDef1: both in class 1.
        // classDef2: glyphs 20..=22 in classes 0, 1, 2.
        // Matrix 2x3 (class1 x class2):
        //   class1=0: [0, 0, 0]          (unused since covered glyphs are class 1)
        //   class1=1: [0, -25, -15]
        let covered = &[10, 11];
        let class_def1 = build_class_def_format1(10, &[1, 1]);
        let class_def2 = build_class_def_format1(20, &[0, 1, 2]);
        let matrix: &[&[i16]] = &[&[0, 0, 0], &[0, -25, -15]];

        let bytes = build_pair_pos_format2(covered, &class_def1, &class_def2, matrix);
        let pp = PairPos::parse(&bytes).unwrap();

        // Pair (10, 21) -> class1=1, class2=1 -> -25.
        let (v1, v2) = pp.lookup(10, 21).unwrap();
        assert_eq!(v1.x_advance, -25);
        assert_eq!(v2, ValueRecord::default());
        // Pair (11, 22) -> class1=1, class2=2 -> -15.
        let (v1b, _) = pp.lookup(11, 22).unwrap();
        assert_eq!(v1b.x_advance, -15);
        // Format 2 measures Device offsets from the subtable itself.
        let (_, _, base) = pp.lookup_with_device_base(11, 22).unwrap();
        assert_eq!(base, &bytes[..]);
    }

    #[test]
    fn format2_uncovered_first_glyph_returns_none() {
        let covered = &[10];
        let class_def1 = build_class_def_format1(10, &[1]);
        let class_def2 = build_class_def_format1(20, &[0]);
        let matrix: &[&[i16]] = &[&[0], &[5]];

        let bytes = build_pair_pos_format2(covered, &class_def1, &class_def2, matrix);
        let pp = PairPos::parse(&bytes).unwrap();
        assert!(pp.lookup(99, 20).is_none());
    }

    #[test]
    fn format2_covered_first_but_unknown_class_returns_out_of_bounds_none() {
        // Coverage includes glyph 10 but classDef1 does not list it,
        // so its class defaults to 0. With class1_count = 1, class
        // 0 is valid, so this actually hits the matrix's class1=0
        // row. We test a different scenario: second glyph class
        // exceeds class2_count.
        let covered = &[10];
        let class_def1 = build_class_def_format1(10, &[0]);
        let class_def2 = build_class_def_format1(20, &[5]); // class 5 but class2_count=1
        let matrix: &[&[i16]] = &[&[0]];

        let bytes = build_pair_pos_format2(covered, &class_def1, &class_def2, matrix);
        let pp = PairPos::parse(&bytes).unwrap();
        assert!(pp.lookup(10, 20).is_none());
    }

    #[test]
    fn rejects_unknown_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        assert!(matches!(
            PairPos::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn value_record_size_used_correctly_for_larger_formats() {
        // valueFormat = X_ADVANCE | Y_PLACEMENT => 4 bytes per record.
        let covered = &[1];
        // Minimal: one pair set with one pair; each ValueRecord is 4 bytes.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u16.to_be_bytes()); // format
        let cov_slot = bytes.len();
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&(X_ADVANCE | Y_PLACEMENT).to_be_bytes()); // vf1
        bytes.extend_from_slice(&0u16.to_be_bytes()); // vf2 = empty
        bytes.extend_from_slice(&1u16.to_be_bytes()); // pairSetCount
        let set_off_slot = bytes.len();
        bytes.extend_from_slice(&0u16.to_be_bytes());
        let set_start = bytes.len();
        bytes.extend_from_slice(&1u16.to_be_bytes()); // pairCount
        bytes.extend_from_slice(&2u16.to_be_bytes()); // secondGlyph
                                                      // ValueRecord1: Y_PLACEMENT (bit 2) written before X_ADVANCE (bit 4)
                                                      // per the field-order rule.
        bytes.extend_from_slice(&7i16.to_be_bytes()); // y_placement
        bytes.extend_from_slice(&(-3i16).to_be_bytes()); // x_advance
                                                         // No ValueRecord2.
        bytes[set_off_slot..set_off_slot + 2].copy_from_slice(&(set_start as u16).to_be_bytes());
        let cov_start = bytes.len();
        bytes.extend_from_slice(&build_coverage_format1(covered));
        bytes[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());

        let pp = PairPos::parse(&bytes).unwrap();
        let (v1, _) = pp.lookup(1, 2).unwrap();
        assert_eq!(v1.y_placement, 7);
        assert_eq!(v1.x_advance, -3);
    }
}
