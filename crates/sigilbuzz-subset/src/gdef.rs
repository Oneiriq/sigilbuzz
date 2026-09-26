//! GDEF byte-level rewriter.
//!
//! Walks the source `GDEF` and produces a new one whose gid-keyed
//! subtables (`GlyphClassDef`, `MarkAttachClassDef`) are remapped
//! through the caller's [`GidMap`].
//!
//! # Per-subtable coverage
//!
//! As of this commit the rewriter ships byte-level support for:
//!
//! - **GlyphClassDef**: ClassDef remap (filter dropped gids out, then
//!   remap to new gids; auto-format-pick via the existing emitter).
//! - **MarkAttachClassDef**: ClassDef remap, same shape as
//!   GlyphClassDef.
//!
//! Other GDEF subtables (`AttachList`, `LigCaretList`,
//! `MarkGlyphSetsDef`, `ItemVariationStore`) are dropped from the
//! rewritten output. Most callers that disable layout-aware shaping
//! for a heavy subset don't notice because GPOS drops too (see
//! [`crate::gpos`]) and these ancillary tables are only consulted
//! during shaping.
//!
//! Issue tracking the remaining GDEF subtables: see the sibling issue
//! filed alongside this module.

use alloc::vec::Vec;

use crate::classdef::emit_classdef;
use crate::layout::{parse_classdef_pairs_from_bytes, GidMap};

/// Rewrites a `GDEF` table. Returns `None` if every contained
/// subtable drops to nothing.
pub(crate) fn rewrite_gdef(face: &sigilbuzz::Face<'_>, map: &GidMap) -> Option<Vec<u8>> {
    let bytes = face.table_bytes(sigilbuzz::tables::tag::GDEF).ok()?;
    if bytes.len() < 12 {
        return None;
    }

    // GDEF header (v1.0):
    //   u16 majorVersion
    //   u16 minorVersion
    //   Offset16 glyphClassDefOffset
    //   Offset16 attachListOffset
    //   Offset16 ligCaretListOffset
    //   Offset16 markAttachClassDefOffset
    //   (v1.2+) Offset16 markGlyphSetsDefOffset
    //   (v1.3+) Offset32 itemVarStoreOffset
    let major = u16::from_be_bytes([bytes[0], bytes[1]]);
    let minor = u16::from_be_bytes([bytes[2], bytes[3]]);
    if major != 1 {
        return None;
    }
    let glyph_class_off = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let _attach_list_off = u16::from_be_bytes([bytes[6], bytes[7]]) as usize;
    let _lig_caret_off = u16::from_be_bytes([bytes[8], bytes[9]]) as usize;
    let mark_attach_off = u16::from_be_bytes([bytes[10], bytes[11]]) as usize;

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
    if new_glyph_class.is_none() && new_mark_attach.is_none() {
        return None;
    }

    // Re-emit a v1.0 GDEF with only the subtables we know how to
    // rewrite. Other subtable offsets are zeroed.
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor: drop to v1.0; we don't carry mark glyph sets / IVS yet.
    let _ = minor;

    // Header offsets get patched once we know the subtable positions.
    let glyph_class_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // glyphClassDef offset
    out.extend_from_slice(&0u16.to_be_bytes()); // attachList offset (dropped)
    out.extend_from_slice(&0u16.to_be_bytes()); // ligCaretList offset (dropped)
    let mark_attach_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // markAttachClassDef offset

    if let Some(gc) = new_glyph_class.as_deref() {
        let pos = out.len() as u16;
        out.extend_from_slice(gc);
        out[glyph_class_slot..glyph_class_slot + 2].copy_from_slice(&pos.to_be_bytes());
    }
    if let Some(ma) = new_mark_attach.as_deref() {
        let pos = out.len() as u16;
        out.extend_from_slice(ma);
        out[mark_attach_slot..mark_attach_slot + 2].copy_from_slice(&pos.to_be_bytes());
    }

    Some(out)
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
