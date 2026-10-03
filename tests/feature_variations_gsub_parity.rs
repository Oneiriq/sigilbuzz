//! GSUB FeatureVariations and `rvrn` against HarfBuzz.
//! `tests/feature_variations_gpos_parity.rs` covers GPOS.
//!
//! Rubik Variable (`tests/fixtures/rubik_vf.ttf`) has a GSUB 1.1 whose
//! one FeatureVariations record holds while `wght` is in
//! `[0.40625, 1.0]` (normalized, after `avar`). The record gives
//! `rvrn`, which has no lookups of its own, lookup 0: it swaps the
//! euro, hryvnia, and yen signs (glyphs 1022, 1023, 1030) for heavier
//! drawings (1171, 1172, 1173). `avar` maps user-space `wght` 500 to
//! 0.40625 exactly, so 499 and 500 sit on either side of the boundary.
//!
//! Every expectation here is HarfBuzz 14.5.0's output (uharfbuzz
//! 0.56.2, `hb.shape` with `guess_segment_properties`): glyph id,
//! cluster, x advance, x offset, y offset. rustybuzz 0.20 is not a
//! reference here: it maps the unrounded `fvar` coordinate through
//! `avar`, where HarfBuzz rounds it to 16.16 first and to F2DOT14
//! after, so at `wght` 700 two of its advances are 1 unit off.

use std::time::{Duration, Instant};

use sigilbuzz::{shape, Blob, Buffer, Face, Feature, Font};

const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");

type Row = (u32, u32, i32, i32, i32);

