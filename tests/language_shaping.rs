//! Buffer language and script reach shaping, cross-checked against
//! rustybuzz with the same language and script set.
//!
//! The fonts here carry language-specific `locl` lookups: Open Sans
//! has comma-below forms under `latn`/`ROM `, Amiri has Urdu and Sindhi
//! digit forms under `arab`/`URD ` and `SND ` and a Turkish `i` under
//! `latn`/`TRK `, and Noto Sans Devanagari has Marathi and Nepali
//! letter and digit forms under `dev2`/`MAR ` and `NEP `.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Face, Font, Language, UnicodeScript};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const MYANMAR: &[u8] = include_bytes!("fonts/NotoSansMyanmar-Regular.ttf");
const SOURCE_SANS: &[u8] = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");

/// What a test sets on both engines' buffers.
#[derive(Clone, Copy, Default)]
struct Setup<'a> {
    language: Option<&'a str>,
    script: Option<(UnicodeScript, rustybuzz::Script)>,
}

fn sigil_ids(data: &[u8], text: &str, setup: Setup<'_>) -> Vec<u32> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_language(setup.language.and_then(Language::new));
    buffer.set_script(setup.script.map(|(s, _)| s));
    shape(&font, &buffer, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| g.glyph_id)
        .collect()
}

/// rustybuzz glyph ids in logical order, plus whether the run was
/// right to left.
fn rusty_ids(data: &[u8], text: &str, setup: Setup<'_>) -> (Vec<u32>, bool) {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    if let Some(lang) = setup.language {
        buffer.set_language(lang.parse().expect("rustybuzz language"));
    }
    if let Some((_, script)) = setup.script {
        buffer.set_script(script);
        buffer.set_direction(RbDirection::LeftToRight);
    }
    buffer.guess_segment_properties();
    let rtl = buffer.direction() == RbDirection::RightToLeft;
    let out = rustybuzz::shape(&face, &[], buffer);
    let mut ids: Vec<u32> = out.glyph_infos().iter().map(|g| g.glyph_id).collect();
    if rtl {
        ids.reverse();
    }
    (ids, rtl)
}

/// Asserts both engines agree. For right-to-left runs sigilbuzz may
/// emit logical or visual order, so either is accepted.
fn assert_matches_rustybuzz(data: &[u8], text: &str, setup: Setup<'_>) -> Vec<u32> {
    let sig = sigil_ids(data, text, setup);
    let (rb, rtl) = rusty_ids(data, text, setup);
    let reversed: Vec<u32> = rb.iter().rev().copied().collect();
    assert!(
        sig == rb || (rtl && sig == reversed),
        "{text:?} with language {:?}: sigilbuzz={sig:?} rustybuzz={rb:?}",
        setup.language
    );
    sig
}

fn lang(language: &str) -> Setup<'_> {
    Setup {
        language: Some(language),
        script: None,
    }
}

/// (font, text, language) cases where the language system changes
/// the output. Each must match rustybuzz and differ from shaping the
/// same text with no language.
const LANGUAGE_CHANGES_OUTPUT: &[(&[u8], &str, &str)] = &[
    // Romanian comma-below letters.
    (OPEN_SANS, "\u{0218}\u{0219}\u{021A}\u{021B} a", "ro"),
    (OPEN_SANS, "\u{0218}\u{0219}\u{021A}\u{021B} a", "ro-MD"),
    (OPEN_SANS, "\u{0218}\u{0219}\u{021A}\u{021B} a", "mo"),
    // Urdu and Sindhi extended Arabic-Indic digits.
    (AMIRI, "\u{06F4}\u{06F6}\u{06F7} \u{0628}\u{0628}", "ur"),
    (AMIRI, "\u{06F4}\u{06F6}\u{06F7} \u{0628}\u{0628}", "sd"),
    (AMIRI, "\u{06F4}\u{06F6}\u{06F7} \u{0628}\u{0628}", "ks"),
    // Turkish dotted i keeps its dot.
    (AMIRI, "fi ti", "tr"),
    (AMIRI, "fi ti", "tr-TR"),
    // HarfBuzz's private-use override names the tag directly.
    (AMIRI, "\u{06F4}\u{06F6}\u{06F7}", "x-hboturd"),
    (AMIRI, "fi", "en-x-hbottrk"),
    // Marathi la and sha, Nepali jha and digits.
    (DEVANAGARI, "\u{0932}\u{0936}", "mr"),
    (DEVANAGARI, "\u{096B}\u{096E}\u{096F} \u{091D}", "ne"),
];

