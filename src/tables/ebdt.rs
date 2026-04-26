//! `EBDT` — Embedded Bitmap Data (Microsoft, monochrome).
//!
//! `EBDT` is the predecessor to [`Cbdt`](crate::tables::cbdt::Cbdt):
//! same indexing model (offsets resolved via
//! [`Eblc`](crate::tables::eblc::Eblc)), but the per-glyph payload is
//! a 1-bit-per-pixel mask rather than a PNG. The mask formats are:
//!
//! - **1**: SmallGlyphMetrics (5 B) + byte-aligned 1bpp. Each scanline
//!   pads to a byte boundary; consecutive scanlines do not pack across
//!   the boundary.
//! - **2**: SmallGlyphMetrics + bit-aligned 1bpp. Pixels pack tight
//!   across scanlines; the row stride is `ceil(width / 8)` bits, not
//!   bytes, and the next row's bits resume mid-byte.
//! - **5**: bit-aligned 1bpp, no inline metrics (paired with EBLC
//!   index format 2/5 which carries strike-level
//!   [`BigGlyphMetrics`](crate::tables::cblc::BigGlyphMetrics)).
//! - **6**: BigGlyphMetrics (8 B) + byte-aligned 1bpp.
//! - **7**: BigGlyphMetrics + bit-aligned 1bpp.
//! - **8**: SmallGlyphMetrics + 1 byte pad + `u16 num_components` +
//!   `EbdtComponent[num_components]`. Each component is a 4-byte
//!   record: `u16 glyph_id`, `i8 x_offset`, `i8 y_offset` referencing
//!   another EBDT entry to alpha-overlay onto the parent canvas.
//! - **9**: BigGlyphMetrics + `u16 num_components` +
//!   `EbdtComponent[num_components]`. Big-metrics counterpart of
//!   format 8.
//!
//! sigilbuzz returns the parsed [`EbdtBitmap`] which carries the
//! resolved metrics, the bit-packing flavour, and the raw mask bytes.
//! For composite formats 8 / 9 the [`EbdtBitmap::data`] slice is empty
//! and the parsed component records hang off [`EbdtBitmap::components`].
//! The renderer is responsible for unpacking the 1bpp data and for
//! recursing into composite components — see `sigilbuzz-render::bitmaps`.

use crate::error::{Error, Result};
use crate::tables::cblc::{BigGlyphMetrics, CbdtLocation, SmallGlyphMetrics};
use crate::tables::parse::Reader;

/// One EBDT entry's metrics. Mirrors the CBDT shape so consumers can
/// code against a single enum across both tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EbdtMetrics {
    /// 5-byte horizontal-only metrics (formats 1, 2).
    Small(SmallGlyphMetrics),
    /// 8-byte horizontal + vertical metrics (formats 5, 6, 7 with
    /// strike-level metrics from EBLC).
    Big(BigGlyphMetrics),
}

impl EbdtMetrics {
    /// Glyph height in pixels (always present in either variant).
    #[must_use]
    pub fn height(&self) -> u8 {
        match self {
            Self::Small(s) => s.height,
            Self::Big(b) => b.height,
        }
    }

    /// Glyph width in pixels.
    #[must_use]
    pub fn width(&self) -> u8 {
        match self {
            Self::Small(s) => s.width,
            Self::Big(b) => b.width,
        }
    }
}

/// How the 1bpp mask is packed in [`EbdtBitmap::data`]. Formats 1 / 6
/// pad each row to a byte boundary; formats 2 / 5 / 7 pack pixel bits
/// tight across rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitPacking {
    /// One scanline per row, tail-padded to the next byte boundary.
    /// Row stride in bytes is `ceil(width / 8)`.
    ByteAligned,
    /// Bits pack tightly across rows; row N+1 resumes mid-byte from
    /// where row N ended. Total mask size is `ceil(width * height / 8)`
    /// bytes.
    BitAligned,
}

/// One component of an EBDT composite entry (formats 8 / 9). Each
/// composite glyph is built by alpha-overlaying the bitmap of the
/// referenced `glyph_id` at the given pixel offset onto the parent
/// canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EbdtComponent {
    /// Glyph id whose EBDT entry is composited into the parent. The
    /// referenced entry should itself be one of the non-composite
    /// formats (1 / 2 / 5 / 6 / 7) at the same strike.
    pub glyph_id: u16,
    /// Pixel offset on the X axis applied before compositing.
    pub x_offset: i8,
    /// Pixel offset on the Y axis applied before compositing.
    pub y_offset: i8,
}