/// HarfBuzz 14.5.0 at user-space `wght` values (`hb_font_set_variations`).
const USER: &[(f32, &str, &[Row])] = &[
    (
        300.0,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1022, 0, 726, 0, 0),
            (861, 3, 572, 0, 0),
            (928, 4, 258, 0, 0),
            (1023, 5, 600, 0, 0),
            (863, 8, 487, 0, 0),
            (928, 9, 258, 0, 0),
            (1030, 10, 586, 0, 0),
            (865, 12, 572, 0, 0),
        ],
    ),
    (
        300.0,
        "A\u{20AC}B",
        &[(1, 0, 650, 0, 0), (1022, 1, 726, 0, 0), (13, 4, 653, 0, 0)],
    ),
    (
        300.0,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1030, 0, 586, 0, 0),
            (1022, 2, 726, 0, 0),
            (1023, 5, 600, 0, 0),
        ],
    ),
    (
        400.0,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1022, 0, 744, 0, 0),
            (861, 3, 594, 0, 0),
            (928, 4, 245, 0, 0),
            (1023, 5, 623, 0, 0),
            (863, 8, 513, 0, 0),
            (928, 9, 245, 0, 0),
            (1030, 10, 617, 0, 0),
            (865, 12, 593, 0, 0),
        ],
    ),
    (
        400.0,
        "A\u{20AC}B",
        &[(1, 0, 670, 0, 0), (1022, 1, 744, 0, 0), (13, 4, 668, 0, 0)],
    ),
    (
        400.0,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1030, 0, 617, 0, 0),
            (1022, 2, 744, 0, 0),
            (1023, 5, 623, 0, 0),
        ],
    ),
    (
        499.0,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1022, 0, 764, 0, 0),
            (861, 3, 620, 0, 0),
            (928, 4, 229, 0, 0),
            (1023, 5, 650, 0, 0),
            (863, 8, 543, 0, 0),
            (928, 9, 229, 0, 0),
            (1030, 10, 652, 0, 0),
            (865, 12, 617, 0, 0),
        ],
    ),
    (
        499.0,
        "A\u{20AC}B",
        &[(1, 0, 692, 0, 0), (1022, 1, 764, 0, 0), (13, 4, 686, 0, 0)],
    ),
    (
        499.0,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1030, 0, 652, 0, 0),
            (1022, 2, 764, 0, 0),
            (1023, 5, 650, 0, 0),
        ],
    ),
    (
        500.0,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1171, 0, 764, 0, 0),
            (861, 3, 620, 0, 0),
            (928, 4, 229, 0, 0),
            (1172, 5, 650, 0, 0),
            (863, 8, 543, 0, 0),
            (928, 9, 229, 0, 0),
            (1173, 10, 653, 0, 0),
            (865, 12, 617, 0, 0),
        ],
    ),
    (
        500.0,
        "A\u{20AC}B",
        &[(1, 0, 693, 0, 0), (1171, 1, 764, 0, 0), (13, 4, 686, 0, 0)],
    ),
    (
        500.0,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1173, 0, 653, 0, 0),
            (1171, 2, 764, 0, 0),
            (1172, 5, 650, 0, 0),
        ],
    ),
    (
        501.0,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1171, 0, 764, 0, 0),
            (861, 3, 620, 0, 0),
            (928, 4, 229, 0, 0),
            (1172, 5, 651, 0, 0),
            (863, 8, 543, 0, 0),
            (928, 9, 229, 0, 0),
            (1173, 10, 653, 0, 0),
            (865, 12, 617, 0, 0),
        ],
    ),
    (
        501.0,
        "A\u{20AC}B",
        &[(1, 0, 693, 0, 0), (1171, 1, 764, 0, 0), (13, 4, 686, 0, 0)],
    ),
    (
        501.0,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1173, 0, 653, 0, 0),
            (1171, 2, 764, 0, 0),
            (1172, 5, 651, 0, 0),
        ],
    ),
    (
        700.0,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1171, 0, 785, 0, 0),
            (861, 3, 646, 0, 0),
            (928, 4, 213, 0, 0),
            (1172, 5, 678, 0, 0),
            (863, 8, 573, 0, 0),
            (928, 9, 213, 0, 0),
            (1173, 10, 689, 0, 0),
            (865, 12, 641, 0, 0),
        ],
    ),
    (
        700.0,
        "A\u{20AC}B",
        &[(1, 0, 716, 0, 0), (1171, 1, 785, 0, 0), (13, 4, 704, 0, 0)],
    ),
    (
        700.0,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1173, 0, 689, 0, 0),
            (1171, 2, 785, 0, 0),
            (1172, 5, 678, 0, 0),
        ],
    ),
    (
        900.0,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1171, 0, 820, 0, 0),
            (861, 3, 690, 0, 0),
            (928, 4, 186, 0, 0),
            (1172, 5, 724, 0, 0),
            (863, 8, 625, 0, 0),
            (928, 9, 186, 0, 0),
            (1173, 10, 750, 0, 0),
            (865, 12, 683, 0, 0),
        ],
    ),
    (
        900.0,
        "A\u{20AC}B",
        &[(1, 0, 755, 0, 0), (1171, 1, 820, 0, 0), (13, 4, 734, 0, 0)],
    ),
    (
        900.0,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1173, 0, 750, 0, 0),
            (1171, 2, 820, 0, 0),
            (1172, 5, 724, 0, 0),
        ],
    ),
];

