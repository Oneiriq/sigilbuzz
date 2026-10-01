//! Myanmar shaper tests: reordering and clusters without a font. The
//! shaping results against HarfBuzz live in
//! `tests/myanmar_harfbuzz_parity.rs`.

use super::*;
use alloc::string::String;
use alloc::vec;

/// Shapes `text` without a font: glyph ids are code points, clusters
/// byte offsets.
fn run_without_font(text: &str, level: ClusterLevel) -> Vec<(u32, u32)> {
    let cps: Vec<char> = text.chars().collect();
    let mut glyphs: Vec<Glyph> = text
        .char_indices()
        .map(|(i, c)| Glyph::new(c as u32, i as u32))
        .collect();
    let run = MyanmarRun {
        gsub: None,
        gdef: None,
        level,
        features: &[],
        vertical: false,
        dotted_circle: Some(0x25CC),
    };
    shape(&run, &cps, &mut glyphs);
    glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect()
}

const MC: ClusterLevel = ClusterLevel::MonotoneCharacters;
const CH: ClusterLevel = ClusterLevel::Characters;

#[test]
fn kinzi_goes_after_the_base_and_sign_e_before_it() {
    // Nga, asat, virama, ka, sign e.
    let text = "\u{1004}\u{103A}\u{1039}\u{1000}\u{1031}";
    assert_eq!(
        run_without_font(text, CH),
        vec![
            (0x1031, 12),
            (0x1000, 9),
            (0x1004, 0),
            (0x103A, 3),
            (0x1039, 6)
        ]
    );
    assert_eq!(
        run_without_font(text, MC),
        vec![
            (0x1031, 0),
            (0x1000, 0),
            (0x1004, 0),
            (0x103A, 0),
            (0x1039, 0)
        ]
    );
}

#[test]
fn medial_ra_goes_before_the_base() {
    // Ka, medial ra, medial wa.
    let text = "\u{1000}\u{103C}\u{103D}";
    assert_eq!(
        run_without_font(text, CH),
        vec![(0x103C, 3), (0x1000, 0), (0x103D, 6)]
    );
    assert_eq!(
        run_without_font(text, MC),
        vec![(0x103C, 0), (0x1000, 0), (0x103D, 6)]
    );
}

#[test]
fn pre_base_vowels_flip() {
    // Ka, sign e, variation selector, sign e: the two signs swap, each
    // keeping its selector after it.
    let text = "\u{1000}\u{1031}\u{FE00}\u{1031}";
    assert_eq!(
        run_without_font(text, CH),
        vec![(0x1031, 9), (0x1031, 3), (0xFE00, 6), (0x1000, 0)]
    );
}

#[test]
fn below_base_vowels_take_their_marks_along() {
    // Ka, sign u, anusvara, sign aa: the anusvara sorts before the
    // sign u, and the aa stays last.
    let text = "\u{1000}\u{102F}\u{1036}\u{102C}";
    assert_eq!(
        run_without_font(text, CH),
        vec![(0x1000, 0), (0x1036, 6), (0x102F, 3), (0x102C, 9)]
    );
}

#[test]
fn broken_clusters_get_a_dotted_circle() {
    // Sign e on its own goes before its circle.
    assert_eq!(
        run_without_font("\u{1031}", MC),
        vec![(0x1031, 0), (0x25CC, 0)]
    );
    // A joiner on its own gets none.
    assert_eq!(run_without_font("\u{200D}", MC), vec![(0x200D, 0)]);
}

#[test]
fn feature_tables_keep_harfbuzz_flags() {
    let early: Vec<[u8; 4]> = EARLY_FEATURES.iter().map(|f| f.tag).collect();
    let basic: Vec<[u8; 4]> = BASIC_FEATURES.iter().map(|f| f.tag).collect();
    let all: Vec<[u8; 4]> = early.iter().chain(&basic).copied().collect();
    let public: Vec<[u8; 4]> = MYANMAR_BASIC_FEATURES.iter().map(|t| **t).collect();
    assert_eq!(all, public);
    let other: Vec<[u8; 4]> = OTHER_FEATURES.iter().map(|f| f.tag).collect();
    let public: Vec<[u8; 4]> = MYANMAR_TOPOGRAPHICAL_FEATURES.iter().map(|t| **t).collect();
    assert_eq!(other, public);
    for f in BASIC_FEATURES {
        assert!(f.flags.contains(F::MANUAL_ZWJ.union(F::PER_SYLLABLE)));
        assert!(!f.flags.contains(F::MANUAL_ZWNJ));
    }
    for f in OTHER_FEATURES {
        assert!(f.flags.contains(F::MANUAL_ZWJ) && !f.flags.contains(F::PER_SYLLABLE));
    }
    for f in EARLY_FEATURES {
        assert!(f.flags.contains(F::PER_SYLLABLE) && !f.flags.contains(F::MANUAL_ZWJ));
    }
    assert!(earlier(*b"blwf") && earlier(*b"ccmp") && !earlier(*b"pres"));
}

#[test]
fn shape_myanmar_reorders_without_a_dotted_circle() {
    let cps: Vec<char> = "\u{1031}\u{1000}\u{1031}".chars().collect();
    let mut glyphs: Vec<Glyph> = (0..3).map(|i| Glyph::new(i, i)).collect();
    shape_myanmar(None, None, &cps, &mut glyphs, MC);
    let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
    assert_eq!(ids, [0, 2, 1]);
}

#[test]
fn long_syllables_reorder_in_linear_time() {
    // Ka, 50,000 asats, and 50,000 signs e is one syllable, and every
    // sign moves past every asat.
    const N: usize = 50_000;
    let mut text = String::from("\u{1000}");
    text.extend(core::iter::repeat('\u{103A}').take(N));
    text.extend(core::iter::repeat('\u{1031}').take(N));
    for level in [MC, CH] {
        let out = run_without_font(&text, level);
        assert_eq!(out.len(), 2 * N + 1);
        assert_eq!(out[0].0, 0x1031);
        assert_eq!(out[N].0, 0x1000);
        assert_eq!(out[2 * N].0, 0x103A);
    }
}
