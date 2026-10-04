//! Tests of the script classification in `super`.

use super::*;

#[test]
fn classifies_latin() {
    assert_eq!(script_of('A'), Script::Latin);
    assert_eq!(script_of('z'), Script::Latin);
    // The ordinal indicators are Latin letters.
    assert_eq!(script_of('\u{00AA}'), Script::Latin);
    // Letters outside Basic Latin and Latin-1 and Extended-A/B: IPA,
    // Latin Extended Additional (Vietnamese), Extended-C, -D, -E,
    // the f ligatures, the fullwidth forms, and the Roman numerals.
    for c in [
        '\u{0250}', '\u{1EF7}', '\u{1EF0}', '\u{1EA0}', '\u{2C60}', '\u{A732}', '\u{AB30}',
        '\u{FB01}', '\u{FF41}', '\u{2160}',
    ] {
        assert_eq!(script_of(c), Script::Latin, "U+{:04X}", c as u32);
    }
    // Digits, punctuation, and the space are Common.
    for c in ['0', ' ', '.', '\u{00D7}'] {
        assert_eq!(script_of(c), Script::Other, "U+{:04X}", c as u32);
    }
}

#[test]
fn classifies_cjk_ideographs() {
    assert_eq!(script_of('字'), Script::Han);
    assert_eq!(script_of('あ'), Script::Han); // Hiragana
    assert_eq!(script_of('カ'), Script::Han); // Katakana

    // Extensions past B, the compatibility ideographs, the radicals,
    // the ideographic iteration mark, and the kana outside the
    // Hiragana and Katakana blocks.
    let han =
        "\u{2A700}\u{2B820}\u{30000}\u{2EBF0}\u{F900}\u{2F00}\u{2E80}\u{3005}\u{31F0}\u{FF66}";
    for c in han.chars().chain(['\u{1B001}', '\u{32D0}']) {
        assert_eq!(script_of(c), Script::Han, "U+{:04X}", c as u32);
    }
    // The CJK punctuation and the prolonged sound mark are Common.
    assert_eq!(script_of('\u{3001}'), Script::Other);
    assert_eq!(script_of('\u{30FC}'), Script::Other);
}

#[test]
fn classifies_arabic() {
    assert_eq!(script_of('ا'), Script::Arabic);
    assert_eq!(script_of('ل'), Script::Arabic);
    // Arabic Extended-A, -B, and -C, the Rumi numerals, and the
    // mathematical alphabetic symbols.
    for c in "\u{08A0}\u{08F0}\u{0870}\u{10EC2}\u{10E60}\u{1EE00}".chars() {
        assert_eq!(script_of(c), Script::Arabic, "U+{:04X}", c as u32);
    }
    // The tatweel and the comma are Common, the harakat Inherited.
    for c in ['\u{0640}', '\u{060C}', '\u{064B}'] {
        assert_eq!(script_of(c), Script::Other, "U+{:04X}", c as u32);
    }
}

#[test]
fn classifies_hebrew() {
    // Main block: alef, lamed, final-mem, sheva, cantillation
    // etnahta.
    assert_eq!(script_of('\u{05D0}'), Script::Hebrew);
    assert_eq!(script_of('\u{05DC}'), Script::Hebrew);
    assert_eq!(script_of('\u{05DD}'), Script::Hebrew);
    assert_eq!(script_of('\u{05B0}'), Script::Hebrew);
    assert_eq!(script_of('\u{0591}'), Script::Hebrew);
    // Presentation forms: alef with patah, shin with dot,
    // lam+alef equivalent position.
    assert_eq!(script_of('\u{FB2E}'), Script::Hebrew);
    assert_eq!(script_of('\u{FB2A}'), Script::Hebrew);
    // Boundary: U+FB50 is Arabic presentation forms, not Hebrew.
    assert_eq!(script_of('\u{FB50}'), Script::Arabic);
}

