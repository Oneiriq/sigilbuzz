//! Tests for the USE category and position tables, grouped by script.

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
    // U+17C9 muusikatoan, U+17CA triisap: register shifters.
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
    // Leading, vowel, trailing: all three classify as B for
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
    // Extended-A is all L. Extended-B has a mix: 0xD7B0..0xD7C6
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

// --- N'Ko tests ----------------------------------------------

#[test]
fn nko_letters_and_marks() {
    assert_eq!(use_category('\u{07CA}'), UseCategory::B); // letter a
    assert_eq!(use_category('\u{07EA}'), UseCategory::B);
    assert_eq!(use_category('\u{07EB}'), UseCategory::M); // tone mark
    assert_eq!(use_category('\u{07FD}'), UseCategory::M); // dantayalan
    assert_eq!(use_category('\u{07C0}'), UseCategory::N); // digit 0
    assert_eq!(use_category('\u{07C9}'), UseCategory::N); // digit 9
    assert_eq!(use_category('\u{07FA}'), UseCategory::CM); // lajanyalan
}

// --- Buginese tests ------------------------------------------

#[test]
fn buginese_letters_and_signs() {
    assert_eq!(use_category('\u{1A00}'), UseCategory::B); // ka
    assert_eq!(use_category('\u{1A16}'), UseCategory::B);
    assert_eq!(use_category('\u{1A17}'), UseCategory::VAbv); // sara i
    assert_eq!(use_category('\u{1A18}'), UseCategory::VBlw); // sara u
    assert_eq!(use_category('\u{1A19}'), UseCategory::VPre); // sara e
    assert_eq!(use_category('\u{1A1A}'), UseCategory::VPst); // sara o
    assert_eq!(use_category('\u{1A1B}'), UseCategory::VAbv); // sara ae
}

// --- Tai Tham tests ------------------------------------------

#[test]
fn tai_tham_basics() {
    assert_eq!(use_category('\u{1A20}'), UseCategory::B); // high ka
    assert_eq!(use_category('\u{1A4D}'), UseCategory::IV);
    assert_eq!(use_category('\u{1A60}'), UseCategory::H); // sakot
    assert_eq!(use_category('\u{1A55}'), UseCategory::CM); // medial ra
    assert_eq!(use_category('\u{1A6E}'), UseCategory::VPre); // pre-base
    assert_eq!(use_category('\u{1A80}'), UseCategory::N); // hora digit 0
}

// --- Balinese tests ------------------------------------------

#[test]
fn balinese_basics() {
    assert_eq!(use_category('\u{1B05}'), UseCategory::B); // letter a
    assert_eq!(use_category('\u{1B35}'), UseCategory::VPst); // tedung
    assert_eq!(use_category('\u{1B36}'), UseCategory::VAbv); // i
    assert_eq!(use_category('\u{1B39}'), UseCategory::VBlw); // u-style
    assert_eq!(use_category('\u{1B44}'), UseCategory::H); // adeg adeg
    assert_eq!(use_category('\u{1B50}'), UseCategory::N); // digit 0
}

// --- Sundanese tests -----------------------------------------

#[test]
fn sundanese_basics() {
    assert_eq!(use_category('\u{1B83}'), UseCategory::IV); // letter a
    assert_eq!(use_category('\u{1B95}'), UseCategory::B); // letter ka
    assert_eq!(use_category('\u{1B80}'), UseCategory::M); // panyecek
    assert_eq!(use_category('\u{1B82}'), UseCategory::FM); // pangwisad
    assert_eq!(use_category('\u{1BA4}'), UseCategory::VAbv); // sara i
    assert_eq!(use_category('\u{1BA6}'), UseCategory::VPre); // sara e
    assert_eq!(use_category('\u{1BAB}'), UseCategory::H); // virama
    assert_eq!(use_category('\u{1BB0}'), UseCategory::N); // digit 0
}

// --- Lepcha tests --------------------------------------------

