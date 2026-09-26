//! GDEF byte-level rewriter.
//!
//! Walks the source `GDEF` and produces a new one whose glyph-keyed
//! subtables are remapped through the caller's [`GidMap`].
//!
//! # Per-subtable coverage
//!
//! - **GlyphClassDef** and **MarkAttachClassDef**: ClassDef remap
//!   (dropped glyphs filtered out, kept glyphs renumbered, format
//!   picked by the emitter).
//! - **AttachList**: Coverage remap; each kept glyph's AttachPoint is
//!   copied in the new Coverage order. See [`attach_list`].
//! - **LigCaretList**: Coverage remap; each kept ligature's LigGlyph is
//!   rebuilt from copies of its CaretValues (formats 1, 2 and 3, the
//!   last with its Device / VariationIndex table). See [`lig_caret`].
//! - **MarkGlyphSetsDef**: every set's Coverage is remapped, and set
//!   indices stay stable (a set that loses all its glyphs becomes an
//!   empty Coverage) because lookups name sets by index. See
//!   [`mark_glyph_sets`].
//! - **ItemVariationStore**: copied verbatim when the caller keeps
//!   variations. It is not keyed by glyph id, and the GPOS and caret
//!   VariationIndex tables that reach into it keep their `(outer,
//!   inner)` indices. A static subset drops it, and the GPOS and caret
//!   rewriters clear every VariationIndex so nothing points into it.
//!
//! The output header carries the lowest version that can hold what
//! survived: 1.3 with an ItemVariationStore, else 1.2 with a
//! MarkGlyphSetsDef, else 1.0. The whole table is dropped only when no
//! subtable has anything left.
//!
//! Structural errors in the subtables (truncation, unknown formats)
//! surface as [`SubsetError::Parse`] with the byte offset from the
//! start of the GDEF table.

mod attach_list;
mod item_var_store;
mod lig_caret;
mod mark_glyph_sets;
mod read;

use alloc::vec::Vec;

use sigilbuzz::Error;

use crate::classdef::emit_classdef;
use crate::coverage::emit_coverage_from_glyphs;
use crate::device::Dedup;
use crate::layout::{parse_classdef_pairs_from_bytes, GidMap};
use crate::SubsetError;
use read::{u16_at, u32_at};

/// Rewrites the face's `GDEF` table. Returns `Ok(None)` when the face
/// has no GDEF or when every subtable drops to nothing.
///
/// `keep_variations` mirrors `SubsetInput::retain_variations`: it
/// decides whether the ItemVariationStore (and the caret
/// VariationIndex tables pointing into it) survive.
pub(crate) fn rewrite_gdef(
    face: &sigilbuzz::Face<'_>,
    map: &GidMap,
    keep_variations: bool,
) -> Result<Option<Vec<u8>>, SubsetError> {
    let Ok(bytes) = face.table_bytes(sigilbuzz::tables::tag::GDEF) else {
        return Ok(None);
    };
    rewrite_gdef_bytes(bytes, map, keep_variations)
}

/// [`rewrite_gdef`] on raw table bytes.
///
/// GDEF header (all offsets from the start of the table, 0 = absent):
///
/// ```text
///   u16      majorVersion = 1
///   u16      minorVersion
///   Offset16 glyphClassDefOffset
///   Offset16 attachListOffset
///   Offset16 ligCaretListOffset
///   Offset16 markAttachClassDefOffset
///   Offset16 markGlyphSetsDefOffset      (1.2+)
///   Offset32 itemVarStoreOffset          (1.3+)
/// ```
fn rewrite_gdef_bytes(
    bytes: &[u8],
    map: &GidMap,
    keep_variations: bool,
) -> Result<Option<Vec<u8>>, SubsetError> {
    const CTX: &str = "GDEF header truncated";
    if u16_at(bytes, 0, CTX)? != 1 {
        return Err(Error::Malformed {
            offset: 0,
            context: "unsupported GDEF major version",
        }
        .into());
    }
    let minor = u16_at(bytes, 2, CTX)?;
    let offset_at = |pos: usize| u16_at(bytes, pos, CTX).map(usize::from);
    let glyph_class_off = offset_at(4)?;
    let attach_list_off = offset_at(6)?;
    let lig_caret_off = offset_at(8)?;
    let mark_attach_off = offset_at(10)?;
    let mark_sets_off = if minor >= 2 { offset_at(12)? } else { 0 };
    let ivs_off = if minor >= 3 {
        u32_at(bytes, 14, CTX)? as usize
    } else {
        0
    };

    let glyph_class =
        present(glyph_class_off).and_then(|off| rewrite_classdef_subtable(bytes, off, map));
    let attach_list = match present(attach_list_off) {
        Some(off) => attach_list::rewrite(bytes, off, map)?,
        None => None,
    };
    let lig_carets = match present(lig_caret_off) {
        Some(off) => lig_caret::rewrite(bytes, off, map, keep_variations)?,
        None => None,
    };
    let mark_attach =
        present(mark_attach_off).and_then(|off| rewrite_classdef_subtable(bytes, off, map));
    let mark_sets = match present(mark_sets_off) {
        Some(off) => Some(mark_glyph_sets::rewrite(bytes, off, map)?),
        None => None,
    };
    let ivs = match present(ivs_off).filter(|_| keep_variations) {
        Some(off) => {
            let len = item_var_store::store_len(bytes, off)?;
            Some(&bytes[off..off + len])
        }
        None => None,
    };

    let anything_left = glyph_class.is_some()
        || attach_list.is_some()
        || lig_carets.is_some()
        || mark_attach.is_some()
        || mark_sets.as_ref().is_some_and(|s| s.any_glyphs)
        || ivs.is_some();
    if !anything_left {
        return Ok(None);
    }

    let (minor, header_len) = if ivs.is_some() {
        (3u16, 18)
    } else if mark_sets.is_some() {
        (2, 14)
    } else {
        (0, 12)
    };
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&minor.to_be_bytes());
    out.resize(header_len, 0);
    // Offset16 subtables in header order, then the Offset32 store last
    // so the 16-bit offsets stay as small as possible.
    let offset16_subtables = [
        (4, glyph_class.as_deref()),
        (6, attach_list.as_deref()),
        (8, lig_carets.as_deref()),
        (10, mark_attach.as_deref()),
        (12, mark_sets.as_ref().map(|s| s.bytes.as_slice())),
    ];
    for (slot, body) in offset16_subtables {
        if let Some(body) = body {
            let at = offset16(out.len())?;
            out[slot..slot + 2].copy_from_slice(&at.to_be_bytes());
            out.extend_from_slice(body);
        }
    }
    if let Some(store) = ivs {
        let at = u32::try_from(out.len())
            .map_err(|_| SubsetError::Unsupported("GDEF rewrite: table exceeds 4 GiB"))?;
        out[14..18].copy_from_slice(&at.to_be_bytes());
        out.extend_from_slice(store);
    }
    Ok(Some(out))
}

