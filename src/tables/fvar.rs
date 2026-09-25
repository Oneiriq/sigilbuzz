//! `fvar`: Font Variations table.
//!
//! Describes the variation axes a font exposes (weight, width,
//! optical size, custom axes). Each axis carries `(min, default,
//! max)` in user-facing design-space units, plus a human-readable
//! name via the `name` table. Optional *named instances* pin every
//! axis to a specific coordinate: Regular, Bold, Condensed, ...
//!
//! sigilbuzz uses `fvar` to:
//!
//! 1. Decide whether a font is variable at all (presence of `fvar`).
//! 2. Normalize user design coordinates into the `[-1.0, 1.0]`
//!    range the ItemVariationStore consumes.
//!
//! # Layout
//!
//! ```text
//!   u16      majorVersion = 1
//!   u16      minorVersion = 0
//!   u16      axesArrayOffset    (from start of fvar)
//!   u16      (reserved)
//!   u16      axisCount
//!   u16      axisSize            always 20
//!   u16      instanceCount
//!   u16      instanceSize        20 + 4 * axisCount  (+ 2 for ps_name variant)
//! ```
//!
//! Axis record (20 bytes):
//!
//! ```text
//!   Tag         tag
//!   F16DOT16    minValue
//!   F16DOT16    defaultValue
//!   F16DOT16    maxValue
//!   u16         flags
//!   u16         axisNameID
//! ```
//!
//! Instance record (instanceSize bytes):
//!
//! ```text
//!   u16       subfamilyNameID
//!   u16       flags
//!   F16DOT16  coordinates[axisCount]
//!   (optional) u16 postScriptNameID
//! ```

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// One variation axis. Coordinates are in user design space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VariationAxis {
    /// Four-byte axis tag (e.g. `b"wght"`, `b"wdth"`, `b"opsz"`).
    pub tag: [u8; 4],
    /// Minimum user-space coordinate.
    pub min_value: f32,
    /// Default user-space coordinate.
    pub default_value: f32,
    /// Maximum user-space coordinate.
    pub max_value: f32,
    /// Flags bitfield; bit 0 = hidden axis.
    pub flags: u16,
    /// `name` table nameID carrying the axis's UI label.
    pub axis_name_id: u16,
}

impl VariationAxis {
    /// Normalizes a user-space coordinate into `[-1.0, 1.0]` per
    /// the OpenType spec. Values outside `[min, max]` clamp to
    /// `-1.0` / `1.0` respectively. The default value maps to
    /// `0.0`, with a piecewise-linear ramp to either endpoint.
    ///
    /// Malformed fvar inputs (`min > max`, a `NaN` bound, or a
    /// `NaN` user value) return `0.0` (the default instance) rather
    /// than panicking; `f32::clamp` has a documented panic contract
    /// on inverted or non-finite bounds that would otherwise bubble
    /// up into the shape pipeline.
    #[must_use]
    pub fn normalize(&self, user: f32) -> f32 {
        // Refuse to run the comparison pipeline on any non-finite
        // bound or inverted range: `f32::clamp` panics in those
        // cases, and the rest of the function would divide by NaN.
        if !self.min_value.is_finite()
            || !self.default_value.is_finite()
            || !self.max_value.is_finite()
            || self.min_value > self.max_value
            || user.is_nan()
        {
            return 0.0;
        }
        let clamped = user.clamp(self.min_value, self.max_value);
        if clamped < self.default_value {
            let denom = self.default_value - self.min_value;
            if denom == 0.0 {
                return 0.0;
            }
            (clamped - self.default_value) / denom
        } else if clamped > self.default_value {
            let denom = self.max_value - self.default_value;
            if denom == 0.0 {
                return 0.0;
            }
            (clamped - self.default_value) / denom
        } else {
            0.0
        }
    }
}

/// A parsed `fvar` table.
#[derive(Debug, Clone)]
pub struct Fvar {
    axes: Vec<VariationAxis>,
}

