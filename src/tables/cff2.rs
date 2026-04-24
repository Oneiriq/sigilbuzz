// CFF2 reuses CFF1's table walking; same relaxations apply.
#![allow(
    clippy::bool_to_int_with_if,
    clippy::elidable_lifetime_names,
    clippy::map_unwrap_or,
    clippy::manual_div_ceil,
    clippy::needless_range_loop,
    clippy::too_many_lines,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::similar_names,
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::needless_bool
)]

//! `CFF2` — CFF for variable fonts.
//!
//! CFF2 is CFF1 with the Name INDEX, String INDEX, Encoding, Charset,
//! and Top DICT INDEX removed; the single Top DICT lives inline in
//! the header. Variation deltas are applied inside the charstring
//! interpreter via the `blend` and `vsindex` operators, reading from
//! an embedded `ItemVariationStore`.
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
//! The Top DICT follows immediately (`topDictLength` bytes). CharStrings
//! INDEX, Global Subr INDEX, FDArray, FDSelect, and the VariationStore
//! are referenced by absolute offset from that dict.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::cff::{read_index, BlendContext};
use crate::tables::outline::OutlineSink;
use crate::tables::parse::Reader;
use crate::tables::variation_store::ItemVariationStore;

/// A parsed `CFF2` table view.
#[derive(Debug, Clone)]
pub struct Cff2<'a> {
    data: &'a [u8],
    global_subrs: Vec<&'a [u8]>,
    local_subrs: Vec<Vec<&'a [u8]>>,
    char_strings: Vec<&'a [u8]>,
    fd_select: Option<Vec<u8>>,
    vstore_off: Option<u32>,
}

impl<'a> Cff2<'a> {
    /// Parses a CFF2 table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u8()?;
        let _minor = r.read_u8()?;
        let hdr_size = r.read_u8()? as usize;
        let top_dict_length = r.read_u16()? as usize;
        if major != 2 {
            return Err(Error::Malformed {
                offset: 0,
                context: "CFF2 major version != 2",
            });
        }
        if hdr_size < 5 {
            return Err(Error::Malformed {
                offset: 2,
                context: "CFF2 hdrSize < 5",
            });
        }

        // Top DICT follows the fixed header.
        let top_dict_start = hdr_size;
        let top_dict_end = top_dict_start + top_dict_length;
        if data.len() < top_dict_end {
            return Err(Error::Truncated {
                offset: top_dict_end,
                context: "CFF2 Top DICT truncated",
            });
        }
        let top_dict_bytes = &data[top_dict_start..top_dict_end];
        let top = Cff2TopDict::parse(top_dict_bytes)?;

        // Global Subr INDEX — immediately after the Top DICT.
        let mut g_reader = Reader::at(data, top_dict_end)?;
        let global_subrs = read_index(&mut g_reader)?;

        // CharStrings INDEX.
        let cs_off = top.char_strings.ok_or(Error::Malformed {
            offset: 0,
            context: "CFF2 Top DICT missing CharStrings",
        })? as usize;
        let mut cs_reader = Reader::at(data, cs_off)?;
        let char_strings = read_index(&mut cs_reader)?;

        // FDArray — CFF2 always uses it.
        let fd_array_off = top.fd_array.ok_or(Error::Malformed {
            offset: 0,
            context: "CFF2 Top DICT missing FDArray",
        })? as usize;
        let mut fda_reader = Reader::at(data, fd_array_off)?;
        let fd_array = read_index(&mut fda_reader)?;

        let mut locals = Vec::with_capacity(fd_array.len());
        for font_dict_bytes in &fd_array {
            let fd = Cff2TopDict::parse(font_dict_bytes)?;
            if let Some((size, off)) = fd.private {
                let priv_bytes = slice_at(data, off as usize, size as usize)?;
                let local = read_local_subrs(data, priv_bytes, off as usize)?;
                locals.push(local);
            } else {
                locals.push(Vec::new());
            }
        }

        let fd_select = if let Some(fd_sel_off) = top.fd_select {
            let n_glyphs = char_strings.len();
            Some(parse_fd_select(data, fd_sel_off as usize, n_glyphs)?)
        } else {
            None
        };

