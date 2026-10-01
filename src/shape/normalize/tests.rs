use alloc::vec::Vec;

use super::{NormChar, Normalizer};
use crate::buffer::{char_class, unicode_prop, ClusterLevel};
use crate::shape::shaper::Shaper;
use crate::tables::cmap::{build_cmap_wrapper, build_format12, Cmap};

/// A cmap mapping each of `chars` to the glyph with the same number as
/// its code point (all test characters are in the BMP).
fn cmap_bytes(chars: &[char]) -> Vec<u8> {
    let mut cps: Vec<u32> = chars.iter().map(|&c| u32::from(c)).collect();
    cps.sort_unstable();
    cps.dedup();
    let groups: Vec<(u32, u32, u32)> = cps.iter().map(|&c| (c, c, c)).collect();
    build_cmap_wrapper(&[(3, 10, build_format12(&groups))])
}

/// Normalizes `text` (cluster = byte offset) under `shaper` with a font
/// mapping `font_chars`; returns `(char, glyph, cluster)` triples.
fn normalize(text: &str, font_chars: &[char], shaper: Shaper) -> Vec<(char, u32, u32)> {
    normalize_with(text, font_chars, shaper, true)
}

fn normalize_with(
    text: &str,
    font_chars: &[char],
    shaper: Shaper,
    has_gpos_mark: bool,
) -> Vec<(char, u32, u32)> {
    let bytes = cmap_bytes(font_chars);
    let cmap = Cmap::parse(&bytes).unwrap();
    let normalizer = Normalizer {
        cmap: &cmap,
        shaper,
        has_gpos_mark,
        level: ClusterLevel::MonotoneGraphemes,
        recategorize_marks: false,
        not_found_variation_selector: None,
    };
    let chars: Vec<NormChar> = text
        .char_indices()
        .map(|(i, c)| NormChar::new(c, i as u32, false))
        .collect();
    normalizer
        .run(&chars)
        .iter()
        .map(|c| (c.ch, c.glyph, c.cluster))
        .collect()
}

#[test]
fn a_mark_composes_with_its_base_when_the_font_has_the_composite() {
    let out = normalize(
        "e\u{0301}x",
        &['e', '\u{0301}', '\u{00E9}', 'x'],
        Shaper::Default,
    );
    assert_eq!(out, [('\u{00E9}', 0xE9, 0), ('x', 0x78, 3)]);
}

#[test]
fn without_the_composite_the_pair_stays_decomposed() {
    let out = normalize("e\u{0301}", &['e', '\u{0301}'], Shaper::Default);
    assert_eq!(out, [('e', 0x65, 0), ('\u{0301}', 0x301, 1)]);
}

#[test]
fn a_precomposed_character_the_font_lacks_decomposes_in_one_cluster() {
    // No glyph for U+1EC7, but the font has e, U+0323, and U+0302.
    let out = normalize("\u{1EC7}", &['e', '\u{0323}', '\u{0302}'], Shaper::Default);
    assert_eq!(
        out,
        [
            ('e', 0x65, 0),
            ('\u{0323}', 0x323, 0),
            ('\u{0302}', 0x302, 0)
        ]
    );
    // With U+1EB9 (e with dot below) mapped, decomposition stops there.
    let out = normalize("\u{1EC7}", &['\u{1EB9}', '\u{0302}'], Shaper::Default);
    assert_eq!(out, [('\u{1EB9}', 0x1EB9, 0), ('\u{0302}', 0x302, 0)]);
}

#[test]
fn a_mapped_single_character_is_left_alone_in_composed_modes() {
    let font = ['\u{00E9}', 'e', '\u{0301}'];
    assert_eq!(
        normalize("\u{00E9}", &font, Shaper::Default),
        [('\u{00E9}', 0xE9, 0)]
    );
    // The no-short-circuit modes decompose even mapped characters. A
    // run whose input has no marks skips the recompose round, so the
    // pieces stay decomposed.
    assert_eq!(
        normalize("\u{00E9}", &font, Shaper::Use),
        [('e', 0x65, 0), ('\u{0301}', 0x301, 0)]
    );
    // With a mark in the input, the run recomposes.
    assert_eq!(
        normalize("\u{00E9}\u{0301}", &font, Shaper::Use),
        [('\u{00E9}', 0xE9, 0), ('\u{0301}', 0x301, 2)]
    );
}

