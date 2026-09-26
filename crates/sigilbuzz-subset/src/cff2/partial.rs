//! Partial instancing bake: trims the VariationStore to the kept axes
//! and rewrites every `blend` against the surviving regions.

use alloc::vec::Vec;

use sigilbuzz::tables::variation_store::ItemVariationStore;

use super::{
    decode_operand_f32, encode_charstring_number, parse_cff2, serialise_cff2_top_dict, subr_bias,
    MAX_BAKE_DEPTH, OP_BLEND, OP_CALLGSUBR, OP_CALLSUBR, OP_CNTRMASK, OP_ESCAPE, OP_HHCURVETO,
    OP_HINTMASK, OP_HLINETO, OP_HMOVETO, OP_HSTEM, OP_HSTEMHM, OP_HVCURVETO, OP_RCURVELINE,
    OP_RETURN, OP_RLINECURVE, OP_RLINETO, OP_RMOVETO, OP_RRCURVETO, OP_SHORTINT, OP_VHCURVETO,
    OP_VLINETO, OP_VMOVETO, OP_VSINDEX, OP_VSTEM, OP_VSTEMHM, OP_VVCURVETO,
};
use crate::cff::{
    emit_fd_select_auto, encode_index_cff2, patch_dict_offset, serialise_font_dict,
    serialise_private_dict, walk_dict, DictEntry,
};
use crate::SubsetError;

// ----------------------------------------------------------------------------
// Partial-instancing CFF2 bake.
// ----------------------------------------------------------------------------

/// Per-source-subtable surviving slot info for the CFF2 partial-bake
/// charstring rewrite.
struct CffSubtableSurvivors {
    /// New outer index in the trimmed VarStore.
    new_outer: u16,
    /// Surviving slots in source-slot order: `(source_slot, scalar)`.
    /// Source delta at `source_slot` becomes `scalar * delta` in the
    /// rewritten blend.
    surviving: alloc::vec::Vec<(u16, f32)>,
}

