//! `avar` subsetting — pass-through.
//!
//! The Axis Variations table refines `fvar`'s normalized
//! coordinates with per-axis piecewise-linear remaps. Each remap is
//! axis-keyed, never glyph-keyed, so subsetting the glyph set leaves
//! `avar` untouched. The bytes are copied verbatim.
//!
//! `avar` is optional even in variable fonts: it appears only when
//! the designer wants non-linear axis interpolation. When the source
//! font carries no `avar`, the subset has no `avar` either.
//!
//! Like `fvar`, this table is dropped by the 0.5.0 baseline and
//! retained whenever [`crate::SubsetInput::retain_variations`] is
//! true (the default).

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::SubsetError;

/// Returns the source `avar` bytes verbatim, or `None` when the
/// source font has no `avar` table.
pub(crate) fn subset_avar(face: &Face<'_>) -> Result<Option<Vec<u8>>, SubsetError> {
    match face.table_bytes(tag::AVAR) {
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
    fn avar_passthrough_byte_identical_when_present() {
        let face = rubik_face();
        match face.table_bytes(tag::AVAR) {
            Ok(original) => {
                let out = subset_avar(&face).unwrap().expect("rubik has avar");
                assert_eq!(out, original);
            }
            Err(_) => {
                // Rubik may or may not carry avar; if it doesn't we
                // exercise the absence path here too.
                assert!(subset_avar(&face).unwrap().is_none());
            }
        }
    }
}
