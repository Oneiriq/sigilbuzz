//! Mongolian joining forms, and the Mongolian entry points.
//!
//! Mongolian (U+1800..U+18AF) is cursive: every letter has up to
//! four positional forms (isolated, initial, medial, final) selected
//! the same way Arabic does. HarfBuzz shapes it with the Universal
//! Shaping Engine ([`crate::ot::use_shaper`]), whose `isol`, `init`,
//! `medi`, and `fina` follow those joining forms
//! (`setup_masks_arabic_plan`). The state machine that picks the forms
//! is shared verbatim with Arabic via [`crate::ot::arabic`]. Around
//! it:
//!
//! - **Free Variation Selectors** (FVS1 = U+180B, FVS2 = U+180C,
//!   FVS3 = U+180D, FVS4 = U+180F) override the default form
//!   choice. The [joining table](crate::unicode::joining) marks them
//!   `Transparent` so the state machine threads them through, and
//!   each then takes the form of the character before it
//!   (HarfBuzz's `mongolian_variation_selectors`). That way the GSUB
//!   lookup driving the variant (e.g. `init` lookups targeting
//!   `letter + FVS1`) sees the FVS as part of the initial-form
//!   cluster.
//! - **Word break via NNBSP** (U+202F NARROW NO-BREAK SPACE) marks
//!   the boundary between two visually-joined Mongolian "words".
//!   sigilbuzz's segmenter already breaks the run on NNBSP because
//!   it falls into the COMMON segmentation bucket. The joining
//!   state machine sees the resulting Mongolian segment in
//!   isolation, which gives the right "the last letter of the
//!   first word is final, the first letter of the second word is
//!   initial" behavior.
//! - **Vertical default**. Mongolian is written top-to-bottom by
//!   default. While the caller has not chosen a direction, the
//!   dispatcher in [`crate::shape`] lays a dominantly Mongolian run
//!   out top to bottom: vertical metrics and the `vert` GSUB
//!   feature. Consumers who want horizontal Mongolian set a
//!   direction explicitly with
//!   [`crate::buffer::Buffer::set_direction`]: LTR keeps logical
//!   order, RTL returns the run reversed like any RTL run.

use alloc::vec::Vec;

use crate::buffer::{ClusterLevel, Glyph};
use crate::ot::arabic::{assign_from_types_in_context, JoiningContext, JoiningForm};
use crate::ot::use_shaper::{shape_script, UseScript};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::joining::{joining_type, JoiningType};

/// Script-tag priority for Mongolian GSUB / GPOS feature lookup.
///
/// Mongolian fonts register their lookups under `mong`. DFLT is the
/// universal fallback for fonts that only carry features in the
/// default LangSys.
pub const MONG_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"mong", *b"DFLT"];

/// Features the Mongolian shaper runs before the positional pass.
/// Always empty: the Universal Shaping Engine runs every feature.
pub const MONG_FEATURES_PRE: &[&[u8; 4]] = &[];

/// True for every codepoint that is part of the Mongolian block.
#[must_use]
pub const fn is_mongolian(ch: char) -> bool {
    matches!(ch as u32, 0x1800..=0x18AF)
}

/// True for the Mongolian Free Variation Selectors (FVS1..FVS4).
/// FVS1..FVS3 are present from Unicode 1.0. FVS4 was added in 14.0
/// and lives at U+180F (just before the Mongolian Vowel Separator).
#[must_use]
pub const fn is_mongolian_fvs(ch: char) -> bool {
    matches!(ch as u32, 0x180B..=0x180D | 0x180F)
}

/// Computes a per-codepoint joining-form vector for a Mongolian run.
///
/// The Arabic state machine handles the heavy lifting (Mongolian
/// shares the same joining types) and this wrapper layers FVS
/// inheritance on top: a Free Variation Selector is Transparent in
/// the state machine (so it does not break the cursive chain) and
/// then takes on the *form* of the character to its left. That way
/// the positional GSUB pass treats the letter+FVS pair as a two-glyph
/// cluster of the same form, which is exactly what Mongolian fonts
/// target.
#[must_use]
pub fn assign_mongolian_forms(codepoints: &[char]) -> Vec<JoiningForm> {
    assign_mongolian_forms_in_context(codepoints, JoiningContext::NONE)
}

/// [`assign_mongolian_forms`] for a run with known surroundings (the
/// buffer's pre- and post-context), so a run that starts or ends
/// mid-word keeps its connected forms.
#[must_use]
pub fn assign_mongolian_forms_in_context(
    codepoints: &[char],
    context: JoiningContext,
) -> Vec<JoiningForm> {
    let types: Vec<JoiningType> = codepoints.iter().map(|&c| joining_type(c)).collect();
    let mut forms = assign_from_types_in_context(&types, context);
    // HarfBuzz's `mongolian_variation_selectors`: each FVS copies the
    // form of the character before it.
    for i in 1..forms.len().min(codepoints.len()) {
        if is_mongolian_fvs(codepoints[i]) {
            forms[i] = forms[i - 1];
        }
    }
    forms
}

/// Entry point: shapes one Mongolian run with the Universal Shaping
/// Engine, as HarfBuzz does, every GSUB feature included.
///
/// `codepoints` and `glyphs` start 1:1 (a glyph per codepoint, post
/// cmap). After the call `glyphs` may have shrunk through ligature
/// collapse. Clusters merge at the monotone characters level.
pub fn shape_mongolian(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_mongolian_in_context(gsub, gdef, codepoints, glyphs, JoiningContext::NONE);
}

