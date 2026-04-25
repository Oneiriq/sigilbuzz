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
//! Hints reference state from `cvt`/`fpgm`/`prep` which we never
//! preserve, so leaving them in would crash a TT interpreter that
//! tried to run them. Removing hints does not affect non-hinted
//! rendering.
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
const COMP_ARGS_ARE_XY_VALUES: u16 = 0x0002;
const COMP_WE_HAVE_A_SCALE: u16 = 0x0008;
const COMP_MORE_COMPONENTS: u16 = 0x0020;
const COMP_WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
const COMP_WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
const COMP_WE_HAVE_INSTRUCTIONS: u16 = 0x0100;

/// Subsets `glyf`+`loca` for the given new-gid order. `kept[new]` is
/// the source gid that lands in slot `new` of the output.
pub fn subset_glyf_loca(
    face: &Face<'_>,
    kept: &[u16],
    retain_hints: bool,
) -> Result<GlyfLoca, SubsetError> {
    let loca = face.loca()?;
    let glyf_bytes = face.table_bytes(tag::GLYF).map_err(SubsetError::from)?;

    // Source gid -> new gid. kept is in old-order so the mapping is
    // a simple linear-time enumeration.
    let mut old_to_new = alloc::vec![None::<u16>; loca.num_glyphs() as usize];
    for (new, &old) in kept.iter().enumerate() {
        old_to_new[old as usize] = Some(new as u16);
    }

    // Build new glyph bodies in new-gid order. Each body is the
    // re-encoded slice of the source glyf for that gid: simple
    // glyphs pass through unchanged (modulo hints removal),
    // composites rewrite their per-component glyphIndex words.
    let mut new_bodies: Vec<Vec<u8>> = Vec::with_capacity(kept.len());
    for &old_gid in kept {
        let body = match loca.range(old_gid) {
            Some((s, e)) if s != e => glyf_bytes
                .get(s as usize..e as usize)
                .ok_or(SubsetError::Unsupported("glyf range outside table"))?,
            _ => &[][..],
        };
        new_bodies.push(rewrite_glyph(body, &old_to_new, retain_hints)?);
    }

    // Pad each body to 2-byte alignment so short-loca offsets divide
    // cleanly. This is what HarfBuzz does and matches what every
    // major font tool produces.
    for body in &mut new_bodies {
        if body.len() % 2 != 0 {
            body.push(0);
        }
    }

    // Compute per-glyph offsets and decide loca format.
    let mut offsets: Vec<u32> = Vec::with_capacity(kept.len() + 1);
    let mut cursor: u32 = 0;
    offsets.push(0);
    for body in &new_bodies {
        cursor = cursor
            .checked_add(body.len() as u32)
            .ok_or(SubsetError::Unsupported("glyf overflow"))?;
        offsets.push(cursor);
    }

    // Short loca caps at 0x1FFFE bytes (last offset / 2 ≤ u16::MAX).
    let long_loca = *offsets.last().unwrap_or(&0) > 0x1_FFFE;

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
        .map_err(|_| SubsetError::Unsupported("simple glyph header"))? as usize;
    r.skip(8).map_err(|_| SubsetError::Unsupported("bbox"))?;
    let endpts_off = r.position();
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
    if instr_end > body.len() {
        return Err(SubsetError::Unsupported("instructions past body end"));
    }

    let mut out = Vec::with_capacity(body.len() - instr_len);
    // Header + endpts unchanged.
    out.extend_from_slice(&body[..instr_len_off]);
    // Zero instruction length.
    out.extend_from_slice(&0u16.to_be_bytes());
    // Skip instructions, copy the rest verbatim (flags + coord
    // streams are independent of the instruction body).
    out.extend_from_slice(&body[instr_end..]);
    let _ = endpts_off;
    Ok(out)
}