#[test]
fn classifies_greek_and_cyrillic() {
    assert_eq!(script_of('Δ'), Script::Greek);
    assert_eq!(script_of('\u{1F00}'), Script::Greek);
    assert_eq!(script_of('Д'), Script::Cyrillic);
    // Cyrillic Extended-A (combining letters), -B, and -C.
    assert_eq!(script_of('\u{2DE0}'), Script::Cyrillic);
    assert_eq!(script_of('\u{A640}'), Script::Cyrillic);
    assert_eq!(script_of('\u{1C80}'), Script::Cyrillic);
    // The Coptic letters of the Greek and Coptic block are Coptic.
    assert_eq!(script_of('\u{03E2}'), Script::Other);
}

#[test]
fn classifies_devanagari() {
    // Devanagari ka (U+0915) and vowel sign I (U+093F).
    assert_eq!(script_of('\u{0915}'), Script::Devanagari);
    assert_eq!(script_of('\u{093F}'), Script::Devanagari);
    // Devanagari Extended and Extended-A.
    assert_eq!(script_of('\u{A8F2}'), Script::Devanagari);
    assert_eq!(script_of('\u{11B00}'), Script::Devanagari);
    // The dandas are Common.
    assert_eq!(script_of('\u{0964}'), Script::Other);
}

#[test]
fn classifies_indic_family() {
    assert_eq!(script_of('\u{09B0}'), Script::Bengali); // Bengali RA
    assert_eq!(script_of('\u{0A30}'), Script::Gurmukhi); // Gurmukhi RA
    assert_eq!(script_of('\u{0AB0}'), Script::Gujarati); // Gujarati RA
    assert_eq!(script_of('\u{0B30}'), Script::Oriya); // Oriya RA
    assert_eq!(script_of('\u{0BB0}'), Script::Tamil); // Tamil RA
    assert_eq!(script_of('\u{0C30}'), Script::Telugu); // Telugu RA
    assert_eq!(script_of('\u{0CB0}'), Script::Kannada); // Kannada RA
    assert_eq!(script_of('\u{0D30}'), Script::Malayalam); // Malayalam RA
    assert_eq!(script_of('\u{0DB1}'), Script::Sinhala); // Sinhala NA
}

#[test]
fn is_indic_covers_full_family() {
    assert!(Script::Devanagari.is_indic());
    assert!(Script::Bengali.is_indic());
    assert!(Script::Gurmukhi.is_indic());
    assert!(Script::Gujarati.is_indic());
    assert!(Script::Oriya.is_indic());
    assert!(Script::Tamil.is_indic());
    assert!(Script::Telugu.is_indic());
    assert!(Script::Kannada.is_indic());
    assert!(Script::Malayalam.is_indic());
    // HarfBuzz shapes Sinhala with the Universal Shaping Engine.
    assert!(!Script::Sinhala.is_indic());
    assert!(!Script::Latin.is_indic());
    assert!(!Script::Arabic.is_indic());
    assert!(!Script::Other.is_indic());
}

#[test]
fn unknown_scripts_fall_through_to_other() {
    // Armenian: not in the bootstrap table.
    assert_eq!(script_of('\u{0531}'), Script::Other);
}

#[test]
fn classifies_khmer() {
    // U+1780 (consonant ka), U+17CA (register shifter),
    // U+17DB (currency riel), and a Khmer Symbols sign
    // U+19E0 sit in the Khmer bucket.
    assert_eq!(script_of('\u{1780}'), Script::Khmer);
    assert_eq!(script_of('\u{17CA}'), Script::Khmer);
    assert_eq!(script_of('\u{17DB}'), Script::Khmer);
    assert_eq!(script_of('\u{19E0}'), Script::Khmer);
}

