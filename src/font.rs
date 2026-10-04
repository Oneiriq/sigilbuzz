//! Font: a [`Face`] scaled to a particular size.
//!
//! Metrics queries go through `Font` rather than `Face` because they
//! inherently depend on a point / pixel size. Beyond size, a `Font`
//! may also carry a set of *normalized* variation-axis coordinates,
//! one `f32` per axis in `[-1.0, 1.0]`, which the shaper uses to
//! pick the right advance widths (via HVAR) and, on the road map,
//! outline deltas (via gvar).
//!
//! Callers usually start from user-space axis values (e.g. `wght = 700`,
//! `wdth = 80`), which [`Font::with_variations`] takes, as HarfBuzz's
//! `hb_font_set_variations` does:
//!
//! ```text
//!   let font = Font::new(face, 16.0).with_variations(&[(*b"wght", 700.0), (*b"wdth", 80.0)]);
//! ```
//!
//! It runs the steps of HarfBuzz's `hb_ot_var_normalize_coords`:
//! [`crate::tables::Fvar::normalize_coords`] rounds each coord to 16.16
//! fixed point and [`crate::tables::Avar::remap_all`] maps it through
//! `avar` and rounds it to 16.16 again. Shaping then rounds every coord
//! to F2DOT14 (a multiple of 1/16384, halves up), the precision
//! HarfBuzz stores coords in, so the same design-space values give the
//! same instance as in HarfBuzz. A caller that already has normalized
//! coords binds them with [`Font::with_coords`], which borrows them for
//! the `Font`'s lifetime.
//!
//! A `Font` with no coords behaves identically to the static default
//! instance, and so does one whose coords all round to zero.

mod cache;

use alloc::sync::Arc;
use alloc::vec::Vec;

pub(crate) use cache::{FontCaches, GlyphValue, InstanceCache, Known};

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
///
/// A `Font` keeps lookup accelerators and glyph metrics between shaping
/// calls, built from its second call on, so a caller that shapes many
/// runs with one font should keep the `Font` rather than build one per
/// run. It is `Send + Sync`: threads may share one font, and its clones
/// share what it has built.
#[derive(Debug, Clone)]
pub struct Font<'a> {
    face: Face<'a>,
    size: f32,
    coords: Coords<'a>,
    /// Lookup accelerators and metrics kept between shaping calls.
    caches: FontCaches,
}

/// A font's normalized coords: the caller's, or computed from
/// user-space values by [`Font::with_variations`].
#[derive(Debug, Clone)]
enum Coords<'a> {
    Borrowed(&'a [f32]),
    Owned(Arc<[f32]>),
}

