//! `gvar` subsetting.
//!
//! `gvar` carries per-glyph contour-point deltas as a sequence of
//! `GlyphVariationData` blocks indexed by gid. Each block holds one
//! or more *tuple variations*, each of which references a peak
//! tuple, either embedded in the block or referenced by index into
//! the file-wide *shared tuple list*.
//!
//! Subsetting copies one block per kept gid into the output, in the
//! new-gid order, and rewrites the per-gid offset array. The shared
//! tuple list is preserved verbatim. Every kept block's references
//! remain valid because the indexes never change. Unreferenced
//! shared tuples are not pruned: the size win is small for any
//! realistic subset against the bulk of the per-glyph delta data.
//!
//! The output picks the smaller offset format (short or long) by
//! the size of the data block: short caps at `0x1FFFE` bytes (each
//! short offset is half-encoded as a `u16`).

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::tables::Reader;
use sigilbuzz::Face;

use crate::{GlyphId, SubsetError};

/// Subsets `gvar` for `kept` (kept gids in new-gid order). Returns
/// `None` when the source has no gvar.
pub(crate) fn subset_gvar(
    face: &Face<'_>,
    kept: &[GlyphId],
) -> Result<Option<Vec<u8>>, SubsetError> {
    let bytes = match face.table_bytes(tag::GVAR) {
        Ok(b) => b,
        Err(sigilbuzz::Error::MissingTable { .. }) => return Ok(None),
        Err(e) => return Err(SubsetError::from(e)),
    };
    let header = parse_gvar_header(bytes)?;

    // The shared tuple list is copied verbatim into the output. Check
    // that the source actually holds it before any size derived from
    // its header counts reaches an allocation.
    let shared_tuples_len =
        usize::from(header.axis_count) * usize::from(header.shared_tuple_count) * 2;
    let shared_tuples = if shared_tuples_len > 0 {
        let src_start = header.shared_tuples_off as usize;
        let src_end = src_start
            .checked_add(shared_tuples_len)
            .ok_or(SubsetError::Unsupported("gvar shared tuples overflow"))?;
        bytes
            .get(src_start..src_end)
            .ok_or(SubsetError::Unsupported(
                "gvar shared tuples past end of source",
            ))?
    } else {
        &[][..]
    };

    // Pull one body per kept gid (empty Vec when source has no
    // entry for that gid). Kept gids are distinct, and a well-formed
    // table stores each glyph's data in its own byte range, so the
    // bodies add up to at most the table size. Offsets that make many
    // glyphs share one large range would multiply the output instead.
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(kept.len());
    let mut body_budget = bytes.len();
    for &old_gid in kept {
        let body = pull_glyph_body(bytes, &header, old_gid);
        body_budget = body_budget
            .checked_sub(body.len())
            .ok_or(SubsetError::Unsupported("gvar glyph data ranges overlap"))?;
        bodies.push(body);
    }

    // Pad each body to 2 bytes so short offsets stay aligned. The
    // spec allows odd-length bodies; padding only applies to the
    // short-offset flavor, but emitting it unconditionally costs at
    // most one byte per glyph and keeps the format-selection logic
    // trivial.
    for body in &mut bodies {
        if body.len() % 2 != 0 {
            body.push(0);
        }
    }

    // Compute glyph offsets (relative to the data array start).
    let mut offsets: Vec<u32> = Vec::with_capacity(bodies.len() + 1);
    let mut cursor: u32 = 0;
    offsets.push(0);
    for body in &bodies {
        cursor = cursor
            .checked_add(body.len() as u32)
            .ok_or(SubsetError::Unsupported("gvar offset overflow"))?;
        offsets.push(cursor);
    }
    let total_data_len = *offsets.last().unwrap_or(&0);
    let long_offsets = total_data_len > 0x1_FFFE;

    // Assemble the output. Layout:
    //   header (20 bytes)
    //   per-gid offsets ((kept.len() + 1) entries)
    //   shared tuple list (axis_count * shared_tuple_count * 2 bytes)
    //   data array (sum of per-glyph bodies)
    let header_len = 20;
    let off_entry_size: usize = if long_offsets { 4 } else { 2 };
    let offsets_len = (kept.len() + 1) * off_entry_size;

    // Round offsets-block up to 4-byte alignment per the spec hint
    // (data array is aligned for u32 reads).
    let offsets_padded = (offsets_len + 3) & !3;

    let shared_tuples_off = (header_len + offsets_padded) as u32;
    let data_array_off = (shared_tuples_off as usize + shared_tuples_len) as u32;

    let mut out: Vec<u8> = Vec::with_capacity(
        header_len + offsets_padded + shared_tuples_len + total_data_len as usize,
    );

    // Header.
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&header.axis_count.to_be_bytes());
    out.extend_from_slice(&header.shared_tuple_count.to_be_bytes());
    out.extend_from_slice(&shared_tuples_off.to_be_bytes());
    out.extend_from_slice(&(kept.len() as u16).to_be_bytes());
    let flags: u16 = if long_offsets { 0x0001 } else { 0x0000 };
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&data_array_off.to_be_bytes());

    // Offsets.
    if long_offsets {
        for o in &offsets {
            out.extend_from_slice(&o.to_be_bytes());
        }
    } else {
        for o in &offsets {
            let half = (*o / 2) as u16;
            out.extend_from_slice(&half.to_be_bytes());
        }
    }
    while out.len() < (header_len + offsets_padded) {
        out.push(0);
    }

    // Shared tuple list: copied verbatim from the source (validated
    // above).
    out.extend_from_slice(shared_tuples);

    // Data array.
    for body in &bodies {
        out.extend_from_slice(body);
    }

    Ok(Some(out))
}

