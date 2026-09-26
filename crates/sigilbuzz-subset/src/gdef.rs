//! GDEF byte-level rewriter.
//!
//! Walks the source `GDEF` and produces a new one whose gid-keyed
//! subtables (`GlyphClassDef`, `MarkAttachClassDef`) are remapped
//! through the caller's [`GidMap`].
//!
//! # Per-subtable coverage
//!
//! The rewriter supports:
//!
//! - **GlyphClassDef**: ClassDef remap (filter dropped gids out, then
//!   remap to new gids; auto-format-pick via the existing emitter).
//! - **MarkAttachClassDef**: ClassDef remap, same shape as
//!   GlyphClassDef.
//! - **ItemVariationStore**: copied verbatim. Its rows are keyed by
//!   `(outer, inner)` pairs, not glyph ids, so the VariationIndex
//!   tables the GPOS rewrite carries over still name the right rows.
//!   The output is a version 1.3 table when the store is present.
//!
//! Other GDEF subtables (`AttachList`, `LigCaretList`,
//! `MarkGlyphSetsDef`) are not rewritten yet and are dropped from the
//! output, which is otherwise a version 1.0 table. Lookups that name a
//! mark filtering set keep that index, so shaping with the subset
//! ignores their mark filter.

use alloc::vec::Vec;

use crate::classdef::emit_classdef;
use crate::layout::{patch_offset16, read_u16, GidMap};

/// Rewrites a `GDEF` table. Returns `None` if every contained
/// subtable drops to nothing, or if the rewritten ClassDefs no longer
/// fit their 16-bit offsets.
///
/// The ItemVariationStore is copied as the byte range from its offset
/// to the end of the source table. Every offset inside the store is
/// relative to its start and unsigned, so that range holds all of it.
pub(crate) fn rewrite_gdef(face: &sigilbuzz::Face<'_>, map: &GidMap) -> Option<Vec<u8>> {
    let bytes = face.table_bytes(sigilbuzz::tables::tag::GDEF).ok()?;

    // GDEF header (v1.0):
    //   u16 majorVersion
    //   u16 minorVersion
    //   Offset16 glyphClassDefOffset
    //   Offset16 attachListOffset
    //   Offset16 ligCaretListOffset
    //   Offset16 markAttachClassDefOffset
    //   (v1.2+) Offset16 markGlyphSetsDefOffset
    //   (v1.3+) Offset32 itemVarStoreOffset
    let major = read_u16(bytes, 0)?;
    if major != 1 {
        return None;
    }
    let minor = read_u16(bytes, 2)?;
    let glyph_class_off = usize::from(read_u16(bytes, 4)?);
    let mark_attach_off = usize::from(read_u16(bytes, 10)?);
    let var_store = if minor >= 3 {
        let off = bytes
            .get(14..18)
            .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        // Offsets inside the 18-byte v1.3 header are malformed.
        usize::try_from(off)
            .ok()
            .filter(|&off| off >= V13_HEADER_LEN)
            .and_then(|off| bytes.get(off..))
            .filter(|store| !store.is_empty())
    } else {
        None
    };

    // Rewrite GlyphClassDef.
    let new_glyph_class = if glyph_class_off != 0 {
        rewrite_classdef_subtable(bytes, glyph_class_off, map)
    } else {
        None
    };
    // Rewrite MarkAttachClassDef.
    let new_mark_attach = if mark_attach_off != 0 {
        rewrite_classdef_subtable(bytes, mark_attach_off, map)
    } else {
        None
    };

    // If both classdefs drop and we don't carry anything else, the
    // whole GDEF is empty. The caller drops it.
    if new_glyph_class.is_none() && new_mark_attach.is_none() && var_store.is_none() {
        return None;
    }

    // Re-emit a v1.0 GDEF, or v1.3 when the store is carried, with
    // only the subtables we know how to rewrite. Other subtable
    // offsets are zeroed.
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    let minor: u16 = if var_store.is_some() { 3 } else { 0 };
    out.extend_from_slice(&minor.to_be_bytes());

    // Header offsets get patched once we know the subtable positions.
    let glyph_class_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // glyphClassDef offset
    out.extend_from_slice(&0u16.to_be_bytes()); // attachList offset (dropped)
    out.extend_from_slice(&0u16.to_be_bytes()); // ligCaretList offset (dropped)
    let mark_attach_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // markAttachClassDef offset
    let var_store_slot = out.len() + 2;
    if var_store.is_some() {
        out.extend_from_slice(&0u16.to_be_bytes()); // markGlyphSetsDef offset (dropped)
        out.extend_from_slice(&0u32.to_be_bytes()); // itemVarStore offset
    }

    if let Some(gc) = new_glyph_class.as_deref() {
        let pos = out.len();
        patch_offset16(&mut out, glyph_class_slot, pos)?;
        out.extend_from_slice(gc);
    }
    if let Some(ma) = new_mark_attach.as_deref() {
        let pos = out.len();
        patch_offset16(&mut out, mark_attach_slot, pos)?;
        out.extend_from_slice(ma);
    }
    if let Some(store) = var_store {
        let pos = u32::try_from(out.len()).ok()?;
        out[var_store_slot..var_store_slot + 4].copy_from_slice(&pos.to_be_bytes());
        out.extend_from_slice(store);
    }

    Some(out)
}

/// Length of a version 1.3 GDEF header.
const V13_HEADER_LEN: usize = 18;

fn rewrite_classdef_subtable(bytes: &[u8], offset: usize, map: &GidMap) -> Option<Vec<u8>> {
    let body = bytes.get(offset..)?;
    let pairs = map.classdef_pairs(body)?;
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
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};
    use sigilbuzz::tables::layout::ClassDef;

    #[test]
    fn rewrite_classdef_filters_dropped_gids() {
        // Build a tiny ClassDef format 2 inline for the helper.
        // Class assignments: gid 5->1, gid 6->2, gid 10->3.
        let mut cd_bytes = Vec::new();
        cd_bytes.extend_from_slice(&2u16.to_be_bytes()); // format
        cd_bytes.extend_from_slice(&3u16.to_be_bytes()); // rangeCount
        for (start, end, class) in [(5u16, 5u16, 1u16), (6, 6, 2), (10, 10, 3)] {
            cd_bytes.extend_from_slice(&start.to_be_bytes());
            cd_bytes.extend_from_slice(&end.to_be_bytes());
            cd_bytes.extend_from_slice(&class.to_be_bytes());
        }
        // GidMap: 5->1 (kept), 6 dropped, 10->3 (kept). Rebuild to length 11.
        let mut table = vec![None; 11];
        table[0] = Some(0);
        table[5] = Some(1);
        table[10] = Some(3);
        let map = GidMap::from_table(table);

        let new_cd = rewrite_classdef_subtable(&cd_bytes, 0, &map).unwrap();
        let parsed = ClassDef::parse(&new_cd).unwrap();
        assert_eq!(parsed.class_of(1), 1, "gid 5->1 keeps class 1");
        assert_eq!(parsed.class_of(3), 3, "gid 10->3 keeps class 3");
        assert_eq!(parsed.class_of(2), 0, "gid 6 dropped -> unlisted = class 0");
    }
}
