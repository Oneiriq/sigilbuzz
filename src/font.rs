//! Font — a [`Face`] scaled to a particular size.
//!
//! Metrics queries go through `Font` rather than `Face` because they
//! inherently depend on a point / pixel size. The current cut carries
//! only what the rest of the shaping pipeline needs: the face and a
//! size in units the caller defines (typically pixels).
//!
//! Subsequent iterations will add units-per-em scaling, variation axis
//! coordinates, synthetic bold / oblique, etc.

use crate::error::Result;
use crate::face::Face;

/// A face bound to a render size.
#[derive(Debug, Clone)]
pub struct Font<'a> {
    face: Face<'a>,
    size: f32,
}

impl<'a> Font<'a> {
    /// Creates a new font from a parsed face and a size.
    ///
    /// `size` is interpreted as a floating-point value in whatever
    /// units the caller chooses — sigilbuzz scales by it and otherwise
    /// does not care. Pixels are the conventional choice.
    #[must_use]
    pub const fn new(face: Face<'a>, size: f32) -> Self {
        Self { face, size }
    }

    /// Borrowed access to the underlying face.
    #[must_use]
    pub const fn face(&self) -> &Face<'a> {
        &self.face
    }

    /// Current size.
    #[must_use]
    pub const fn size(&self) -> f32 {
        self.size
    }

    /// Returns a new `Font` at a different size, sharing the same face.
    #[must_use]
    pub fn with_size(&self, size: f32) -> Self {
        Self {
            face: self.face.clone(),
            size,
        }
    }

    /// Reads `unitsPerEm` from the font's `head` table. Useful when a
    /// caller wants to convert sigilbuzz's design-unit advances into
    /// pixels: `pixels = advance * font.size() / font.units_per_em()?`.
    pub fn units_per_em(&self) -> Result<u16> {
        Ok(self.face.head()?.units_per_em)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Blob;

    fn minimal_face_bytes() -> alloc::vec::Vec<u8> {
        let mut bytes = alloc::vec::Vec::new();
        bytes.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // SFNT TrueType
        bytes.extend_from_slice(&0u16.to_be_bytes()); // numTables
        bytes.extend_from_slice(&[0; 6]);
        bytes
    }

    #[test]
    fn font_records_size_and_preserves_face() {
        let data = minimal_face_bytes();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);

        assert!((font.size() - 16.0).abs() < f32::EPSILON);
        assert_eq!(font.face().num_tables(), 0);
    }

    #[test]
    fn with_size_produces_new_font_at_new_size() {
        let data = minimal_face_bytes();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);

        let big = font.with_size(32.0);
        assert!((big.size() - 32.0).abs() < f32::EPSILON);
        // Original is untouched.
        assert!((font.size() - 16.0).abs() < f32::EPSILON);
    }
}
