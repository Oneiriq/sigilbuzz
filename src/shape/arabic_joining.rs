//! HarfBuzz's Arabic joining state machine (`arabic_joining` in
//! `hb-ot-shaper-arabic.cc`): the joining action of each character and
//! the glyph flags it sets.
//!
//! The action of a character is the feature that picks its form:
//! `isol`, `fina`, `medi`, or `init`, and for the Syriac letter ALAPH
//! after a letter that does not join it, `fin2` or `fin3`, with `med2`
//! for the letter before an ALAPH in its final form. The machine reads
//! each character's joining type (see [`crate::unicode::joining`]),
//! with two Syriac joining groups of `ArabicShaping.txt` as columns of
//! their own: ALAPH (U+0710) and DALATH RISH (U+0715, U+0716, U+072A,
//! U+072F).
//!
//! While it picks the forms, the machine marks where they depend on the
//! neighbors: a letter whose form the next letter changed is unsafe to
//! break with it (or, with `PRODUCE_SAFE_TO_INSERT_TATWEEL`, a place a
//! tatweel may go), and a pair whose forms the next letter could have
//! changed is unsafe to concatenate.
//!
//! The Arabic shaper runs the machine for horizontal Arabic and Syriac,
//! and the Universal Shaping Engine for its other joining scripts
//! (`has_arabic_joining`). Those take their forms from
//! [`crate::ot::arabic`], which gives the same forms for every joining
//! type and has no Syriac groups.

use alloc::vec::Vec;

use super::glyph_flags::FlagCx;
use crate::buffer::Glyph;
use crate::unicode::joining::{joining_type, JoiningType};

/// The joining action of a character (`arabic_action_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Isol,
    Fina,
    Fin2,
    Fin3,
    Medi,
    Med2,
    Init,
    None,
}

impl Action {
    /// The actions with their features, in the order the Arabic shaper
    /// applies them, one stage each (`arabic_features`).
    pub(super) const FEATURES: [(Self, [u8; 4]); 7] = [
        (Self::Isol, *b"isol"),
        (Self::Fina, *b"fina"),
        (Self::Fin2, *b"fin2"),
        (Self::Fin3, *b"fin3"),
        (Self::Medi, *b"medi"),
        (Self::Med2, *b"med2"),
        (Self::Init, *b"init"),
    ];

    /// The action's code in [`Glyph::indic_position`], where it rides
    /// with its glyph through the substitutions (HarfBuzz keeps it in
    /// the glyph info as `arabic_shaping_action`).
    const fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Isol => 1,
            Self::Fina => 2,
            Self::Fin2 => 3,
            Self::Fin3 => 4,
            Self::Medi => 5,
            Self::Med2 => 6,
            Self::Init => 7,
        }
    }

    /// True when `glyph` carries this action.
    pub(super) fn is_on(self, glyph: &Glyph) -> bool {
        self != Self::None && glyph.indic_position == self.code()
    }
}

/// The code of a glyph of a `stch` multiple substitution that keeps
/// its width when the stretch fills the word (`STCH_FIXED`).
pub(super) const STCH_FIXED: u8 = 8;
/// The code of a glyph of a `stch` multiple substitution that repeats
/// to fill the word (`STCH_REPEATING`).
pub(super) const STCH_REPEATING: u8 = 9;

/// Gives each glyph of an Arabic shaper segment its action, before
/// the segment's first substitution. `glyphs` and `actions` are one to
/// one.
pub(super) fn stash(glyphs: &mut [Glyph], actions: &[Action]) {
    for (g, a) in glyphs.iter_mut().zip(actions) {
        g.indic_position = a.code();
    }
}

/// HarfBuzz's `record_stch`, after `stch` applied: each glyph a
/// multiple substitution produced becomes a stretch tile, repeating
/// when it is an odd component and fixed otherwise. Returns whether it
/// recorded any.
pub(super) fn record_stch(glyphs: &mut [Glyph]) -> bool {
    let mut any = false;
    for g in glyphs.iter_mut().filter(|g| super::lig::is_multiplied(g)) {
        g.indic_position = if super::lig::lig_comp(g) % 2 == 1 {
            STCH_REPEATING
        } else {
            STCH_FIXED
        };
        any = true;
    }
    any
}

/// Drops the joining actions once the joining features ran, keeping
/// the stretch tiles for [`super::stch`] (HarfBuzz deallocates the
/// variable there, and `apply_stch` reads the tiles after positioning).
pub(super) fn clear_actions(glyphs: &mut [Glyph]) {
    for g in glyphs {
        if !matches!(g.indic_position, STCH_FIXED | STCH_REPEATING) {
            g.indic_position = 0;
        }
    }
}

/// A state table entry: the action for the previous character, the
/// action for this one, and the next state.
#[derive(Clone, Copy)]
struct Entry {
    prev: Action,
    curr: Action,
    next: u8,
}

const fn e(prev: Action, curr: Action, next: u8) -> Entry {
    Entry { prev, curr, next }
}

use Action::{Fin2, Fin3, Fina, Init, Isol, Med2, Medi, None as No};

