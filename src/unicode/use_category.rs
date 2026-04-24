//! Universal Shaping Engine (USE) per-codepoint categorisation.
//!
//! The USE is Microsoft's generalised complex-script shaper. It covers
//! Khmer, Myanmar, Tai Tham, Buginese, Cham, New Tai Lue and several
//! others that do not fit the Arabic / Indic2 moulds. Each codepoint
//! it sees is classified along two axes:
//!
//! - [`UseCategory`] — the role the codepoint plays inside a syllable
//!   (base, halant, vowel, final mark, ...). Drives the syllable state
//!   machine and the feature masking.
//! - [`UsePosition`] — where a mark visually sits relative to its base
//!   (pre-base, above-base, below-base, post-base). Drives the reorder
//!   pass and picks the correct positional feature bucket (`abvf`,
//!   `blwf`, `pstf`, `pref`).
//!
//! sigilbuzz 0.2.0 wires up Khmer, Myanmar, Thai, Lao, and the Jamo
//! subset of Hangul against these tables. Tai Tham / Buginese / Cham
//! stay on the generic path until follow-up work. The tables live here
//! rather than inside a script-specific module because the same state
//! machine consumes them for every USE script.
//!
//! # Sources
//!
//! The classification mirrors the columns in the MS-published
//! `IndicSyllabicCategory.txt` / `IndicPositionalCategory.txt` files
//! combined with the USE-specific overrides listed in the MS USE docs.
//! Each script slice was cross-checked against `rustybuzz`'s
//! `ot_shaper_use_table.rs` (reference implementation) so the state
//! machine consumes identical categories.

/// USE per-codepoint category. The mnemonic names mirror the labels
/// from the MS USE documentation so OpenType spec readers can map
/// straight across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum UseCategory {
    /// Base consonant / independent letter — anchors a syllable.
    B,
    /// Independent vowel.
    IV,
    /// Number — digits and number signs.
    N,
    /// Generic base — default for letters that do not participate in
    /// the syllable cluster (punctuation, signs that stand alone).
    GB,
    /// Repha — reordering ra-equivalent. Myanmar uses this for the
    /// `kinzi` cluster (ra + asat + virama preceding the base).
    R,
    /// Symbol.
    S,
    /// Halant / virama — deletes the inherent vowel of the preceding
    /// base and glues it to the following consonant as a subscript.
    /// Khmer uses U+17D2 COENG, Myanmar U+1039, Hangul none.
    H,
    /// Pre-base vowel sign — renders before the base visually.
    VPre,
    /// Above-base vowel sign.
    VAbv,
    /// Below-base vowel sign.
    VBlw,
    /// Post-base vowel sign — renders after the base visually.
    VPst,
    /// Modifying mark — tone marks, registers, robat, bindu-likes.
    M,
    /// Final mark — syllable-final modifiers (visarga, anusvara).
    FM,
    /// Consonant modifier / medial — Myanmar medial ya/ra/wa/ha,
    /// Myanmar asat (U+103A when it is not acting as a kinzi virama).
    CM,
    /// Variation selector.
    VS,
    /// Zero-width non-joiner.
    ZWNJ,
    /// Zero-width joiner.
    ZWJ,
    /// Whitespace / cluster boundary.
    WS,
    /// Anything else — treated as a cluster-break / pass-through.
    O,
}

/// Where a mark sits relative to its base. Matches the IPC partition
/// used by the Indic shaper but with the USE-specific pre/below/post
/// split laid out explicitly so the reorder pass can branch cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum UsePosition {
    /// No positional role — the default for bases, whitespace, marks
    /// that attach at the overall glyph box.
    NotApplicable,
    /// Before the base visually — pre-base matras.
    PreBase,
    /// Above the base — above-base vowel signs and tone marks.
    AboveBase,
    /// Below the base — below-base vowel signs and subscript marks.
    BelowBase,
    /// After the base visually — post-base matras and final marks.
    PostBase,
}