/// HarfBuzz 14.5.0 at normalized coordinates
/// (`hb_font_set_var_coords_normalized`).
const NORMALIZED: &[(f32, &str, &[Row])] = &[
    (
        -0.5,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1022, 0, 726, 0, 0),
            (861, 3, 572, 0, 0),
            (928, 4, 258, 0, 0),
            (1023, 5, 600, 0, 0),
            (863, 8, 487, 0, 0),
            (928, 9, 258, 0, 0),
            (1030, 10, 586, 0, 0),
            (865, 12, 572, 0, 0),
        ],
    ),
    (
        -0.5,
        "A\u{20AC}B",
        &[(1, 0, 650, 0, 0), (1022, 1, 726, 0, 0), (13, 4, 653, 0, 0)],
    ),
    (
        -0.5,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1030, 0, 586, 0, 0),
            (1022, 2, 726, 0, 0),
            (1023, 5, 600, 0, 0),
        ],
    ),
    (
        0.0,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1022, 0, 726, 0, 0),
            (861, 3, 572, 0, 0),
            (928, 4, 258, 0, 0),
            (1023, 5, 600, 0, 0),
            (863, 8, 487, 0, 0),
            (928, 9, 258, 0, 0),
            (1030, 10, 586, 0, 0),
            (865, 12, 572, 0, 0),
        ],
    ),
    (
        0.0,
        "A\u{20AC}B",
        &[(1, 0, 650, 0, 0), (1022, 1, 726, 0, 0), (13, 4, 653, 0, 0)],
    ),
    (
        0.0,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1030, 0, 586, 0, 0),
            (1022, 2, 726, 0, 0),
            (1023, 5, 600, 0, 0),
        ],
    ),
    (
        0.40619,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1022, 0, 764, 0, 0),
            (861, 3, 620, 0, 0),
            (928, 4, 229, 0, 0),
            (1023, 5, 650, 0, 0),
            (863, 8, 543, 0, 0),
            (928, 9, 229, 0, 0),
            (1030, 10, 653, 0, 0),
            (865, 12, 617, 0, 0),
        ],
    ),
    (
        0.40619,
        "A\u{20AC}B",
        &[(1, 0, 693, 0, 0), (1022, 1, 764, 0, 0), (13, 4, 686, 0, 0)],
    ),
    (
        0.40619,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1030, 0, 653, 0, 0),
            (1022, 2, 764, 0, 0),
            (1023, 5, 650, 0, 0),
        ],
    ),
    (
        0.40625,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1171, 0, 764, 0, 0),
            (861, 3, 620, 0, 0),
            (928, 4, 229, 0, 0),
            (1172, 5, 650, 0, 0),
            (863, 8, 543, 0, 0),
            (928, 9, 229, 0, 0),
            (1173, 10, 653, 0, 0),
            (865, 12, 617, 0, 0),
        ],
    ),
    (
        0.40625,
        "A\u{20AC}B",
        &[(1, 0, 693, 0, 0), (1171, 1, 764, 0, 0), (13, 4, 686, 0, 0)],
    ),
    (
        0.40625,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1173, 0, 653, 0, 0),
            (1171, 2, 764, 0, 0),
            (1172, 5, 650, 0, 0),
        ],
    ),
    (
        0.5,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1171, 0, 773, 0, 0),
            (861, 3, 631, 0, 0),
            (928, 4, 222, 0, 0),
            (1172, 5, 662, 0, 0),
            (863, 8, 556, 0, 0),
            (928, 9, 222, 0, 0),
            (1173, 10, 668, 0, 0),
            (865, 12, 628, 0, 0),
        ],
    ),
    (
        0.5,
        "A\u{20AC}B",
        &[(1, 0, 703, 0, 0), (1171, 1, 773, 0, 0), (13, 4, 694, 0, 0)],
    ),
    (
        0.5,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1173, 0, 668, 0, 0),
            (1171, 2, 773, 0, 0),
            (1172, 5, 662, 0, 0),
        ],
    ),
    (
        1.0,
        "\u{20AC}5 \u{20B4}7 \u{A5}9",
        &[
            (1171, 0, 820, 0, 0),
            (861, 3, 690, 0, 0),
            (928, 4, 186, 0, 0),
            (1172, 5, 724, 0, 0),
            (863, 8, 625, 0, 0),
            (928, 9, 186, 0, 0),
            (1173, 10, 750, 0, 0),
            (865, 12, 683, 0, 0),
        ],
    ),
    (
        1.0,
        "A\u{20AC}B",
        &[(1, 0, 755, 0, 0), (1171, 1, 820, 0, 0), (13, 4, 734, 0, 0)],
    ),
    (
        1.0,
        "\u{A5}\u{20AC}\u{20B4}",
        &[
            (1173, 0, 750, 0, 0),
            (1171, 2, 820, 0, 0),
            (1172, 5, 724, 0, 0),
        ],
    ),
];

/// HarfBuzz 14.5.0 at `wght` 900 with `rvrn` turned off.
const RVRN_OFF: (&str, &[Row]) = (
    "\u{20AC}5 \u{20B4}7 \u{A5}9",
    &[
        (1022, 0, 820, 0, 0),
        (861, 3, 690, 0, 0),
        (928, 4, 186, 0, 0),
        (1023, 5, 724, 0, 0),
        (863, 8, 625, 0, 0),
        (928, 9, 186, 0, 0),
        (1030, 10, 750, 0, 0),
        (865, 12, 683, 0, 0),
    ],
);

