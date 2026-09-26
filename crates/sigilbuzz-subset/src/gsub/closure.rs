//! The GSUB side of the subset closure: the substitution targets of
//! types 1, 2, 3 and 8 that kept input glyphs pull in.

use alloc::vec::Vec;

use sigilbuzz::tables::gsub::lookup_type as gsub_type;

use super::unwrap_extension_lookup_type;
use crate::layout::parse_coverage_glyphs;
use crate::util::WorkBudget;

/// Walks a parsed GSUB table and pulls in implicit substitution
/// targets (types 1, 2, 3 and 8) for every kept input glyph. The
/// closure walker already pulls in ligature components (type 4) and
/// mark-base partners; this fills in the substitution-target side.
///
/// Iterates the source GSUB lookups; for each kept input glyph that a
/// type-1/2/3/8 lookup covers, marks the substitution output(s) as
/// kept. Mutates `keep` in place and returns whether anything was
/// added so the caller can decide to re-run the closure pass. Stops
/// early once `budget` is spent.
pub(crate) fn pull_in_substitution_targets(
    face: &sigilbuzz::Face<'_>,
    keep: &mut [bool],
    budget: &WorkBudget,
) -> bool {
    let Ok(Some(gsub)) = face.gsub() else {
        return false;
    };
    let lookups = gsub.lookup_list();
    let mut changed = false;
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else {
            continue;
        };
        if !budget.spend(1 + usize::from(lookup.subtable_count())) {
            return changed;
        }
        let lt = unwrap_extension_lookup_type(&lookup);
        for si in 0..lookup.subtable_count() {
            let Some(sub) = subtable_with_extension(&lookup, si) else {
                continue;
            };
            match lt {
                gsub_type::SINGLE => {
                    changed |= pull_single_in(sub, keep, budget);
                }
                gsub_type::MULTIPLE => {
                    changed |= pull_multiple_in(sub, keep, budget);
                }
                gsub_type::ALTERNATE => {
                    changed |= pull_alternate_default_in(sub, keep, budget);
                }
                gsub_type::REVERSE_CHAINED => {
                    changed |= pull_reverse_chain_in(sub, keep, budget);
                }
                _ => {}
            }
        }
    }
    changed
}

/// Enumerates a Coverage table and charges `budget` for it. Returns
/// `None` once the budget is spent.
fn budgeted_coverage(cov_bytes: &[u8], budget: &WorkBudget) -> Option<Vec<u16>> {
    if budget.is_spent() {
        return None;
    }
    let covered = parse_coverage_glyphs(cov_bytes);
    budget.spend(covered.len() + 1).then_some(covered)
}

#[cfg(test)]
pub(super) fn pull_reverse_chain(sub: &[u8], keep: &mut [bool]) -> bool {
    pull_reverse_chain_in(sub, keep, &WorkBudget::new(crate::util::WORK_LIMIT))
}

#[cfg(test)]
pub(super) fn pull_single(sub: &[u8], keep: &mut [bool]) -> bool {
    pull_single_in(sub, keep, &WorkBudget::new(crate::util::WORK_LIMIT))
}

#[cfg(test)]
pub(super) fn pull_multiple(sub: &[u8], keep: &mut [bool]) -> bool {
    pull_multiple_in(sub, keep, &WorkBudget::new(crate::util::WORK_LIMIT))
}

#[cfg(test)]
pub(super) fn pull_alternate_default(sub: &[u8], keep: &mut [bool]) -> bool {
    pull_alternate_default_in(sub, keep, &WorkBudget::new(crate::util::WORK_LIMIT))
}

fn subtable_with_extension<'a>(
    lookup: &sigilbuzz::tables::layout::Lookup<'a>,
    index: u16,
) -> Option<&'a [u8]> {
    let sub = lookup.subtable_bytes(index)?;
    if lookup.lookup_type() != gsub_type::EXTENSION {
        return Some(sub);
    }
    if sub.len() < 8 {
        return None;
    }
    let ext_off = u32::from_be_bytes([sub[4], sub[5], sub[6], sub[7]]) as usize;
    sub.get(ext_off..)
}

