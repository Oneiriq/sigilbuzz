//! Per-script entry points. Each routes one script's run through the
//! Universal Shaping Engine ([`shape_use`]) with its script tags, and
//! Hangul through the Hangul shaper's GSUB stage.

use alloc::vec::Vec;

use super::{
    shape, UseRun, BALINESE_SCRIPT_PRIORITY, BRAHMI_SCRIPT_PRIORITY, BUGINESE_SCRIPT_PRIORITY,
    CHAM_SCRIPT_PRIORITY, KHOJKI_SCRIPT_PRIORITY, LEPCHA_SCRIPT_PRIORITY, LIMBU_SCRIPT_PRIORITY,
    MODI_SCRIPT_PRIORITY, NKO_SCRIPT_PRIORITY, SHARADA_SCRIPT_PRIORITY, SUNDANESE_SCRIPT_PRIORITY,
    TAI_THAM_SCRIPT_PRIORITY, TIRHUTA_SCRIPT_PRIORITY,
};
use crate::buffer::{ClusterLevel, Glyph};
use crate::ot::arabic::{assign_from_types_in_context, JoiningContext, JoiningForm};
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::joining::{joining_type, JoiningType};

/// Entry point for Hangul runs whose syllables composed already: the
/// GSUB stage of HarfBuzz's Hangul shaper (`crate::ot::hangul`), with
/// `ljmo`, `vjmo`, and `tjmo` on each `<L,V>` or `<L,V,T>` jamo
/// sequence, `calt` kept off jamo, and the default features, all in
/// one stage as HarfBuzz runs them.
///
/// Each such jamo sequence forms one cluster at the grapheme levels, as
/// HarfBuzz's Hangul shaper merges it (`merge_out_grapheme_clusters`).
/// Shaping through [`crate::shape`] also composes and decomposes
/// syllables and moves tone marks first, as HarfBuzz's preprocessing
/// does.
pub fn shape_hangul(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    if glyphs.len() != codepoints.len() {
        return;
    }
    merge_jamo_syllables(codepoints, glyphs, level);
    let jamo = crate::ot::hangul::jamo_features(codepoints);
    let run = crate::ot::hangul::HangulRun {
        gsub,
        gdef,
        features: &[],
        vertical: false,
    };
    crate::ot::hangul::shape(&run, codepoints, &jamo, glyphs);
}

/// HarfBuzz's leading, vowel, and trailing jamo ranges (`isL`, `isV`,
/// `isT` in `hb-ot-shaper-hangul.cc`).
const fn is_l(ch: char) -> bool {
    matches!(ch as u32, 0x1100..=0x115F | 0xA960..=0xA97C)
}

const fn is_v(ch: char) -> bool {
    matches!(ch as u32, 0x1160..=0x11A7 | 0xD7B0..=0xD7C6)
}

const fn is_t(ch: char) -> bool {
    matches!(ch as u32, 0x11A8..=0x11FF | 0xD7CB..=0xD7FB)
}

/// Merges each `<L,V>` / `<L,V,T>` jamo sequence of `codepoints`
/// (one glyph each) into one cluster at the grapheme levels. The text
/// reaching here already had its composable sequences composed, so
/// every such sequence is one HarfBuzz leaves decomposed.
fn merge_jamo_syllables(codepoints: &[char], glyphs: &mut [Glyph], level: ClusterLevel) {
    let mut i = 0;
    while i + 1 < codepoints.len() {
        if !(is_l(codepoints[i]) && is_v(codepoints[i + 1])) {
            i += 1;
            continue;
        }
        let end = if codepoints.get(i + 2).is_some_and(|&c| is_t(c)) {
            i + 3
        } else {
            i + 2
        };
        crate::shape::merge_grapheme_clusters(glyphs, i, end, level);
        i = end;
    }
}

/// Entry point for N'Ko runs: the Universal Shaping Engine with the
/// `nko ` script tags, whose `isol`, `init`, `medi`, and `fina` follow
/// the Arabic-style joining forms of the letters, as in HarfBuzz
/// (`setup_masks_arabic_plan`). Clusters merge at the monotone
/// characters level, the default of a Rust [`crate::Buffer`].
pub fn shape_nko(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    let context = JoiningContext::NONE;
    shape_nko_in_context(gsub, gdef, codepoints, glyphs, context);
}

