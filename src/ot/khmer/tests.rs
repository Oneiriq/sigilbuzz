//! Khmer shaper tests: reordering and masks without a font. The
//! shaping results against HarfBuzz live in
//! `tests/khmer_harfbuzz_parity.rs`.

use super::*;
use alloc::vec;

fn run_without_font(text: &str, level: ClusterLevel) -> Vec<(u32, u32)> {
    let cps: Vec<char> = text.chars().collect();
    let mut glyphs: Vec<Glyph> = text
        .char_indices()
        .map(|(i, c)| Glyph::new(c as u32, i as u32))
        .collect();
    let run = KhmerRun {
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

#[test]
fn coeng_ro_moves_to_the_syllable_start() {
    // Ka, coeng, ro, sign aa.
    let out = run_without_font("\u{1780}\u{17D2}\u{179A}\u{17B6}", ClusterLevel::Characters);
    assert_eq!(
        out,
        vec![(0x17D2, 3), (0x179A, 6), (0x1780, 0), (0x17B6, 9)]
    );
    let out = run_without_font(
        "\u{1780}\u{17D2}\u{179A}\u{17B6}",
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(
        out,
        vec![(0x17D2, 0), (0x179A, 0), (0x1780, 0), (0x17B6, 9)]
    );
}

#[test]
fn pre_base_vowel_moves_and_broken_clusters_get_a_circle() {
    // Sign e, then ka, sign e: the first sign is a broken cluster.
    let out = run_without_font("\u{17C1}\u{1780}\u{17C1}", ClusterLevel::MonotoneCharacters);
    assert_eq!(
        out,
        vec![(0x17C1, 0), (0x25CC, 0), (0x17C1, 3), (0x1780, 3)]
    );
}

#[test]
fn reorder_sets_harfbuzz_masks() {
    let mut glyphs: Vec<Glyph> = (0..5).map(|i| Glyph::new(i, i)).collect();
    let cats = [cat::C, cat::H, cat::RA, cat::H, cat::C];
    let mut info: Vec<GlyphInfo> = cats
        .iter()
        .map(|&category| GlyphInfo {
            category,
            ..GlyphInfo::default()
        })
        .collect();
    let masks = Masks { cfar: true };
    reorder_consonant_syllable(
        &mut glyphs,
        &mut info,
        0..5,
        masks,
        ClusterLevel::Characters,
    );
    let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
    assert_eq!(ids, [1, 2, 0, 3, 4]);
    let post = BLWF | ABVF | PSTF;
    let got: Vec<u32> = info.iter().map(|g| g.mask).collect();
    assert_eq!(got, [post | PREF, post | PREF, 0, post | CFAR, post | CFAR]);
}

#[test]
fn feature_table_keeps_harfbuzz_flags() {
    let basic = F::MANUAL_JOINERS.union(F::PER_SYLLABLE);
    for f in &KHMER_FEATURES[..KHMER_BASIC_FEATURES] {
        assert_eq!(f.flags, basic, "{:?}", f.tag);
    }
    for f in &KHMER_FEATURES[KHMER_BASIC_FEATURES..] {
        assert_eq!(f.flags, F::GLOBAL_MANUAL_JOINERS, "{:?}", f.tag);
    }
    let tags: Vec<[u8; 4]> = KHMER_FEATURES.iter().map(|f| f.tag).collect();
    assert_eq!(
        tags,
        [
            *b"pref", *b"blwf", *b"abvf", *b"pstf", *b"cfar", *b"pres", *b"abvs", *b"blws",
            *b"psts"
        ]
    );
}
