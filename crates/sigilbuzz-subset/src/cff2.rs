//! CFF2 subsetting helpers.
//!
//! CFF2 is CFF1 trimmed: no Name INDEX, no String INDEX, no Encoding,
//! no charset, no `endchar` operator. Only one Top DICT, stored
//! directly (no enclosing INDEX). VariationStore is optional. Every
//! CFF2 font is implicitly CID-keyed (FDArray mandatory). FDSelect is
//! conventionally required, but Adobe's CFF2 emitter elides it when
//! a font carries a single FontDict. The [`parse_cff2`] reader
//! synthesizes the implicit "every gid -> FD 0" mapping in that case.
//!
//! The Type 2 charstring scanner from [`crate::cff`] already accepts
//! both flavors: it stops at `OP_RETURN` / `OP_ENDCHAR` /
//! end-of-stream, and recognizes `vsindex` / `blend` so CFF2-specific
//! ops don't confuse the operand-stack tracking. The byte-level emitter
//! primitives ([`crate::cff::encode_index`],
//! [`crate::cff::encode_dict_int`], [`crate::cff::renumber_charstring`])
//! are shared with CFF1. CFF2's smaller surface (no Name / String /
//! Encoding / charset INDEXes) means the emitter is a strict subset of
//! the CFF1 layout pass.
//!
//! # Header
//!
//! ```text
//!   u8   major = 2
//!   u8   minor
//!   u8   hdrSize
//!   u16  topDictLength
//! ```
//!
//! Top DICT follows immediately. CharStrings INDEX, Global Subr INDEX,
//! FDArray, FDSelect, and the VariationStore are referenced by absolute
//! offset from that dict.

use alloc::vec::Vec;

use crate::cff::{
    encode_dict_offset_placeholder, parse_fd_select, read_index_cff2, walk_dict, DictEntry,
    OP_CHARSTRINGS, OP_FD_ARRAY, OP_FD_SELECT, OP_PRIVATE, OP_SUBRS, OP_VSTORE,
};
use crate::SubsetError;

mod bake;
mod partial;
mod subset;

pub use bake::bake_at_coords;
pub(crate) use partial::bake_cff2_partial;
pub use subset::subset_non_identity;

/// CFF2 Top DICT placeholder slots.
///
/// CFF2 always carries CharStrings (op 17), FDArray (op 12 36),
/// FDSelect (op 12 37), and optionally VariationStore (op 24). All four
/// are absolute offsets from the start of the table, patched after
/// downstream sections land.
#[derive(Debug, Default, Clone)]
struct Cff2TopDictSlots {
    char_strings_slot: Option<usize>,
    fd_array_slot: Option<usize>,
    fd_select_slot: Option<usize>,
    vstore_slot: Option<usize>,
}

/// Serializes the CFF2 Top DICT body. CharStrings (17), FDArray (12 36),
/// FDSelect (12 37), and VariationStore (24) get 5-byte placeholders.
/// Other operators ride through verbatim.
fn serialise_cff2_top_dict(entries: &[DictEntry]) -> (Vec<u8>, Cff2TopDictSlots) {
    let mut out = Vec::new();
    let mut slots = Cff2TopDictSlots::default();
    for e in entries {
        match e.op {
            OP_CHARSTRINGS => {
                slots.char_strings_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(17);
            }
            OP_FD_ARRAY => {
                slots.fd_array_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(12);
                out.push(0x24);
            }
            OP_FD_SELECT => {
                slots.fd_select_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(12);
                out.push(0x25);
            }
            OP_VSTORE => {
                slots.vstore_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(24);
            }
            _ => {
                for o in &e.operands {
                    out.extend_from_slice(&o.raw);
                }
                if e.op >= 0x0C00 {
                    out.push(12);
                    out.push(e.op as u8);
                } else {
                    out.push(e.op as u8);
                }
            }
        }
    }
    (out, slots)
}

/// Captures the source CFF2 layout in raw form.
struct ParsedCff2<'a> {
    /// Header bytes (5+ bytes). Reproduced into the output with a
    /// rewritten topDictLength field.
    hdr_size: usize,
    /// Source top DICT body bytes.
    top_dict: &'a [u8],
    /// Charstrings INDEX entries.
    char_strings: Vec<&'a [u8]>,
    /// Global Subr INDEX entries.
    global_subrs: Vec<&'a [u8]>,
    /// FDArray INDEX entries (Font DICT bodies).
    fd_array: Vec<&'a [u8]>,
    /// Per-FD: (Private DICT bytes, Local Subrs INDEX entries).
    per_fd_private: Vec<&'a [u8]>,
    per_fd_local_subrs: Vec<Vec<&'a [u8]>>,
    /// FDSelect parsed into per-gid FD indices.
    fd_select: Vec<u8>,
    /// VariationStore bytes (the u16-length-prefixed payload, when
    /// present). Includes the 2-byte length prefix.
    vstore_blob: Option<&'a [u8]>,
}

