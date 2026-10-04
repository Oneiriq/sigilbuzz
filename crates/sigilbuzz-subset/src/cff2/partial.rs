//! Partial instancing bake: trims the VariationStore to the kept axes
//! and rewrites every `blend` against the surviving regions.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use sigilbuzz::tables::variation_store::ItemVariationStore;

use super::{
    bake_token_budget, biased_subr, charge_token, charstring_number_bytes, decode_operand_f32,
    parse_cff2, serialise_cff2_top_dict, MAX_BAKE_DEPTH, OP_BLEND, OP_CALLGSUBR, OP_CALLSUBR,
    OP_CNTRMASK, OP_ESCAPE, OP_HHCURVETO, OP_HINTMASK, OP_HLINETO, OP_HMOVETO, OP_HSTEM,
    OP_HSTEMHM, OP_HVCURVETO, OP_RCURVELINE, OP_RETURN, OP_RLINECURVE, OP_RLINETO, OP_RMOVETO,
    OP_RRCURVETO, OP_SHORTINT, OP_VHCURVETO, OP_VLINETO, OP_VMOVETO, OP_VSINDEX, OP_VSTEM,
    OP_VSTEMHM, OP_VVCURVETO,
};
use crate::cff::{
    emit_fd_select_auto, encode_index_cff2, patch_dict_offset, serialise_font_dict,
    serialise_private_dict, walk_dict, DictEntry,
};
use crate::SubsetError;

// ----------------------------------------------------------------------------
// Partial-instancing CFF2 bake.
// ----------------------------------------------------------------------------

/// What the partial bake does with the slots of one source VarStore
/// subtable: the same projection the trimmed VarStore was built with.
#[derive(Default)]
pub(super) struct CffSubtableSurvivors {
    /// New outer index in the trimmed VarStore; `None` when no region
    /// with a peak on a kept axis survives, so its blends go.
    pub(super) new_outer: Option<u16>,
    /// Surviving slots in source-slot order: `(source_slot, scalar)`.
    /// Source delta at `source_slot` becomes `scalar * delta` in the
    /// rewritten blend.
    pub(super) surviving: Vec<(u16, f32)>,
    /// Slots whose regions lie on the pinned axes only: `scalar *
    /// delta` moves into the blend's default value, where a renderer
    /// at the new default (which applies no variations) reads it.
    pub(super) folded: Vec<(u16, f32)>,
}