impl Coords<'_> {
    fn as_slice(&self) -> &[f32] {
        match self {
            Self::Borrowed(coords) => coords,
            Self::Owned(coords) => coords,
        }
    }
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
            coords: Coords::Borrowed(&[]),
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
    ///
    /// Since 0.24.0 the slice borrows the font rather than living for
    /// `'a`: a font built with [`Font::with_variations`] owns its
    /// coords.
    #[must_use]
    pub fn coords(&self) -> &[f32] {
        self.coords.as_slice()
    }

    /// Returns a new `Font` at a different size, sharing the same
    /// face and coords.
    #[must_use]
    pub fn with_size(&self, size: f32) -> Self {
        Self {
            face: self.face.clone(),
            size,
            coords: self.coords.clone(),
            caches: self.caches.clone(),
        }
    }

    /// Returns a new `Font` bound to the supplied normalized variation
    /// coords. Each entry corresponds to one axis from the face's
    /// `fvar` table, in file order, in `[-1.0, 1.0]`. Shaping rounds
    /// each coord to F2DOT14 (a multiple of 1/16384, halves up), as
    /// HarfBuzz stores coords; [`Font::coords`] returns them as given.
    ///
    /// **User-space values are not accepted here.** For raw
    /// design-space values (e.g. `wght = 700.0`) use
    /// [`Font::with_variations`]. Passing raw user-space values to this
    /// method silently yields wrong deltas because the ItemVariationStore
    /// only reads normalized input.
    #[must_use]
    pub fn with_coords(self, coords: &'a [f32]) -> Self {
        Self {
            caches: self.caches.for_other_coords(),
            face: self.face,
            size: self.size,
            coords: Coords::Borrowed(coords),
        }
    }

    /// Returns a new `Font` at the instance user-space axis values
    /// `variations` select, as HarfBuzz's `hb_font_set_variations`
    /// does: every axis starts at its `fvar` default, each `(tag,
    /// value)` sets every axis with that tag (a later entry wins), and
    /// the values are normalized through `fvar` and `avar` with
    /// HarfBuzz's rounding (see the module docs). Tags the font has no
    /// axis for are ignored, values outside an axis's range are
    /// clamped to it, and an empty list selects the default instance.
    ///
    /// A face without `fvar`, or whose `fvar` does not parse, has no
    /// axes, so the font is its default instance. An `avar` that does
    /// not parse is left out. HarfBuzz's sanitizer drops both tables
    /// in those cases.
    ///
    /// ```
    /// use sigilbuzz::{Face, Font};
    ///
    /// let data = include_bytes!("../tests/fixtures/rubik_vf.ttf");
    /// let face = Face::parse_bytes(data, 0)?;
    /// let font = Font::new(face.clone(), 16.0).with_variations(&[(*b"wght", 650.0)]);
    ///
    /// // The same coords, normalized by hand.
    /// let fvar = face.fvar()?.expect("Rubik is variable");
    /// let user: Vec<f32> = fvar
    ///     .axes()
    ///     .iter()
    ///     .map(|a| if &a.tag == b"wght" { 650.0 } else { a.default_value })
    ///     .collect();
    /// let normalized = fvar.normalize_coords(&user);
    /// let normalized = match face.avar()? {
    ///     Some(avar) => avar.remap_all(&normalized),
    ///     None => normalized,
    /// };
    /// assert_eq!(font.coords(), normalized.as_slice());
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    #[must_use]
    pub fn with_variations(self, variations: &[([u8; 4], f32)]) -> Self {
        let coords: Vec<f32> = match self.face.fvar() {
            Ok(Some(fvar)) => {
                let axes = fvar.axes();
                let mut user: Vec<f32> = axes.iter().map(|a| a.default_value).collect();
                for &(tag, value) in variations {
                    for (axis, slot) in axes.iter().zip(user.iter_mut()) {
                        if axis.tag == tag {
                            *slot = value;
                        }
                    }
                }
                let normalized = fvar.normalize_coords(&user);
                match self.face.avar() {
                    Ok(Some(avar)) => avar.remap_all(&normalized),
                    _ => normalized,
                }
            }
            _ => Vec::new(),
        };
        Self {
            caches: self.caches.for_other_coords(),
            face: self.face,
            size: self.size,
            coords: Coords::Owned(coords.into()),
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

    /// The coords `with_variations` should give `face` for `user`, one
    /// user value per axis, normalized by hand.
    fn by_hand(face: &Face<'_>, user: &[f32]) -> Vec<f32> {
        let normalized = face.fvar().unwrap().unwrap().normalize_coords(user);
        match face.avar().unwrap() {
            Some(avar) => avar.remap_all(&normalized),
            None => normalized,
        }
    }

    #[test]
    fn variations_normalize_like_harfbuzz() {
        let data = include_bytes!("../tests/fixtures/rubik_vf.ttf");
        let face = Face::parse_bytes(data, 0).unwrap();
        let axis = face.fvar().unwrap().unwrap().axes()[0];
        assert_eq!(&axis.tag, b"wght");
        let font = |v: &[([u8; 4], f32)]| Font::new(face.clone(), 16.0).with_variations(v);
        assert_eq!(
            font(&[(*b"wght", 650.0)]).coords(),
            by_hand(&face, &[650.0])
        );
        // A later entry for the same tag wins, unknown tags are ignored,
        // and values past the axis range are clamped to it.
        let later = font(&[(*b"wght", 300.0), (*b"XXXX", 5.0), (*b"wght", 650.0)]);
        assert_eq!(later.coords(), by_hand(&face, &[650.0]));
        let clamped = font(&[(*b"wght", 5000.0)]);
        assert_eq!(clamped.coords(), by_hand(&face, &[axis.max_value]));
        assert_eq!(clamped.coords(), [1.0]);
        // No variations is the default instance.
        assert_eq!(font(&[]).coords(), [0.0]);
        assert!(f2dot14_coords(font(&[]).coords()).is_empty());
        // The coords survive a size change, and with_coords replaces them.
        let resized = font(&[(*b"wght", 650.0)]).with_size(32.0);
        assert_eq!(resized.coords(), by_hand(&face, &[650.0]));
        let rebound = resized.with_coords(&[0.25]);
        assert_eq!(rebound.coords(), [0.25]);
    }

    #[test]
    fn variations_on_a_static_face_select_the_default_instance() {
        let data = minimal_face_bytes();
        let face = Face::parse_bytes(&data, 0).unwrap();
        let font = Font::new(face, 16.0).with_variations(&[(*b"wght", 700.0)]);
        assert!(font.coords().is_empty());
    }
}
