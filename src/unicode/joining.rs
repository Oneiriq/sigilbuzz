//! Arabic joining-type classification.
//!
//! Every codepoint that participates in Arabic cursive joining carries
//! a *joining type* from `ArabicShaping.txt` (a companion UCD file to
//! `UnicodeData.txt`). The type drives the [`crate::ot::arabic`]
//! state machine that assigns `init` / `medi` / `fina` / `isol`
//! feature tags to each position in an Arabic run.
//!
//! # The six joining types
//!
//! - **U**: Non-joining. Does not connect to its neighbors on
//!   either side (hamza U+0621, brackets, most punctuation).
//! - **R**: Right-joining. Connects to the *previous* (right-side in
//!   logical order for RTL) letter only (alef U+0627, dal U+062F,
//!   reh U+0631, zain U+0632, waw U+0648).
//! - **D**: Dual-joining. Connects on both sides (beh U+0628, teh
//!   U+062A, seen U+0633, the bulk of Arabic letters).
//! - **C**: Join-causing. Forces joining behavior through itself
//!   without having a visual form that changes (tatweel U+0640,
//!   ZWJ U+200D).
//! - **T**: Transparent. Skipped by the joining state machine but
//!   kept in the glyph run (combining marks, harakat, format
//!   characters).
//! - **L**: Left-joining. Connects to the *next* letter only (a few
//!   Manichaean and Mongolian-script-family letters).
//!
//! # Source
//!
//! The table in `joining_table.rs` is generated from Unicode 17.0.0
//! `ArabicShaping.txt` and `DerivedGeneralCategory.txt` (snapshots in
//! `tests/tools/ucd/`; regenerate with
//! `cargo test --test unicode_table_gen -- --ignored`). It follows
//! HarfBuzz's `get_joining_type` in `hb-ot-shaper-arabic.cc`: a code
//! point `ArabicShaping.txt` lists has the type listed there, one it
//! does not list is Transparent when its General_Category is Mn, Me,
//! or Cf (combining marks outside the Arabic blocks, variation
//! selectors, bidi controls), and Non_Joining otherwise. The Syriac
//! ALAPH and DALATH RISH joining groups, which HarfBuzz gives states
//! of their own, keep their joining type R here.

use super::joining_table::JOINING_TYPES;

/// The six Arabic joining types from `ArabicShaping.txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoiningType {
    /// Non-joining (`U`). Does not connect on either side.
    U,
    /// Right-joining (`R`). Connects to the preceding letter only.
    R,
    /// Dual-joining (`D`). Connects on both sides.
    D,
    /// Join-causing (`C`). Propagates joining without visual change.
    C,
    /// Transparent (`T`). Skipped by the joining state machine.
    T,
    /// Left-joining (`L`). Connects to the following letter only.
    L,
}

