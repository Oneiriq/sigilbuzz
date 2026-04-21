//! `hhea` — horizontal header.
//!
//! The field sigilbuzz needs most is `numberOfHMetrics`, which slices
//! `hmtx` in half between full-metric glyphs (advance + LSB) and
//! LSB-only tail glyphs. Line metrics (`ascent`, `descent`,
//! `lineGap`) are captured at the same time because callers asking
//! for a line height should not have to parse `hhea` a second time.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// The parsed `hhea` table — only the fields a shaper needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hhea {
    /// Distance from the baseline to the highest ascender, in design
    /// units. Positive above the baseline.
    pub ascent: i16,
    /// Distance from the baseline to the lowest descender, in design
    /// units. Typically negative.
    pub descent: i16,
    /// Typographic line gap in design units. Adds to
    /// `ascent - descent` to yield the recommended line height.
    pub line_gap: i16,
    /// Count of `longHorMetric` entries at the start of `hmtx`.
    /// Glyph ids beyond this count repeat the last entry's advance
    /// and store only the LSB in the trailing bearing array.
    pub number_of_h_metrics: u16,
}

impl Hhea {
    /// Parses a `hhea` table.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);

        let major = r.read_u16()?;
        let minor = r.read_u16()?;
        if major != 1 || minor != 0 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported hhea version",
            });
        }

        let ascent = r.read_i16()?;
        let descent = r.read_i16()?;
        let line_gap = r.read_i16()?;

        // advanceWidthMax, minLeftSideBearing, minRightSideBearing,
        // xMaxExtent, caretSlopeRise, caretSlopeRun, caretOffset —
        // seven u16/i16 we don't consume yet.
        r.skip(2 * 7)?;
        // Four i16 reserved fields.
        r.skip(2 * 4)?;

        let metric_format_offset = r.position();
        let metric_format = r.read_i16()?;
        if metric_format != 0 {
            return Err(Error::Malformed {
                offset: metric_format_offset,
                context: "hhea metricDataFormat must be 0",
            });
        }

        let number_of_h_metrics = r.read_u16()?;
        if number_of_h_metrics == 0 {
            return Err(Error::Malformed {
                offset: r.position() - 2,
                context: "numberOfHMetrics must be at least 1",
            });
        }

        Ok(Self {
            ascent,
            descent,
            line_gap,
            number_of_h_metrics,
        })
    }

    /// Recommended line height in design units:
    /// `ascent - descent + line_gap`.
    #[must_use]
    pub const fn line_height(&self) -> i32 {
        self.ascent as i32 - self.descent as i32 + self.line_gap as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn valid_hhea(ascent: i16, descent: i16, line_gap: i16, n: u16) -> Vec<u8> {
        let mut b = Vec::with_capacity(36);
        b.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
        b.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
        b.extend_from_slice(&ascent.to_be_bytes());
        b.extend_from_slice(&descent.to_be_bytes());
        b.extend_from_slice(&line_gap.to_be_bytes());
        // Seven more i16/u16 we ignore.
        b.extend_from_slice(&[0; 14]);
        // Four reserved i16.
        b.extend_from_slice(&[0; 8]);
        // metricDataFormat.
        b.extend_from_slice(&0i16.to_be_bytes());
        // numberOfHMetrics.
        b.extend_from_slice(&n.to_be_bytes());
        b
    }

    #[test]
    fn parses_typical_line_metrics() {
        let b = valid_hhea(1638, -410, 90, 256);
        let hhea = Hhea::parse(&b).unwrap();
        assert_eq!(hhea.ascent, 1638);
        assert_eq!(hhea.descent, -410);
        assert_eq!(hhea.line_gap, 90);
        assert_eq!(hhea.number_of_h_metrics, 256);
    }

    #[test]
    fn line_height_is_ascent_minus_descent_plus_gap() {
        let b = valid_hhea(1638, -410, 90, 1);
        let hhea = Hhea::parse(&b).unwrap();
        assert_eq!(hhea.line_height(), 1638 - (-410) + 90);
    }

    #[test]
    fn rejects_non_zero_metric_data_format() {
        let mut b = valid_hhea(1000, -200, 0, 1);
        // metricDataFormat lives two bytes before the end.
        let idx = b.len() - 4;
        b[idx..idx + 2].copy_from_slice(&1i16.to_be_bytes());
        assert!(matches!(Hhea::parse(&b), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_zero_metrics_count() {
        let b = valid_hhea(1000, -200, 0, 0);
        assert!(matches!(Hhea::parse(&b), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_input() {
        let b = valid_hhea(1000, -200, 0, 1);
        assert!(matches!(
            Hhea::parse(&b[..10]),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_wrong_version() {
        let mut b = valid_hhea(1000, -200, 0, 1);
        b[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Hhea::parse(&b), Err(Error::Malformed { .. })));
    }
}