/// Normalized coordinates for user-space `wght` `value`, through
/// `fvar` and `avar`.
fn normalize_wght(face: &Face<'_>, value: f32) -> Vec<f32> {
    let fvar = face.fvar().unwrap().expect("Rubik has fvar");
    let normalized = fvar.normalize_coords(&[value]);
    match face.avar().unwrap() {
        Some(avar) => avar.remap_all(&normalized),
        None => normalized,
    }
}

fn sigilbuzz_rows(
    coords: Option<&[f32]>,
    user: Option<f32>,
    text: &str,
    features: &[Feature],
) -> Vec<Row> {
    font_rows(RUBIK, coords, user, text, features)
}

/// sigilbuzz's output for the font `data` at user-space `wght` `user`,
/// or else at the normalized coordinates `coords`.
fn font_rows(
    data: &[u8],
    coords: Option<&[f32]>,
    user: Option<f32>,
    text: &str,
    features: &[Feature],
) -> Vec<Row> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).unwrap();
    let user_coords = user.map(|w| normalize_wght(&face, w));
    let coords = user_coords.as_deref().or(coords).unwrap_or(&[]);
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

#[test]
fn gsub_rvrn_follows_harfbuzz_in_user_space() {
    // The coordinates go through `fvar` and `avar` the way HarfBuzz's
    // `hb_ot_var_normalize_coords` takes them, rounded to 16.16 before
    // `avar` and to F2DOT14 after, so the advances match exactly too.
    for &(wght, text, expected) in USER {
        let rows = sigilbuzz_rows(None, Some(wght), text, &[]);
        assert_eq!(rows, expected, "wght {wght} {text:?}");
    }
}
#[test]
fn gsub_rvrn_follows_harfbuzz_at_normalized_coordinates() {
    for &(coord, text, expected) in NORMALIZED {
        let rows = sigilbuzz_rows(Some(&[coord]), None, text, &[]);
        assert_eq!(rows, expected, "coord {coord} {text:?}");
    }
}

#[test]
fn default_instance_selects_no_record() {
    // No coordinates at all reads every axis as 0, which is outside
    // the record's range.
    let (_, text, expected) = NORMALIZED
        .iter()
        .find(|&&(coord, _, _)| coord == 0.0)
        .copied()
        .unwrap();
    assert_eq!(sigilbuzz_rows(None, None, text, &[]), expected);
}

#[test]
fn turning_rvrn_off_keeps_the_default_glyphs() {
    let (text, expected) = RVRN_OFF;
    let off = [Feature {
        tag: *b"rvrn",
        value: 0,
    }];
    assert_eq!(sigilbuzz_rows(None, Some(900.0), text, &off), expected);
    // Turning it on changes nothing: it is on already, and applies
    // once.
    let on = [Feature {
        tag: *b"rvrn",
        value: 1,
    }];
    let (_, _, heavy) = USER
        .iter()
        .find(|&&(w, t, _)| w == 900.0 && t == text)
        .copied()
        .unwrap();
    assert_eq!(sigilbuzz_rows(None, Some(900.0), text, &on), heavy);
}

/// Rubik with the major version of its GSUB FeatureVariations set to 2.
fn rubik_with_unsupported_feature_variations() -> Vec<u8> {
    let mut data = RUBIK.to_vec();
    let u32_at = |data: &[u8], at: usize| {
        u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize
    };
    let tables = usize::from(u16::from_be_bytes([data[4], data[5]]));
    let record = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|&r| &data[r..r + 4] == b"GSUB")
        .expect("Rubik has GSUB");
    let gsub = u32_at(&data, record + 8);
    let variations = gsub + u32_at(&data, gsub + 10);
    data[variations..variations + 2].copy_from_slice(&2u16.to_be_bytes());
    data
}

