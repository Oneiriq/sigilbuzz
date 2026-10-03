//! GPOS FeatureVariations against HarfBuzz and rustybuzz.
//!
//! `tests/fonts/NotoSansKR-Palt-Subset.ttf` keeps the GPOS 1.1 of Noto
//! Sans KR, whose one FeatureVariations record holds while `wght` is in
//! `[0.77899, 1.0]` (normalized, after `avar`, so from about user-space
//! 699 on). The record gives every `palt` feature lookups 2 and 6
//! instead of lookup 2 alone, and every `vpal` lookups 3 and 7 instead
//! of 3. Lookup 6 tightens the kana and fullwidth punctuation further
//! at heavy weights.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz
//! 0.56.2, `hb.shape` with `guess_segment_properties`): glyph id,
//! cluster, x advance, x offset, y offset. rustybuzz 0.20, which reads
//! FeatureVariations condition format 1, agrees on the user-space
//! cases, and the tests check that too.

use rustybuzz::ttf_parser::Tag;
use rustybuzz::{Face as RbFace, UnicodeBuffer, Variation};
use sigilbuzz::{shape, Blob, Buffer, Face, Feature, Font};

const FONT: &[u8] = include_bytes!("fonts/NotoSansKR-Palt-Subset.ttf");

type Row = (u32, u32, i32, i32, i32);

const PALT: Feature = Feature {
    tag: *b"palt",
    value: 1,
};

/// Hiragana, katakana, and CJK punctuation.
const KANA: &str = concat!(
    "\u{3042}\u{3044}\u{3046}\u{3048}\u{304A}\u{3001}\u{300C}",
    "\u{30AB}\u{30BF}\u{30AB}\u{30CA}\u{300D}\u{3002}"
);

/// Katakana between fullwidth punctuation.
const FULLWIDTH: &str = "\u{FF08}\u{30C6}\u{30B9}\u{30C8}\u{FF09}\u{FF01}\u{FF1F}";

