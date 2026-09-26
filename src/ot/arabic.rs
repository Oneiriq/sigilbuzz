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

/// The joining types just outside a run: the nearest non-transparent
/// character before it and after it. HarfBuzz's `arabic_joining`
/// reads these from the buffer's pre- and post-context so a run that
/// starts or ends mid-word still gets connected forms. `None` on a
/// side means nothing is known there, which joins like a non-joining
/// neighbor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JoiningContext {
    /// Joining type of the nearest non-transparent character before
    /// the run.
    pub before: Option<JoiningType>,
    /// Joining type of the nearest non-transparent character after
    /// the run.
    pub after: Option<JoiningType>,
}

impl JoiningContext {
    /// No context on either side.
    pub const NONE: Self = Self {
        before: None,
        after: None,
    };

    /// Resolves the context from the characters around a run.
    /// `before` yields the characters preceding the run nearest first
    /// (that is, in reverse order), `after` the characters following
    /// it in order. Transparent characters are skipped.
    pub fn from_chars(
        before: impl IntoIterator<Item = char>,
        after: impl IntoIterator<Item = char>,
    ) -> Self {
        let first_solid = |chars: &mut dyn Iterator<Item = char>| {
            chars.map(joining_type).find(|t| *t != JoiningType::T)
        };
        Self {
            before: first_solid(&mut before.into_iter()),
            after: first_solid(&mut after.into_iter()),
        }
    }

    /// Resolves the context from a buffer's pre-context (text before
    /// the run, in logical order) and post-context (text after it).
    #[must_use]
    pub fn from_context(pre_context: &str, post_context: &str) -> Self {
        Self::from_chars(pre_context.chars().rev(), post_context.chars())
    }