/// A parsed EBDT entry — metrics, packing flavour, and either the raw
/// mask bytes (formats 1 / 2 / 5 / 6 / 7) or the parsed component
/// references (formats 8 / 9).
#[derive(Debug, Clone, Copy)]
pub struct EbdtBitmap<'a> {
    /// EBDT image format id (1 / 2 / 5 / 6 / 7 / 8 / 9).
    pub image_format: u16,
    /// Resolved per-glyph metrics. For formats with strike-level
    /// metrics (5), the metrics are folded in from the EBLC location.
    pub metrics: EbdtMetrics,
    /// Whether the mask is byte- or bit-aligned per scanline. For
    /// composite formats (8 / 9) this is set to
    /// [`BitPacking::ByteAligned`] but is not meaningful — the
    /// component records carry no mask of their own.
    pub packing: BitPacking,
    /// Raw mask bytes. Most-significant bit of each byte holds the
    /// leftmost pixel. Empty slice for composite formats (8 / 9).
    pub data: &'a [u8],
    /// Raw, unparsed component-array bytes for composite formats
    /// (8 / 9). Each record is exactly 4 bytes: `u16 glyph_id`,
    /// `i8 x_offset`, `i8 y_offset` (big-endian). Empty slice for
    /// non-composite formats. Iterate via [`EbdtBitmap::components`].
    pub components_raw: &'a [u8],
}

impl<'a> EbdtBitmap<'a> {
    /// Number of composite components (formats 8 / 9). Always zero
    /// for non-composite formats.
    #[must_use]
    pub fn component_count(&self) -> u16 {
        // 4 bytes per record. The byte slice was sized to a multiple
        // of 4 by the parser, and `num_components` is u16, so the
        // count fits.
        #[allow(clippy::cast_possible_truncation)]
        let n = (self.components_raw.len() / 4) as u16;
        n
    }

    /// Iterates parsed [`EbdtComponent`] records for composite formats
    /// (8 / 9). Yields nothing for non-composite formats.
    pub fn components(&self) -> impl Iterator<Item = EbdtComponent> + 'a {
        self.components_raw.chunks_exact(4).map(|c| EbdtComponent {
            glyph_id: u16::from_be_bytes([c[0], c[1]]),
            #[allow(clippy::cast_possible_wrap)]
            x_offset: c[2] as i8,
            #[allow(clippy::cast_possible_wrap)]
            y_offset: c[3] as i8,
        })
    }

    /// True when this entry is one of the composite formats (8 / 9).
    #[must_use]
    pub fn is_composite(&self) -> bool {
        matches!(self.image_format, 8 | 9)
    }
}

/// Wraps an EBDT byte slice and dispenses per-glyph parses.
#[derive(Debug, Clone, Copy)]
pub struct Ebdt<'a> {
    data: &'a [u8],
}

impl<'a> Ebdt<'a> {
    /// Validates the table header and stores the slice.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 2 && major != 3 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported EBDT major version",
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
    /// `loc` comes from [`Eblc::locate`](crate::tables::eblc::Eblc::locate).
    /// For formats 1 / 2 / 6 / 7 the inline header carries the
    /// metrics; for format 5 the strike-level metrics from
    /// [`CbdtLocation::metrics`] are used (if absent, a zero
    /// `BigGlyphMetrics` substitutes — sigilbuzz never panics on a
    /// malformed pairing).
    pub fn glyph_bitmap(&self, loc: &CbdtLocation) -> Result<EbdtBitmap<'a>> {
        let start = loc.offset as usize;
        let end = start
            .checked_add(loc.length as usize)
            .ok_or(Error::Malformed {
                offset: start,
                context: "EBDT location overflow",
            })?;
        if end > self.data.len() {
            return Err(Error::Truncated {
                offset: end,
                context: "EBDT slice past end of table",
            });
        }
        let slice = &self.data[start..end];
        let mut r = Reader::new(slice);