/// [`shape_nko`] for a run whose surroundings are known: the first and
/// last letters join toward `context` (the buffer's pre- and
/// post-context, as in HarfBuzz's Arabic-family joining).
pub fn shape_nko_in_context(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    context: JoiningContext,
) {
    let types: Vec<JoiningType> = codepoints.iter().map(|&c| joining_type(c)).collect();
    let forms = assign_from_types_in_context(&types, context);
    let script = UseScript {
        script_priority: NKO_SCRIPT_PRIORITY,
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

/// The script facts [`shape_script`] needs: the script tags, and the
/// joining forms of a script with Arabic-style joining.
#[derive(Clone, Copy)]
pub(crate) struct UseScript<'a> {
    pub(crate) script_priority: &'a [[u8; 4]],
    pub(crate) joining: Option<&'a [JoiningForm]>,
}

/// Shapes one run with the Universal Shaping Engine under the script
/// tags `script_priority`, every GSUB feature of the run included, the
/// default ones too, with no caller features. `codepoints` is in
/// one-to-one correspondence with `glyphs` on entry. After the call
/// `glyphs` may be shorter (GSUB collapses) and reordered, and a
/// reordered glyph shares one cluster with the glyphs it moved across
/// at the monotone cluster `level`s. Broken clusters get no dotted
/// circle here. Shaping through [`crate::shape`] adds them.
pub(crate) fn shape_use(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    script_priority: &[[u8; 4]],
    level: ClusterLevel,
) {
    let script = UseScript {
        script_priority,
        joining: None,
    };
    shape_script(gsub, gdef, codepoints, glyphs, script, level);
}

/// [`shape_use`] for a script that may join Arabic-style.
pub(crate) fn shape_script(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    script: UseScript<'_>,
    level: ClusterLevel,
) {
    let run = UseRun {
        gsub,
        gdef,
        script_priority: script.script_priority,
        level,
        features: &[],
        vertical: false,
        dotted_circle: None,
        joining: script.joining,
    };
    shape(&run, codepoints, glyphs);
}

/// Entry point for Buginese runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_buginese(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        BUGINESE_SCRIPT_PRIORITY,
        level,
    );
}

/// Entry point for Tai Tham (Lanna) runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_tai_tham(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        TAI_THAM_SCRIPT_PRIORITY,
        level,
    );
}

/// Entry point for Balinese runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_balinese(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        BALINESE_SCRIPT_PRIORITY,
        level,
    );
}

/// Entry point for Sundanese runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_sundanese(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        SUNDANESE_SCRIPT_PRIORITY,
        level,
    );
}

/// Entry point for Lepcha runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_lepcha(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        LEPCHA_SCRIPT_PRIORITY,
        level,
    );
}

/// Entry point for Limbu runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_limbu(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(gsub, gdef, codepoints, glyphs, LIMBU_SCRIPT_PRIORITY, level);
}

/// Entry point for Cham runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_cham(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(gsub, gdef, codepoints, glyphs, CHAM_SCRIPT_PRIORITY, level);
}

/// Entry point for Brahmi runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_brahmi(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        BRAHMI_SCRIPT_PRIORITY,
        level,
    );
}

/// Entry point for Sharada runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_sharada(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        SHARADA_SCRIPT_PRIORITY,
        level,
    );
}

/// Entry point for Khojki runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_khojki(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        KHOJKI_SCRIPT_PRIORITY,
        level,
    );
}

/// Entry point for Tirhuta runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_tirhuta(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        TIRHUTA_SCRIPT_PRIORITY,
        level,
    );
}

/// Entry point for Modi runs: the Universal Shaping Engine with the
/// script's tags, every GSUB feature of the run included, the default
/// ones too. `codepoints` is in one-to-one correspondence with `glyphs`
/// on entry, and a reordered glyph shares one cluster with the glyphs it
/// moved across at the monotone cluster `level`s. Broken clusters get no
/// dotted circle here. Shaping through [`crate::shape`] adds them.
pub fn shape_modi(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    shape_use(gsub, gdef, codepoints, glyphs, MODI_SCRIPT_PRIORITY, level);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn merged(text: &str, level: ClusterLevel) -> Vec<u32> {
        let cps: Vec<char> = text.chars().collect();
        let mut glyphs: Vec<Glyph> = text
            .char_indices()
            .map(|(i, c)| Glyph::new(c as u32, i as u32))
            .collect();
        merge_jamo_syllables(&cps, &mut glyphs, level);
        glyphs.iter().map(|g| g.cluster).collect()
    }

    #[test]
    fn jamo_sequences_merge_at_grapheme_levels() {
        // <L,V,T> with an Extended-B T, then an <L,V> with an
        // Extended-A L, then a lone vowel.
        let text = "\u{1100}\u{1161}\u{D7CB}\u{A960}\u{1161}\u{1161}";
        assert_eq!(
            merged(text, ClusterLevel::MonotoneGraphemes),
            vec![0, 0, 0, 9, 9, 15]
        );
        assert_eq!(
            merged(text, ClusterLevel::Graphemes),
            vec![0, 0, 0, 9, 9, 15]
        );
        assert_eq!(
            merged(text, ClusterLevel::MonotoneCharacters),
            vec![0, 3, 6, 9, 12, 15]
        );
        // A trailing jamo alone or before a vowel starts nothing.
        assert_eq!(
            merged("\u{11A8}\u{1161}", ClusterLevel::MonotoneGraphemes),
            vec![0, 3]
        );
    }
}
