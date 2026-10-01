//! The glyph flags of Arabic-style cursive joining, from HarfBuzz's
//! `arabic_joining` (`hb-ot-shaper-arabic.cc`).
//!
//! While it picks each letter's joining form, HarfBuzz's state machine
//! marks where the forms depend on the neighbors: a letter whose form
//! the next letter changed is unsafe to break with it (or, with
//! `PRODUCE_SAFE_TO_INSERT_TATWEEL`, a place a tatweel may go), and a
//! pair whose forms the next letter could have changed is unsafe to
//! concatenate. The forms themselves come from
//! [`crate::ot::arabic`]; this module walks the same table for the
//! flags only.
//!
//! The Arabic shaper does this for horizontal Arabic, and the
//! Universal Shaping Engine for the other joining scripts sigilbuzz
//! shapes, Mongolian and N'Ko (`has_arabic_joining`).

use super::glyph_flags::FlagCx;
use crate::buffer::Glyph;
use crate::ot::arabic::JoiningContext;
use crate::unicode::joining::JoiningType;

/// A state table entry: whether it acts on the previous letter, and
/// the next state.
#[derive(Clone, Copy)]
struct Entry {
    prev_action: bool,
    next_state: u8,
}

const fn e(prev_action: bool, next_state: u8) -> Entry {
    Entry {
        prev_action,
        next_state,
    }
}

/// HarfBuzz's `arabic_state_table`, columns U, L, R, D, ALAPH and
/// DALATH_RISH, with only the parts the flags need.
const TABLE: [[Entry; 6]; 7] = [
    [
        e(false, 0),
        e(false, 2),
        e(false, 1),
        e(false, 2),
        e(false, 1),
        e(false, 6),
    ],
    [
        e(false, 0),
        e(false, 2),
        e(false, 1),
        e(false, 2),
        e(false, 5),
        e(false, 6),
    ],
    [
        e(false, 0),
        e(false, 2),
        e(true, 1),
        e(true, 3),
        e(true, 4),
        e(true, 6),
    ],
    [
        e(false, 0),
        e(false, 2),
        e(true, 1),
        e(true, 3),
        e(true, 4),
        e(true, 6),
    ],
    [
        e(false, 0),
        e(false, 2),
        e(true, 1),
        e(true, 2),
        e(true, 5),
        e(true, 6),
    ],
    [
        e(false, 0),
        e(false, 2),
        e(true, 1),
        e(true, 2),
        e(true, 5),
        e(true, 6),
    ],
    [
        e(false, 0),
        e(false, 2),
        e(false, 1),
        e(false, 2),
        e(false, 5),
        e(false, 6),
    ],
];

/// The table column of a joining type; `None` for a transparent one,
/// which the machine passes over. sigilbuzz has no Syriac joining
/// groups, so ALAPH and DALATH_RISH never occur.
const fn column(t: JoiningType) -> Option<usize> {
    match t {
        JoiningType::U => Some(0),
        JoiningType::L => Some(1),
        JoiningType::R => Some(2),
        JoiningType::D | JoiningType::C => Some(3),
        JoiningType::T => None,
    }
}

/// HarfBuzz's "this type is R or later": R, D, C, and the groups.
const fn joins_backward(t: JoiningType) -> bool {
    matches!(t, JoiningType::R | JoiningType::D | JoiningType::C)
}

/// Sets the joining flags on `glyphs`, one glyph per character of
/// joining type `types[i]`, with `context` the nearest
/// non-transparent characters around the run.
pub(super) fn set_joining_flags(
    glyphs: &mut [Glyph],
    types: &[JoiningType],
    context: JoiningContext,
    flags: FlagCx,
) {
    if glyphs.len() != types.len() {
        return;
    }
    let step = |state: u8, col: usize| {
        TABLE
            .get(usize::from(state))
            .and_then(|row| row.get(col))
            .copied()
            .unwrap_or(e(false, 0))
    };
    let mut state = context
        .before
        .and_then(column)
        .map_or(0, |col| step(0, col).next_state);
    let mut prev: Option<usize> = None;
    for (i, &t) in types.iter().enumerate() {
        let Some(col) = column(t) else {
            continue;
        };
        let entry = step(state, col);
        match prev {
            Some(p) if entry.prev_action => flags.safe_to_insert_tatweel(glyphs, p, i + 1),
            Some(p) if joins_backward(t) || (2..=5).contains(&state) => {
                flags.unsafe_to_concat(glyphs, p, i + 1);
            }
            None if joins_backward(t) => flags.unsafe_to_concat(glyphs, 0, i + 1),
            _ => {}
        }
        prev = Some(i);
        state = entry.next_state;
    }
    let len = glyphs.len();
    if let (Some(col), Some(p)) = (context.after.and_then(column), prev) {
        if step(state, col).prev_action {
            flags.safe_to_insert_tatweel(glyphs, p, len);
        } else if (2..=5).contains(&state) {
            flags.unsafe_to_concat(glyphs, p, len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{BufferFlags, ClusterLevel};
    use alloc::vec::Vec;

    fn flags_of(types: &[JoiningType], context: JoiningContext, buffer: BufferFlags) -> Vec<u32> {
        let mut glyphs: Vec<Glyph> = (0..types.len() as u32).map(|i| Glyph::new(1, i)).collect();
        let cx = FlagCx::new(ClusterLevel::MonotoneCharacters, buffer);
        set_joining_flags(&mut glyphs, types, context, cx);
        glyphs.iter().map(|g| g.flags.bits()).collect()
    }

    use JoiningType::{D, R, T, U};

    #[test]
    fn joined_letters_are_unsafe_to_break_between_them() {
        let none = JoiningContext::NONE;
        // beh beh: the second turns the first into its initial form.
        assert_eq!(flags_of(&[D, D], none, BufferFlags::DEFAULT), [0, 3]);
        // A mark between them is passed over.
        assert_eq!(flags_of(&[D, T, D], none, BufferFlags::DEFAULT), [0, 3, 3]);
        // alef then beh do not join.
        assert_eq!(flags_of(&[R, D], none, BufferFlags::DEFAULT), [0, 0]);
    }

    #[test]
    fn concat_and_tatweel_flags_follow_the_buffer_flags() {
        let none = JoiningContext::NONE;
        let concat = BufferFlags::PRODUCE_UNSAFE_TO_CONCAT;
        // beh, then a non-joining letter: a joining letter could have
        // followed, so the pair is unsafe to concatenate.
        assert_eq!(flags_of(&[D, U], none, concat), [2, 2]);
        // A letter that joins backward at the start of the run.
        assert_eq!(flags_of(&[R], none, concat), [2]);
        let tatweel = BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL;
        assert_eq!(flags_of(&[D, D], none, tatweel), [0, 4]);
        // The post-context can still change the last letter's form.
        let after = JoiningContext {
            before: None,
            after: Some(D),
        };
        assert_eq!(flags_of(&[D], after, BufferFlags::DEFAULT), [0]);
        assert_eq!(flags_of(&[D, D], after, BufferFlags::DEFAULT), [0, 3]);
    }
}