#[test]
fn language_selects_lang_sys_like_rustybuzz() {
    for &(data, text, language) in LANGUAGE_CHANGES_OUTPUT {
        let with = assert_matches_rustybuzz(data, text, lang(language));
        let without = sigil_ids(data, text, Setup::default());
        assert_ne!(with, without, "{language} should change {text:?}");
    }
}

#[test]
fn languages_without_a_lang_sys_use_the_default() {
    let cases: &[(&[u8], &str, &[&str])] = &[
        (
            OPEN_SANS,
            "\u{0218}\u{0219} office",
            &["de", "en-US", "xy", "fr"],
        ),
        (
            AMIRI,
            "\u{06F4}\u{06F6} \u{0628}\u{0628}",
            &["ar", "fa", "he"],
        ),
        (DEVANAGARI, "\u{0932}\u{0936}", &["hi", "sa", "ne"]),
    ];
    for &(data, text, languages) in cases {
        let default = sigil_ids(data, text, Setup::default());
        for &language in languages {
            assert_eq!(
                assert_matches_rustybuzz(data, text, lang(language)),
                default,
                "{language} on {text:?}"
            );
        }
    }
}

#[test]
fn language_reaches_the_use_shaper() {
    // Noto Sans Myanmar's S'gaw Karen language system reshapes the
    // medial wa and ha.
    let text = "\u{1000}\u{103D} \u{1000}\u{103E}";
    let with = assert_matches_rustybuzz(MYANMAR, text, lang("ksw"));
    assert_ne!(with, sigil_ids(MYANMAR, text, Setup::default()));
}

#[test]
fn phonetic_variant_subtags_match_rustybuzz() {
    for language in ["en-fonipa", "und-fonnapa", "en-fonipa-x-foo"] {
        assert_matches_rustybuzz(SOURCE_SANS, "gag \u{0251}", lang(language));
    }
}

#[test]
fn script_override_shapes_the_whole_buffer_as_one_script() {
    let latin = Some((UnicodeScript::Latin, rustybuzz::script::LATIN));
    // Mixed text: by default sigilbuzz shapes the Arabic run with
    // joining; forced to Latin, the whole buffer takes the default
    // shaper and `latn` features, exactly as HarfBuzz does.
    let text = "Hi \u{0628}\u{0628}!";
    let forced = Setup {
        language: None,
        script: latin,
    };
    let forced_ids = assert_matches_rustybuzz(AMIRI, text, forced);
    assert_ne!(forced_ids, sigil_ids(AMIRI, text, Setup::default()));
    // Language selection follows the forced script too.
    let turkish = Setup {
        language: Some("tr"),
        script: latin,
    };
    assert_matches_rustybuzz(AMIRI, text, turkish);
}

#[test]
fn script_override_skips_the_complex_shaper() {
    let latin = Setup {
        language: None,
        script: Some((UnicodeScript::Latin, rustybuzz::script::LATIN)),
    };
    // Arabic letters under a Latin script stay unjoined.
    let arabic = "\u{0628}\u{0628}\u{0628}";
    let forced = assert_matches_rustybuzz(AMIRI, arabic, latin);
    assert_ne!(forced, sigil_ids(AMIRI, arabic, Setup::default()));
    // Devanagari under a Latin script is not reordered.
    let deva = "\u{0915}\u{093F}";
    let forced = assert_matches_rustybuzz(DEVANAGARI, deva, latin);
    assert_ne!(forced, sigil_ids(DEVANAGARI, deva, Setup::default()));
}

#[test]
fn matching_script_override_changes_nothing() {
    let arabic = Setup {
        language: None,
        script: Some((UnicodeScript::Arabic, rustybuzz::script::ARABIC)),
    };
    let text = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
    assert_eq!(
        sigil_ids(AMIRI, text, arabic),
        sigil_ids(AMIRI, text, Setup::default())
    );
    let deva = Setup {
        language: Some("mr"),
        script: Some((UnicodeScript::Devanagari, rustybuzz::script::DEVANAGARI)),
    };
    assert_matches_rustybuzz(DEVANAGARI, "\u{0932}\u{0936}\u{094D}\u{0930}", deva);
}

#[test]
fn leading_punctuation_joins_the_following_script() {
    // HarfBuzz gives a buffer the script of its first non-common
    // character, so digits and punctuation before Arabic text shape
    // under `arab`, not `latn`.
    for text in ["123 \u{0628}\u{0628}", "! \u{06F4}"] {
        assert_matches_rustybuzz(AMIRI, text, lang("ur"));
    }
    // A buffer with no script-bearing character shapes under DFLT.
    for text in ["!", "12:30", " "] {
        assert_matches_rustybuzz(AMIRI, text, lang("tr"));
    }
}