/// Returns the USE positional category for a character.
///
/// Unknown codepoints return [`UsePosition::NotApplicable`].
///
/// The arms are grouped by script rather than by category so the
/// table reads top-to-bottom against the Unicode block layout;
/// clippy's `match_same_arms` lint flags this as mergeable but
/// merging across script boundaries destroys the script-locality
/// that makes the table maintainable.
#[must_use]
#[allow(clippy::match_same_arms)]
pub const fn use_position(ch: char) -> UsePosition {
    let cp = ch as u32;
    match cp {
        // --- Khmer dependent vowel signs --------------------------
        // PostBase: 17B6 sign aa, 17C4/17C5 signs oo/au
        // AboveBase: 17B7..17BA (i/ii/y/yy) and 17BE..17C0 (oe/ya/ie)
        // BelowBase: 17BB..17BD (u/uu/ua)
        // PreBase:   17C1..17C3 (e/ai/am-prefix)
        0x17B6 | 0x17C4 | 0x17C5 => UsePosition::PostBase,
        0x17B7..=0x17BA | 0x17BE..=0x17C0 => UsePosition::AboveBase,
        0x17BB..=0x17BD => UsePosition::BelowBase,
        0x17C1..=0x17C3 => UsePosition::PreBase,

        // --- Myanmar dependent vowel signs ------------------------
        //   VPst: 102B sign tall aa, 102C sign aa,
        //         1056/1057 signs vocalic r/rr, 1062..1064 shan post,
        //         1067/1068/1083/1084, 109C sign shan aa
        //   VAbv: 102D sign i, 102E sign ii, 1032..1035,
        //         1071..1074 shan above-base,
        //         1085..1086, 108D, 109D,
        //         AA7C, A9E5
        //   VBlw: 102F sign u, 1030 sign uu,
        //         1058/1059 vocalic l/ll
        //   VPre: 1031 sign e
        //
        //   CM medials (103B..103E, 105E..1060, 1082) carry no
        //   positional role in the USE tables — they attach as
        //   consonant modifiers and the font chooses their visual
        //   position via GSUB.
        0x102B | 0x102C | 0x1056 | 0x1057 | 0x1062..=0x1064 | 0x1067..=0x1068 | 0x1083..=0x1084
        | 0x109C => UsePosition::PostBase,
        0x102D
        | 0x102E
        | 0x1032..=0x1035
        | 0x1071..=0x1074
        | 0x1085..=0x1086
        | 0x108D
        | 0x109D
        | 0xAA7C
        | 0xA9E5 => UsePosition::AboveBase,
        0x102F | 0x1030 | 0x1058 | 0x1059 => UsePosition::BelowBase,
        // Medial ra (U+103C) renders before the base in Myanmar —
        // USE classifies it as pre-base for reorder purposes. Medial
        // ya / wa / ha (U+103B / 103D / 103E) keep NotApplicable;
        // the font's `blwf` / `pstf` features place them.
        0x1031 | 0x103C => UsePosition::PreBase,

        // --- Thai dependent vowel signs ---------------------------
        //   VPre: 0E40..0E44 (sara e, ae, o, ai-maimuan, ai-maimalai)
        //   VAbv: 0E31 mai han-akat, 0E34..0E37, 0E47..0E4E
        //   VBlw: 0E38..0E3A
        //   VPst: 0E30 sara a, 0E32 sara aa, 0E33 sara am (composed)
        0x0E40..=0x0E44 => UsePosition::PreBase,
        0x0E31 | 0x0E34..=0x0E37 | 0x0E47..=0x0E4E => UsePosition::AboveBase,
        0x0E38..=0x0E3A => UsePosition::BelowBase,
        0x0E30 | 0x0E32 | 0x0E33 => UsePosition::PostBase,

        // --- Lao dependent vowel signs ----------------------------
        //   VPre: 0EC0..0EC4
        //   VAbv: 0EB1, 0EB4..0EB7, 0EC8..0ECD (tone marks)
        //   VBlw: 0EB8, 0EB9
        //   VPst: 0EB0, 0EB2, 0EB3 (lao am, composed)
        0x0EC0..=0x0EC4 => UsePosition::PreBase,
        0x0EB1 | 0x0EB4..=0x0EB7 | 0x0EC8..=0x0ECD => UsePosition::AboveBase,
        0x0EB8 | 0x0EB9 => UsePosition::BelowBase,
        0x0EB0 | 0x0EB2 | 0x0EB3 => UsePosition::PostBase,

        // Currency + signs — no positional role (the currency is a
        // base glyph itself).
        _ => UsePosition::NotApplicable,
    }
}

