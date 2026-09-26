//! `cmap` subsetting.
//!
//! The source font's cmap is consulted character-by-character across
//! the BMP (U+0000..=U+FFFF). For every codepoint that maps to a kept
//! gid we record `(codepoint, new_gid)`; the result becomes a freshly
//! encoded format-4 subtable wrapped in a single Windows BMP encoding
//! record.
//!
//! Format 4 caps its subtable at 64 KiB, which holds about 8,000
//! segments. A mapping too fragmented for that is written as a single
//! format-12 subtable under the Windows full-repertoire encoding
//! record instead.
//!
//! Only BMP codepoints are carried over. Supplementary-plane mappings
//! are dropped; bundled fixtures (Open Sans, Amiri) are BMP-only.

use alloc::vec::Vec;

use sigilbuzz::Face;

use crate::SubsetError;

/// Largest number of format 4 segments, the terminator included, whose
/// subtable fits the 16-bit `length` field: 14 header bytes, 2 bytes of
/// `reservedPad`, and 8 bytes per segment.
const MAX_FORMAT4_SEGMENTS: usize = (u16::MAX as usize - 16) / 8;

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
        let i = gid_map
            .binary_search_by_key(&old_gid, |(old, _)| *old)
            .ok()?;
        gid_map.get(i).map(|&(_, new)| new)
    };

    let mut entries: Vec<(u16, u16)> = Vec::new(); // (codepoint, new_gid)
    for cp in 0u16..=0xFFFF {
        // Surrogates are never legal in cmap input, and `char` rejects
        // them.
        let Some(ch) = char::from_u32(u32::from(cp)) else {
            continue;
        };
        let Some(old_gid) = cmap.glyph_id(ch) else {
            continue;
        };
        if let Some(new_gid) = lookup(old_gid) {
            // .notdef (gid 0) intentionally never appears in cmap output.
            if new_gid != 0 {
                entries.push((cp, new_gid));
            }
        }
    }

    Ok(encode_cmap(&entries))
}

/// Encodes sorted `(codepoint, new_gid)` pairs as a complete `cmap`
/// table: format 4 when it fits, format 12 otherwise.
fn encode_cmap(entries: &[(u16, u16)]) -> Vec<u8> {
    // Build segments. A segment is a run of codepoints with a
    // constant `new_gid - codepoint` delta. Each break in continuity
    // (whether on codepoint or delta) starts a new segment.
    let mut segments: Vec<(u16, u16, i32)> = Vec::new(); // (start, end, delta)
    for &(cp, gid) in entries {
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
    // idDelta arbitrary (we use 1 -> wraps to 0 for codepoint 0xFFFF,
    // i.e. the `.notdef` slot, which is the conventional choice).
    segments.push((0xFFFF, 0xFFFF, 1));

    if segments.len() <= MAX_FORMAT4_SEGMENTS {
        // Windows BMP encoding record.
        wrap_cmap(3, 1, &build_format4(&segments))
    } else {
        // Windows full-repertoire encoding record.
        wrap_cmap(3, 10, &build_format12(entries))
    }
}

/// Wraps a single subtable in a `cmap` table header with one encoding
/// record for `(platform_id, encoding_id)`.
fn wrap_cmap(platform_id: u16, encoding_id: u16, subtable: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 8 + subtable.len());
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&1u16.to_be_bytes()); // numTables
                                                // Encoding record: platform, encoding, offset to subtable.
    let subtable_off: u32 = 4 + 8;
    out.extend_from_slice(&platform_id.to_be_bytes());
    out.extend_from_slice(&encoding_id.to_be_bytes());
    out.extend_from_slice(&subtable_off.to_be_bytes());
    out.extend_from_slice(subtable);
    out
}