/// Re-emits a CFF2 table with its `VariationStore` partially trimmed
/// to the Keep axes and every `blend` operator rewritten to reference
/// the new region count. The `vsindex` operator and source axis order
/// inside surviving subtables are preserved (subtables that collapse
/// entirely are elided; `vsindex` ops that pointed at them are
/// dropped, and any blend that runs against a collapsed subtable
/// emits no blend: its masters survive untouched, matching the
/// post-execution stack of `n, 0, blend`).
///
/// `coords` and `pins` follow the same shape as
/// [`crate::instance::bake_ivs_partial`]: one entry per source axis,
/// with Pin axes folded into the surviving deltas at `coords[i]` and
/// Keep axes carried through the new VarStore unchanged.
///
/// The bake inlines local + global subroutines into each charstring
/// (mirroring [`bake_at_coords`](super::bake_at_coords)'s strategy) so the rebuilt CFF2
/// carries empty Subr INDEXes: vsindex tracking inside subroutines
/// would otherwise require cross-call stack modeling.
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] when the source is malformed,
/// when its VarStore parse fails, or when a charstring references a
/// dropped subroutine.
pub(crate) fn bake_cff2_partial(
    cff_bytes: &[u8],
    coords: &[f32],
    pins: &[crate::instance::AxisPin],
) -> Result<Vec<u8>, SubsetError> {
    let parsed = parse_cff2(cff_bytes)?;
    let n_glyphs = parsed.char_strings.len();
    if n_glyphs == 0 {
        return Err(SubsetError::Unsupported("CFF2 source has zero glyphs"));
    }

    // No VarStore, so there are no blend ops to rewrite (CFF2 charstrings
    // can't blend without a VarStore). Re-emit the source as-is so the
    // caller's table-list always gets a deterministic CFF2 buffer.
    let Some(vstore_blob) = parsed.vstore_blob else {
        return Ok(cff_bytes.to_vec());
    };
    if vstore_blob.len() < 2 {
        return Err(SubsetError::Unsupported(
            "CFF2 VariationStore blob too short",
        ));
    }
    let src_ivs_bytes = &vstore_blob[2..];

    // Build the trimmed IVS via the IVS-bearing-table primitive.
    let (new_ivs_bytes, _remap) = crate::instance::bake_ivs_partial(src_ivs_bytes, coords, pins)
        .ok_or(SubsetError::Unsupported(
            "CFF2 VarStore partial bake failed",
        ))?;

    // Compute per-source-subtable surviving-slot info for the
    // charstring rewrite. We re-walk the source IVS rather than
    // extending bake_ivs_partial's return shape. The walk is cheap
    // (O(subtables * regions)) and keeps the IVS-bearing-table API
    // narrow.
    let survivors = compute_subtable_survivors(src_ivs_bytes, coords, pins).ok_or(
        SubsetError::Unsupported("CFF2 VarStore region projection failed"),
    )?;

    // Source IVS handle for blend stack arithmetic
    // (variation_region_count tells us how many deltas the source
    // blend op popped).
    let src_ivs = ItemVariationStore::parse(src_ivs_bytes)
        .map_err(|_| SubsetError::Unsupported("CFF2 VariationStore parse failed"))?;

    // Per-FD: rewrite each charstring with subr inlining + blend
    // rewrite.
    let mut new_charstrings: Vec<Vec<u8>> = Vec::with_capacity(n_glyphs);
    for (gid, cs) in parsed.char_strings.iter().enumerate() {
        let fd = parsed
            .fd_select
            .get(gid)
            .copied()
            .ok_or(SubsetError::Unsupported("CFF2 FDSelect gid past end"))?;
        let local_subrs = parsed
            .per_fd_local_subrs
            .get(fd as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut rewriter =
            PartialBaker::new(&src_ivs, &survivors, &parsed.global_subrs, local_subrs);
        let baked = rewriter.bake_charstring(cs)?;
        new_charstrings.push(baked);
    }

    // Per-FD Font DICT bodies. We re-emit each Font DICT pointing at
    // its source Private DICT body with the Subrs operator stripped:
    // no local subrs survive after inlining.
    struct FdEmit {
        font_dict_body: Vec<u8>,
        font_dict_private_slot: Option<(usize, usize)>,
        new_private_body: Vec<u8>,
    }
    let mut fd_emits: Vec<FdEmit> = Vec::with_capacity(parsed.fd_array.len());
    for (i, fd_bytes) in parsed.fd_array.iter().enumerate() {
        let fd_entries = walk_dict(fd_bytes)?;
        let (font_dict_body, font_dict_private_slot) = serialise_font_dict(&fd_entries);
        let priv_entries = if parsed.per_fd_private[i].is_empty() {
            Vec::new()
        } else {
            walk_dict(parsed.per_fd_private[i])?
        };
        let (new_private_body, _) = serialise_private_dict(&priv_entries, false);
        fd_emits.push(FdEmit {
            font_dict_body,
            font_dict_private_slot,
            new_private_body,
        });
    }

    // Empty Subr INDEXes (subrs were inlined).
    let global_subr_index = encode_index_cff2(&[]);
    let fd_array_refs: Vec<&[u8]> = fd_emits
        .iter()
        .map(|f| f.font_dict_body.as_slice())
        .collect();
    let fd_array_index = encode_index_cff2(&fd_array_refs);
    let fd_select_bytes = emit_fd_select_auto(&parsed.fd_select);
    let cs_refs: Vec<&[u8]> = new_charstrings.iter().map(Vec::as_slice).collect();
    let charstrings_index = encode_index_cff2(&cs_refs);

    // Top DICT: keep VariationStore op (we still emit a VarStore).
    let top_entries: Vec<DictEntry> = walk_dict(parsed.top_dict)?;
    let (top_dict_body, top_slots) = serialise_cff2_top_dict(&top_entries);

    // Layout:
    //   header -> Top DICT -> Global Subr INDEX -> FDSelect ->
    //   CharStrings INDEX -> FDArray INDEX -> [per-FD: Private DICT] ->
    //   VariationStore (length-prefixed).
    let mut out = Vec::with_capacity(cff_bytes.len());
    out.push(2u8);
    out.push(0u8);
    out.push(parsed.hdr_size as u8);
    out.extend_from_slice(&(top_dict_body.len() as u16).to_be_bytes());
    while out.len() < parsed.hdr_size {
        out.push(0);
    }

    let top_dict_body_abs = out.len();
    out.extend_from_slice(&top_dict_body);

    out.extend_from_slice(&global_subr_index);

    let fd_select_abs = out.len();
    out.extend_from_slice(&fd_select_bytes);

    let charstrings_abs = out.len();
    out.extend_from_slice(&charstrings_index);

    let fd_array_abs = out.len();
    out.extend_from_slice(&fd_array_index);

    let fd_count = fd_emits.len();
    let fd_index_off_size: usize = {
        let total: usize = fd_emits.iter().map(|f| f.font_dict_body.len()).sum();
        let last_off = 1 + total;
        if last_off <= 0xFF {
            1
        } else if last_off <= 0xFFFF {
            2
        } else if last_off <= 0x00FF_FFFF {
            3
        } else {
            4
        }
    };
    let fd_index_data_start = 4 + 1 + (fd_count + 1) * fd_index_off_size;
    let mut fd_body_offsets_in_index: Vec<usize> = Vec::with_capacity(fd_count);
    let mut acc = fd_index_data_start;
    for f in &fd_emits {
        fd_body_offsets_in_index.push(acc);
        acc += f.font_dict_body.len();
    }

    let mut per_fd_private_abs: Vec<usize> = Vec::with_capacity(fd_count);
    let mut per_fd_private_size: Vec<usize> = Vec::with_capacity(fd_count);
    for f in &fd_emits {
        per_fd_private_abs.push(out.len());
        per_fd_private_size.push(f.new_private_body.len());
        out.extend_from_slice(&f.new_private_body);
    }

    // VariationStore: 2-byte length prefix + new IVS body.
    let vstore_abs = out.len();
    let new_ivs_len: u16 = new_ivs_bytes
        .len()
        .try_into()
        .map_err(|_| SubsetError::Unsupported("CFF2 partial VarStore overflows u16 length"))?;
    out.extend_from_slice(&new_ivs_len.to_be_bytes());
    out.extend_from_slice(&new_ivs_bytes);

    // Patch Top DICT placeholders.
    if let Some(slot) = top_slots.char_strings_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, charstrings_abs as i32);
    }
    if let Some(slot) = top_slots.fd_array_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, fd_array_abs as i32);
    }
    if let Some(slot) = top_slots.fd_select_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, fd_select_abs as i32);
    }
    if let Some(slot) = top_slots.vstore_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, vstore_abs as i32);
    }

    // Patch Font DICT Private slots.
    for (i, f) in fd_emits.iter().enumerate() {
        if let Some((size_slot, off_slot)) = f.font_dict_private_slot {
            let body_abs_in_out = fd_array_abs + fd_body_offsets_in_index[i];
            patch_dict_offset(
                &mut out,
                body_abs_in_out + size_slot,
                per_fd_private_size[i] as i32,
            );
            patch_dict_offset(
                &mut out,
                body_abs_in_out + off_slot,
                per_fd_private_abs[i] as i32,
            );
        }
    }

    Ok(out)
}