fn parse_cff2(data: &[u8]) -> Result<ParsedCff2<'_>, SubsetError> {
    if data.len() < 5 {
        return Err(SubsetError::Unsupported("CFF2 header truncated"));
    }
    let major = data[0];
    if major != 2 {
        return Err(SubsetError::Unsupported("CFF2 major version != 2"));
    }
    let hdr_size = data[2] as usize;
    if hdr_size < 5 {
        return Err(SubsetError::Unsupported("CFF2 hdrSize < 5"));
    }
    let top_dict_length = u16::from_be_bytes([data[3], data[4]]) as usize;
    if data.len() < hdr_size + top_dict_length {
        return Err(SubsetError::Unsupported("CFF2 Top DICT past end"));
    }
    let top_dict = &data[hdr_size..hdr_size + top_dict_length];

    // Walk Top DICT for offsets.
    let entries = walk_dict(top_dict)?;
    let mut cs_off: Option<u32> = None;
    let mut fd_array_off: Option<u32> = None;
    let mut fd_select_off: Option<u32> = None;
    let mut vstore_off: Option<u32> = None;
    for e in &entries {
        match e.op {
            OP_CHARSTRINGS => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        cs_off = Some(v as u32);
                    }
                }
            }
            OP_FD_ARRAY => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        fd_array_off = Some(v as u32);
                    }
                }
            }
            OP_FD_SELECT => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        fd_select_off = Some(v as u32);
                    }
                }
            }
            OP_VSTORE => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        vstore_off = Some(v as u32);
                    }
                }
            }
            _ => {}
        }
    }

    // Global Subr INDEX is immediately after the Top DICT.
    let g_pos = hdr_size + top_dict_length;
    let (global_subrs, _) = read_index_cff2(data, g_pos)?;

    let cs_off = cs_off.ok_or(SubsetError::Unsupported(
        "CFF2 Top DICT missing CharStrings",
    ))? as usize;
    let (char_strings, _) = read_index_cff2(data, cs_off)?;
    let n_glyphs = char_strings.len();

    let fd_array_off =
        fd_array_off.ok_or(SubsetError::Unsupported("CFF2 Top DICT missing FDArray"))? as usize;
    let (fd_array, _) = read_index_cff2(data, fd_array_off)?;

    // Walk each Font DICT for its Private offset.
    let mut per_fd_private: Vec<&[u8]> = Vec::with_capacity(fd_array.len());
    let mut per_fd_local_subrs: Vec<Vec<&[u8]>> = Vec::with_capacity(fd_array.len());
    for fd_bytes in &fd_array {
        let fd_entries = walk_dict(fd_bytes)?;
        let mut priv_info: Option<(u32, u32)> = None;
        for e in &fd_entries {
            if e.op == OP_PRIVATE && e.operands.len() >= 2 {
                let s = e.operands[e.operands.len() - 2].int_value;
                let o = e.operands[e.operands.len() - 1].int_value;
                if let (Some(sv), Some(ov)) = (s, o) {
                    if sv >= 0 && ov >= 0 {
                        priv_info = Some((sv as u32, ov as u32));
                    }
                }
            }
        }
        let (priv_bytes, locals) = if let Some((size, off)) = priv_info {
            let off_u = off as usize;
            let size_u = size as usize;
            if off_u + size_u > data.len() {
                return Err(SubsetError::Unsupported("CFF2 Private DICT past end"));
            }
            let priv_bytes = &data[off_u..off_u + size_u];
            let priv_entries = walk_dict(priv_bytes)?;
            let mut subrs_rel: Option<u32> = None;
            for e in &priv_entries {
                if e.op == OP_SUBRS {
                    if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                        if v >= 0 {
                            subrs_rel = Some(v as u32);
                        }
                    }
                }
            }
            if let Some(rel) = subrs_rel {
                let abs = off_u + rel as usize;
                let (locals, _) = read_index_cff2(data, abs)?;
                (priv_bytes, locals)
            } else {
                (priv_bytes, Vec::new())
            }
        } else {
            (&[][..], Vec::new())
        };
        per_fd_private.push(priv_bytes);
        per_fd_local_subrs.push(locals);
    }

    // Adobe's CFF2 builds elide FDSelect when the font has a single
    // FontDict. The spec marks FDSelect optional in that case. When
    // missing AND the FDArray has exactly one entry, synthesize the
    // implicit "every gid -> FD 0" mapping; any other shape is a
    // malformed CFF2 (multi-FD without FDSelect cannot
    // round-trip).
    let fd_select = if let Some(off) = fd_select_off {
        parse_fd_select(data, off as usize, n_glyphs)?
    } else if fd_array.len() == 1 {
        alloc::vec![0u8; n_glyphs]
    } else {
        return Err(SubsetError::Unsupported(
            "CFF2 multi-FD source missing FDSelect",
        ));
    };

    let vstore_blob = if let Some(off) = vstore_off {
        let off_u = off as usize;
        if off_u + 2 > data.len() {
            return Err(SubsetError::Unsupported("CFF2 VariationStore truncated"));
        }
        let len = u16::from_be_bytes([data[off_u], data[off_u + 1]]) as usize;
        let end = off_u + 2 + len;
        if end > data.len() {
            return Err(SubsetError::Unsupported("CFF2 VariationStore past end"));
        }
        Some(&data[off_u..end])
    } else {
        None
    };

    Ok(ParsedCff2 {
        hdr_size,
        top_dict,
        char_strings,
        global_subrs,
        fd_array,
        per_fd_private,
        per_fd_local_subrs,
        fd_select,
        vstore_blob,
    })
}

