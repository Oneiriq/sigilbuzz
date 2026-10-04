//! `blend` in a CFF2 Private DICT, at an instance.
//!
//! A CFF2 Private DICT can vary its hinting values the way a charstring
//! varies its outline: `vsindex` picks a store subtable, and `blend`
//! takes `n` default values followed by `n` rows of one delta per region
//! and leaves `n` values on the operand stack for the next operator.
//!
//! - A full instance resolves every `blend` at the coordinates and
//!   drops `vsindex`, so the Private DICT no longer reads the store the
//!   instance drops. Values an operator takes as whole numbers round
//!   halves up, the delta-coded arrays (`BlueValues` and the like) on
//!   their absolute values, as fontTools' instancer writes them;
//!   `BlueScale` and `ExpansionFactor` keep their fractions.
//! - A partial instance keeps each `blend`, its deltas cut to the
//!   regions the projected store keeps and scaled by the pinned axes,
//!   as the charstrings' blends are (see [`super::partial`]), and points
//!   `vsindex` at the subtable's new index.
//!
//! An entry no `blend` feeds keeps its bytes.

use alloc::string::String;
use alloc::vec::Vec;

use sigilbuzz::tables::variation_store::ItemVariationStore;

use super::partial::CffSubtableSurvivors;
use super::BlendCache;
use crate::cff::{encode_dict_int, DictEntry, DictOperand};
use crate::util::round_half_up;
use crate::SubsetError;

/// The CFF2 DICT `vsindex` operator.
const OP_VSINDEX: u16 = 22;
/// The CFF2 DICT `blend` operator.
const OP_BLEND: u16 = 23;
/// `BlueScale` and `ExpansionFactor`, which take fractional values.
const REAL_OPS: [u16; 2] = [0x0C09, 0x0C12];
/// `BlueValues`, `OtherBlues`, `FamilyBlues`, `FamilyOtherBlues`,
/// `StemSnapH` and `StemSnapV`: arrays stored as differences.
const DELTA_OPS: [u16; 6] = [6, 7, 8, 9, 0x0C0C, 0x0C0D];

const MALFORMED: SubsetError = SubsetError::Unsupported("CFF2 Private DICT blend is malformed");

/// One operand on the DICT stack: its value, the source bytes when no
/// `blend` produced it, and whether a `blend` did.
struct Operand {
    value: f64,
    raw: Option<DictOperand>,
}

/// The Private DICT `entries` with every `blend` resolved through
/// `blend` (a full instance) and `vsindex` dropped.
pub(super) fn bake_private(
    entries: Vec<DictEntry>,
    blend: &mut BlendCache<'_>,
) -> Result<Vec<DictEntry>, SubsetError> {
    let mut out = Vec::with_capacity(entries.len());
    let mut stack: Vec<Operand> = Vec::new();
    let mut vsindex: u16 = 0;
    for entry in entries {
        push_operands(&mut stack, entry.operands)?;
        match entry.op {
            OP_VSINDEX => {
                vsindex = index_operand(&stack)?;
                stack.clear();
            }
            OP_BLEND => {
                let (k, scalars) = blend.resolve(vsindex);
                let (defaults, deltas) = blend_operands(&mut stack, k)?;
                for (i, &d) in defaults.iter().enumerate() {
                    let row = deltas.get(i * k..(i + 1) * k).unwrap_or_default();
                    let value = row
                        .iter()
                        .zip(scalars)
                        .fold(d, |acc, (&delta, &s)| acc + delta * f64::from(s));
                    stack.push(Operand { value, raw: None });
                }
            }
            op => {
                out.push(DictEntry {
                    op,
                    operands: resolved_operands(op, &stack),
                });
                stack.clear();
            }
        }
    }
    Ok(out)
}

/// The Private DICT `entries` of a partial instance: each `blend` keeps
/// the deltas of the regions `survivors` keeps, scaled, and `vsindex`
/// names the subtable's new index; a `blend` whose subtable is gone
/// leaves its defaults.
pub(super) fn project_private(
    entries: Vec<DictEntry>,
    src_ivs: &ItemVariationStore<'_>,
    survivors: &[Option<CffSubtableSurvivors>],
) -> Result<Vec<DictEntry>, SubsetError> {
    let mut out = Vec::with_capacity(entries.len());
    // Raw operands of the entry being built, blends already rewritten.
    let mut pending: Vec<DictOperand> = Vec::new();
    let mut stack: Vec<Operand> = Vec::new();
    let mut vsindex: u16 = 0;
    for entry in entries {
        push_operands(&mut stack, entry.operands)?;
        match entry.op {
            OP_VSINDEX => {
                vsindex = index_operand(&stack)?;
                stack.clear();
                if let Some(s) = survivors.get(usize::from(vsindex)).and_then(Option::as_ref) {
                    out.push(DictEntry {
                        op: OP_VSINDEX,
                        operands: alloc::vec![int_operand(i32::from(s.new_outer))],
                    });
                }
            }
            OP_BLEND => {
                let old_k = src_ivs
                    .variation_region_count(vsindex)
                    .map_or(0, usize::from);
                let (defaults, deltas) = blend_operands(&mut stack, old_k)?;
                let n = defaults.len();
                // What the blend sits on stays in front of it.
                pending.extend(stack.iter().map(raw_operand));
                stack.clear();
                let survivor = survivors.get(usize::from(vsindex)).and_then(Option::as_ref);
                pending.extend(defaults.iter().map(|&d| number_operand(d, true)));
                if let Some(s) = survivor {
                    for i in 0..n {
                        for &(slot, scalar) in &s.surviving {
                            let d = deltas.get(i * old_k + usize::from(slot)).ok_or(MALFORMED)?;
                            pending.push(number_operand(d * f64::from(scalar), true));
                        }
                    }
                    pending.push(int_operand(n as i32));
                    out.push(DictEntry {
                        op: OP_BLEND,
                        operands: core::mem::take(&mut pending),
                    });
                }
            }
            op => {
                pending.extend(stack.iter().map(raw_operand));
                stack.clear();
                out.push(DictEntry {
                    op,
                    operands: core::mem::take(&mut pending),
                });
            }
        }
    }
    Ok(out)
}