/// Walks the source IVS to build per-source-subtable surviving-slot
/// info for the partial CFF2 bake. Mirrors the per-region/per-subtable
/// projection inside [`crate::instance::bake_ivs_partial`] but
/// surfaces the per-subtable shape the charstring rewrite needs.
///
/// Returns one entry per source subtable: `Some(survivors)` when the
/// subtable contains at least one surviving region (its blend ops can
/// re-emit), or `None` when every region drops (blend ops against
/// this subtable degenerate to a no-op).
///
/// Returns `None` (the outer `Option`) on malformed input: every
/// length and offset matches what `bake_ivs_partial` accepts.
fn compute_subtable_survivors(
    ivs_bytes: &[u8],
    coords: &[f32],
    pins: &[crate::instance::AxisPin],
) -> Option<Vec<Option<CffSubtableSurvivors>>> {
    if ivs_bytes.len() < 8 {
        return None;
    }
    let format = u16::from_be_bytes([ivs_bytes[0], ivs_bytes[1]]);
    if format != 1 {
        return None;
    }
    let region_list_off =
        u32::from_be_bytes([ivs_bytes[2], ivs_bytes[3], ivs_bytes[4], ivs_bytes[5]]) as usize;
    let subtable_count = u16::from_be_bytes([ivs_bytes[6], ivs_bytes[7]]) as usize;
    if ivs_bytes.len() < 8 + subtable_count * 4 {
        return None;
    }
    let mut subtable_offsets: Vec<usize> = Vec::with_capacity(subtable_count);
    for i in 0..subtable_count {
        let off = u32::from_be_bytes([
            ivs_bytes[8 + i * 4],
            ivs_bytes[8 + i * 4 + 1],
            ivs_bytes[8 + i * 4 + 2],
            ivs_bytes[8 + i * 4 + 3],
        ]) as usize;
        subtable_offsets.push(off);
    }

    if ivs_bytes.len() < region_list_off + 4 {
        return None;
    }
    let axis_count =
        u16::from_be_bytes([ivs_bytes[region_list_off], ivs_bytes[region_list_off + 1]]) as usize;
    let region_count = u16::from_be_bytes([
        ivs_bytes[region_list_off + 2],
        ivs_bytes[region_list_off + 3],
    ]) as usize;
    if pins.len() != axis_count || coords.len() != axis_count {
        return None;
    }
    let regions_start = region_list_off + 4;
    let region_size = axis_count * 6;
    if ivs_bytes.len() < regions_start + region_count * region_size {
        return None;
    }

    // Project each region onto Keep axes; track new index + scalar.
    // None -> dropped at pin coords.
    let mut region_remap: Vec<Option<(u16, f32)>> = Vec::with_capacity(region_count);
    let mut next_new_idx: u16 = 0;
    for ri in 0..region_count {
        let base = regions_start + ri * region_size;
        let mut region: alloc::vec::Vec<(f32, f32, f32)> =
            alloc::vec::Vec::with_capacity(axis_count);
        for axis_i in 0..axis_count {
            let off = base + axis_i * 6;
            let s = read_f2dot14_at(ivs_bytes, off);
            let p = read_f2dot14_at(ivs_bytes, off + 2);
            let e = read_f2dot14_at(ivs_bytes, off + 4);
            region.push((s, p, e));
        }
        match crate::instance::project_region_onto_kept_axes(&region, pins, coords) {
            Some(p) => {
                region_remap.push(Some((next_new_idx, p.pin_scalar)));
                next_new_idx += 1;
            }
            None => region_remap.push(None),
        }
    }

    let mut per_subtable: Vec<Option<CffSubtableSurvivors>> = Vec::with_capacity(subtable_count);
    let mut new_outer: u16 = 0;
    for sub_off in &subtable_offsets {
        let sub_off = *sub_off;
        if ivs_bytes.len() < sub_off + 6 {
            return None;
        }
        let item_count = u16::from_be_bytes([ivs_bytes[sub_off], ivs_bytes[sub_off + 1]]);
        let region_index_count =
            u16::from_be_bytes([ivs_bytes[sub_off + 4], ivs_bytes[sub_off + 5]]) as usize;
        let ri_start = sub_off + 6;
        if ivs_bytes.len() < ri_start + region_index_count * 2 {
            return None;
        }
        let mut surviving: alloc::vec::Vec<(u16, f32)> = alloc::vec::Vec::new();
        for slot in 0..region_index_count {
            let old_ri = u16::from_be_bytes([
                ivs_bytes[ri_start + slot * 2],
                ivs_bytes[ri_start + slot * 2 + 1],
            ]);
            if let Some(Some((_new_ri, scalar))) = region_remap.get(old_ri as usize) {
                #[allow(clippy::cast_possible_truncation)]
                surviving.push((slot as u16, *scalar));
            }
        }
        if item_count == 0 || surviving.is_empty() {
            per_subtable.push(None);
        } else {
            per_subtable.push(Some(CffSubtableSurvivors {
                new_outer,
                surviving,
            }));
            new_outer += 1;
        }
    }
    Some(per_subtable)
}