/// HarfBuzz 14.5.0 with `palt` at user-space `wght` values.
const PALT_USER: &[(f32, &str, &[Row])] = &[
    (
        100.0,
        KANA,
        &[
            (15, 0, 971, -14, 0),
            (16, 3, 944, -17, 0),
            (17, 6, 831, -75, 0),
            (18, 9, 905, -34, 0),
            (19, 12, 959, -21, 0),
            (11, 15, 500, -9, 0),
            (13, 18, 500, -481, 0),
            (20, 21, 926, -24, 0),
            (22, 24, 874, -46, 0),
            (20, 27, 926, -24, 0),
            (25, 30, 955, -25, 0),
            (14, 33, 500, -19, 0),
            (12, 36, 500, 11, 0),
        ],
    ),
    (
        100.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -464, 0),
            (23, 3, 908, -42, 0),
            (21, 6, 899, -50, 0),
            (24, 9, 770, -149, 0),
            (36, 12, 500, -36, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 717, -134, 0),
        ],
    ),
    (
        400.0,
        KANA,
        &[
            (15, 0, 971, -14, 0),
            (16, 3, 944, -17, 0),
            (17, 6, 831, -75, 0),
            (18, 9, 905, -34, 0),
            (19, 12, 959, -21, 0),
            (11, 15, 500, -9, 0),
            (13, 18, 500, -481, 0),
            (20, 21, 926, -24, 0),
            (22, 24, 874, -46, 0),
            (20, 27, 926, -24, 0),
            (25, 30, 955, -25, 0),
            (14, 33, 500, -19, 0),
            (12, 36, 500, 11, 0),
        ],
    ),
    (
        400.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -464, 0),
            (23, 3, 908, -42, 0),
            (21, 6, 899, -50, 0),
            (24, 9, 770, -149, 0),
            (36, 12, 500, -36, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 717, -134, 0),
        ],
    ),
    (
        698.0,
        KANA,
        &[
            (15, 0, 971, -14, 0),
            (16, 3, 944, -17, 0),
            (17, 6, 831, -75, 0),
            (18, 9, 905, -34, 0),
            (19, 12, 959, -21, 0),
            (11, 15, 500, -9, 0),
            (13, 18, 500, -481, 0),
            (20, 21, 926, -24, 0),
            (22, 24, 874, -46, 0),
            (20, 27, 926, -24, 0),
            (25, 30, 955, -25, 0),
            (14, 33, 500, -19, 0),
            (12, 36, 500, 11, 0),
        ],
    ),
    (
        698.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -464, 0),
            (23, 3, 908, -42, 0),
            (21, 6, 899, -50, 0),
            (24, 9, 770, -149, 0),
            (36, 12, 500, -36, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 717, -134, 0),
        ],
    ),
    (
        699.0,
        KANA,
        &[
            (15, 0, 971, -14, 0),
            (16, 3, 944, -17, 0),
            (17, 6, 831, -75, 0),
            (18, 9, 905, -34, 0),
            (19, 12, 959, -21, 0),
            (11, 15, 500, -9, 0),
            (13, 18, 500, -481, 0),
            (20, 21, 926, -24, 0),
            (22, 24, 874, -46, 0),
            (20, 27, 926, -24, 0),
            (25, 30, 955, -25, 0),
            (14, 33, 500, -19, 0),
            (12, 36, 500, 11, 0),
        ],
    ),
    (
        699.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -464, 0),
            (23, 3, 908, -42, 0),
            (21, 6, 899, -50, 0),
            (24, 9, 770, -149, 0),
            (36, 12, 500, -36, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 717, -134, 0),
        ],
    ),
    (
        700.0,
        KANA,
        &[
            (15, 0, 986, -7, 0),
            (16, 3, 987, -5, 0),
            (17, 6, 830, -71, 0),
            (18, 9, 894, -70, 0),
            (19, 12, 968, -11, 0),
            (11, 15, 500, -3, 0),
            (13, 18, 500, -485, 0),
            (20, 21, 924, -38, 0),
            (22, 24, 914, -27, 0),
            (20, 27, 924, -38, 0),
            (25, 30, 929, -24, 0),
            (14, 33, 500, -15, 0),
            (12, 36, 500, 10, 0),
        ],
    ),
    (
        700.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -456, 0),
            (23, 3, 924, -43, 0),
            (21, 6, 941, -43, 0),
            (24, 9, 838, -108, 0),
            (36, 12, 500, -44, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 696, -157, 0),
        ],
    ),
    (
        701.0,
        KANA,
        &[
            (15, 0, 986, -7, 0),
            (16, 3, 987, -5, 0),
            (17, 6, 830, -71, 0),
            (18, 9, 894, -70, 0),
            (19, 12, 968, -11, 0),
            (11, 15, 500, -3, 0),
            (13, 18, 500, -485, 0),
            (20, 21, 924, -38, 0),
            (22, 24, 914, -27, 0),
            (20, 27, 924, -38, 0),
            (25, 30, 929, -24, 0),
            (14, 33, 500, -15, 0),
            (12, 36, 500, 10, 0),
        ],
    ),
    (
        701.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -456, 0),
            (23, 3, 924, -43, 0),
            (21, 6, 941, -43, 0),
            (24, 9, 838, -108, 0),
            (36, 12, 500, -44, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 696, -157, 0),
        ],
    ),
    (
        900.0,
        KANA,
        &[
            (15, 0, 986, -7, 0),
            (16, 3, 987, -5, 0),
            (17, 6, 830, -71, 0),
            (18, 9, 894, -70, 0),
            (19, 12, 968, -11, 0),
            (11, 15, 500, -3, 0),
            (13, 18, 500, -485, 0),
            (20, 21, 924, -38, 0),
            (22, 24, 914, -27, 0),
            (20, 27, 924, -38, 0),
            (25, 30, 929, -24, 0),
            (14, 33, 500, -15, 0),
            (12, 36, 500, 10, 0),
        ],
    ),
    (
        900.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -456, 0),
            (23, 3, 924, -43, 0),
            (21, 6, 941, -43, 0),
            (24, 9, 838, -108, 0),
            (36, 12, 500, -44, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 696, -157, 0),
        ],
    ),
];

