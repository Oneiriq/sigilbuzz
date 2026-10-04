//! The feature map of the syllable-based shapers against HarfBuzz
//! 14.5.0: which features get a mask bit, and where a language
//! system's required feature runs.
//!
//! HarfBuzz (`hb_ot_map_builder_t::compile`) gives a shaper feature a
//! mask bit when the language system lists it, whatever lookups it has,
//! and the Indic, Khmer, and Universal Shaping Engine shapers read that
//! bit to find a reph or a pre-base form (`rphf`, `pref`) or to mark
//! the glyphs after a coeng and ro (`cfar`). A required feature with
//! the same tag does not list the feature. Its lookups run in that
//! feature's stage, on every glyph, with automatic joiner handling and
//! across syllables, as HarfBuzz adds them with the global mask.
//!
//! Which stages run depends on the shaper HarfBuzz picks, not on the
//! script alone: an Indic or Myanmar script whose font has only `DFLT`
//! or `latn` lookups (or `mymr` for Myanmar) takes the default shaper,
//! and a required feature then runs in that shaper's stages. A font
//! with only the misspelled `dflt` script keeps the script's shaper.
//! The Khmer shaper turns `liga` off and `clig` on after the caller's
//! features, whatever the caller asks.
//!
//! None of the vendored fonts has a required feature, so each test
//! patches one at run time, or builds a GSUB of one lookup. Every
//! expectation is HarfBuzz 14.5.0's output (uharfbuzz,
//! `guess_segment_properties`) on the same patched bytes: glyph id,
//! cluster, x advance (y advance in vertical text), x offset, y offset.

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Feature, Font};

const DEVANAGARI: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");
const MALAYALAM: &[u8] = include_bytes!("fonts/NotoSansMalayalam-Regular.ttf");
const KHMER: &[u8] = include_bytes!("fonts/NotoSansKhmer-Regular.ttf");
const TIRHUTA: &[u8] = include_bytes!("fonts/NotoSansTirhuta-Regular.ttf");
const MYANMAR: &[u8] = include_bytes!("fonts/NotoSansMyanmar-Regular.ttf");
const OLD_HANGUL: &[u8] = include_bytes!("fonts/NotoSansOldHangul-Subset.ttf");

type Row = (u32, u32, i32, i32, i32);

fn rows(font: &[u8], text: &str, features: &[Feature]) -> Vec<Row> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
    shape(&font, &buffer, features)
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.x_advance, g.x_offset, g.y_offset))
        .collect()
}

/// [`rows`] for vertical text, with the y advance for the x advance.
fn vertical_rows(font: &[u8], text: &str, features: &[Feature]) -> Vec<Row> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_direction(Direction::Ttb);
    buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
    shape(&font, &buffer, features)
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.y_advance, g.x_offset, g.y_offset))
        .collect()
}

fn check(font: &[u8], cases: &[(&str, &[Row])]) {
    check_with(font, &[], cases);
}

fn check_with(font: &[u8], features: &[Feature], cases: &[(&str, &[Row])]) {
    for &(text, expected) in cases {
        assert_eq!(rows(font, text, features), expected, "{text:?}");
    }
}

fn u16_at(data: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([data[at], data[at + 1]]))
}

fn put_u16(data: &mut [u8], at: usize, value: usize) {
    data[at..at + 2].copy_from_slice(&(value as u16).to_be_bytes());
}

/// The bytes of table `tag`.
fn table<'a>(font: &'a [u8], tag: &[u8; 4]) -> &'a [u8] {
    let face = Face::parse_bytes(font, 0).unwrap();
    face.table_bytes(*tag).unwrap()
}

/// A copy of `font` with `gsub` as its GSUB.
fn with_gsub(font: &[u8], gsub: Vec<u8>) -> Vec<u8> {
    let face = Face::parse_bytes(font, 0).unwrap();
    let mut tables: Vec<([u8; 4], Vec<u8>)> = face
        .records()
        .iter()
        .filter(|r| r.tag != *b"GSUB")
        .map(|r| (r.tag, face.table_bytes(r.tag).unwrap().to_vec()))
        .collect();
    tables.push((*b"GSUB", gsub));
    tables.sort_by_key(|(tag, _)| *tag);
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    let mut offset = 12 + 16 * tables.len();
    for (tag, body) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        offset += body.len().next_multiple_of(4);
    }
    for (_, body) in &tables {
        out.extend_from_slice(body);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    out
}

/// The positions in `gsub` of every language system, default ones
/// included.
fn lang_systems(gsub: &[u8]) -> Vec<usize> {
    let scripts = u16_at(gsub, 4);
    let mut out = Vec::new();
    for i in 0..u16_at(gsub, scripts) {
        let script = scripts + u16_at(gsub, scripts + 2 + 6 * i + 4);
        if u16_at(gsub, script) != 0 {
            out.push(script + u16_at(gsub, script));
        }
        for j in 0..u16_at(gsub, script + 2) {
            out.push(script + u16_at(gsub, script + 4 + 6 * j + 4));
        }
    }
    out
}

