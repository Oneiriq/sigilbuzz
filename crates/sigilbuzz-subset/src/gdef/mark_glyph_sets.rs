//! `MarkGlyphSetsDef` rewriter.
//!
//! ```text
//!   MarkGlyphSetsDef:
//!     u16      format = 1
//!     u16      markGlyphSetCount
//!     Offset32 coverageOffsets[markGlyphSetCount]   (from MarkGlyphSetsDef)
//! ```
//!
//! GSUB and GPOS lookups with the `UseMarkFilteringSet` flag name a set
//! by its index, and the lookup rewriters keep that index unchanged.
//! So every set survives in its original slot: its Coverage is remapped
//! through the glyph map, and a set whose glyphs all dropped stays as
//! an empty Coverage rather than shifting the sets after it.

use alloc::vec::Vec;

use sigilbuzz::Error;

use super::read::{coverage, u16_at, u32_at};
use crate::coverage::emit_coverage_from_glyphs;
use crate::device::Dedup;
use crate::layout::GidMap;
use crate::warnings::Diag;

/// A rewritten MarkGlyphSetsDef.
pub(super) struct MarkGlyphSets {
    /// The serialized subtable.
    pub bytes: Vec<u8>,
    /// True when at least one set still lists a glyph.
    pub any_glyphs: bool,
}

/// Rewrites the MarkGlyphSetsDef at `off` (from the GDEF start). A set
/// whose Coverage cannot be read is kept empty and reported through
/// `diag`.
pub(super) fn rewrite(
    table: &[u8],
    off: usize,
    map: &GidMap,
    diag: &Diag<'_>,
) -> Result<MarkGlyphSets, Error> {
    const CTX: &str = "GDEF MarkGlyphSetsDef truncated";
    let format = u16_at(table, off, CTX)?;
    if format != 1 {
        return Err(Error::Malformed {
            offset: off,
            context: "unsupported GDEF MarkGlyphSetsDef format",
        });
    }
    let count = usize::from(u16_at(table, off + 2, CTX)?);
    let mut out = Vec::with_capacity(4 + count * 8);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(count as u16).to_be_bytes());
    out.resize(4 + count * 4, 0);
    let mut coverages = Dedup::default();
    let mut any_glyphs = false;
    for i in 0..count {
        let slot = off + 4 + i * 4;
        let rel = u32_at(table, slot, CTX)? as usize;
        // A null slot is an empty set, the reading the shaper uses, and
        // so is a Coverage that cannot be read. The sum is checked: an
        // Offset32 can wrap a 32-bit `usize`, and a target past the
        // table is reported at the slot on every target alike.
        let glyphs: Vec<u16> = if rel == 0 {
            Vec::new()
        } else if let Some(at) = off.checked_add(rel).filter(|&at| at < table.len()) {
            coverage(table, at)
                .and_then(|glyphs| {
                    if map.spend(1 + glyphs.len()) {
                        Ok(glyphs)
                    } else {
                        Err(super::out_of_budget(at))
                    }
                })
                .unwrap_or_else(|e| {
                    diag.error(&e, "the glyphs of one mark glyph set");
                    Vec::new()
                })
                .into_iter()
                .filter_map(|(gid, _)| map.map(gid))
                .collect()
        } else {
            diag.at(
                slot,
                "GDEF mark glyph set Coverage offset past the end",
                "the glyphs of one mark glyph set",
            );
            Vec::new()
        };
        any_glyphs |= !glyphs.is_empty();
        let at = u32::try_from(coverages.place(&mut out, &emit_coverage_from_glyphs(&glyphs)))
            .map_err(|_| Error::Malformed {
                offset: slot,
                context: "GDEF MarkGlyphSetsDef rewrite exceeds 4 GiB",
            })?;
        out[4 + i * 4..8 + i * 4].copy_from_slice(&at.to_be_bytes());
    }
    Ok(MarkGlyphSets {
        bytes: out,
        any_glyphs,
    })
}
