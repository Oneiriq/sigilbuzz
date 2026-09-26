//! `glyf` + `loca` subsetting.
//!
//! Composite glyphs in the source carry a 16-bit `glyphIndex` field
//! per component that points at another gid. When we keep a composite
//! we must rewrite each of those references to the component's
//! *new* gid. The byte slice is otherwise copied verbatim.
//!
//! Hint instructions (the simple-glyph `instructions[...]` byte run
//! and any composite glyph's tail when `WE_HAVE_INSTRUCTIONS` is set)
//! are stripped when [`crate::SubsetInput::retain_hints`] is false.
//! Hints reference state from `cvt`/`fpgm`/`prep`, which the subset
//! keeps only when `retain_hints` is set. Removing hints does not
//! affect non-hinted rendering.
//!
//! `loca` is rebuilt from the new glyph offsets. Short loca holds
//! `u16` offsets divided by two and tops out at 128 KiB; long loca
//! is `u32` and unrestricted. We pick the smallest format that fits.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::tables::Reader;
use sigilbuzz::Face;

use crate::SubsetError;

/// Result of glyf+loca subsetting: both byte buffers plus the loca
/// format flag (false = short, true = long) so the caller can write
/// it back into `head.indexToLocFormat`.
pub struct GlyfLoca {
    /// New `glyf` bytes, in new-gid order, padded to 4-byte alignment
    /// per the spec hint.
    pub glyf: Vec<u8>,
    /// New `loca` bytes, in the chosen format.
    pub loca: Vec<u8>,
    /// True when long-format loca was selected.
    pub long_loca: bool,
}

const COMP_ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
const COMP_WE_HAVE_A_SCALE: u16 = 0x0008;
const COMP_MORE_COMPONENTS: u16 = 0x0020;
const COMP_WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const COMP_WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
const COMP_WE_HAVE_INSTRUCTIONS: u16 = 0x0100;

/// Subsets `glyf`+`loca` for the given new-gid order. `kept[new]` is
/// the source gid that lands in slot `new` of the output.
///
/// The rebuilt table never outgrows the source `glyf` by more than one
/// padding byte per glyph: each kept glyph copies the bytes between two
/// neighboring `loca` offsets, the rewrites only ever shrink a glyph,
/// and a `loca` whose offsets are not ascending has a glyph whose end
/// precedes its start, which is reported instead of copied.
pub fn subset_glyf_loca(
    face: &Face<'_>,
    kept: &[u16],
    retain_hints: bool,
) -> Result<GlyfLoca, SubsetError> {
    let loca = face.loca()?;
    let glyf_bytes = face.table_bytes(tag::GLYF).map_err(SubsetError::from)?;

    // Source gid -> new gid. kept is in old-order so the mapping is
    // a simple linear-time enumeration. Gids past the font's glyph
    // count have no outline, so they stay unmapped.
    let mut old_to_new = alloc::vec![None::<u16>; loca.num_glyphs() as usize];
    for (new, &old) in kept.iter().enumerate() {
        if let Some(slot) = old_to_new.get_mut(old as usize) {
            *slot = Some(new as u16);
        }
    }

    // Build new glyph bodies in new-gid order. Each body is the
    // re-encoded slice of the source glyf for that gid: simple
    // glyphs pass through unchanged (modulo hints removal),
    // composites rewrite their per-component glyphIndex words.
    //
    // Each body is padded to 2-byte alignment so short-loca offsets
    // divide cleanly. This is what HarfBuzz does and matches what
    // every major font tool produces.
    let mut new_bodies: Vec<Vec<u8>> = Vec::with_capacity(kept.len());
    for &old_gid in kept {
        let body = match loca.range(old_gid) {
            Some((s, e)) if s != e => glyf_bytes
                .get(s as usize..e as usize)
                .ok_or(SubsetError::Unsupported("glyf range outside table"))?,
            _ => &[][..],
        };
        let mut new_body = rewrite_glyph(body, &old_to_new, retain_hints)?;
        if new_body.len() % 2 != 0 {
            new_body.push(0);
        }
        new_bodies.push(new_body);
    }

    // Compute per-glyph offsets and decide loca format.
    let mut offsets: Vec<u32> = Vec::with_capacity(kept.len() + 1);
    let mut cursor: u32 = 0;
    offsets.push(0);
    for body in &new_bodies {
        cursor = u32::try_from(body.len())
            .ok()
            .and_then(|len| cursor.checked_add(len))
            .ok_or(SubsetError::Unsupported("glyf overflow"))?;
        offsets.push(cursor);
    }

    // Short loca caps at 0x1FFFE bytes (last offset / 2 <= u16::MAX).
    let long_loca = cursor > 0x1_FFFE;

    let mut glyf_out = Vec::with_capacity(cursor as usize);
    for body in &new_bodies {
        glyf_out.extend_from_slice(body);
    }
    // Final 4-byte alignment pad on glyf is conventional.
    while glyf_out.len() % 4 != 0 {
        glyf_out.push(0);
    }

    let loca_out = if long_loca {
        let mut out = Vec::with_capacity(offsets.len() * 4);
        for o in &offsets {
            out.extend_from_slice(&o.to_be_bytes());
        }
        out
    } else {
        let mut out = Vec::with_capacity(offsets.len() * 2);
        for o in &offsets {
            // Every offset is at most 0x1FFFE here, so half fits u16.
            let half = (*o / 2) as u16;
            out.extend_from_slice(&half.to_be_bytes());
        }
        out
    };

    Ok(GlyfLoca {
        glyf: glyf_out,
        loca: loca_out,
        long_loca,
    })
}

