//! Sequence matching over the skipping iterator: HarfBuzz's
//! `match_input`, `match_backtrack` and `match_lookahead`, and the
//! match-position bookkeeping of `apply_lookup` that dispatches a
//! contextual rule's nested lookups.

use alloc::vec::Vec;

use super::{LayoutTable, MatchContext, MatchGlyph, MaySkip};
use crate::tables::layout::SequenceLookupRecord;

/// HarfBuzz's `HB_MAX_CONTEXT_LENGTH`: the longest input sequence a
/// rule can match, and the most match positions nested lookups can
/// grow it to.
pub const MAX_CONTEXT_LENGTH: usize = 64;

/// Glyph positions of a matched input sequence, first glyph first.
#[derive(Debug, Clone, Default)]
pub struct MatchPositions {
    positions: Vec<usize>,
}

impl MatchPositions {
    /// Positions holding just `first`.
    #[must_use]
    pub fn new(first: usize) -> Self {
        Self {
            positions: alloc::vec![first],
        }
    }

    /// The positions, in match order.
    #[must_use]
    pub fn as_slice(&self) -> &[usize] {
        &self.positions
    }

    /// Number of positions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// True when there are none (never for a match, which has at least
    /// its first glyph).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    pub(super) fn push(&mut self, pos: usize) {
        self.positions.push(pos);
    }
}

/// A matched input sequence.
#[derive(Debug, Clone)]
pub struct InputMatch {
    /// Positions of the input glyphs; the first is the start glyph.
    pub positions: MatchPositions,
    /// One past the last input glyph (HarfBuzz's `match_end`).
    pub end: usize,
}

/// Whether the ligature a mark component belongs to may be skipped
/// (the `ligbase` state of HarfBuzz's `match_input`).
#[derive(PartialEq, Eq)]
enum LigBase {
    NotChecked,
    MayNotSkip,
    MaySkip,
}

/// HarfBuzz's `match_input`: matches the `count` input glyphs that
/// follow the glyph at `start`, with the input walk of `cx`.
/// `matches(k, id)` tests the `k`-th of them (from 0). The glyph at
/// `start` itself is not tested; the caller's coverage check stands
/// for it.
///
/// Also enforces HarfBuzz's ligature component rule: glyphs attached
/// to different components of an earlier ligature do not match
/// together, unless that ligature is one the lookup skips (GSUB only;
/// GPOS has no output buffer to find it in).
pub fn match_input(
    glyphs: &[MatchGlyph],
    start: usize,
    count: usize,
    cx: &MatchContext<'_>,
    mut matches: impl FnMut(usize, u16) -> bool,
) -> Option<InputMatch> {
    if count + 1 > MAX_CONTEXT_LENGTH {
        return None;
    }
    let first = *glyphs.get(start)?;
    let rules = cx.input();
    let (first_lig_id, first_lig_comp) = (first.lig_id(), first.lig_comp());
    let mut ligbase = LigBase::NotChecked;
    // Allocated once the first component matches: most attempts fail
    // on it and never allocate.
    let mut positions = MatchPositions::default();
    let mut idx = start;
    for k in 0..count {
        idx = rules.next(glyphs, idx + 1, |id| Some(matches(k, id)))?;
        if positions.is_empty() {
            positions.positions.reserve_exact(count + 1);
            positions.push(start);
        }
        positions.push(idx);
        let this = glyphs[idx];
        let (this_lig_id, this_lig_comp) = (this.lig_id(), this.lig_comp());
        if first_lig_id != 0 && first_lig_comp != 0 {
            // A component attached to an earlier ligature's component:
            // the others must be attached to the same one...
            if first_lig_id != this_lig_id || first_lig_comp != this_lig_comp {
                // ...unless that ligature is ignorable.
                if ligbase == LigBase::NotChecked {
                    ligbase = if cx.table() == LayoutTable::Gsub
                        && ligature_before(glyphs, start, first_lig_id)
                            .is_some_and(|j| rules.may_skip(glyphs[j]) == MaySkip::Yes)
                    {
                        LigBase::MaySkip
                    } else {
                        LigBase::MayNotSkip
                    };
                }
                if ligbase == LigBase::MayNotSkip {
                    return None;
                }
            }
        } else if this_lig_id != 0 && this_lig_comp != 0 && this_lig_id != first_lig_id {
            // Components not attached to a ligature may not match
            // glyphs attached to one, other than the first itself.
            return None;
        }
    }
    if positions.is_empty() {
        positions.push(start);
    }
    Some(InputMatch {
        positions,
        end: idx + 1,
    })
}

