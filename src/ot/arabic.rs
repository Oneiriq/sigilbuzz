//! Arabic cursive-joining state machine.
//!
//! Given a sequence of codepoints in logical (memory) order, this
//! module computes the joining-form feature tag for each one:
//!
//! - `isol`: stands alone, no joining on either side
//! - `init`: initial (joins on the trailing side)
//! - `medi`: medial (joins on both sides)
//! - `fina`: final (joins on the leading side)
//!
//! The decision per position depends on the [`JoiningType`] of its
//! two nearest *non-transparent* neighbors. Transparent codepoints
//! (harakat, combining marks) are threaded through unchanged. They
//! inherit no feature of their own, and they do not influence the
//! shape of the letters around them.
//!
//! # The four shape slots on an OpenType font
//!
//! Standard Arabic fonts carry up to four glyphs per dual-joining
//! letter: isolated, initial, medial, final. The OpenType way to
//! select between them is to tag each input position with exactly
//! one of the `isol` / `init` / `medi` / `fina` features, and let
//! the GSUB dispatcher swap the glyph id through the font's
//! lookup for that feature. sigilbuzz's [`crate::shape`] dispatcher
//! already knows how to apply a GSUB feature across a subset of the
//! run; this module tells it *which* positions each feature covers.
//!
//! # Spec, in 30 seconds
//!
//! Given the [`JoiningType`] of the previous non-transparent letter
//! (`prev`) and the next non-transparent letter (`next`), the
//! feature for the current position is:
//!
//! ```text
//!   current is R  or U   ->  isol            if prev is not D/C/L
//!                        ->  fina            otherwise
//!   current is D  or C   ->  isol            if prev is not D/C/L and next is not D/C/R
//!                        ->  init            if prev is not D/C/L and next is     D/C/R
//!                        ->  fina            if prev is     D/C/L and next is not D/C/R
//!                        ->  medi            if prev is     D/C/L and next is     D/C/R
//!   current is L         ->  (mirror; no Unicode 15.1 characters hit this)
//!   current is T         ->  transparent: caller carries the tag through
//! ```
//!
//! The state machine below is that table, flattened.

use alloc::vec::Vec;

use crate::unicode::joining::{joining_type, JoiningType};

/// Which OpenType feature the Arabic joining pass should apply at a
/// given position. `None` means the position did not match any of
/// the cursive forms and should not be touched by
/// `init`/`medi`/`fina`/`isol`. Applies to non-Arabic spacers in the
/// run and to transparent marks that inherit from their base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoiningForm {
    /// Isolated form: `isol` feature.
    Isol,
    /// Initial form (joins on trailing side): `init` feature.
    Init,
    /// Medial form (joins on both sides): `medi` feature.
    Medi,
    /// Final form (joins on leading side): `fina` feature.
    Fina,
    /// No feature selected: transparent mark, or a position with a
    /// joining type the state machine leaves alone.
    None,
}

impl JoiningForm {
    /// The OpenType feature tag associated with this form, if any.
    #[must_use]
    pub const fn feature_tag(self) -> Option<[u8; 4]> {
        match self {
            Self::Isol => Some(*b"isol"),
            Self::Init => Some(*b"init"),
            Self::Medi => Some(*b"medi"),
            Self::Fina => Some(*b"fina"),
            Self::None => None,
        }
    }
}

/// Assigns a [`JoiningForm`] to every code point in `text`, in
/// logical (memory) order. The returned vector has one entry per
/// `char` in `text`, matching the iteration order of `text.chars()`.
///
/// The caller is expected to have already split runs by script so
/// that the input here is (mostly) Arabic; a non-Arabic codepoint
/// is treated as a boundary: it carries [`JoiningForm::None`] and
/// forces the neighboring Arabic letter into its final or isolated
/// form, which matches the way OpenType shapers handle mixed runs.
#[must_use]
pub fn assign_joining_forms(text: &str) -> Vec<JoiningForm> {
    let chars: Vec<char> = text.chars().collect();
    let types: Vec<JoiningType> = chars.iter().map(|&c| joining_type(c)).collect();
    assign_from_types(&types)
}

/// Core of [`assign_joining_forms`]. Split out so the state machine
/// can be tested against a synthetic joining-type sequence without
/// relying on the lookup table.
#[must_use]
pub fn assign_from_types(types: &[JoiningType]) -> Vec<JoiningForm> {
    let mut out = Vec::with_capacity(types.len());
    for i in 0..types.len() {
        out.push(form_at(types, i));
    }
    out
}

