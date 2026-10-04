//! Charstring execution for the subsetter: which subroutines the kept
//! glyphs reach, and where each body reached calls them.
//!
//! A `hintmask` or `cntrmask` is followed by one bit per stem hint the
//! glyph declared before its first mask. Those hints, like the operands
//! a stem operator or a CFF2 `blend` takes, can come from the caller of
//! the subroutine that holds the mask, so a body read on its own does
//! not say how many mask bytes to step over. Read wrong, the mask bytes
//! pass for operators and operands, and the subroutine calls found
//! after them are wrong.
//!
//! The walk runs each kept glyph's charstring as HarfBuzz's subsetter
//! interprets it:
//!
//! - A subroutine call runs the callee with the caller's operand stack
//!   and hint count, and the callee's stems count for the caller's
//!   masks after it returns.
//! - The stem count sizes the glyph's first mask, operands left on the
//!   stack counting as an implicit `vstem`, and every later mask takes
//!   the same size.
//! - A CFF2 `blend` takes `n`, then `n` default values and `n` rows of
//!   one delta per region of the `vsindex` in effect, and leaves the
//!   defaults. The region count comes from the font's variation store;
//!   a `vsindex` past its subtables blends no deltas, as in HarfBuzz.
//!   The `vsindex` in effect starts at the one the glyph's Private DICT
//!   sets.
//! - Subroutines nest at most 10 deep, and the operand stack holds at
//!   most 513 operands.
//!
//! Each body is read once, the first time a glyph reaches it, as
//! HarfBuzz parses it, and the walk records the subroutine calls of
//! that reading for the renumbering. A subroutine number must be pushed
//! by the body that calls it, so the push can be rewritten in place.
//!
//! The walk charges one unit of `budget` for every operand and operator
//! it reads, inside subroutines too, and one for each value a `blend`
//! leaves, and fails once the budget is spent. Subroutines that call
//! each other many times can describe far more work than the table's
//! size. Its memory follows the subroutines the glyphs reach, not the
//! size of the INDEXes: Font DICTs that share one Subrs INDEX keep
//! nothing per entry they do not reach.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use sigilbuzz::tables::variation_store::ItemVariationStore;

use super::charstring::{
    decode_operand, subr_bias, SubrCall, SubrKind, OP_BLEND, OP_CALLGSUBR, OP_CALLSUBR,
    OP_CNTRMASK, OP_ENDCHAR, OP_ESCAPE, OP_HHCURVETO, OP_HINTMASK, OP_HLINETO, OP_HMOVETO,
    OP_HSTEM, OP_HSTEMHM, OP_HVCURVETO, OP_RCURVELINE, OP_RETURN, OP_RLINECURVE, OP_RLINETO,
    OP_RMOVETO, OP_RRCURVETO, OP_SHORTINT, OP_VHCURVETO, OP_VLINETO, OP_VMOVETO, OP_VSINDEX,
    OP_VSTEM, OP_VSTEMHM, OP_VVCURVETO,
};
use crate::SubsetError;

/// Subroutine calls one glyph may nest, HarfBuzz's `kMaxCallLimit`
/// and the Type 2 limit.
const MAX_CALL_DEPTH: usize = 10;

/// Operands the stack may hold, HarfBuzz's argument stack size and the
/// CFF2 limit.
const MAX_STACK: usize = 513;

/// Minimum number of charstring tokens one subset may read.
const MIN_WALK_TOKENS: usize = 1 << 22;

/// Token allowance per byte of the source table.
const WALK_TOKENS_PER_BYTE: usize = 64;

/// Token budget for walking the kept glyphs of a `CFF ` or `CFF2` table
/// of `table_len` bytes: 64 tokens per byte, at least 2^22. Real fonts
/// read their subroutines a few times over at most.
pub(crate) fn walk_budget(table_len: usize) -> usize {
    table_len
        .saturating_mul(WALK_TOKENS_PER_BYTE)
        .max(MIN_WALK_TOKENS)
}

