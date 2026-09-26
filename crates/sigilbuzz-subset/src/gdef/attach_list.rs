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

use super::read::{slice_at, u16_at};
use super::{emit_covered_list, kept_entries};
use crate::layout::GidMap;
use crate::SubsetError;

/// Rewrites the AttachList at `off` (from the GDEF start). Returns
/// `None` when no covered glyph survives.
pub(super) fn rewrite(
    table: &[u8],
    off: usize,
    map: &GidMap,
) -> Result<Option<Vec<u8>>, SubsetError> {
    const CTX: &str = "GDEF AttachPoint truncated";
    let mut entries = Vec::new();
    for (new_gid, point_off) in kept_entries(table, off, map, "GDEF AttachList")? {
        let count = usize::from(u16_at(table, point_off, CTX)?);
        let body = slice_at(table, point_off, 2 + count * 2, CTX)?;
        entries.push((new_gid, body.to_vec()));
    }
    if entries.is_empty() {
        return Ok(None);
    }
    emit_covered_list(&entries).map(Some)
}