#[test]
fn classifies_myanmar() {
    // U+1000 (consonant ka) and U+102C (sign aa).
    assert_eq!(script_of('\u{1000}'), Script::Myanmar);
    assert_eq!(script_of('\u{102C}'), Script::Myanmar);
    // Myanmar Extended-A (e.g. Shan sign maun).
    assert_eq!(script_of('\u{AA60}'), Script::Myanmar);
    // Myanmar Extended-B.
    assert_eq!(script_of('\u{A9E0}'), Script::Myanmar);
}

#[test]
fn classifies_thai() {
    // U+0E01 (consonant ko kai) and U+0E31 (mai han-akat).
    assert_eq!(script_of('\u{0E01}'), Script::Thai);
    assert_eq!(script_of('\u{0E31}'), Script::Thai);
    // Block end: Thai digits.
    assert_eq!(script_of('\u{0E50}'), Script::Thai);
}

#[test]
fn classifies_lao() {
    // U+0E81 (consonant ko) and U+0EB1 (mai kan).
    assert_eq!(script_of('\u{0E81}'), Script::Lao);
    assert_eq!(script_of('\u{0EB1}'), Script::Lao);
}

#[test]
fn classifies_hangul() {
    // Jamo: leading U+1100, vowel U+1161, trailing U+11A8.
    // Precomposed U+AC00 is also in the Hangul
    // bucket: `is_hangul_jamo` separates the USE-routed subset.
    assert_eq!(script_of('\u{1100}'), Script::Hangul);
    assert_eq!(script_of('\u{1161}'), Script::Hangul);
    assert_eq!(script_of('\u{11A8}'), Script::Hangul);
    assert_eq!(script_of('\u{AC00}'), Script::Hangul);
    assert_eq!(script_of('\u{A960}'), Script::Hangul);
    assert_eq!(script_of('\u{D7B0}'), Script::Hangul);
    // The tone marks, a parenthesized syllable, and a halfwidth jamo.
    assert_eq!(script_of('\u{302E}'), Script::Hangul);
    assert_eq!(script_of('\u{3200}'), Script::Hangul);
    assert_eq!(script_of('\u{FFA1}'), Script::Hangul);
}

#[test]
fn classifies_supplements_of_the_older_buckets() {
    assert_eq!(script_of('\u{11660}'), Script::Mongolian); // Mongolian Supplement
    assert_eq!(script_of('\u{1820}'), Script::Mongolian);
    assert_eq!(script_of('\u{11FC0}'), Script::Tamil); // Tamil Supplement
    assert_eq!(script_of('\u{111E1}'), Script::Sinhala); // archaic numbers
    assert_eq!(script_of('\u{116D0}'), Script::Myanmar); // Myanmar Extended-C
    assert_eq!(script_of('\u{1CC0}'), Script::Sundanese); // Sundanese Supplement

    // The Mongolian comma and full stop are Common.
    assert_eq!(script_of('\u{1802}'), Script::Other);
    assert_eq!(script_of('\u{1803}'), Script::Other);
}

#[test]
fn hangul_jamo_predicate_only_matches_jamo_blocks() {
    assert!(is_hangul_jamo('\u{1100}'));
    assert!(is_hangul_jamo('\u{11A8}'));
    assert!(is_hangul_jamo('\u{A960}'));
    assert!(is_hangul_jamo('\u{D7B0}'));
    // Precomposed syllables are NOT jamo.
    assert!(!is_hangul_jamo('\u{AC00}'));
    // Compatibility jamo are NOT the USE-routed block.
    assert!(!is_hangul_jamo('\u{3131}'));
}

