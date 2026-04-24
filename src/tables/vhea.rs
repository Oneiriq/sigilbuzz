//! `vhea` — vertical header.
//!
//! Symmetric with `hhea`: carries vertical line metrics plus the
//! `numberOfLongVerMetrics` count that slices `vmtx` into its full-
//! metric prefix and its top-side-bearing tail. Fonts that support
//! vertical writing (CJK, Mongolian, some Hebrew) ship `vhea` and
//! `vmtx` side by side.
//!
//! The spec has two versions: 1.0 (Apple Advanced Typography) and
//! 1.1 (OpenType). The field at offsets 4..10 differs slightly —
//! `ascent`/`descent`/`lineGap` in 1.1 vs. `vertTypoAscender` et al.
//! in 1.0 — but the byte layout is identical, so sigilbuzz treats
//! them uniformly and reads whichever version the font carries.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// The parsed `vhea` table — only the fields a shaper needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vhea {
    /// Distance from the centerline to the previous line's
    /// descent, in design units. Positive for ascenders in the
    /// vertical writing sense (i.e. toward the top of the page).
    pub ascent: i16,
    /// Distance from the centerline to the next line's ascent, in
    /// design units. Typically negative.
    pub descent: i16,
    /// Typographic line gap in design units. Adds to
    /// `ascent - descent` to yield recommended line advance.
    pub line_gap: i16,
    /// Count of `longVerMetric` entries at the start of `vmtx`.
    /// Glyph ids beyond this count repeat the last entry's advance
    /// and store only the top-side-bearing in the trailing array.
    pub number_of_long_ver_metrics: u16,
}

impl Vhea {
    /// Parses a `vhea` table. Accepts both version 1.0 (AAT) and 1.1
    /// (OpenType); the byte layout is identical across the two.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);

        let major = r.read_u16()?;
        let minor = r.read_u16()?;
        // OpenType ships 1.0 (legacy) and 1.1; Apple ships 1.0. We
        // accept any (major=1, minor in {0, 1}) because the field
        // layout through numberOfLongVerMetrics is identical.
        if major != 1 || (minor != 0 && minor != 1 && minor != 0x1000) {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported vhea version",
            });
        }

        let ascent = r.read_i16()?;
        let descent = r.read_i16()?;
        let line_gap = r.read_i16()?;

        // advanceHeightMax, minTopSideBearing, minBottomSideBearing,
        // yMaxExtent, caretSlopeRise, caretSlopeRun, caretOffset —
        // seven u16/i16 we don't consume yet.
        r.skip(2 * 7)?;
        // Four i16 reserved fields.
        r.skip(2 * 4)?;

        let metric_format_offset = r.position();
        let metric_format = r.read_i16()?;
        if metric_format != 0 {
            return Err(Error::Malformed {
                offset: metric_format_offset,
                context: "vhea metricDataFormat must be 0",
            });
        }

        let number_of_long_ver_metrics = r.read_u16()?;
        if number_of_long_ver_metrics == 0 {
            return Err(Error::Malformed {
                offset: r.position() - 2,
                context: "numberOfLongVerMetrics must be at least 1",
            });
        }

        Ok(Self {
            ascent,
            descent,
            line_gap,
            number_of_long_ver_metrics,
        })
    }

    /// Recommended vertical line advance, in design units:
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

    fn valid_vhea(major: u16, minor: u16, ascent: i16, descent: i16, gap: i16, n: u16) -> Vec<u8> {
        let mut b = Vec::with_capacity(36);
        b.extend_from_slice(&major.to_be_bytes());
        b.extend_from_slice(&minor.to_be_bytes());
        b.extend_from_slice(&ascent.to_be_bytes());
        b.extend_from_slice(&descent.to_be_bytes());
        b.extend_from_slice(&gap.to_be_bytes());
        // Seven u16/i16 we ignore.
        b.extend_from_slice(&[0; 14]);
        // Four reserved i16.
        b.extend_from_slice(&[0; 8]);
        // metricDataFormat.
        b.extend_from_slice(&0i16.to_be_bytes());
        // numberOfLongVerMetrics.
        b.extend_from_slice(&n.to_be_bytes());
        b
    }

    #[test]
    fn parses_version_1_0() {
        let b = valid_vhea(1, 0, 500, -500, 0, 256);
        let v = Vhea::parse(&b).unwrap();
        assert_eq!(v.ascent, 500);
        assert_eq!(v.descent, -500);
        assert_eq!(v.number_of_long_ver_metrics, 256);
    }

    #[test]
    fn parses_version_1_1() {
        let b = valid_vhea(1, 1, 880, -120, 40, 4);
        let v = Vhea::parse(&b).unwrap();
        assert_eq!(v.ascent, 880);
        assert_eq!(v.descent, -120);
        assert_eq!(v.line_gap, 40);
    }

    #[test]
    fn line_height_combines_metrics() {
        let b = valid_vhea(1, 1, 880, -120, 40, 1);
        let v = Vhea::parse(&b).unwrap();
        assert_eq!(v.line_height(), 880 - (-120) + 40);
    }

    #[test]
    fn rejects_wrong_major_version() {
        let b = valid_vhea(2, 0, 0, 0, 0, 1);
        assert!(matches!(Vhea::parse(&b), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_non_zero_metric_data_format() {
        let mut b = valid_vhea(1, 1, 100, -100, 0, 1);
        let idx = b.len() - 4;
        b[idx..idx + 2].copy_from_slice(&1i16.to_be_bytes());
        assert!(matches!(Vhea::parse(&b), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_zero_metrics_count() {
        let b = valid_vhea(1, 1, 100, -100, 0, 0);
        assert!(matches!(Vhea::parse(&b), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_input() {
        let b = valid_vhea(1, 1, 100, -100, 0, 1);
        assert!(matches!(
            Vhea::parse(&b[..10]),
            Err(Error::Truncated { .. })
        ));
    }
}
