//! Font: a [`Face`] scaled to a particular size.
//!
//! Metrics queries go through `Font` rather than `Face` because they
//! inherently depend on a point / pixel size. Beyond size, a `Font`
//! may also carry a set of *normalized* variation-axis coordinates,
//! one `f32` per axis in `[-1.0, 1.0]`, which the shaper uses to
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
//! These are the steps of HarfBuzz's `hb_ot_var_normalize_coords`:
//! `normalize_coords` rounds each coord to 16.16 fixed point and
//! `remap_all` maps it through `avar` and rounds it to 16.16 again.
//! Shaping then rounds every coord to F2DOT14 (a multiple of 1/16384,
//! halves up), the precision HarfBuzz stores coords in, so the same
//! design-space values give the same instance as in HarfBuzz.
//!
//! The coord slice is borrowed for the `Font`'s lifetime, so callers
//! own the storage. A `Font` with an empty coord slice behaves
//! identically to the static default instance, and so does one whose
//! coords all round to zero.

mod cache;

use alloc::vec::Vec;

pub(crate) use cache::FontCaches;

use crate::error::Result;
use crate::face::Face;
use crate::tables::parse::hb_round_to;

/// `coords` rounded to F2DOT14 as HarfBuzz stores a font's coords: each
/// a multiple of 1/16384, rounded halves up, with NaN read as zero.
/// Empty when every coordinate rounds to zero, the default instance.
/// Shaping and the face's outline and bounds methods read coords
/// through this, so they agree with each other and with HarfBuzz.
///
/// For a coordinate from [`crate::tables::Fvar::normalize_coords`] and
/// [`crate::tables::Avar::remap_all`], a multiple `k / 65536`, this is
/// HarfBuzz's `(k + 2) >> 2`.
pub(crate) fn f2dot14_coords(coords: &[f32]) -> Vec<f32> {
    let rounded: Vec<f32> = coords
        .iter()
        .map(|&c| {
            if c.is_nan() {
                0.0
            } else {
                hb_round_to(c, 16384.0)
            }
        })
        .collect();
    if rounded.iter().all(|&c| c == 0.0) {
        return Vec::new();
    }
    rounded
}

/// A face bound to a render size (and optional variation coords).
#[derive(Debug, Clone)]
pub struct Font<'a> {
    face: Face<'a>,
    size: f32,
    coords: &'a [f32],
    /// Lookup accelerators and metrics kept between shaping calls.
    caches: FontCaches,
}

impl<'a> Font<'a> {
    /// Creates a new font from a parsed face and a size.
    ///
    /// `size` is interpreted as a floating-point value in whatever
    /// units the caller chooses: sigilbuzz scales by it and otherwise
    /// does not care. Pixels are the conventional choice. The new
    /// font starts with no variation coords; see [`Font::with_coords`]
    /// to bind a variable-font instance.
    #[must_use]
    pub const fn new(face: Face<'a>, size: f32) -> Self {
        Self {
            face,
            size,
            coords: &[],
            caches: FontCaches::new(),
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
            caches: self.caches.clone(),
        }
    }

    /// Returns a new `Font` bound to the supplied normalized variation
    /// coords. Each entry corresponds to one axis from the face's
    /// `fvar` table, in file order, in `[-1.0, 1.0]`. Shaping rounds
    /// each coord to F2DOT14 (a multiple of 1/16384, halves up), as
    /// HarfBuzz stores coords; [`Font::coords`] returns them as given.
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
            caches: self.caches.for_other_coords(),
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

    /// The lookup accelerators and metrics the font keeps between
    /// shaping calls (see the `cache` module).
    pub(crate) fn caches(&self) -> &FontCaches {
        &self.caches
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

    #[test]
    fn coords_round_to_f2dot14_like_harfbuzz() {
        // HarfBuzz's `(k + 2) >> 2` for a 16.16 coordinate k / 65536.
        for k in [-65536i32, -39322, -6, -3, -2, 2, 3, 6, 39322, 65536] {
            let got = f2dot14_coords(&[k as f32 / 65536.0, 1.0]);
            assert_eq!(got[0], ((k + 2) >> 2) as f32 / 16384.0, "{k}");
        }
        // Coordinates that all round to zero are the default instance.
        assert!(f2dot14_coords(&[2.0 / 65536.0 - 1e-9, -2.0 / 65536.0]).is_empty());
        assert!(f2dot14_coords(&[f32::NAN, 0.0]).is_empty());
        assert!(f2dot14_coords(&[]).is_empty());
        assert_eq!(f2dot14_coords(&[f32::NAN, 0.5]), [0.0, 0.5]);
    }
}
