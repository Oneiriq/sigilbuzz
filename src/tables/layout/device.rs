//! `Device` / `VariationIndex`: the variable-metric sub-offset the
//! spec tacks onto a `ValueRecord` (or an `Anchor`, or an MVAR
//! entry).
//!
//! # Two tables, one offset slot
//!
//! OpenType reuses the same 16-bit sub-offset for two *different*
//! tables. What it points at depends on the `deltaFormat` word that
//! starts the referenced table:
//!
//! ```text
//!   u16 startSize
//!   u16 endSize
//!   u16 deltaFormat          (0x0001 / 0x0002 / 0x0003 = Device,
//!                             0x8000                    = VariationIndex)
//!   ... format-dependent payload ...
//! ```
//!
//! `Device` carries bit-packed per-ppem adjustments used by hinted
//! rasterizers. sigilbuzz does not have a ppem (the shaper emits
//! design-unit advances and lets the renderer scale), so we parse
//! the header but skip the payload.
//!
//! `VariationIndex` (`deltaFormat == 0x8000`) repurposes the
//! `startSize`/`endSize` slots as a 16-bit outer + 16-bit inner
//! index into the enclosing table's `ItemVariationStore`. This is
//! what makes GPOS value records participate in feature-variations
//! and without it, kerning deltas would be frozen at the font's
//! default instance.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// `deltaFormat` sentinel that turns a Device-shaped offset into a
/// `VariationIndex`. Any other value is a plain Device table.
pub const VARIATION_INDEX_DELTA_FORMAT: u16 = 0x8000;

/// A resolved Device-or-VariationIndex reference. The parser normalizes
/// the ambiguous offset slot into this enum so downstream code never
/// has to re-peek at `deltaFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceOrVariationIndex {
    /// Per-ppem hinted adjustment. sigilbuzz parses the header so it
    /// can tell a Device apart from a VariationIndex, but does not
    /// apply the deltas: we run in design units, not device pixels.
    Device {
        /// Smallest ppem this table applies to.
        start_size: u16,
        /// Largest ppem this table applies to.
        end_size: u16,
        /// `deltaFormat` in 1..=3. The payload encoding and the
        /// width of the delta values (2/4/8 bits) follow from this.
        delta_format: u16,
    },
    /// Index into the enclosing table's `ItemVariationStore`. This is
    /// the one feature-variations uses: kerning deltas that scale
    /// with the user's axis coords live here.
    VariationIndex {
        /// Outer index: which `ItemVariationData` subtable to consult.
        outer: u16,
        /// Inner index: which row within that subtable.
        inner: u16,
    },
}

impl DeviceOrVariationIndex {
    /// Parses a Device-shaped table starting at byte zero of `data`.
    /// The header is always three u16s; only the last one decides
    /// which variant we emit.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let first = r.read_u16()?;
        let second = r.read_u16()?;
        let delta_format = r.read_u16()?;
        if delta_format == VARIATION_INDEX_DELTA_FORMAT {
            Ok(Self::VariationIndex {
                outer: first,
                inner: second,
            })
        } else if (1..=3).contains(&delta_format) {
            Ok(Self::Device {
                start_size: first,
                end_size: second,
                delta_format,
            })
        } else {
            Err(Error::Malformed {
                offset: 0,
                context: "Device/VariationIndex deltaFormat out of range",
            })
        }
    }

    /// Parses a Device/VariationIndex pointed at by a 16-bit
    /// sub-offset relative to `base`. Returns `Ok(None)` for the
    /// spec-blessed "absent" sentinel (offset = 0); `Err` for
    /// anything that is shaped like a table but whose header fails
    /// validation.
    pub fn parse_from(base: &[u8], offset: u16) -> Result<Option<Self>> {
        if offset == 0 {
            return Ok(None);
        }
        let start = offset as usize;
        let sub = base.get(start..).ok_or(Error::Malformed {
            offset: start,
            context: "Device/VariationIndex offset past end",
        })?;
        Self::parse(sub).map(Some)
    }

    /// True if this is a `VariationIndex`, the only variant the
    /// shaper actually consumes today. Handy when the caller wants
    /// to short-circuit Device-table resolution.
    #[must_use]
    pub const fn is_variation_index(&self) -> bool {
        matches!(self, Self::VariationIndex { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_device(start: u16, end: u16, delta_format: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&end.to_be_bytes());
        out.extend_from_slice(&delta_format.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn parses_variation_index() {
        let bytes = build_device(5, 42, VARIATION_INDEX_DELTA_FORMAT, &[]);
        let d = DeviceOrVariationIndex::parse(&bytes).unwrap();
        assert_eq!(
            d,
            DeviceOrVariationIndex::VariationIndex {
                outer: 5,
                inner: 42,
            }
        );
        assert!(d.is_variation_index());
    }

    #[test]
    fn parses_device_format_three() {
        let bytes = build_device(8, 16, 3, &[0, 0]);
        let d = DeviceOrVariationIndex::parse(&bytes).unwrap();
        assert_eq!(
            d,
            DeviceOrVariationIndex::Device {
                start_size: 8,
                end_size: 16,
                delta_format: 3,
            }
        );
        assert!(!d.is_variation_index());
    }

    #[test]
    fn rejects_zero_delta_format() {
        // deltaFormat = 0 is reserved by the spec; refuse it rather
        // than silently treating it as a Device with an unknown
        // payload encoding.
        let bytes = build_device(0, 0, 0, &[]);
        assert!(matches!(
            DeviceOrVariationIndex::parse(&bytes),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_reserved_delta_format() {
        // 0x0004..=0x7FFF are unassigned today.
        let bytes = build_device(0, 0, 4, &[]);
        assert!(DeviceOrVariationIndex::parse(&bytes).is_err());
        let bytes = build_device(0, 0, 0x1234, &[]);
        assert!(DeviceOrVariationIndex::parse(&bytes).is_err());
    }

    #[test]
    fn parse_from_treats_zero_offset_as_absent() {
        let out = DeviceOrVariationIndex::parse_from(&[], 0).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn parse_from_follows_nonzero_offset_against_base() {
        // base = [padding, variation_index_table@offset 4]
        let mut base = Vec::new();
        base.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // 4 bytes of pad
        base.extend_from_slice(&build_device(7, 9, VARIATION_INDEX_DELTA_FORMAT, &[]));
        let got = DeviceOrVariationIndex::parse_from(&base, 4)
            .unwrap()
            .unwrap();
        assert_eq!(
            got,
            DeviceOrVariationIndex::VariationIndex { outer: 7, inner: 9 }
        );
    }

    #[test]
    fn parse_from_past_end_errors() {
        let base = [0u8; 2];
        let err = DeviceOrVariationIndex::parse_from(&base, 100);
        assert!(matches!(err, Err(Error::Malformed { .. })));
    }

    #[test]
    fn truncated_header_surfaces_as_error() {
        let bytes = [0u8; 4]; // 2 fewer bytes than the 6-byte header
        assert!(DeviceOrVariationIndex::parse(&bytes).is_err());
    }
}
