//! Font — a [`Face`] scaled to a particular size.
//!
//! Metrics queries go through `Font` rather than `Face` because they
//! inherently depend on a point / pixel size. Beyond size, a `Font`
//! may also carry a set of *normalized* variation-axis coordinates —
//! one `f32` per axis in `[-1.0, 1.0]` — which the shaper uses to
//! pick the right advance widths (via HVAR) and, on the road map,
//! outline deltas (via gvar).
//!
//! Callers start from user-space axis values (e.g. `wght = 700`,
//! `wdth = 80`) and pass them through the `fvar` / `avar` pipeline to
//! get normalized coords:
//!
//! ```text
//!   let fvar = face.fvar()?.unwrap();
//!   let avar = face.avar()?;
//!   let coords = fvar.normalize_coords(&[700.0, 80.0]);
//!   let coords = match avar {
//!       Some(a) => a.remap_all(&coords),
//!       None => coords,
//!   };
//!   let font = Font::new(face, 16.0).with_coords(&coords);
//! ```
//!
//! The coord slice is borrowed for the `Font`'s lifetime, so callers
//! own the storage. A `Font` with an empty coord slice behaves
//! identically to the static default instance.

use crate::error::Result;
use crate::face::Face;

/// A face bound to a render size (and optional variation coords).
#[derive(Debug, Clone)]
pub struct Font<'a> {
    face: Face<'a>,
    size: f32,
    coords: &'a [f32],
}

impl<'a> Font<'a> {
    /// Creates a new font from a parsed face and a size.
    ///
    /// `size` is interpreted as a floating-point value in whatever
    /// units the caller chooses — sigilbuzz scales by it and otherwise
    /// does not care. Pixels are the conventional choice. The new
    /// font starts with no variation coords; see [`Font::with_coords`]
    /// to bind a variable-font instance.
    #[must_use]
    pub const fn new(face: Face<'a>, size: f32) -> Self {
        Self {
            face,
            size,
            coords: &[],
        }
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

    /// Current normalized variation coords. Empty when the font is
    /// bound to its default instance (or when the face is not a
    /// variable font at all).
    #[must_use]
    pub const fn coords(&self) -> &'a [f32] {
        self.coords
    }

    /// Returns a new `Font` at a different size, sharing the same
    /// face and coords.
    #[must_use]
    pub fn with_size(&self, size: f32) -> Self {
        Self {
            face: self.face.clone(),
            size,
            coords: self.coords,
        }
    }

    /// Returns a new `Font` bound to the supplied normalized variation
    /// coords. Each entry corresponds to one axis from the face's
    /// `fvar` table, in file order, in `[-1.0, 1.0]`.
    ///
    /// **User-space values are not accepted here.** If the caller has
    /// raw design-space values (e.g. `wght = 700.0`), they must first
    /// pass them through [`crate::tables::Fvar::normalize_coords`] and
    /// then through [`crate::tables::Avar::remap_all`] when the face
    /// carries an avar table. Passing raw user-space values to this
    /// method silently yields wrong deltas because the ItemVariationStore
    /// only reads normalized input.
    #[must_use]
    pub fn with_coords(self, coords: &'a [f32]) -> Self {
        Self {
            face: self.face,
            size: self.size,
            coords,
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
        assert!(font.coords().is_empty());
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

    #[test]
    fn with_coords_binds_a_variation_instance() {
        let data = minimal_face_bytes();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let coords = [0.5, -0.25];
        let font = Font::new(face, 16.0).with_coords(&coords);
        assert_eq!(font.coords(), &coords[..]);
    }

    #[test]
    fn with_size_preserves_coords() {
        let data = minimal_face_bytes();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let coords = [0.5];
        let font = Font::new(face, 16.0).with_coords(&coords);
        let rescaled = font.with_size(32.0);
        assert_eq!(rescaled.coords(), &coords[..]);
    }
}
