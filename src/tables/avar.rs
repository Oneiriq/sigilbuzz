//! `avar`: Axis Variations table.
//!
//! Remaps normalized axis coordinates through piecewise-linear
//! segments, after `fvar` normalization and before the variation
//! store consumes them. Font designers use `avar` to correct
//! non-linear interpolation: for example, a weight axis whose
//! mid-point should render not as the arithmetic mean of Regular
//! and Black but closer to Semibold.
//!
//! # Layout
//!
//! ```text
//!   u16      majorVersion = 1
//!   u16      minorVersion = 0
//!   u16      (reserved)
//!   u16      axisCount
//!   SegmentMaps  maps[axisCount]
//! ```
//!
//! Each `SegmentMaps`:
//!
//! ```text
//!   u16       positionMapCount
//!   AxisValueMap  positionMaps[positionMapCount]:
//!     F2DOT14 fromCoordinate
//!     F2DOT14 toCoordinate
//! ```
//!
//! The maps are always strictly increasing in `fromCoordinate`.
//! Interpolation is linear between adjacent entries; inputs
//! outside the outermost entries move by the nearest entry's shift,
//! as in HarfBuzz.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::{abs_f32, hb_round_to, Reader};

/// A parsed `avar` table.
#[derive(Debug, Clone)]
pub struct Avar {
    segment_maps: Vec<Vec<(f32, f32)>>,
}

impl Avar {
    /// Parses an `avar` table.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported avar major version",
            });
        }
        let _reserved = r.read_u16()?;
        let axis_count = r.read_u16()? as usize;

        // Capacities are capped by the bytes left, since each
        // SegmentMaps takes at least 2 bytes and each AxisValueMap 4.
        let mut segment_maps = Vec::with_capacity(axis_count.min(r.remaining() / 2));
        for _ in 0..axis_count {
            let count = r.read_u16()? as usize;
            let mut map = Vec::with_capacity(count.min(r.remaining() / 4));
            for _ in 0..count {
                let from = r.read_f2dot14()?;
                let to = r.read_f2dot14()?;
                map.push((from, to));
            }
            segment_maps.push(map);
        }

        Ok(Self { segment_maps })
    }

    /// Number of axes covered by the avar table. Must match the
    /// corresponding fvar axis count in a valid font.
    #[must_use]
    pub fn axis_count(&self) -> usize {
        self.segment_maps.len()
    }

    /// Applies the segment map for `axis_index` to a normalized
    /// coordinate, as HarfBuzz's `SegmentMaps::map_float` does. The
    /// result is not rounded; see [`Avar::remap_all`].
    ///
    /// Between two maps the coordinate is interpolated linearly. Past
    /// the first or last map it moves by that map's shift
    /// (`to - from`) instead of clamping. HarfBuzz's recovery for maps
    /// the OpenType spec does not allow applies too: no map leaves the
    /// coordinate unchanged, a single map shifts it, a duplicated
    /// `-1 -> -1` or `+1 -> +1` end map is dropped, and several maps
    /// from the same coordinate resolve the way CoreText resolves them.
    /// An axis past the table's last segment map is left unchanged.
    #[must_use]
    pub fn remap(&self, axis_index: usize, coord: f32) -> f32 {
        match self.segment_maps.get(axis_index) {
            Some(map) => map_float(map, coord),
            None => coord,
        }
    }

    /// Remaps every coordinate through its corresponding segment
    /// map, the way HarfBuzz's `hb_ot_var_normalize_coords` does: each
    /// coordinate is rounded to 16.16 fixed point (a multiple of
    /// 1/65536, halves up), mapped with [`Avar::remap`], and rounded to
    /// 16.16 again. Coordinates past the table's axes pass through
    /// unchanged. Output length matches `coords.len()`.
    ///
    /// Shaping rounds the coordinates once more, to F2DOT14, so a
    /// coordinate from [`crate::tables::Fvar::normalize_coords`] and
    /// this method lands on the F2DOT14 value HarfBuzz uses.
    #[must_use]
    pub fn remap_all(&self, coords: &[f32]) -> Vec<f32> {
        coords
            .iter()
            .enumerate()
            .map(|(i, &c)| match self.segment_maps.get(i) {
                Some(map) => {
                    let fixed = hb_round_to(c, 65536.0);
                    hb_round_to(map_float(map, fixed), 65536.0)
                }
                None => c,
            })
            .collect()
    }
}

