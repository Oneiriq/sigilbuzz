//! Glyph-id closure walker.
//!
//! Starting from the caller's gid set, we expand transitively through
//! composite-glyph references in `glyf`. Ligature components in GSUB
//! type 4 and mark anchors in GPOS 4/5/6 *would* land here too, but
//! the first pass of the subsetter drops GSUB/GPOS entirely so those
//! references cannot keep glyphs alive — there is no point pulling
//! them in.
//!
//! Glyph 0 (`.notdef`) is always retained: every SFNT font has one,
//! every glyph index that fails a cmap lookup falls back to it, and
//! every TrueType-outlined font's first glyf entry is reserved for it.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::tables::Reader;
use sigilbuzz::Face;

use crate::SubsetError;

/// Computes the closure of `seed` over the source font's composite-
/// reference graph. The returned vec is sorted ascending and contains
/// gid 0 even when `seed` does not.
pub fn compute_closure(face: &Face<'_>, seed: &[u16]) -> Result<Vec<u16>, SubsetError> {
    let num_glyphs = face.maxp()?.num_glyphs;

    // Bitset for membership: a Vec<bool> sized to num_glyphs is
    // O(numGlyphs) in memory but lookups are O(1) and writes are
    // deterministic — no HashMap iteration order to worry about.
    let mut keep = alloc::vec![false; num_glyphs as usize];
    keep[0] = true;
    for &g in seed {
        if (g as usize) < keep.len() {
            keep[g as usize] = true;
        }
    }

    // If the source has glyf, walk composites until we hit a fixed
    // point. A worklist plus a visited bit avoids re-walking glyphs.
    if face.record(tag::GLYF).is_some() && face.record(tag::LOCA).is_some() {
        let loca = face.loca()?;
        let glyf_bytes = face.table_bytes(tag::GLYF).map_err(SubsetError::from)?;
        let mut stack: Vec<u16> = Vec::new();
        for (gid, &k) in keep.iter().enumerate() {
            if k {
                stack.push(gid as u16);
            }
        }

        while let Some(g) = stack.pop() {
            let Some((start, end)) = loca.range(g) else {
                continue;
            };
            if start == end {
                continue;
            }
            let body = glyf_bytes
                .get(start as usize..end as usize)
                .ok_or(SubsetError::Unsupported("glyf offset past end"))?;
            for child in composite_components(body)? {
                if (child as usize) < keep.len() && !keep[child as usize] {
                    keep[child as usize] = true;
                    stack.push(child);
                }
            }
        }
    }

    let mut out: Vec<u16> = keep
        .iter()
        .enumerate()
        .filter_map(|(i, &k)| if k { Some(i as u16) } else { None })
        .collect();
    out.sort_unstable();
    Ok(out)
}

