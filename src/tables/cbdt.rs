//! `CBDT`: Color Bitmap Data.
//!
//! `CBDT` holds the bitmap pixels themselves; [`Cblc`](crate::tables::cblc::Cblc)
//! is the index that turns a glyph id at a given strike into a byte
//! offset into this table.
//!
//! sigilbuzz stops at "return the raw payload bytes plus their
//! metrics". Decoding PNG / mask data is the consumer's job: every
//! sane caller already has a PNG decoder, and pulling one in here
//! would violate the no-default-feature build.
//!
//! # Format
//!
//! ```text
//!   CBDT header
//!     0  u16  majorVersion        = 3
//!     2  u16  minorVersion        = 0
//!
//!   Per-glyph data (formats 1-9, 17-19; sigilbuzz handles 17-19):
//!     17: SmallGlyphMetrics (5 B), u32 dataLen, u8 data[dataLen] (PNG)
//!     18: BigGlyphMetrics   (8 B), u32 dataLen, u8 data[dataLen] (PNG)
//!     19:                          u32 dataLen, u8 data[dataLen] (PNG, metrics from CBLC)
//! ```
//!
//! Formats 1-9 are the legacy EBDT mask formats (1-bit, 8-bit, etc.).
//! sigilbuzz's API still works for those (`BitmapData::Mask` is the
//! pass-through variant), but consumers of CBDT today are uniformly
//! looking at PNG payloads.

use crate::error::{Error, Result};
use crate::tables::cblc::{BigGlyphMetrics, CbdtLocation, SmallGlyphMetrics};
use crate::tables::parse::Reader;

/// One glyph's metrics. For formats with no inline metrics (CBDT 19,
/// CBLC index 2 / 5) the strike-level `BigGlyphMetrics` lives in CBLC
/// and arrives via [`CbdtLocation::metrics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphBitmapMetrics {
    /// 5-byte horizontal-only metrics.
    Small(SmallGlyphMetrics),
    /// 8-byte horizontal + vertical metrics.
    Big(BigGlyphMetrics),
}

/// A parsed CBDT entry: payload bytes plus their format and metrics.
/// Borrows into the CBDT table; cloning is a slice-copy.
#[derive(Debug, Clone, Copy)]
pub struct GlyphBitmap<'a> {
    /// CBDT image format id (17 / 18 / 19 etc.). Mirrors the
    /// `imageFormat` field from CBLC's IndexSubTable header.
    pub image_format: u16,
    /// Per-glyph metrics. Always present after parsing: for format
    /// 19 the strike-level metrics from CBLC are folded in.
    pub metrics: GlyphBitmapMetrics,
    /// The raw payload: PNG bytes for formats 17-19, mask bytes for
    /// the older formats. sigilbuzz never decodes.
    pub data: &'a [u8],
}

/// Wraps a CBDT byte slice and dispenses per-glyph parses.
#[derive(Debug, Clone, Copy)]
pub struct Cbdt<'a> {
    data: &'a [u8],
}

impl<'a> Cbdt<'a> {
    /// Validates the table header and stores the slice.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 2 && major != 3 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported CBDT major version",
            });
        }
        Ok(Self { data })
    }

    /// Raw table bytes.
    #[must_use]
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Reads the glyph bitmap pointed to by `loc`.
    ///
    /// `loc` comes from [`Cblc::locate`](crate::tables::cblc::Cblc::locate).
    /// For format 17/18 the inline header carries the metrics; for
    /// format 19 the strike-level metrics in `loc.metrics` are used
    /// instead (and an absent `loc.metrics` falls back to a zero
    /// SmallGlyphMetrics so the parser never panics on a malformed
    /// pairing).
    pub fn glyph_bitmap(&self, loc: &CbdtLocation) -> Result<GlyphBitmap<'a>> {
        let start = loc.offset as usize;
        let end = start
            .checked_add(loc.length as usize)
            .ok_or(Error::Malformed {
                offset: start,
                context: "CBDT location overflow",
            })?;
        if end > self.data.len() {
            return Err(Error::Truncated {
                offset: end,
                context: "CBDT slice past end of table",
            });
        }
        let slice = &self.data[start..end];
        let mut r = Reader::new(slice);

        match loc.image_format {
            17 => {
                // Small metrics + PNG data.
                let metrics = SmallGlyphMetrics::parse(&mut r)?;
                let data_len = r.read_u32()? as usize;
                let data_start = r.position();
                if data_start + data_len > slice.len() {
                    return Err(Error::Truncated {
                        offset: data_start + data_len,
                        context: "CBDT format 17 PNG data",
                    });
                }
                Ok(GlyphBitmap {
                    image_format: 17,
                    metrics: GlyphBitmapMetrics::Small(metrics),
                    data: &slice[data_start..data_start + data_len],
                })
            }
            18 => {
                // Big metrics + PNG data.
                let metrics = parse_big(&mut r)?;
                let data_len = r.read_u32()? as usize;
                let data_start = r.position();
                if data_start + data_len > slice.len() {
                    return Err(Error::Truncated {
                        offset: data_start + data_len,
                        context: "CBDT format 18 PNG data",
                    });
                }
                Ok(GlyphBitmap {
                    image_format: 18,
                    metrics: GlyphBitmapMetrics::Big(metrics),
                    data: &slice[data_start..data_start + data_len],
                })
            }
            19 => {
                // No inline metrics; PNG data only. Metrics ride along
                // on the CBLC location record (constant-metric format).
                let data_len = r.read_u32()? as usize;
                let data_start = r.position();
                if data_start + data_len > slice.len() {
                    return Err(Error::Truncated {
                        offset: data_start + data_len,
                        context: "CBDT format 19 PNG data",
                    });
                }
                let metrics = match loc.metrics {
                    Some(big) => GlyphBitmapMetrics::Big(big),
                    None => GlyphBitmapMetrics::Small(SmallGlyphMetrics::default()),
                };
                Ok(GlyphBitmap {
                    image_format: 19,
                    metrics,
                    data: &slice[data_start..data_start + data_len],
                })
            }
            _ => Err(Error::Unsupported {
                context: "CBDT image format (only 17/18/19 are decoded)",
            }),
        }
    }
}