/// Computes the joining form at position `i`. Transparent positions
/// return [`JoiningForm::None`]: the caller's glyph at that cluster
/// is a combining mark that rides along with its base.
fn form_at(types: &[JoiningType], i: usize) -> JoiningForm {
    let current = types[i];
    match current {
        JoiningType::T => JoiningForm::None,
        JoiningType::U => {
            // Non-joining letter: always isolated, regardless of
            // neighbors. Hamza is the canonical example.
            JoiningForm::Isol
        }
        JoiningType::R => {
            // Right-joining letters (alef, waw, reh ...) connect to
            // the *preceding* letter only. So the only question is
            // whether `prev` is a joiner on the right side: if yes,
            // this letter takes its final form; otherwise isolated.
            if prev_joins_toward_us(types, i) {
                JoiningForm::Fina
            } else {
                JoiningForm::Isol
            }
        }
        JoiningType::D | JoiningType::C => {
            // Dual / join-causing connect on both sides. Four-way
            // decision based on both neighbors.
            let prev_joins = prev_joins_toward_us(types, i);
            let next_joins = next_joins_toward_us(types, i);
            match (prev_joins, next_joins) {
                (false, false) => JoiningForm::Isol,
                (false, true) => JoiningForm::Init,
                (true, false) => JoiningForm::Fina,
                (true, true) => JoiningForm::Medi,
            }
        }
        JoiningType::L => {
            // Mirror of R: connects only on the trailing side. No
            // Unicode 15.1 characters map here, but the state machine
            // stays symmetric so the table is future-proof.
            if next_joins_toward_us(types, i) {
                JoiningForm::Init
            } else {
                JoiningForm::Isol
            }
        }
    }
}

/// True when the nearest non-transparent code point *before* `i`
/// connects on *its* trailing side, i.e. it is dual-joining
/// (`D`), join-causing (`C`), or left-joining (`L`). That is the
/// full set of types that draw a connector into the letter at
/// position `i`.
fn prev_joins_toward_us(types: &[JoiningType], i: usize) -> bool {
    let mut j = i;
    while j > 0 {
        j -= 1;
        match types[j] {
            JoiningType::T => {}
            JoiningType::D | JoiningType::C | JoiningType::L => return true,
            JoiningType::R | JoiningType::U => return false,
        }
    }
    false
}