/// Region counts of a CFF2 variation store, per `vsindex`.
pub(crate) struct BlendRegions<'a> {
    store: Option<ItemVariationStore<'a>>,
    counts: BTreeMap<u16, usize>,
}

impl<'a> BlendRegions<'a> {
    /// The region counts of the store in `store_bytes` (without the CFF2
    /// length prefix). A store that is absent or cannot be read has no
    /// subtables, so every `blend` blends no deltas, as HarfBuzz reads
    /// a missing store.
    pub(crate) fn new(store_bytes: Option<&'a [u8]>) -> Self {
        Self {
            store: store_bytes.and_then(|b| ItemVariationStore::parse(b).ok()),
            counts: BTreeMap::new(),
        }
    }

    /// The deltas one blended value carries under `vsindex`: the region
    /// count of that subtable, or 0 when there is none.
    fn count(&mut self, vsindex: u16) -> usize {
        let store = self.store.as_ref();
        *self.counts.entry(vsindex).or_insert_with(|| {
            store
                .and_then(|s| s.variation_region_count(vsindex))
                .map_or(0, usize::from)
        })
    }
}

/// How far the walk has read one subroutine it reached. A subroutine
/// not reached has no entry.
#[derive(Debug, Clone)]
enum Visit {
    /// Being read for the first time: a call to it now is recursion.
    Reading,
    /// Read, with the calls its reading found.
    Read(Vec<SubrCall>),
}

impl Visit {
    fn calls(&self) -> Option<&[SubrCall]> {
        match self {
            Self::Read(calls) => Some(calls),
            Self::Reading => None,
        }
    }
}

/// The bodies of one Font DICT: its local subroutines and the
/// `vsindex` its Private DICT sets, and what the walk found in them.
pub(crate) struct FdWalk<'a> {
    locals: &'a [&'a [u8]],
    vsindex: u16,
    /// The local subroutines reached, by index.
    local_visits: BTreeMap<u32, Visit>,
    /// Global subroutines this Font DICT's glyphs reached.
    reached_globals: BTreeSet<u32>,
}

impl<'a> FdWalk<'a> {
    /// The bodies of a Font DICT with local subroutines `locals` and
    /// Private DICT `vsindex`, ready for its glyphs.
    pub(crate) fn new(locals: &'a [&'a [u8]], vsindex: u16) -> Self {
        Self {
            locals,
            vsindex,
            local_visits: BTreeMap::new(),
            reached_globals: BTreeSet::new(),
        }
    }

    /// The calls of local subroutine `index`, when a glyph reached it.
    pub(crate) fn local_calls(&self, index: usize) -> Option<&[SubrCall]> {
        let index = u32::try_from(index).ok()?;
        self.local_visits.get(&index).and_then(Visit::calls)
    }

    /// The local subroutines the glyphs reached, ascending.
    pub(crate) fn kept_locals(&self) -> Vec<u32> {
        kept(&self.local_visits)
    }

    /// The global subroutines this Font DICT's glyphs reached, ascending.
    pub(crate) fn reached_globals(&self) -> Vec<u32> {
        self.reached_globals.iter().copied().collect()
    }
}

/// The walk over every kept glyph of one table. See the module docs.
pub(crate) struct CharstringWalk<'a> {
    globals: &'a [&'a [u8]],
    /// The global subroutines reached, by index.
    global_visits: BTreeMap<u32, Visit>,
    /// `Some` for CFF2, whose `blend` and `vsindex` operators the walk
    /// runs. A CFF1 charstring has neither, and the walk clears the
    /// stack at those opcodes as the per-body scanner does.
    blend: Option<BlendRegions<'a>>,
    budget: usize,
    /// Identifies each body run, so a call can tell whether its
    /// subroutine number was pushed by the calling body.
    next_frame: u64,
}

/// One operand on the stack.
#[derive(Debug, Clone, Copy)]
struct Operand {
    /// The value, its integer part for a 16.16 push.
    value: i32,
    /// Where it was pushed: the body run, the byte offset and the
    /// encoded length. `None` for a `blend` result.
    push: Option<Push>,
}