#[test]
fn a_gsub_whose_feature_variations_do_not_parse_is_left_out() {
    // HarfBuzz 14.5.0 rejects the whole GSUB, so neither `numr` nor the
    // `rvrn` swap applies at `wght` 900.
    let numr = [Feature {
        tag: *b"numr",
        value: 1,
    }];
    let text = "5\u{20AC}";
    let broken = rubik_with_unsupported_feature_variations();
    assert_eq!(
        font_rows(&broken, None, Some(900.0), text, &numr),
        [(861, 0, 690, 0, 0), (1022, 1, 820, 0, 0)]
    );
    assert_eq!(
        font_rows(RUBIK, None, Some(900.0), text, &numr),
        [(893, 0, 365, 0, 0), (1171, 1, 820, 0, 0)]
    );
    // The GSUB itself still parses; its FeatureVariations do not.
    let blob = Blob::new(&broken);
    let face = Face::parse(&blob, 0).unwrap();
    let gsub = face.gsub().unwrap().expect("GSUB");
    assert!(gsub.feature_variations().is_err());
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

/// A 12-byte GSUB 1.1 whose ScriptList, FeatureList, and LookupList
/// share the empty list at byte 10, where `featureVariationsOffset`
/// would start, with no room for that field.
const SHORT_1_1_HEADER: [u8; 12] = [0, 1, 0, 1, 0, 10, 0, 10, 0, 10, 0, 0];

#[test]
fn a_gsub_whose_1_1_header_is_cut_short_is_left_out() {
    // HarfBuzz 14.5.0 rejects the table and shapes without it, as
    // sigilbuzz did before it read FeatureVariations.
    let font = with_tables(RUBIK, &[(*b"GSUB", SHORT_1_1_HEADER.to_vec())]);
    let numr = [Feature {
        tag: *b"numr",
        value: 1,
    }];
    assert_eq!(
        font_rows(&font, None, Some(900.0), "5\u{20AC}", &numr),
        [(861, 0, 690, 0, 0), (1022, 1, 820, 0, 0)]
    );
}

/// A one-record FeatureVariations whose ConditionSet names an "and" of
/// two entries that both name the next "and", `levels` deep, over one
/// value condition that holds whatever the delta of varIndex 0:
/// `2^levels` paths to it, and `2^(levels + 1)` checks counting the
/// ConditionSet. The record substitutes nothing.
fn shared_ands(levels: usize) -> Vec<u8> {
    let mut out = vec![0, 1, 0, 0, 0, 0, 0, 1];
    out.extend_from_slice(&16u32.to_be_bytes()); // ConditionSet
    out.extend_from_slice(&0u32.to_be_bytes()); // no substitution
    out.extend_from_slice(&1u16.to_be_bytes()); // conditionCount
    out.extend_from_slice(&6u32.to_be_bytes()); // the first "and"
    for _ in 0..levels {
        // Format 3, two entries, both naming the condition 9 bytes on.
        out.extend_from_slice(&[0, 3, 2, 0, 0, 9, 0, 0, 9]);
    }
    // Format 2: defaultValue 32767, varIndex 0.
    out.extend_from_slice(&[0, 2, 0x7F, 0xFF, 0, 0, 0, 0]);
    out
}

/// Rubik with `variations` in place of its GSUB FeatureVariations, at
/// the end of the GSUB.
fn rubik_with_feature_variations(variations: &[u8]) -> Vec<u8> {
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    let mut gsub = face.table_bytes(*b"GSUB").unwrap().to_vec();
    let offset = gsub.len() as u32;
    gsub[10..14].copy_from_slice(&offset.to_be_bytes());
    gsub.extend_from_slice(variations);
    with_tables(RUBIK, &[(*b"GSUB", gsub)])
}

#[test]
fn shared_conditions_cost_no_more_than_the_table_size_allows() {
    // 13 levels make 2^14 checks, which any table may need. Every one
    // of the 2^13 paths ends in a value condition that holds, so every
    // shaping call evaluates them all.
    let heavy = rubik_with_feature_variations(&shared_ands(13));
    let face = Face::parse_bytes(&heavy, 0).unwrap();
    let gsub = face.gsub().unwrap().unwrap();
    let variations = gsub.feature_variations().unwrap().unwrap();
    assert_eq!(variations.find_index(&[1.0], None), Some(0));
    // 17 levels make 2^18 checks. A 183-byte table may not need more
    // than 16384, so the GSUB is left out. HarfBuzz 14.5.0 leaves it
    // out too: its sanitizer runs out of operations on this 6 KB GSUB.
    // Both outputs below are HarfBuzz's.
    let runaway = rubik_with_feature_variations(&shared_ands(17));
    let face = Face::parse_bytes(&runaway, 0).unwrap();
    let gsub = face.gsub().unwrap().unwrap();
    assert!(matches!(
        gsub.feature_variations(),
        Err(sigilbuzz::Error::Malformed {
            context: "FeatureVariations need more checks than sigilbuzz makes",
            ..
        })
    ));
    let numr = [Feature {
        tag: *b"numr",
        value: 1,
    }];
    let text = "5\u{20AC}";
    // The record substitutes nothing, so `rvrn` swaps nothing in.
    assert_eq!(
        font_rows(&heavy, None, Some(900.0), text, &numr),
        [(893, 0, 365, 0, 0), (1022, 1, 820, 0, 0)]
    );
    assert_eq!(
        font_rows(&runaway, None, Some(900.0), text, &numr),
        [(861, 0, 690, 0, 0), (1022, 1, 820, 0, 0)]
    );
    // Every shaping call parses and evaluates the conditions again.
    // In a debug build the heavy font takes about 30 times as long as
    // Rubik itself, the most a table of its size can cost. Without the
    // size limit and the cached deltas, it took 180 times as long, and
    // the runaway font over 2000 times.
    let time = |font: &[u8]| {
        let start = Instant::now();
        for _ in 0..ROUNDS {
            font_rows(font, None, Some(900.0), text, &numr);
        }
        start.elapsed()
    };
    let plain = time(RUBIK);
    let budget = plain * 60 + Duration::from_secs(1);
    for (label, font) in [("heavy", &heavy), ("runaway", &runaway)] {
        let took = time(font);
        assert!(
            took < budget,
            "{label}: {took:?} for {ROUNDS} calls, budget {budget:?}"
        );
    }
}

/// Shaping calls each timing makes.
const ROUNDS: u32 = 20;

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

/// A ScriptList with `DFLT` and `latn`, whose default language systems
/// both list feature 0 alone.
fn one_feature_script_list() -> Vec<u8> {
    let mut out = vec![0, 2];
    out.extend_from_slice(b"DFLT\0\x0E");
    out.extend_from_slice(b"latn\0\x0E");
    // The Script both records name: a default language system right
    // after it, and no others.
    out.extend_from_slice(&[0, 4, 0, 0]);
    // No lookupOrder, no required feature, feature 0.
    out.extend_from_slice(&[0, 0, 0xFF, 0xFF, 0, 1, 0, 0]);
    out
}

/// A FeatureList of one `rvrn` feature with `lookups`.
fn rvrn_feature_list(lookups: &[u16]) -> Vec<u8> {
    let mut out = vec![0, 1];
    out.extend_from_slice(b"rvrn\0\x08");
    out.extend_from_slice(&[0, 0]); // featureParamsOffset
    out.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
    for l in lookups {
        out.extend_from_slice(&l.to_be_bytes());
    }
    out
}

/// A LookupList of one lookup of `lookup_type` with the one subtable
/// `subtable`.
fn one_lookup_list(lookup_type: u16, subtable: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 1, 0, 4];
    out.extend_from_slice(&lookup_type.to_be_bytes());
    // No lookupFlag, one subtable, right after the lookup.
    out.extend_from_slice(&[0, 0, 0, 1, 0, 8]);
    out.extend_from_slice(subtable);
    out
}