        Ok(Self {
            data,
            global_subrs,
            local_subrs: locals,
            char_strings,
            fd_select,
            vstore_off: top.vstore,
        })
    }

    /// Number of glyphs covered.
    #[must_use]
    pub fn num_glyphs(&self) -> u16 {
        self.char_strings.len() as u16
    }

    /// Draws the outline for `glyph_id` at `coords` (normalized axis
    /// values). An empty `coords` slice draws the default instance.
    pub fn outline<S: OutlineSink>(
        &self,
        glyph_id: u16,
        coords: &[f32],
        sink: &mut S,
    ) -> Result<bool> {
        let Some(cs) = self.char_strings.get(glyph_id as usize) else {
            return Ok(false);
        };
        let local_idx = if let Some(ref sel) = self.fd_select {
            sel.get(glyph_id as usize).copied().unwrap_or(0) as usize
        } else {
            0
        };
        let local_subrs = self
            .local_subrs
            .get(local_idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        // Parse the variation store when we have non-zero coords.
        let ivs = if coords.is_empty() {
            None
        } else if let Some(off) = self.vstore_off {
            // CFF2 VariationStore: u16 length prefix, then the
            // ItemVariationStore bytes.
            let mut vr = Reader::at(self.data, off as usize)?;
            let len = vr.read_u16()? as usize;
            let start = vr.position();
            if self.data.len() < start + len {
                return Err(Error::Truncated {
                    offset: start + len,
                    context: "CFF2 VariationStore truncated",
                });
            }
            Some(ItemVariationStore::parse(&self.data[start..start + len])?)
        } else {
            None
        };

        let blend = ivs.as_ref().map(|ivs| BlendContext {
            coords,
            ivs,
            vsindex: 0,
        });
        let mut interp =
            crate::tables::cff::Interp2::new(&self.global_subrs, local_subrs, sink, blend);
        interp.run(cs, 0)?;
        Ok(true)
    }
}

// ----------------------------------------------------------------------------
// CFF2 Top DICT.
// ----------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Cff2TopDict {
    /// Operator 17 — CharStrings.
    char_strings: Option<u32>,
    /// Operator 12 36 — FDArray.
    fd_array: Option<u32>,
    /// Operator 12 37 — FDSelect.
    fd_select: Option<u32>,
    /// Operator 24 — VariationStore offset.
    vstore: Option<u32>,
    /// Operator 18 — Private (size, offset). CFF2 Font DICTs only;
    /// the main Top DICT never carries one.
    private: Option<(u32, u32)>,
    /// Operator 19 — Local Subrs offset (relative to Private DICT).
    local_subrs_off: Option<u32>,
}

impl Cff2TopDict {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        let mut r = Reader::new(bytes);
        let mut operands: Vec<i32> = Vec::new();
        while !r.is_empty() {
            let b0 = r.peek_bytes(1)?[0];
            if b0 <= 24 {
                let op = if b0 == 12 {
                    r.skip(1)?;
                    let b1 = r.read_u8()?;
                    0x0C00 | u16::from(b1)
                } else {
                    r.skip(1)?;
                    u16::from(b0)
                };
                match op {
                    17 => out.char_strings = operands.last().map(|v| (*v).max(0) as u32),
                    18 => {
                        if operands.len() >= 2 {
                            let size = operands[operands.len() - 2].max(0) as u32;
                            let off = operands[operands.len() - 1].max(0) as u32;
                            out.private = Some((size, off));
                        }
                    }
                    19 => out.local_subrs_off = operands.last().map(|v| (*v).max(0) as u32),
                    24 => out.vstore = operands.last().map(|v| (*v).max(0) as u32),
                    0x0C24 => out.fd_array = operands.last().map(|v| (*v).max(0) as u32),
                    0x0C25 => out.fd_select = operands.last().map(|v| (*v).max(0) as u32),
                    _ => {}
                }
                operands.clear();
            } else {
                operands.push(read_dict_int(&mut r)?);
            }
        }
        Ok(out)
    }
}