impl Fvar {
    /// Parses an `fvar` table. Named instances are skipped: the
    /// shaper only needs axes today.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported fvar major version",
            });
        }
        let axes_array_off = r.read_u16()? as usize;
        let _reserved = r.read_u16()?;
        let axis_count = r.read_u16()? as usize;
        let axis_size = r.read_u16()? as usize;
        let _instance_count = r.read_u16()?;
        let _instance_size = r.read_u16()?;

        if axis_size < 20 {
            return Err(Error::Malformed {
                offset: 8,
                context: "fvar axisSize smaller than 20",
            });
        }
        let need = axes_array_off + axis_count * axis_size;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: axes_array_off,
                context: "fvar axes array truncated",
            });
        }

        let mut axes = Vec::with_capacity(axis_count);
        for i in 0..axis_count {
            let base = axes_array_off + i * axis_size;
            let mut ar = Reader::at(data, base)?;
            let tag = ar.read_tag()?;
            let min_value = ar.read_f16dot16()?;
            let default_value = ar.read_f16dot16()?;
            let max_value = ar.read_f16dot16()?;
            let flags = ar.read_u16()?;
            let axis_name_id = ar.read_u16()?;
            axes.push(VariationAxis {
                tag,
                min_value,
                default_value,
                max_value,
                flags,
                axis_name_id,
            });
        }

        Ok(Self { axes })
    }

    /// Variation axes in file order.
    #[must_use]
    pub fn axes(&self) -> &[VariationAxis] {
        &self.axes
    }

    /// Finds an axis by tag.
    #[must_use]
    pub fn axis_by_tag(&self, tag: [u8; 4]) -> Option<&VariationAxis> {
        self.axes.iter().find(|a| a.tag == tag)
    }

    /// Returns the index of an axis by tag.
    #[must_use]
    pub fn axis_index(&self, tag: [u8; 4]) -> Option<usize> {
        self.axes.iter().position(|a| a.tag == tag)
    }

    /// Converts a full user-space coordinate vector into the
    /// normalized `[-1.0, 1.0]` vector the variation store wants.
    /// `user_coords` must be indexed in the same order as
    /// [`Fvar::axes`]; missing entries default to the axis default.
    #[must_use]
    pub fn normalize_coords(&self, user_coords: &[f32]) -> Vec<f32> {
        self.axes
            .iter()
            .enumerate()
            .map(|(i, axis)| {
                let user = user_coords.get(i).copied().unwrap_or(axis.default_value);
                axis.normalize(user)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_f16dot16(out: &mut Vec<u8>, v: f32) {
        let raw = (v * 65536.0).round() as i32;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    fn build_fvar(axes: &[VariationAxis]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&16u16.to_be_bytes()); // axesArrayOffset
        out.extend_from_slice(&2u16.to_be_bytes()); // reserved
        out.extend_from_slice(&(axes.len() as u16).to_be_bytes());
        out.extend_from_slice(&20u16.to_be_bytes()); // axisSize
        out.extend_from_slice(&0u16.to_be_bytes()); // instanceCount
        out.extend_from_slice(&0u16.to_be_bytes()); // instanceSize
        for ax in axes {
            out.extend_from_slice(&ax.tag);
            write_f16dot16(&mut out, ax.min_value);
            write_f16dot16(&mut out, ax.default_value);
            write_f16dot16(&mut out, ax.max_value);
            out.extend_from_slice(&ax.flags.to_be_bytes());
            out.extend_from_slice(&ax.axis_name_id.to_be_bytes());
        }
        out
    }

    #[test]
    fn parses_weight_and_width_axes() {
        let axes = [
            VariationAxis {
                tag: *b"wght",
                min_value: 100.0,
                default_value: 400.0,
                max_value: 900.0,
                flags: 0,
                axis_name_id: 256,
            },
            VariationAxis {
                tag: *b"wdth",
                min_value: 50.0,
                default_value: 100.0,
                max_value: 200.0,
                flags: 0,
                axis_name_id: 257,
            },
        ];
        let bytes = build_fvar(&axes);
        let fvar = Fvar::parse(&bytes).unwrap();
        assert_eq!(fvar.axes().len(), 2);
        assert_eq!(fvar.axes()[0].tag, *b"wght");
        assert_eq!(fvar.axes()[1].tag, *b"wdth");
        assert!((fvar.axes()[0].default_value - 400.0).abs() < 1e-3);
    }

    #[test]
    fn normalizes_in_both_directions() {
        let axis = VariationAxis {
            tag: *b"wght",
            min_value: 100.0,
            default_value: 400.0,
            max_value: 900.0,
            flags: 0,
            axis_name_id: 0,
        };
        assert!((axis.normalize(400.0) - 0.0).abs() < 1e-6);
        assert!((axis.normalize(900.0) - 1.0).abs() < 1e-6);
        assert!((axis.normalize(100.0) + 1.0).abs() < 1e-6);
        // Halfway between default and max -> +0.5.
        assert!((axis.normalize(650.0) - 0.5).abs() < 1e-6);
        // Out-of-range clamps, not extrapolates.
        assert!((axis.normalize(2000.0) - 1.0).abs() < 1e-6);
        assert!((axis.normalize(-500.0) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_coords_fills_defaults_for_missing_axes() {
        let axes = [
            VariationAxis {
                tag: *b"wght",
                min_value: 100.0,
                default_value: 400.0,
                max_value: 900.0,
                flags: 0,
                axis_name_id: 0,
            },
            VariationAxis {
                tag: *b"wdth",
                min_value: 50.0,
                default_value: 100.0,
                max_value: 200.0,
                flags: 0,
                axis_name_id: 0,
            },
        ];
        let bytes = build_fvar(&axes);
        let fvar = Fvar::parse(&bytes).unwrap();
        let n = fvar.normalize_coords(&[700.0]); // wght only, wdth defaults
        assert!((n[0] - 0.6).abs() < 1e-3); // (700-400)/(900-400)
        assert!((n[1] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let mut bytes = build_fvar(&[]);
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Fvar::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn normalize_with_inverted_range_returns_zero_not_panic() {
        // A malformed axis with `min_value > max_value` would make
        // `f32::clamp` panic with its documented "min > max" contract.
        // The fvar parser does not validate axis ranges, so a user who
        // loads such a font would crash the shape pipeline on the
        // first call to `normalize_coords`. Ensure we fall back to 0
        // (the default instance) instead.
        let axis = VariationAxis {
            tag: *b"wght",
            min_value: 900.0,
            default_value: 400.0,
            max_value: 100.0,
            flags: 0,
            axis_name_id: 0,
        };
        assert!((axis.normalize(500.0) - 0.0).abs() < f32::EPSILON);
        assert!((axis.normalize(1000.0) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn normalize_with_nan_bounds_returns_zero_not_panic() {
        let axis = VariationAxis {
            tag: *b"wght",
            min_value: f32::NAN,
            default_value: 400.0,
            max_value: 900.0,
            flags: 0,
            axis_name_id: 0,
        };
        assert!((axis.normalize(500.0) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn normalize_with_nan_user_value_returns_zero() {
        let axis = VariationAxis {
            tag: *b"wght",
            min_value: 100.0,
            default_value: 400.0,
            max_value: 900.0,
            flags: 0,
            axis_name_id: 0,
        };
        assert!((axis.normalize(f32::NAN) - 0.0).abs() < f32::EPSILON);
    }
}