fn rewrite_composite(
    body: &[u8],
    old_to_new: &[Option<u16>],
    retain_hints: bool,
) -> Result<Vec<u8>, SubsetError> {
    let mut out = Vec::with_capacity(body.len());
    // Copy header (10 bytes) verbatim.
    out.extend_from_slice(&body[..10]);

    let mut cursor = 10usize;
    let mut last_flags: u16;
    loop {
        if cursor + 4 > body.len() {
            return Err(SubsetError::Unsupported("composite component truncated"));
        }
        let flags = u16::from_be_bytes([body[cursor], body[cursor + 1]]);
        let component = u16::from_be_bytes([body[cursor + 2], body[cursor + 3]]);

        // Look up the new gid. If a referenced component is dropped
        // we have a problem — that's a closure bug. Fail loudly.
        let new_gid = old_to_new
            .get(component as usize)
            .copied()
            .flatten()
            .ok_or(SubsetError::Unsupported(
                "composite references dropped glyph (closure walker bug)",
            ))?;

        // Emit flags + new gid.
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&new_gid.to_be_bytes());
        cursor += 4;

        // Args: 4 bytes if WORDS, else 2 bytes.
        let args_len = if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
            4
        } else {
            2
        };
        if cursor + args_len > body.len() {
            return Err(SubsetError::Unsupported("composite args truncated"));
        }
        out.extend_from_slice(&body[cursor..cursor + args_len]);
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
        if cursor + xform_len > body.len() {
            return Err(SubsetError::Unsupported("composite xform truncated"));
        }
        out.extend_from_slice(&body[cursor..cursor + xform_len]);
        cursor += xform_len;

        last_flags = flags;
        let _ = COMP_ARGS_ARE_XY_VALUES;
        if flags & COMP_MORE_COMPONENTS == 0 {
            break;
        }
    }

    // Composite trailing instructions: only present when the last
    // component's flags carry WE_HAVE_INSTRUCTIONS. Drop them when
    // not retaining hints; otherwise copy verbatim.
    if last_flags & COMP_WE_HAVE_INSTRUCTIONS != 0 {
        if cursor + 2 > body.len() {
            return Err(SubsetError::Unsupported("composite instr len truncated"));
        }
        let instr_len = u16::from_be_bytes([body[cursor], body[cursor + 1]]) as usize;
        let instr_end = cursor
            .checked_add(2)
            .and_then(|c| c.checked_add(instr_len))
            .ok_or(SubsetError::Unsupported("composite instr overflow"))?;
        if instr_end > body.len() {
            return Err(SubsetError::Unsupported("composite instructions past body"));
        }
        if retain_hints {
            // Patch flags to keep WE_HAVE_INSTRUCTIONS — already
            // emitted above for the last component, so the bit
            // survives.
            out.extend_from_slice(&body[cursor..instr_end]);
        } else {
            // Clear WE_HAVE_INSTRUCTIONS in the last emitted flags
            // word. We know exactly where we wrote it: at offset
            // out.len() - (header + ... ) — recalculating is
            // fiddly, so we patch by scanning for the last
            // occurrence of the bit pattern. Simpler: walk back
            // 4 bytes per flags entry... Actually we kept track of
            // last_flags but not its position; rewrite the whole
            // composite cheaply by recreating with the bit cleared
            // is overkill. Easier: locate the last flags word
            // we wrote (it's right before whatever args+xform we
            // appended, which is `args_len + xform_len + 4` bytes
            // back from the current end, where 4 = the flags+gid).
            // For safety we compute that span explicitly.
            let last_flags_off = out.len()
                - (
                    // args + xform that followed the last flags
                    (if last_flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
                        4
                    } else {
                        2
                    }) + if last_flags & COMP_WE_HAVE_A_SCALE != 0 {
                        2
                    } else if last_flags & COMP_WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                        4
                    } else if last_flags & COMP_WE_HAVE_A_TWO_BY_TWO != 0 {
                        8
                    } else {
                        0
                    } + 4
                    // flags + gid
                );
            let cleared = last_flags & !COMP_WE_HAVE_INSTRUCTIONS;
            out[last_flags_off..last_flags_off + 2].copy_from_slice(&cleared.to_be_bytes());
            // Skip both the length word and its instructions.
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
                               // Need slot 5 too — fix sizing.
        let mut map = alloc::vec![None::<u16>; 6];
        map[5] = Some(17);
        let out = rewrite_composite(&body, &map, false).unwrap();
        let new_gid = u16::from_be_bytes([out[12], out[13]]);
        assert_eq!(new_gid, 17);
    }
}