        match loc.image_format {
            1 => {
                // Small metrics + byte-aligned 1bpp.
                let metrics = SmallGlyphMetrics::parse(&mut r)?;
                let data_start = r.position();
                Ok(EbdtBitmap {
                    image_format: 1,
                    metrics: EbdtMetrics::Small(metrics),
                    packing: BitPacking::ByteAligned,
                    data: &slice[data_start..],
                    components_raw: &[],
                })
            }
            2 => {
                // Small metrics + bit-aligned 1bpp.
                let metrics = SmallGlyphMetrics::parse(&mut r)?;
                let data_start = r.position();
                Ok(EbdtBitmap {
                    image_format: 2,
                    metrics: EbdtMetrics::Small(metrics),
                    packing: BitPacking::BitAligned,
                    data: &slice[data_start..],
                    components_raw: &[],
                })
            }
            5 => {
                // No inline metrics; bit-aligned 1bpp. Strike metrics
                // ride along on the EBLC location (constant-metric
                // formats 2 / 5).
                let metrics = match loc.metrics {
                    Some(big) => EbdtMetrics::Big(big),
                    None => EbdtMetrics::Big(BigGlyphMetrics::default()),
                };
                Ok(EbdtBitmap {
                    image_format: 5,
                    metrics,
                    packing: BitPacking::BitAligned,
                    data: slice,
                    components_raw: &[],
                })
            }
            6 => {
                // Big metrics + byte-aligned 1bpp.
                let metrics = parse_big(&mut r)?;
                let data_start = r.position();
                Ok(EbdtBitmap {
                    image_format: 6,
                    metrics: EbdtMetrics::Big(metrics),
                    packing: BitPacking::ByteAligned,
                    data: &slice[data_start..],
                    components_raw: &[],
                })
            }
            7 => {
                // Big metrics + bit-aligned 1bpp.
                let metrics = parse_big(&mut r)?;
                let data_start = r.position();
                Ok(EbdtBitmap {
                    image_format: 7,
                    metrics: EbdtMetrics::Big(metrics),
                    packing: BitPacking::BitAligned,
                    data: &slice[data_start..],
                    components_raw: &[],
                })
            }
            8 => {
                // Composite, small metrics. Layout:
                //   SmallGlyphMetrics (5 B)
                //   pad           (1 B, ignored)
                //   numComponents (u16 BE)
                //   EbdtComponent[numComponents] (4 B each)
                let metrics = SmallGlyphMetrics::parse(&mut r)?;
                // Spec calls for one byte of padding between the
                // small metrics and num_components; consume it
                // explicitly so the cursor lands on the count.
                r.skip(1)?;
                let components_raw = read_components(&mut r, slice)?;
                Ok(EbdtBitmap {
                    image_format: 8,
                    metrics: EbdtMetrics::Small(metrics),
                    packing: BitPacking::ByteAligned,
                    data: &[],
                    components_raw,
                })
            }
            9 => {
                // Composite, big metrics. No padding between metrics
                // and num_components.
                let metrics = parse_big(&mut r)?;
                let components_raw = read_components(&mut r, slice)?;
                Ok(EbdtBitmap {
                    image_format: 9,
                    metrics: EbdtMetrics::Big(metrics),
                    packing: BitPacking::ByteAligned,
                    data: &[],
                    components_raw,
                })
            }
            _ => Err(Error::Unsupported {
                context: "EBDT image format (only 1/2/5/6/7/8/9 are decoded)",
            }),
        }
    }
}

