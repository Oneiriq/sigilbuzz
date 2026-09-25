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
//! outside the outermost entries clamp.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

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
    /// coordinate. Returns the coord unchanged when the axis has
    /// no segment map (empty or missing).
    #[must_use]
    pub fn remap(&self, axis_index: usize, coord: f32) -> f32 {
        let Some(map) = self.segment_maps.get(axis_index) else {
            return coord;
        };
        if map.len() < 2 {
            return coord;
        }
        if coord <= map[0].0 {
            return map[0].1;
        }
        if coord >= map[map.len() - 1].0 {
            return map[map.len() - 1].1;
        }
        for window in map.windows(2) {
            let (a_from, a_to) = window[0];
            let (b_from, b_to) = window[1];
            if coord >= a_from && coord <= b_from {
                let span = b_from - a_from;
                if span == 0.0 {
                    return a_to;
                }
                let t = (coord - a_from) / span;
                return a_to + t * (b_to - a_to);
            }
        }
        coord
    }

    /// Remaps every coordinate through its corresponding segment
    /// map. Output length matches `coords.len()`.
    #[must_use]
    pub fn remap_all(&self, coords: &[f32]) -> Vec<f32> {
        coords
            .iter()
            .enumerate()
            .map(|(i, c)| self.remap(i, *c))
            .collect()
    }
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
    fn out_of_range_clamps_to_endpoints() {
        let map = &[(-1.0, -0.5), (1.0, 0.5)];
        let bytes = build_avar(&[map]);
        let avar = Avar::parse(&bytes).unwrap();
        assert!((avar.remap(0, -2.0) - (-0.5)).abs() < 1e-3);
        assert!((avar.remap(0, 2.0) - 0.5).abs() < 1e-3);
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