/// Reads an F2DOT14 from `data[off..]`.
fn read_f2dot14_at(data: &[u8], off: usize) -> f32 {
    let raw = i16::from_be_bytes([data[off], data[off + 1]]);
    f32::from(raw) / 16384.0
}

// CFF2 charstring partial-rewrite baker. Walks the source charstring
// re-emitting every push operand verbatim and every operator
// verbatim, with `blend` (and the surrounding masters / deltas)
// rewritten to reference the surviving regions only. `vsindex` ops
// are dropped from the output and re-emitted lazily, immediately
// before each blend whose surviving subtable's `new_outer` differs
// from the last value we wrote.
//
// Subroutines are inlined into the output charstring so the rebuilt
// CFF2 carries empty Subr INDEXes. Cross-call vsindex tracking
// would otherwise need stack modeling.
struct PartialBaker<'a> {
    src_ivs: &'a ItemVariationStore<'a>,
    survivors: &'a [Option<CffSubtableSurvivors>],
    global_subrs: &'a [&'a [u8]],
    local_subrs: &'a [&'a [u8]],
    out: Vec<u8>,
    /// Per stack entry: byte position in `out` where this entry's push
    /// began. Non-push values (results of a prior blend) carry the
    /// position of the original master push that fed that blend:
    /// the master bytes survive the truncate and remain the "anchor"
    /// for a later blend's truncate.
    stack_starts: Vec<usize>,
    /// Active source-vsindex (mirrors the source's running vsindex).
    src_vsindex: u16,
    /// Last-emitted new-outer in `out`. We emit `new_outer, vsindex`
    /// before each blend whose surviving subtable's `new_outer`
    /// differs from the last value we wrote.
    last_emitted_new_outer: Option<u16>,
    stem_count: u32,
}