/// A GSUB or GPOS of the three lists, version 1.1 with `variations`
/// when there are some.
fn layout_table(
    scripts: &[u8],
    features: &[u8],
    lookups: &[u8],
    variations: Option<&[u8]>,
) -> Vec<u8> {
    let header = if variations.is_some() { 14 } else { 10 };
    let feature_list = header + scripts.len();
    let lookup_list = feature_list + features.len();
    let mut out = vec![0, 1, 0, u8::from(variations.is_some())];
    for offset in [header, feature_list, lookup_list] {
        out.extend_from_slice(&(offset as u16).to_be_bytes());
    }
    if variations.is_some() {
        let offset = lookup_list + lookups.len();
        out.extend_from_slice(&(offset as u32).to_be_bytes());
    }
    out.extend_from_slice(scripts);
    out.extend_from_slice(features);
    out.extend_from_slice(lookups);
    out.extend_from_slice(variations.unwrap_or(&[]));
    out
}

/// Open Sans with a GSUB whose `rvrn` has no lookups until its one
/// FeatureVariations record, which holds everywhere, gives it an
/// AlternateSubst lookup from `a` (glyph 68) to `b`, `c`, and `d` (69
/// to 71), and a GPOS whose `rvrn` adds 100 units to the advance of
/// all four.
fn open_sans_with_rvrn() -> Vec<u8> {
    // Format 1: Coverage at 16, one AlternateSet, at 8.
    let alternates = [
        0, 1, 0, 16, 0, 1, 0, 8, 0, 3, 0, 69, 0, 70, 0, 71, 0, 1, 0, 1, 0, 68,
    ];
    let variations = [
        0, 1, 0, 0, 0, 0, 0, 1, // header
        0, 0, 0, 0, 0, 0, 0, 16, // null ConditionSet, substitution
        0, 1, 0, 0, 0, 1, // FeatureTableSubstitution
        0, 0, 0, 0, 0, 12, // feature 0
        0, 0, 0, 1, 0, 0, // the alternate Feature: lookup 0
    ];
    let gsub = layout_table(
        &one_feature_script_list(),
        &rvrn_feature_list(&[]),
        &one_lookup_list(3, &alternates),
        Some(&variations),
    );
    // Format 1: Coverage at 8, an XAdvance of 100 for every glyph.
    let advance = [
        0, 1, 0, 8, 0, 4, 0, 100, 0, 1, 0, 4, 0, 68, 0, 69, 0, 70, 0, 71,
    ];
    let gpos = layout_table(
        &one_feature_script_list(),
        &rvrn_feature_list(&[0]),
        &one_lookup_list(1, &advance),
        None,
    );
    with_tables(OPEN_SANS, &[(*b"GSUB", gsub), (*b"GPOS", gpos)])
}