/// HarfBuzz 14.5.0 with `palt` at normalized coordinates: the default,
/// one F2DOT14 step below the record's range, its lower bound, and 1.
const PALT_NORMALIZED: &[(f32, &str, &[Row])] = &[
    (
        0.0,
        KANA,
        &[
            (15, 0, 971, -14, 0),
            (16, 3, 944, -17, 0),
            (17, 6, 831, -75, 0),
            (18, 9, 905, -34, 0),
            (19, 12, 959, -21, 0),
            (11, 15, 500, -9, 0),
            (13, 18, 500, -481, 0),
            (20, 21, 926, -24, 0),
            (22, 24, 874, -46, 0),
            (20, 27, 926, -24, 0),
            (25, 30, 955, -25, 0),
            (14, 33, 500, -19, 0),
            (12, 36, 500, 11, 0),
        ],
    ),
    (
        0.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -464, 0),
            (23, 3, 908, -42, 0),
            (21, 6, 899, -50, 0),
            (24, 9, 770, -149, 0),
            (36, 12, 500, -36, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 717, -134, 0),
        ],
    ),
    (
        12762.0 / 16384.0,
        KANA,
        &[
            (15, 0, 971, -14, 0),
            (16, 3, 944, -17, 0),
            (17, 6, 831, -75, 0),
            (18, 9, 905, -34, 0),
            (19, 12, 959, -21, 0),
            (11, 15, 500, -9, 0),
            (13, 18, 500, -481, 0),
            (20, 21, 926, -24, 0),
            (22, 24, 874, -46, 0),
            (20, 27, 926, -24, 0),
            (25, 30, 955, -25, 0),
            (14, 33, 500, -19, 0),
            (12, 36, 500, 11, 0),
        ],
    ),
    (
        12762.0 / 16384.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -464, 0),
            (23, 3, 908, -42, 0),
            (21, 6, 899, -50, 0),
            (24, 9, 770, -149, 0),
            (36, 12, 500, -36, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 717, -134, 0),
        ],
    ),
    (
        12763.0 / 16384.0,
        KANA,
        &[
            (15, 0, 986, -7, 0),
            (16, 3, 987, -5, 0),
            (17, 6, 830, -71, 0),
            (18, 9, 894, -70, 0),
            (19, 12, 968, -11, 0),
            (11, 15, 500, -3, 0),
            (13, 18, 500, -485, 0),
            (20, 21, 924, -38, 0),
            (22, 24, 914, -27, 0),
            (20, 27, 924, -38, 0),
            (25, 30, 929, -24, 0),
            (14, 33, 500, -15, 0),
            (12, 36, 500, 10, 0),
        ],
    ),
    (
        12763.0 / 16384.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -456, 0),
            (23, 3, 924, -43, 0),
            (21, 6, 941, -43, 0),
            (24, 9, 838, -108, 0),
            (36, 12, 500, -44, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 696, -157, 0),
        ],
    ),
    (
        1.0,
        KANA,
        &[
            (15, 0, 986, -7, 0),
            (16, 3, 987, -5, 0),
            (17, 6, 830, -71, 0),
            (18, 9, 894, -70, 0),
            (19, 12, 968, -11, 0),
            (11, 15, 500, -3, 0),
            (13, 18, 500, -485, 0),
            (20, 21, 924, -38, 0),
            (22, 24, 914, -27, 0),
            (20, 27, 924, -38, 0),
            (25, 30, 929, -24, 0),
            (14, 33, 500, -15, 0),
            (12, 36, 500, 10, 0),
        ],
    ),
    (
        1.0,
        FULLWIDTH,
        &[
            (35, 0, 500, -456, 0),
            (23, 3, 924, -43, 0),
            (21, 6, 941, -43, 0),
            (24, 9, 838, -108, 0),
            (36, 12, 500, -44, 0),
            (34, 15, 500, -250, 0),
            (37, 18, 696, -157, 0),
        ],
    ),
];

/// HarfBuzz 14.5.0 at `wght` 900 without `palt`.
const NO_PALT: &[(&str, &[Row])] = &[
    (
        KANA,
        &[
            (15, 0, 1000, 0, 0),
            (16, 3, 1000, 0, 0),
            (17, 6, 1000, 0, 0),
            (18, 9, 1000, 0, 0),
            (19, 12, 1000, 0, 0),
            (11, 15, 1000, 0, 0),
            (13, 18, 1000, 0, 0),
            (20, 21, 1000, 0, 0),
            (22, 24, 1000, 0, 0),
            (20, 27, 1000, 0, 0),
            (25, 30, 1000, 0, 0),
            (14, 33, 1000, 0, 0),
            (12, 36, 1000, 0, 0),
        ],
    ),
    (
        FULLWIDTH,
        &[
            (35, 0, 1000, 0, 0),
            (23, 3, 1000, 0, 0),
            (21, 6, 1000, 0, 0),
            (24, 9, 1000, 0, 0),
            (36, 12, 1000, 0, 0),
            (34, 15, 1000, 0, 0),
            (37, 18, 1000, 0, 0),
        ],
    ),
];

/// Normalized coordinates for user-space `wght` `value`, through
/// `fvar` and `avar`.
fn normalize_wght(face: &Face<'_>, value: f32) -> Vec<f32> {
    let fvar = face.fvar().unwrap().expect("Noto Sans KR has fvar");
    let normalized = fvar.normalize_coords(&[value]);
    match face.avar().unwrap() {
        Some(avar) => avar.remap_all(&normalized),
        None => normalized,
    }
}

/// sigilbuzz's output at user-space `wght` `user`, or else at the
/// normalized coordinates `coords`.
fn sigilbuzz_rows(user: Option<f32>, coords: &[f32], text: &str, features: &[Feature]) -> Vec<Row> {
    font_rows(FONT, user, coords, text, features)
}

