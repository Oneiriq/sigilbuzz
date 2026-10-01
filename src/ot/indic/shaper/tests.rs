//! Indic shaper tests without a font: reordering, masks, and linear
//! time on hostile runs. The shaping results against HarfBuzz live in
//! `tests/indic_harfbuzz_parity.rs`.

use super::*;
use crate::ot::indic::indic_config_for;
use alloc::vec;

const KA: char = '\u{0915}';
const HALANT: char = '\u{094D}';
const SIGN_I: char = '\u{093F}';
const RA: char = '\u{0930}';
const ZWJ: char = '\u{200D}';
const ZWNJ: char = '\u{200C}';

fn run(text: &[char], script: Script, level: ClusterLevel) -> Vec<(u32, u32)> {
    let mut glyphs: Vec<Glyph> = text
        .iter()
        .enumerate()
        .map(|(i, &c)| Glyph::new(c as u32, i as u32))
        .collect();
    let Some(config) = indic_config_for(script) else {
        return Vec::new();
    };
    let run = IndicRun {
        gsub: None,
        gdef: None,
        level,
        features: &[],
        vertical: false,
        dotted_circle: Some(0x25CC),
        virama_glyph: None,
    };
    shape(&run, &config, text, &mut glyphs);
    glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect()
}

fn ids(out: &[(u32, u32)]) -> Vec<u32> {
    out.iter().map(|&(g, _)| g).collect()
}

#[test]
fn left_matra_moves_before_the_base_and_merges() {
    let out = run(
        &[KA, SIGN_I],
        Script::Devanagari,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(out, vec![(SIGN_I as u32, 0), (KA as u32, 0)]);
    let out = run(&[KA, SIGN_I], Script::Devanagari, ClusterLevel::Characters);
    assert_eq!(out, vec![(SIGN_I as u32, 1), (KA as u32, 0)]);
}

#[test]
fn left_matra_stays_after_a_halant_zwj() {
    // Ka,H,ZWJ,ka,i: the matra goes before the first ka, since a ZWJ
    // follows the halant.
    let text = [KA, HALANT, ZWJ, KA, SIGN_I];
    let out = run(&text, Script::Devanagari, ClusterLevel::Characters);
    assert_eq!(ids(&out), [SIGN_I, KA, HALANT, ZWJ, KA].map(|c| c as u32));
    // With ZWNJ the syllable ends at it, and the matra stays with the
    // second ka.
    let text = [KA, HALANT, ZWNJ, KA, SIGN_I];
    let out = run(&text, Script::Devanagari, ClusterLevel::Characters);
    assert_eq!(ids(&out), [KA, HALANT, ZWNJ, SIGN_I, KA].map(|c| c as u32));
}

#[test]
fn broken_matra_gets_a_dotted_circle() {
    let out = run(
        &[SIGN_I],
        Script::Devanagari,
        ClusterLevel::MonotoneCharacters,
    );
    assert_eq!(out, vec![(SIGN_I as u32, 0), (0x25CC, 0)]);
}

#[test]
fn no_reph_without_the_rphf_feature() {
    // Without GSUB there is no `rphf`, so Ra,H,ka keeps its order.
    let text = [RA, HALANT, KA];
    let out = run(&text, Script::Devanagari, ClusterLevel::Characters);
    assert_eq!(ids(&out), [RA, HALANT, KA].map(|c| c as u32));
}

#[test]
fn long_zwnj_run_before_a_matra_stays_linear() {
    // Ka, 200000 ZWNJ, i is one syllable. Walking back from each ZWNJ
    // to the consonant, as HarfBuzz does, would take about 2e10 steps.
    const N: usize = 200_000;
    let mut text = vec![KA];
    text.extend(core::iter::repeat(ZWNJ).take(N));
    text.push(SIGN_I);
    let out = run(&text, Script::Devanagari, ClusterLevel::MonotoneCharacters);
    assert_eq!(out.len(), N + 2);
    assert_eq!(out[0].0, SIGN_I as u32);
    assert_eq!(out[1].0, KA as u32);
}

#[test]
fn many_left_matras_across_many_half_forms_stay_linear() {
    // (Ka,H) x M, ka, then K matras i: after initial reordering the
    // matras lead the syllable, and final reordering moves each one
    // after the last halant, past all 2M glyphs before it.
    const M: usize = 20_000;
    const K: usize = 20_000;
    let mut text = Vec::new();
    for _ in 0..M {
        text.push(KA);
        text.push(HALANT);
    }
    text.push(KA);
    text.extend(core::iter::repeat(SIGN_I).take(K));
    let out = run(&text, Script::Devanagari, ClusterLevel::MonotoneCharacters);
    assert_eq!(out.len(), 2 * M + 1 + K);
    let got = ids(&out);
    assert!(got[..2 * M]
        .chunks(2)
        .all(|p| p == [KA as u32, HALANT as u32]));
    assert!(got[2 * M..2 * M + K].iter().all(|&g| g == SIGN_I as u32));
    assert_eq!(got[2 * M + K], KA as u32);
}

#[test]
fn feature_table_keeps_harfbuzz_flags() {
    let tags: Vec<[u8; 4]> = INDIC_FEATURES.iter().map(|f| f.tag).collect();
    assert_eq!(
        tags,
        [
            *b"nukt", *b"akhn", *b"rphf", *b"rkrf", *b"pref", *b"blwf", *b"abvf", *b"half",
            *b"pstf", *b"vatu", *b"cjct", *b"init", *b"pres", *b"abvs", *b"blws", *b"psts",
            *b"haln"
        ]
    );
    for f in &INDIC_FEATURES {
        assert!(f.flags.contains(F::PER_SYLLABLE), "{:?}", f.tag);
        assert!(f.flags.contains(F::MANUAL_JOINERS), "{:?}", f.tag);
    }
    let masked: Vec<[u8; 4]> = INDIC_FEATURES
        .iter()
        .filter(|f| !f.flags.contains(F::GLOBAL))
        .map(|f| f.tag)
        .collect();
    assert_eq!(
        masked,
        [*b"rphf", *b"pref", *b"blwf", *b"abvf", *b"half", *b"pstf", *b"init"]
    );
}