/// Rewrites a single glyph body. Simple glyphs just have their hints
/// removed when requested; composites get their per-component gid
/// references rewritten through `old_to_new`.
fn rewrite_glyph(
    body: &[u8],
    old_to_new: &[Option<u16>],
    retain_hints: bool,
) -> Result<Vec<u8>, SubsetError> {
    if body.is_empty() {
        return Ok(Vec::new());
    }
    if body.len() < 10 {
        return Err(SubsetError::Unsupported("glyf body shorter than 10 bytes"));
    }
    // Peek numContours.
    let nc = i16::from_be_bytes([body[0], body[1]]);
    if nc >= 0 {
        rewrite_simple(body, retain_hints)
    } else {
        rewrite_composite(body, old_to_new, retain_hints)
    }
}

fn rewrite_simple(body: &[u8], retain_hints: bool) -> Result<Vec<u8>, SubsetError> {
    if retain_hints {
        return Ok(body.to_vec());
    }
    // We need to splice out the instruction stream:
    //   header (10) + endPtsOfContours[numContours] (2*nc)
    //   + instructionLength (u16) + instructions[instructionLength]
    //   + flags + xCoords + yCoords
    let mut r = Reader::new(body);
    let nc = r
        .read_i16()
        .map_err(|_| SubsetError::Unsupported("simple glyph header"))?;
    let nc = usize::try_from(nc).map_err(|_| SubsetError::Unsupported("simple glyph header"))?;
    r.skip(8).map_err(|_| SubsetError::Unsupported("bbox"))?;
    r.skip(nc * 2)
        .map_err(|_| SubsetError::Unsupported("endPtsOfContours"))?;
    let instr_len_off = r.position();
    let instr_len = r
        .read_u16()
        .map_err(|_| SubsetError::Unsupported("instructionLength"))? as usize;
    let instr_start = r.position();
    let instr_end = instr_start
        .checked_add(instr_len)
        .ok_or(SubsetError::Unsupported("instr overflow"))?;
    let (Some(head), Some(tail)) = (body.get(..instr_len_off), body.get(instr_end..)) else {
        return Err(SubsetError::Unsupported("instructions past body end"));
    };

    let mut out = Vec::with_capacity(head.len() + 2 + tail.len());
    // Header + endpts unchanged.
    out.extend_from_slice(head);
    // Zero instruction length.
    out.extend_from_slice(&0u16.to_be_bytes());
    // Skip instructions, copy the rest verbatim (flags + coord
    // streams are independent of the instruction body).
    out.extend_from_slice(tail);
    Ok(out)
}

