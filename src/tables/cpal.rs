//! `CPAL` — Colour Palette table.
//!
//! Paired with `COLR`; provides the palettes of BGRA colour values the
//! `COLR` paint tree indexes into. Fonts can ship multiple palettes
//! (typically light-mode / dark-mode variants or themed sets); every
//! palette has the same number of entries. Palette index `0xFFFF` is
//! a reserved sentinel meaning "use the foreground text colour"; the
//! renderer handles that at paint time.
//!
//! Layout (version 0):
//!
//! ```text
//!   u16   version           (0 or 1)
//!   u16   numPaletteEntries
//!   u16   numPalettes
//!   u16   numColorRecords       = numPaletteEntries * numPalettes
//!   Offset32 colorRecordsArrayOffset
//!   u16   colorRecordIndices[numPalettes]
//!   ColorRecord[numColorRecords] {
//!       u8 blue, u8 green, u8 red, u8 alpha
//!   }
//! ```
//!
//! Version 1 appends palette-type / label / entry-label tables for
//! metadata (light vs. dark, foreground index). sigilbuzz reads the
//! v1 appendix lazily because renderers rarely need it.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// A single palette entry — stored BGRA per spec, but exposed as
/// RGBA for friendlier consumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    /// Red channel, 0..=255.
    pub r: u8,
    /// Green channel, 0..=255.
    pub g: u8,
    /// Blue channel, 0..=255.
    pub b: u8,
    /// Alpha channel. `0` is fully transparent, `255` is opaque.
    pub a: u8,
}

/// Palette-type flags from CPAL v1. Bit flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct PaletteType(u32);

impl PaletteType {
    /// True if the palette is suited to display on light backgrounds.
    #[must_use]
    pub const fn usable_with_light_background(self) -> bool {
        (self.0 & 0x0001) != 0
    }

    /// True if the palette is suited to display on dark backgrounds.
    #[must_use]
    pub const fn usable_with_dark_background(self) -> bool {
        (self.0 & 0x0002) != 0
    }

    /// Raw flag word.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

/// Parsed `CPAL`.
#[derive(Debug, Clone, Copy)]
pub struct Cpal<'a> {
    num_palette_entries: u16,
    num_palettes: u16,
    /// `colorRecordIndices[palette_index]` points at the first colour
    /// record in the palette; `num_palette_entries` follow.
    color_record_indices: &'a [u8],
    /// Entire colour-record pool. Each record is four bytes (BGRA).
    color_records: &'a [u8],
    /// V1 extension: optional palette type flags array.
    palette_types: Option<&'a [u8]>,
}