/// Maps the spec's "0 means absent" offset convention onto `Option`.
fn present(off: usize) -> Option<usize> {
    (off != 0).then_some(off)
}

/// Narrows a position to an Offset16, or reports that the rebuilt
/// table outgrew what 16-bit offsets can address.
fn offset16(pos: usize) -> Result<u16, SubsetError> {
    u16::try_from(pos)
        .map_err(|_| SubsetError::Unsupported("GDEF rewrite: subtable offset exceeds 64 KiB"))
}

/// Walks the coverage-indexed offset array shared by AttachList and
/// LigCaretList:
///
/// ```text
///   Offset16 coverageOffset          (from the list)
///   u16      count
///   Offset16 offsets[count]          (from the list)
/// ```
///
/// Returns `(new glyph id, absolute position of the table the entry
/// names)` for every covered glyph the map keeps, sorted by new glyph
/// id. Coverage entries past `count` have no table and are skipped.
fn kept_entries(
    table: &[u8],
    list_off: usize,
    map: &GidMap,
    context: &'static str,
) -> Result<Vec<(u16, usize)>, Error> {
    let coverage_rel = usize::from(u16_at(table, list_off, context)?);
    if coverage_rel == 0 {
        return Err(Error::Malformed {
            offset: list_off,
            context: "GDEF list has a null Coverage offset",
        });
    }
    let count = u16_at(table, list_off + 2, context)?;
    let mut out = Vec::new();
    for (gid, index) in read::coverage(table, list_off + coverage_rel)? {
        let Some(new_gid) = map.map(gid) else {
            continue;
        };
        if index >= count {
            continue;
        }
        let slot = list_off + 4 + usize::from(index) * 2;
        let rel = usize::from(u16_at(table, slot, context)?);
        if rel == 0 {
            return Err(Error::Malformed {
                offset: slot,
                context: "GDEF list entry has a null offset",
            });
        }
        out.push((new_gid, list_off + rel));
    }
    out.sort_unstable_by_key(|&(gid, _)| gid);
    out.dedup_by_key(|&mut (gid, _)| gid);
    Ok(out)
}

/// Emits the list shape [`kept_entries`] reads: the offset array, one
/// body per entry (identical bodies share one copy), then the
/// Coverage. `entries` must be sorted by glyph id.
fn emit_covered_list(entries: &[(u16, Vec<u8>)]) -> Result<Vec<u8>, SubsetError> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    out.resize(4 + entries.len() * 2, 0);
    let mut bodies = Dedup::default();
    for (i, (_, body)) in entries.iter().enumerate() {
        let at = offset16(bodies.place(&mut out, body))?;
        out[4 + i * 2..6 + i * 2].copy_from_slice(&at.to_be_bytes());
    }
    let coverage_at = offset16(out.len())?;
    out[0..2].copy_from_slice(&coverage_at.to_be_bytes());
    let glyphs: Vec<u16> = entries.iter().map(|&(gid, _)| gid).collect();
    out.extend_from_slice(&emit_coverage_from_glyphs(&glyphs));
    Ok(out)
}

fn rewrite_classdef_subtable(bytes: &[u8], offset: usize, map: &GidMap) -> Option<Vec<u8>> {
    let body = bytes.get(offset..)?;
    let pairs = parse_classdef_pairs_from_bytes(body);
    let mut new_pairs: Vec<(u16, u16)> = Vec::with_capacity(pairs.len());
    for (gid, class) in pairs {
        let Some(new_gid) = map.map(gid) else {
            continue;
        };
        new_pairs.push((new_gid, class));
    }
    if new_pairs.is_empty() {
        return None;
    }
    Some(emit_classdef(&new_pairs))
}

#[cfg(test)]
mod tests;
