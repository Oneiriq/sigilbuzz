//! `fvar` subsetting — pass-through.
//!
//! The Font Variations table describes the *axis space* of a variable
//! font: tag, min/default/max user-space coordinates, and the optional
//! list of named instances (Regular, Bold, Condensed, ...). None of
//! those records are keyed by glyph id, so subsetting the glyph set
//! has no bearing on `fvar` content. The bytes are copied verbatim.
//!
//! We retain the table for two reasons:
//!
//! 1. Without `fvar` the resulting font is a *static* font: shapers
//!    will refuse to honour axis coordinates passed by the caller and
//!    `Face::fvar()` will return `None`. A subset of a variable font
//!    that drops `fvar` is no longer a variable font.
//! 2. The `name` table — which we already pass through — references
//!    `fvar.axisNameID` and `fvar.instanceSubfamilyNameID`. Those name
//!    records would become orphaned strings if `fvar` disappeared.
//!
//! The 0.5.0 baseline dropped `fvar` along with every other variable-
//! font table. Callers who want that behaviour back set
//! [`crate::SubsetInput::retain_variations`] to `false`.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::SubsetError;

/// Returns the source `fvar` bytes verbatim, or `None` when the
/// source font has no `fvar` table (static font).
pub(crate) fn subset_fvar(face: &Face<'_>) -> Result<Option<Vec<u8>>, SubsetError> {
    match face.table_bytes(tag::FVAR) {
        Ok(bytes) => Ok(Some(bytes.to_vec())),
        Err(sigilbuzz::Error::MissingTable { .. }) => Ok(None),
        Err(e) => Err(SubsetError::from(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

    fn rubik_face() -> Face<'static> {
        Face::parse_bytes(RUBIK, 0).unwrap()
    }

    #[test]
    fn fvar_passthrough_is_byte_identical() {
        let face = rubik_face();
        let original = face.table_bytes(tag::FVAR).unwrap().to_vec();
        let out = subset_fvar(&face).unwrap().expect("rubik has fvar");
        assert_eq!(out, original);
    }

    #[test]
    fn fvar_missing_returns_none() {
        // Open Sans is a static font with no fvar.
        const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
        let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
        assert!(subset_fvar(&face).unwrap().is_none());
    }
}