#[test]
fn vietnamese_marks_in_any_order_recompose() {
    // Circumflex (230) typed before dot below (220): canonical order
    // puts the dot below first, then both compose.
    let font = ['e', '\u{0302}', '\u{0323}', '\u{1EB9}', '\u{1EC7}'];
    let out = normalize("e\u{0302}\u{0323}", &font, Shaper::Default);
    assert_eq!(out, [('\u{1EC7}', 0x1EC7, 0)]);
    // o with circumflex and acute stacks the same way in either typing
    // order only when canonically equivalent; this one is canonical.
    let font = ['o', '\u{0302}', '\u{0301}', '\u{00F4}', '\u{1ED1}'];
    assert_eq!(
        normalize("o\u{0302}\u{0301}", &font, Shaper::Default),
        [('\u{1ED1}', 0x1ED1, 0)]
    );
}

#[test]
fn reordering_merges_the_clusters_it_moves_across() {
    // Dot below after acute: it moves in front and merges clusters,
    // but the font composes nothing.
    let out = normalize(
        "q\u{0301}\u{0323}",
        &['q', '\u{0301}', '\u{0323}'],
        Shaper::Default,
    );
    assert_eq!(
        out,
        [
            ('q', 0x71, 0),
            ('\u{0323}', 0x323, 1),
            ('\u{0301}', 0x301, 1)
        ]
    );
}

#[test]
fn a_blocked_mark_does_not_compose() {
    // The bridge above (230) composes with nothing and blocks the
    // acute of the same class from reaching the starter.
    let font = ['a', '\u{0346}', '\u{0301}', '\u{00E1}'];
    let out = normalize("a\u{0346}\u{0301}", &font, Shaper::Default);
    let chars: Vec<char> = out.iter().map(|c| c.0).collect();
    assert_eq!(chars, ['a', '\u{0346}', '\u{0301}']);
    // A grave below (220) does not block it; the composite takes the
    // clusters of everything it spans.
    let font = ['a', '\u{0316}', '\u{0301}', '\u{00E1}'];
    let out = normalize("a\u{0316}\u{0301}", &font, Shaper::Default);
    assert_eq!(out, [('\u{00E1}', 0xE1, 0), ('\u{0316}', 0x316, 0)]);
    // Once a mark composes, the next one tries the composite.
    let font = ['a', '\u{0323}', '\u{030A}', '\u{1EA1}'];
    let out = normalize("a\u{0323}\u{030A}", &font, Shaper::Default);
    assert_eq!(out, [('\u{1EA1}', 0x1EA1, 0), ('\u{030A}', 0x30A, 3)]);
}

#[test]
fn hebrew_points_sort_into_the_sbl_order() {
    // Shin, qamats, shin dot: the shin dot sorts first. U+FB2A is a
    // composition exclusion, and the font has GPOS mark positioning,
    // so nothing composes.
    let font = ['\u{05E9}', '\u{05B8}', '\u{05C1}', '\u{FB2A}'];
    let out = normalize("\u{05E9}\u{05B8}\u{05C1}", &font, Shaper::Hebrew);
    assert_eq!(
        out,
        [
            ('\u{05E9}', 0x5E9, 0),
            ('\u{05C1}', 0x5C1, 2),
            ('\u{05B8}', 0x5B8, 2)
        ]
    );
}