/// The ligature glyph with id `lig_id` among the glyphs before
/// `start` that carry that id, as `match_input` looks for it in the
/// output buffer.
fn ligature_before(glyphs: &[MatchGlyph], start: usize, lig_id: u8) -> Option<usize> {
    let mut j = start;
    while j > 0 && glyphs[j - 1].lig_id() == lig_id {
        j -= 1;
        if glyphs[j].lig_comp() == 0 {
            return Some(j);
        }
    }
    None
}

/// HarfBuzz's `match_backtrack`: matches `count` glyphs walking back
/// from `start` with the context walk of `cx`; `matches(k, id)` tests
/// the `k`-th (0 is the nearest).
pub fn match_backtrack(
    glyphs: &[MatchGlyph],
    start: usize,
    count: usize,
    cx: &MatchContext<'_>,
    mut matches: impl FnMut(usize, u16) -> bool,
) -> bool {
    let rules = cx.context();
    let mut idx = start;
    for k in 0..count {
        match rules.prev(glyphs, idx, |id| Some(matches(k, id))) {
            Some(i) => idx = i,
            None => return false,
        }
    }
    true
}

/// HarfBuzz's `match_lookahead`: matches `count` glyphs from `end`
/// (inclusive) onward with the context walk of `cx`.
pub fn match_lookahead(
    glyphs: &[MatchGlyph],
    end: usize,
    count: usize,
    cx: &MatchContext<'_>,
    mut matches: impl FnMut(usize, u16) -> bool,
) -> bool {
    let rules = cx.context();
    let mut from = end;
    for k in 0..count {
        match rules.next(glyphs, from, |id| Some(matches(k, id))) {
            Some(i) => from = i + 1,
            None => return false,
        }
    }
    true
}

/// HarfBuzz's `apply_lookup`: runs a matched rule's nested lookup
/// `records` against the match `positions`, keeping the positions in
/// step with the run as nested substitutions grow or shrink it.
/// Returns where the match now ends, which is where the lookup's walk
/// continues.
///
/// `apply(lookup_index, position)` runs one nested lookup at one glyph
/// position and returns how much it changed the run's length, or
/// `None` when it did not apply. `run_len` is the run's length before
/// the first record.
///
/// As in HarfBuzz, a length change of `n` is assumed to have added
/// glyphs right after the position (growth) or removed the `n` match
/// positions after it (shrinkage).
pub fn apply_nested(
    positions: &mut MatchPositions,
    match_end: usize,
    run_len: usize,
    records: &[SequenceLookupRecord],
    mut apply: impl FnMut(u16, usize) -> Option<isize>,
) -> usize {
    let positions = &mut positions.positions;
    let mut end = match_end as isize;
    let mut len = run_len as isize;
    for rec in records {
        let idx = usize::from(rec.sequence_index);
        let Some(&at) = positions.get(idx) else {
            continue;
        };
        // Earlier nested lookups can delete enough glyphs to leave a
        // position past the end.
        if at as isize >= len {
            continue;
        }
        let Some(mut delta) = apply(rec.lookup_list_index, at) else {
            continue;
        };
        if delta == 0 {
            continue;
        }
        len += delta;
        end += delta;
        if end < at as isize {
            // Never rewind the end past the current position.
            delta += at as isize - end;
            end = at as isize;
        }
        let after = idx + 1;
        if delta > 0 {
            let grown = delta.unsigned_abs();
            if grown + positions.len() > MAX_CONTEXT_LENGTH {
                break;
            }
            // The new glyphs follow the one the nested lookup ran on,
            // and the later positions move along.
            for p in &mut positions[after..] {
                *p += grown;
            }
            positions.splice(after..after, (1..=grown).map(|k| at + k));
        } else {
            // The positions right after this one are gone (at most as
            // many as there are), and the later ones move back.
            let removed = delta.unsigned_abs().min(positions.len() - after);
            positions.drain(after..after + removed);
            for p in &mut positions[after..] {
                *p -= removed;
            }
        }
    }
    end.max(0) as usize
}