fn rewrite_composite(
    body: &[u8],
    old_to_new: &[Option<u16>],
    retain_hints: bool,
) -> Result<Vec<u8>, SubsetError> {
    let truncated = |what: &'static str| SubsetError::Unsupported(what);
    let mut out = Vec::with_capacity(body.len());
    // Copy header (10 bytes) verbatim.
    out.extend_from_slice(
        body.get(..10)
            .ok_or(truncated("composite header truncated"))?,
    );

    let mut cursor = 10usize;
    // Flags of the last component, and where they were written in `out`.
    let (last_flags, last_flags_pos) = loop {
        let (Some(flags), Some(component)) = (
            crate::layout::read_u16(body, cursor),
            crate::layout::read_u16(body, cursor + 2),
        ) else {
            return Err(truncated("composite component truncated"));
        };

        // Look up the new gid. The closure walker keeps every
        // component inside the font, so a miss means the component
        // gid is past `numGlyphs` (or the walk hit its work limit).
        let new_gid = old_to_new
            .get(component as usize)
            .copied()
            .flatten()
            .ok_or(SubsetError::Unsupported(
                "composite references a glyph outside the subset",
            ))?;

        // Emit flags + new gid.
        let flags_pos = out.len();
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&new_gid.to_be_bytes());
        cursor += 4;

        // Args: 4 bytes if WORDS, else 2 bytes.
        let args_len = if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        let args = body
            .get(cursor..cursor + args_len)
            .ok_or(truncated("composite args truncated"))?;
        out.extend_from_slice(args);
        cursor += args_len;

        // Optional 2x2 / scale block.
        let xform_len = if flags & COMP_WE_HAVE_A_SCALE != 0 {
            2
        } else if flags & COMP_WE_HAVE_AN_X_AND_Y_SCALE != 0 {
            4
        } else if flags & COMP_WE_HAVE_A_TWO_BY_TWO != 0 {
            8
        } else {
            0
        };
        let xform = body
            .get(cursor..cursor + xform_len)
            .ok_or(truncated("composite xform truncated"))?;
        out.extend_from_slice(xform);
        cursor += xform_len;

        if flags & COMP_MORE_COMPONENTS == 0 {
            break (flags, flags_pos);
        }
    };

    // Composite trailing instructions: only present when the last
    // component's flags carry WE_HAVE_INSTRUCTIONS. Drop them when
    // not retaining hints; otherwise copy verbatim.
    if last_flags & COMP_WE_HAVE_INSTRUCTIONS != 0 {
        let instr_len = crate::layout::read_u16(body, cursor)
            .ok_or(truncated("composite instr len truncated"))? as usize;
        let instr_end = cursor
            .checked_add(2)
            .and_then(|c| c.checked_add(instr_len))
            .ok_or(truncated("composite instr overflow"))?;
        let instructions = body
            .get(cursor..instr_end)
            .ok_or(truncated("composite instructions past body"))?;
        if retain_hints {
            // The last flags word already carries WE_HAVE_INSTRUCTIONS.
            out.extend_from_slice(instructions);
        } else {
            // Clear WE_HAVE_INSTRUCTIONS in the last emitted flags word
            // and leave out both the length word and its instructions.
            let cleared = last_flags & !COMP_WE_HAVE_INSTRUCTIONS;
            out.get_mut(last_flags_pos..last_flags_pos + 2)
                .ok_or(truncated("composite flags out of range"))?
                .copy_from_slice(&cleared.to_be_bytes());
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_simple_strips_instructions() {
        // Simple glyph: numContours=1, bbox, endPts=[0],
        // instructionLength=3, instructions=[0xCA,0xFE,0xFE], flags=0x01,
        // x=0, y=0.
        let mut body = Vec::new();
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 8]);
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&3u16.to_be_bytes());
        body.extend_from_slice(&[0xCA, 0xFE, 0xFE]);
        body.push(0x01);
        body.push(0x00);
        body.push(0x00);

        let out = rewrite_simple(&body, false).unwrap();
        // Reparse and confirm instructionLength = 0.
        let il = u16::from_be_bytes([out[12], out[13]]);
        assert_eq!(il, 0);
        // Trailing flags+coord bytes preserved.
        assert_eq!(&out[14..], &[0x01, 0x00, 0x00]);
    }

    #[test]
    fn rewrite_simple_retain_hints_passes_through() {
        let mut body = Vec::new();
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 8]);
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&3u16.to_be_bytes());
        body.extend_from_slice(&[0xCA, 0xFE, 0xFE]);
        body.push(0x01);
        body.push(0x00);
        body.push(0x00);
        let out = rewrite_simple(&body, true).unwrap();
        assert_eq!(out, body);
    }

    #[test]
    fn rewrite_composite_remaps_component_gid() {
        // Composite: numContours=-1, bbox, one component with
        // ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES = 0x0003
        // (no MORE_COMPONENTS, no instructions), gid=5, args=(0,0).
        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&[0u8; 8]);
        body.extend_from_slice(&0x0003u16.to_be_bytes());
        body.extend_from_slice(&5u16.to_be_bytes());
        body.extend_from_slice(&[0u8; 4]);

        // Map old gid 5 -> new gid 17.
        let mut map = alloc::vec![None; 16];
        map.push(Some(17u16)); // index 16
                               // Need slot 5 too. Fix sizing.
        let mut map = alloc::vec![None::<u16>; 6];
        map[5] = Some(17);
        let out = rewrite_composite(&body, &map, false).unwrap();
        let new_gid = u16::from_be_bytes([out[12], out[13]]);
        assert_eq!(new_gid, 17);
    }
}