/// Parsed gvar header: only the fields the subsetter needs.
#[derive(Debug, Clone, Copy)]
struct GvarHeader {
    axis_count: u16,
    shared_tuple_count: u16,
    shared_tuples_off: u32,
    glyph_count: u16,
    long_offsets: bool,
    data_array_off: u32,
    /// Cached glyph-offset array byte position. Each offset is two
    /// bytes (short) or four bytes (long), and the array has
    /// `glyph_count + 1` entries.
    glyph_offsets_start: usize,
}

fn parse_gvar_header(bytes: &[u8]) -> Result<GvarHeader, SubsetError> {
    let mut r = Reader::new(bytes);
    let major = r
        .read_u16()
        .map_err(|_| SubsetError::Unsupported("gvar header"))?;
    let _minor = r
        .read_u16()
        .map_err(|_| SubsetError::Unsupported("gvar minor"))?;
    if major != 1 {
        return Err(SubsetError::Unsupported("gvar major != 1"));
    }
    let axis_count = r
        .read_u16()
        .map_err(|_| SubsetError::Unsupported("gvar axis count"))?;
    let shared_tuple_count = r
        .read_u16()
        .map_err(|_| SubsetError::Unsupported("gvar shared tuple count"))?;
    let shared_tuples_off = r
        .read_u32()
        .map_err(|_| SubsetError::Unsupported("gvar shared tuples off"))?;
    let glyph_count = r
        .read_u16()
        .map_err(|_| SubsetError::Unsupported("gvar glyph count"))?;
    let flags = r
        .read_u16()
        .map_err(|_| SubsetError::Unsupported("gvar flags"))?;
    let data_array_off = r
        .read_u32()
        .map_err(|_| SubsetError::Unsupported("gvar data array off"))?;
    let long_offsets = flags & 0x0001 != 0;
    let glyph_offsets_start = r.position();
    Ok(GvarHeader {
        axis_count,
        shared_tuple_count,
        shared_tuples_off,
        glyph_count,
        long_offsets,
        data_array_off,
        glyph_offsets_start,
    })
}