/// The tag of feature `index` in `gsub`.
fn feature_tag(gsub: &[u8], index: usize) -> [u8; 4] {
    let at = u16_at(gsub, 6) + 2 + 6 * index;
    gsub[at..at + 4].try_into().unwrap()
}

/// Every language system that lists a feature tagged `tag` makes that
/// feature its required feature and stops listing it: the entry gives
/// way to a copy of the first entry that is not `tag`.
fn make_required(gsub: &mut [u8], tag: &[u8; 4]) {
    for lang_sys in lang_systems(gsub) {
        let count = u16_at(gsub, lang_sys + 4);
        let entries: Vec<usize> = (0..count).map(|k| lang_sys + 6 + 2 * k).collect();
        let Some(&slot) = entries
            .iter()
            .find(|&&at| feature_tag(gsub, u16_at(gsub, at)) == *tag)
        else {
            continue;
        };
        let other = entries
            .iter()
            .map(|&at| u16_at(gsub, at))
            .find(|&f| feature_tag(gsub, f) != *tag)
            .expect("another feature");
        let index = u16_at(gsub, slot);
        put_u16(gsub, lang_sys + 2, index);
        put_u16(gsub, slot, other);
    }
}

/// Retags every feature tagged `from` as `to`.
fn retag(gsub: &mut [u8], from: &[u8; 4], to: &[u8; 4]) {
    let list = u16_at(gsub, 6);
    for i in 0..u16_at(gsub, list) {
        let at = list + 2 + 6 * i;
        if gsub[at..at + 4] == *from {
            gsub[at..at + 4].copy_from_slice(to);
        }
    }
}

/// `gsub` (version 1.0) as version 1.1, with one FeatureVariations
/// record that holds everywhere and gives each feature of
/// `substitutions` the lookups listed with it.
fn with_substituted_features(gsub: &[u8], substitutions: &[(usize, &[u16])]) -> Vec<u8> {
    let mut out = vec![0, 1, 0, 1];
    for header in [4, 6, 8] {
        out.extend_from_slice(&((u16_at(gsub, header) + 4) as u16).to_be_bytes());
    }
    let variations = 14 + gsub.len() - 10;
    out.extend_from_slice(&(variations as u32).to_be_bytes());
    out.extend_from_slice(&gsub[10..]);
    // FeatureVariations: one record, a null ConditionSet, and the
    // FeatureTableSubstitution right after.
    out.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 16]);
    out.extend_from_slice(&[0, 1, 0, 0]);
    out.extend_from_slice(&(substitutions.len() as u16).to_be_bytes());
    let mut feature = 6 + 6 * substitutions.len();
    for &(index, lookups) in substitutions {
        out.extend_from_slice(&(index as u16).to_be_bytes());
        out.extend_from_slice(&(feature as u32).to_be_bytes());
        feature += 4 + 2 * lookups.len();
    }
    // The alternate Feature tables: no params, then the lookups.
    for &(_, lookups) in substitutions {
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
        for l in lookups {
            out.extend_from_slice(&l.to_be_bytes());
        }
    }
    out
}

/// The index of the first feature tagged `tag`.
fn feature_index(gsub: &[u8], tag: &[u8; 4]) -> usize {
    (0..u16_at(gsub, u16_at(gsub, 6)))
        .find(|&i| feature_tag(gsub, i) == *tag)
        .unwrap()
}

/// The lookups of feature `index`.
fn feature_lookups(gsub: &[u8], index: usize) -> Vec<u16> {
    let list = u16_at(gsub, 6);
    let feature = list + u16_at(gsub, list + 2 + 6 * index + 4);
    (0..u16_at(gsub, feature + 2))
        .map(|k| u16_at(gsub, feature + 4 + 2 * k) as u16)
        .collect()
}

/// Devanagari whose `rphf` is only the required feature.
fn devanagari_required_rphf() -> Vec<u8> {
    let mut gsub = table(DEVANAGARI, b"GSUB").to_vec();
    make_required(&mut gsub, b"rphf");
    with_gsub(DEVANAGARI, gsub)
}

/// Devanagari whose `rphf` is listed but left without lookups by
/// FeatureVariations.
fn devanagari_emptied_rphf() -> Vec<u8> {
    let gsub = table(DEVANAGARI, b"GSUB");
    let rphf = feature_index(gsub, b"rphf");
    with_gsub(DEVANAGARI, with_substituted_features(gsub, &[(rphf, &[])]))
}