/// Pushes the decoded `operands` onto `stack`.
fn push_operands(stack: &mut Vec<Operand>, operands: Vec<DictOperand>) -> Result<(), SubsetError> {
    for o in operands {
        let value = match o.int_value {
            Some(v) => f64::from(v),
            None => decode_real(&o.raw).ok_or(MALFORMED)?,
        };
        stack.push(Operand {
            value,
            raw: Some(o),
        });
    }
    Ok(())
}

/// The subtable index a `vsindex` takes from the top of `stack`.
fn index_operand(stack: &[Operand]) -> Result<u16, SubsetError> {
    let v = stack.last().ok_or(MALFORMED)?.value;
    if (0.0..=f64::from(u16::MAX)).contains(&v) {
        Ok(v as u16)
    } else {
        Err(MALFORMED)
    }
}

/// Pops a `blend`'s count, then its defaults and deltas, off the top of
/// `stack`: `n` defaults followed by `n` rows of `k` deltas, one per
/// region of the subtable. What lies below them stays.
fn blend_operands(stack: &mut Vec<Operand>, k: usize) -> Result<(Vec<f64>, Vec<f64>), SubsetError> {
    let n = stack.pop().ok_or(MALFORMED)?.value;
    if !(1.0..=f64::from(u16::MAX)).contains(&n) {
        return Err(MALFORMED);
    }
    let n = n as usize;
    let take = k
        .checked_add(1)
        .and_then(|r| r.checked_mul(n))
        .ok_or(MALFORMED)?;
    let split = stack.len().checked_sub(take).ok_or(MALFORMED)?;
    let top: Vec<f64> = stack.drain(split..).map(|o| o.value).collect();
    let (defaults, deltas) = top.split_at(n);
    Ok((defaults.to_vec(), deltas.to_vec()))
}

/// The operands of an operator `op` whose operands are `stack`: the
/// source bytes when no `blend` fed any, else the values written out.
fn resolved_operands(op: u16, stack: &[Operand]) -> Vec<DictOperand> {
    if stack.iter().all(|o| o.raw.is_some()) {
        return stack.iter().map(raw_operand).collect();
    }
    let real = REAL_OPS.contains(&op);
    if DELTA_OPS.contains(&op) {
        // Round the absolute values, then store their differences.
        let mut absolute = 0.0f64;
        let mut previous = 0i32;
        return stack
            .iter()
            .map(|o| {
                absolute += o.value;
                let rounded = round_half_up(absolute as f32);
                let d = rounded.wrapping_sub(previous);
                previous = rounded;
                int_operand(d)
            })
            .collect();
    }
    stack
        .iter()
        .map(|o| number_operand(o.value, real))
        .collect()
}

/// The operand as written: its source bytes, or its value.
fn raw_operand(o: &Operand) -> DictOperand {
    o.raw
        .clone()
        .unwrap_or_else(|| number_operand(o.value, true))
}

/// An integer operand.
fn int_operand(v: i32) -> DictOperand {
    DictOperand {
        int_value: Some(v),
        raw: encode_dict_int(v),
    }
}

/// `v` as an operand: an integer when it is one (or, unless `real`,
/// rounded halves up), else a real number.
fn number_operand(v: f64, real: bool) -> DictOperand {
    let whole = v.round();
    if (v - whole).abs() < 1e-9 && whole.abs() < 2e9 {
        return int_operand(whole as i32);
    }
    if !real {
        return int_operand(round_half_up(v as f32));
    }
    DictOperand {
        int_value: None,
        raw: encode_real(v),
    }
}

/// Decodes a DICT real number (`b0 = 30`, nibble-packed).
fn decode_real(raw: &[u8]) -> Option<f64> {
    let mut text = String::new();
    for &b in raw.get(1..)? {
        for nibble in [b >> 4, b & 0x0F] {
            match nibble {
                0..=9 => text.push(char::from(b'0' + nibble)),
                0x0A => text.push('.'),
                0x0B => text.push('E'),
                0x0C => text.push_str("E-"),
                0x0E => text.push('-'),
                0x0F => return text.parse().ok(),
                _ => return None,
            }
        }
    }
    text.parse().ok()
}

/// Encodes `v` as a DICT real number, with up to nine significant
/// digits.
fn encode_real(v: f64) -> Vec<u8> {
    let text = alloc::format!("{:.9}", v);
    // Trim trailing zeros after the point, and the point itself.
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        text.as_str()
    };
    let mut nibbles: Vec<u8> = Vec::with_capacity(text.len() + 1);
    for c in text.chars() {
        nibbles.push(match c {
            '0'..='9' => c as u8 - b'0',
            '.' => 0x0A,
            _ => 0x0E, // '-'
        });
    }
    nibbles.push(0x0F);
    if nibbles.len() % 2 != 0 {
        nibbles.push(0x0F);
    }
    let mut out = alloc::vec![30u8];
    out.extend(nibbles.chunks_exact(2).map(|p| (p[0] << 4) | p[1]));
    out
}

#[cfg(test)]
mod tests;