/// The surviving and folded slots of every source subtable, from the
/// projection `remap` of the VarStore.
fn subtable_survivors(remap: &crate::instance::RegionRemap) -> Vec<CffSubtableSurvivors> {
    (0..remap.subtable_count())
        .map(|outer| {
            let layout = remap.layout(outer);
            CffSubtableSurvivors {
                new_outer: remap.new_outer(outer),
                // The CFF2 projection does not merge: one slot a column.
                surviving: layout.map_or_else(Vec::new, |l| {
                    l.columns
                        .iter()
                        .filter_map(|c| c.first().copied())
                        .collect()
                }),
                folded: layout.map_or_else(Vec::new, |l| l.folded.clone()),
            }
        })
        .collect()
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
/// Returns [`SubsetError::Unsupported`] when the source is malformed
/// (a charstring that stacks more than the 513 operands CFF2 allows
/// included), when its VarStore parse fails, when a charstring
/// references a dropped subroutine, or when the charstrings run out
/// the token budget.
pub(crate) fn bake_cff2_partial(
    cff_bytes: &[u8],
    coords: &[f32],
    pins: &[crate::instance::AxisPin],
) -> Result<Vec<u8>, SubsetError> {
    let budget = bake_token_budget(cff_bytes.len());
    bake_cff2_partial_within(cff_bytes, coords, pins, budget).map(|(out, _)| out)
}

/// [`bake_cff2_partial`] with a charstring budget of `budget` tokens:
/// the baked table and the tokens left of the budget.
pub(super) fn bake_cff2_partial_within(
    cff_bytes: &[u8],
    coords: &[f32],
    pins: &[crate::instance::AxisPin],
    budget: usize,
) -> Result<(Vec<u8>, usize), SubsetError> {
    let parsed = parse_cff2(cff_bytes)?;
    let n_glyphs = parsed.char_strings.len();
    if n_glyphs == 0 {
        return Err(SubsetError::Unsupported("CFF2 source has zero glyphs"));
    }

    // No VarStore, so there are no blend ops to rewrite (CFF2 charstrings
    // can't blend without a VarStore). Re-emit the source as-is so the
    // caller's table-list always gets a deterministic CFF2 buffer.
    let Some(vstore_blob) = parsed.vstore_blob else {
        return Ok((cff_bytes.to_vec(), budget));
    };
    let src_ivs_bytes = vstore_blob.get(2..).ok_or(SubsetError::Unsupported(
        "CFF2 VariationStore blob too short",
    ))?;

    // Build the trimmed IVS via the IVS-bearing-table primitive. Its
    // remap says, per source subtable, which slots survive and which
    // fold into the default values, so the blends follow the store
    // exactly.
    let (new_ivs_bytes, remap) = crate::instance::bake_ivs_partial(src_ivs_bytes, coords, pins)
        .ok_or(SubsetError::Unsupported(
            "CFF2 VarStore partial bake failed",
        ))?;
    let survivors = subtable_survivors(&remap);

    // Source IVS handle for blend stack arithmetic
    // (variation_region_count tells us how many deltas the source
    // blend op popped).
    let src_ivs = ItemVariationStore::parse(src_ivs_bytes)
        .map_err(|_| SubsetError::Unsupported("CFF2 VariationStore parse failed"))?;

    // Per-FD: rewrite each charstring with subr inlining + blend
    // rewrite.
    let mut rewriter = PartialBaker::new(&src_ivs, &survivors, &parsed.global_subrs, budget);
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
            .map(|locals| locals.as_slice())
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
        /// An earlier Font DICT whose Private DICT this one shares.
        shares: Option<usize>,
    }
    let mut fd_emits: Vec<FdEmit> = Vec::with_capacity(parsed.fd_array.len());
    for (i, fd_bytes) in parsed.fd_array.iter().enumerate() {
        let fd_entries = walk_dict(fd_bytes)?;
        let (font_dict_body, font_dict_private_slot) = serialise_font_dict(&fd_entries);
        // Its blends keep the regions the projected store keeps.
        let shares = parsed.private_of.get(i).copied().filter(|&j| j != i);
        let priv_entries = if parsed.per_fd_private[i].is_empty() || shares.is_some() {
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
            shares,
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
        // A shared Private DICT is written once, where its first Font
        // DICT put it.
        let earlier = f.shares.and_then(|j| {
            per_fd_private_abs
                .get(j)
                .copied()
                .zip(per_fd_private_size.get(j).copied())
        });
        let (abs, size) = earlier.unwrap_or((out.len(), f.new_private_body.len()));
        if earlier.is_none() {
            out.extend_from_slice(&f.new_private_body);
        }
        per_fd_private_abs.push(abs);
        per_fd_private_size.push(size);
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

    Ok((out, rewriter.budget))
}

/// Operands a CFF2 charstring may stack: the CFF2 limit, which
/// HarfBuzz's interpreter enforces too.
const MAX_STACK: usize = 513;

/// One token of the charstring tail the baker can still edit: an
/// operand push or an operator, as the bytes it is written with.
#[derive(Clone, Copy)]
struct Token {
    bytes: [u8; 5],
    len: u8,
}

impl Token {
    /// A token written as `raw` (at most 5 bytes: the longest push).
    fn raw(raw: &[u8]) -> Self {
        let mut bytes = [0; 5];
        let len = raw.len().min(bytes.len());
        bytes[..len].copy_from_slice(&raw[..len]);
        Self {
            bytes,
            len: len as u8,
        }
    }

    /// The push of `value`, in its shortest form.
    fn number(value: f32) -> Self {
        let (bytes, len) = charstring_number_bytes(value);
        Self { bytes, len }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// The value this token pushes, or `None` for an operator.
    fn value(&self) -> Option<f32> {
        decode_operand_f32(self.as_bytes(), 0).map(|(v, _)| v)
    }
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
// Until an operator clears the operand stack, what the baker writes
// stays in `tail` as tokens, so a blend can rewrite a master in place
// without moving what follows it. Each step is constant work per token
// it reads or writes. The budget is charged a token for each token the
// bake reads (a blend reads deltas the stack can hold only once, each
// charged when it was pushed) and for each master a blend rewrites.
//
// One baker serves a whole table: the region-count cache and the token
// budget are shared across glyphs, and the per-glyph state is reset by
// `bake_charstring`.
struct PartialBaker<'a> {
    src_ivs: &'a ItemVariationStore<'a>,
    survivors: &'a [CffSubtableSurvivors],
    global_subrs: &'a [&'a [u8]],
    local_subrs: &'a [&'a [u8]],
    /// Charstring bytes up to the last stack-clearing operator.
    out: Vec<u8>,
    /// Tokens written since then, which blends can still edit.
    tail: Vec<Token>,
    /// Per stack entry: the index in `tail` of its push. A blend
    /// result carries the index of the master push that fed that
    /// blend: a blend is linear in its master, so editing that push
    /// moves the result, and a later blend that pops the result cuts
    /// `tail` back to it, keeping the earlier blend whole.
    stack: Vec<usize>,
    /// Active source-vsindex (mirrors the source's running vsindex).
    src_vsindex: u16,
    /// Last-emitted new-outer in the output. We emit `new_outer,
    /// vsindex` before each blend whose surviving subtable's
    /// `new_outer` differs from the last value we wrote.
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
        survivors: &'a [CffSubtableSurvivors],
        global_subrs: &'a [&'a [u8]],
        budget: usize,
    ) -> Self {
        Self {
            src_ivs,
            survivors,
            global_subrs,
            local_subrs: &[],
            out: Vec::new(),
            tail: Vec::new(),
            stack: Vec::new(),
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
        self.tail.clear();
        self.stack.clear();
        self.src_vsindex = 0;
        self.last_emitted_new_outer = None;
        self.stem_count = 0;
        self.run(cs, 0)?;
        self.clear_stack();
        Ok(core::mem::take(&mut self.out))
    }

    /// Writes the tail out and clears the operand stack, as every
    /// operator but `blend`, `vsindex` and the subroutine calls does.
    fn clear_stack(&mut self) {
        self.stack.clear();
        for token in self.tail.drain(..) {
            self.out.extend_from_slice(token.as_bytes());
        }
    }

    /// Pushes the operand written as `raw`.
    fn push_operand(&mut self, raw: &[u8]) -> Result<(), SubsetError> {
        if self.stack.len() >= MAX_STACK {
            return Err(SubsetError::Unsupported(
                "CFF2 partial bake: operand stack overflow",
            ));
        }
        self.stack.push(self.tail.len());
        self.tail.push(Token::raw(raw));
        Ok(())
    }

    /// The value of stack entry `index`: its push, or the master push
    /// of the blend that left it.
    fn operand_value(&self, index: usize, bad: &'static str) -> Result<f32, SubsetError> {
        self.stack
            .get(index)
            .and_then(|&at| self.tail.get(at))
            .and_then(Token::value)
            .ok_or(SubsetError::Unsupported(bad))
    }

    /// Pops the top operand, decodes its value, and removes its tokens
    /// from the tail.
    fn pop_operand(
        &mut self,
        missing: &'static str,
        bad: &'static str,
    ) -> Result<f32, SubsetError> {
        let top = self
            .stack
            .len()
            .checked_sub(1)
            .ok_or(SubsetError::Unsupported(missing))?;
        let v = self.operand_value(top, bad)?;
        if let Some(at) = self.stack.pop() {
            self.tail.truncate(at);
        }
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
                self.push_operand(push)?;
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
                    // consume (#198). Clearing the stack here used to
                    // corrupt the caller's tracking and made any
                    // subr-pushes-deltas-then-returns pattern fail with
                    // "blend without count operand". Just hand control
                    // back; the inliner's caller continues from the
                    // current stack state.
                    return Ok(());
                }
                OP_HSTEM | OP_VSTEM | OP_HSTEMHM | OP_VSTEMHM => {
                    self.stem_count = self.stem_count.saturating_add(self.stack.len() / 2);
                    self.clear_stack();
                    self.out.push(b0);
                    pos += 1;
                }
                OP_HINTMASK | OP_CNTRMASK => {
                    self.stem_count = self.stem_count.saturating_add(self.stack.len() / 2);
                    self.clear_stack();
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
                    self.clear_stack();
                    self.out.push(b0);
                    self.out.push(b1);
                    pos += 2;
                }
                OP_RMOVETO | OP_HMOVETO | OP_VMOVETO | OP_RLINETO | OP_HLINETO | OP_VLINETO
                | OP_RRCURVETO | OP_HHCURVETO | OP_VVCURVETO | OP_HVCURVETO | OP_VHCURVETO
                | OP_RCURVELINE | OP_RLINECURVE => {
                    self.clear_stack();
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
    /// blend` block in the tail to `[vsindex] | n masters | n*new_k
    /// scaled deltas | count | blend`. When the active subtable
    /// collapsed entirely (no surviving regions), drops the deltas +
    /// count entirely and emits no blend (the masters become the
    /// post-blend stack values directly, equivalent to `n, 0, blend`
    /// post-execution).
    fn apply_blend(&mut self) -> Result<(), SubsetError> {
        // Pop the count operand and strip it from the tail. It is
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
            .stack
            .len()
            .checked_sub(total_deltas)
            .filter(|&first| first >= n)
            .ok_or(underflow)?;

        // Decode every delta operand. Each one leaves the stack here.
        let src_deltas = (delta_first_idx..self.stack.len())
            .map(|i| self.operand_value(i, "CFF2 partial bake: delta decode failed"))
            .collect::<Result<Vec<f32>, SubsetError>>()?;

        // Cut the tail back to the first delta push. The n masters
        // survive; everything from the first delta to the end of the
        // count operand is gone. With no deltas (a subtable with zero
        // regions) nothing is cut.
        if let Some(&first_delta) = self.stack.get(delta_first_idx) {
            self.tail.truncate(first_delta);
        }
        self.stack.truncate(delta_first_idx);

        // Look up the surviving subtable.
        let survivors = self.survivors;
        let Some(survivor) = survivors.get(self.src_vsindex as usize) else {
            // No such subtable: emit no blend at all. The n masters
            // already sit in the tail. They'll be consumed by the next
            // outline op verbatim, equivalent to executing
            // `n, 0, blend` (count consumed, masters intact).
            return Ok(());
        };
        let delta = |i: usize, slot: u16| {
            i.checked_mul(old_k)
                .and_then(|row| row.checked_add(usize::from(slot)))
                .and_then(|k| src_deltas.get(k))
                .copied()
                .ok_or(SubsetError::Unsupported(
                    "CFF2 partial bake: blend delta slot out of range",
                ))
        };

        // The deltas of regions on the pinned axes only move into the
        // masters: a renderer at the new default applies no variations,
        // so they must be in the default values.
        if !survivor.folded.is_empty() {
            let first_master = delta_first_idx - n;
            for i in 0..n {
                let mut fold = 0.0f32;
                for &(slot, scalar) in &survivor.folded {
                    fold += delta(i, slot)? * scalar;
                }
                if fold != 0.0 {
                    self.add_to_operand(first_master + i, fold)?;
                }
            }
        }
        let Some(new_outer) = survivor.new_outer else {
            // Subtable collapsed: emit no blend at all; the masters
            // stand alone, as after `n, 0, blend`.
            return Ok(());
        };

        // Emit `new_outer, vsindex` before this blend when the active
        // outer-in-output differs. CFF2 default vsindex is 0. If the
        // surviving outer is also 0 and we haven't emitted vsindex
        // yet, the prefix is a no-op.
        let need_vsindex = match self.last_emitted_new_outer {
            Some(prev) => prev != new_outer,
            None => new_outer != 0,
        };
        if need_vsindex {
            self.tail.push(Token::number(f32::from(new_outer)));
            self.tail.push(Token::raw(&[OP_VSINDEX]));
            self.last_emitted_new_outer = Some(new_outer);
        }

        // Emit the new deltas in source-slot order, each scaled by the
        // pin_scalar.
        if !survivor.surviving.is_empty() {
            for i in 0..n {
                for &(slot, scalar) in &survivor.surviving {
                    self.tail.push(Token::number(delta(i, slot)? * scalar));
                }
            }
        }
        // Emit the count operand and blend op.
        self.tail.push(Token::number(n as f32));
        self.tail.push(Token::raw(&[OP_BLEND]));

        // The post-blend stack carries n result values. Their stack
        // entries are the original master pushes; the cut above left
        // those in place. A subsequent blend that pops these masters as
        // its own masters cuts back to the original master positions,
        // leaving the already-emitted [masters][deltas][count][BLEND]
        // block alone, which is sound for chained blend ops that build
        // on prior blend results.
        Ok(())
    }

    /// Adds `amount` to stack entry `index`. The entry is a push (its
    /// own, or the master push of the blend that left it), and a blend
    /// is linear in its master, so rewriting that push moves the entry
    /// by `amount`. The push is a token of its own, so nothing after it
    /// moves. The rewrite costs a token of the budget.
    fn add_to_operand(&mut self, index: usize, amount: f32) -> Result<(), SubsetError> {
        const BAD: &str = "CFF2 partial bake: blend master decode failed";
        charge_token(
            &mut self.budget,
            "CFF2 partial bake: charstring work budget exceeded",
        )?;
        let value = self.operand_value(index, BAD)?;
        let token = self
            .stack
            .get(index)
            .and_then(|&at| self.tail.get_mut(at))
            .ok_or(SubsetError::Unsupported(BAD))?;
        *token = Token::number(value + amount);
        Ok(())
    }
}
