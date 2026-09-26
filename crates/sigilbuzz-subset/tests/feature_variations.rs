//! FeatureVariations survive subsetting and instancing.
//!
//! Rubik's GSUB is version 1.1: above about 544 wght its `rvrn` feature,
//! empty by default, swaps in a lookup that replaces the yen, euro and
//! hryvnia signs with heavier forms. Every case shapes the source and
//! the output with rustybuzz, which applies FeatureVariations, and the
//! glyphs must agree:
//!
//! - a subset, at several weights (it used to fall back to version 1.0
//!   and lose the swap);
//! - full instances at several weights (they used to keep the
//!   FeatureVariations, which then applied at the default coordinates);
//! - partial instances of a two-axis variant of Rubik whose GSUB carries
//!   hand-built records over both axes, pinning each axis in turn (the
//!   records used to keep naming axes the instance no longer has).

#[path = "support/sfnt.rs"]
mod sfnt;

use rustybuzz::{Face as RbFace, UnicodeBuffer, Variation};
use sigilbuzz::Face;
use sigilbuzz_subset::{instance, subset, AxisPin, InstanceInput, SubsetInput};

use sfnt::{be16, be32, edit_tables};

const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

/// The signs Rubik's `rvrn` swaps, and a letter it leaves alone.
const TEXT: &str = "\u{A5}\u{20AC}\u{20B4}A";

/// Rubik's `rvrn` feature index and the lookup its alternate uses.
const RVRN: u16 = 20;
const HEAVY_SIGNS: u16 = 0;

/// `(glyph id, x advance)` for each glyph `text` shapes to at the given
/// `(axis tag, user value)` settings.
fn shape(font: &[u8], text: &str, axes: &[(&[u8; 4], f32)]) -> Vec<(u32, i32)> {
    let mut face = RbFace::from_slice(font, 0).expect("rustybuzz parses");
    let variations: Vec<Variation> = axes
        .iter()
        .map(|(tag, value)| Variation {
            tag: rustybuzz::ttf_parser::Tag::from_bytes(tag),
            value: *value,
        })
        .collect();
    face.set_variations(&variations);
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance))
        .collect()
}

fn glyphs(shaped: &[(u32, i32)]) -> Vec<u32> {
    shaped.iter().map(|&(g, _)| g).collect()
}

/// The GSUB minor version of `font`.
fn gsub_minor(font: &[u8]) -> u16 {
    let face = Face::parse_bytes(font, 0).unwrap();
    let gsub = face.table_bytes(*b"GSUB").unwrap();
    u16::from_be_bytes([gsub[2], gsub[3]])
}

#[test]
fn a_subset_keeps_the_weight_dependent_substitutions() {
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let input = SubsetInput {
        gids: TEXT.chars().filter_map(|c| cmap.glyph_id(c)).collect(),
        ..SubsetInput::default()
    };
    let out = subset(&face, &input).expect("the subset succeeds");
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert_eq!(gsub_minor(&out.bytes), 1, "the subset GSUB is 1.1");
    let to_new = |old: u32| {
        let old = u16::try_from(old).unwrap();
        let i = out.gid_map.binary_search_by_key(&old, |&(o, _)| o).unwrap();
        u32::from(out.gid_map[i].1)
    };
    let mut swapped = false;
    for wght in [300.0, 450.0, 540.0, 550.0, 600.0, 750.0, 900.0] {
        let axes = [(b"wght", wght)];
        let expected: Vec<(u32, i32)> = shape(RUBIK, TEXT, &axes)
            .into_iter()
            .map(|(g, advance)| (to_new(g), advance))
            .collect();
        assert_eq!(shape(&out.bytes, TEXT, &axes), expected, "wght {wght}");
        swapped |= expected != shape(&out.bytes, TEXT, &[(b"wght", 300.0)]);
    }
    assert!(swapped, "the heavy signs show up at some weight");
}

#[test]
fn full_instances_apply_the_record_that_matches_their_coordinates() {
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    for normalized in [0.0f32, 0.25, 0.4, 0.45, 0.5, 0.75, 1.0] {
        let input = InstanceInput {
            coords: vec![normalized],
            ..InstanceInput::default()
        };
        let out = instance(&face, &input).expect("the instance succeeds");
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(gsub_minor(&out.bytes), 0, "no FeatureVariations left");
        let source = shape(RUBIK, TEXT, &[(b"wght", 300.0 + 600.0 * normalized)]);
        assert_eq!(
            glyphs(&shape(&out.bytes, TEXT, &[])),
            glyphs(&source),
            "normalized wght {normalized}"
        );
    }
}

/// One record: `(axis, min, max)` conditions and `(feature, lookups)`
/// substitutions.
type Record = (Vec<(u16, i16, i16)>, Vec<(u16, Vec<u16>)>);

