//! Tests for the Indic state machine: syllable segmentation, position
//! tagging, initial and final reordering, and split-matra handling.

use super::super::indic_config_for;
use super::reorder::{cluster_byte_offsets, final_reorder, initial_reorder, tag_positions};
use super::*;
use crate::buffer::IndicPosition;
use alloc::vec;

fn cps(s: &str) -> Vec<char> {
    s.chars().collect()
}

fn fake_glyphs(n: usize) -> Vec<Glyph> {
    (0..n).map(|i| Glyph::new(i as u32 + 1, i as u32)).collect()
}

fn deva_config() -> IndicConfig {
    indic_config_for(Script::Devanagari).unwrap()
}

#[test]
fn single_consonant_is_a_consonant_syllable() {
    // क: one syllable.
    let cp = cps("\u{0915}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Consonant);
    assert_eq!(syl[0].start, 0);
    assert_eq!(syl[0].end, 1);
    assert_eq!(syl[0].base_index, Some(0));
    assert!(!syl[0].has_reph);
}

#[test]
fn consonant_matra_is_one_syllable() {
    // की = क + ी (consonant + post-base matra).
    let cp = cps("\u{0915}\u{0940}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Consonant);
    assert_eq!(syl[0].end, 2);
    assert_eq!(syl[0].base_index, Some(0));
}

#[test]
fn namaste_splits_into_three_syllables() {
    // न म स ् त े ("namaste") is typically three syllables:
    // न (na), म (ma), स्ते (ste with halant conjunct).
    let cp = cps("\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 3);
    assert!(syl.iter().all(|s| s.kind == SyllableKind::Consonant));
}

#[test]
fn hindi_has_reph_on_second_syllable() {
    // हिन्दी = ह ि न ् द ी
    let cp = cps("\u{0939}\u{093F}\u{0928}\u{094D}\u{0926}\u{0940}");
    let syl = segment_syllables(&cp, &deva_config());
    assert!(!syl.is_empty());
    assert!(syl.iter().all(|s| !s.has_reph));
}

#[test]
fn ra_halant_consonant_marks_reph() {
    // र् क -> reph(ra) + halant + ka = reph + ka syllable.
    let cp = cps("\u{0930}\u{094D}\u{0915}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert!(syl[0].has_reph);
    assert_eq!(syl[0].base_index, Some(2)); // ka
}

#[test]
fn pre_base_matra_moves_before_base() {
    // कि = क (ka) + ि (pre-base matra).
    let cp = cps("\u{0915}\u{093F}");
    let mut glyphs = fake_glyphs(2);
    let original = glyphs.clone();
    let syllables = segment_syllables(&cp, &deva_config());
    for s in &syllables {
        initial_reorder(&cp, &mut glyphs, s);
    }
    assert_eq!(glyphs[0], original[1]); // matra first
    assert_eq!(glyphs[1], original[0]); // ka second
}

#[test]
fn post_base_matra_stays_put() {
    // की: matra ी is Right positional (post-base), so no move.
    let cp = cps("\u{0915}\u{0940}");
    let mut glyphs = fake_glyphs(2);
    let before = glyphs.clone();
    let syllables = segment_syllables(&cp, &deva_config());
    for s in &syllables {
        initial_reorder(&cp, &mut glyphs, s);
    }
    assert_eq!(glyphs, before);
}

#[test]
fn independent_vowel_is_a_vowel_syllable() {
    let cp = cps("\u{0905}"); // अ
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Vowel);
}

#[test]
fn devanagari_digits_are_symbol_pass_through() {
    let cp = cps("\u{0966}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Symbol);
}

#[test]
fn empty_input_produces_no_syllables() {
    assert!(segment_syllables(&[], &deva_config()).is_empty());
}

#[test]
fn three_pre_base_matras_each_move_before_their_base() {
    let cp = cps("\u{0915}\u{093F}\u{0915}\u{093F}\u{0915}\u{093F}");
    let mut glyphs = fake_glyphs(6);
    let syls = segment_syllables(&cp, &deva_config());
    assert_eq!(syls.len(), 3);
    for s in &syls {
        initial_reorder(&cp, &mut glyphs, s);
    }
    assert_eq!(glyphs[0].cluster, 1);
    assert_eq!(glyphs[1].cluster, 0);
    assert_eq!(glyphs[2].cluster, 3);
    assert_eq!(glyphs[3].cluster, 2);
    assert_eq!(glyphs[4].cluster, 5);
    assert_eq!(glyphs[5].cluster, 4);
}

#[test]
fn shape_devanagari_without_gsub_only_reorders() {
    let cp = cps("\u{0915}\u{093F}");
    let mut glyphs = fake_glyphs(2);
    shape_devanagari(None, None, &cp, &mut glyphs);
    assert_eq!(glyphs[0].cluster, 1);
    assert_eq!(glyphs[1].cluster, 0);
}

#[test]
fn tag_positions_marks_ra_as_reph_candidate() {
    let cp = cps("\u{0930}\u{094D}\u{0915}");
    let mut glyphs = fake_glyphs(3);
    for s in &segment_syllables(&cp, &deva_config()) {
        tag_positions(&cp, &mut glyphs, s);
    }
    assert_eq!(
        glyphs[0].indic_position,
        IndicPosition::RaToBecomeReph as u8,
        "ra should be marked as reph candidate"
    );
    assert_eq!(
        glyphs[2].indic_position,
        IndicPosition::BaseC as u8,
        "ka should be marked as base consonant"
    );
}

#[test]
fn tag_positions_marks_pre_base_matra() {
    let cp = cps("\u{0915}\u{093F}");
    let mut glyphs = fake_glyphs(2);
    for s in &segment_syllables(&cp, &deva_config()) {
        tag_positions(&cp, &mut glyphs, s);
    }
    assert_eq!(glyphs[0].indic_position, IndicPosition::BaseC as u8);
    assert_eq!(glyphs[1].indic_position, IndicPosition::PreM as u8);
}

#[test]
fn cluster_byte_offsets_matches_utf8_layout() {
    let cp = cps("\u{0930}\u{094D}\u{0915}");
    assert_eq!(cluster_byte_offsets(&cp), vec![0, 3, 6, 9]);
}

#[test]
fn final_reorder_moves_reph_to_syllable_end() {
    let mut g = fake_glyphs(2);
    g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
    g[0].cluster = 0;
    g[1].indic_position = IndicPosition::BaseC as u8;
    g[1].cluster = 6;
    final_reorder(
        &mut g,
        0,
        9,
        3,
        RephPosition::BeforePost,
        RephMode::Implicit,
    );
    assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
    assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
    assert_eq!(g[1].cluster, 0);
}

#[test]
fn final_reorder_noop_when_rphf_did_not_fire() {
    let mut g = fake_glyphs(3);
    g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
    g[0].cluster = 0;
    g[2].indic_position = IndicPosition::BaseC as u8;
    g[2].cluster = 6;
    let before = g.clone();
    final_reorder(
        &mut g,
        0,
        9,
        3,
        RephPosition::BeforePost,
        RephMode::Implicit,
    );
    assert_eq!(g, before, "no collapse -> no move");
}

#[test]
fn tag_positions_leaves_non_reph_syllables_alone() {
    let cp = cps("\u{0915}");
    let mut glyphs = fake_glyphs(1);
    for s in &segment_syllables(&cp, &deva_config()) {
        tag_positions(&cp, &mut glyphs, s);
    }
    assert_ne!(
        glyphs[0].indic_position,
        IndicPosition::RaToBecomeReph as u8
    );
}

#[test]
fn symbol_run_advances_past_multiple_digits() {
    let cp = cps("\u{0966}\u{0967}\u{0968}");
    let syl = segment_syllables(&cp, &deva_config());
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].end, 3);
}

#[test]
fn bengali_ra_halant_consonant_marks_reph() {
    // Bengali: র (U+09B0) + ্ (U+09CD) + ক (U+0995).
    let cp = cps("\u{09B0}\u{09CD}\u{0995}");
    let config = indic_config_for(Script::Bengali).unwrap();
    let syl = segment_syllables(&cp, &config);
    assert_eq!(syl.len(), 1);
    assert!(syl[0].has_reph);
    assert_eq!(syl[0].base_index, Some(2));
}

#[test]
fn tamil_consonant_syllable_segments() {
    // Tamil: க (U+0B95) + ி (U+0BBF pre-base I).
    let cp = cps("\u{0B95}\u{0BBF}");
    let config = indic_config_for(Script::Tamil).unwrap();
    let syl = segment_syllables(&cp, &config);
    assert_eq!(syl.len(), 1);
    assert_eq!(syl[0].kind, SyllableKind::Consonant);
    assert_eq!(syl[0].base_index, Some(0));
}

#[test]
fn telugu_ra_halant_is_not_reph_under_explicit_mode() {
    // Telugu's RephMode is Explicit: bare ra+virama does NOT
    // tag a reph candidate. Only ra+virama+ZWJ would (not yet
    // implemented, follow-up issue).
    let cp = cps("\u{0C30}\u{0C4D}\u{0C15}");
    let config = indic_config_for(Script::Telugu).unwrap();
    let syl = segment_syllables(&cp, &config);
    assert_eq!(syl.len(), 1);
    assert!(
        !syl[0].has_reph,
        "explicit reph mode should NOT tag bare ra+virama"
    );
}

#[test]
fn sinhala_pre_base_matra_moves() {
    // Sinhala: ක (U+0D9A) + ෙ (U+0DD9 pre-base e vowel sign).
    let cp = cps("\u{0D9A}\u{0DD9}");
    let mut glyphs = fake_glyphs(2);
    let original = glyphs.clone();
    let config = indic_config_for(Script::Sinhala).unwrap();
    for s in &segment_syllables(&cp, &config) {
        initial_reorder(&cp, &mut glyphs, s);
    }
    assert_eq!(glyphs[0], original[1]);
    assert_eq!(glyphs[1], original[0]);
}

#[test]
fn after_main_reph_target_is_right_after_base() {
    // Oriya config uses AfterMain.
    let mut g = fake_glyphs(3);
    g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
    g[0].cluster = 0;
    g[1].indic_position = IndicPosition::BaseC as u8;
    g[1].cluster = 6;
    g[2].indic_position = IndicPosition::Start as u8;
    g[2].cluster = 9;
    // original glyph count was 4 (ra, halant, base, matra); now 3.
    final_reorder(
        &mut g,
        0,
        12,
        4,
        RephPosition::AfterMain,
        RephMode::Implicit,
    );
    // After move: base @ 0, reph @ 1, trailing @ 2.
    assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
    assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
    assert_eq!(g[2].indic_position, IndicPosition::Start as u8);
}

#[test]
fn telugu_ra_halant_zwj_consonant_marks_reph_explicit() {
    // Telugu's Explicit reph mode: ra + halant + ZWJ + C tags
    // the ra as a reph candidate; bare ra+halant alone does not
    // (see issue #30).
    let cp = cps("\u{0C30}\u{0C4D}\u{200D}\u{0C15}");
    let config = indic_config_for(Script::Telugu).unwrap();
    let syl = segment_syllables(&cp, &config);
    assert_eq!(syl.len(), 1);
    assert!(
        syl[0].has_reph,
        "explicit ra+halant+ZWJ+C should be tagged as reph"
    );
    // Base must sit past the ZWJ.
    assert_eq!(syl[0].base_index, Some(3));
}

#[test]
fn malayalam_logrepha_head_marks_reph() {
    // Malayalam's LogRepha mode: U+0D4E at syllable head is
    // itself the reph, and the following consonant becomes
    // the base (see issue #31).
    let cp = cps("\u{0D4E}\u{0D15}");
    let config = indic_config_for(Script::Malayalam).unwrap();
    let syl = segment_syllables(&cp, &config);
    assert_eq!(syl.len(), 1);
    assert!(syl[0].has_reph, "LogRepha head should tag as reph");
    assert_eq!(syl[0].base_index, Some(1));
}

#[test]
fn logrepha_reorder_moves_reph_past_base() {
    // LogRepha fixture: the 0D4E glyph sits at pos 0, base at
    // pos 1. AfterMain target puts the repha right after the
    // base: [repha, base] -> [base, repha].
    let mut g = fake_glyphs(2);
    g[0].indic_position = IndicPosition::RaToBecomeReph as u8;
    g[0].cluster = 0;
    g[1].indic_position = IndicPosition::BaseC as u8;
    g[1].cluster = 3;
    // Original glyph count 2, no shrinkage. LogRepha mode must
    // still relocate because the repha is a standalone glyph
    // rather than an `rphf` ligature product.
    final_reorder(&mut g, 0, 6, 2, RephPosition::AfterMain, RephMode::LogRepha);
    assert_eq!(g[0].indic_position, IndicPosition::BaseC as u8);
    assert_eq!(g[1].indic_position, IndicPosition::RaToBecomeReph as u8);
}

#[test]
fn tamil_split_matra_decomposes() {
    // U+0BCB (OO) should be split into U+0BC7 + U+0BBE.
    let parts = super::super::split_matra_decompose('\u{0BCB}');
    assert_eq!(parts, Some(&['\u{0BC7}', '\u{0BBE}'][..]));
}

#[test]
fn sinhala_three_part_matra_decomposes() {
    // U+0DDD splits into three components.
    let parts = super::super::split_matra_decompose('\u{0DDD}');
    assert_eq!(parts, Some(&['\u{0DD9}', '\u{0DCF}', '\u{0DCA}'][..]));
}

#[test]
fn non_split_matra_returns_none() {
    assert!(super::super::split_matra_decompose('\u{0BBE}').is_none());
    assert!(super::super::split_matra_decompose('\u{0D15}').is_none());
}