fn read_dict_int(r: &mut Reader<'_>) -> Result<i32> {
    let b0 = r.read_u8()?;
    if b0 == 28 {
        let v = r.read_i16()?;
        Ok(i32::from(v))
    } else if b0 == 29 {
        let v = r.read_i32()?;
        Ok(v)
    } else if b0 == 30 {
        // Real — skip content, yield 0.
        loop {
            let b = r.read_u8()?;
            if (b & 0x0F) == 0x0F || (b >> 4) == 0x0F {
                break;
            }
        }
        Ok(0)
    } else if (32..=246).contains(&b0) {
        Ok(i32::from(b0) - 139)
    } else if (247..=250).contains(&b0) {
        let b1 = r.read_u8()?;
        Ok((i32::from(b0) - 247) * 256 + i32::from(b1) + 108)
    } else if (251..=254).contains(&b0) {
        let b1 = r.read_u8()?;
        Ok(-(i32::from(b0) - 251) * 256 - i32::from(b1) - 108)
    } else {
        Err(Error::Malformed {
            offset: r.position(),
            context: "CFF2 DICT operand out of range",
        })
    }
}

// ----------------------------------------------------------------------------
// Shared helpers copied from CFF1. Duplication is cheaper than pub
// plumbing in a module that's this compact.
// ----------------------------------------------------------------------------

fn slice_at(data: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    let end = off.checked_add(len).ok_or(Error::Malformed {
        offset: off,
        context: "CFF2 slice overflow",
    })?;
    if end > data.len() {
        return Err(Error::Truncated {
            offset: end,
            context: "CFF2 slice past end",
        });
    }
    Ok(&data[off..end])
}

fn read_local_subrs<'a>(
    data: &'a [u8],
    priv_bytes: &'a [u8],
    priv_off: usize,
) -> Result<Vec<&'a [u8]>> {
    let priv_dict = Cff2TopDict::parse(priv_bytes)?;
    let Some(off) = priv_dict.local_subrs_off else {
        return Ok(Vec::new());
    };
    let subr_off = priv_off + off as usize;
    let mut r = Reader::at(data, subr_off)?;
    read_index(&mut r)
}