#[test]
fn hebrew_presentation_forms_compose_only_without_gpos_marks() {
    let font = ['\u{05E9}', '\u{05C1}', '\u{FB2A}'];
    let with_gpos = normalize_with("\u{05E9}\u{05C1}", &font, Shaper::Hebrew, true);
    assert_eq!(with_gpos.len(), 2);
    let without = normalize_with("\u{05E9}\u{05C1}", &font, Shaper::Hebrew, false);
    assert_eq!(without, [('\u{FB2A}', 0xFB2A, 0)]);
    // The default shaper has no such composition.
    let default = normalize_with("\u{05E9}\u{05C1}", &font, Shaper::Default, false);
    assert_eq!(default.len(), 2);
}

#[test]
fn hebrew_meteg_moves_before_sheva_after_patah() {
    // Alef, patah, sheva, meteg: sorted by class that is patah, sheva,
    // meteg; the Hebrew hook swaps sheva and meteg.
    let font = ['\u{05D0}', '\u{05B7}', '\u{05B0}', '\u{05BD}'];
    let out = normalize("\u{05D0}\u{05B7}\u{05B0}\u{05BD}", &font, Shaper::Hebrew);
    let chars: Vec<char> = out.iter().map(|c| c.0).collect();
    assert_eq!(chars, ['\u{05D0}', '\u{05B7}', '\u{05BD}', '\u{05B0}']);
    assert_eq!(out[2].2, out[3].2);
}

#[test]
fn arabic_shadda_sorts_before_the_vowel_and_modifier_marks_lead() {
    let font = ['\u{0628}', '\u{064E}', '\u{0651}'];
    let out = normalize("\u{0628}\u{064E}\u{0651}", &font, Shaper::Arabic);
    let chars: Vec<char> = out.iter().map(|c| c.0).collect();
    assert_eq!(chars, ['\u{0628}', '\u{0651}', '\u{064E}']);

    // Alef, fatha, hamza above: the hamza (a modifier combining mark)
    // moves in front of the fatha, then composes with the alef.
    let font = ['\u{0627}', '\u{064E}', '\u{0654}', '\u{0623}'];
    let out = normalize("\u{0627}\u{064E}\u{0654}", &font, Shaper::Arabic);
    assert_eq!(out, [('\u{0623}', 0x623, 0), ('\u{064E}', 0x64E, 0)]);
}

#[test]
fn arabic_modifier_marks_get_the_renumbered_classes() {
    let bytes = cmap_bytes(&['\u{0628}', '\u{0650}', '\u{0655}']);
    let cmap = Cmap::parse(&bytes).unwrap();
    let normalizer = Normalizer {
        cmap: &cmap,
        shaper: Shaper::Arabic,
        has_gpos_mark: true,
        level: ClusterLevel::MonotoneGraphemes,
        recategorize_marks: false,
        not_found_variation_selector: None,
    };
    let chars: Vec<NormChar> = "\u{0628}\u{0650}\u{0655}"
        .char_indices()
        .map(|(i, c)| NormChar::new(c, i as u32, false))
        .collect();
    let out = normalizer.run(&chars);
    // The kasra's modified class (33) sorts it before the hamza below
    // (220); the hook then moves the hamza, a modifier combining mark,
    // to the front with the meteg class 25.
    assert_eq!(out[1].ch, '\u{0655}');
    assert_eq!(out[1].mcc, 25);
    assert_eq!(out[2].ch, '\u{0650}');
}

#[test]
fn khmer_split_vowels_get_their_pre_base_part() {
    let font = ['\u{1780}', '\u{17C1}', '\u{17C4}'];
    let out = normalize("\u{1780}\u{17C4}", &font, Shaper::Khmer);
    assert_eq!(
        out,
        [
            ('\u{1780}', 0x1780, 0),
            ('\u{17C1}', 0x17C1, 3),
            ('\u{17C4}', 0x17C4, 3)
        ]
    );
    // Without the sign e glyph the vowel stays whole.
    let out = normalize("\u{1780}\u{17C4}", &['\u{1780}', '\u{17C4}'], Shaper::Khmer);
    assert_eq!(out.len(), 2);
}