#[test]
fn is_use_covers_all_use_scripts() {
    // The scripts HarfBuzz gives the Universal Shaping Engine.
    assert!(Script::Sinhala.is_use());
    assert!(Script::Tibetan.is_use());
    assert!(Script::Mongolian.is_use());
    assert!(Script::NKo.is_use());
    assert!(Script::Buginese.is_use());
    assert!(Script::TaiTham.is_use());
    assert!(Script::Balinese.is_use());
    assert!(Script::Sundanese.is_use());
    assert!(Script::Lepcha.is_use());
    assert!(Script::Limbu.is_use());
    assert!(Script::Cham.is_use());
    assert!(Script::Brahmi.is_use());
    assert!(Script::Sharada.is_use());
    assert!(Script::Khojki.is_use());
    assert!(Script::Tirhuta.is_use());
    assert!(Script::Modi.is_use());
    assert!(!Script::Latin.is_use());
    assert!(!Script::Devanagari.is_use());
    // These have shapers of their own.
    for script in [
        Script::Khmer,
        Script::Myanmar,
        Script::Thai,
        Script::Lao,
        Script::Hangul,
    ] {
        assert!(!script.is_use(), "{script:?}");
    }
}

#[test]
fn classifies_brahmi_family_smp_scripts() {
    // Brahmi (U+11000..U+1107F).
    assert_eq!(script_of('\u{11000}'), Script::Brahmi); // candrabindu
    assert_eq!(script_of('\u{11015}'), Script::Brahmi); // letter ka
    assert_eq!(script_of('\u{1107F}'), Script::Brahmi); // block end
                                                        // Sharada (U+11180..U+111DF).
    assert_eq!(script_of('\u{11180}'), Script::Sharada);
    assert_eq!(script_of('\u{11192}'), Script::Sharada); // letter ka
    assert_eq!(script_of('\u{111DF}'), Script::Sharada);
    // Khojki (U+11200..U+1124F).
    assert_eq!(script_of('\u{11200}'), Script::Khojki); // letter a
    assert_eq!(script_of('\u{11208}'), Script::Khojki); // letter ka
                                                        // Tirhuta (U+11480..U+114DF).
    assert_eq!(script_of('\u{11480}'), Script::Tirhuta); // letter a
    assert_eq!(script_of('\u{1148A}'), Script::Tirhuta); // letter ka
                                                         // Modi (U+11600..U+1165F).
    assert_eq!(script_of('\u{11600}'), Script::Modi); // letter a
    assert_eq!(script_of('\u{11606}'), Script::Modi); // letter ka
}

#[test]
fn classifies_added_use_scripts() {
    assert_eq!(script_of('\u{07CA}'), Script::NKo); // letter ba
    assert_eq!(script_of('\u{1A00}'), Script::Buginese); // letter ka
    assert_eq!(script_of('\u{1A20}'), Script::TaiTham); // letter high ka
    assert_eq!(script_of('\u{1B00}'), Script::Balinese);
    assert_eq!(script_of('\u{1B83}'), Script::Sundanese); // letter a
    assert_eq!(script_of('\u{1C00}'), Script::Lepcha); // letter ka
    assert_eq!(script_of('\u{1900}'), Script::Limbu); // vowel-carrier
    assert_eq!(script_of('\u{AA00}'), Script::Cham); // letter a
                                                     // Block boundaries.
    assert_eq!(script_of('\u{AA5F}'), Script::Cham);
    assert_eq!(script_of('\u{AA60}'), Script::Myanmar);
}

#[test]
fn classifies_scripts_from_the_script_property() {
    // Letters of the buckets added in 0.22.0, which come from the
    // generated Script table (Unicode 18.0.0).
    assert_eq!(script_of('\u{0712}'), Script::Syriac); // beth
    assert_eq!(script_of('\u{0860}'), Script::Syriac); // Syriac Supplement
    assert_eq!(script_of('\u{A98F}'), Script::Javanese); // ka
    assert_eq!(script_of('\u{11107}'), Script::Chakma); // ka
    assert_eq!(script_of('\u{112BA}'), Script::Khudawadi); // ka
    assert_eq!(script_of('\u{1168A}'), Script::Takri); // ka
    assert_eq!(script_of('\u{1E900}'), Script::Adlam);
    assert_eq!(script_of('\u{0840}'), Script::Mandaic);
    assert_eq!(script_of('\u{11080}'), Script::Kaithi);
    // Common code points and unassigned ones in those blocks stay
    // Other: Javanese pangrangkep, and a gap in the Syriac block.
    assert_eq!(script_of('\u{A9CF}'), Script::Other);
    assert_eq!(script_of('\u{070E}'), Script::Other);
    // So do the Common code points in the blocks of the older
    // buckets, and the unassigned and private-use code points.
    assert_eq!(script_of('\u{0640}'), Script::Other);
    assert_eq!(script_of('\u{0964}'), Script::Other);
    assert_eq!(script_of('\u{0378}'), Script::Other);
    assert_eq!(script_of('\u{E000}'), Script::Other);
    assert_eq!(script_of('\u{10FFFF}'), Script::Other);
}