/// A FeatureVariations table of `(conditions, substitutions)` records,
/// conditions as `(axis, min, max)` in F2DOT14 units, substitutions as
/// `(feature, lookups)`.
fn feature_variations(records: &[Record]) -> Vec<u8> {
    let mut out = Vec::new();
    be16(&mut out, 1);
    be16(&mut out, 0);
    be32(&mut out, records.len() as u32);
    out.resize(8 + records.len() * 8, 0);
    for (i, (conditions, substitutions)) in records.iter().enumerate() {
        let set = out.len();
        be16(&mut out, conditions.len() as u16);
        for k in 0..conditions.len() {
            be32(&mut out, (2 + conditions.len() * 4 + k * 8) as u32);
        }
        for &(axis, min, max) in conditions {
            for v in [1, axis, min as u16, max as u16] {
                be16(&mut out, v);
            }
        }
        let fts = out.len();
        for v in [1, 0, substitutions.len() as u16] {
            be16(&mut out, v);
        }
        let records_at = out.len();
        out.resize(records_at + substitutions.len() * 6, 0);
        for (k, (feature, lookups)) in substitutions.iter().enumerate() {
            let rec = records_at + k * 6;
            let alternate = (out.len() - fts) as u32;
            out[rec..rec + 2].copy_from_slice(&feature.to_be_bytes());
            out[rec + 2..rec + 6].copy_from_slice(&alternate.to_be_bytes());
            be16(&mut out, 0);
            be16(&mut out, lookups.len() as u16);
            for &l in lookups {
                be16(&mut out, l);
            }
        }
        out[8 + i * 8..12 + i * 8].copy_from_slice(&(set as u32).to_be_bytes());
        out[12 + i * 8..16 + i * 8].copy_from_slice(&(fts as u32).to_be_bytes());
    }
    out
}

/// An fvar with `wght` 300..900 (default 300) and `wdth` 75..125
/// (default 100), no named instances.
fn two_axis_fvar() -> Vec<u8> {
    let mut out = Vec::new();
    for v in [1u16, 0, 16, 2, 2, 20, 0, 12] {
        be16(&mut out, v);
    }
    for (tag, min, default, max) in [(b"wght", 300, 300, 900), (b"wdth", 75, 100, 125)] {
        out.extend_from_slice(tag);
        for v in [min, default, max] {
            be32(&mut out, v << 16);
        }
        be16(&mut out, 0);
        be16(&mut out, 256);
    }
    out
}

/// Rubik with a second axis and a GSUB whose FeatureVariations use both:
/// record 0 needs wght high; record 1 needs wdth wide and wght above a
/// quarter; record 2 needs wght above a half and turns `rvrn` off;
/// record 3 needs wdth narrow. The tables that vary with the one real
/// axis (outlines, metrics, positioning) are left out, so glyph choice
/// is all that varies.
fn two_axis_rubik() -> Vec<u8> {
    let q = 0x1000i16;
    let one = 0x4000i16;
    let fv = feature_variations(&[
        (vec![(0, 3 * q, one)], vec![(RVRN, vec![HEAVY_SIGNS])]),
        (
            vec![(1, 2 * q, one), (0, q, one)],
            vec![(RVRN, vec![HEAVY_SIGNS])],
        ),
        (vec![(0, 2 * q, one)], vec![(RVRN, vec![])]),
        (vec![(1, -one, -2 * q)], vec![(RVRN, vec![HEAVY_SIGNS])]),
    ]);
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    let mut gsub = face.table_bytes(*b"GSUB").unwrap().to_vec();
    let at = gsub.len() as u32;
    gsub[10..14].copy_from_slice(&at.to_be_bytes());
    gsub.extend_from_slice(&fv);
    edit_tables(
        RUBIK,
        &[
            (*b"GSUB", Some(gsub)),
            (*b"fvar", Some(two_axis_fvar())),
            (*b"avar", None),
            (*b"gvar", None),
            (*b"HVAR", None),
            (*b"MVAR", None),
            (*b"STAT", None),
            (*b"GDEF", None),
            (*b"GPOS", None),
        ],
    )
}

/// User value of `axis` at a normalized coordinate.
fn user(axis: usize, normalized: f32) -> f32 {
    if axis == 0 {
        300.0 + 600.0 * normalized.max(0.0)
    } else {
        100.0 + 25.0 * normalized
    }
}

#[test]
fn partial_instances_settle_pinned_axes_and_renumber_the_rest() {
    let source = two_axis_rubik();
    let face = Face::parse_bytes(&source, 0).unwrap();
    let tags = [b"wght", b"wdth"];
    let samples: [&[f32]; 2] = [
        &[0.0, 0.1, 0.3, 0.55, 0.8, 1.0],
        &[-1.0, -0.6, -0.4, 0.0, 0.3, 0.6, 1.0],
    ];
    let mut heavy = 0;
    for pinned in 0..2 {
        let kept = 1 - pinned;
        for &pin in samples[pinned] {
            let mut coords = vec![0.0; 2];
            coords[pinned] = pin;
            let mut axis_pins = vec![AxisPin::Keep; 2];
            axis_pins[pinned] = AxisPin::Pin;
            let input = InstanceInput {
                coords,
                drop_var_tables: true,
                axis_pins,
            };
            let out = instance(&face, &input).expect("the partial instance succeeds");
            assert!(out.warnings.is_empty(), "{:?}", out.warnings);
            for &free in samples[kept] {
                let mut axes = [(tags[0], 0.0), (tags[1], 0.0)];
                axes[pinned] = (tags[pinned], user(pinned, pin));
                axes[kept] = (tags[kept], user(kept, free));
                let expected = glyphs(&shape(&source, TEXT, &axes));
                let got = glyphs(&shape(&out.bytes, TEXT, &[(tags[kept], user(kept, free))]));
                assert_eq!(
                    got,
                    expected,
                    "{} pinned at {pin}, {} at {free}",
                    String::from_utf8_lossy(tags[pinned]),
                    String::from_utf8_lossy(tags[kept]),
                );
                heavy += usize::from(expected != glyphs(&shape(&source, TEXT, &[])));
            }
        }
    }
    assert!(heavy > 0, "some samples swap the signs");
}