/// Returns the joining type for `ch`: the `ArabicShaping.txt` type,
/// else [`JoiningType::T`] for a General_Category Mn, Me, or Cf code
/// point, else [`JoiningType::U`], which the state machine treats as
/// a run boundary.
#[must_use]
pub fn joining_type(ch: char) -> JoiningType {
    let cp = ch as u32;
    // Nothing below U+00AD SOFT HYPHEN (Cf) joins or is transparent.
    if cp < 0x00AD {
        return JoiningType::U;
    }
    match JOINING_TYPES.binary_search_by(|&(start, end, _)| {
        if end < cp {
            core::cmp::Ordering::Less
        } else if start > cp {
            core::cmp::Ordering::Greater
        } else {
            core::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => JOINING_TYPES[i].2,
        Err(_) => JoiningType::U,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_non_joining() {
        assert_eq!(joining_type('A'), JoiningType::U);
        assert_eq!(joining_type(' '), JoiningType::U);
        assert_eq!(joining_type('0'), JoiningType::U);
    }

    #[test]
    fn alef_is_right_joining() {
        // U+0627 ARABIC LETTER ALEF: connects only on the right.
        assert_eq!(joining_type('\u{0627}'), JoiningType::R);
        // U+0622..0625: alef variants.
        assert_eq!(joining_type('\u{0622}'), JoiningType::R);
        assert_eq!(joining_type('\u{0625}'), JoiningType::R);
    }

    #[test]
    fn beh_is_dual_joining() {
        // U+0628 ARABIC LETTER BEH: connects both sides.
        assert_eq!(joining_type('\u{0628}'), JoiningType::D);
        // U+062A TEH, U+062B THEH: also D.
        assert_eq!(joining_type('\u{062A}'), JoiningType::D);
        assert_eq!(joining_type('\u{062B}'), JoiningType::D);
    }

    #[test]
    fn reh_and_waw_are_right_joining() {
        assert_eq!(joining_type('\u{0631}'), JoiningType::R); // reh
        assert_eq!(joining_type('\u{0632}'), JoiningType::R); // zain
        assert_eq!(joining_type('\u{0648}'), JoiningType::R); // waw
    }

    #[test]
    fn tatweel_is_join_causing() {
        // U+0640 ARABIC TATWEEL: the stretching baseline glyph.
        assert_eq!(joining_type('\u{0640}'), JoiningType::C);
    }

    #[test]
    fn harakat_are_transparent() {
        // U+064E FATHA, U+064F DAMMA, U+0650 KASRA: vowel marks
        // skipped by the joining state machine.
        assert_eq!(joining_type('\u{064E}'), JoiningType::T);
        assert_eq!(joining_type('\u{064F}'), JoiningType::T);
        assert_eq!(joining_type('\u{0650}'), JoiningType::T);
        // U+0651 SHADDA, U+0652 SUKUN.
        assert_eq!(joining_type('\u{0651}'), JoiningType::T);
        assert_eq!(joining_type('\u{0652}'), JoiningType::T);
    }

    #[test]
    fn zwj_is_join_causing_and_zwnj_is_non_joining() {
        assert_eq!(joining_type('\u{200D}'), JoiningType::C); // ZWJ
        assert_eq!(joining_type('\u{200C}'), JoiningType::U); // ZWNJ
    }

    #[test]
    fn hamza_is_non_joining() {
        // U+0621 ARABIC LETTER HAMZA: visually isolated.
        assert_eq!(joining_type('\u{0621}'), JoiningType::U);
    }

    #[test]
    fn supplement_letters_classify() {
        // U+0750 ARABIC LETTER BEH WITH THREE DOTS HORIZONTALLY
        // BELOW: dual-joining.
        assert_eq!(joining_type('\u{0750}'), JoiningType::D);
    }

    #[test]
    fn extended_a_letters_classify() {
        // U+08A0 ARABIC LETTER BEH WITH SMALL V BELOW: Dual.
        assert_eq!(joining_type('\u{08A0}'), JoiningType::D);
    }

    #[test]
    fn cjk_and_greek_fall_through_to_non_joining() {
        assert_eq!(joining_type('字'), JoiningType::U);
        assert_eq!(joining_type('Δ'), JoiningType::U);
        // Above BMP: astral code point.
        assert_eq!(joining_type('\u{1F600}'), JoiningType::U);
    }

    #[test]
    fn unlisted_marks_and_format_characters_are_transparent() {
        // HarfBuzz: not in ArabicShaping.txt and General_Category Mn,
        // Me, or Cf means Transparent.
        assert_eq!(joining_type('\u{0301}'), JoiningType::T); // Mn, combining acute
        assert_eq!(joining_type('\u{20DD}'), JoiningType::T); // Me, enclosing circle
        assert_eq!(joining_type('\u{00AD}'), JoiningType::T); // Cf, soft hyphen
        assert_eq!(joining_type('\u{200E}'), JoiningType::T); // Cf, LRM
        assert_eq!(joining_type('\u{202A}'), JoiningType::T); // Cf, LRE
        assert_eq!(joining_type('\u{FE0F}'), JoiningType::T); // Mn, VS16
        assert_eq!(joining_type('\u{180B}'), JoiningType::T); // Mn, Mongolian FVS1
        assert_eq!(joining_type('\u{E0100}'), JoiningType::T); // Mn, VS17
                                                               // Listed entries win over the category: the Arabic number
                                                               // signs, ZWNJ, and the bidi isolates are Cf but listed as U.
        assert_eq!(joining_type('\u{0600}'), JoiningType::U);
        assert_eq!(joining_type('\u{200C}'), JoiningType::U);
        assert_eq!(joining_type('\u{2066}'), JoiningType::U);
        // Spacing marks (Mc) are not transparent.
        assert_eq!(joining_type('\u{0903}'), JoiningType::U);
    }

    #[test]
    fn corrections_over_the_old_hand_table() {
        // ALEF MAKSURA (DOTLESS YEH) and KASHMIRI YEH are
        // dual-joining; N'Ko LAJANYALAN and Mongolian NIRUGU are
        // join-causing.
        assert_eq!(joining_type('\u{0649}'), JoiningType::D);
        assert_eq!(joining_type('\u{0620}'), JoiningType::D);
        assert_eq!(joining_type('\u{07FA}'), JoiningType::C);
        assert_eq!(joining_type('\u{180A}'), JoiningType::C);
        // Syriac, outside the old table's blocks: alaph is R, beth D.
        assert_eq!(joining_type('\u{0710}'), JoiningType::R);
        assert_eq!(joining_type('\u{0712}'), JoiningType::D);
    }

    #[test]
    fn table_entries_sorted_and_non_overlapping() {
        // Invariant check: binary search depends on monotonic starts.
        for pair in JOINING_TYPES.windows(2) {
            let (_, end_prev, _) = pair[0];
            let (start_next, _, _) = pair[1];
            assert!(
                end_prev < start_next,
                "overlap or out-of-order entry: {end_prev:#x} then {start_next:#x}"
            );
        }
    }
}
