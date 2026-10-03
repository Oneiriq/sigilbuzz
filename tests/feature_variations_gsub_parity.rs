//! GSUB FeatureVariations and `rvrn` against HarfBuzz and rustybuzz.
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
//! cluster, x advance, x offset, y offset. rustybuzz 0.20, which reads
//! FeatureVariations condition format 1, agrees on the user-space
//! cases, and the tests check that too.

use rustybuzz::ttf_parser::Tag;
use rustybuzz::{Face as RbFace, UnicodeBuffer, Variation};
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

fn rustybuzz_rows(user: f32, text: &str) -> Vec<Row> {
    let mut face = RbFace::from_slice(RUBIK, 0).unwrap();
    face.set_variations(&[Variation {
        tag: Tag::from_bytes(b"wght"),
        value: user,
    }]);
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, i.cluster, p.x_advance, p.x_offset, p.y_offset))
        .collect()
}

#[test]
fn gsub_rvrn_follows_harfbuzz_in_user_space() {
    // Glyph ids, clusters, and offsets match exactly. An advance may be
    // 1 unit off: HarfBuzz rounds the `fvar` coordinate to F2DOT14
    // before `avar` maps it, and sigilbuzz (like rustybuzz) maps the
    // unrounded value, so HVAR sees a slightly different coordinate.
    // At `wght` 700 that moves four advances by 1. The normalized cases
    // below give both engines the same coordinate and match exactly.
    let mut advance_diffs = 0;
    for &(wght, text, expected) in USER {
        let rows = sigilbuzz_rows(None, Some(wght), text, &[]);
        let without_advance = |rows: &[Row]| -> Vec<(u32, u32, i32, i32)> {
            rows.iter().map(|&(g, c, _, x, y)| (g, c, x, y)).collect()
        };
        assert_eq!(
            without_advance(&rows),
            without_advance(expected),
            "wght {wght} {text:?}"
        );
        for (got, want) in rows.iter().zip(expected) {
            assert!((got.2 - want.2).abs() <= 1, "wght {wght} {text:?}");
            advance_diffs += usize::from(got.2 != want.2);
        }
    }
    assert!(advance_diffs <= 4, "{advance_diffs} advances differ");
}

#[test]
fn gsub_rvrn_follows_rustybuzz_in_user_space() {
    for &(wght, text, _) in USER {
        let rows = sigilbuzz_rows(None, Some(wght), text, &[]);
        assert_eq!(rows, rustybuzz_rows(wght, text), "wght {wght} {text:?}");
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
