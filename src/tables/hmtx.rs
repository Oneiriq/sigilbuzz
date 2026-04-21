//! `hmtx` — horizontal metrics.
//!
//! Layout, as prescribed by the OpenType spec:
//!
//! ```text
//!   longHorMetric[numberOfHMetrics] {
//!       u16 advanceWidth
//!       i16 leftSideBearing
//!   }
//!   i16  leftSideBearing[numGlyphs - numberOfHMetrics]
//! ```
//!
//! Glyph ids beyond the `numberOfHMetrics` boundary re-use the advance
//! of the final `longHorMetric` entry. This is how monospace fonts
//! encode 60 000 glyphs with one advance width.

use crate::error::{Error, Result};

/// Parsed horizontal metrics for a font.
///
/// Borrows the underlying bytes; every glyph lookup is a small
/// arithmetic expression on top of two slices.
#[derive(Debug, Clone, Copy)]
pub struct Hmtx<'a> {
    /// Fixed-size long-metric block: `number_of_h_metrics` entries of
    /// four bytes each (u16 advance + i16 lsb).
    long_metrics: &'a [u8],
    /// Trailing LSB-only block: two bytes per glyph past the long
    /// boundary. May be empty when every glyph has its own advance.
    trailing_lsbs: &'a [u8],
    /// Count of full long-metric entries.
    number_of_h_metrics: u16,
    /// Total glyph count from `maxp`.
    num_glyphs: u16,
    /// Cached last advance so the "tail glyph" lookup is O(1).
    last_advance: u16,
}

impl<'a> Hmtx<'a> {
    /// Parses the table layout given the glyph count from `maxp` and
    /// the long-metric count from `hhea`.
    pub fn parse(data: &'a [u8], num_glyphs: u16, number_of_h_metrics: u16) -> Result<Self> {
        if number_of_h_metrics == 0 {
            return Err(Error::Malformed {
                offset: 0,
                context: "hmtx requires at least one long metric",
            });
        }
        if number_of_h_metrics > num_glyphs {
            return Err(Error::Malformed {
                offset: 0,
                context: "numberOfHMetrics exceeds numGlyphs",
            });
        }

        let long_bytes = number_of_h_metrics as usize * 4;
        let trailing_count = (num_glyphs - number_of_h_metrics) as usize;
        let trailing_bytes = trailing_count * 2;
        let required = long_bytes + trailing_bytes;
        if data.len() < required {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "hmtx body shorter than declared",
            });
        }

        let long_metrics = &data[..long_bytes];
        let trailing_lsbs = &data[long_bytes..long_bytes + trailing_bytes];

        // The last long metric's advance is the one repeated for
        // trailing glyphs. Pull it now so lookups stay branch-light.
        let last_idx = (number_of_h_metrics as usize - 1) * 4;
        let last_advance = u16::from_be_bytes([long_metrics[last_idx], long_metrics[last_idx + 1]]);

        Ok(Self {
            long_metrics,
            trailing_lsbs,
            number_of_h_metrics,
            num_glyphs,
            last_advance,
        })
    }

    /// Total glyph count this table covers.
    #[must_use]
    pub const fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    /// Horizontal advance width for `glyph`, in font design units.
    /// Returns `None` if the glyph id is out of range.
    #[must_use]
    pub fn advance(&self, glyph: u16) -> Option<u16> {
        if glyph >= self.num_glyphs {
            return None;
        }
        if glyph < self.number_of_h_metrics {
            let off = glyph as usize * 4;
            Some(u16::from_be_bytes([
                self.long_metrics[off],
                self.long_metrics[off + 1],
            ]))
        } else {
            Some(self.last_advance)
        }
    }

    /// Left side bearing for `glyph`.
    #[must_use]
    pub fn lsb(&self, glyph: u16) -> Option<i16> {
        if glyph >= self.num_glyphs {
            return None;
        }
        if glyph < self.number_of_h_metrics {
            let off = glyph as usize * 4 + 2;
            Some(i16::from_be_bytes([
                self.long_metrics[off],
                self.long_metrics[off + 1],
            ]))
        } else {
            let off = (glyph - self.number_of_h_metrics) as usize * 2;
            Some(i16::from_be_bytes([
                self.trailing_lsbs[off],
                self.trailing_lsbs[off + 1],
            ]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_hmtx(longs: &[(u16, i16)], trailing_lsbs: &[i16]) -> Vec<u8> {
        let mut b = Vec::new();
        for (adv, lsb) in longs {
            b.extend_from_slice(&adv.to_be_bytes());
            b.extend_from_slice(&lsb.to_be_bytes());
        }
        for lsb in trailing_lsbs {
            b.extend_from_slice(&lsb.to_be_bytes());
        }
        b
    }

    #[test]
    fn every_glyph_has_its_own_metric() {
        let bytes = build_hmtx(&[(500, 10), (600, 20), (700, 30)], &[]);
        let hmtx = Hmtx::parse(&bytes, 3, 3).unwrap();
        assert_eq!(hmtx.advance(0), Some(500));
        assert_eq!(hmtx.advance(1), Some(600));
        assert_eq!(hmtx.advance(2), Some(700));
        assert_eq!(hmtx.lsb(2), Some(30));
    }

    #[test]
    fn trailing_glyphs_share_the_last_advance() {
        // numGlyphs = 5, numberOfHMetrics = 2.
        let bytes = build_hmtx(&[(500, 10), (600, 20)], &[30, 40, 50]);
        let hmtx = Hmtx::parse(&bytes, 5, 2).unwrap();
        assert_eq!(hmtx.advance(0), Some(500));
        assert_eq!(hmtx.advance(1), Some(600));
        // Glyphs 2..5 all repeat the last long metric's advance.
        assert_eq!(hmtx.advance(2), Some(600));
        assert_eq!(hmtx.advance(4), Some(600));
        // But keep their own LSBs.
        assert_eq!(hmtx.lsb(2), Some(30));
        assert_eq!(hmtx.lsb(3), Some(40));
        assert_eq!(hmtx.lsb(4), Some(50));
    }

    #[test]
    fn out_of_range_glyph_returns_none() {
        let bytes = build_hmtx(&[(500, 0)], &[]);
        let hmtx = Hmtx::parse(&bytes, 1, 1).unwrap();
        assert_eq!(hmtx.advance(1), None);
        assert_eq!(hmtx.lsb(7), None);
    }

    #[test]
    fn rejects_zero_long_metrics() {
        let bytes = Vec::new();
        assert!(matches!(
            Hmtx::parse(&bytes, 0, 0),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_long_count_exceeding_total() {
        let bytes = build_hmtx(&[(500, 0), (500, 0)], &[]);
        assert!(matches!(
            Hmtx::parse(&bytes, 1, 2),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_truncated_body() {
        let bytes = build_hmtx(&[(500, 0), (600, 0)], &[]);
        // Declare 2 longs + 3 trailing LSBs but only provide 2 longs.
        assert!(matches!(
            Hmtx::parse(&bytes, 5, 2),
            Err(Error::Truncated { .. })
        ));
    }
}