#[test]
fn script_code_reads_the_script_property() {
    assert_eq!(script_code('\u{A98F}'), *b"Java");
    assert_eq!(script_code('\u{0640}'), *b"Zyyy");
    assert_eq!(script_code('\u{064B}'), *b"Zinh");
    assert_eq!(script_code('\u{0964}'), *b"Zyyy");
    assert_eq!(script_code('\u{10FFFF}'), *b"Zzzz");
    // A Unicode 18.0 script.
    assert_eq!(script_code('\u{3D000}'), *b"Seal");
}

#[test]
fn common_and_inherited_characters_take_the_run_script() {
    // The Script property's Common and Inherited code points: the
    // tatweel, the Arabic harakat, the dandas, Javanese pangrangkep,
    // CJK punctuation.
    for c in ['\u{0640}', '\u{064B}', '\u{0964}', '\u{A9CF}', '\u{3001}'] {
        assert!(is_common_or_inherited(c), "U+{:04X}", c as u32);
    }
    // Common punctuation and digits, the combining marks, the joiners
    // and selectors, and the Cyrillic combining half marks of the
    // hand-listed combining mark blocks.
    for c in [
        ' ', '1', '\u{0301}', '\u{200D}', '\u{25CC}', '\u{FE0F}', '\u{FE2E}',
    ] {
        assert!(is_common_or_inherited(c), "U+{:04X}", c as u32);
    }
    // Unknown: unassigned and private-use code points, which
    // HarfBuzz's script guess skips too.
    for c in ['\u{0378}', '\u{E000}', '\u{F0000}', '\u{10FFFF}'] {
        assert!(is_common_or_inherited(c), "U+{:04X}", c as u32);
    }
    // Letters of a script, the ordinal indicators (Latin) included,
    // and the default ignorables of a script of their own.
    for c in [
        'a', '\u{00AA}', '\u{00BA}', '\u{1EF7}', '\u{0628}', '\u{08A0}', '\u{0915}', '\u{A98F}',
        '\u{17B4}', '\u{180B}',
    ] {
        assert!(!is_common_or_inherited(c), "U+{:04X}", c as u32);
    }
}

#[test]
fn new_buckets_route_to_harfbuzz_shapers() {
    // `hb_ot_shaper_categorize`: Syriac takes the Arabic shaper, the
    // other added buckets the Universal Shaping Engine.
    assert!(!Script::Syriac.is_use());
    for script in [
        Script::Javanese,
        Script::Chakma,
        Script::Khudawadi,
        Script::Takri,
        Script::Adlam,
        Script::Mandaic,
        Script::Kaithi,
        Script::Grantha,
        Script::Seal,
    ] {
        assert!(script.is_use(), "{script:?}");
        assert!(!script.is_indic(), "{script:?}");
    }
    // `has_arabic_joining`.
    for script in [
        Script::Arabic,
        Script::Syriac,
        Script::Adlam,
        Script::Mandaic,
        Script::Mongolian,
        Script::NKo,
        Script::PhagsPa,
        Script::Sogdian,
    ] {
        assert!(script.has_arabic_joining(), "{script:?}");
    }
    for script in [Script::Javanese, Script::Hebrew, Script::OldSogdian] {
        assert!(!script.has_arabic_joining(), "{script:?}");
    }
}