/// HarfBuzz's `arabic_state_table`. Columns: U, L, R, D (and C),
/// ALAPH, DALATH RISH.
const TABLE: [[Entry; 6]; 7] = [
    // 0: the previous character is U, not willing to join.
    [
        e(No, No, 0),
        e(No, Isol, 2),
        e(No, Isol, 1),
        e(No, Isol, 2),
        e(No, Isol, 1),
        e(No, Isol, 6),
    ],
    // 1: the previous character is R, or an isolated ALAPH, not
    // willing to join.
    [
        e(No, No, 0),
        e(No, Isol, 2),
        e(No, Isol, 1),
        e(No, Isol, 2),
        e(No, Fin2, 5),
        e(No, Isol, 6),
    ],
    // 2: the previous character is D or L in its isolated form,
    // willing to join.
    [
        e(No, No, 0),
        e(No, Isol, 2),
        e(Init, Fina, 1),
        e(Init, Fina, 3),
        e(Init, Fina, 4),
        e(Init, Fina, 6),
    ],
    // 3: the previous character is D in its final form, willing to
    // join.
    [
        e(No, No, 0),
        e(No, Isol, 2),
        e(Medi, Fina, 1),
        e(Medi, Fina, 3),
        e(Medi, Fina, 4),
        e(Medi, Fina, 6),
    ],
    // 4: the previous character is a final ALAPH, not willing to join.
    [
        e(No, No, 0),
        e(No, Isol, 2),
        e(Med2, Isol, 1),
        e(Med2, Isol, 2),
        e(Med2, Fin2, 5),
        e(Med2, Isol, 6),
    ],
    // 5: the previous character is ALAPH in its fin2 or fin3 form, not
    // willing to join.
    [
        e(No, No, 0),
        e(No, Isol, 2),
        e(Isol, Isol, 1),
        e(Isol, Isol, 2),
        e(Isol, Fin2, 5),
        e(Isol, Isol, 6),
    ],
    // 6: the previous character is DALATH or RISH, not willing to join.
    [
        e(No, No, 0),
        e(No, Isol, 2),
        e(No, Isol, 1),
        e(No, Isol, 2),
        e(No, Fin3, 5),
        e(No, Isol, 6),
    ],
];

/// The table column of `ch` (`get_joining_type`), or `None` for a
/// transparent character, which the machine passes over.
fn column(ch: char) -> Option<usize> {
    match ch as u32 {
        0x0710 => return Some(4),
        0x0715 | 0x0716 | 0x072A | 0x072F => return Some(5),
        _ => {}
    }
    match joining_type(ch) {
        JoiningType::U => Some(0),
        JoiningType::L => Some(1),
        JoiningType::R => Some(2),
        JoiningType::D | JoiningType::C => Some(3),
        JoiningType::T => None,
    }
}

/// The entry for `col` in `state`.
fn step(state: u8, col: usize) -> Entry {
    TABLE
        .get(usize::from(state))
        .and_then(|row| row.get(col))
        .copied()
        .unwrap_or(e(No, No, 0))
}

/// The column of the nearest non-transparent character of `chars`.
fn first_solid(mut chars: impl Iterator<Item = char>) -> Option<usize> {
    chars.find_map(column)
}

/// A glyph flag range the machine sets.
enum FlagRange {
    /// `safe_to_insert_tatweel(start, end)`.
    Tatweel(usize, usize),
    /// `unsafe_to_concat(start, end)`.
    Concat(usize, usize),
}

/// Runs the machine over `cps` with the buffer's `pre_context` (text
/// before the run, in logical order) and `post_context`, reporting each
/// flag range to `flag`.
fn walk(
    cps: &[char],
    pre_context: &str,
    post_context: &str,
    mut flag: impl FnMut(FlagRange),
) -> Vec<Action> {
    let mut actions = alloc::vec![Action::None; cps.len()];
    let mut state = first_solid(pre_context.chars().rev()).map_or(0, |col| step(0, col).next);
    let mut prev: Option<usize> = None;
    for (i, &ch) in cps.iter().enumerate() {
        let Some(col) = column(ch) else {
            continue;
        };
        let entry = step(state, col);
        match prev {
            Some(p) if entry.prev != Action::None => {
                if let Some(a) = actions.get_mut(p) {
                    *a = entry.prev;
                }
                flag(FlagRange::Tatweel(p, i + 1));
            }
            // This column is R or a later one, or the state is one
            // with a possible action on the previous character.
            Some(p) if col >= 2 || (2..=5).contains(&state) => flag(FlagRange::Concat(p, i + 1)),
            None if col >= 2 => flag(FlagRange::Concat(0, i + 1)),
            _ => {}
        }
        if let Some(a) = actions.get_mut(i) {
            *a = entry.curr;
        }
        prev = Some(i);
        state = entry.next;
    }
    if let (Some(col), Some(p)) = (first_solid(post_context.chars()), prev) {
        let entry = step(state, col);
        if entry.prev != Action::None {
            if let Some(a) = actions.get_mut(p) {
                *a = entry.prev;
            }
            flag(FlagRange::Tatweel(p, cps.len()));
        } else if (2..=5).contains(&state) {
            flag(FlagRange::Concat(p, cps.len()));
        }
    }
    actions
}