/// [`shape_mongolian`] for a run whose surroundings are known: the
/// first and last letters join toward `context` (see
/// [`JoiningContext::around`]).
pub fn shape_mongolian_in_context(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    context: JoiningContext,
) {
    let forms = assign_mongolian_forms_in_context(codepoints, context);
    let script = UseScript {
        script_priority: MONG_SCRIPT_PRIORITY,
        joining: Some(&forms),
    };
    shape_script(
        gsub,
        gdef,
        codepoints,
        glyphs,
        script,
        ClusterLevel::MonotoneCharacters,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_mongolian_covers_block() {
        assert!(is_mongolian('\u{1800}'));
        assert!(is_mongolian('\u{1820}')); // letter A
        assert!(is_mongolian('\u{1844}')); // letter MA
        assert!(is_mongolian('\u{18AA}')); // last letter
        assert!(is_mongolian('\u{18AF}')); // block end
        assert!(!is_mongolian('\u{17FF}'));
        assert!(!is_mongolian('\u{18B0}'));
        assert!(!is_mongolian('A'));
    }

    #[test]
    fn fvs_predicate_matches_all_four_selectors() {
        assert!(is_mongolian_fvs('\u{180B}')); // FVS1
        assert!(is_mongolian_fvs('\u{180C}')); // FVS2
        assert!(is_mongolian_fvs('\u{180D}')); // FVS3
        assert!(is_mongolian_fvs('\u{180F}')); // FVS4 (U14.0)
                                               // Vowel separator (U+180E) is NOT an FVS.
        assert!(!is_mongolian_fvs('\u{180E}'));
        assert!(!is_mongolian_fvs('\u{1820}'));
    }

    #[test]
    fn single_letter_is_isolated() {
        // U+1820 MONGOLIAN LETTER A: alone is `isol`.
        let cps: Vec<char> = "\u{1820}".chars().collect();
        assert_eq!(assign_mongolian_forms(&cps), alloc::vec![JoiningForm::Isol]);
    }

    #[test]
    fn two_letters_split_init_fina() {
        // U+1820 A + U+1821 E: both Dual; the pair shapes init+fina.
        let cps: Vec<char> = "\u{1820}\u{1821}".chars().collect();
        assert_eq!(
            assign_mongolian_forms(&cps),
            alloc::vec![JoiningForm::Init, JoiningForm::Fina]
        );
    }

    #[test]
    fn three_letter_word_is_init_medi_fina() {
        // A + E + I (all Dual): classic init/medi/fina chain.
        let cps: Vec<char> = "\u{1820}\u{1821}\u{1822}".chars().collect();
        assert_eq!(
            assign_mongolian_forms(&cps),
            alloc::vec![JoiningForm::Init, JoiningForm::Medi, JoiningForm::Fina]
        );
    }

    #[test]
    fn fvs_inherits_previous_letter_form() {
        // A (init) + FVS1: the FVS should also carry Init so the
        // `init` lookup gated on `letter + FVS` triggers.
        let cps: Vec<char> = "\u{1820}\u{180B}\u{1821}".chars().collect();
        let forms = assign_mongolian_forms(&cps);
        assert_eq!(
            forms,
            alloc::vec![JoiningForm::Init, JoiningForm::Init, JoiningForm::Fina]
        );
    }

    #[test]
    fn fvs_at_word_end_inherits_final_form() {
        // A + E + FVS2: E is `fina`; the trailing FVS inherits Fina.
        let cps: Vec<char> = "\u{1820}\u{1821}\u{180C}".chars().collect();
        let forms = assign_mongolian_forms(&cps);
        assert_eq!(
            forms,
            alloc::vec![JoiningForm::Init, JoiningForm::Fina, JoiningForm::Fina]
        );
    }

    #[test]
    fn vowel_separator_breaks_joining() {
        // A + MVS + E: the vowel separator (U+180E, type U) breaks
        // the cursive chain. Walk:
        //   A: prev=none, next=MVS(U) -> no joiner before, no joiner
        //      after -> isol.
        //   MVS: U -> no joining feature, as in HarfBuzz.
        //   E: prev=MVS(U) -> no joiner before, next=none -> isol.
        let cps: Vec<char> = "\u{1820}\u{180E}\u{1821}".chars().collect();
        let forms = assign_mongolian_forms(&cps);
        assert_eq!(
            forms,
            alloc::vec![JoiningForm::Isol, JoiningForm::None, JoiningForm::Isol]
        );
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(assign_mongolian_forms(&[]).is_empty());
    }

    #[test]
    fn fvs_copies_the_form_of_the_character_before_it() {
        // A, vowel separator, FVS1: the separator has no form, and
        // the FVS copies that (HarfBuzz's
        // `mongolian_variation_selectors`).
        let cps: Vec<char> = "\u{1820}\u{180E}\u{180B}".chars().collect();
        let forms = assign_mongolian_forms(&cps);
        assert_eq!(
            forms,
            alloc::vec![JoiningForm::Isol, JoiningForm::None, JoiningForm::None]
        );
    }

    #[test]
    fn shape_with_no_gsub_keeps_the_glyphs() {
        let cps: Vec<char> = "\u{1820}\u{1821}".chars().collect();
        let mut glyphs = alloc::vec![Glyph::new(1, 0), Glyph::new(2, 3)];
        shape_mongolian(None, None, &cps, &mut glyphs);
        let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
        assert_eq!(ids, [1, 2]);
    }

    #[test]
    fn script_priority_is_mong_then_dflt() {
        assert_eq!(MONG_SCRIPT_PRIORITY, &[*b"mong", *b"DFLT"]);
    }
}