fn parse_fd_select(data: &[u8], off: usize, n_glyphs: usize) -> Result<Vec<u8>> {
    let mut r = Reader::at(data, off)?;
    let format = r.read_u8()?;
    match format {
        0 => {
            let mut out = Vec::with_capacity(n_glyphs);
            for _ in 0..n_glyphs {
                out.push(r.read_u8()?);
            }
            Ok(out)
        }
        3 => {
            let n_ranges = r.read_u16()? as usize;
            let mut out = alloc::vec![0u8; n_glyphs];
            let mut ranges = Vec::with_capacity(n_ranges);
            for _ in 0..n_ranges {
                let first = r.read_u16()?;
                let fd = r.read_u8()?;
                ranges.push((first, fd));
            }
            let sentinel = r.read_u16()?;
            for i in 0..n_ranges {
                let start = ranges[i].0 as usize;
                let end = if i + 1 < n_ranges {
                    ranges[i + 1].0 as usize
                } else {
                    sentinel as usize
                };
                let fd = ranges[i].1;
                for g in start..end.min(n_glyphs) {
                    out[g] = fd;
                }
            }
            Ok(out)
        }
        4 => {
            // Format 4: 32-bit ranges. Used by huge CID fonts.
            let n_ranges = r.read_u32()? as usize;
            let mut out = alloc::vec![0u8; n_glyphs];
            let mut ranges = Vec::with_capacity(n_ranges);
            for _ in 0..n_ranges {
                let first = r.read_u32()? as usize;
                let fd = r.read_u16()? as u8;
                ranges.push((first, fd));
            }
            let sentinel = r.read_u32()? as usize;
            for i in 0..n_ranges {
                let start = ranges[i].0;
                let end = if i + 1 < n_ranges {
                    ranges[i + 1].0
                } else {
                    sentinel
                };
                let fd = ranges[i].1;
                for g in start..end.min(n_glyphs) {
                    out[g] = fd;
                }
            }
            Ok(out)
        }
        _ => Err(Error::Unsupported {
            context: "CFF2 FDSelect format unsupported",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::cff::{op_code, BlendContext, Interp2};
    use crate::tables::outline::{Outline, PathOp};
    use alloc::vec::Vec;

    /// Builds an ItemVariationStore with one axis, one region whose
    /// scalar is 1.0 at coord=1.0, one subtable referencing that
    /// region. Used as the blend source for CFF2 test charstrings.
    fn build_test_ivs() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format = 1
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // regionListOff (patched)
        out.extend_from_slice(&1u16.to_be_bytes()); // itemVariationDataCount
        let subtable_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // subtable offset (patched)

        // Region list.
        let region_list_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_list_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
                                                    // Region: (start, peak, end) = (0, 1, 1).
        let to_f2d14 = |v: f32| ((v * 16384.0) as i16).to_be_bytes();
        out.extend_from_slice(&to_f2d14(0.0));
        out.extend_from_slice(&to_f2d14(1.0));
        out.extend_from_slice(&to_f2d14(1.0));

        // Subtable.
        let subtable_start = out.len() as u32;
        out[subtable_slot..subtable_slot + 4].copy_from_slice(&subtable_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // itemCount
        out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount = 1 (short form)
        out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
        out.extend_from_slice(&0u16.to_be_bytes()); // regionIndex[0]
        out.extend_from_slice(&0i16.to_be_bytes()); // single delta (unused by our synthesis)

        out
    }

    /// Exercises the CFF2 `blend` operator: push 2 default values
    /// plus 2 delta values (one per default) and a count of 2, then
    /// `blend`, then consume the resulting 2 values as coordinates
    /// for an rmoveto.
    #[test]
    fn cff2_blend_applies_delta_at_peak_coord() {
        let ivs_bytes = build_test_ivs();
        let ivs = ItemVariationStore::parse(&ivs_bytes).unwrap();

        // Charstring: push 100 0 (defaults for x and y rmoveto)
        // push 50 0 (per-region deltas, one per default)
        // push 2 (n = 2 default values)
        // blend
        // rmoveto
        // endchar
        let mut cs = Vec::new();
        cs.push(239); // 100 (100+139 = 239)
        cs.push(139); // 0
        cs.push(189); // 50 (50+139 = 189)
        cs.push(139); // 0
        cs.push(141); // n = 2 (2+139=141)
        cs.push(op_code::BLEND);
        cs.push(op_code::RMOVETO);
        cs.push(op_code::ENDCHAR);

        let mut out = Outline::new();
        let coords = [1.0_f32];
        let blend = BlendContext {
            coords: &coords,
            ivs: &ivs,
            vsindex: 0,
        };
        let globals: Vec<&[u8]> = Vec::new();
        let locals: Vec<&[u8]> = Vec::new();
        let mut interp = Interp2::new(&globals, &locals, &mut out, Some(blend));
        interp.run(&cs, 0).unwrap();

        // At coord = 1.0 (region peak), scalar = 1.0, so x becomes
        // 100 + 50 = 150; y stays 0.
        match out.ops()[0] {
            PathOp::MoveTo { x, y } => {
                assert!((x - 150.0).abs() < 1e-3);
                assert!(y.abs() < 1e-6);
            }
            _ => panic!("expected MoveTo"),
        }
    }

    #[test]
    fn cff2_blend_without_context_yields_defaults() {
        // Same charstring, but no BlendContext provided — the
        // interpreter falls back to the default values and ignores
        // the delta columns.
        let mut cs = Vec::new();
        cs.push(239); // 100
        cs.push(139); // 0
        cs.push(189); // 50 (unused)
        cs.push(139); // 0
        cs.push(141); // n = 2
        cs.push(op_code::BLEND);
        cs.push(op_code::RMOVETO);
        cs.push(op_code::ENDCHAR);

        let mut out = Outline::new();
        let globals: Vec<&[u8]> = Vec::new();
        let locals: Vec<&[u8]> = Vec::new();
        let mut interp = Interp2::new(&globals, &locals, &mut out, None);
        interp.run(&cs, 0).unwrap();

        match out.ops()[0] {
            PathOp::MoveTo { x, y } => {
                assert!((x - 100.0).abs() < 1e-3);
                assert!(y.abs() < 1e-6);
            }
            _ => panic!("expected MoveTo"),
        }
    }
}