/// Pulls in the substitute glyph for every kept input glyph in a
/// type-1 (single-sub) subtable.
pub(super) fn pull_single_in(sub: &[u8], keep: &mut [bool], budget: &WorkBudget) -> bool {
    if sub.len() < 4 {
        return false;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let Some(cov_bytes) = sub.get(cov_off..) else {
        return false;
    };
    let Some(covered) = budgeted_coverage(cov_bytes, budget) else {
        return false;
    };
    let mut changed = false;
    match format {
        1 => {
            if sub.len() < 6 {
                return false;
            }
            let delta = i16::from_be_bytes([sub[4], sub[5]]);
            for &g in &covered {
                if (g as usize) >= keep.len() || !keep[g as usize] {
                    continue;
                }
                let target = g.wrapping_add(delta as u16);
                if (target as usize) < keep.len() && !keep[target as usize] {
                    keep[target as usize] = true;
                    changed = true;
                }
            }
        }
        2 => {
            if sub.len() < 6 {
                return false;
            }
            let count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
            if count != covered.len() {
                return false;
            }
            let need = 6 + count * 2;
            if sub.len() < need {
                return false;
            }
            for (i, &g) in covered.iter().enumerate() {
                if (g as usize) >= keep.len() || !keep[g as usize] {
                    continue;
                }
                let off = 6 + i * 2;
                let target = u16::from_be_bytes([sub[off], sub[off + 1]]);
                if (target as usize) < keep.len() && !keep[target as usize] {
                    keep[target as usize] = true;
                    changed = true;
                }
            }
        }
        _ => {}
    }
    changed
}

/// Pulls in every substitute in the sequence for every kept input in a
/// type-2 (multiple-sub) subtable.
pub(super) fn pull_multiple_in(sub: &[u8], keep: &mut [bool], budget: &WorkBudget) -> bool {
    if sub.len() < 6 {
        return false;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return false;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let seq_count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let Some(cov_bytes) = sub.get(cov_off..) else {
        return false;
    };
    let Some(covered) = budgeted_coverage(cov_bytes, budget) else {
        return false;
    };
    if covered.len() != seq_count {
        return false;
    }
    let mut changed = false;
    for (i, &g) in covered.iter().enumerate() {
        if (g as usize) >= keep.len() || !keep[g as usize] {
            continue;
        }
        let off_off = 6 + i * 2;
        if off_off + 2 > sub.len() {
            return changed;
        }
        let seq_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        let Some(seq_bytes) = sub.get(seq_off..) else {
            continue;
        };
        if seq_bytes.len() < 2 {
            continue;
        }
        let glyph_count = u16::from_be_bytes([seq_bytes[0], seq_bytes[1]]) as usize;
        let need = 2 + glyph_count * 2;
        if seq_bytes.len() < need {
            continue;
        }
        if !budget.spend(glyph_count) {
            return changed;
        }
        for j in 0..glyph_count {
            let goff = 2 + j * 2;
            let target = u16::from_be_bytes([seq_bytes[goff], seq_bytes[goff + 1]]);
            if (target as usize) < keep.len() && !keep[target as usize] {
                keep[target as usize] = true;
                changed = true;
            }
        }
    }
    changed
}

/// Pulls in only the *default* alternate (index 0) for every kept input
/// in a type-3 (alternate-sub) subtable. User-selected alternates ride
/// in only when their alternate-set output happens to be reachable
/// some other way.
pub(super) fn pull_alternate_default_in(
    sub: &[u8],
    keep: &mut [bool],
    budget: &WorkBudget,
) -> bool {
    if sub.len() < 6 {
        return false;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return false;
    }
    let cov_off = u16::from_be_bytes([sub[2], sub[3]]) as usize;
    let alt_set_count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
    let Some(cov_bytes) = sub.get(cov_off..) else {
        return false;
    };
    let Some(covered) = budgeted_coverage(cov_bytes, budget) else {
        return false;
    };
    if covered.len() != alt_set_count {
        return false;
    }
    let mut changed = false;
    for (i, &g) in covered.iter().enumerate() {
        if (g as usize) >= keep.len() || !keep[g as usize] {
            continue;
        }
        let off_off = 6 + i * 2;
        if off_off + 2 > sub.len() {
            return changed;
        }
        let alt_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
        let Some(alt_bytes) = sub.get(alt_off..) else {
            continue;
        };
        if alt_bytes.len() < 4 {
            continue;
        }
        let glyph_count = u16::from_be_bytes([alt_bytes[0], alt_bytes[1]]) as usize;
        if glyph_count == 0 {
            continue;
        }
        let target = u16::from_be_bytes([alt_bytes[2], alt_bytes[3]]);
        if (target as usize) < keep.len() && !keep[target as usize] {
            keep[target as usize] = true;
            changed = true;
        }
    }
    changed
}

/// Pulls in the substitute of every kept input glyph in a type-8
/// (reverse chaining single substitution) subtable whose context can
/// still match, the rule HarfBuzz's closure applies: every backtrack
/// and lookahead Coverage must list at least one kept glyph. A context
/// that has lost every glyph at one of its positions can never match,
/// and [`rewrite_type8`](super::reverse_chain::rewrite_type8) drops
/// that subtable, so its substitutes are not needed. The closure loop
/// reruns this pass, so context glyphs kept later still bring the
/// substitutes in.
///
/// The layout is the one
/// [`rewrite_type8`](super::reverse_chain::rewrite_type8) reads. A
/// subtable whose Coverage and substitute counts disagree is skipped,
/// as there. Every Coverage walked is charged to `budget`, and the
/// pass stops once it is spent.
pub(super) fn pull_reverse_chain_in(sub: &[u8], keep: &mut [bool], budget: &WorkBudget) -> bool {
    let read = |pos: usize| -> Option<usize> {
        let b = sub.get(pos..pos.checked_add(2)?)?;
        Some(usize::from(u16::from_be_bytes([b[0], b[1]])))
    };
    let is_kept = |g: u16| keep.get(usize::from(g)).copied().unwrap_or(false);
    let context_can_match = |first_slot: usize, count: usize| {
        (0..count).all(|j| {
            read(first_slot + j * 2)
                .and_then(|off| sub.get(off..))
                .and_then(|cov| budgeted_coverage(cov, budget))
                .is_some_and(|glyphs| glyphs.into_iter().any(is_kept))
        })
    };
    if read(0) != Some(1) {
        return false;
    }
    let (Some(cov_off), Some(bt_count)) = (read(2), read(4)) else {
        return false;
    };
    let la_count_at = 6 + bt_count * 2;
    let Some(la_count) = read(la_count_at) else {
        return false;
    };
    let glyph_count_at = la_count_at + 2 + la_count * 2;
    let Some(glyph_count) = read(glyph_count_at) else {
        return false;
    };
    let Some(covered) = sub
        .get(cov_off..)
        .and_then(|cov| budgeted_coverage(cov, budget))
    else {
        return false;
    };
    if covered.len() != glyph_count
        || !context_can_match(6, bt_count)
        || !context_can_match(la_count_at + 2, la_count)
    {
        return false;
    }
    let mut targets = Vec::new();
    for (i, &g) in covered.iter().enumerate() {
        if !is_kept(g) {
            continue;
        }
        let Some(target) = read(glyph_count_at + 2 + i * 2) else {
            return false;
        };
        targets.push(target);
    }
    let mut changed = false;
    for target in targets {
        if let Some(slot) = keep.get_mut(target) {
            changed |= !*slot;
            *slot = true;
        }
    }
    changed
}