#[test]
fn indic_split_matras_decompose_and_stay_split() {
    // Tamil sign o decomposes into its two halves even though the font
    // maps it, and the Indic compose hook keeps them apart.
    let font = ['\u{0B95}', '\u{0BCA}', '\u{0BC6}', '\u{0BBE}'];
    let out = normalize("\u{0B95}\u{0BCA}", &font, Shaper::Indic);
    let chars: Vec<char> = out.iter().map(|c| c.0).collect();
    assert_eq!(chars, ['\u{0B95}', '\u{0BC6}', '\u{0BBE}']);
    // Devanagari rra stays whole under the Indic shaper.
    let font = ['\u{0931}', '\u{0930}', '\u{093C}'];
    assert_eq!(normalize("\u{0931}", &font, Shaper::Indic).len(), 1);
    // A three-part Sinhala vowel decomposes fully under USE.
    let font = ['\u{0D9A}', '\u{0DD9}', '\u{0DCF}', '\u{0DCA}'];
    let out = normalize("\u{0D9A}\u{0DDD}", &font, Shaper::Use);
    let chars: Vec<char> = out.iter().map(|c| c.0).collect();
    assert_eq!(chars, ['\u{0D9A}', '\u{0DD9}', '\u{0DCF}', '\u{0DCA}']);
}

#[test]
fn hangul_mode_keeps_mapped_characters_and_never_recomposes() {
    let font = ['\u{1100}', '\u{1161}', '\u{AC00}'];
    let out = normalize("\u{1100}\u{1161}", &font, Shaper::Hangul);
    assert_eq!(out.len(), 2);
    // A syllable the font lacks decomposes into jamo it has.
    let out = normalize("\u{AC00}", &['\u{1100}', '\u{1161}'], Shaper::Hangul);
    assert_eq!(out, [('\u{1100}', 0x1100, 0), ('\u{1161}', 0x1161, 0)]);
}

#[test]
fn unmapped_characters_fall_back_to_notdef_or_the_hyphen() {
    let out = normalize("a\u{2011}\u{2011}", &['a', '\u{2010}'], Shaper::Default);
    assert_eq!(
        out,
        [
            ('a', 0x61, 0),
            ('\u{2011}', 0x2010, 1),
            ('\u{2011}', 0x2010, 4)
        ]
    );
    let out = normalize("z", &['a'], Shaper::Default);
    assert_eq!(out, [('z', 0, 0)]);
}

#[test]
fn a_cluster_with_a_variation_selector_is_not_decomposed() {
    // Without a variation selector the unmapped e-acute decomposes;
    // with one, each character maps on its own.
    let font = ['e', '\u{0301}', '\u{FE00}'];
    let out = normalize("\u{00E9}\u{FE00}", &font, Shaper::Default);
    assert_eq!(out, [('\u{00E9}', 0, 0), ('\u{FE00}', 0xFE00, 2)]);
    // An unmapped selector gets glyph 0.
    let out = normalize("e\u{FE01}", &['e'], Shaper::Default);
    assert_eq!(out, [('e', 0x65, 0), ('\u{FE01}', 0, 1)]);
    // Recomposition still runs over the whole run.
    let font = ['e', '\u{0301}', '\u{00E9}', '\u{FE00}'];
    let out = normalize("e\u{0301}\u{FE00}", &font, Shaper::Default);
    let chars: Vec<char> = out.iter().map(|c| c.0).collect();
    assert_eq!(chars, ['\u{00E9}', '\u{FE00}']);
}

#[test]
fn glyphs_carry_the_mark_class_bits() {
    use crate::buffer::char_class;
    let c = NormChar::new('\u{0301}', 0, false).with_glyph(7);
    let g = c.glyph();
    assert_eq!(g.glyph_id, 7);
    assert_eq!(g.char_class, char_class::MARK | char_class::NONSPACING_MARK);
    assert_eq!(g.combining_class, 230);
    let g = NormChar::new('\u{0903}', 0, false).glyph();
    assert_eq!(g.char_class, char_class::MARK);
    let g = NormChar::new('a', 0, false).glyph();
    assert_eq!((g.char_class, g.combining_class), (0, 0));
    let g = NormChar::new('\u{200D}', 0, false).glyph();
    assert_ne!(g.unicode_props, 0);
}