/// Returns the gids of every component referenced by a composite
/// glyph. Returns an empty vec for simple glyphs and zero-byte
/// (whitespace) glyphs.
fn composite_components(body: &[u8]) -> Result<Vec<u16>, SubsetError> {
    if body.len() < 10 {
        return Ok(Vec::new());
    }
    let mut r = Reader::new(body);
    // numberOfContours: negative => composite.
    let num_contours = r
        .read_i16()
        .map_err(|_| SubsetError::Unsupported("glyf header truncated"))?;
    if num_contours >= 0 {
        return Ok(Vec::new());
    }
    // Skip xMin/yMin/xMax/yMax.
    r.skip(8)
        .map_err(|_| SubsetError::Unsupported("glyf header truncated"))?;

    let mut out = Vec::new();
    // Composite-glyph flag bits we need to walk argument widths.
    const COMP_ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
    const COMP_ARGS_ARE_XY_VALUES: u16 = 0x0002;
    const COMP_WE_HAVE_A_SCALE: u16 = 0x0008;
    const COMP_MORE_COMPONENTS: u16 = 0x0020;
    const COMP_WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
    const COMP_WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
    const COMP_WE_HAVE_INSTRUCTIONS: u16 = 0x0100;

    loop {
        let flags = r
            .read_u16()
            .map_err(|_| SubsetError::Unsupported("composite flags truncated"))?;
        let component = r
            .read_u16()
            .map_err(|_| SubsetError::Unsupported("composite glyph index truncated"))?;
        out.push(component);

        // Skip the args and any 2x2 transform.
        if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
            r.skip(4)
                .map_err(|_| SubsetError::Unsupported("composite args truncated"))?;
        } else if flags & COMP_ARGS_ARE_XY_VALUES != 0 {
            r.skip(2)
                .map_err(|_| SubsetError::Unsupported("composite args truncated"))?;
        } else {
            r.skip(2)
                .map_err(|_| SubsetError::Unsupported("composite args truncated"))?;
        }

        if flags & COMP_WE_HAVE_A_SCALE != 0 {
            r.skip(2)
                .map_err(|_| SubsetError::Unsupported("composite scale truncated"))?;
        } else if flags & COMP_WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            r.skip(4)
                .map_err(|_| SubsetError::Unsupported("composite scales truncated"))?;
        } else if flags & COMP_WE_HAVE_A_TWO_BY_TWO != 0 {
            r.skip(8)
                .map_err(|_| SubsetError::Unsupported("composite 2x2 truncated"))?;
        }

        if flags & COMP_MORE_COMPONENTS == 0 {
            // Last component — instructions (if present) follow but
            // we don't care about them in the closure pass.
            let _ = flags & COMP_WE_HAVE_INSTRUCTIONS;
            break;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_components_empty_for_simple_glyph() {
        // A minimal simple glyph: numContours=1 + bbox + endpts +
        // instructionLength=0 + flags(1) + x(0) + y(0).
        let mut body = Vec::new();
        body.extend_from_slice(&1i16.to_be_bytes()); // numContours
        body.extend_from_slice(&[0u8; 8]); // bbox
        body.extend_from_slice(&0u16.to_be_bytes()); // endPtsOfContours[0] = 0
        body.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
        body.push(0); // flags
        body.push(0); // x
        body.push(0); // y
        let comps = composite_components(&body).unwrap();
        assert!(comps.is_empty());
    }

    #[test]
    fn composite_components_walks_two_children() {
        // numContours=-1, bbox, then two components in XY mode with
        // word args.
        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes()); // numContours
        body.extend_from_slice(&[0u8; 8]); // bbox

        // First component: flags = ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES
        // | MORE_COMPONENTS = 0x0023, gid = 7, args = (10, 20).
        body.extend_from_slice(&0x0023u16.to_be_bytes());
        body.extend_from_slice(&7u16.to_be_bytes());
        body.extend_from_slice(&10i16.to_be_bytes());
        body.extend_from_slice(&20i16.to_be_bytes());

        // Second component: flags = ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES
        // = 0x0003 (no MORE_COMPONENTS), gid = 9, args = (5, 6).
        body.extend_from_slice(&0x0003u16.to_be_bytes());
        body.extend_from_slice(&9u16.to_be_bytes());
        body.extend_from_slice(&5i16.to_be_bytes());
        body.extend_from_slice(&6i16.to_be_bytes());

        let comps = composite_components(&body).unwrap();
        assert_eq!(comps, alloc::vec![7, 9]);
    }

    #[test]
    fn composite_components_skips_two_by_two_block() {
        // numContours=-1, bbox, one component with WE_HAVE_A_TWO_BY_TWO.
        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&[0u8; 8]);

        // flags: ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES | WE_HAVE_A_TWO_BY_TWO
        // (no MORE_COMPONENTS) = 0x0083.
        body.extend_from_slice(&0x0083u16.to_be_bytes());
        body.extend_from_slice(&42u16.to_be_bytes());
        body.extend_from_slice(&[0u8; 4]); // word args
        body.extend_from_slice(&[0u8; 8]); // 2x2

        let comps = composite_components(&body).unwrap();
        assert_eq!(comps, alloc::vec![42]);
    }
}
