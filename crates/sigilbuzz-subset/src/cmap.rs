//! `cmap` subsetting.
//!
//! The source font's cmap is consulted character-by-character across
//! the BMP (U+0000..=U+FFFF). For every codepoint that maps to a kept
//! gid we record `(codepoint, new_gid)`; the result becomes a freshly
//! encoded format-4 subtable wrapped in a single Windows BMP encoding
//! record.
//!
//! Format 4 covers the BMP only. Subset fonts that need astral-plane
//! coverage will require a format-12 emitter alongside this — left as
//! a TODO; bundled fixtures (Open Sans, Amiri) are BMP-only.

use alloc::vec::Vec;

use sigilbuzz::Face;

use crate::SubsetError;

/// Builds a new cmap for the subset.
///
/// `gid_map` is sorted by old gid. We walk the BMP, ask the source's
/// cmap for each character, look the resulting gid up in `gid_map`,
/// and collect the `(char, new_gid)` pairs that survive.
pub fn subset_cmap(face: &Face<'_>, gid_map: &[(u16, u16)]) -> Result<Vec<u8>, SubsetError> {
    let cmap = face.cmap()?;

    // Old gid -> new gid lookup. gid_map is small (<= numGlyphs of
    // the subset, usually a few dozen) so a sorted binary search is
    // cheaper than a HashMap and keeps determinism baked in.
    let lookup = |old_gid: u16| -> Option<u16> {
        gid_map
            .binary_search_by_key(&old_gid, |(old, _)| *old)
            .ok()
            .map(|i| gid_map[i].1)
    };

    let mut entries: Vec<(u16, u16)> = Vec::new(); // (codepoint, new_gid)
    for cp in 0u32..=0xFFFF {
        // Skip surrogates — never legal in cmap input.
        if (0xD800..=0xDFFF).contains(&cp) {
            continue;
        }
        let Some(ch) = char::from_u32(cp) else {
            continue;
        };
        let Some(old_gid) = cmap.glyph_id(ch) else {
            continue;
        };
        if let Some(new_gid) = lookup(old_gid) {
            // .notdef (gid 0) intentionally never appears in cmap output.
            if new_gid != 0 {
                entries.push((cp as u16, new_gid));
            }
        }
    }

    // Build segments. A segment is a run of codepoints with a
    // constant `new_gid - codepoint` delta. Each break in continuity
    // — whether on codepoint or delta — starts a new segment.
    let mut segments: Vec<(u16, u16, i32)> = Vec::new(); // (start, end, delta)
    for (cp, gid) in entries {
        let delta = gid as i32 - cp as i32;
        if let Some(last) = segments.last_mut() {
            if last.1.wrapping_add(1) == cp && last.2 == delta {
                last.1 = cp;
                continue;
            }
        }
        segments.push((cp, cp, delta));
    }

    // Append the spec-mandated terminator: startCode = endCode = 0xFFFF,
    // idDelta arbitrary (we use 1 → wraps to 0 for codepoint 0xFFFF,
    // i.e. the `.notdef` slot, which is the conventional choice).
    segments.push((0xFFFF, 0xFFFF, 1));

    let format4 = build_format4(&segments);
    Ok(wrap_cmap(&format4))
}

/// Wraps a single subtable in a `cmap` table header with one
/// (platform=3, encoding=1 — Windows BMP) encoding record.
fn wrap_cmap(subtable: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 8 + subtable.len());
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&1u16.to_be_bytes()); // numTables
                                                // Encoding record: platform=3, encoding=1, offset to subtable.
    let subtable_off: u32 = (4 + 8) as u32;
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&subtable_off.to_be_bytes());
    out.extend_from_slice(subtable);
    out
}

/// Encodes segments as a cmap format-4 subtable. Every segment uses
/// `idRangeOffset = 0` and folds the gid math into `idDelta`.
fn build_format4(segments: &[(u16, u16, i32)]) -> Vec<u8> {
    let seg_count = segments.len();
    let seg_count_x2 = (seg_count * 2) as u16;
    // Header: format(2) + length(2) + language(2) + segCountX2(2) +
    //         searchRange(2) + entrySelector(2) + rangeShift(2) = 14
    // Body: 4 × segCount × u16 + 1 reservedPad u16 = 8 * seg_count + 2.
    let body_bytes = 8 * seg_count + 2;
    let total = 14 + body_bytes;

    // Ceil-log2(seg_count). Format 4's searchRange / entrySelector
    // are derived from the largest power of two ≤ seg_count, scaled
    // by 2. We compute it longhand because sigilbuzz is no_std.
    let mut entry_selector: u16 = 0;
    let mut sr_pow: u16 = 1;
    while sr_pow * 2 <= seg_count as u16 {
        sr_pow *= 2;
        entry_selector += 1;
    }
    let search_range = sr_pow * 2;
    let range_shift = (seg_count as u16)
        .wrapping_mul(2)
        .wrapping_sub(search_range);

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&4u16.to_be_bytes()); // format
    out.extend_from_slice(&(total as u16).to_be_bytes()); // length
    out.extend_from_slice(&0u16.to_be_bytes()); // language
    out.extend_from_slice(&seg_count_x2.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());

    for &(_, end, _) in segments {
        out.extend_from_slice(&end.to_be_bytes());
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
    for &(start, _, _) in segments {
        out.extend_from_slice(&start.to_be_bytes());
    }
    for &(_, _, delta) in segments {
        // idDelta is i16 modulo 2^16.
        let delta_i16 = (delta & 0xFFFF) as i16;
        out.extend_from_slice(&delta_i16.to_be_bytes());
    }
    for _ in segments {
        out.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset = 0
    }

    debug_assert_eq!(out.len(), total);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_format4_emits_well_formed_header() {
        // Two segments: A..A delta=10, terminator.
        let segs: alloc::vec::Vec<_> =
            alloc::vec![(b'A' as u16, b'A' as u16, 10), (0xFFFF, 0xFFFF, 1),];
        let bytes = build_format4(&segs);
        // format == 4
        assert_eq!(&bytes[0..2], &4u16.to_be_bytes());
        // length == bytes.len()
        assert_eq!(&bytes[2..4], &(bytes.len() as u16).to_be_bytes(),);
        // segCountX2 == 4
        assert_eq!(&bytes[6..8], &4u16.to_be_bytes());
    }
}
