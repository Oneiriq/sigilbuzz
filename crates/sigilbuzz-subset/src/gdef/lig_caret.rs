//! `LigCaretList` rewriter.
//!
//! ```text
//!   LigCaretList:
//!     Offset16 coverageOffset                   (from LigCaretList)
//!     u16      ligGlyphCount
//!     Offset16 ligGlyphOffsets[ligGlyphCount]   (from LigCaretList)
//!   LigGlyph:
//!     u16      caretCount
//!     Offset16 caretValueOffsets[caretCount]    (from LigGlyph)
//!   CaretValue format 1: u16 format=1, i16 coordinate
//!   CaretValue format 2: u16 format=2, u16 caretValuePointIndex
//!   CaretValue format 3: u16 format=3, i16 coordinate,
//!                        Offset16 deviceOffset  (from CaretValue)
//! ```
//!
//! Caret values carry no glyph ids, so each kept ligature's LigGlyph is
//! rebuilt from copies of its CaretValues. A format 3 caret's Device or
//! VariationIndex table is measured from the CaretValue itself, so it
//! is copied right behind the caret and the offset re-pointed at it.

use alloc::vec::Vec;

use sigilbuzz::Error;

use super::read::{slice_at, u16_at};
use super::{emit_covered_list, kept_entries, offset16, StorePlan};
use crate::device::{read_device, Dedup, VARIATION_INDEX_FORMAT};
use crate::layout::GidMap;
use crate::warnings::Diag;
use crate::SubsetError;

const CTX: &str = "GDEF LigGlyph or CaretValue truncated";

/// Rewrites the LigCaretList at `off` (from the GDEF start). Returns
/// `None` when no covered ligature survives. Format 3 carets handle
/// their VariationIndex tables per `plan` (see [`StorePlan`]); hinting
/// Device tables always stay. A ligature whose LigGlyph cannot be read
/// loses its carets, and a caret Device table that cannot be read is
/// cleared; both are reported through `diag`.
pub(super) fn rewrite(
    table: &[u8],
    off: usize,
    map: &GidMap,
    plan: StorePlan<'_>,
    diag: &Diag<'_>,
) -> Result<Option<Vec<u8>>, SubsetError> {
    let mut entries = Vec::new();
    for (new_gid, lig_glyph) in kept_entries(table, off, map, "GDEF LigCaretList", diag)? {
        // Many ligatures can share one LigGlyph, so its carets are
        // charged on every visit.
        let carets = u16_at(table, lig_glyph, CTX).map_or(0, usize::from);
        if !map.spend(1 + carets) {
            return Err(super::out_of_budget(off).into());
        }
        match rewrite_lig_glyph(table, lig_glyph, plan, diag) {
            Ok(body) => entries.push((new_gid, body)),
            Err(SubsetError::Parse(e)) => diag.error(&e, "one ligature's carets"),
            Err(overflow) => return Err(overflow),
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    emit_covered_list(&entries).map(Some)
}

/// Rebuilds the LigGlyph at `pos`: the caret count, one offset per
/// caret, then the caret copies (identical carets share one copy).
pub(super) fn rewrite_lig_glyph(
    table: &[u8],
    pos: usize,
    plan: StorePlan<'_>,
    diag: &Diag<'_>,
) -> Result<Vec<u8>, SubsetError> {
    let count = usize::from(u16_at(table, pos, CTX)?);
    let mut out = Vec::with_capacity(2 + count * 6);
    out.extend_from_slice(&(count as u16).to_be_bytes());
    out.resize(2 + count * 2, 0);
    let mut carets = Dedup::default();
    for i in 0..count {
        let slot = pos + 2 + i * 2;
        let rel = usize::from(u16_at(table, slot, CTX)?);
        if rel == 0 {
            return Err(Error::Malformed {
                offset: slot,
                context: "GDEF LigGlyph has a null CaretValue offset",
            }
            .into());
        }
        let caret = copy_caret_value(table, pos + rel, plan, diag)?;
        let at = offset16(carets.place(&mut out, &caret))?;
        out[2 + i * 2..4 + i * 2].copy_from_slice(&at.to_be_bytes());
    }
    Ok(out)
}

/// Copies the CaretValue at `pos` into a standalone blob. Format 3
/// brings its device table along at blob offset 6, a VariationIndex
/// one handled per `plan`; a device table that is missing, malformed
/// (reported through `diag`), or a VariationIndex the plan drops
/// clears the offset instead.
fn copy_caret_value(
    table: &[u8],
    pos: usize,
    plan: StorePlan<'_>,
    diag: &Diag<'_>,
) -> Result<Vec<u8>, Error> {
    match u16_at(table, pos, CTX)? {
        1 | 2 => Ok(slice_at(table, pos, 4, CTX)?.to_vec()),
        3 => {
            let mut out = slice_at(table, pos, 6, CTX)?.to_vec();
            let rel = usize::from(u16_at(table, pos + 4, CTX)?);
            let device = read_device(table, if rel == 0 { 0 } else { pos + rel })
                .unwrap_or_else(|e| {
                    diag.error(&e, "a caret's Device table");
                    None
                })
                .and_then(|t| variation_index_per_plan(t, plan));
            match device {
                Some(t) => {
                    out[4..6].copy_from_slice(&6u16.to_be_bytes());
                    out.extend_from_slice(&t);
                }
                None => out[4..6].copy_from_slice(&0u16.to_be_bytes()),
            }
            Ok(out)
        }
        _ => Err(Error::Malformed {
            offset: pos,
            context: "unsupported GDEF CaretValue format",
        }),
    }
}

/// The device table a caret copy carries: a hinting Device as is, a
/// VariationIndex kept, renumbered or dropped per `plan`.
fn variation_index_per_plan(table: &[u8], plan: StorePlan<'_>) -> Option<Vec<u8>> {
    if u16_at(table, 4, CTX) != Ok(VARIATION_INDEX_FORMAT) {
        return Some(table.to_vec());
    }
    match plan {
        StorePlan::Keep => Some(table.to_vec()),
        StorePlan::Drop => None,
        StorePlan::Replace { remap, .. } => {
            let (outer, inner) = remap(u16_at(table, 0, CTX).ok()?, u16_at(table, 2, CTX).ok()?)?;
            let mut out = table.to_vec();
            out[0..2].copy_from_slice(&outer.to_be_bytes());
            out[2..4].copy_from_slice(&inner.to_be_bytes());
            Some(out)
        }
    }
}