#[test]
fn the_rvrn_value_picks_the_alternate_and_rvrn_positions_too() {
    // HarfBuzz 14.5.0: the value of `rvrn` picks the alternate, 1 for
    // the first, and one past the last substitutes nothing. GPOS
    // `rvrn` adds its 100 units unless `rvrn` is off.
    let font = open_sans_with_rvrn();
    let rows = |features: &[Feature]| font_rows(&font, None, None, "a", features);
    let rvrn = |value| Feature {
        tag: *b"rvrn",
        value,
    };
    assert_eq!(rows(&[]), [(69, 0, 1355, 0, 0)]);
    assert_eq!(rows(&[rvrn(1)]), [(69, 0, 1355, 0, 0)]);
    assert_eq!(rows(&[rvrn(2)]), [(70, 0, 1075, 0, 0)]);
    assert_eq!(rows(&[rvrn(3)]), [(71, 0, 1355, 0, 0)]);
    assert_eq!(rows(&[rvrn(4)]), [(68, 0, 1239, 0, 0)]);
    assert_eq!(rows(&[rvrn(255)]), [(68, 0, 1239, 0, 0)]);
    assert_eq!(rows(&[rvrn(0)]), [(68, 0, 1139, 0, 0)]);
}

/// A ScriptList with `DFLT` and `latn`, whose default language systems
/// both list feature 0 and make feature 1 their required feature.
fn script_list_with_required_feature_1() -> Vec<u8> {
    let mut out = vec![0, 2];
    out.extend_from_slice(b"DFLT\0\x0E");
    out.extend_from_slice(b"latn\0\x0E");
    out.extend_from_slice(&[0, 4, 0, 0]);
    // No lookupOrder, required feature 1, feature 0.
    out.extend_from_slice(&[0, 0, 0, 1, 0, 1, 0, 0]);
    out
}

/// A FeatureList of `(tag, lookups)` features, in order.
fn feature_list(features: &[(&[u8; 4], &[u16])]) -> Vec<u8> {
    let mut out = (features.len() as u16).to_be_bytes().to_vec();
    let mut offset = 2 + 6 * features.len();
    for (tag, lookups) in features {
        out.extend_from_slice(*tag);
        out.extend_from_slice(&(offset as u16).to_be_bytes());
        offset += 4 + 2 * lookups.len();
    }
    for (_, lookups) in features {
        out.extend_from_slice(&[0, 0]); // featureParamsOffset
        out.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
        for l in *lookups {
            out.extend_from_slice(&l.to_be_bytes());
        }
    }
    out
}