/// Devanagari whose listed `rphf` is left without lookups by
/// FeatureVariations, while another `rphf` with its lookups is the
/// required feature. That one is feature 9, a `half` that only `deva`
/// `NEP ` lists, retagged, which FeatureVariations gives the lookups
/// of the listed `rphf`.
fn devanagari_required_and_emptied_rphf() -> Vec<u8> {
    let mut gsub = table(DEVANAGARI, b"GSUB").to_vec();
    let rphf = feature_index(&gsub, b"rphf");
    let lookups = feature_lookups(&gsub, rphf);
    let other = 9;
    assert_eq!(feature_tag(&gsub, other), *b"half");
    let record = u16_at(&gsub, 6) + 2 + 6 * other;
    gsub[record..record + 4].copy_from_slice(b"rphf");
    for lang_sys in lang_systems(&gsub) {
        put_u16(&mut gsub, lang_sys + 2, other);
    }
    let substitutions: [(usize, &[u16]); 2] = [(other, &lookups), (rphf, &[])];
    with_gsub(DEVANAGARI, with_substituted_features(&gsub, &substitutions))
}

/// Malayalam whose `pref` is only the required feature.
fn malayalam_required_pref() -> Vec<u8> {
    let mut gsub = table(MALAYALAM, b"GSUB").to_vec();
    make_required(&mut gsub, b"pref");
    with_gsub(MALAYALAM, gsub)
}

/// Khmer whose `pstf` is retagged `cfar` and made only the required
/// feature.
fn khmer_required_cfar() -> Vec<u8> {
    let mut gsub = table(KHMER, b"GSUB").to_vec();
    retag(&mut gsub, b"pstf", b"cfar");
    make_required(&mut gsub, b"cfar");
    with_gsub(KHMER, gsub)
}

/// Tirhuta (Universal Shaping Engine) whose `rphf` is only the
/// required feature.
fn tirhuta_required_rphf() -> Vec<u8> {
    let mut gsub = table(TIRHUTA, b"GSUB").to_vec();
    make_required(&mut gsub, b"rphf");
    with_gsub(TIRHUTA, gsub)
}

/// Khmer whose `clig` is retagged `liga` and made only the required
/// feature.
fn khmer_required_liga() -> Vec<u8> {
    let mut gsub = table(KHMER, b"GSUB").to_vec();
    retag(&mut gsub, b"clig", b"liga");
    make_required(&mut gsub, b"liga");
    with_gsub(KHMER, gsub)
}

/// Khmer whose `clig` is only the required feature.
fn khmer_required_clig() -> Vec<u8> {
    let mut gsub = table(KHMER, b"GSUB").to_vec();
    make_required(&mut gsub, b"clig");
    with_gsub(KHMER, gsub)
}

/// The glyph `font` maps `c` to.
fn glyph(font: &[u8], c: char) -> u16 {
    let face = Face::parse_bytes(font, 0).unwrap();
    face.cmap().unwrap().glyph_id(c).unwrap()
}