#[test]
fn lepcha_basics() {
    assert_eq!(use_category('\u{1C00}'), UseCategory::B); // ka
    assert_eq!(use_category('\u{1C24}'), UseCategory::CM); // subjoined ya
    assert_eq!(use_category('\u{1C26}'), UseCategory::VPst); // sign i
    assert_eq!(use_category('\u{1C27}'), UseCategory::VPre); // sign o
    assert_eq!(use_category('\u{1C36}'), UseCategory::M); // ran
    assert_eq!(use_category('\u{1C40}'), UseCategory::N); // digit 0
}

// --- Limbu tests ---------------------------------------------

#[test]
fn limbu_basics() {
    assert_eq!(use_category('\u{1900}'), UseCategory::B); // letter ka
    assert_eq!(use_category('\u{1920}'), UseCategory::VAbv); // sign a
    assert_eq!(use_category('\u{1923}'), UseCategory::VBlw); // sign ee
    assert_eq!(use_category('\u{1929}'), UseCategory::CM); // subjoined ya
    assert_eq!(use_category('\u{1930}'), UseCategory::CM); // small ka
    assert_eq!(use_category('\u{1939}'), UseCategory::M); // tone marker
    assert_eq!(use_category('\u{1946}'), UseCategory::N); // digit 0
}

// --- Cham tests ----------------------------------------------

#[test]
fn cham_basics() {
    assert_eq!(use_category('\u{AA00}'), UseCategory::IV); // letter a
    assert_eq!(use_category('\u{AA06}'), UseCategory::B);
    assert_eq!(use_category('\u{AA29}'), UseCategory::VAbv); // sign aa
    assert_eq!(use_category('\u{AA2F}'), UseCategory::VPre); // sign oe (pre-base)
    assert_eq!(use_category('\u{AA34}'), UseCategory::CM); // medial ra
    assert_eq!(use_category('\u{AA40}'), UseCategory::CM); // final k
    assert_eq!(use_category('\u{AA43}'), UseCategory::FM); // final ng
    assert_eq!(use_category('\u{AA50}'), UseCategory::N); // digit 0
}

// --- Brahmi-family historical-script tests -------------------

#[test]
fn brahmi_basics() {
    // 11000 candrabindu (M), 11001 anusvara (M), 11002 visarga (FM).
    assert_eq!(use_category('\u{11000}'), UseCategory::M);
    assert_eq!(use_category('\u{11001}'), UseCategory::M);
    assert_eq!(use_category('\u{11002}'), UseCategory::FM);
    // Independent vowels.
    assert_eq!(use_category('\u{11003}'), UseCategory::IV);
    assert_eq!(use_category('\u{11005}'), UseCategory::IV);
    // Letter ka.
    assert_eq!(use_category('\u{11015}'), UseCategory::B);
    // Vowel signs.
    assert_eq!(use_category('\u{11038}'), UseCategory::VPst); // sign aa
    assert_eq!(use_category('\u{11039}'), UseCategory::VAbv); // sign i
    assert_eq!(use_category('\u{1103B}'), UseCategory::VBlw); // sign u
                                                              // Virama.
    assert_eq!(use_category('\u{11046}'), UseCategory::H);
    // Digits.
    assert_eq!(use_category('\u{11066}'), UseCategory::N);
    assert_eq!(use_category('\u{1106F}'), UseCategory::N);
}