/// Reads `u16 numComponents` followed by `numComponents` 4-byte
/// component records out of `r` and returns the raw component bytes
/// borrowed from `slice`. The records are returned unparsed so the
/// surrounding `EbdtBitmap` stays `Copy`-friendly; iterate them via
/// [`EbdtBitmap::components`].
fn read_components<'a>(r: &mut Reader<'a>, slice: &'a [u8]) -> Result<&'a [u8]> {
    let num_components = r.read_u16()? as usize;
    let need = num_components
        .checked_mul(4)
        .ok_or(Error::Malformed {
            offset: r.position(),
            context: "EBDT component count overflow",
        })?;
    let start = r.position();
    let end = start.checked_add(need).ok_or(Error::Malformed {
        offset: start,
        context: "EBDT component array overflow",
    })?;
    if end > slice.len() {
        return Err(Error::Truncated {
            offset: end,
            context: "EBDT component array past end of entry",
        });
    }
    Ok(&slice[start..end])
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
        h.extend_from_slice(&2u16.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h
    }

    #[test]
    fn header_round_trips() {
        let h = header();
        let ebdt = Ebdt::parse(&h).unwrap();
        assert_eq!(ebdt.data().len(), 4);
    }

    #[test]
    fn rejects_bad_version() {
        let mut h = Vec::new();
        h.extend_from_slice(&7u16.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        assert!(matches!(Ebdt::parse(&h), Err(Error::Malformed { .. })));
    }

    #[test]
    fn parses_format1_byte_aligned_with_small_metrics() {
        let mut data = header();
        let payload_off = data.len() as u32;
        // small metrics: h=2, w=8, bx=0, by=2, adv=10
        data.extend_from_slice(&[2, 8, 0, 2, 10]);
        // byte-aligned 1bpp 8-wide x 2-tall = 2 bytes
        data.extend_from_slice(&[0xAA, 0x55]);
        let payload_len = data.len() as u32 - payload_off;

        let ebdt = Ebdt::parse(&data).unwrap();
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 1,
            metrics: None,
        };
        let bm = ebdt.glyph_bitmap(&loc).unwrap();
        assert_eq!(bm.image_format, 1);
        assert_eq!(bm.packing, BitPacking::ByteAligned);
        assert_eq!(bm.metrics.height(), 2);
        assert_eq!(bm.metrics.width(), 8);
        assert_eq!(bm.data, &[0xAA, 0x55]);
    }

    #[test]
    fn parses_format2_bit_aligned_with_small_metrics() {
        let mut data = header();
        let payload_off = data.len() as u32;
        data.extend_from_slice(&[3, 5, 0, 3, 6]); // 3-tall 5-wide
                                                  // bit-aligned: 15 bits packed → 2 bytes
        data.extend_from_slice(&[0xFF, 0xC0]);
        let payload_len = data.len() as u32 - payload_off;

        let ebdt = Ebdt::parse(&data).unwrap();
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 2,
            metrics: None,
        };
        let bm = ebdt.glyph_bitmap(&loc).unwrap();
        assert_eq!(bm.packing, BitPacking::BitAligned);
        assert_eq!(bm.metrics.width(), 5);
    }

    #[test]
    fn parses_format5_uses_strike_metrics() {
        let mut data = header();
        let payload_off = data.len() as u32;
        data.extend_from_slice(&[0xF0, 0xF0]); // raw mask
        let payload_len = data.len() as u32 - payload_off;

        let ebdt = Ebdt::parse(&data).unwrap();
        let strike_metrics = BigGlyphMetrics {
            height: 4,
            width: 4,
            hori_bearing_x: 0,
            hori_bearing_y: 4,
            hori_advance: 5,
            vert_bearing_x: 0,
            vert_bearing_y: 0,
            vert_advance: 0,
        };
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 5,
            metrics: Some(strike_metrics),
        };
        let bm = ebdt.glyph_bitmap(&loc).unwrap();
        match bm.metrics {
            EbdtMetrics::Big(b) => assert_eq!(b.height, 4),
            EbdtMetrics::Small(_) => panic!("expected Big from strike metrics"),
        }
        assert_eq!(bm.packing, BitPacking::BitAligned);
    }

    #[test]
    fn parses_format6_big_metrics_byte_aligned() {
        let mut data = header();
        let payload_off = data.len() as u32;
        data.extend_from_slice(&[2, 8, 0, 2, 10, 0, 0, 0]); // big metrics
        data.extend_from_slice(&[0x55, 0xAA]);
        let payload_len = data.len() as u32 - payload_off;

        let ebdt = Ebdt::parse(&data).unwrap();
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 6,
            metrics: None,
        };
        let bm = ebdt.glyph_bitmap(&loc).unwrap();
        assert_eq!(bm.packing, BitPacking::ByteAligned);
        match bm.metrics {
            EbdtMetrics::Big(b) => assert_eq!(b.width, 8),
            EbdtMetrics::Small(_) => panic!("expected Big"),
        }
    }

    #[test]
    fn unsupported_format_surfaces_error() {
        let h = header();
        let ebdt = Ebdt::parse(&h).unwrap();
        let loc = CbdtLocation {
            offset: 0,
            length: 4,
            // 17 is a CBDT-only format (PNG); the EBDT dispatch
            // rejects it as unsupported.
            image_format: 17,
            metrics: None,
        };
        assert!(matches!(
            ebdt.glyph_bitmap(&loc),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn parses_format8_composite_small_metrics() {
        let mut data = header();
        let payload_off = data.len() as u32;
        // Small metrics (5B): h=4, w=4, bx=0, by=4, adv=4
        data.extend_from_slice(&[4, 4, 0, 4, 4]);
        // 1 byte of pad
        data.push(0);
        // numComponents = 2
        data.extend_from_slice(&2u16.to_be_bytes());
        // Component 1: glyph_id=2, x_offset=0, y_offset=0
        data.extend_from_slice(&2u16.to_be_bytes());
        data.push(0);
        data.push(0);
        // Component 2: glyph_id=3, x_offset=8, y_offset=-2
        data.extend_from_slice(&3u16.to_be_bytes());
        #[allow(clippy::cast_sign_loss)]
        data.push(8i8 as u8);
        #[allow(clippy::cast_sign_loss)]
        data.push(-2i8 as u8);
        let payload_len = data.len() as u32 - payload_off;

        let ebdt = Ebdt::parse(&data).unwrap();
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 8,
            metrics: None,
        };
        let bm = ebdt.glyph_bitmap(&loc).unwrap();
        assert!(bm.is_composite());
        assert_eq!(bm.image_format, 8);
        assert_eq!(bm.component_count(), 2);
        let comps: Vec<_> = bm.components().collect();
        assert_eq!(
            comps,
            vec![
                EbdtComponent {
                    glyph_id: 2,
                    x_offset: 0,
                    y_offset: 0
                },
                EbdtComponent {
                    glyph_id: 3,
                    x_offset: 8,
                    y_offset: -2
                },
            ]
        );
    }

    #[test]
    fn parses_format9_composite_big_metrics() {
        let mut data = header();
        let payload_off = data.len() as u32;
        // Big metrics (8B): h=8, w=8, hbx=0, hby=8, hadv=8, vbx=0, vby=0, vadv=0
        data.extend_from_slice(&[8, 8, 0, 8, 8, 0, 0, 0]);
        // numComponents = 1
        data.extend_from_slice(&1u16.to_be_bytes());
        // Component: glyph_id=42, x_offset=-1, y_offset=3
        data.extend_from_slice(&42u16.to_be_bytes());
        #[allow(clippy::cast_sign_loss)]
        data.push(-1i8 as u8);
        data.push(3);
        let payload_len = data.len() as u32 - payload_off;

        let ebdt = Ebdt::parse(&data).unwrap();
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 9,
            metrics: None,
        };
        let bm = ebdt.glyph_bitmap(&loc).unwrap();
        assert!(bm.is_composite());
        assert_eq!(bm.image_format, 9);
        assert_eq!(bm.component_count(), 1);
        let c = bm.components().next().unwrap();
        assert_eq!(c.glyph_id, 42);
        assert_eq!(c.x_offset, -1);
        assert_eq!(c.y_offset, 3);
    }

    #[test]
    fn format8_truncated_component_array_rejected() {
        let mut data = header();
        let payload_off = data.len() as u32;
        // Small metrics + pad
        data.extend_from_slice(&[4, 4, 0, 4, 4, 0]);
        // numComponents = 4 but only 1 record present (4 bytes).
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(&[0, 1, 0, 0]);
        let payload_len = data.len() as u32 - payload_off;

        let ebdt = Ebdt::parse(&data).unwrap();
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 8,
            metrics: None,
        };
        assert!(matches!(
            ebdt.glyph_bitmap(&loc),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn format8_zero_components_round_trips() {
        let mut data = header();
        let payload_off = data.len() as u32;
        data.extend_from_slice(&[2, 2, 0, 2, 2, 0]);
        data.extend_from_slice(&0u16.to_be_bytes());
        let payload_len = data.len() as u32 - payload_off;

        let ebdt = Ebdt::parse(&data).unwrap();
        let loc = CbdtLocation {
            offset: payload_off,
            length: payload_len,
            image_format: 8,
            metrics: None,
        };
        let bm = ebdt.glyph_bitmap(&loc).unwrap();
        assert_eq!(bm.component_count(), 0);
        assert!(bm.components().next().is_none());
    }
}