/// Normalizes `text` with a font mapping 'a'..='z' to their code
/// points, U+FE00..U+FE02 unmapped, and a format 14 subtable: 'e' with
/// VS1 has glyph 500, 'e' with VS2 is a default sequence, and 'x' with
/// VS1 maps to glyph 0 (not found).
fn normalize_uvs(text: &str, level: ClusterLevel) -> Vec<(char, u32, u32)> {
    let chars: Vec<(char, u32)> = text.char_indices().map(|(i, c)| (c, i as u32)).collect();
    normalize_uvs_clustered(&chars, level)
}

/// [`normalize_uvs`] over characters with the given clusters.
fn normalize_uvs_clustered(chars: &[(char, u32)], level: ClusterLevel) -> Vec<(char, u32, u32)> {
    normalize_uvs_with(chars, level, None)
        .into_iter()
        .map(|(c, g, cl, _, _)| (c, g, cl))
        .collect()
}

/// [`normalize_uvs_clustered`] with a not-found variation selector
/// glyph. The last two fields say whether the glyph is still default
/// ignorable and whether it is an unresolved variation selector.
fn normalize_uvs_with(
    chars: &[(char, u32)],
    level: ClusterLevel,
    not_found: Option<u32>,
) -> Vec<(char, u32, u32, bool, bool)> {
    use crate::tables::cmap::build_format14;
    let uvs = build_format14(&[
        (0xFE00, &[], &[(0x65, 500), (0x78, 0)]),
        (0xFE01, &[(0x65, 0)], &[]),
    ]);
    let bytes = build_cmap_wrapper(&[(0, 5, uvs), (3, 10, build_format12(&[(0x61, 0x7A, 0x61)]))]);
    let cmap = Cmap::parse(&bytes).unwrap();
    let normalizer = Normalizer {
        cmap: &cmap,
        shaper: Shaper::Default,
        has_gpos_mark: true,
        level,
        recategorize_marks: false,
        not_found_variation_selector: not_found,
    };
    let chars: Vec<NormChar> = chars
        .iter()
        .map(|&(c, cluster)| NormChar::new(c, cluster, false))
        .collect();
    normalizer
        .run(&chars)
        .iter()
        .map(|c| {
            let g = c.glyph();
            let ignorable = g.unicode_props & unicode_prop::DEFAULT_IGNORABLE != 0;
            let unresolved = g.char_class & char_class::UNRESOLVED_SELECTOR != 0;
            (c.ch, c.glyph, c.cluster, ignorable, unresolved)
        })
        .collect()
}

#[test]
fn a_variation_sequence_takes_its_glyph_and_drops_the_selector() {
    let level = ClusterLevel::MonotoneCharacters;
    // Its own glyph.
    let out = normalize_uvs("ae\u{FE00}b", level);
    assert_eq!(out, [('a', 0x61, 0), ('e', 500, 1), ('b', 0x62, 5)]);
    // A default sequence: the base glyph.
    let out = normalize_uvs("e\u{FE01}", level);
    assert_eq!(out, [('e', 0x65, 0)]);
    // A selector after a resolved sequence maps on its own.
    let out = normalize_uvs("e\u{FE00}\u{FE01}", level);
    assert_eq!(out, [('e', 500, 0), ('\u{FE01}', 0, 4)]);
}