/// The joining action of each character of `cps`, with the buffer's
/// pre- and post-context.
pub(super) fn actions(cps: &[char], pre_context: &str, post_context: &str) -> Vec<Action> {
    walk(cps, pre_context, post_context, |_| {})
}

/// Sets the joining flags on `glyphs`, one glyph per character of
/// `cps`.
pub(super) fn set_flags(
    glyphs: &mut [Glyph],
    cps: &[char],
    pre_context: &str,
    post_context: &str,
    flags: FlagCx,
) {
    if glyphs.len() != cps.len() {
        return;
    }
    walk(cps, pre_context, post_context, |range| match range {
        FlagRange::Tatweel(start, end) => flags.safe_to_insert_tatweel(glyphs, start, end),
        FlagRange::Concat(start, end) => flags.unsafe_to_concat(glyphs, start, end),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{BufferFlags, ClusterLevel};

    const BEH: char = '\u{0628}';
    const ALEF: char = '\u{0627}';
    const FATHA: char = '\u{064E}';
    const HAMZA: char = '\u{0621}';
    const ALAPH: char = '\u{0710}';
    const BETH: char = '\u{0712}';
    const DALATH: char = '\u{0715}';
    const WAW: char = '\u{0718}';

    fn flags_of(cps: &[char], pre: &str, post: &str, buffer: BufferFlags) -> Vec<u32> {
        let mut glyphs: Vec<Glyph> = (0..cps.len() as u32).map(|i| Glyph::new(1, i)).collect();
        let cx = FlagCx::new(ClusterLevel::MonotoneCharacters, buffer);
        set_flags(&mut glyphs, cps, pre, post, cx);
        glyphs.iter().map(|g| g.flags.bits()).collect()
    }

    #[test]
    fn arabic_letters_take_isol_init_medi_fina() {
        assert_eq!(actions(&[BEH, BEH, BEH], "", ""), [Init, Medi, Fina]);
        assert_eq!(actions(&[BEH, FATHA, BEH], "", ""), [Init, No, Fina]);
        assert_eq!(actions(&[BEH, ALEF, BEH], "", ""), [Init, Fina, Isol]);
        assert_eq!(actions(&[HAMZA, BEH], "", ""), [No, Isol]);
        // The context joins the letters at the ends.
        assert_eq!(actions(&[BEH], "\u{0628}", "\u{0627}"), [Medi]);
    }

    #[test]
    fn syriac_alaph_takes_fin2_fin3_and_med2() {
        // HarfBuzz's state table: an ALAPH after a letter that does not
        // join it is fin2, or fin3 after DALATH or RISH.
        assert_eq!(actions(&[WAW, ALAPH], "", ""), [Isol, Fin2]);
        assert_eq!(actions(&[DALATH, ALAPH], "", ""), [Isol, Fin3]);
        assert_eq!(actions(&[ALAPH], "", ""), [Isol]);
        // An ALAPH joined to a beth is fina. A letter after it turns it
        // into med2, and an ALAPH in fin2 back into isol.
        assert_eq!(actions(&[BETH, ALAPH], "", ""), [Init, Fina]);
        assert_eq!(actions(&[BETH, ALAPH, BETH], "", ""), [Init, Med2, Isol]);
        assert_eq!(actions(&[ALAPH, ALAPH], "", ""), [Isol, Fin2]);
        assert_eq!(actions(&[WAW, ALAPH, BETH], "", ""), [Isol, Isol, Isol]);
    }

    #[test]
    fn joined_letters_are_unsafe_to_break_between_them() {
        let none = BufferFlags::DEFAULT;
        assert_eq!(flags_of(&[BEH, BEH], "", "", none), [0, 3]);
        assert_eq!(flags_of(&[BEH, FATHA, BEH], "", "", none), [0, 3, 3]);
        assert_eq!(flags_of(&[ALEF, BEH], "", "", none), [0, 0]);
    }

    #[test]
    fn concat_and_tatweel_flags_follow_the_buffer_flags() {
        let concat = BufferFlags::PRODUCE_UNSAFE_TO_CONCAT;
        // beh, then a non-joining letter: a joining letter could have
        // followed, so the pair is unsafe to concatenate.
        assert_eq!(flags_of(&[BEH, HAMZA], "", "", concat), [2, 2]);
        // A letter that joins backward at the start of the run.
        assert_eq!(flags_of(&[ALEF], "", "", concat), [2]);
        let tatweel = BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL;
        assert_eq!(flags_of(&[BEH, BEH], "", "", tatweel), [0, 4]);
        // The post-context can still change the last letter's form.
        let none = BufferFlags::DEFAULT;
        assert_eq!(flags_of(&[BEH], "", "\u{0628}", none), [0]);
        assert_eq!(flags_of(&[BEH, BEH], "", "\u{0628}", none), [0, 3]);
    }
}