#[test]
fn sharada_basics() {
    // 11180 candrabindu (M), 11181 anusvara (M), 11182 visarga (FM).
    assert_eq!(use_category('\u{11180}'), UseCategory::M);
    assert_eq!(use_category('\u{11181}'), UseCategory::M);
    assert_eq!(use_category('\u{11182}'), UseCategory::FM);
    // Letters.
    assert_eq!(use_category('\u{11183}'), UseCategory::IV); // letter a
    assert_eq!(use_category('\u{11192}'), UseCategory::B); // letter ka
                                                           // Vowel signs. Sign-i renders visually before the base in
                                                           // Sharada (font has spacing pre-base sign-i glyph), so the
                                                           // USE category is VPre to drive the pre-base reorder.
    assert_eq!(use_category('\u{111B3}'), UseCategory::VPst); // sign aa
    assert_eq!(use_category('\u{111B4}'), UseCategory::VPre); // sign i (pre-base)
    assert_eq!(use_position('\u{111B4}'), UsePosition::PreBase);
    assert_eq!(use_category('\u{111B6}'), UseCategory::VBlw); // sign u
                                                              // Virama.
    assert_eq!(use_category('\u{111C0}'), UseCategory::H);
    // Digits.
    assert_eq!(use_category('\u{111D0}'), UseCategory::N);
    assert_eq!(use_category('\u{111D9}'), UseCategory::N);
}

#[test]
fn khojki_basics() {
    // Letters.
    assert_eq!(use_category('\u{11200}'), UseCategory::IV); // letter a
    assert_eq!(use_category('\u{11208}'), UseCategory::B); // letter ka
                                                           // Vowel signs.
    assert_eq!(use_category('\u{1122C}'), UseCategory::VPst); // sign aa
    assert_eq!(use_category('\u{11230}'), UseCategory::VAbv); // sign e
    assert_eq!(use_category('\u{1122F}'), UseCategory::VBlw); // sign u
                                                              // Anusvara / virama.
    assert_eq!(use_category('\u{11234}'), UseCategory::M); // anusvara
    assert_eq!(use_category('\u{11235}'), UseCategory::H); // virama
                                                           // Letter qa.
    assert_eq!(use_category('\u{1123F}'), UseCategory::B);
}

#[test]
fn tirhuta_basics() {
    // Letters.
    assert_eq!(use_category('\u{11480}'), UseCategory::IV); // letter a
    assert_eq!(use_category('\u{1148A}'), UseCategory::B); // letter ka
                                                           // Vowel signs.
    assert_eq!(use_category('\u{114B0}'), UseCategory::VPst); // sign aa
    assert_eq!(use_category('\u{114B3}'), UseCategory::VBlw); // sign u
    assert_eq!(use_category('\u{114B9}'), UseCategory::VPre); // sign e (pre-base)
    assert_eq!(use_category('\u{114BC}'), UseCategory::VPre); // sign o (pre-base)
    assert_eq!(use_category('\u{114BA}'), UseCategory::VAbv); // sign short e
                                                              // Marks / virama.
    assert_eq!(use_category('\u{114C0}'), UseCategory::M); // anusvara
    assert_eq!(use_category('\u{114C1}'), UseCategory::FM); // visarga
    assert_eq!(use_category('\u{114C2}'), UseCategory::H); // virama
                                                           // Digits.
    assert_eq!(use_category('\u{114D0}'), UseCategory::N);
}

#[test]
fn modi_basics() {
    // Letters.
    assert_eq!(use_category('\u{11600}'), UseCategory::IV); // letter a
    assert_eq!(use_category('\u{11606}'), UseCategory::B); // letter ka
                                                           // Vowel signs.
    assert_eq!(use_category('\u{11630}'), UseCategory::VPst); // sign aa
    assert_eq!(use_category('\u{11633}'), UseCategory::VBlw); // sign u
    assert_eq!(use_category('\u{11639}'), UseCategory::VAbv); // sign e
                                                              // Marks / virama.
    assert_eq!(use_category('\u{1163D}'), UseCategory::M); // anusvara
    assert_eq!(use_category('\u{1163E}'), UseCategory::FM); // visarga
    assert_eq!(use_category('\u{1163F}'), UseCategory::H); // virama
                                                           // Digits.
    assert_eq!(use_category('\u{11650}'), UseCategory::N);
    assert_eq!(use_category('\u{11659}'), UseCategory::N);
}
