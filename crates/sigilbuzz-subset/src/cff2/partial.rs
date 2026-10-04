//! Partial instancing bake: trims the VariationStore to the kept axes
//! and rewrites every `blend` against the surviving regions.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use sigilbuzz::tables::variation_store::ItemVariationStore;

use super::{
    bake_token_budget, biased_subr, charge_token, decode_operand_f32, encode_charstring_number,
    parse_cff2, read_u16_at, read_u32_at, serialise_cff2_top_dict, MAX_BAKE_DEPTH, OP_BLEND,
    OP_CALLGSUBR, OP_CALLSUBR, OP_CNTRMASK, OP_ESCAPE, OP_HHCURVETO, OP_HINTMASK, OP_HLINETO,
    OP_HMOVETO, OP_HSTEM, OP_HSTEMHM, OP_HVCURVETO, OP_RCURVELINE, OP_RETURN, OP_RLINECURVE,
    OP_RLINETO, OP_RMOVETO, OP_RRCURVETO, OP_SHORTINT, OP_VHCURVETO, OP_VLINETO, OP_VMOVETO,
    OP_VSINDEX, OP_VSTEM, OP_VSTEMHM, OP_VVCURVETO,
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
pub(super) struct CffSubtableSurvivors {
    /// New outer index in the trimmed VarStore.
    pub(super) new_outer: u16,
    /// Surviving slots in source-slot order: `(source_slot, scalar)`.
    /// Source delta at `source_slot` becomes `scalar * delta` in the
    /// rewritten blend.
    pub(super) surviving: alloc::vec::Vec<(u16, f32)>,
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
    let src_ivs_bytes = vstore_blob.get(2..).ok_or(SubsetError::Unsupported(
        "CFF2 VariationStore blob too short",
    ))?;

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
    let mut rewriter = PartialBaker::new(
        &src_ivs,
        &survivors,
        &parsed.global_subrs,
        bake_token_budget(cff_bytes.len()),
    );
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
        let baked = rewriter.bake_charstring(cs, local_subrs)?;
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
        // Its blends keep the regions the projected store keeps.
        let priv_entries = if parsed.per_fd_private[i].is_empty() {
            Vec::new()
        } else {
            super::private::project_private(
                walk_dict(parsed.per_fd_private[i])?,
                &src_ivs,
                &survivors,
            )?
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
    if read_u16_at(ivs_bytes, 0)? != 1 {
        return None;
    }
    let region_list_off = read_u32_at(ivs_bytes, 2)? as usize;
    let subtable_count = usize::from(read_u16_at(ivs_bytes, 6)?);
    let subtable_offsets: Vec<usize> = ivs_bytes
        .get(8..)?
        .get(..subtable_count * 4)?
        .chunks_exact(4)
        .map(|off| u32::from_be_bytes([off[0], off[1], off[2], off[3]]) as usize)
        .collect();

    let axis_count = usize::from(read_u16_at(ivs_bytes, region_list_off)?);
    let region_count = usize::from(read_u16_at(ivs_bytes, region_list_off.checked_add(2)?)?);
    if pins.len() != axis_count || coords.len() != axis_count {
        return None;
    }
    // The count reads above put `region_list_off + 4` inside the data.
    let region_size = axis_count * 6;
    let regions = ivs_bytes
        .get(region_list_off + 4..)?
        .get(..region_count.checked_mul(region_size)?)?;

    // Project each region onto Keep axes; track new index + scalar.
    // None -> dropped at pin coords.
    let mut region_remap: Vec<Option<(u16, f32)>> = Vec::with_capacity(region_count);
    let mut next_new_idx: u16 = 0;
    for ri in 0..region_count {
        let base = ri * region_size;
        let region: Vec<(f32, f32, f32)> = (0..axis_count)
            .map(|axis_i| {
                let off = base + axis_i * 6;
                (
                    read_f2dot14_at(regions, off),
                    read_f2dot14_at(regions, off + 2),
                    read_f2dot14_at(regions, off + 4),
                )
            })
            .collect();
        match crate::instance::project_region_onto_kept_axes(&region, pins, coords) {
            Some(p) => {
                region_remap.push(Some((next_new_idx, p.pin_scalar)));
                next_new_idx = next_new_idx.saturating_add(1);
            }
            None => region_remap.push(None),
        }
    }

    let mut per_subtable: Vec<Option<CffSubtableSurvivors>> = Vec::with_capacity(subtable_count);
    let mut new_outer: u16 = 0;
    for &sub_off in &subtable_offsets {
        // The subtable's header must be readable; its item count does
        // not matter (see below).
        read_u16_at(ivs_bytes, sub_off)?;
        let region_index_count = usize::from(read_u16_at(ivs_bytes, sub_off.checked_add(4)?)?);
        // The read above put `sub_off + 6` inside the data.
        let region_indexes = ivs_bytes
            .get(sub_off + 6..)?
            .get(..region_index_count * 2)?;
        let mut surviving: Vec<(u16, f32)> = Vec::new();
        for (slot, old_ri) in region_indexes.chunks_exact(2).enumerate() {
            let old_ri = usize::from(u16::from_be_bytes([old_ri[0], old_ri[1]]));
            if let Some(Some((_new_ri, scalar))) = region_remap.get(old_ri) {
                surviving.push((slot as u16, *scalar));
            }
        }
        // A CFF2 subtable holds no rows: the charstrings carry its
        // deltas. It survives while any of its regions does.
        if surviving.is_empty() {
            per_subtable.push(None);
        } else {
            per_subtable.push(Some(CffSubtableSurvivors {
                new_outer,
                surviving,
            }));
            new_outer = new_outer.saturating_add(1);
        }
    }
    Some(per_subtable)
}

/// Reads an F2DOT14 from `data[off..]`, or 0 past the end.
fn read_f2dot14_at(data: &[u8], off: usize) -> f32 {
    read_u16_at(data, off).map_or(0.0, |raw| f32::from(raw as i16) / 16384.0)
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
//
// One baker serves a whole table: the region-count cache and the token
// budget are shared across glyphs, and the per-glyph state is reset by
// `bake_charstring`.
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
    stem_count: usize,
    /// Source region count per `vsindex`, resolved once per table.
    region_counts: BTreeMap<u16, usize>,
    /// Tokens left for the rest of the table.
    budget: usize,
}

impl<'a> PartialBaker<'a> {
    fn new(
        src_ivs: &'a ItemVariationStore<'a>,
        survivors: &'a [Option<CffSubtableSurvivors>],
        global_subrs: &'a [&'a [u8]],
        budget: usize,
    ) -> Self {
        Self {
            src_ivs,
            survivors,
            global_subrs,
            local_subrs: &[],
            out: Vec::new(),
            stack_starts: Vec::new(),
            src_vsindex: 0,
            last_emitted_new_outer: None,
            stem_count: 0,
            region_counts: BTreeMap::new(),
            budget,
        }
    }

    fn bake_charstring(
        &mut self,
        cs: &[u8],
        local_subrs: &'a [&'a [u8]],
    ) -> Result<Vec<u8>, SubsetError> {
        self.local_subrs = local_subrs;
        self.out = Vec::new();
        self.stack_starts.clear();
        self.src_vsindex = 0;
        self.last_emitted_new_outer = None;
        self.stem_count = 0;
        self.run(cs, 0)?;
        Ok(core::mem::take(&mut self.out))
    }

    /// Pops the top operand, decodes its value from `out`, and removes
    /// its bytes from `out`.
    fn pop_operand(
        &mut self,
        missing: &'static str,
        bad: &'static str,
    ) -> Result<f32, SubsetError> {
        let start = self
            .stack_starts
            .pop()
            .ok_or(SubsetError::Unsupported(missing))?;
        let v = decode_operand_f32(&self.out, start)
            .ok_or(SubsetError::Unsupported(bad))?
            .0;
        self.out.truncate(start);
        Ok(v)
    }

    fn run(&mut self, code: &[u8], depth: u8) -> Result<(), SubsetError> {
        if depth > MAX_BAKE_DEPTH {
            return Err(SubsetError::Unsupported(
                "CFF2 partial bake: subroutine depth exceeded",
            ));
        }
        let mut pos = 0;
        while let Some(&b0) = code.get(pos) {
            charge_token(
                &mut self.budget,
                "CFF2 partial bake: charstring work budget exceeded",
            )?;
            if b0 >= 32 || b0 == OP_SHORTINT {
                let len = match b0 {
                    OP_SHORTINT => 3,
                    247..=254 => 2,
                    255 => 5,
                    _ => 1,
                };
                let push = code.get(pos..).and_then(|rest| rest.get(..len)).ok_or(
                    SubsetError::Unsupported(if b0 == OP_SHORTINT {
                        "CFF2 partial bake: shortint truncated"
                    } else {
                        "CFF2 partial bake: push operand truncated"
                    }),
                )?;
                self.stack_starts.push(self.out.len());
                self.out.extend_from_slice(push);
                pos += len;
                continue;
            }
            match b0 {
                OP_BLEND => {
                    self.apply_blend()?;
                    pos += 1;
                }
                OP_VSINDEX => {
                    let v = self.pop_operand(
                        "CFF2 partial bake: vsindex without operand",
                        "CFF2 partial bake: vsindex operand decode failed",
                    )?;
                    if !(0.0..=f32::from(u16::MAX)).contains(&v) {
                        return Err(SubsetError::Unsupported(
                            "CFF2 partial bake: vsindex operand out of range",
                        ));
                    }
                    self.src_vsindex = v as u16;
                    pos += 1;
                }
                OP_CALLSUBR => {
                    let raw = self.pop_operand(
                        "CFF2 partial bake: callsubr without operand",
                        "CFF2 partial bake: callsubr operand decode failed",
                    )?;
                    let body = biased_subr(self.local_subrs, raw).ok_or(
                        SubsetError::Unsupported("CFF2 partial bake: callsubr index out of range"),
                    )?;
                    self.run(body, depth + 1)?;
                    pos += 1;
                }
                OP_CALLGSUBR => {
                    let raw = self.pop_operand(
                        "CFF2 partial bake: callgsubr without operand",
                        "CFF2 partial bake: callgsubr operand decode failed",
                    )?;
                    let body = biased_subr(self.global_subrs, raw).ok_or(
                        SubsetError::Unsupported("CFF2 partial bake: callgsubr index out of range"),
                    )?;
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
                    self.stem_count = self.stem_count.saturating_add(self.stack_starts.len() / 2);
                    self.stack_starts.clear();
                    self.out.push(b0);
                    pos += 1;
                }
                OP_HINTMASK | OP_CNTRMASK => {
                    self.stem_count = self.stem_count.saturating_add(self.stack_starts.len() / 2);
                    self.stack_starts.clear();
                    self.out.push(b0);
                    let mask_bytes = self.stem_count.div_ceil(8);
                    let mask = code
                        .get(pos + 1..)
                        .and_then(|rest| rest.get(..mask_bytes))
                        .ok_or(SubsetError::Unsupported(
                            "CFF2 partial bake: hintmask tail truncated",
                        ))?;
                    self.out.extend_from_slice(mask);
                    pos += 1 + mask_bytes;
                }
                OP_ESCAPE => {
                    let &b1 = code.get(pos + 1).ok_or(SubsetError::Unsupported(
                        "CFF2 partial bake: escape truncated",
                    ))?;
                    self.stack_starts.clear();
                    self.out.push(b0);
                    self.out.push(b1);
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
        // Pop the count operand and strip it from `out`. It is
        // re-emitted below.
        let n_raw = self.pop_operand(
            "CFF2 partial bake: blend without count operand",
            "CFF2 partial bake: blend count decode failed",
        )?;
        if !(0.0..=f32::from(u16::MAX)).contains(&n_raw) {
            return Err(SubsetError::Unsupported(
                "CFF2 partial bake: blend count out of range",
            ));
        }
        let n = n_raw as usize;
        if n == 0 {
            return Ok(());
        }

        // Source subtable info.
        let src_ivs = self.src_ivs;
        let old_k = *self
            .region_counts
            .entry(self.src_vsindex)
            .or_insert_with_key(|&v| src_ivs.variation_region_count(v).map_or(0, usize::from));
        let underflow = SubsetError::Unsupported("CFF2 partial bake: blend stack underflow");
        let total_deltas = n.checked_mul(old_k).ok_or(underflow.clone())?;
        let delta_first_idx = self
            .stack_starts
            .len()
            .checked_sub(total_deltas)
            .filter(|&first| first >= n)
            .ok_or(underflow)?;

        // Decode every delta operand from out (these are still live in
        // the byte stream; we'll truncate over them shortly).
        let delta_starts = self.stack_starts.get(delta_first_idx..).unwrap_or_default();
        let mut src_deltas: Vec<f32> = Vec::with_capacity(total_deltas);
        for &start in delta_starts {
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
        // the corresponding `stack_starts` entries. With no deltas
        // (a subtable with zero regions) nothing is cut.
        let truncate_to = delta_starts.first().copied().unwrap_or(self.out.len());
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
                let src = i
                    .checked_mul(old_k)
                    .and_then(|row| row.checked_add(usize::from(slot)))
                    .and_then(|k| src_deltas.get(k))
                    .ok_or(SubsetError::Unsupported(
                        "CFF2 partial bake: blend delta slot out of range",
                    ))?;
                encode_charstring_number(src * scalar, &mut self.out);
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
