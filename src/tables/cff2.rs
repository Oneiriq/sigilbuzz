//! `CFF2`: CFF for variable fonts.
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
//!
//! Parsing is lazy, as for `CFF `: [`Cff2::parse`] reads the header,
//! the Top DICT, and INDEX headers, and each outline locates its own
//! charstring, Font DICT, Private DICT, and Local Subrs. Variable fonts
//! are often drawn one glyph at a time at changing coordinates, so the
//! per-call cost matters more here than anywhere else.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::cff::{read_index2, BlendContext, FdSelect, Index};
use crate::tables::outline::OutlineSink;
use crate::tables::parse::Reader;
use crate::tables::variation_store::ItemVariationStore;

/// A parsed `CFF2` table view.
#[derive(Debug, Clone)]
pub struct Cff2<'a> {
    data: &'a [u8],
    global_subrs: Index<'a>,
    char_strings: Index<'a>,
    /// Font DICTs. Each names a Private DICT, which holds the offset of
    /// that font's Local Subrs.
    fd_array: Index<'a>,
    /// Which Font DICT each glyph uses. Without it every glyph uses
    /// Font DICT 0.
    fd_select: Option<FdSelect<'a>>,
    vstore_off: Option<u32>,
}

impl<'a> Cff2<'a> {
    /// Parses a CFF2 table.
    ///
    /// This reads only the header, the Top DICT, and INDEX headers, so
    /// its cost does not grow with the glyph count. Problems inside a
    /// single charstring, subroutine, or Font DICT surface from
    /// [`Cff2::outline`] for the glyphs that use it.
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

        // Global Subr INDEX: immediately after the Top DICT.
        let mut g_reader = Reader::at(data, top_dict_end)?;
        let global_subrs = read_index2(&mut g_reader)?;

        // CharStrings INDEX.
        let cs_off = top.char_strings.ok_or(Error::Malformed {
            offset: 0,
            context: "CFF2 Top DICT missing CharStrings",
        })? as usize;
        let mut cs_reader = Reader::at(data, cs_off)?;
        let char_strings = read_index2(&mut cs_reader)?;

        // FDArray: CFF2 always uses it.
        let fd_array_off = top.fd_array.ok_or(Error::Malformed {
            offset: 0,
            context: "CFF2 Top DICT missing FDArray",
        })? as usize;
        let mut fda_reader = Reader::at(data, fd_array_off)?;
        let fd_array = read_index2(&mut fda_reader)?;

        // FDSelect: formats 0, 3, and 4.
        let fd_select = match top.fd_select {
            Some(off) => Some(FdSelect::parse(
                data,
                off as usize,
                char_strings.len(),
                true,
                "CFF2 FDSelect format unsupported",
            )?),
            None => None,
        };

        Ok(Self {
            data,
            global_subrs,
            char_strings,
            fd_array,
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
        let gid = usize::from(glyph_id);
        if gid >= self.char_strings.len() {
            return Ok(false);
        }
        let cs = self.char_strings.get(gid)?;
        let local_subrs = self.local_subrs(gid)?;

        // Parse the variation store. CFF2 charstrings call `blend`
        // even at the default instance (empty coords). The operator
        // pops `n` defaults plus `n * n_regions` per-region deltas
        // off the stack. Without an IVS the interpreter must guess
        // `n_regions` from surplus stack depth, and that guess is
        // wrong whenever a `blend` is followed by additional
        // operands left over from earlier ops. Resolving the IVS up
        // front pins `n_regions` from the spec, so the stack stays
        // balanced regardless of coord vector.
        let ivs = if let Some(off) = self.vstore_off {
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
            crate::tables::cff::Interp2::new(self.global_subrs, local_subrs, sink, blend);
        interp.run(cs, 0)?;
        // CFF2 has no endchar, so the last contour is still open when
        // the charstring runs out. Close it here.
        interp.finish();
        Ok(true)
    }

    /// The Local Subrs INDEX for glyph `gid`, from the Private DICT of
    /// the Font DICT that FDSelect picks. A glyph whose FD has no Font
    /// DICT, or whose Font DICT has no Private DICT, gets an empty one.
    fn local_subrs(&self, gid: usize) -> Result<Index<'a>> {
        let fd = usize::from(self.fd_select.map_or(0, |s| s.fd_for_glyph(gid)));
        if fd >= self.fd_array.len() {
            return Ok(Index::default());
        }
        let font_dict = Cff2TopDict::parse(self.fd_array.get(fd)?)?;
        let Some((size, off)) = font_dict.private else {
            return Ok(Index::default());
        };
        let priv_bytes = slice_at(self.data, off as usize, size as usize)?;
        read_local_subrs(self.data, priv_bytes, off as usize)
    }
}

// ----------------------------------------------------------------------------
// CFF2 Top DICT.
// ----------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Cff2TopDict {
    /// Operator 17: CharStrings.
    char_strings: Option<u32>,
    /// Operator 12 36: FDArray.
    fd_array: Option<u32>,
    /// Operator 12 37: FDSelect.
    fd_select: Option<u32>,
    /// Operator 24: VariationStore offset.
    vstore: Option<u32>,
    /// Operator 18: Private (size, offset). CFF2 Font DICTs only;
    /// the main Top DICT never carries one.
    private: Option<(u32, u32)>,
    /// Operator 19: Local Subrs offset (relative to Private DICT).
    local_subrs_off: Option<u32>,
}