/// Encodes segments as a cmap format-4 subtable. Every segment uses
/// `idRangeOffset = 0` and folds the gid math into `idDelta`.
///
/// The caller keeps `segments.len()` at or below
/// [`MAX_FORMAT4_SEGMENTS`], which keeps every header field in range.
fn build_format4(segments: &[(u16, u16, i32)]) -> Vec<u8> {
    debug_assert!(segments.len() <= MAX_FORMAT4_SEGMENTS);
    let seg_count = segments.len();
    // Header: format(2) + length(2) + language(2) + segCountX2(2) +
    //         searchRange(2) + entrySelector(2) + rangeShift(2) = 14
    // Body: 4 * segCount * u16 + 1 reservedPad u16 = 8 * seg_count + 2.
    let body_bytes = 8 * seg_count + 2;
    let total = 14 + body_bytes;

    // searchRange / entrySelector come from the largest power of two
    // <= seg_count, scaled by 2. Computed in `usize` so the doubling
    // cannot overflow.
    let mut entry_selector: u16 = 0;
    let mut sr_pow: usize = 1;
    while sr_pow * 2 <= seg_count {
        sr_pow *= 2;
        entry_selector += 1;
    }
    let seg_count_x2 = (seg_count * 2) as u16;
    let search_range = (sr_pow * 2) as u16;
    let range_shift = seg_count_x2.wrapping_sub(search_range);

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

/// Encodes sorted `(codepoint, new_gid)` pairs as a cmap format-12
/// subtable. A group is a run where both the codepoint and the gid
/// step by one.
fn build_format12(entries: &[(u16, u16)]) -> Vec<u8> {
    let mut groups: Vec<(u32, u32, u32)> = Vec::new(); // (startChar, endChar, startGlyph)
    for &(cp, gid) in entries {
        let (cp, gid) = (u32::from(cp), u32::from(gid));
        if let Some(last) = groups.last_mut() {
            if last.1 + 1 == cp && last.2 + (cp - last.0) == gid {
                last.1 = cp;
                continue;
            }
        }
        groups.push((cp, cp, gid));
    }

    // Header: format(2) + reserved(2) + length(4) + language(4) +
    // numGroups(4) = 16, then 12 bytes per group. At most one group
    // per BMP codepoint, so both counts fit in 32 bits.
    let total = 16 + groups.len() * 12;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&12u16.to_be_bytes()); // format
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    out.extend_from_slice(&(total as u32).to_be_bytes()); // length
    out.extend_from_slice(&0u32.to_be_bytes()); // language
    out.extend_from_slice(&(groups.len() as u32).to_be_bytes());
    for (start, end, start_gid) in groups {
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&end.to_be_bytes());
        out.extend_from_slice(&start_gid.to_be_bytes());
    }
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

    /// Maps codepoints `1..=count` to gids so that no two
    /// neighbors share a delta, which makes one format 4 segment per
    /// codepoint.
    fn fragmented_entries(count: u16) -> alloc::vec::Vec<(u16, u16)> {
        (1..=count).map(|cp| (cp, 1 + (cp % 2) * 2)).collect()
    }

    #[test]
    fn encode_cmap_with_40000_segments_uses_format12() {
        // 40000 segments used to overflow the u16 searchRange doubling
        // in `build_format4` (a panic in debug builds, an endless loop
        // in release builds).
        let entries = fragmented_entries(40_000);
        let bytes = encode_cmap(&entries);
        let cmap = sigilbuzz::tables::Cmap::parse(&bytes).unwrap();
        assert_eq!(cmap.glyph_id('\u{1}'), Some(3));
        assert_eq!(cmap.glyph_id('\u{2}'), Some(1));
        assert_eq!(cmap.glyph_id(char::from_u32(40_000).unwrap()), Some(1));
        assert_eq!(cmap.glyph_id(char::from_u32(40_001).unwrap()), None);
    }

    #[test]
    fn encode_cmap_past_format4_length_limit_uses_format12() {
        // 9000 segments no longer fit format 4's 16-bit length field,
        // which used to wrap and produce a corrupt subtable.
        let entries = fragmented_entries(9_000);
        let bytes = encode_cmap(&entries);
        let cmap = sigilbuzz::tables::Cmap::parse(&bytes).unwrap();
        assert_eq!(cmap.glyph_id(char::from_u32(8_999).unwrap()), Some(3));
        assert_eq!(cmap.glyph_id(char::from_u32(9_000).unwrap()), Some(1));
    }

    #[test]
    fn encode_cmap_at_format4_limit_stays_format4() {
        // Exactly at the limit once the terminator segment is added.
        let entries = fragmented_entries((MAX_FORMAT4_SEGMENTS - 1) as u16);
        let bytes = encode_cmap(&entries);
        // Subtable starts after the 4-byte header and 8-byte record.
        assert_eq!(&bytes[12..14], &4u16.to_be_bytes());
        let cmap = sigilbuzz::tables::Cmap::parse(&bytes).unwrap();
        assert_eq!(cmap.glyph_id('\u{3}'), Some(3));
    }
}
