//! The public shaping entry point.
//!
//! This is the seam where every previous abstraction meets. Current
//! state: the pipeline exists, accepts real inputs, and validates that
//! the font at least has a `cmap` table — but codepoint → glyph
//! resolution, advance lookup, and positioning are still stubs that
//! return an empty [`ShapedRun`].
//!
//! The shape of this function is stable. Implementation lands in
//! follow-up commits that flesh out the table parsers the stub depends
//! on.

use crate::buffer::{Buffer, ShapedRun};
use crate::error::Result;
use crate::font::Font;
use crate::tables::tag;

/// One entry in a feature list passed to [`shape`]. The tag is a
/// four-byte OpenType feature tag (e.g. `b"liga"`, `b"kern"`, `b"smcp"`);
/// the value is interpreted per-feature — typically `0` disables and
/// any non-zero value enables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    /// Four-byte feature tag.
    pub tag: [u8; 4],
    /// Feature value. Zero means off; non-zero means on (or a
    /// feature-specific selector for alternates).
    pub value: u32,
}

/// Shapes `buffer` against `font` with optional feature overrides.
///
/// Feature tags passed here layer on top of whatever the font's
/// `GSUB` / `GPOS` default list prescribes. An empty feature slice
/// selects the font's defaults.
///
/// # Errors
///
/// Returns an error if the font is missing tables required to perform
/// shaping (today: `cmap`).
pub fn shape(font: &Font<'_>, buffer: &Buffer, features: &[Feature]) -> Result<ShapedRun> {
    // Today: only check that the bare minimum table exists. The full
    // path (codepoint → glyph via cmap, glyph → advance via hmtx,
    // GSUB / GPOS passes) lands in follow-up commits. The `buffer` and
    // `features` arguments are accepted and stored for that future
    // body; silence unused-variable warnings without renaming the
    // parameters, because the names are part of the public doc example.
    let _ = (buffer, features);

    let _cmap_bytes = font.face().table_bytes(tag::CMAP)?;

    Ok(ShapedRun::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::Blob;
    use crate::face::Face;
    use alloc::vec::Vec;

    fn face_with_cmap() -> Vec<u8> {
        let mut bytes = Vec::new();
        // SFNT header
        bytes.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes()); // numTables
        bytes.extend_from_slice(&[0; 6]);
        // Table record for cmap — table body starts right after the
        // 28-byte header + record section (12 + 16 = 28).
        bytes.extend_from_slice(b"cmap");
        bytes.extend_from_slice(&0u32.to_be_bytes()); // checksum
        bytes.extend_from_slice(&28u32.to_be_bytes()); // offset
        bytes.extend_from_slice(&2u32.to_be_bytes()); // length
                                                      // Table body
        bytes.extend_from_slice(&[0xAB, 0xCD]);
        bytes
    }

    fn face_without_cmap() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes
    }

    #[test]
    fn shape_succeeds_on_empty_input_with_valid_font() {
        let data = face_with_cmap();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let buffer = Buffer::new();

        let shaped = shape(&font, &buffer, &[]).unwrap();
        assert!(shaped.is_empty());
    }

    #[test]
    fn shape_fails_when_cmap_is_missing() {
        let data = face_without_cmap();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let buffer = Buffer::new();

        let err = shape(&font, &buffer, &[]).unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::MissingTable { tag } if tag == *b"cmap"
        ));
    }
}