impl Cff2TopDict {
    // Each operator keeps one arm with its operand-count check inside,
    // so the dispatch reads like the spec's operator table.
    #[allow(clippy::collapsible_match)]
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
        // Real: skip content, yield 0.
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
) -> Result<Index<'a>> {
    let priv_dict = Cff2TopDict::parse(priv_bytes)?;
    let Some(off) = priv_dict.local_subrs_off else {
        return Ok(Index::default());
    };
    let subr_off = priv_off.checked_add(off as usize).ok_or(Error::Malformed {
        offset: priv_off,
        context: "CFF2 Local Subrs offset overflow",
    })?;
    let mut r = Reader::at(data, subr_off)?;
    read_index2(&mut r)
}

#[cfg(test)]
#[allow(clippy::vec_init_then_push, clippy::same_item_push)]
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
        let mut interp = Interp2::new(Index::default(), Index::default(), &mut out, Some(blend));
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
        // Same charstring, but no BlendContext provided. The
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
        let mut interp = Interp2::new(Index::default(), Index::default(), &mut out, None);
        interp.run(&cs, 0).unwrap();

        match out.ops()[0] {
            PathOp::MoveTo { x, y } => {
                assert!((x - 100.0).abs() < 1e-3);
                assert!(y.abs() < 1e-6);
            }
            _ => panic!("expected MoveTo"),
        }
    }

    /// Builds an ItemVariationStore with one axis, two regions, and two
    /// subtables. Region 0 runs (0, 1, 1) and region 1 runs
    /// (0, 0.5, 1); subtable `k` uses region `k` alone.
    fn build_two_subtable_ivs() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&16u32.to_be_bytes()); // regionListOffset
        out.extend_from_slice(&2u16.to_be_bytes()); // itemVariationDataCount
        out.extend_from_slice(&32u32.to_be_bytes()); // subtable 0
        out.extend_from_slice(&40u32.to_be_bytes()); // subtable 1
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&2u16.to_be_bytes()); // regionCount
        for peak in [0x4000i16, 0x2000] {
            out.extend_from_slice(&0i16.to_be_bytes()); // start 0.0
            out.extend_from_slice(&peak.to_be_bytes());
            out.extend_from_slice(&0x4000i16.to_be_bytes()); // end 1.0
        }
        assert_eq!(out.len(), 32);
        for region in 0u16..2 {
            out.extend_from_slice(&0u16.to_be_bytes()); // itemCount
            out.extend_from_slice(&0u16.to_be_bytes()); // wordDeltaCount
            out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
            out.extend_from_slice(&region.to_be_bytes());
        }
        out
    }

    #[test]
    fn cff2_blend_follows_a_vsindex_change() {
        // 100 10 1 blend, then 1 vsindex, then 0 20 1 blend, then
        // rmoveto. At coord 0.5 region 0 scales by 0.5 and region 1 by
        // 1.0, so the first blend must use subtable 0's scalars and the
        // second subtable 1's, not the ones cached for vsindex 0.
        let ivs_bytes = build_two_subtable_ivs();
        let ivs = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let cs = [
            239, // 100
            149, // 10
            140, // n = 1
            op_code::BLEND,
            140, // 1
            op_code::VSINDEX,
            139, // 0
            159, // 20
            140, // n = 1
            op_code::BLEND,
            op_code::RMOVETO,
        ];
        let coords = [0.5_f32];
        let blend = BlendContext {
            coords: &coords,
            ivs: &ivs,
            vsindex: 0,
        };
        let mut out = Outline::new();
        let mut interp = Interp2::new(Index::default(), Index::default(), &mut out, Some(blend));
        interp.run(&cs, 0).unwrap();
        assert_eq!(out.ops(), [PathOp::MoveTo { x: 105.0, y: 20.0 }]);
    }

    /// Builds an ItemVariationStore with one axis and one region that
    /// peaks at coord 1.0, plus one subtable that lists that region
    /// `columns` times. At coord 1.0 every column scales by 1.0.
    fn build_wide_ivs(columns: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&12u32.to_be_bytes()); // regionListOffset
        out.extend_from_slice(&1u16.to_be_bytes()); // itemVariationDataCount
        out.extend_from_slice(&22u32.to_be_bytes()); // subtable offset
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
        out.extend_from_slice(&0i16.to_be_bytes()); // start 0.0
        out.extend_from_slice(&0x4000i16.to_be_bytes()); // peak 1.0
        out.extend_from_slice(&0x4000i16.to_be_bytes()); // end 1.0
        assert_eq!(out.len(), 22);
        out.extend_from_slice(&0u16.to_be_bytes()); // itemCount
        out.extend_from_slice(&0u16.to_be_bytes()); // wordDeltaCount
        out.extend_from_slice(&columns.to_be_bytes()); // regionIndexCount
        for _ in 0..columns {
            out.extend_from_slice(&0u16.to_be_bytes());
        }
        out
    }

    /// Builds a minimal CFF2 table: a Top DICT naming the CharStrings
    /// INDEX and the FDArray, an empty Global Subr INDEX, one glyph
    /// whose charstring is `cs`, and one empty Font DICT.
    fn build_cff2(cs: &[u8]) -> Vec<u8> {
        let top_dict_len = 13u8;
        let cs_off = 5 + usize::from(top_dict_len) + 4;
        let fda_off = cs_off + 4 + 1 + 2 + cs.len();
        let mut out = alloc::vec![2, 0, 5, 0, top_dict_len];
        out.push(29);
        out.extend_from_slice(&(cs_off as i32).to_be_bytes());
        out.push(17); // CharStrings
        out.push(29);
        out.extend_from_slice(&(fda_off as i32).to_be_bytes());
        out.extend_from_slice(&[12, 36]); // FDArray
        out.extend_from_slice(&0u32.to_be_bytes()); // Global Subr INDEX
        assert_eq!(out.len(), cs_off);
        out.extend_from_slice(&1u32.to_be_bytes()); // CharStrings INDEX
        out.extend_from_slice(&[1, 1, 1 + cs.len() as u8]);
        out.extend_from_slice(cs);
        assert_eq!(out.len(), fda_off);
        out.extend_from_slice(&1u32.to_be_bytes()); // FDArray INDEX
        out.extend_from_slice(&[1, 1, 1]); // one empty Font DICT
        out
    }

    fn cff2_ops(cs: &[u8]) -> Vec<PathOp> {
        let table = build_cff2(cs);
        let cff2 = Cff2::parse(&table).unwrap();
        let mut out = Outline::new();
        assert!(cff2.outline(0, &[], &mut out).unwrap());
        out.ops().to_vec()
    }

    /// The Private DICT of one Font DICT in a [`TestCff2`].
    struct TestPrivate {
        /// DICT bytes, written ahead of the Subrs entry.
        dict: Vec<u8>,
        /// Local Subrs. Without any, the Private DICT has no Subrs entry.
        subrs: Vec<Vec<u8>>,
    }

    /// The parts of a CFF2 test table, laid out by [`TestCff2::build`].
    #[derive(Default)]
    struct TestCff2 {
        /// One charstring per glyph.
        charstrings: Vec<Vec<u8>>,
        /// The FDArray. `None` is an empty Font DICT.
        font_dicts: Vec<Option<TestPrivate>>,
        /// FDSelect bytes, format byte first.
        fd_select: Option<Vec<u8>>,
        /// ItemVariationStore bytes for the VariationStore.
        ivs: Option<Vec<u8>>,
    }

    /// Encodes a CFF2 INDEX (u32 count) with 4-byte offsets.
    fn encode_index2(entries: &[Vec<u8>]) -> Vec<u8> {
        let entries: Vec<&[u8]> = entries.iter().map(Vec::as_slice).collect();
        let mut out = alloc::vec![0, 0];
        out.extend(crate::tables::cff::encode_index(&entries, 4));
        out
    }

    /// Appends `v` as a 5-byte DICT integer.
    fn dict_i32(out: &mut Vec<u8>, v: usize) {
        out.push(29);
        out.extend_from_slice(&(v as i32).to_be_bytes());
    }

    impl TestCff2 {
        /// Lays the table out as header, Top DICT, Global Subrs (empty),
        /// CharStrings, FDArray, FDSelect, VariationStore, and then each
        /// Private DICT followed by its Local Subrs.
        fn build(&self) -> Vec<u8> {
            let top_len = 6
                + 7
                + if self.fd_select.is_some() { 7 } else { 0 }
                + if self.ivs.is_some() { 6 } else { 0 };
            let char_strings = encode_index2(&self.charstrings);
            let cs_off = 5 + top_len + 4;
            let fda_off = cs_off + char_strings.len();
            // A Font DICT that names a Private DICT is two 5-byte
            // integers and operator 18.
            let fd_lens: usize = self.font_dicts.iter().flatten().map(|_| 11).sum();
            let n_fds = self.font_dicts.len();
            let fda_len = if n_fds == 0 {
                4
            } else {
                4 + 1 + 4 * (n_fds + 1) + fd_lens
            };
            let fds = self.fd_select.clone().unwrap_or_default();
            let fds_off = fda_off + fda_len;
            let vstore_off = fds_off + fds.len();
            let vstore_len = self.ivs.as_ref().map_or(0, |ivs| 2 + ivs.len());

            let mut private_off = vstore_off + vstore_len;
            let mut font_dicts = Vec::new();
            let mut privates = Vec::new();
            for fd in &self.font_dicts {
                let mut dict = Vec::new();
                if let Some(p) = fd {
                    let mut private = p.dict.clone();
                    if !p.subrs.is_empty() {
                        // Subrs start right after the Private DICT.
                        let size = private.len() + 6;
                        dict_i32(&mut private, size);
                        private.push(19);
                    }
                    let size = private.len();
                    if !p.subrs.is_empty() {
                        private.extend(encode_index2(&p.subrs));
                    }
                    dict_i32(&mut dict, size);
                    dict_i32(&mut dict, private_off);
                    dict.push(18);
                    private_off += private.len();
                    privates.extend(private);
                }
                font_dicts.push(dict);
            }

            let mut out = alloc::vec![2, 0, 5];
            out.extend_from_slice(&(top_len as u16).to_be_bytes());
            dict_i32(&mut out, cs_off);
            out.push(17); // CharStrings
            dict_i32(&mut out, fda_off);
            out.extend_from_slice(&[12, 36]); // FDArray
            if self.fd_select.is_some() {
                dict_i32(&mut out, fds_off);
                out.extend_from_slice(&[12, 37]); // FDSelect
            }
            if self.ivs.is_some() {
                dict_i32(&mut out, vstore_off);
                out.push(24); // VariationStore
            }
            out.extend_from_slice(&0u32.to_be_bytes()); // Global Subr INDEX
            assert_eq!(out.len(), cs_off);
            out.extend(char_strings);
            out.extend(encode_index2(&font_dicts));
            assert_eq!(out.len(), fds_off);
            out.extend(fds);
            if let Some(ivs) = &self.ivs {
                out.extend_from_slice(&(ivs.len() as u16).to_be_bytes());
                out.extend_from_slice(ivs);
            }
            out.extend(privates);
            out
        }
    }

    #[test]
    fn cff2_glyph_uses_a_font_dict_past_255() {
        // FDSelect format 4 sends glyph 0 to FD 256, the only Font DICT
        // with Local Subrs. Its subr 0 draws `10 0 rlineto`, called as
        // -107 (byte 32) since one subr has bias 107. The FD used to be
        // cut to a byte, which read FD 0, so the call found no subrs.
        let mut font_dicts: Vec<Option<TestPrivate>> = (0..256).map(|_| None).collect();
        font_dicts.push(Some(TestPrivate {
            dict: Vec::new(),
            subrs: alloc::vec![alloc::vec![149, 139, op_code::RLINETO]],
        }));
        let mut fd_select = alloc::vec![4];
        fd_select.extend_from_slice(&1u32.to_be_bytes()); // nRanges
        fd_select.extend_from_slice(&0u32.to_be_bytes()); // first glyph
        fd_select.extend_from_slice(&256u16.to_be_bytes()); // FD
        fd_select.extend_from_slice(&1u32.to_be_bytes()); // sentinel
        let table = TestCff2 {
            charstrings: alloc::vec![alloc::vec![
                139,
                139,
                op_code::RMOVETO,
                32,
                op_code::CALLSUBR
            ]],
            font_dicts,
            fd_select: Some(fd_select),
            ..TestCff2::default()
        }
        .build();
        let cff2 = Cff2::parse(&table).unwrap();
        let mut out = Outline::new();
        assert!(cff2.outline(0, &[], &mut out).unwrap());
        assert_eq!(
            out.ops(),
            [
                PathOp::MoveTo { x: 0.0, y: 0.0 },
                PathOp::LineTo { x: 10.0, y: 0.0 },
                PathOp::Close,
            ]
        );
    }

    #[test]
    fn cff2_outline_closes_the_last_contour_without_endchar() {
        // 100 100 rmoveto 50 0 rlineto 0 50 rlineto. CFF2 has no
        // endchar, so the charstring just ends with the contour open.
        let cs = [
            239,
            239,
            op_code::RMOVETO,
            189,
            139,
            op_code::RLINETO,
            139,
            189,
            op_code::RLINETO,
        ];
        assert_eq!(
            cff2_ops(&cs),
            [
                PathOp::MoveTo { x: 100.0, y: 100.0 },
                PathOp::LineTo { x: 150.0, y: 100.0 },
                PathOp::LineTo { x: 150.0, y: 150.0 },
                PathOp::Close,
            ]
        );
    }

    #[test]
    fn cff2_moveto_closes_the_previous_contour() {
        // 0 0 rmoveto 10 0 rlineto 0 10 rmoveto 0 10 rlineto. The second
        // moveto closes the first contour, and the end of the
        // charstring closes the second.
        let cs = [
            139,
            139,
            op_code::RMOVETO,
            149,
            139,
            op_code::RLINETO,
            139,
            149,
            op_code::RMOVETO,
            139,
            149,
            op_code::RLINETO,
        ];
        assert_eq!(
            cff2_ops(&cs),
            [
                PathOp::MoveTo { x: 0.0, y: 0.0 },
                PathOp::LineTo { x: 10.0, y: 0.0 },
                PathOp::Close,
                PathOp::MoveTo { x: 10.0, y: 10.0 },
                PathOp::LineTo { x: 10.0, y: 20.0 },
                PathOp::Close,
            ]
        );
    }

    #[test]
    fn cff2_outline_without_drawing_ops_has_no_close() {
        // A charstring that never moves draws nothing, so there is no
        // contour to close.
        assert!(cff2_ops(&[139, 139, op_code::HSTEM]).is_empty());
    }

    #[test]
    fn callgsubr_with_blended_index_past_i32_is_out_of_range() {
        // Each blend adds 511 deltas of 32767 to one running value, so
        // 140 blends push it past i32::MAX. Adding the subroutine bias
        // to that value used to overflow.
        let ivs_bytes = build_wide_ivs(511);
        let ivs = ItemVariationStore::parse(&ivs_bytes).unwrap();
        let mut cs = alloc::vec![139]; // running value starts at 0
        for _ in 0..140 {
            for _ in 0..511 {
                cs.push(op_code::SHORTINT);
                cs.extend_from_slice(&i16::MAX.to_be_bytes());
            }
            cs.push(140); // n = 1
            cs.push(op_code::BLEND);
        }
        cs.push(op_code::CALLGSUBR);

        let coords = [1.0_f32];
        let blend = BlendContext {
            coords: &coords,
            ivs: &ivs,
            vsindex: 0,
        };
        let mut out = Outline::new();
        let mut interp = Interp2::new(Index::default(), Index::default(), &mut out, Some(blend));
        let err = interp.run(&cs, 0).unwrap_err();
        assert!(matches!(err, Error::Malformed { .. }), "{err:?}");
    }
}