fn parse_big(r: &mut Reader<'_>) -> Result<BigGlyphMetrics> {
    Ok(BigGlyphMetrics {
        height: r.read_u8()?,
        width: r.read_u8()?,
        hori_bearing_x: r.read_i8()?,
        hori_bearing_y: r.read_i8()?,
        hori_advance: r.read_u8()?,
        vert_bearing_x: r.read_i8()?,
        vert_bearing_y: r.read_i8()?,
        vert_advance: r.read_u8()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn header() -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&3u16.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h
    }

    #[test]
    fn header_round_trips() {
        let h = header();
        let cbdt = Cbdt::parse(&h).unwrap();
        assert_eq!(cbdt.data().len(), 4);
    }

    #[test]
    fn rejects_bad_version() {
        let mut h = Vec::new();
        h.extend_from_slice(&7u16.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        let err = Cbdt::parse(&h).unwrap_err();
        assert!(matches!(err, Error::Malformed { .. }));
    }

    #[test]
    fn parses_format17_small_metrics_plus_png() {
        // Header (4) + payload.
        let mut data = header();
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0xff, 0xee];
        let payload_off = data.len() as u32;
        // small metrics: h, w, bx, by, adv
        data.extend_from_slice(&[20, 18, 1, 22, 24]);
        data.extend_from_slice(&(png.len() as u32).to_be_bytes());
        data.extend_from_slice(&png);
        let payload_len = data.len() as u32 - payload_off;

        let cbdt = Cbdt::parse(&data).unwrap();
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 17,
            metrics: None,
        };
        let bm = cbdt.glyph_bitmap(&loc).unwrap();
        assert_eq!(bm.image_format, 17);
        match bm.metrics {
            GlyphBitmapMetrics::Small(s) => {
                assert_eq!(s.height, 20);
                assert_eq!(s.width, 18);
                assert_eq!(s.advance, 24);
            }
            GlyphBitmapMetrics::Big(_) => panic!("expected Small"),
        }
        assert_eq!(bm.data, &png);
    }

    #[test]
    fn parses_format18_big_metrics_plus_png() {
        let mut data = header();
        let png = [0xab, 0xcd, 0xef];
        let payload_off = data.len() as u32;
        // big metrics: 8 bytes
        data.extend_from_slice(&[10, 12, -1i8 as u8, 9, 14, 0, 1, 16]);
        data.extend_from_slice(&(png.len() as u32).to_be_bytes());
        data.extend_from_slice(&png);
        let payload_len = data.len() as u32 - payload_off;

        let cbdt = Cbdt::parse(&data).unwrap();
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 18,
            metrics: None,
        };
        let bm = cbdt.glyph_bitmap(&loc).unwrap();
        match bm.metrics {
            GlyphBitmapMetrics::Big(b) => {
                assert_eq!(b.height, 10);
                assert_eq!(b.width, 12);
                assert_eq!(b.hori_bearing_x, -1);
                assert_eq!(b.vert_advance, 16);
            }
            GlyphBitmapMetrics::Small(_) => panic!("expected Big"),
        }
        assert_eq!(bm.data, &png);
    }

    #[test]
    fn parses_format19_uses_strike_metrics() {
        let mut data = header();
        let png = [0x01, 0x02, 0x03, 0x04];
        let payload_off = data.len() as u32;
        data.extend_from_slice(&(png.len() as u32).to_be_bytes());
        data.extend_from_slice(&png);
        let payload_len = data.len() as u32 - payload_off;

        let cbdt = Cbdt::parse(&data).unwrap();
        let strike_metrics = BigGlyphMetrics {
            height: 32,
            width: 32,
            hori_bearing_x: 0,
            hori_bearing_y: 32,
            hori_advance: 36,
            vert_bearing_x: 0,
            vert_bearing_y: 0,
            vert_advance: 0,
        };
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 19,
            metrics: Some(strike_metrics),
        };
        let bm = cbdt.glyph_bitmap(&loc).unwrap();
        match bm.metrics {
            GlyphBitmapMetrics::Big(b) => assert_eq!(b.height, 32),
            GlyphBitmapMetrics::Small(_) => panic!("expected Big"),
        }
        assert_eq!(bm.data, &png);
    }

    #[test]
    fn unsupported_format_surfaces_error() {
        let h = header();
        let cbdt = Cbdt::parse(&h).unwrap();
        let loc = CbdtLocation {
            offset: 0,
            length: 4,
            image_format: 5,
            metrics: None,
        };
        assert!(matches!(
            cbdt.glyph_bitmap(&loc),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn rejects_payload_extending_past_table_end() {
        let h = header();
        let cbdt = Cbdt::parse(&h).unwrap();
        let loc = CbdtLocation {
            offset: 4,
            length: 1000,
            image_format: 17,
            metrics: None,
        };
        let err = cbdt.glyph_bitmap(&loc).unwrap_err();
        assert!(matches!(err, Error::Truncated { .. }));
    }
}