/// Returns the USE category for a character.
///
/// The table is structured as a series of range matches so additional
/// USE scripts (Tai Tham, Buginese, Cham, ...) can be added as
/// additional match arms without touching the state machine.
///
/// Unknown codepoints return [`UseCategory::O`]. sigilbuzz only has
/// data for scripts it can actively shape; everything else flows
/// through the generic GSUB/GPOS pipeline.
///
/// # Coverage
///
/// Khmer (U+1780..U+17FF, U+19E0..U+19FF), Myanmar (U+1000..U+109F plus
/// Extended-A U+AA60..U+AA7F and Extended-B U+A9E0..U+A9FF), Thai
/// (U+0E00..U+0E7F), Lao (U+0E80..U+0EFF), and the Hangul Jamo blocks
/// (U+1100..U+11FF, U+A960..U+A97F, U+D7B0..U+D7FF) are covered. Format
/// characters and variation selectors carry their shared USE categories.
///
/// Arms are grouped by script / Unicode block — `match_same_arms` is
/// silenced so the table reads top-to-bottom against the block layout
/// and a reviewer can check each script slice in isolation.
#[must_use]
#[allow(clippy::match_same_arms)]
pub const fn use_category(ch: char) -> UseCategory {
    let cp = ch as u32;
    match cp {
        // --- Shared format characters -----------------------------
        0x200C => UseCategory::ZWNJ,
        0x200D => UseCategory::ZWJ,
        0xFE00..=0xFE0F | 0xE0100..=0xE01EF => UseCategory::VS,

        // --- Khmer -----------------------------------------------
        // Khmer consonants (U+1780..U+17A4, incl. deprecated inherent-
        // vowel consonants 17A3 / 17A4 which rustybuzz still treats
        // as bases) plus U+17DC avakrahasanya.
        0x1780..=0x17A4 | 0x17DC => UseCategory::B,
        // Independent vowels U+17A5..U+17B3.
        0x17A5..=0x17B3 => UseCategory::IV,
        // Dotted circle, Khmer inherent-vowel signs (17B4/17B5), the
        // riel currency (17DB), and the Khmer Symbols block.
        0x25CC | 0x17B4..=0x17B5 | 0x17DB | 0x19E0..=0x19FF => UseCategory::GB,
        0x17B6 | 0x17C4 | 0x17C5 => UseCategory::VPst,
        0x17B7..=0x17BA | 0x17BE..=0x17C0 => UseCategory::VAbv,
        0x17BB..=0x17BD => UseCategory::VBlw,
        0x17C1..=0x17C3 => UseCategory::VPre,
        0x17C6..=0x17C8 => UseCategory::FM,
        0x17C9..=0x17D1 | 0x17D3 | 0x17D7 | 0x17DD => UseCategory::M,
        0x17D2 => UseCategory::H,
        0x17E0..=0x17E9 | 0x17F0..=0x17F9 => UseCategory::N,

        // --- Myanmar main block (U+1000..U+109F) ------------------
        // Consonants 1000..1020, plus the late-added ones 1050..1055,
        // 105A..105D, 1061, 1065..1066, 106E..1070, 1075..1081, 108E.
        0x1000..=0x1020
        | 0x103F
        | 0x1050..=0x1055
        | 0x105A..=0x105D
        | 0x1061
        | 0x1065..=0x1066
        | 0x106E..=0x1070
        | 0x1075..=0x1081
        | 0x108E => UseCategory::B,
        // Independent vowels 1021..102A (incl. i, ii, u, uu, e, o, ai).
        0x1021..=0x102A => UseCategory::IV,
        // Vowel signs split by visual position.
        0x102B | 0x102C => UseCategory::VPst,
        0x102D | 0x102E => UseCategory::VAbv,
        0x102F | 0x1030 => UseCategory::VBlw,
        0x1031 => UseCategory::VPre,
        0x1032..=0x1035 => UseCategory::VAbv,
        // Anusvara, dot below, visarga — syllable-final marks.
        0x1036..=0x1038 => UseCategory::FM,
        // Virama (U+1039) + asat (U+103A). asat is the "explicit
        // virama" that doesn't trigger subjoining; classifying it as
        // H lets the state machine end the syllable cleanly.
        0x1039..=0x103A => UseCategory::H,
        // Medial consonants — ya (103B), ra (103C), wa (103D), ha
        // (103E). These are not full halant-joined consonants, they
        // are special Myanmar modifiers that attach to the preceding
        // base via `pref`/`blwf`/`pstf`.
        0x103B..=0x103E => UseCategory::CM,
        // Myanmar digits + shan digits (1040..1049, 1090..1099).
        0x1040..=0x1049 | 0x1090..=0x1099 => UseCategory::N,
        // Myanmar punctuation + symbols (104A..104F).
        0x104A..=0x104F => UseCategory::GB,
        // Vocalic-r/l vowel signs.
        0x1056..=0x1057 => UseCategory::VPst,
        0x1058..=0x1059 => UseCategory::VBlw,
        // Mon medial consonants (105E..1060).
        0x105E..=0x1060 => UseCategory::CM,
        // Shan vowel signs (1062..1064 post, 1067..1068 post,
        // 1071..1074 above, 1083..1084 post, 1085..1086 above,
        // 108D above).
        0x1062..=0x1064 | 0x1067..=0x1068 | 0x1083..=0x1084 => UseCategory::VPst,
        0x1071..=0x1074 | 0x1085..=0x1086 | 0x108D => UseCategory::VAbv,
        // Shan tone marks (1069..106D, 1087..108C, 108F, 109A..109B).
        0x1069..=0x106D | 0x1087..=0x108C | 0x108F | 0x109A..=0x109B => UseCategory::M,
        // Myanmar medial mon la (1082) — consonant modifier.
        0x1082 => UseCategory::CM,
        // Aiton / Khamti sign (109C post, 109D above).
        0x109C => UseCategory::VPst,
        0x109D => UseCategory::VAbv,
        // Symbols at block end (109E..109F).
        0x109E..=0x109F => UseCategory::GB,

        // --- Myanmar Extended-A (U+AA60..U+AA7F) -----------------
        // Shan/Mon/Khamti consonants 0xAA60..0xAA6F,
        // myanmar letter khamti reduplication signs 0xAA70 (sign),
        // 0xAA71..0xAA76 more consonants,
        // 0xAA77..0xAA79 symbols,
        // 0xAA7A letter aiton ra, 0xAA7B sign aiton pa (M),
        // 0xAA7C sign aiton ai (VAbv), 0xAA7D sign aiton bhaa (M),
        // 0xAA7E..0xAA7F shan consonants.
        0xAA60..=0xAA6F | 0xAA71..=0xAA76 | 0xAA7A | 0xAA7E..=0xAA7F => UseCategory::B,
        0xAA70 => UseCategory::CM,
        0xAA77..=0xAA79 => UseCategory::GB,
        0xAA7B | 0xAA7D => UseCategory::M,
        0xAA7C => UseCategory::VAbv,

        // --- Myanmar Extended-B (U+A9E0..U+A9FF) -----------------
        // Shan consonants 0xA9E0..0xA9E4, sign shan saw 0xA9E5
        // (VAbv), letter sign (0xA9E6 — modifier), more consonants
        // 0xA9E7..0xA9EF, Shan digits 0xA9F0..0xA9F9, consonants
        // 0xA9FA..0xA9FE, reserved 0xA9FF.
        0xA9E0..=0xA9E4 | 0xA9E7..=0xA9EF | 0xA9FA..=0xA9FE => UseCategory::B,
        0xA9E5 => UseCategory::VAbv,
        0xA9E6 => UseCategory::CM,
        0xA9F0..=0xA9F9 => UseCategory::N,

        // --- Thai (U+0E00..U+0E7F) -------------------------------
        // Consonants 0E01..0E2E (incl. ng, cho, phoom, ro, lo, wo,
        // so, ho, o, ng-obsolete). No explicit halant — Thai has no
        // subjoining.
        0x0E01..=0x0E2E => UseCategory::B,
        // Independent vowels 0E2F paiyannoi, 0E46 maiyamok (B-like
        // repeat mark). Treat 0E2F as GB (punctuation-style) and
        // 0E46 as GB — both can stand alone.
        0x0E2F | 0x0E46 | 0x0E4F | 0x0E5A..=0x0E5B => UseCategory::GB,
        // Thai tonal / vowel placement.
        0x0E30 | 0x0E32 | 0x0E33 => UseCategory::VPst,
        0x0E31 | 0x0E34..=0x0E37 => UseCategory::VAbv,
        0x0E38..=0x0E39 => UseCategory::VBlw,
        // Pinthu (0E3A) — silencer, above-base in Thai.
        0x0E3A => UseCategory::VBlw,
        // Pre-base vowels.
        0x0E40..=0x0E44 => UseCategory::VPre,
        // Thai currency signs (0E3F baht). GB.
        0x0E3F => UseCategory::GB,
        // Mai Taikhu / Mai Ek / Mai Tho / Mai Tri / Mai Chattawa /
        // Thanthakhat / Nikhahit / Yamakkan — tone / final marks.
        // 0E45 (lakkhangyao) — long-vowel extender (VPst-ish).
        0x0E45 => UseCategory::VPst,
        0x0E47..=0x0E4E => UseCategory::M,
        // Thai digits.
        0x0E50..=0x0E59 => UseCategory::N,

        // --- Lao (U+0E80..U+0EFF) -------------------------------
        // Lao consonants (with gaps — 0E81, 0E82, 0E84, 0E86..0E8A,
        // 0E8C..0EA3, 0EA5, 0EA7..0EAE, 0EB0 boundary, etc.). For
        // simplicity treat every codepoint in the consonant sub-
        // range as B; unassigned codepoints will fall through to
        // the wildcard below.
        0x0E81..=0x0E82
        | 0x0E84
        | 0x0E86..=0x0E8A
        | 0x0E8C..=0x0EA3
        | 0x0EA5
        | 0x0EA7..=0x0EAE
        | 0x0EDC..=0x0EDF => UseCategory::B,
        // Lao vowel signs.
        0x0EAF => UseCategory::GB,
        0x0EB0 | 0x0EB2 | 0x0EB3 => UseCategory::VPst,
        0x0EB1 | 0x0EB4..=0x0EB7 => UseCategory::VAbv,
        0x0EB8..=0x0EB9 => UseCategory::VBlw,
        // 0EBA (sign pali virama) — used as silencer, like pinthu.
        0x0EBA => UseCategory::VBlw,
        // Lao semivowels (0EBB..0EBC) — above-base consonant-like
        // modifier (wo/yo attached).
        0x0EBB..=0x0EBC => UseCategory::CM,
        0x0EBD => UseCategory::B,
        0x0EC0..=0x0EC4 => UseCategory::VPre,
        0x0EC6 => UseCategory::GB,
        0x0EC8..=0x0ECD => UseCategory::M,
        0x0ED0..=0x0ED9 => UseCategory::N,

        // --- Hangul Jamo (U+1100..U+11FF) + Extensions -----------
        // Leading consonants (Choseong): 1100..115F + A960..A97C.
        // Vowels (Jungseong): 1160..11A7 + D7B0..D7C6.
        // Trailing consonants (Jongseong): 11A8..11FF + D7CB..D7FB.
        //
        // All three categories map to B under USE — the state
        // machine treats them as stackable bases, and the font's
        // ljmo/vjmo/tjmo features pick the correct variant form per
        // position. The segmenter emits one syllable per L (+V+T),
        // matching HarfBuzz.
        0x1100..=0x115F | 0xA960..=0xA97C => UseCategory::B,
        0x1160..=0x11A7 | 0xD7B0..=0xD7C6 => UseCategory::B,
        0x11A8..=0x11FF | 0xD7CB..=0xD7FB => UseCategory::B,
        // Precomposed Hangul syllables are single bases.
        0xAC00..=0xD7A3 => UseCategory::B,

        _ => UseCategory::O,
    }
}