/// Pulls the source `GlyphVariationData` body for `gid` out of the
/// gvar table. Returns an empty `Vec` for gids past the source's
/// glyph count, gids whose offset range is empty, or any malformed
/// truncation. All of those are treated as "no variation data".
fn pull_glyph_body(bytes: &[u8], header: &GvarHeader, gid: u16) -> Vec<u8> {
    glyph_body_range(bytes, header, gid)
        .and_then(|range| bytes.get(range))
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

/// Resolves the byte range of `gid`'s body inside `bytes`, or `None`
/// when the entry is missing, empty, or malformed.
fn glyph_body_range(
    bytes: &[u8],
    header: &GvarHeader,
    gid: u16,
) -> Option<core::ops::Range<usize>> {
    if gid >= header.glyph_count {
        return None;
    }
    let entry_size: usize = if header.long_offsets { 4 } else { 2 };
    let off_a = header.glyph_offsets_start + usize::from(gid) * entry_size;
    // Both offsets are read from the array entry pair at `off_a`.
    let pair = bytes.get(off_a..)?.get(..entry_size * 2)?;
    let (start, end) = match *pair {
        [a0, a1, a2, a3, b0, b1, b2, b3] => (
            u32::from_be_bytes([a0, a1, a2, a3]),
            u32::from_be_bytes([b0, b1, b2, b3]),
        ),
        [a0, a1, b0, b1] => (
            u32::from(u16::from_be_bytes([a0, a1])) * 2,
            u32::from(u16::from_be_bytes([b0, b1])) * 2,
        ),
        _ => return None,
    };
    if end <= start {
        return None;
    }
    let data_array_off = header.data_array_off as usize;
    let body_start = data_array_off.checked_add(start as usize)?;
    let body_end = data_array_off.checked_add(end as usize)?;
    (body_end <= bytes.len()).then_some(body_start..body_end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigilbuzz::tables::gvar::Gvar as ParsedGvar;

    const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

    fn rubik_face() -> Face<'static> {
        Face::parse_bytes(RUBIK, 0).unwrap()
    }

    #[test]
    fn missing_gvar_returns_none() {
        const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
        let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
        let kept: Vec<u16> = (0..face.maxp().unwrap().num_glyphs).collect();
        assert!(subset_gvar(&face, &kept).unwrap().is_none());
    }

    #[test]
    fn subset_gvar_preserves_per_glyph_deltas() {
        // Pull a kept gid's deltas from the source gvar and compare
        // against the same gid's deltas in the subset gvar at a
        // non-default coord. They must match exactly: the subset is
        // a verbatim copy of the per-glyph block.
        let face = rubik_face();
        let cmap = face.cmap().unwrap();
        let gid_a = cmap.glyph_id('A').unwrap();
        let gid_b = cmap.glyph_id('B').unwrap();
        let gid_c = cmap.glyph_id('C').unwrap();
        let mut kept: Vec<u16> = alloc::vec![0, gid_a, gid_b, gid_c];
        kept.sort_unstable();

        let src = face.gvar().unwrap().expect("rubik has gvar");
        let coords = face.fvar().unwrap().unwrap().normalize_coords(&[900.0]);

        let new_bytes = subset_gvar(&face, &kept).unwrap().expect("gvar bytes");
        let new = ParsedGvar::parse(&new_bytes).expect("parse subset gvar");

        // Pull num_points from the source so the subset's
        // glyph_deltas walks the same number of points.
        let loca = face.loca().unwrap();
        let glyf = face.glyf().unwrap();

        for (new_gid, &old_gid) in kept.iter().enumerate() {
            let Some(num_points) = glyf.point_count(&loca, old_gid).unwrap() else {
                continue;
            };
            let want = src.glyph_deltas(old_gid, &coords, num_points);
            let got = new.glyph_deltas(new_gid as u16, &coords, num_points);
            assert_eq!(
                want.len(),
                got.len(),
                "delta count mismatch at gid {old_gid}"
            );
            for (a, b) in want.iter().zip(got.iter()) {
                assert_eq!(a.point, b.point, "point index mismatch");
                assert!((a.dx - b.dx).abs() <= 1e-3, "dx mismatch");
                assert!((a.dy - b.dy).abs() <= 1e-3, "dy mismatch");
            }
        }
    }

    #[test]
    fn subset_gvar_emits_glyph_count_matching_kept() {
        let face = rubik_face();
        let kept: Vec<u16> = alloc::vec![0, 1, 2];
        let bytes = subset_gvar(&face, &kept).unwrap().unwrap();
        let parsed = ParsedGvar::parse(&bytes).unwrap();
        assert_eq!(parsed.glyph_count(), 3);
        assert_eq!(
            parsed.axis_count(),
            face.gvar().unwrap().unwrap().axis_count()
        );
    }

    /// Wraps `gvar` in an SFNT whose only table it is.
    fn font_with_gvar(gvar: Vec<u8>) -> Vec<u8> {
        crate::sfnt::build(0x0001_0000, &[(tag::GVAR, gvar)])
    }

    /// gvar header with the given counts and offsets.
    fn gvar_header(
        axis_count: u16,
        shared_tuple_count: u16,
        shared_tuples_off: u32,
        glyph_count: u16,
        long_offsets: bool,
        data_array_off: u32,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&axis_count.to_be_bytes());
        out.extend_from_slice(&shared_tuple_count.to_be_bytes());
        out.extend_from_slice(&shared_tuples_off.to_be_bytes());
        out.extend_from_slice(&glyph_count.to_be_bytes());
        out.extend_from_slice(&u16::from(long_offsets).to_be_bytes());
        out.extend_from_slice(&data_array_off.to_be_bytes());
        out
    }

    #[test]
    fn shared_tuple_list_past_end_is_rejected_before_allocating() {
        // axisCount and sharedTupleCount at their maximum claim an
        // 8.6 GB shared tuple list that the table does not hold. The
        // output buffer used to be sized from that claim.
        let mut gvar = gvar_header(u16::MAX, u16::MAX, 24, 1, false, 24);
        gvar.extend_from_slice(&[0, 0, 0, 0]); // offsets[0..=1]
        let font = font_with_gvar(gvar);
        let face = Face::parse_bytes(&font, 0).unwrap();
        let r = subset_gvar(&face, &[0]);
        assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
    }

    #[test]
    fn glyphs_sharing_one_data_range_are_rejected() {
        // 2000 glyphs whose offsets alternate 0, 100000, 0, ... so every
        // other glyph claims the same 100 KB body. Copying it once per
        // glyph multiplied the table size by a thousand.
        const GLYPHS: u16 = 2000;
        const BODY: u32 = 100_000;
        let data_array_off = 20 + (u32::from(GLYPHS) + 1) * 4;
        let mut gvar = gvar_header(1, 0, 0, GLYPHS, true, data_array_off);
        for i in 0..=u32::from(GLYPHS) {
            let off = if i % 2 == 0 { 0 } else { BODY };
            gvar.extend_from_slice(&off.to_be_bytes());
        }
        gvar.resize(gvar.len() + BODY as usize, 0);
        let font = font_with_gvar(gvar);
        let face = Face::parse_bytes(&font, 0).unwrap();
        let kept: Vec<u16> = (0..GLYPHS).collect();
        let r = subset_gvar(&face, &kept);
        assert!(matches!(r, Err(SubsetError::Unsupported(_))), "{r:?}");
    }
}