/// [`sigilbuzz_rows`] for the font `data`.
fn font_rows(
    data: &[u8],
    user: Option<f32>,
    coords: &[f32],
    text: &str,
    features: &[Feature],
) -> Vec<Row> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).unwrap();
    let user_coords = user.map(|w| normalize_wght(&face, w));
    let coords = user_coords.as_deref().unwrap_or(coords);
    let font = Font::new(face, 1000.0).with_coords(coords);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    shape(&font, &buffer, features)
        .unwrap()
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.cluster, g.x_advance, g.x_offset, g.y_offset))
        .collect()
}

fn rustybuzz_rows(user: f32, text: &str) -> Vec<Row> {
    let mut face = RbFace::from_slice(FONT, 0).unwrap();
    face.set_variations(&[Variation {
        tag: Tag::from_bytes(b"wght"),
        value: user,
    }]);
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let palt = rustybuzz::Feature::new(Tag::from_bytes(b"palt"), 1, ..);
    let out = rustybuzz::shape(&face, &[palt], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, i.cluster, p.x_advance, p.x_offset, p.y_offset))
        .collect()
}

#[test]
fn the_subset_keeps_the_feature_variations() {
    let blob = Blob::new(FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let gpos = face.gpos().unwrap().expect("GPOS");
    let variations = gpos.feature_variations().unwrap().expect("GPOS 1.1");
    assert_eq!(variations.len(), 1);
    let substitution = variations.substitution(0).unwrap();
    let (index, alternate) = substitution.get(0).unwrap();
    let (tag, default) = gpos.feature_list().get(index).unwrap();
    assert_eq!(tag, *b"palt");
    assert_eq!(default.lookup_indices().collect::<Vec<_>>(), [2]);
    assert_eq!(alternate.lookup_indices().collect::<Vec<_>>(), [2, 6]);
}

#[test]
fn gpos_palt_follows_harfbuzz_in_user_space() {
    for &(wght, text, expected) in PALT_USER {
        let rows = sigilbuzz_rows(Some(wght), &[], text, &[PALT]);
        assert_eq!(rows, expected, "wght {wght} {text:?}");
    }
}

#[test]
fn gpos_palt_follows_rustybuzz_in_user_space() {
    for &(wght, text, _) in PALT_USER {
        let rows = sigilbuzz_rows(Some(wght), &[], text, &[PALT]);
        assert_eq!(rows, rustybuzz_rows(wght, text), "wght {wght} {text:?}");
    }
}

#[test]
fn gpos_palt_follows_harfbuzz_at_normalized_coordinates() {
    for &(coord, text, expected) in PALT_NORMALIZED {
        let rows = sigilbuzz_rows(None, &[coord], text, &[PALT]);
        assert_eq!(rows, expected, "coord {coord} {text:?}");
    }
}

#[test]
fn the_default_instance_keeps_the_default_lookups() {
    for &(coord, text, expected) in PALT_NORMALIZED {
        if coord == 0.0 {
            assert_eq!(sigilbuzz_rows(None, &[], text, &[PALT]), expected);
        }
    }
}

#[test]
fn a_substituted_feature_that_is_off_applies_nothing() {
    for &(text, expected) in NO_PALT {
        assert_eq!(sigilbuzz_rows(Some(900.0), &[], text, &[]), expected);
    }
}

/// A copy of `font` with the tables of `overrides` in place of its own
/// or added to it.
fn with_tables(font: &[u8], overrides: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let face = Face::parse_bytes(font, 0).unwrap();
    let mut tables: Vec<([u8; 4], Vec<u8>)> = face
        .records()
        .iter()
        .filter(|r| overrides.iter().all(|(tag, _)| *tag != r.tag))
        .map(|r| (r.tag, face.table_bytes(r.tag).unwrap().to_vec()))
        .collect();
    tables.extend(overrides.iter().cloned());
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

#[test]
fn a_gpos_whose_1_1_header_is_cut_short_is_left_out() {
    // A 12-byte GPOS 1.1 whose ScriptList, FeatureList, and LookupList
    // share the empty list at byte 10, where `featureVariationsOffset`
    // would start, with no room for that field. HarfBuzz 14.5.0
    // rejects the table and shapes without it, as sigilbuzz did before
    // it read FeatureVariations.
    let short = vec![0, 1, 0, 1, 0, 10, 0, 10, 0, 10, 0, 0];
    let font = with_tables(FONT, &[(*b"GPOS", short)]);
    for &(text, expected) in NO_PALT {
        assert_eq!(font_rows(&font, Some(900.0), &[], text, &[PALT]), expected);
    }
}