// Type 2 op codes consumed by the baker. Duplicates of crate::cff
// constants kept private to this module. The baker only reads, never
// renumbers.
const OP_HSTEM: u8 = 1;
const OP_VSTEM: u8 = 3;
const OP_VMOVETO: u8 = 4;
const OP_RLINETO: u8 = 5;
const OP_HLINETO: u8 = 6;
const OP_VLINETO: u8 = 7;
const OP_RRCURVETO: u8 = 8;
const OP_CALLSUBR: u8 = 10;
const OP_RETURN: u8 = 11;
const OP_ESCAPE: u8 = 12;
const OP_VSINDEX: u8 = 15;
const OP_BLEND: u8 = 16;
const OP_HSTEMHM: u8 = 18;
const OP_HINTMASK: u8 = 19;
const OP_CNTRMASK: u8 = 20;
const OP_RMOVETO: u8 = 21;
const OP_HMOVETO: u8 = 22;
const OP_VSTEMHM: u8 = 23;
const OP_RCURVELINE: u8 = 24;
const OP_RLINECURVE: u8 = 25;
const OP_VVCURVETO: u8 = 26;
const OP_HHCURVETO: u8 = 27;
const OP_SHORTINT: u8 = 28;
const OP_CALLGSUBR: u8 = 29;
const OP_VHCURVETO: u8 = 30;
const OP_HVCURVETO: u8 = 31;

const MAX_BAKE_DEPTH: u8 = 10;

/// Decodes a single push operand at `data[pos..]` into an f32 plus
/// byte-length. Mirrors `crate::cff::decode_operand` but preserves
/// fractional values from the 16.16-fixed (b0=255) form.
fn decode_operand_f32(data: &[u8], pos: usize) -> Option<(f32, usize)> {
    let b0 = *data.get(pos)?;
    if (32..=246).contains(&b0) {
        Some(((i32::from(b0) - 139) as f32, 1))
    } else if (247..=250).contains(&b0) {
        let b1 = *data.get(pos + 1)?;
        Some((
            ((i32::from(b0) - 247) * 256 + i32::from(b1) + 108) as f32,
            2,
        ))
    } else if (251..=254).contains(&b0) {
        let b1 = *data.get(pos + 1)?;
        Some((
            (-(i32::from(b0) - 251) * 256 - i32::from(b1) - 108) as f32,
            2,
        ))
    } else if b0 == OP_SHORTINT {
        // Type 2 shortint: 2-byte big-endian i16 follows. Required for
        // any integer in `[-32768, -1132]` or `[1132, 32767]` (#197).
        // Omitting this branch breaks fonts with >= 1240 subrs whose
        // call indices spill into shortint encoding.
        let b1 = *data.get(pos + 1)?;
        let b2 = *data.get(pos + 2)?;
        let raw = i16::from_be_bytes([b1, b2]);
        Some((f32::from(raw), 3))
    } else if b0 == 255 {
        let bytes = data.get(pos + 1..pos + 5)?;
        let raw = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        Some((raw as f32 / 65536.0, 5))
    } else {
        None
    }
}

/// Encodes a numeric value as a Type 2 push, using the shortest valid
/// form for the integer case and the 16.16 fixed form when fractional.
fn encode_charstring_number(v: f32, out: &mut Vec<u8>) {
    // If `v` is an exact integer in the i16 range, use an integer push.
    let rounded = v.round();
    let is_integer = (v - rounded).abs() < 1e-6;
    if is_integer && (-32768.0..=32767.0).contains(&rounded) {
        #[allow(clippy::cast_possible_truncation)]
        let iv = rounded as i32;
        if (-107..=107).contains(&iv) {
            out.push((iv + 139) as u8);
        } else if (108..=1131).contains(&iv) {
            let v0 = iv - 108;
            out.push(((v0 >> 8) + 247) as u8);
            out.push((v0 & 0xff) as u8);
        } else if (-1131..=-108).contains(&iv) {
            let v0 = -iv - 108;
            out.push(((v0 >> 8) + 251) as u8);
            out.push((v0 & 0xff) as u8);
        } else {
            #[allow(clippy::cast_possible_truncation)]
            let bytes = (iv as i16).to_be_bytes();
            out.push(OP_SHORTINT);
            out.push(bytes[0]);
            out.push(bytes[1]);
        }
    } else {
        // 16.16 fixed.
        #[allow(clippy::cast_possible_truncation)]
        let raw = (v * 65536.0).round() as i32;
        out.push(255);
        out.extend_from_slice(&raw.to_be_bytes());
    }
}

fn subr_bias(count: usize) -> i32 {
    if count < 1240 {
        107
    } else if count < 33_900 {
        1131
    } else {
        32_768
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
mod tests;