/// A GSUB with one script, `script`, whose default language system
/// lists no feature and has a feature tagged `required` as its required
/// feature. Its one lookup swaps glyphs `a` and `b` (a single
/// substitution, format 2), so it shows how often it ran.
fn swap_gsub(script: &[u8; 4], required: &[u8; 4], a: u16, b: u16) -> Vec<u8> {
    fn words(out: &mut Vec<u8>, values: &[u16]) {
        for v in values {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
    let (lo, hi) = (a.min(b), a.max(b));
    let mut out = Vec::new();
    // Header: version 1.0, then the ScriptList, FeatureList and
    // LookupList offsets.
    words(&mut out, &[1, 0, 10, 28, 42]);
    // ScriptList at 10: one record. Its Script at 18 has its default
    // language system at 22: no lookup order, required feature 0, and
    // no listed features.
    words(&mut out, &[1]);
    out.extend_from_slice(script);
    words(&mut out, &[8, 4, 0, 0, 0, 0]);
    // FeatureList at 28: one record, whose Feature at 36 has lookup 0.
    words(&mut out, &[1]);
    out.extend_from_slice(required);
    words(&mut out, &[8, 0, 1, 0]);
    // LookupList at 42: one Lookup at 46, of type 1 with no flags and
    // one subtable at 54, which maps the coverage at 64 (`lo`, `hi`) to
    // `hi`, `lo`.
    words(
        &mut out,
        &[1, 4, 1, 0, 1, 8, 2, 10, 2, hi, lo, 1, 2, lo, hi],
    );
    out
}

/// `font` with a [`swap_gsub`] that swaps the glyphs of `a` and `b`.
fn with_swap(font: &[u8], script: &[u8; 4], required: &[u8; 4], a: char, b: char) -> Vec<u8> {
    let gsub = swap_gsub(script, required, glyph(font, a), glyph(font, b));
    with_gsub(font, gsub)
}

/// `font` whose GSUB keeps one script, the one tagged `keep`, retagged
/// `tag`. The other scripts' tables stay in the bytes, unreferenced.
fn with_only_script(font: &[u8], keep: &[u8; 4], tag: &[u8; 4]) -> Vec<u8> {
    let mut gsub = table(font, b"GSUB").to_vec();
    let list = u16_at(&gsub, 4);
    let offset = (0..u16_at(&gsub, list))
        .map(|i| list + 2 + 6 * i)
        .find(|&at| gsub[at..at + 4] == *keep)
        .map(|at| u16_at(&gsub, at + 4))
        .expect("script");
    put_u16(&mut gsub, list, 1);
    gsub[list + 2..list + 6].copy_from_slice(tag);
    put_u16(&mut gsub, list + 6, offset);
    with_gsub(font, gsub)
}

#[test]
fn a_required_rphf_finds_no_reph_and_runs_on_every_glyph() {
    // The language systems no longer list `rphf`, so HarfBuzz's map has
    // no `rphf`: initial reordering looks for no reph, and the reph
    // forms where Ra and halant stand, before the base. The required
    // feature's lookups run in the `rphf` stage on every glyph, so a
    // Ra and halant after the base forms a reph too.
    check(
        &devanagari_required_rphf(),
        &[
            (
                "\u{0930}\u{094D}\u{0915}",
                &[(506, 0, 0, 0, 0), (56, 6, 768, 0, 0)],
            ),
            (
                "\u{0930}\u{094D}\u{0915}\u{093F}",
                &[(32, 0, 259, 0, 0), (506, 0, 0, 0, 0), (56, 0, 768, 0, 0)],
            ),
            (
                "\u{0915}\u{0930}\u{094D}",
                &[(56, 0, 768, 0, 0), (506, 3, 0, -221, 0)],
            ),
            (
                "\u{0927}\u{0930}\u{094D}\u{092E}",
                &[(74, 0, 615, 0, 0), (506, 3, 0, 0, 0), (80, 9, 598, 0, 0)],
            ),
            (
                "\u{0930}\u{094D}\u{0915}\u{094D}\u{092F}",
                &[(506, 0, 0, 0, 0), (232, 6, 535, 0, 0), (81, 12, 580, 0, 0)],
            ),
            (
                "\u{0930}\u{094D}\u{200D}\u{0915}",
                &[(507, 0, 379, 0, 0), (56, 9, 768, 0, 0)],
            ),
        ],
    );
}

#[test]
fn an_rphf_without_lookups_forms_no_reph() {
    // FeatureVariations leaves the listed `rphf` without lookups. It
    // keeps its mask bit, but nothing substitutes, so Ra and halant
    // stay as they are.
    check(
        &devanagari_emptied_rphf(),
        &[
            (
                "\u{0930}\u{094D}\u{0915}",
                &[(82, 0, 409, 0, 0), (103, 0, 0, 0, 0), (56, 6, 768, 0, 0)],
            ),
            (
                "\u{0930}\u{094D}\u{0915}\u{093F}",
                &[
                    (82, 0, 409, 0, 0),
                    (103, 0, 0, 0, 0),
                    (545, 6, 259, 0, 0),
                    (56, 6, 768, 0, 0),
                ],
            ),
            (
                "\u{0915}\u{0930}\u{094D}",
                &[(56, 0, 768, 0, 0), (82, 3, 409, 0, 0), (103, 3, 0, 0, 0)],
            ),
            (
                "\u{0927}\u{0930}\u{094D}\u{092E}",
                &[
                    (74, 0, 615, 0, 0),
                    (82, 3, 409, 0, 0),
                    (103, 3, 0, 0, 0),
                    (80, 9, 598, 0, 0),
                ],
            ),
        ],
    );
}

#[test]
fn a_listed_rphf_without_lookups_still_takes_the_required_reph() {
    // FeatureVariations leaves the listed `rphf` without lookups, and
    // the required feature, also `rphf`, keeps them. The listed feature
    // gives `rphf` its mask bit, so initial reordering finds the reph
    // through the required feature's lookups and moves it after the
    // base, and those lookups form a reph after a base too.
    check(
        &devanagari_required_and_emptied_rphf(),
        &[
            (
                "\u{0930}\u{094D}\u{0915}",
                &[(56, 0, 768, 0, 0), (506, 0, 0, -221, 0)],
            ),
            (
                "\u{0915}\u{0930}\u{094D}",
                &[(56, 0, 768, 0, 0), (506, 3, 0, -221, 0)],
            ),
            (
                "\u{0930}\u{094D}\u{200C}\u{0915}",
                &[(506, 0, 0, 0, 0), (3, 6, 0, 0, 0), (56, 9, 768, 0, 0)],
            ),
            (
                "\u{0915}\u{094D}\u{0930}\u{094D}\u{0915}",
                &[(232, 0, 545, 0, 0), (506, 6, 0, 0, 0), (56, 12, 768, 0, 0)],
            ),
            (
                "\u{0930}\u{094D}\u{0930}\u{094D}\u{0915}",
                &[(506, 0, 0, 0, 0), (56, 0, 768, 0, 0), (506, 0, 0, -221, 0)],
            ),
            (
                "\u{0927}\u{0930}\u{094D}\u{092E}",
                &[(74, 0, 615, 0, 0), (80, 3, 598, 0, 0), (506, 3, 0, 0, 0)],
            ),
        ],
    );
}

#[test]
fn a_required_pref_reorders_no_pre_base_form() {
    // Malayalam whose `pref` is only the required feature: HarfBuzz's
    // map has no `pref`, so the `pref` test of initial reordering, and
    // the consonant positions that read `pref`, find nothing, while
    // the required lookups still form the pre-base Ra where it stands.
    check(
        &malayalam_required_pref(),
        &[
            (
                "\u{0D15}\u{0D4D}\u{0D30}",
                &[(21, 0, 1038, 0, 0), (158, 0, 244, 0, 0)],
            ),
            (
                "\u{0D15}\u{0D4D}\u{0D30}\u{0D3F}",
                &[(21, 0, 1038, 0, 0), (158, 0, 244, 0, 0), (61, 0, 228, 0, 0)],
            ),
            (
                "\u{0D15}\u{0D4D}\u{0D30}\u{0D46}",
                &[(21, 0, 1038, 0, 0), (67, 0, 715, 0, 0), (158, 0, 244, 0, 0)],
            ),
            (
                "\u{0D2A}\u{0D4D}\u{0D30}\u{0D4A}",
                &[
                    (42, 0, 896, 0, 0),
                    (67, 0, 715, 0, 0),
                    (158, 0, 244, 0, 0),
                    (60, 0, 504, 0, 0),
                ],
            ),
            (
                "\u{0D38}\u{0D4D}\u{0D24}\u{0D4D}\u{0D30}\u{0D46}",
                &[
                    (56, 0, 1223, 0, 0),
                    (73, 0, 0, 0, 0),
                    (36, 6, 1014, 0, 0),
                    (67, 6, 715, 0, 0),
                    (158, 6, 244, 0, 0),
                ],
            ),
        ],
    );
}

#[test]
fn a_required_cfar_runs_in_the_khmer_basic_stage_on_every_glyph() {
    // Khmer whose `pstf` is retagged `cfar` and made only the required
    // feature. HarfBuzz's Khmer shaper has `cfar`, so the required
    // feature runs in its basic stage, on every glyph, not only after
    // a coeng and ro: the coeng and ya take their post-base form and
    // ligate with the vowel sign that follows.
    check(
        &khmer_required_cfar(),
        &[
            (
                "\u{1780}\u{17D2}\u{1799}\u{17B6}",
                &[(25, 0, 636, 0, 0), (302, 0, 580, 0, 0)],
            ),
            (
                "\u{1780}\u{17D2}\u{1799}\u{17BE}",
                &[
                    (107, 0, 288, 0, 0),
                    (25, 0, 636, 0, 0),
                    (194, 0, 298, 0, 0),
                    (85, 0, 0, 1, 30),
                ],
            ),
            (
                "\u{1780}\u{17D2}\u{1799}\u{17C4}",
                &[(107, 0, 288, 0, 0), (25, 0, 636, 0, 0), (302, 0, 580, 0, 0)],
            ),
            (
                "\u{1780}\u{17D2}\u{179A}\u{17B6}",
                &[(196, 0, 287, 0, 0), (212, 0, 924, 0, 0)],
            ),
            (
                "\u{1780}\u{17D2}\u{179A}\u{17BE}",
                &[
                    (107, 0, 288, 0, 0),
                    (196, 0, 287, 0, 0),
                    (25, 0, 636, 0, 0),
                    (85, 0, 0, -23, -29),
                ],
            ),
        ],
    );
}

#[test]
fn the_universal_shaping_engine_finds_no_reph_in_a_required_rphf() {
    // Tirhuta whose `rphf` is only the required feature: no `rphf` mask,
    // so no repha to reorder, while the required lookups still form the
    // reph glyph in place.
    check(
        &tirhuta_required_rphf(),
        &[
            (
                "\u{114A9}\u{114C2}\u{1148F}",
                &[(134, 0, 0, 0, 0), (25, 8, 807, 0, 0)],
            ),
            (
                "\u{114A9}\u{114C2}\u{1148F}\u{114B1}",
                &[(59, 0, 266, 0, 0), (134, 0, 0, 138, 47), (25, 0, 807, 0, 0)],
            ),
            (
                "\u{1148F}\u{114A9}\u{114C2}",
                &[(25, 0, 807, 0, 0), (51, 4, 596, 0, 0), (76, 4, 0, -76, 0)],
            ),
            (
                "\u{114A9}\u{114C2}\u{1148F}\u{114C2}\u{114A8}",
                &[(134, 0, 0, 0, 0), (25, 8, 807, 0, 0), (132, 8, 266, 0, 0)],
            ),
        ],
    );
}

#[test]
fn a_required_feature_whose_tag_the_caller_turns_off_runs_in_stage_zero() {
    // With `rphf=0` HarfBuzz's map has no `rphf`, so the required
    // `rphf` runs in stage 0 with `rvrn`, on every glyph, and no reph is
    // reordered. It used to run nowhere.
    let off = [Feature {
        tag: *b"rphf",
        value: 0,
    }];
    check_with(
        &devanagari_required_rphf(),
        &off,
        &[
            (
                "\u{0930}\u{094D}\u{0915}",
                &[(506, 0, 0, 0, 0), (56, 6, 768, 0, 0)],
            ),
            (
                "\u{0915}\u{0930}\u{094D}",
                &[(56, 0, 768, 0, 0), (506, 3, 0, -221, 0)],
            ),
            (
                "\u{0930}\u{094D}\u{0930}\u{094D}\u{0915}",
                &[(506, 0, 0, 0, 0), (506, 6, 0, 0, 0), (56, 12, 768, 0, 0)],
            ),
        ],
    );
}

#[test]
fn a_required_liga_runs_in_stage_zero_in_khmer() {
    // HarfBuzz's Khmer shaper turns `liga` off after the caller's
    // features, so a required `liga` runs in stage 0, whatever the
    // caller says about `liga`. It used to run nowhere.
    let font = khmer_required_liga();
    let on = [Feature {
        tag: *b"liga",
        value: 1,
    }];
    for features in [&[][..], &on[..]] {
        check_with(
            &font,
            features,
            &[
                ("\u{1780}\u{17B6}", &[(212, 0, 924, 0, 0)]),
                (
                    "\u{1781}\u{17B6}\u{17C6}",
                    &[(214, 0, 923, 0, 0), (113, 0, 0, 47, -29)],
                ),
            ],
        );
    }
}

fn feature(tag: &[u8; 4], value: u32) -> Feature {
    Feature { tag: *tag, value }
}

#[test]
fn a_required_liga_runs_once_when_devanagari_takes_the_default_shaper() {
    // A GSUB whose only script is `DFLT` sends Devanagari to HarfBuzz's
    // default shaper. That shaper runs `liga`, so the required `liga`
    // runs once, in the stage of `liga`, or in stage 0 when the caller
    // turns `liga` off. It used to run in both, which swapped ka back.
    // With a `dev2` script, or the misspelled `dflt`, which keeps the
    // Indic shaper, that shaper turns `liga` off, so the required `liga`
    // runs once, in stage 0.
    let off = [feature(b"liga", 0)];
    let on = [feature(b"liga", 1)];
    for script in [b"DFLT", b"dev2", b"dflt"] {
        let font = with_swap(DEVANAGARI, script, b"liga", '\u{0915}', '\u{0916}');
        for features in [&[][..], &off, &on] {
            check_with(
                &font,
                features,
                &[
                    ("\u{0915}", &[(57, 0, 818, 0, 0)]),
                    ("\u{0916}", &[(56, 0, 768, 0, 0)]),
                    (
                        "\u{0915}\u{0916}",
                        &[(57, 0, 818, 0, 0), (56, 3, 768, 0, 0)],
                    ),
                ],
            );
        }
    }
}

#[test]
fn a_dflt_script_keeps_the_indic_and_myanmar_shapers() {
    // HarfBuzz picks the misspelled `dflt` script tag when a font has
    // no better one, but only sends Devanagari or Myanmar to the default
    // shaper for `DFLT` and `latn` (and `mymr`), not for `dflt`. With
    // `dflt` the Indic shaper still moves the pre-base matra and forms
    // the reph and the conjuncts, and the Myanmar shaper moves medial ra
    // and sign e before the consonant and forms the kinzi. Both used to
    // take the default shaper, as they still do with `DFLT`.
    check(
        &with_only_script(DEVANAGARI, b"dev2", b"dflt"),
        &[
            (
                "\u{0915}\u{093F}",
                &[(545, 0, 259, 0, 0), (56, 0, 768, 0, 0)],
            ),
            ("\u{0915}\u{094D}\u{0937}", &[(90, 0, 717, 0, 0)]),
            (
                "\u{0930}\u{094D}\u{0915}",
                &[(56, 0, 768, 0, 0), (506, 0, 0, -221, 0)],
            ),
            (
                "\u{0926}\u{094D}\u{0935}\u{093F}",
                &[(546, 0, 259, 0, 0), (455, 0, 575, 0, 0)],
            ),
        ],
    );
    check(
        &with_only_script(DEVANAGARI, b"dev2", b"DFLT"),
        &[
            (
                "\u{0915}\u{093F}",
                &[(56, 0, 768, 0, 0), (32, 0, 259, 0, 0)],
            ),
            (
                "\u{0915}\u{094D}\u{0937}",
                &[(56, 0, 768, 0, 0), (103, 0, 0, -221, 0), (86, 6, 578, 0, 0)],
            ),
        ],
    );
    check(
        &with_only_script(MYANMAR, b"mym2", b"dflt"),
        &[
            (
                "\u{1000}\u{103C}",
                &[(198, 0, 229, 0, 0), (4, 0, 1124, 0, 0)],
            ),
            (
                "\u{1000}\u{1031}",
                &[(372, 0, 618, 0, 0), (4, 0, 1124, 0, 0)],
            ),
            (
                "\u{1004}\u{103A}\u{1039}\u{1000}",
                &[(4, 0, 1124, 0, 0), (189, 0, 0, -1, 0)],
            ),
        ],
    );
    check(
        &with_only_script(MYANMAR, b"mym2", b"DFLT"),
        &[
            (
                "\u{1000}\u{103C}",
                &[(4, 0, 1124, 0, 0), (47, 0, 229, 0, 0)],
            ),
            (
                "\u{1000}\u{1031}",
                &[(4, 0, 1124, 0, 0), (372, 0, 618, 0, 0)],
            ),
        ],
    );
}

#[test]
fn a_required_liga_runs_in_stage_zero_in_vertical_text() {
    // Vertical text has no `liga` unless the caller turns it on, so the
    // required `liga` of a `DFLT` font runs in stage 0 there, and in
    // the stage of `liga` with `liga=1`.
    let font = with_swap(DEVANAGARI, b"DFLT", b"liga", '\u{0915}', '\u{0916}');
    let off = [feature(b"liga", 0)];
    let on = [feature(b"liga", 1)];
    for features in [&[][..], &off, &on] {
        let text = "\u{0915}\u{0916}";
        assert_eq!(
            vertical_rows(&font, text, features),
            [(57, 0, -1304, -409, -963), (56, 3, -1304, -384, -963)],
            "{text:?} {features:?}"
        );
    }
}

#[test]
fn a_required_rphf_runs_in_stage_zero_when_devanagari_takes_the_default_shaper() {
    // HarfBuzz's default shaper has no `rphf`, so with a `DFLT` or
    // `latn` script the required `rphf` runs in stage 0. It used to run
    // nowhere. With `dev2` or `dflt` it runs in the Indic shaper's
    // `rphf` stage, or in stage 0 when the caller turns `rphf` off.
    let off = [feature(b"rphf", 0)];
    for script in [b"DFLT", b"latn", b"dev2", b"dflt"] {
        let font = with_swap(DEVANAGARI, script, b"rphf", '\u{0915}', '\u{0916}');
        for features in [&[][..], &off] {
            check_with(
                &font,
                features,
                &[
                    ("\u{0915}", &[(57, 0, 818, 0, 0)]),
                    (
                        "\u{0930}\u{094D}\u{0915}",
                        &[(82, 0, 409, 0, 0), (103, 0, 0, 0, 0), (57, 6, 818, 0, 0)],
                    ),
                ],
            );
        }
    }
}

#[test]
fn a_required_pref_runs_in_stage_zero_when_myanmar_takes_the_default_shaper() {
    // HarfBuzz sends Myanmar to the default shaper when the GSUB script
    // is `DFLT`, or `mymr`, the tag of fonts made before the Myanmar
    // shaping model. That shaper has no `pref`, so the required `pref`
    // runs in stage 0, before anything moves. It used to run nowhere.
    // With `mym2`, or the misspelled `dflt`, the Myanmar shaper runs it
    // in its `pref` stage, after medial ra moves before the consonant.
    let off = [feature(b"pref", 0)];
    for script in [b"DFLT", b"mymr", b"mym2", b"dflt"] {
        let font = with_swap(MYANMAR, script, b"pref", '\u{1000}', '\u{1001}');
        let medial_ra: &[Row] = if matches!(script, b"mym2" | b"dflt") {
            &[(47, 0, 229, 0, 0), (5, 0, 676, 0, 0)]
        } else {
            &[(5, 0, 676, 0, 0), (47, 0, 229, 0, 0)]
        };
        for features in [&[][..], &off] {
            check_with(
                &font,
                features,
                &[
                    ("\u{1000}", &[(5, 0, 676, 0, 0)]),
                    ("\u{1001}", &[(4, 0, 1124, 0, 0)]),
                    ("\u{1000}\u{103C}", medial_ra),
                ],
            );
        }
    }
}

#[test]
fn a_required_ljmo_runs_once_on_hangul_syllables() {
    // The Hangul shaper has `ljmo`, so a required `ljmo` runs once, in
    // its stage, on syllables as on jamo, which compose to a syllable
    // first. When no jamo was left it used to run in stage 0 too, which
    // swapped the syllable back. With `ljmo=0` it runs in stage 0.
    let font = with_swap(OLD_HANGUL, b"hang", b"ljmo", '\u{AC00}', '\u{AC01}');
    for features in [&[][..], &[feature(b"ljmo", 0)]] {
        check_with(
            &font,
            features,
            &[
                ("\u{AC00}", &[(341, 0, 920, 0, 0)]),
                ("\u{AC01}", &[(340, 0, 920, 0, 0)]),
                (
                    "\u{AC00}\u{AC01}",
                    &[(341, 0, 920, 0, 0), (340, 3, 920, 0, 0)],
                ),
                ("\u{1100}\u{1161}", &[(341, 0, 920, 0, 0)]),
            ],
        );
    }
}

#[test]
fn khmer_runs_clig_whatever_the_caller_says() {
    // `override_features_khmer` turns `clig` on after the caller's
    // features, so `clig=0` changes nothing. It used to turn `clig` off.
    let off = [feature(b"clig", 0)];
    let liga_on = [feature(b"liga", 1), feature(b"clig", 0)];
    for features in [&off[..], &liga_on] {
        check_with(
            KHMER,
            features,
            &[
                (
                    "\u{1787}\u{17B6}\u{17DC}\u{17D3}\u{17A9}",
                    &[
                        (226, 0, 923, 0, 0),
                        (146, 6, 733, 0, 0),
                        (360, 6, 635, 0, 0),
                        (136, 6, 0, -20, -84),
                        (69, 12, 734, 0, 0),
                    ],
                ),
                (
                    "\u{1782}\u{17C4}\u{17D3}\u{1794}\u{17D2}\u{1781}",
                    &[
                        (107, 0, 288, 0, 0),
                        (216, 0, 923, 0, 0),
                        (136, 0, 0, 47, -29),
                        (46, 9, 635, 0, 0),
                        (160, 9, 0, -1, -26),
                    ],
                ),
                (
                    "\u{1792}\u{1798}\u{17C5}\u{17BF}",
                    &[
                        (44, 0, 635, 0, 0),
                        (107, 3, 288, 0, 0),
                        (265, 3, 923, 0, 0),
                        (107, 3, 288, 0, 0),
                        (360, 3, 635, 0, 0),
                        (99, 3, 288, 0, 0),
                    ],
                ),
            ],
        );
    }
}

#[test]
fn khmer_runs_clig_in_vertical_text() {
    // The override turns `clig` on in vertical text too, where HarfBuzz
    // runs none of the other horizontal default features. It used to
    // stay off there.
    let cases: [(&str, &[Row]); 2] = [
        (
            "\u{1799}\u{17D2}\u{1783}\u{17C5}",
            &[
                (107, 0, -1362, -144, -974),
                (53, 0, -1362, -476, -974),
                (288, 0, -1362, -287, -986),
            ],
        ),
        (
            "\u{1791}\u{1783}\u{17D2}\u{17AC}\u{17B6}",
            &[
                (43, 0, -1362, -299, -974),
                (218, 3, -1362, -608, -974),
                (210, 3, -1362, 353, 362),
            ],
        ),
    ];
    for features in [&[][..], &[feature(b"clig", 0)]] {
        for (text, expected) in cases {
            assert_eq!(vertical_rows(KHMER, text, features), expected, "{text:?}");
        }
    }
}

#[test]
fn a_required_clig_stays_in_the_khmer_last_stage() {
    // As `clig` is always on, a required `clig` always runs in the Khmer
    // shaper's last stage, after the syllables are reordered. With
    // `clig=0` it used to run in stage 0, and in vertical text nowhere.
    let font = khmer_required_clig();
    let off = [feature(b"clig", 0)];
    let calt_off = [feature(b"calt", 0), feature(b"clig", 0)];
    for features in [&[][..], &off, &calt_off] {
        check_with(
            &font,
            features,
            &[
                (
                    "\u{17AA}\u{17A1}\u{17C4}",
                    &[
                        (70, 0, 636, 0, 0),
                        (107, 3, 288, 0, 0),
                        (282, 3, 1134, 0, 0),
                    ],
                ),
                (
                    "\u{1790}\u{17C5}\u{17C9}",
                    &[
                        (107, 0, 288, 0, 0),
                        (247, 0, 923, 0, 0),
                        (360, 0, 635, 0, 0),
                        (117, 0, 0, -20, -84),
                    ],
                ),
            ],
        );
        let text = "\u{1794}\u{17C5}\u{17D2}\u{1789}\u{1788}";
        assert_eq!(
            vertical_rows(&font, text, features),
            [
                (107, 0, -1362, -144, -974),
                (256, 0, -1362, -461, -1088),
                (170, 0, -1362, 482, 218),
                (33, 12, -1362, -625, -974),
            ],
            "{text:?}"
        );
    }
}