impl<'a> PartialBaker<'a> {
    fn new(
        src_ivs: &'a ItemVariationStore<'a>,
        survivors: &'a [Option<CffSubtableSurvivors>],
        global_subrs: &'a [&'a [u8]],
        local_subrs: &'a [&'a [u8]],
    ) -> Self {
        Self {
            src_ivs,
            survivors,
            global_subrs,
            local_subrs,
            out: Vec::new(),
            stack_starts: Vec::new(),
            src_vsindex: 0,
            last_emitted_new_outer: None,
            stem_count: 0,
        }
    }

    fn bake_charstring(&mut self, cs: &[u8]) -> Result<Vec<u8>, SubsetError> {
        self.run(cs, 0)?;
        Ok(core::mem::take(&mut self.out))
    }

    fn run(&mut self, code: &[u8], depth: u8) -> Result<(), SubsetError> {
        if depth > MAX_BAKE_DEPTH {
            return Err(SubsetError::Unsupported(
                "CFF2 partial bake: subroutine depth exceeded",
            ));
        }
        let mut pos = 0;
        while pos < code.len() {
            let b0 = code[pos];
            if b0 >= 32 {
                let len = match b0 {
                    32..=246 => 1,
                    247..=254 => 2,
                    255 => 5,
                    _ => unreachable!(),
                };
                if pos + len > code.len() {
                    return Err(SubsetError::Unsupported(
                        "CFF2 partial bake: push operand truncated",
                    ));
                }
                self.stack_starts.push(self.out.len());
                self.out.extend_from_slice(&code[pos..pos + len]);
                pos += len;
                continue;
            }
            if b0 == OP_SHORTINT {
                if pos + 3 > code.len() {
                    return Err(SubsetError::Unsupported(
                        "CFF2 partial bake: shortint truncated",
                    ));
                }
                self.stack_starts.push(self.out.len());
                self.out.extend_from_slice(&code[pos..pos + 3]);
                pos += 3;
                continue;
            }
            match b0 {
                OP_BLEND => {
                    self.apply_blend()?;
                    pos += 1;
                }
                OP_VSINDEX => {
                    let start = self.stack_starts.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 partial bake: vsindex without operand",
                    ))?;
                    let v = decode_operand_f32(&self.out, start)
                        .ok_or(SubsetError::Unsupported(
                            "CFF2 partial bake: vsindex operand decode failed",
                        ))?
                        .0;
                    if !(0.0..=f32::from(u16::MAX)).contains(&v) {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: vsindex operand out of range",
                        ));
                    }
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        self.src_vsindex = v as u16;
                    }
                    self.out.truncate(start);
                    pos += 1;
                }
                OP_CALLSUBR => {
                    let start = self.stack_starts.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 partial bake: callsubr without operand",
                    ))?;
                    let raw = decode_operand_f32(&self.out, start)
                        .ok_or(SubsetError::Unsupported(
                            "CFF2 partial bake: callsubr operand decode failed",
                        ))?
                        .0;
                    self.out.truncate(start);
                    #[allow(clippy::cast_possible_truncation)]
                    let raw_i = raw as i32;
                    let bias = subr_bias(self.local_subrs.len());
                    let abs = raw_i + bias;
                    if abs < 0 || (abs as usize) >= self.local_subrs.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: callsubr index out of range",
                        ));
                    }
                    let body = self.local_subrs[abs as usize];
                    self.run(body, depth + 1)?;
                    pos += 1;
                }
                OP_CALLGSUBR => {
                    let start = self.stack_starts.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 partial bake: callgsubr without operand",
                    ))?;
                    let raw = decode_operand_f32(&self.out, start)
                        .ok_or(SubsetError::Unsupported(
                            "CFF2 partial bake: callgsubr operand decode failed",
                        ))?
                        .0;
                    self.out.truncate(start);
                    #[allow(clippy::cast_possible_truncation)]
                    let raw_i = raw as i32;
                    let bias = subr_bias(self.global_subrs.len());
                    let abs = raw_i + bias;
                    if abs < 0 || (abs as usize) >= self.global_subrs.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: callgsubr index out of range",
                        ));
                    }
                    let body = self.global_subrs[abs as usize];
                    self.run(body, depth + 1)?;
                    pos += 1;
                }
                OP_RETURN => {
                    // CFF2 subroutines do NOT own the caller's stack:
                    // they may leave operands on it for the caller to
                    // consume (#198). Clearing `stack_starts` here used
                    // to corrupt the caller's tracking and made any
                    // subr-pushes-deltas-then-returns pattern fail with
                    // "blend without count operand". Just hand control
                    // back; the inliner's caller continues from the
                    // current stack state.
                    return Ok(());
                }
                OP_HSTEM | OP_VSTEM | OP_HSTEMHM | OP_VSTEMHM => {
                    let n_pairs = (self.stack_starts.len() as u32) / 2;
                    self.stem_count += n_pairs;
                    self.stack_starts.clear();
                    self.out.push(b0);
                    pos += 1;
                }
                OP_HINTMASK | OP_CNTRMASK => {
                    let extra_pairs = (self.stack_starts.len() as u32) / 2;
                    self.stem_count += extra_pairs;
                    self.stack_starts.clear();
                    self.out.push(b0);
                    let mask_bytes = (self.stem_count as usize).div_ceil(8);
                    if pos + 1 + mask_bytes > code.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: hintmask tail truncated",
                        ));
                    }
                    self.out
                        .extend_from_slice(&code[pos + 1..pos + 1 + mask_bytes]);
                    pos += 1 + mask_bytes;
                }
                OP_ESCAPE => {
                    if pos + 2 > code.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: escape truncated",
                        ));
                    }
                    self.stack_starts.clear();
                    self.out.push(b0);
                    self.out.push(code[pos + 1]);
                    pos += 2;
                }
                OP_RMOVETO | OP_HMOVETO | OP_VMOVETO | OP_RLINETO | OP_HLINETO | OP_VLINETO
                | OP_RRCURVETO | OP_HHCURVETO | OP_VVCURVETO | OP_HVCURVETO | OP_VHCURVETO
                | OP_RCURVELINE | OP_RLINECURVE => {
                    self.stack_starts.clear();
                    self.out.push(b0);
                    pos += 1;
                }
                _ => {
                    return Err(SubsetError::Unsupported(
                        "CFF2 partial bake: unknown charstring operator",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Rewrites the trailing `n masters | n*old_k deltas | count |
    /// blend` block in `out` to `[vsindex] | n masters | n*new_k
    /// scaled deltas | count | blend`. When the active subtable
    /// collapsed entirely (no surviving regions), drops the deltas +
    /// count entirely and emits no blend (the masters become the
    /// post-blend stack values directly, equivalent to `n, 0, blend`
    /// post-execution).
    fn apply_blend(&mut self) -> Result<(), SubsetError> {
        // Pop the count operand.
        let count_start = self.stack_starts.pop().ok_or(SubsetError::Unsupported(
            "CFF2 partial bake: blend without count operand",
        ))?;
        let n_raw = decode_operand_f32(&self.out, count_start)
            .ok_or(SubsetError::Unsupported(
                "CFF2 partial bake: blend count decode failed",
            ))?
            .0;
        // Strip the count operand from `out`. We'll re-emit it below.
        self.out.truncate(count_start);
        if !(0.0..=f32::from(u16::MAX)).contains(&n_raw) {
            return Err(SubsetError::Unsupported(
                "CFF2 partial bake: blend count out of range",
            ));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = n_raw as usize;
        if n == 0 {
            return Ok(());
        }

        // Source subtable info.
        let old_k = self
            .src_ivs
            .variation_region_count(self.src_vsindex)
            .map(usize::from)
            .unwrap_or(0);
        let total_deltas = n * old_k;
        if self.stack_starts.len() < n + total_deltas {
            return Err(SubsetError::Unsupported(
                "CFF2 partial bake: blend stack underflow",
            ));
        }

        // Decode every delta operand from out (these are still live in
        // the byte stream; we'll truncate over them shortly).
        let delta_first_idx = self.stack_starts.len() - total_deltas;
        let mut src_deltas: Vec<f32> = Vec::with_capacity(total_deltas);
        for slot in 0..total_deltas {
            let start = self.stack_starts[delta_first_idx + slot];
            let v = decode_operand_f32(&self.out, start)
                .ok_or(SubsetError::Unsupported(
                    "CFF2 partial bake: delta decode failed",
                ))?
                .0;
            src_deltas.push(v);
        }

        // Truncate `out` to the byte position before the first delta
        // push. The n masters' bytes survive; everything from the
        // first delta to the end of the count operand is gone. Drop
        // the corresponding `stack_starts` entries.
        let truncate_to = self.stack_starts[delta_first_idx];
        self.stack_starts.truncate(delta_first_idx);
        self.out.truncate(truncate_to);

        // Look up the surviving subtable.
        let survivor = self
            .survivors
            .get(self.src_vsindex as usize)
            .and_then(|s| s.as_ref());
        let Some(survivor) = survivor else {
            // Subtable collapsed: emit no blend at all. The n masters
            // already sit in `out`. They'll be consumed by the next
            // outline op verbatim, equivalent to executing
            // `n, 0, blend` (count consumed, masters intact).
            return Ok(());
        };

        // Emit `new_outer, vsindex` before this blend when the active
        // outer-in-output differs. CFF2 default vsindex is 0. If the
        // surviving outer is also 0 and we haven't emitted vsindex
        // yet, the prefix is a no-op.
        let need_vsindex = match self.last_emitted_new_outer {
            Some(prev) => prev != survivor.new_outer,
            None => survivor.new_outer != 0,
        };
        if need_vsindex {
            encode_charstring_number(f32::from(survivor.new_outer), &mut self.out);
            self.out.push(OP_VSINDEX);
            self.last_emitted_new_outer = Some(survivor.new_outer);
        }

        // Emit the new deltas in source-slot order, each scaled by the
        // pin_scalar.
        for i in 0..n {
            for &(slot, scalar) in &survivor.surviving {
                let src = src_deltas[i * old_k + slot as usize];
                let scaled = src * scalar;
                encode_charstring_number(scaled, &mut self.out);
            }
        }
        // Emit the count operand and blend op.
        encode_charstring_number(n as f32, &mut self.out);
        self.out.push(OP_BLEND);

        // The post-blend stack carries n result values. Their
        // `stack_starts` are the original master push positions; the
        // truncate above left those bytes in place. A subsequent
        // blend that pops these masters as its own masters will
        // truncate to the original master positions, leaving the
        // already-emitted [masters][deltas][count][BLEND] block alone
        // which is sound for chained blend ops that build on prior blend
        // results.
        Ok(())
    }
}