#[derive(Debug, Clone, Copy)]
struct Push {
    frame: u64,
    offset: usize,
    len: usize,
}

/// The state one glyph's run carries through its subroutine calls.
struct Glyph {
    stack: Vec<Operand>,
    /// Stem hints declared so far.
    stems: usize,
    /// Bytes each mask takes, fixed at the glyph's first mask.
    mask_bytes: Option<usize>,
    vsindex: u16,
    /// `endchar` ran: the glyph is done, in whatever body it ran.
    ended: bool,
}

const WORK_EXCEEDED: SubsetError = SubsetError::Unsupported("CFF charstring work budget exceeded");

impl<'a> CharstringWalk<'a> {
    /// A walk over a table with global subroutines `globals`, charging
    /// `budget` (see [`walk_budget`]). `blend` is `Some` for a CFF2
    /// table.
    pub(crate) fn new(
        globals: &'a [&'a [u8]],
        blend: Option<BlendRegions<'a>>,
        budget: usize,
    ) -> Self {
        Self {
            globals,
            global_visits: BTreeMap::new(),
            blend,
            budget,
            next_frame: 0,
        }
    }

    /// The calls of global subroutine `index`, when a glyph reached it.
    pub(crate) fn global_calls(&self, index: usize) -> Option<&[SubrCall]> {
        let index = u32::try_from(index).ok()?;
        self.global_visits.get(&index).and_then(Visit::calls)
    }

    /// The global subroutines any glyph reached, ascending.
    pub(crate) fn kept_globals(&self) -> Vec<u32> {
        kept(&self.global_visits)
    }

    /// Runs the charstring of one glyph of `fd` and returns the calls
    /// it makes itself, in order.
    ///
    /// # Errors
    ///
    /// [`SubsetError::Unsupported`] for a charstring HarfBuzz's
    /// subsetter would not read either: a truncated operand or mask, an
    /// unknown operator, a call without its number, to a subroutine
    /// that does not exist, nested more than 10 deep or reaching itself,
    /// a stack past 513 operands, a `blend` short of operands, or a
    /// spent budget.
    pub(crate) fn glyph(
        &mut self,
        fd: &mut FdWalk<'a>,
        charstring: &[u8],
    ) -> Result<Vec<SubrCall>, SubsetError> {
        let mut glyph = Glyph {
            stack: Vec::new(),
            stems: 0,
            mask_bytes: None,
            vsindex: fd.vsindex,
            ended: false,
        };
        let mut calls = Vec::new();
        self.run(fd, &mut glyph, charstring, 0, Some(&mut calls))?;
        Ok(calls)
    }

    /// Runs one body at call depth `depth`, recording its calls into
    /// `record` when this is its first reading.
    fn run(
        &mut self,
        fd: &mut FdWalk<'a>,
        glyph: &mut Glyph,
        body: &[u8],
        depth: usize,
        mut record: Option<&mut Vec<SubrCall>>,
    ) -> Result<(), SubsetError> {
        let frame = self.next_frame;
        self.next_frame = self.next_frame.wrapping_add(1);
        let mut pos = 0;
        while let Some(&b0) = body.get(pos) {
            self.budget = self.budget.checked_sub(1).ok_or(WORK_EXCEEDED)?;
            if b0 >= 32 || b0 == OP_SHORTINT {
                let (value, len) = if b0 == OP_SHORTINT {
                    let bytes = body
                        .get(pos + 1..pos + 3)
                        .ok_or(SubsetError::Unsupported("CFF shortint truncated"))?;
                    (i32::from(i16::from_be_bytes([bytes[0], bytes[1]])), 3)
                } else {
                    decode_operand(body, pos)
                        .ok_or(SubsetError::Unsupported("CFF charstring operand truncated"))?
                };
                if glyph.stack.len() >= MAX_STACK {
                    return Err(SubsetError::Unsupported(
                        "CFF charstring stack past 513 operands",
                    ));
                }
                glyph.stack.push(Operand {
                    value,
                    push: Some(Push {
                        frame,
                        offset: pos,
                        len,
                    }),
                });
                pos += len;
                continue;
            }
            match b0 {
                OP_CALLSUBR | OP_CALLGSUBR => {
                    let (kind, missing) = if b0 == OP_CALLSUBR {
                        (SubrKind::Local, "CFF callsubr without operand")
                    } else {
                        (SubrKind::Global, "CFF callgsubr without operand")
                    };
                    let top = glyph.stack.pop().ok_or(SubsetError::Unsupported(missing))?;
                    let push =
                        top.push
                            .filter(|p| p.frame == frame)
                            .ok_or(SubsetError::Unsupported(
                                "CFF subroutine number not pushed by the calling body",
                            ))?;
                    let count = match kind {
                        SubrKind::Local => fd.locals.len(),
                        SubrKind::Global => self.globals.len(),
                    };
                    let index_after_bias = i64::from(top.value) + i64::from(subr_bias(count));
                    let index = usize::try_from(index_after_bias)
                        .ok()
                        .filter(|&i| i < count)
                        .ok_or(SubsetError::Unsupported(
                            "CFF subroutine index out of range",
                        ))?;
                    if let Some(calls) = record.as_deref_mut() {
                        calls.push(SubrCall {
                            kind,
                            index_after_bias,
                            raw_operand: top.value,
                            operand_byte_offset: push.offset,
                            operand_byte_len: push.len,
                        });
                    }
                    if depth >= MAX_CALL_DEPTH {
                        return Err(SubsetError::Unsupported(
                            "CFF subroutines nested more than 10 deep",
                        ));
                    }
                    self.call(fd, glyph, kind, index, depth + 1)?;
                    if glyph.ended {
                        return Ok(());
                    }
                    pos += 1;
                }
                // A subroutine returns; a charstring that returns ends,
                // as the per-body scanner reads it.
                OP_RETURN => return Ok(()),
                OP_ENDCHAR => {
                    glyph.ended = true;
                    return Ok(());
                }
                OP_HSTEM | OP_VSTEM | OP_HSTEMHM | OP_VSTEMHM => {
                    glyph.stems = glyph.stems.saturating_add(glyph.stack.len() / 2);
                    glyph.stack.clear();
                    pos += 1;
                }
                OP_HINTMASK | OP_CNTRMASK => {
                    let mask_bytes = match glyph.mask_bytes {
                        Some(bytes) => bytes,
                        None => {
                            // Operands left before the first mask are
                            // an implicit vstem.
                            glyph.stems = glyph.stems.saturating_add(glyph.stack.len() / 2);
                            let bytes = glyph.stems.div_ceil(8);
                            glyph.mask_bytes = Some(bytes);
                            bytes
                        }
                    };
                    glyph.stack.clear();
                    // `pos < len` here, so `pos + 1` cannot overflow.
                    pos = (pos + 1)
                        .checked_add(mask_bytes)
                        .filter(|&next| next <= body.len())
                        .ok_or(SubsetError::Unsupported("CFF hintmask tail truncated"))?;
                }
                OP_ESCAPE => {
                    if pos + 2 > body.len() {
                        return Err(SubsetError::Unsupported("CFF escape truncated"));
                    }
                    glyph.stack.clear();
                    pos += 2;
                }
                OP_RMOVETO | OP_HMOVETO | OP_VMOVETO | OP_RLINETO | OP_HLINETO | OP_VLINETO
                | OP_RRCURVETO | OP_HHCURVETO | OP_VVCURVETO | OP_HVCURVETO | OP_VHCURVETO
                | OP_RCURVELINE | OP_RLINECURVE => {
                    glyph.stack.clear();
                    pos += 1;
                }
                OP_VSINDEX | OP_BLEND if self.blend.is_none() => {
                    glyph.stack.clear();
                    pos += 1;
                }
                OP_VSINDEX => {
                    let top = glyph
                        .stack
                        .pop()
                        .ok_or(SubsetError::Unsupported("CFF2 vsindex without operand"))?;
                    // Out-of-range values saturate, as the core reads
                    // them; past the store's subtables they blend no
                    // deltas.
                    glyph.vsindex = u16::try_from(top.value.max(0)).unwrap_or(u16::MAX);
                    glyph.stack.clear();
                    pos += 1;
                }
                OP_BLEND => {
                    self.blend(glyph)?;
                    pos += 1;
                }
                _ => {
                    return Err(SubsetError::Unsupported("CFF unknown charstring operator"));
                }
            }
        }
        Ok(())
    }

    /// Runs subroutine `index` of `kind` for `glyph`, reading it for the
    /// first time when no glyph has reached it yet.
    fn call(
        &mut self,
        fd: &mut FdWalk<'a>,
        glyph: &mut Glyph,
        kind: SubrKind,
        index: usize,
        depth: usize,
    ) -> Result<(), SubsetError> {
        const OUT_OF_RANGE: SubsetError =
            SubsetError::Unsupported("CFF subroutine index out of range");
        let body = match kind {
            SubrKind::Local => fd.locals.get(index).copied(),
            SubrKind::Global => self.globals.get(index).copied(),
        }
        .ok_or(OUT_OF_RANGE)?;
        let key = u32::try_from(index).map_err(|_| OUT_OF_RANGE)?;
        if kind == SubrKind::Global {
            fd.reached_globals.insert(key);
        }
        let visits = match kind {
            SubrKind::Local => &mut fd.local_visits,
            SubrKind::Global => &mut self.global_visits,
        };
        match visits.get(&key) {
            Some(Visit::Read(_)) => return self.run(fd, glyph, body, depth, None),
            Some(Visit::Reading) => {
                return Err(SubsetError::Unsupported("CFF subroutine reaches itself"));
            }
            None => {
                visits.insert(key, Visit::Reading);
            }
        }
        let mut calls = Vec::new();
        self.run(fd, glyph, body, depth, Some(&mut calls))?;
        let visits = match kind {
            SubrKind::Local => &mut fd.local_visits,
            SubrKind::Global => &mut self.global_visits,
        };
        visits.insert(key, Visit::Read(calls));
        Ok(())
    }

    /// Runs a CFF2 `blend`: pops `n`, the `n` rows of deltas, and leaves
    /// the `n` defaults, now results of the blend.
    fn blend(&mut self, glyph: &mut Glyph) -> Result<(), SubsetError> {
        const SHORT: SubsetError = SubsetError::Unsupported("CFF2 blend short of operands");
        let n = glyph.stack.pop().ok_or(SHORT)?.value;
        let n = usize::try_from(n)
            .map_err(|_| SubsetError::Unsupported("CFF2 blend count negative"))?;
        let regions = self
            .blend
            .as_mut()
            .map_or(0, |blend| blend.count(glyph.vsindex));
        let deltas = n.checked_mul(regions).ok_or(SHORT)?;
        let total = deltas.checked_add(n).ok_or(SHORT)?;
        let start = glyph.stack.len().checked_sub(total).ok_or(SHORT)?;
        // The deltas were charged as they were pushed; the `n` values
        // the blend leaves are charged here, so blends that leave the
        // same values again and again (no regions) cost what they do.
        self.budget = self.budget.checked_sub(n).ok_or(WORK_EXCEEDED)?;
        glyph.stack.truncate(start + n);
        for operand in glyph.stack.get_mut(start..).unwrap_or_default() {
            operand.push = None;
        }
        Ok(())
    }
}

/// The indexes of the `visits` that were read, ascending.
fn kept(visits: &BTreeMap<u32, Visit>) -> Vec<u32> {
    visits
        .iter()
        .filter(|(_, v)| matches!(v, Visit::Read(_)))
        .map(|(&i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests;