    /// Resolves the context of `codepoints[range]`: its neighbors
    /// inside `codepoints` first, then the buffer's pre- and
    /// post-context beyond the ends of `codepoints`.
    #[must_use]
    pub fn around(
        codepoints: &[char],
        range: core::ops::Range<usize>,
        pre_context: &str,
        post_context: &str,
    ) -> Self {
        let before = codepoints
            .get(..range.start)
            .unwrap_or(&[])
            .iter()
            .rev()
            .copied()
            .chain(pre_context.chars().rev());
        let after = codepoints
            .get(range.end..)
            .unwrap_or(&[])
            .iter()
            .copied()
            .chain(post_context.chars());
        Self::from_chars(before, after)
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
    assign_joining_forms_in_context(text, JoiningContext::NONE)
}

/// [`assign_joining_forms`] for a run with known surroundings: the
/// first and last letters join toward `context` the way they would
/// join toward a neighbor inside the run.
///
/// # Examples
///
/// ```
/// use sigilbuzz::ot::arabic::{assign_joining_forms_in_context, JoiningContext};
/// use sigilbuzz::JoiningForm;
///
/// // A beh preceded by a beh in the source text takes its final form.
/// let context = JoiningContext::from_context("\u{0628}", "");
/// assert_eq!(assign_joining_forms_in_context("\u{0628}", context), [JoiningForm::Fina]);
/// assert_eq!(
///     assign_joining_forms_in_context("\u{0628}", JoiningContext::NONE),
///     [JoiningForm::Isol]
/// );
/// ```
#[must_use]
pub fn assign_joining_forms_in_context(text: &str, context: JoiningContext) -> Vec<JoiningForm> {
    let types: Vec<JoiningType> = text.chars().map(joining_type).collect();
    assign_from_types_in_context(&types, context)
}

/// Core of [`assign_joining_forms`]. Split out so the state machine
/// can be tested against a synthetic joining-type sequence without
/// relying on the lookup table.
#[must_use]
pub fn assign_from_types(types: &[JoiningType]) -> Vec<JoiningForm> {
    assign_from_types_in_context(types, JoiningContext::NONE)
}

/// [`assign_from_types`] with the joining types just outside the run.
#[must_use]
pub fn assign_from_types_in_context(
    types: &[JoiningType],
    context: JoiningContext,
) -> Vec<JoiningForm> {
    (0..types.len())
        .map(|i| form_at(types, i, context))
        .collect()
}

/// Computes the joining form at position `i`. Transparent positions
/// return [`JoiningForm::None`]: the caller's glyph at that cluster
/// is a combining mark that rides along with its base.
fn form_at(types: &[JoiningType], i: usize, context: JoiningContext) -> JoiningForm {
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
            if prev_joins_toward_us(types, i, context.before) {
                JoiningForm::Fina
            } else {
                JoiningForm::Isol
            }
        }
        JoiningType::D | JoiningType::C => {
            // Dual / join-causing connect on both sides. Four-way
            // decision based on both neighbors.
            let prev_joins = prev_joins_toward_us(types, i, context.before);
            let next_joins = next_joins_toward_us(types, i, context.after);
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
            if next_joins_toward_us(types, i, context.after) {
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
/// position `i`. Past the start of the run, `before` (the
/// pre-context) answers instead.
fn prev_joins_toward_us(types: &[JoiningType], i: usize, before: Option<JoiningType>) -> bool {
    let joins = |t: JoiningType| matches!(t, JoiningType::D | JoiningType::C | JoiningType::L);
    types[..i]
        .iter()
        .rev()
        .copied()
        .chain(before)
        .find(|t| *t != JoiningType::T)
        .is_some_and(joins)
}

/// True when the nearest non-transparent code point *after* `i`
/// connects on *its* leading side, i.e. it is dual-joining (`D`),
/// join-causing (`C`), or right-joining (`R`). Past the end of the
/// run, `after` (the post-context) answers instead.
fn next_joins_toward_us(types: &[JoiningType], i: usize, after: Option<JoiningType>) -> bool {
    let joins = |t: JoiningType| matches!(t, JoiningType::D | JoiningType::C | JoiningType::R);
    types[i + 1..]
        .iter()
        .copied()
        .chain(after)
        .find(|t| *t != JoiningType::T)
        .is_some_and(joins)
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

    fn in_context(pre: &str, text: &str, post: &str) -> Vec<JoiningForm> {
        assign_joining_forms_in_context(text, JoiningContext::from_context(pre, post))
    }

    #[test]
    fn pre_context_joiner_makes_first_letter_final() {
        use JoiningForm::{Fina, Isol, Medi};
        assert_eq!(in_context("\u{0628}", "\u{0628}", ""), alloc::vec![Fina]);
        assert_eq!(in_context("\u{0628}", "\u{0627}", ""), alloc::vec![Fina]);
        assert_eq!(
            in_context("\u{0628}", "\u{0628}", "\u{0628}"),
            alloc::vec![Medi]
        );
        // Alef joins only backward, so it does not feed the run.
        assert_eq!(in_context("\u{0627}", "\u{0628}", ""), alloc::vec![Isol]);
        // A non-joining neighbor behaves like no context.
        assert_eq!(in_context("a", "\u{0628}", "b"), alloc::vec![Isol]);
    }

    #[test]
    fn post_context_joiner_makes_last_letter_initial() {
        use JoiningForm::{Fina, Init, Isol};
        assert_eq!(in_context("", "\u{0628}", "\u{0627}"), alloc::vec![Init]);
        assert_eq!(
            in_context("", "\u{0628}\u{0628}", "\u{0628}"),
            alloc::vec![Init, JoiningForm::Medi]
        );
        // Right-joining letters never look forward.
        assert_eq!(
            in_context("\u{0628}", "\u{0627}", "\u{0628}"),
            alloc::vec![Fina]
        );
        assert_eq!(in_context("", "\u{0627}", "\u{0628}"), alloc::vec![Isol]);
    }

    #[test]
    fn context_skips_transparent_marks() {
        use JoiningForm::{Fina, Init};
        // Beh + fatha before the run, fatha + beh after it.
        assert_eq!(
            in_context("\u{0628}\u{064E}", "\u{0628}", ""),
            alloc::vec![Fina]
        );
        assert_eq!(
            in_context("", "\u{0628}", "\u{064E}\u{0628}"),
            alloc::vec![Init]
        );
        // A transparent-only context is no context.
        assert_eq!(
            JoiningContext::from_context("\u{064E}\u{0651}", "\u{064E}"),
            JoiningContext::NONE
        );
    }

    #[test]
    fn context_only_affects_the_run_edges() {
        use JoiningForm::{Fina, Init, Isol};
        // "b a b": the alef resets the chain, so the context only
        // reaches the outer letters.
        let got = in_context("\u{0628}", "\u{0628}\u{0627}\u{0628}", "\u{0628}");
        assert_eq!(got, alloc::vec![JoiningForm::Medi, Fina, Init]);
        let got = in_context("", "\u{0628}\u{0627}\u{0628}", "");
        assert_eq!(got, alloc::vec![Init, Fina, Isol]);
    }

    #[test]
    fn around_reads_neighbors_before_buffer_context() {
        let cps: Vec<char> = "a\u{0628}\u{0628}c".chars().collect();
        // The run is the two behs; its neighbors inside the text are
        // non-joining, so the buffer context is never consulted.
        let ctx = JoiningContext::around(&cps, 1..3, "\u{0628}", "\u{0628}");
        assert_eq!(ctx.before, Some(JoiningType::U));
        assert_eq!(ctx.after, Some(JoiningType::U));
        // A run at the edges falls through to the buffer context.
        let ctx = JoiningContext::around(&cps[1..3], 0..2, "\u{0628}", "\u{0627}");
        assert_eq!(ctx.before, Some(JoiningType::D));
        assert_eq!(ctx.after, Some(JoiningType::R));
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