impl<'a> Cpal<'a> {
    /// Parses a `CPAL` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 0 && version != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported CPAL version",
            });
        }
        let num_palette_entries = r.read_u16()?;
        let num_palettes = r.read_u16()?;
        let num_color_records = r.read_u16()?;
        let color_records_off = r.read_u32()? as usize;

        if num_palette_entries == 0 || num_palettes == 0 {
            return Err(Error::Malformed {
                offset: 2,
                context: "CPAL must carry at least one palette",
            });
        }

        let indices_start = r.position();
        let indices_bytes = num_palettes as usize * 2;
        let indices_end = indices_start
            .checked_add(indices_bytes)
            .ok_or(Error::Malformed {
                offset: indices_start,
                context: "CPAL palette index array overflow",
            })?;
        if indices_end > data.len() {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "CPAL palette indices truncated",
            });
        }
        let color_record_indices = &data[indices_start..indices_end];

        let records_start = color_records_off;
        let records_bytes = num_color_records as usize * 4;
        let records_end = records_start
            .checked_add(records_bytes)
            .ok_or(Error::Malformed {
                offset: records_start,
                context: "CPAL color records overflow",
            })?;
        if records_end > data.len() {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "CPAL color records truncated",
            });
        }
        let color_records = &data[records_start..records_end];

        // v1 appendix carries offsets to palette-type / label arrays.
        // The three offsets can be 0 (absent). We read paletteTypesArray
        // into a slice; the label arrays stay raw because they're rarely
        // touched.
        let mut palette_types = None;
        if version == 1 {
            // The appendix sits immediately after colorRecordIndices,
            // but we must position the reader there regardless of where
            // colorRecordsArrayOffset landed us.
            let app_pos = indices_end;
            if app_pos + 12 <= data.len() {
                let pt_off = u32::from_be_bytes([
                    data[app_pos],
                    data[app_pos + 1],
                    data[app_pos + 2],
                    data[app_pos + 3],
                ]) as usize;
                if pt_off != 0 {
                    let need = num_palettes as usize * 4;
                    if let Some(end) = pt_off.checked_add(need) {
                        if end <= data.len() {
                            palette_types = Some(&data[pt_off..end]);
                        }
                    }
                }
            }
        }

        Ok(Self {
            num_palette_entries,
            num_palettes,
            color_record_indices,
            color_records,
            palette_types,
        })
    }

    /// Number of entries in every palette. All palettes share this
    /// count.
    #[must_use]
    pub const fn num_palette_entries(&self) -> u16 {
        self.num_palette_entries
    }

    /// Number of palettes available (usually 1 for simple fonts, 2+
    /// for fonts that ship light-mode / dark-mode variants).
    #[must_use]
    pub const fn num_palettes(&self) -> u16 {
        self.num_palettes
    }

    /// Reads the colour at `(palette_index, entry_index)`. Returns
    /// `None` if either index is out of range.
    #[must_use]
    pub fn color(&self, palette_index: u16, entry_index: u16) -> Option<Color> {
        if palette_index >= self.num_palettes || entry_index >= self.num_palette_entries {
            return None;
        }
        let idx_off = palette_index as usize * 2;
        let base = u16::from_be_bytes([
            self.color_record_indices[idx_off],
            self.color_record_indices[idx_off + 1],
        ]) as usize;
        let rec_idx = base + entry_index as usize;
        let off = rec_idx * 4;
        if off + 4 > self.color_records.len() {
            return None;
        }
        // Stored BGRA; we return RGBA.
        Some(Color {
            b: self.color_records[off],
            g: self.color_records[off + 1],
            r: self.color_records[off + 2],
            a: self.color_records[off + 3],
        })
    }

    /// Palette flags for `palette_index` if the font shipped a v1
    /// palette-types array, else `None`.
    #[must_use]
    pub fn palette_type(&self, palette_index: u16) -> Option<PaletteType> {
        let pt = self.palette_types?;
        if palette_index >= self.num_palettes {
            return None;
        }
        let off = palette_index as usize * 4;
        Some(PaletteType(u32::from_be_bytes([
            pt[off],
            pt[off + 1],
            pt[off + 2],
            pt[off + 3],
        ])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_cpal(
        version: u16,
        entries_per_palette: u16,
        palettes: &[&[(u8, u8, u8, u8)]],
    ) -> Vec<u8> {
        let num_palettes = palettes.len() as u16;
        let num_color_records: u16 = palettes.iter().map(|p| p.len() as u16).sum();
        let mut b = Vec::new();
        b.extend_from_slice(&version.to_be_bytes());
        b.extend_from_slice(&entries_per_palette.to_be_bytes());
        b.extend_from_slice(&num_palettes.to_be_bytes());
        b.extend_from_slice(&num_color_records.to_be_bytes());
        // colorRecordsArrayOffset — computed below; header is 12
        // bytes, then num_palettes * 2 for indices, then optional
        // v1 appendix (12 bytes).
        let header_plus_indices = 12 + num_palettes as usize * 2;
        let records_off = header_plus_indices + if version == 1 { 12 } else { 0 };
        b.extend_from_slice(&(records_off as u32).to_be_bytes());
        // colorRecordIndices[num_palettes].
        let mut cursor: u16 = 0;
        for p in palettes {
            b.extend_from_slice(&cursor.to_be_bytes());
            cursor += p.len() as u16;
        }
        if version == 1 {
            // three Offset32 — all zero (no appendix arrays).
            b.extend_from_slice(&[0u8; 12]);
        }
        for p in palettes {
            for (r, g, bl, a) in *p {
                b.push(*bl);
                b.push(*g);
                b.push(*r);
                b.push(*a);
            }
        }
        b
    }

    #[test]
    fn single_palette_round_trip() {
        let bytes = build_cpal(
            0,
            3,
            &[&[(255, 0, 0, 255), (0, 255, 0, 128), (0, 0, 255, 64)]],
        );
        let c = Cpal::parse(&bytes).unwrap();
        assert_eq!(c.num_palettes(), 1);
        assert_eq!(c.num_palette_entries(), 3);
        assert_eq!(
            c.color(0, 0),
            Some(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            })
        );
        assert_eq!(
            c.color(0, 2),
            Some(Color {
                r: 0,
                g: 0,
                b: 255,
                a: 64,
            })
        );
    }

    #[test]
    fn two_palettes_address_independently() {
        let bytes = build_cpal(
            0,
            2,
            &[
                &[(10, 20, 30, 255), (40, 50, 60, 255)],
                &[(100, 110, 120, 255), (130, 140, 150, 255)],
            ],
        );
        let c = Cpal::parse(&bytes).unwrap();
        assert_eq!(c.num_palettes(), 2);
        assert_eq!(c.color(0, 0).unwrap().r, 10);
        assert_eq!(c.color(1, 0).unwrap().r, 100);
        assert_eq!(c.color(1, 1).unwrap().r, 130);
    }

    #[test]
    fn out_of_range_index_returns_none() {
        let bytes = build_cpal(0, 1, &[&[(0, 0, 0, 255)]]);
        let c = Cpal::parse(&bytes).unwrap();
        assert_eq!(c.color(0, 1), None);
        assert_eq!(c.color(1, 0), None);
    }

    #[test]
    fn rejects_bad_version() {
        let mut bytes = build_cpal(0, 1, &[&[(0, 0, 0, 0)]]);
        bytes[0..2].copy_from_slice(&9u16.to_be_bytes());
        assert!(matches!(Cpal::parse(&bytes), Err(Error::Malformed { .. })));
    }
}