/// Returns `true` if the codepoint is a Hangul Leading Jamo
/// (Choseong). The USE segmenter uses this to anchor a Hangul syllable
/// — L is the required opening of `L V? T?`.
#[must_use]
pub const fn is_hangul_l(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp, 0x1100..=0x115F | 0xA960..=0xA97C)
}

/// Returns `true` if the codepoint is a Hangul Vowel Jamo (Jungseong).
#[must_use]
pub const fn is_hangul_v(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp, 0x1160..=0x11A7 | 0xD7B0..=0xD7C6)
}

/// Returns `true` if the codepoint is a Hangul Trailing Jamo
/// (Jongseong).
#[must_use]
pub const fn is_hangul_t(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp, 0x11A8..=0x11FF | 0xD7CB..=0xD7FB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn khmer_consonant_ka_is_base() {
        assert_eq!(use_category('\u{1780}'), UseCategory::B);
        assert_eq!(use_category('\u{17A2}'), UseCategory::B); // ha
    }

    #[test]
    fn khmer_independent_vowels_are_iv() {
        assert_eq!(use_category('\u{17A5}'), UseCategory::IV);
        assert_eq!(use_category('\u{17B3}'), UseCategory::IV);
    }

    #[test]
    fn khmer_coeng_is_halant() {
        assert_eq!(use_category('\u{17D2}'), UseCategory::H);
    }

    #[test]
    fn khmer_pre_base_vowels() {
        // U+17C1 sign e, U+17C2 sign ai, U+17C3 sign am-prefix.
        assert_eq!(use_category('\u{17C1}'), UseCategory::VPre);
        assert_eq!(use_category('\u{17C2}'), UseCategory::VPre);
        assert_eq!(use_category('\u{17C3}'), UseCategory::VPre);
        assert_eq!(use_position('\u{17C1}'), UsePosition::PreBase);
    }

    #[test]
    fn khmer_above_base_vowels() {
        assert_eq!(use_category('\u{17B7}'), UseCategory::VAbv); // i
        assert_eq!(use_category('\u{17B8}'), UseCategory::VAbv); // ii
        assert_eq!(use_position('\u{17B7}'), UsePosition::AboveBase);
    }

    #[test]
    fn khmer_below_base_vowels() {
        assert_eq!(use_category('\u{17BB}'), UseCategory::VBlw); // u
        assert_eq!(use_category('\u{17BC}'), UseCategory::VBlw); // uu
        assert_eq!(use_position('\u{17BB}'), UsePosition::BelowBase);
    }

    #[test]
    fn khmer_post_base_aa_and_au() {
        assert_eq!(use_category('\u{17B6}'), UseCategory::VPst);
        assert_eq!(use_category('\u{17C4}'), UseCategory::VPst);
        assert_eq!(use_position('\u{17B6}'), UsePosition::PostBase);
    }

    #[test]
    fn khmer_nikahit_and_reahmuk_are_final_marks() {
        assert_eq!(use_category('\u{17C6}'), UseCategory::FM);
        assert_eq!(use_category('\u{17C7}'), UseCategory::FM);
    }

    #[test]
    fn khmer_register_shifters_are_modifiers() {
        // U+17C9 muusikatoan, U+17CA triisap — register shifters.
        assert_eq!(use_category('\u{17C9}'), UseCategory::M);
        assert_eq!(use_category('\u{17CA}'), UseCategory::M);
    }

    #[test]
    fn khmer_digits_are_numbers() {
        assert_eq!(use_category('\u{17E0}'), UseCategory::N);
        assert_eq!(use_category('\u{17E9}'), UseCategory::N);
    }

    #[test]
    fn khmer_symbols_block_is_generic_base() {
        assert_eq!(use_category('\u{19E0}'), UseCategory::GB);
        assert_eq!(use_category('\u{19FF}'), UseCategory::GB);
    }

    #[test]
    fn format_characters_classify_correctly() {
        assert_eq!(use_category('\u{200C}'), UseCategory::ZWNJ);
        assert_eq!(use_category('\u{200D}'), UseCategory::ZWJ);
        assert_eq!(use_category('\u{FE00}'), UseCategory::VS);
    }

    #[test]
    fn dotted_circle_is_generic_base() {
        assert_eq!(use_category('\u{25CC}'), UseCategory::GB);
    }

    #[test]
    fn latin_and_other_scripts_fall_through_to_other() {
        assert_eq!(use_category('A'), UseCategory::O);
        assert_eq!(use_category('\u{0915}'), UseCategory::O); // Devanagari
    }

    #[test]
    fn positional_defaults_to_not_applicable_for_bases() {
        assert_eq!(use_position('\u{1780}'), UsePosition::NotApplicable);
        assert_eq!(use_position('A'), UsePosition::NotApplicable);
    }

    // --- Myanmar tests -------------------------------------------

    #[test]
    fn myanmar_consonants_are_bases() {
        assert_eq!(use_category('\u{1000}'), UseCategory::B); // ka
        assert_eq!(use_category('\u{1020}'), UseCategory::B); // la
        assert_eq!(use_category('\u{103F}'), UseCategory::B); // great sa
    }

    #[test]
    fn myanmar_independent_vowels() {
        assert_eq!(use_category('\u{1021}'), UseCategory::IV); // a
        assert_eq!(use_category('\u{1027}'), UseCategory::IV); // e
        assert_eq!(use_category('\u{102A}'), UseCategory::IV); // aw
    }

    #[test]
    fn myanmar_vowel_signs_positions() {
        // Post: 102B tall aa, 102C aa.
        assert_eq!(use_category('\u{102B}'), UseCategory::VPst);
        assert_eq!(use_category('\u{102C}'), UseCategory::VPst);
        // Above: 102D i, 102E ii, 1032 ai.
        assert_eq!(use_category('\u{102D}'), UseCategory::VAbv);
        assert_eq!(use_category('\u{102E}'), UseCategory::VAbv);
        assert_eq!(use_category('\u{1032}'), UseCategory::VAbv);
        // Below: 102F u, 1030 uu.
        assert_eq!(use_category('\u{102F}'), UseCategory::VBlw);
        assert_eq!(use_category('\u{1030}'), UseCategory::VBlw);
        // Pre: 1031 e (only pre-base vowel sign in Myanmar).
        assert_eq!(use_category('\u{1031}'), UseCategory::VPre);
        assert_eq!(use_position('\u{1031}'), UsePosition::PreBase);
    }

    #[test]
    fn myanmar_virama_and_asat_are_halant() {
        assert_eq!(use_category('\u{1039}'), UseCategory::H); // virama
        assert_eq!(use_category('\u{103A}'), UseCategory::H); // asat
    }

    #[test]
    fn myanmar_medial_consonants_are_cm() {
        assert_eq!(use_category('\u{103B}'), UseCategory::CM); // medial ya
        assert_eq!(use_category('\u{103C}'), UseCategory::CM); // medial ra
        assert_eq!(use_category('\u{103D}'), UseCategory::CM); // medial wa
        assert_eq!(use_category('\u{103E}'), UseCategory::CM); // medial ha
    }

    #[test]
    fn myanmar_final_marks() {
        assert_eq!(use_category('\u{1036}'), UseCategory::FM); // anusvara
        assert_eq!(use_category('\u{1037}'), UseCategory::FM); // dot below
        assert_eq!(use_category('\u{1038}'), UseCategory::FM); // visarga
    }

    #[test]
    fn myanmar_digits_are_numbers() {
        assert_eq!(use_category('\u{1040}'), UseCategory::N);
        assert_eq!(use_category('\u{1049}'), UseCategory::N);
    }

    #[test]
    fn myanmar_extended_a_consonants() {
        assert_eq!(use_category('\u{AA60}'), UseCategory::B); // shan letter kha
        assert_eq!(use_category('\u{AA7C}'), UseCategory::VAbv); // aiton ai
    }

    #[test]
    fn myanmar_extended_b_digits() {
        assert_eq!(use_category('\u{A9F0}'), UseCategory::N);
        assert_eq!(use_category('\u{A9F9}'), UseCategory::N);
    }

    // --- Thai tests ----------------------------------------------

    #[test]
    fn thai_consonants_are_bases() {
        assert_eq!(use_category('\u{0E01}'), UseCategory::B); // ko kai
        assert_eq!(use_category('\u{0E2E}'), UseCategory::B); // ho nokhuk
    }

    #[test]
    fn thai_pre_base_vowels() {
        // Sara e (0E40), sara ae (0E41), sara o (0E42),
        // sara ai-maimuan (0E43), sara ai-maimalai (0E44).
        assert_eq!(use_category('\u{0E40}'), UseCategory::VPre);
        assert_eq!(use_category('\u{0E44}'), UseCategory::VPre);
        assert_eq!(use_position('\u{0E40}'), UsePosition::PreBase);
    }

    #[test]
    fn thai_above_and_below_vowels() {
        // Mai han-akat (0E31), sara i (0E34), sara ii (0E35),
        // sara ue (0E36), sara uee (0E37).
        assert_eq!(use_category('\u{0E31}'), UseCategory::VAbv);
        assert_eq!(use_category('\u{0E34}'), UseCategory::VAbv);
        // Sara u (0E38), sara uu (0E39), phinthu (0E3A).
        assert_eq!(use_category('\u{0E38}'), UseCategory::VBlw);
        assert_eq!(use_category('\u{0E39}'), UseCategory::VBlw);
    }

    #[test]
    fn thai_tone_marks_are_modifiers() {
        // Mai taikhu (0E47), mai ek (0E48), mai tho (0E49),
        // thanthakhat (0E4C), nikkhahit (0E4D), yamakkan (0E4E).
        assert_eq!(use_category('\u{0E47}'), UseCategory::M);
        assert_eq!(use_category('\u{0E48}'), UseCategory::M);
        assert_eq!(use_category('\u{0E4C}'), UseCategory::M);
        assert_eq!(use_category('\u{0E4D}'), UseCategory::M);
    }

    #[test]
    fn thai_digits() {
        assert_eq!(use_category('\u{0E50}'), UseCategory::N);
        assert_eq!(use_category('\u{0E59}'), UseCategory::N);
    }

    // --- Lao tests -----------------------------------------------

    #[test]
    fn lao_consonants_are_bases() {
        assert_eq!(use_category('\u{0E81}'), UseCategory::B); // ko
        assert_eq!(use_category('\u{0E97}'), UseCategory::B); // tho
    }

    #[test]
    fn lao_vowels_positions() {
        assert_eq!(use_category('\u{0EB1}'), UseCategory::VAbv); // mai kan
        assert_eq!(use_category('\u{0EB8}'), UseCategory::VBlw); // sara u
        assert_eq!(use_category('\u{0EC0}'), UseCategory::VPre); // sara e
        assert_eq!(use_category('\u{0EB2}'), UseCategory::VPst); // sara aa
    }

    #[test]
    fn lao_tone_marks() {
        assert_eq!(use_category('\u{0EC8}'), UseCategory::M);
        assert_eq!(use_category('\u{0ECD}'), UseCategory::M);
    }

    #[test]
    fn lao_digits() {
        assert_eq!(use_category('\u{0ED0}'), UseCategory::N);
        assert_eq!(use_category('\u{0ED9}'), UseCategory::N);
    }

    // --- Hangul tests --------------------------------------------

    #[test]
    fn hangul_jamo_are_bases() {
        // Leading, vowel, trailing — all three classify as B for
        // the USE state machine; the font's ljmo/vjmo/tjmo features
        // pick the positional variant.
        assert_eq!(use_category('\u{1100}'), UseCategory::B); // L kiyeok
        assert_eq!(use_category('\u{1161}'), UseCategory::B); // V a
        assert_eq!(use_category('\u{11A8}'), UseCategory::B); // T kiyeok
    }

    #[test]
    fn hangul_jamo_predicates_split_l_v_t() {
        assert!(is_hangul_l('\u{1100}'));
        assert!(!is_hangul_l('\u{1161}'));
        assert!(is_hangul_v('\u{1161}'));
        assert!(!is_hangul_v('\u{11A8}'));
        assert!(is_hangul_t('\u{11A8}'));
        assert!(!is_hangul_t('\u{1100}'));
    }

    #[test]
    fn hangul_jamo_extensions_covered() {
        // Extended-A is all L. Extended-B has a mix — 0xD7B0..0xD7C6
        // are V, 0xD7CB..0xD7FB are T.
        assert!(is_hangul_l('\u{A960}'));
        assert!(is_hangul_v('\u{D7B0}'));
        assert!(is_hangul_t('\u{D7CB}'));
    }

    #[test]
    fn precomposed_hangul_syllable_is_base() {
        assert_eq!(use_category('\u{AC00}'), UseCategory::B); // 가
        assert_eq!(use_category('\u{D7A3}'), UseCategory::B);
    }
}