/// True when the nearest non-transparent code point *after* `i`
/// connects on *its* leading side, i.e. it is dual-joining (`D`),
/// join-causing (`C`), or right-joining (`R`).
fn next_joins_toward_us(types: &[JoiningType], i: usize) -> bool {
    let mut j = i + 1;
    while j < types.len() {
        match types[j] {
            JoiningType::T => j += 1,
            JoiningType::D | JoiningType::C | JoiningType::R => return true,
            JoiningType::L | JoiningType::U => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Handy helper: map the literal to its joining forms as a
    /// `Vec<JoiningForm>`.
    fn forms(text: &str) -> Vec<JoiningForm> {
        assign_joining_forms(text)
    }

    #[test]
    fn single_alef_is_isolated() {
        // U+0627 ALEF alone has no neighbor to join with: isol.
        assert_eq!(forms("\u{0627}"), alloc::vec![JoiningForm::Isol]);
    }

    #[test]
    fn single_beh_is_isolated() {
        // U+0628 BEH is dual-joining but stands alone.
        assert_eq!(forms("\u{0628}"), alloc::vec![JoiningForm::Isol]);
    }

    #[test]
    fn beh_beh_pair_splits_into_init_and_fina() {
        // "BB": first beh connects forward, second beh connects
        // backward. Forward: init + fina.
        let got = forms("\u{0628}\u{0628}");
        assert_eq!(got, alloc::vec![JoiningForm::Init, JoiningForm::Fina]);
    }

    #[test]
    fn three_beh_run_is_init_medi_fina() {
        let got = forms("\u{0628}\u{0628}\u{0628}");
        assert_eq!(
            got,
            alloc::vec![JoiningForm::Init, JoiningForm::Medi, JoiningForm::Fina]
        );
    }

    #[test]
    fn alef_breaks_medial_run_on_its_trailing_side() {
        // "BAB": alef is Right-joining. It joins to the preceding
        // beh (so alef is `fina`), but does not feed the trailing
        // beh (so the trailing beh loses its medial form and falls
        // back to isolated).
        let got = forms("\u{0628}\u{0627}\u{0628}");
        assert_eq!(
            got,
            alloc::vec![JoiningForm::Init, JoiningForm::Fina, JoiningForm::Isol]
        );
    }

    #[test]
    fn harakat_between_letters_are_transparent() {
        // Beh + fatha + beh. The vowel mark does not influence the
        // cursive chain; both behs still see each other and the
        // result is init + none + fina.
        let got = forms("\u{0628}\u{064E}\u{0628}");
        assert_eq!(
            got,
            alloc::vec![JoiningForm::Init, JoiningForm::None, JoiningForm::Fina]
        );
    }

    #[test]
    fn tatweel_acts_as_join_causing_bridge() {
        // Beh + tatweel + beh. Tatweel (C) propagates joining
        // through itself without changing shape, so the first beh
        // is init, the tatweel is medi (C is treated like D), and
        // the second beh is fina.
        let got = forms("\u{0628}\u{0640}\u{0628}");
        assert_eq!(
            got,
            alloc::vec![JoiningForm::Init, JoiningForm::Medi, JoiningForm::Fina]
        );
    }

    #[test]
    fn zwj_forces_joining_at_boundary() {
        // ZWJ (Join-causing) after a beh forces that beh to render
        // in `init` even though nothing follows visually. The ZWJ
        // itself is join-causing; with a joiner on its left side
        // and nothing on the right it takes `fina`.
        let got = forms("\u{0628}\u{200D}");
        assert_eq!(got, alloc::vec![JoiningForm::Init, JoiningForm::Fina]);
    }

    #[test]
    fn zwj_before_letter_forces_final_form() {
        // ZWJ + beh: caller wants the beh in its final form even
        // though it is at the start of the input. ZWJ is Join-causing
        // and carries no visual glyph of its own, but it tells the
        // state machine "pretend there is a joiner to my left."
        let got = forms("\u{200D}\u{0628}");
        assert_eq!(got, alloc::vec![JoiningForm::Init, JoiningForm::Fina]);
    }

    #[test]
    fn zwnj_breaks_joining_at_boundary() {
        // ZWNJ is Non_joining (U): breaks the chain. "BeB" with
        // ZWNJ in the middle: first beh is isol (trailing ZWNJ breaks
        // join), ZWNJ itself is isol from the state machine (carried
        // through as None-equivalent isol), second beh is isol too.
        let got = forms("\u{0628}\u{200C}\u{0628}");
        assert_eq!(
            got,
            alloc::vec![JoiningForm::Isol, JoiningForm::Isol, JoiningForm::Isol]
        );
    }

    #[test]
    fn space_between_letters_breaks_joining() {
        // Mixing non-Arabic into an Arabic run breaks cursive joining.
        let got = forms("\u{0628} \u{0628}");
        assert_eq!(
            got,
            alloc::vec![JoiningForm::Isol, JoiningForm::Isol, JoiningForm::Isol]
        );
    }

    #[test]
    fn word_salam_shapes_correctly() {
        // س ل ا م: seen, lam, alef, meem
        // seen D, lam D, alef R, meem D.
        // seen sees lam after -> init.
        // lam sees seen before and alef after -> medi.
        // alef (R) sees lam before -> fina; does not feed meem.
        // meem sees nothing before it that joins toward meem (alef is R) -> isol.
        let got = forms("\u{0633}\u{0644}\u{0627}\u{0645}");
        assert_eq!(
            got,
            alloc::vec![
                JoiningForm::Init,
                JoiningForm::Medi,
                JoiningForm::Fina,
                JoiningForm::Isol,
            ]
        );
    }

    #[test]
    fn word_marhaba_shapes_correctly() {
        // مرحبا: meem, reh, hah, beh, alef
        // meem D, reh R, hah D, beh D, alef R.
        // meem: next reh is R (joins on its leading side) -> init.
        // reh (R): prev meem is D (joins on trailing side) -> fina.
        //   reh is R so it does not feed forward; hah sees nothing
        //   joining toward it from the left -> isol (not init).
        // hah: prev reh is R (no), next beh is D (yes) -> init.
        // beh: prev hah D (yes), next alef R (yes) -> medi.
        // alef (R): prev beh D (yes) -> fina.
        let got = forms("\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}");
        assert_eq!(
            got,
            alloc::vec![
                JoiningForm::Init,
                JoiningForm::Fina,
                JoiningForm::Init,
                JoiningForm::Medi,
                JoiningForm::Fina,
            ]
        );
    }

    #[test]
    fn feature_tag_mapping() {
        assert_eq!(JoiningForm::Isol.feature_tag(), Some(*b"isol"));
        assert_eq!(JoiningForm::Init.feature_tag(), Some(*b"init"));
        assert_eq!(JoiningForm::Medi.feature_tag(), Some(*b"medi"));
        assert_eq!(JoiningForm::Fina.feature_tag(), Some(*b"fina"));
        assert_eq!(JoiningForm::None.feature_tag(), None);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(forms("").is_empty());
    }

    #[test]
    fn non_arabic_run_is_all_isol() {
        // A run of non-joining types resolves to isol for every
        // position (non-arabic spaces + letters behave the same way).
        let got = forms("abc");
        assert_eq!(
            got,
            alloc::vec![JoiningForm::Isol, JoiningForm::Isol, JoiningForm::Isol]
        );
    }

    #[test]
    fn transparent_chain_between_joiners_still_joins() {
        // Beh + fatha + shadda + beh. Multiple transparent marks in a
        // row must not break joining.
        let got = forms("\u{0628}\u{064E}\u{0651}\u{0628}");
        assert_eq!(
            got,
            alloc::vec![
                JoiningForm::Init,
                JoiningForm::None,
                JoiningForm::None,
                JoiningForm::Fina,
            ]
        );
    }
}
