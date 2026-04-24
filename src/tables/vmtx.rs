//! `vmtx` — vertical metrics.
//!
//! Symmetric mirror of `hmtx`: per-glyph `advanceHeight` + top-side
//! bearing, with the same "repeat the last long metric" trick past
//! the `numberOfLongVerMetrics` boundary. Vertical CJK layout picks
//! glyph Y advances from this table while the X advance is forced to
//! zero (glyphs are stacked, not walked).
//!
//! Layout:
//!
//! ```text
//!   longVerMetric[number_of_long_ver_metrics] {
//!       u16 advanceHeight
//!       i16 topSideBearing
//!   }
//!   i16  topSideBearing[num_glyphs - number_of_long_ver_metrics]
//! ```

use crate::error::{Error, Result};

/// Parsed vertical metrics for a font.
#[derive(Debug, Clone, Copy)]
pub struct Vmtx<'a> {
    /// Fixed-size long-metric block: `number_of_long_ver_metrics`
    /// entries of four bytes each (u16 advance + i16 tsb).
    long_metrics: &'a [u8],
    /// Trailing TSB-only block: two bytes per glyph past the long
    /// boundary. May be empty when every glyph has its own advance.
    trailing_tsbs: &'a [u8],
    /// Count of full long-metric entries.
    number_of_long_ver_metrics: u16,
    /// Total glyph count from `maxp`.
    num_glyphs: u16,
    /// Cached last advance so the "tail glyph" lookup is O(1).
    last_advance: u16,
}

impl<'a> Vmtx<'a> {
    /// Parses the table layout given the glyph count from `maxp` and
    /// the long-metric count from `vhea`.
    pub fn parse(data: &'a [u8], num_glyphs: u16, number_of_long_ver_metrics: u16) -> Result<Self> {
        if number_of_long_ver_metrics == 0 {
            return Err(Error::Malformed {
                offset: 0,
                context: "vmtx requires at least one long metric",
            });
        }
        if number_of_long_ver_metrics > num_glyphs {
            return Err(Error::Malformed {
                offset: 0,
                context: "numberOfLongVerMetrics exceeds numGlyphs",
            });
        }

        let long_bytes = number_of_long_ver_metrics as usize * 4;
        let trailing_count = (num_glyphs - number_of_long_ver_metrics) as usize;
        let trailing_bytes = trailing_count * 2;
        let required = long_bytes + trailing_bytes;
        if data.len() < required {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "vmtx body shorter than declared",
            });
        }

        let long_metrics = &data[..long_bytes];
        let trailing_tsbs = &data[long_bytes..long_bytes + trailing_bytes];

        let last_idx = (number_of_long_ver_metrics as usize - 1) * 4;
        let last_advance = u16::from_be_bytes([long_metrics[last_idx], long_metrics[last_idx + 1]]);

        Ok(Self {
            long_metrics,
            trailing_tsbs,
            number_of_long_ver_metrics,
            num_glyphs,
            last_advance,
        })
    }

    /// Total glyph count this table covers.
    #[must_use]
    pub const fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    /// Vertical advance height for `glyph`, in font design units.
    /// Returns `None` if the glyph id is out of range.
    #[must_use]
    pub fn advance(&self, glyph: u16) -> Option<u16> {
        if glyph >= self.num_glyphs {
            return None;
        }
        if glyph < self.number_of_long_ver_metrics {
            let off = glyph as usize * 4;
            Some(u16::from_be_bytes([
                self.long_metrics[off],
                self.long_metrics[off + 1],
            ]))
        } else {
            Some(self.last_advance)
        }
    }

    /// Top side bearing for `glyph`.
    #[must_use]
    pub fn tsb(&self, glyph: u16) -> Option<i16> {
        if glyph >= self.num_glyphs {
            return None;
        }
        if glyph < self.number_of_long_ver_metrics {
            let off = glyph as usize * 4 + 2;
            Some(i16::from_be_bytes([
                self.long_metrics[off],
                self.long_metrics[off + 1],
            ]))
        } else {
            let off = (glyph - self.number_of_long_ver_metrics) as usize * 2;
            Some(i16::from_be_bytes([
                self.trailing_tsbs[off],
                self.trailing_tsbs[off + 1],
            ]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_vmtx(longs: &[(u16, i16)], trailing_tsbs: &[i16]) -> Vec<u8> {
        let mut b = Vec::new();
        for (adv, tsb) in longs {
            b.extend_from_slice(&adv.to_be_bytes());
            b.extend_from_slice(&tsb.to_be_bytes());
        }
        for tsb in trailing_tsbs {
            b.extend_from_slice(&tsb.to_be_bytes());
        }
        b
    }

    #[test]
    fn every_glyph_has_its_own_metric() {
        let bytes = build_vmtx(&[(1000, 50), (1100, 60), (1200, 70)], &[]);
        let vmtx = Vmtx::parse(&bytes, 3, 3).unwrap();
        assert_eq!(vmtx.advance(0), Some(1000));
        assert_eq!(vmtx.advance(2), Some(1200));
        assert_eq!(vmtx.tsb(2), Some(70));
    }

    #[test]
    fn trailing_glyphs_share_the_last_advance() {
        let bytes = build_vmtx(&[(1000, 0), (1100, 0)], &[10, 20, 30]);
        let vmtx = Vmtx::parse(&bytes, 5, 2).unwrap();
        assert_eq!(vmtx.advance(1), Some(1100));
        assert_eq!(vmtx.advance(4), Some(1100));
        assert_eq!(vmtx.tsb(2), Some(10));
        assert_eq!(vmtx.tsb(4), Some(30));
    }

    #[test]
    fn out_of_range_glyph_returns_none() {
        let bytes = build_vmtx(&[(1000, 0)], &[]);
        let vmtx = Vmtx::parse(&bytes, 1, 1).unwrap();
        assert_eq!(vmtx.advance(1), None);
        assert_eq!(vmtx.tsb(7), None);
    }

    #[test]
    fn rejects_zero_long_metrics() {
        let bytes = Vec::new();
        assert!(matches!(
            Vmtx::parse(&bytes, 0, 0),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_long_count_exceeding_total() {
        let bytes = build_vmtx(&[(1000, 0), (1000, 0)], &[]);
        assert!(matches!(
            Vmtx::parse(&bytes, 1, 2),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_truncated_body() {
        let bytes = build_vmtx(&[(1000, 0), (1100, 0)], &[]);
        assert!(matches!(
            Vmtx::parse(&bytes, 5, 2),
            Err(Error::Truncated { .. })
        ));
    }
}