/// HarfBuzz's `SegmentMaps::map_float` (hb-ot-var-avar-table.hh) over one
/// axis's `(from, to)` maps, in `f32` with the same operations in the
/// same order.
fn map_float(map: &[(f32, f32)], value: f32) -> f32 {
    let (Some(&first), Some(&last)) = (map.first(), map.last()) else {
        return value;
    };
    if map.len() < 2 {
        return value - first.0 + first.1;
    }
    // At least two maps. Drop a duplicated end map that HarfBuzz
    // drops: a `-1 -> -1` followed by another map from -1, or a
    // `+1 -> +1` preceded by another map from +1.
    let mut start = 0;
    let mut end = map.len();
    if first == (-1.0, -1.0) && map[1].0 == -1.0 {
        start += 1;
    }
    if last == (1.0, 1.0) && map[end - 2].0 == 1.0 {
        end -= 1;
    }
    // Never empty: dropping both ends would need map[1] to be from -1
    // and map[len - 2] from +1, which cannot both hold for two or three
    // maps, and longer lists keep their middle.
    let Some(maps) = map.get(start..end).filter(|m| !m.is_empty()) else {
        return value;
    };

    // An exact match, and the special cases HarfBuzz gives several
    // maps from the same coordinate.
    if let Some(i) = maps.iter().position(|m| m.0 == value) {
        let j = i + maps[i..].iter().take_while(|m| m.0 == value).count() - 1;
        if i == j {
            return maps[i].1;
        }
        if i + 2 == j {
            return maps[i + 1].1;
        }
        // Return the one mapping closer to 0.
        if value < 0.0 {
            return maps[j].1;
        }
        if value > 0.0 {
            return maps[i].1;
        }
        // Mapping 0: the smaller of the two, as CoreText seems to.
        return if abs_f32(maps[i].1) < abs_f32(maps[j].1) {
            maps[i].1
        } else {
            maps[j].1
        };
    }

    // Not an exact match: find the segment and interpolate, or shift
    // past either end.
    let i = maps.iter().position(|m| value < m.0).unwrap_or(maps.len());
    if i == 0 {
        return value - maps[0].0 + maps[0].1;
    }
    let before = maps[i - 1];
    let Some(&after) = maps.get(i) else {
        return value - before.0 + before.1;
    };
    // `before.0 < value < after.0`, so the span is not zero.
    let denom = after.0 - before.0;
    before.1 + ((after.1 - before.1) * (value - before.0)) / denom
}
#[cfg(test)]
mod tests {
    use super::*;

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    fn build_avar(maps: &[&[(f32, f32)]]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(maps.len() as u16).to_be_bytes());
        for map in maps {
            out.extend_from_slice(&(map.len() as u16).to_be_bytes());
            for (from, to) in *map {
                write_f2dot14(&mut out, *from);
                write_f2dot14(&mut out, *to);
            }
        }
        out
    }

    #[test]
    fn identity_map_passes_through() {
        let map = &[(-1.0, -1.0), (0.0, 0.0), (1.0, 1.0)];
        let bytes = build_avar(&[map]);
        let avar = Avar::parse(&bytes).unwrap();
        assert!((avar.remap(0, 0.0) - 0.0).abs() < 1e-3);
        assert!((avar.remap(0, 0.5) - 0.5).abs() < 1e-3);
        assert!((avar.remap(0, 1.0) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn non_identity_map_interpolates_linearly() {
        // 0.5 user -> 0.75 normalized (weight bias toward heavy).
        let map = &[(-1.0, -1.0), (0.0, 0.0), (0.5, 0.75), (1.0, 1.0)];
        let bytes = build_avar(&[map]);
        let avar = Avar::parse(&bytes).unwrap();
        assert!((avar.remap(0, 0.25) - 0.375).abs() < 1e-3);
        assert!((avar.remap(0, 0.5) - 0.75).abs() < 1e-3);
        assert!((avar.remap(0, 0.75) - 0.875).abs() < 1e-3);
    }

    #[test]
    fn out_of_range_shifts_by_the_nearest_map() {
        // HarfBuzz moves a coordinate past the outermost maps by their
        // shift (`to - from`) rather than clamping it.
        let map = &[(-1.0, -0.5), (1.0, 0.5)];
        let bytes = build_avar(&[map]);
        let avar = Avar::parse(&bytes).unwrap();
        assert_eq!(avar.remap(0, -2.0), -1.5);
        assert_eq!(avar.remap(0, 2.0), 1.5);
        assert_eq!(avar.remap(0, -1.0), -0.5);
        assert_eq!(avar.remap(0, 0.0), 0.0);
    }

    #[test]
    fn short_maps_leave_or_shift_the_coordinate() {
        let bytes = build_avar(&[&[], &[(0.25, 0.5)]]);
        let avar = Avar::parse(&bytes).unwrap();
        assert_eq!(avar.remap(0, 0.3), 0.3);
        // One map: the coordinate moves by its shift.
        assert_eq!(avar.remap(1, 0.5), 0.75);
        assert_eq!(avar.remap(1, -1.0), -0.75);
    }

    #[test]
    fn duplicated_end_maps_and_repeated_sources() {
        // A `-1 -> -1` map followed by another map from -1 is dropped,
        // so -1 maps through the second one.
        let bytes = build_avar(&[&[(-1.0, -1.0), (-1.0, -0.5), (0.0, 0.0), (1.0, 1.0)]]);
        let avar = Avar::parse(&bytes).unwrap();
        assert_eq!(avar.remap(0, -1.0), -0.5);
        // Two maps from 0.5: HarfBuzz takes the one closer to zero
        // for a positive coordinate (the first), and three take the
        // middle one.
        let two = build_avar(&[&[(-1.0, -1.0), (0.5, 0.25), (0.5, 0.75), (1.0, 1.0)]]);
        assert_eq!(Avar::parse(&two).unwrap().remap(0, 0.5), 0.25);
        let three = build_avar(&[&[(0.5, 0.25), (0.5, 0.5), (0.5, 0.75)]]);
        assert_eq!(Avar::parse(&three).unwrap().remap(0, 0.5), 0.5);
        // At zero, the smaller of the two.
        let zero = build_avar(&[&[(-1.0, -1.0), (0.0, 0.25), (0.0, -0.125), (1.0, 1.0)]]);
        assert_eq!(Avar::parse(&zero).unwrap().remap(0, 0.0), -0.125);
    }

    #[test]
    fn remap_all_rounds_to_16_16_like_harfbuzz() {
        // 0 -> 0, 1 -> 1, so 0.3 maps to itself, then rounds to
        // 19661 / 65536 (0.3 * 65536 = 19660.8).
        let map = &[(-1.0, -1.0), (0.0, 0.0), (1.0, 1.0)];
        let bytes = build_avar(&[map]);
        let avar = Avar::parse(&bytes).unwrap();
        assert_eq!(avar.remap_all(&[0.3]), [19661.0 / 65536.0]);
        // A coordinate past the table's axes passes through.
        assert_eq!(avar.remap_all(&[0.5, 0.3]), [0.5, 0.3]);
        // 0.1 rounds to 6554 / 65536 first, which the 0 -> 0, 0.5 ->
        // 0.75 segment maps to 9831 / 65536.
        let bias = build_avar(&[&[(-1.0, -1.0), (0.0, 0.0), (0.5, 0.75), (1.0, 1.0)]]);
        let avar = Avar::parse(&bias).unwrap();
        assert_eq!(avar.remap_all(&[0.1]), [9831.0 / 65536.0]);
    }

    #[test]
    fn unknown_axis_index_returns_input_unchanged() {
        let bytes = build_avar(&[&[]]);
        let avar = Avar::parse(&bytes).unwrap();
        assert!((avar.remap(9, 0.42) - 0.42).abs() < 1e-6);
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let mut bytes = build_avar(&[]);
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Avar::parse(&bytes), Err(Error::Malformed { .. })));
    }
}
