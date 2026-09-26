//! Per-script entry points. Each routes one script's run through
//! [`shape_use`](super::shape_use) with its script tags and feature
//! chain; N'Ko runs its own joining-form pass instead.

use alloc::vec::Vec;

use super::{
    shape_use, BALINESE_SCRIPT_PRIORITY, BRAHMI_SCRIPT_PRIORITY, BUGINESE_SCRIPT_PRIORITY,
    CHAM_SCRIPT_PRIORITY, HANGUL_FEATURES, HANGUL_SCRIPT_PRIORITY, KHOJKI_SCRIPT_PRIORITY,
    LEPCHA_SCRIPT_PRIORITY, LIMBU_SCRIPT_PRIORITY, MODI_SCRIPT_PRIORITY, MYANMAR_BASIC_FEATURES,
    MYANMAR_SCRIPT_PRIORITY, MYANMAR_TOPOGRAPHICAL_FEATURES, NKO_SCRIPT_PRIORITY,
    SHARADA_SCRIPT_PRIORITY, SUNDANESE_SCRIPT_PRIORITY, TAI_THAM_SCRIPT_PRIORITY,
    TIRHUTA_SCRIPT_PRIORITY, USE_BASIC_FEATURES, USE_TOPOGRAPHICAL_FEATURES,
};
use crate::buffer::{ClusterLevel, Glyph};
use crate::shape::JoinerTable;
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;

/// Entry point for Myanmar runs. Routes through the generic USE
/// dispatch with the Myanmar script-tag priority and the Myanmar-
/// specific feature chain (adds `rphf` for kinzi and keeps
/// `pref`/`blwf`/`pstf`/`cjct` for medial + subjoined handling).
pub fn shape_myanmar(
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
        MYANMAR_SCRIPT_PRIORITY,
        MYANMAR_BASIC_FEATURES,
        MYANMAR_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Myanmar,
    );
}

/// Entry point for Hangul runs, specifically Jamo (Old Hangul)
/// decomposed text. Precomposed syllables still flow through the
/// default path in [`crate::shape`]; only runs containing at least
/// one Jamo codepoint land here. The feature chain drives
/// `ljmo`/`vjmo`/`tjmo` so Leading / Vowel / Trailing jamo pick
/// their positional variant glyphs; `ccmp` and the other default
/// features run once, in the default pass after this.
///
/// An `<L,V>` or `<L,V,T>` jamo sequence that did not compose into a
/// precomposed syllable forms one cluster at the grapheme levels, as
/// HarfBuzz's Hangul shaper merges it (`merge_out_grapheme_clusters`).
pub fn shape_hangul(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    level: ClusterLevel,
) {
    if glyphs.len() == codepoints.len() {
        merge_jamo_syllables(codepoints, glyphs, level);
    }
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        HANGUL_SCRIPT_PRIORITY,
        HANGUL_FEATURES,
        &[],
        false,
        level,
        JoinerTable::Default,
    );
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

/// Entry point for N'Ko runs. N'Ko is RTL alphabetic with cursive
/// joining of the same shape as Arabic: every letter has up to four
/// positional forms (`isol`/`init`/`medi`/`fina`) selected by the
/// shared joining state machine in [`crate::unicode::joining`]. The
/// shaper:
///
/// 1. Runs `locl` and `ccmp` as one stage, so localized forms and
///    any precomposed N'Ko diphthongs in the font's composition lookup
///    settle before the positional pass (HarfBuzz's USE order).
/// 2. Computes a per-codepoint joining-form vector via the shared
///    Arabic state machine. N'Ko's joining types live in the same
///    [`JoiningType`](crate::unicode::joining::JoiningType) table.
/// 3. Applies `isol`/`init`/`medi`/`fina` masked by the joining-form
///    vector under the `nko ` script tag. Noto Sans NKo registers
///    `init`/`medi`/`fina` (no `isol` lookup: the unfeatured glyph
///    is the isolated form already), so the masked dispatcher
///    naturally no-ops on `isol` positions.
/// 4. Lets the generic default-GSUB pass run `calt` / `liga` after
///    the shaper returns. Tone-mark zeroing (mark advances -> 0)
///    happens in the generic pipeline.
pub fn shape_nko(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    let context = crate::ot::arabic::JoiningContext::NONE;
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
    context: crate::ot::arabic::JoiningContext,
) {
    if codepoints.is_empty() || glyphs.is_empty() {
        return;
    }
    let Some(gsub) = gsub else {
        return;
    };

    // 1. locl + ccmp first, as one stage: HarfBuzz shapes N'Ko with
    //    its USE shaper, whose first stage runs both before the
    //    positional features see the glyph stream.
    crate::shape::apply_gsub_features_merged(
        gsub,
        glyphs,
        gdef,
        &[],
        &[*b"locl", *b"ccmp"],
        NKO_SCRIPT_PRIORITY,
        JoinerTable::Use,
    );

    // 2. Compute the joining-form vector using the shared Arabic
    //    state machine. The vector is aligned with `codepoints`;
    //    after `ccmp` the glyph count may have shifted (a multi-sub
    //    in `ccmp` would split one glyph into two), so we only run
    //    the masked positional pass when lengths still align.
    let types: Vec<crate::unicode::joining::JoiningType> = codepoints
        .iter()
        .map(|&c| crate::unicode::joining::joining_type(c))
        .collect();
    let forms = crate::ot::arabic::assign_from_types_in_context(&types, context);

    if glyphs.len() == forms.len() {
        for (form, tag) in [
            (crate::ot::arabic::JoiningForm::Isol, *b"isol"),
            (crate::ot::arabic::JoiningForm::Init, *b"init"),
            (crate::ot::arabic::JoiningForm::Medi, *b"medi"),
            (crate::ot::arabic::JoiningForm::Fina, *b"fina"),
        ] {
            let mask: Vec<bool> = forms.iter().map(|&f| f == form).collect();
            crate::shape::apply_gsub_feature_masked(
                gsub,
                glyphs,
                gdef,
                tag,
                NKO_SCRIPT_PRIORITY,
                &mask,
                JoinerTable::Use.joiners(tag),
            );
        }
    }

    // calt / liga fire in the generic default-GSUB pass after this
    // shaper returns; nothing else to drive here.
}

/// Entry point for Buginese runs. Brahmic: pre-base reorder fires
/// for sara e (U+1A19). Uses the full USE feature chain.
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
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Tai Tham (Lanna) runs.
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
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Balinese runs.
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
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Sundanese runs.
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
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Lepcha runs.
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
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Limbu runs.
pub fn shape_limbu(
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
        LIMBU_SCRIPT_PRIORITY,
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Cham runs.
pub fn shape_cham(
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
        CHAM_SCRIPT_PRIORITY,
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Brahmi runs. Brahmic: full USE feature chain.
/// SMP block (U+11000..U+1107F). No pre-base reorder fires (no
/// pre-base vowel signs in Brahmi); included on the consonant
/// shaping path so virama / vowel-sign substitutions still see
/// the syllable structure.
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
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Sharada runs.
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
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Khojki runs.
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
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Tirhuta runs. Tirhuta has pre-base vowel signs
/// (sign-e U+114B9, sign-o U+114BC) that the USE pre-base reorder
/// fires for.
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
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
}

/// Entry point for Modi runs.
pub fn shape_modi(
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
        MODI_SCRIPT_PRIORITY,
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
        level,
        JoinerTable::Use,
    );
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
