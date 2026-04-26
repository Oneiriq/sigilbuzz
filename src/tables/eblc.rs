//! `EBLC` — Embedded Bitmap Location (Microsoft, monochrome).
//!
//! `EBLC` is the older, monochrome cousin of [`Cblc`](crate::tables::cblc::Cblc):
//! same on-disk layout (BitmapSize records, IndexSubTableArray, index
//! sub-table formats 1-5), only the expected payload format codes
//! differ — `EBDT` carries 1-bit (and historically 2/4/8-bit) mask
//! data, while `CBDT` carries PNG. The shared layout is why `EBLC`
//! is implemented as a tag-only newtype around the `CBLC` parser:
//! every byte is in the same place, so reusing the byte-level walk is
//! both correct and keeps the two parsers in lock-step if either
//! moves.
//!
//! sigilbuzz handles EBLC with the same depth as CBLC: locate the
//! best strike for a glyph at a target ppem, return a
//! [`CbdtLocation`](crate::tables::cblc::CbdtLocation) record that
//! [`Ebdt`](crate::tables::ebdt::Ebdt) decodes against. The
//! `image_format` field in the location record is what distinguishes
//! mono masks (`1..=9`) from CBDT's PNG payloads (`17..=19`); the
//! data table is responsible for honouring the format dispatch.

use crate::error::Result;
use crate::tables::cblc::{BitmapSize, CbdtLocation, Cblc};

/// Parsed `EBLC` table. Structurally identical to `CBLC` — same
/// header, same BitmapSize records, same index sub-table formats.
/// Wraps the shared parser so the two stay byte-identical.
#[derive(Debug, Clone, Copy)]
pub struct Eblc<'a> {
    inner: Cblc<'a>,
}

impl<'a> Eblc<'a> {
    /// Parses an `EBLC` table. Accepts both v2 (1-bit/2-bit/4-bit/8-bit
    /// masks; the original Microsoft EBLC) and v3 (rare in the wild;
    /// shipped alongside CBLC v3 in some Google releases).
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        Ok(Self {
            inner: Cblc::parse(data)?,
        })
    }

    /// Total number of strikes (BitmapSize records).
    #[must_use]
    pub fn num_sizes(&self) -> u32 {
        self.inner.num_sizes()
    }

    /// Returns the BitmapSize at index `i`, or `None` if out of range.
    #[must_use]
    pub fn size(&self, i: u32) -> Option<BitmapSize> {
        self.inner.size(i)
    }

    /// Iterates every BitmapSize in directory order.
    pub fn sizes(&self) -> impl Iterator<Item = BitmapSize> + '_ {
        self.inner.sizes()
    }

    /// Picks the strike whose `ppem_y` is closest to `target_ppem`.
    /// Ties prefer the larger size; strikes that don't cover
    /// `glyph_id` are skipped. See [`Cblc::best_strike`] for the full
    /// rationale — the mono case applies the same selection policy.
    #[must_use]
    pub fn best_strike(&self, glyph_id: u16, target_ppem: u16) -> Option<BitmapSize> {
        self.inner.best_strike(glyph_id, target_ppem)
    }

    /// Resolves `glyph_id` inside `size` to a CBDT-style location
    /// record. The `image_format` on the result identifies the EBDT
    /// payload shape (1, 2, 5 for 1bpp masks; 6, 7 for 8bpp; etc.).
    pub fn locate(&self, size: &BitmapSize, glyph_id: u16) -> Result<Option<CbdtLocation>> {
        self.inner.locate(size, glyph_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn parses_minimal_eblc_with_one_strike() {
        // Reuse the same blob shape as CBLC; only the table tag at the
        // SFNT level distinguishes them.
        let mut blob = Vec::new();
        blob.extend_from_slice(&2u16.to_be_bytes()); // major (v2 EBLC)
        blob.extend_from_slice(&0u16.to_be_bytes()); // minor
        blob.extend_from_slice(&1u32.to_be_bytes()); // numSizes

        // BitmapSize record (48 B)
        blob.extend_from_slice(&56u32.to_be_bytes()); // indexSubTableArrayOffset
        blob.extend_from_slice(&16u32.to_be_bytes()); // indexTablesSize
        blob.extend_from_slice(&1u32.to_be_bytes()); // numberOfIndexSubTables
        blob.extend_from_slice(&0u32.to_be_bytes()); // colorRef
        blob.extend_from_slice(&[0u8; 12]); // hori metrics
        blob.extend_from_slice(&[0u8; 12]); // vert metrics
        blob.extend_from_slice(&1u16.to_be_bytes()); // startGlyphIndex
        blob.extend_from_slice(&1u16.to_be_bytes()); // endGlyphIndex
        blob.push(16); // ppemX
        blob.push(16); // ppemY
        blob.push(1); // bitDepth (mono)
        blob.push(0x01); // flags

        // IndexSubTableArray: one entry pointing at +8
        blob.extend_from_slice(&1u16.to_be_bytes()); // firstGlyphIndex
        blob.extend_from_slice(&1u16.to_be_bytes()); // lastGlyphIndex
        blob.extend_from_slice(&8u32.to_be_bytes()); // additional offset
        // IndexSubTable header: format 1, image format 1, image data offset 0
        blob.extend_from_slice(&1u16.to_be_bytes());
        blob.extend_from_slice(&1u16.to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes());

        let eblc = Eblc::parse(&blob).unwrap();
        assert_eq!(eblc.num_sizes(), 1);
        let size = eblc.size(0).unwrap();
        assert_eq!(size.ppem_y, 16);
        assert_eq!(size.bit_depth, 1);
    }
}
