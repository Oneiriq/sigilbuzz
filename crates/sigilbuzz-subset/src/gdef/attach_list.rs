//! `AttachList` rewriter.
//!
//! ```text
//!   AttachList:
//!     Offset16 coverageOffset               (from AttachList)
//!     u16      glyphCount
//!     Offset16 attachPointOffsets[glyphCount] (from AttachList)
//!   AttachPoint:
//!     u16 pointCount
//!     u16 pointIndices[pointCount]
//! ```
//!
//! AttachPoint tables hold contour point indices, which do not change
//! when glyphs are renumbered, so each kept glyph's table is copied
//! whole and listed in the new Coverage order.

use alloc::vec::Vec;

use sigilbuzz::Error;

use super::read::{slice_at, u16_at};
use super::{emit_covered_list, kept_entries};
use crate::layout::GidMap;
use crate::warnings::Diag;
use crate::SubsetError;

/// Rewrites the AttachList at `off` (from the GDEF start). Returns
/// `None` when no covered glyph survives. A glyph whose AttachPoint is
/// truncated loses its entry, reported through `diag`; the others
/// stay.
pub(super) fn rewrite(
    table: &[u8],
    off: usize,
    map: &GidMap,
    diag: &Diag<'_>,
) -> Result<Option<Vec<u8>>, SubsetError> {
    let mut entries = Vec::new();
    for (new_gid, point_off) in kept_entries(table, off, map, "GDEF AttachList", diag)? {
        match attach_point(table, point_off) {
            Ok(body) => entries.push((new_gid, body.to_vec())),
            Err(e) => diag.error(&e, "one glyph's AttachPoint"),
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    emit_covered_list(&entries).map(Some)
}

/// The AttachPoint at `pos`: its point count and indices.
pub(super) fn attach_point(table: &[u8], pos: usize) -> Result<&[u8], Error> {
    const CTX: &str = "GDEF AttachPoint truncated";
    let count = usize::from(u16_at(table, pos, CTX)?);
    slice_at(table, pos, 2 + count * 2, CTX)
}