/// A LookupList of AlternateSubst lookups, one per `(glyph,
/// alternates)` entry, each with one format 1 subtable.
fn alternate_lookup_list(lookups: &[(u16, &[u16])]) -> Vec<u8> {
    let mut out = (lookups.len() as u16).to_be_bytes().to_vec();
    let bodies: Vec<Vec<u8>> = lookups
        .iter()
        .map(|&(glyph, alternates)| {
            // Lookup header: type 3, no flags, one subtable at 8.
            let mut body = vec![0, 3, 0, 0, 0, 1, 0, 8];
            // Subtable: format 1, Coverage after the one AlternateSet,
            // which sits at 8.
            let coverage = 8 + 2 + 2 * alternates.len();
            body.extend_from_slice(&[0, 1]);
            body.extend_from_slice(&(coverage as u16).to_be_bytes());
            body.extend_from_slice(&[0, 1, 0, 8]);
            body.extend_from_slice(&(alternates.len() as u16).to_be_bytes());
            for a in alternates {
                body.extend_from_slice(&a.to_be_bytes());
            }
            body.extend_from_slice(&[0, 1, 0, 1]);
            body.extend_from_slice(&glyph.to_be_bytes());
            body
        })
        .collect();
    let mut offset = 2 + 2 * lookups.len();
    for body in &bodies {
        out.extend_from_slice(&(offset as u16).to_be_bytes());
        offset += body.len();
    }
    for body in &bodies {
        out.extend_from_slice(body);
    }
    out
}

/// Open Sans with a GSUB of three AlternateSubst lookups: 0 from `a`
/// (glyph 68) to `b`, `c`, `d` (69 to 71), 1 from `e` (72) to `f`, `g`
/// (73, 74), and 2 from `h` (75) to `i`, `j` (76, 77). Feature 0,
/// `rvrn`, has lookups 0 and 1, and feature 1, the required feature,
/// tagged `required_tag`, has lookups 0 and 2.
fn open_sans_with_required_and_rvrn(required_tag: &[u8; 4]) -> Vec<u8> {
    let gsub = layout_table(
        &script_list_with_required_feature_1(),
        &feature_list(&[(b"rvrn", &[0, 1]), (required_tag, &[0, 2])]),
        &alternate_lookup_list(&[(68, &[69, 70, 71]), (72, &[73, 74]), (75, &[76, 77])]),
        None,
    );
    with_tables(OPEN_SANS, &[(*b"GSUB", gsub)])
}

#[test]
fn a_lookup_rvrn_shares_with_the_required_feature_takes_no_alternate_past_the_first() {
    // HarfBuzz 14.5.0 runs the required feature with the global mask
    // and merges it with `rvrn` in stage 0. With `rvrn` above 1, which
    // takes mask bits of its own, lookup 0, which both have, reads an
    // alternate index from the OR of the two masks and substitutes
    // nothing. Lookup 1, `rvrn`'s alone, takes the caller's alternate,
    // and lookup 2, the required feature's alone, the first. The
    // required feature's tag is either one no pass applies, or `rvrn`
    // itself, whose stage is stage 0.
    let rvrn = |value| Feature {
        tag: *b"rvrn",
        value,
    };
    for tag in [b"zreq", b"rvrn"] {
        let font = open_sans_with_required_and_rvrn(tag);
        let ids = |features: &[Feature]| -> Vec<u32> {
            font_rows(&font, None, None, "aeh", features)
                .iter()
                .map(|r| r.0)
                .collect()
        };
        let tag = core::str::from_utf8(tag).unwrap();
        assert_eq!(ids(&[]), [69, 73, 76], "{tag}");
        assert_eq!(ids(&[rvrn(1)]), [69, 73, 76], "{tag}");
        assert_eq!(ids(&[rvrn(2)]), [68, 74, 76], "{tag}");
        assert_eq!(ids(&[rvrn(3)]), [68, 72, 76], "{tag}");
        assert_eq!(ids(&[rvrn(0)]), [69, 72, 76], "{tag}");
    }
}