#[test]
fn an_unlisted_variation_sequence_maps_both_characters() {
    let level = ClusterLevel::MonotoneCharacters;
    let out = normalize_uvs("e\u{FE02}", level);
    assert_eq!(out, [('e', 0x65, 0), ('\u{FE02}', 0, 1)]);
    // A mapping to glyph 0 is not found.
    let out = normalize_uvs("x\u{FE00}", level);
    assert_eq!(out, [('x', 0x78, 0), ('\u{FE00}', 0, 1)]);
    // Only the character right before the selector is looked up.
    let out = normalize_uvs("e\u{0301}\u{FE00}", level);
    assert_eq!(
        out,
        [('e', 0x65, 0), ('\u{0301}', 0, 1), ('\u{FE00}', 0, 3)]
    );
}

#[test]
fn a_variation_sequence_merges_clusters_only_at_monotone_levels() {
    // The glyph keeps the base's cluster at every level. A mark that
    // shares the selector's cluster (as two characters split from one
    // would) joins the base's cluster only at the monotone levels, as
    // HarfBuzz's `merge_clusters` does.
    let chars = [('e', 0), ('\u{FE00}', 1), ('\u{0301}', 1)];
    for (level, mark_cluster) in [
        (ClusterLevel::MonotoneGraphemes, 0),
        (ClusterLevel::MonotoneCharacters, 0),
        (ClusterLevel::Characters, 1),
        (ClusterLevel::Graphemes, 1),
    ] {
        let out = normalize_uvs_clustered(&chars, level);
        assert_eq!(
            out,
            [('e', 500, 0), ('\u{0301}', 0, mark_cluster)],
            "{level:?}"
        );
    }
}

#[test]
fn an_unresolved_selector_is_no_mark_and_shows_when_asked() {
    let level = ClusterLevel::MonotoneCharacters;
    let chars = |text: &str| -> Vec<(char, u32)> {
        text.char_indices().map(|(i, c)| (c, i as u32)).collect()
    };
    // Unset: the selector maps on its own and stays default ignorable,
    // but HarfBuzz no longer counts it as a mark.
    let out = normalize_uvs_with(&chars("e\u{FE02}"), level, None);
    assert_eq!(
        out,
        [('e', 0x65, 0, false, false), ('\u{FE02}', 0, 1, true, true)]
    );
    // Set: no longer ignorable, so neither hidden nor removed. The glyph
    // is swapped in after positioning.
    let out = normalize_uvs_with(&chars("e\u{FE02}"), level, Some(7));
    assert_eq!(
        out,
        [
            ('e', 0x65, 0, false, false),
            ('\u{FE02}', 0, 1, false, true)
        ]
    );
    // Only the selector right after the base: a further one is ignorable.
    let out = normalize_uvs_with(&chars("x\u{FE00}\u{FE01}"), level, Some(7));
    assert_eq!(
        out,
        [
            ('x', 0x78, 0, false, false),
            ('\u{FE00}', 0, 1, false, true),
            ('\u{FE01}', 0, 4, true, false)
        ]
    );
    // A resolved sequence leaves no selector.
    let out = normalize_uvs_with(&chars("e\u{FE00}"), level, Some(7));
    assert_eq!(out, [('e', 500, 0, false, false)]);
}

#[test]
fn show_variation_selectors_swaps_in_the_glyph_with_no_position() {
    use crate::buffer::Glyph;
    let mut selector = Glyph::new(0, 1);
    selector.char_class = char_class::UNRESOLVED_SELECTOR;
    selector.x_advance = 1000;
    selector.x_offset = -5;
    selector.y_offset = 3;
    let mut base = Glyph::new(4, 0);
    base.x_advance = 563;
    let mut glyphs = [base, selector];
    super::show_variation_selectors(&mut glyphs, None);
    assert_eq!(glyphs[1].glyph_id, 0);
    assert_eq!(glyphs[1].x_advance, 1000);
    super::show_variation_selectors(&mut glyphs, Some(9));
    assert_eq!(glyphs[0], base);
    let g = glyphs[1];
    assert_eq!(
        (g.glyph_id, g.cluster, g.x_advance, g.x_offset, g.y_offset),
        (9, 1, 0, 0, 0)
    );
}
