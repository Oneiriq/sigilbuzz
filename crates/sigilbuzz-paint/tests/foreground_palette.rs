//! Foreground color, palette selection, and variation coordinates
//! through [`evaluate_with`].
//!
//! COLR palette entry `0xFFFF` means "the current text color". These
//! tests pin down that the evaluator reports it as foreground (on solid
//! fills and on gradient stops) instead of collapsing it into an
//! ordinary color, that [`EvalOptions`] picks the CPAL palette and the
//! foreground color, and that variation deltas still apply when the
//! options carry coordinates.
//!
//! Fixtures are hand-built COLR + CPAL byte blobs in the same layout as
//! `evaluator.rs`.

use sigilbuzz::Face;
use sigilbuzz_paint::{
    evaluate, evaluate_at_coords, evaluate_with, Color, ColorStop, DrawCmd, EvalOptions,
    PaintSource,
};

// =========================================================================
// Fixture builders
// =========================================================================

const FOREGROUND: u16 = 0xFFFF;

fn f2dot14(v: f32) -> [u8; 2] {
    ((v * 16384.0).round() as i16).to_be_bytes()
}

/// SFNT directory holding exactly COLR and CPAL.
fn build_face_bytes(colr: &[u8], cpal: &[u8]) -> Vec<u8> {
    let dir_len = 12 + 2 * 16;
    let cpal_off = dir_len;
    let colr_off = cpal_off + cpal.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    for (tag, off, len) in [
        (b"COLR", colr_off, colr.len()),
        (b"CPAL", cpal_off, cpal.len()),
    ] {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(off as u32).to_be_bytes());
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
    out.extend_from_slice(cpal);
    out.extend_from_slice(colr);
    out
}

/// CPAL v0 carrying one palette per entry of `palettes`. Every palette
/// must have the same number of colors.
fn build_cpal(palettes: &[&[(u8, u8, u8, u8)]]) -> Vec<u8> {
    let entries = palettes[0].len() as u16;
    let num_palettes = palettes.len() as u16;
    let records = entries * num_palettes;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&entries.to_be_bytes());
    out.extend_from_slice(&num_palettes.to_be_bytes());
    out.extend_from_slice(&records.to_be_bytes());
    let records_off = 12 + 2 * u32::from(num_palettes);
    out.extend_from_slice(&records_off.to_be_bytes());
    for i in 0..num_palettes {
        out.extend_from_slice(&(i * entries).to_be_bytes());
    }
    for palette in palettes {
        assert_eq!(palette.len(), entries as usize);
        for (r, g, b, a) in *palette {
            out.extend_from_slice(&[*b, *g, *r, *a]);
        }
    }
    out
}

/// COLRv1 with one base-glyph paint record per `(gid, paint bytes)`.
/// `var_store`, when non-empty, is appended and referenced from the
/// header's `varStoreOffset`.
fn build_colr(paints: &[(u16, Vec<u8>)], var_store: &[u8]) -> Vec<u8> {
    let header_len: u32 = 30;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // layerListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // clipListOffset
    let var_store_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // varStoreOffset
    out.extend_from_slice(&(paints.len() as u32).to_be_bytes());
    let record_slots = out.len();
    for (gid, _) in paints {
        out.extend_from_slice(&gid.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    for (i, (_, bytes)) in paints.iter().enumerate() {
        let rel = out.len() as u32 - header_len;
        let slot = record_slots + i * 6 + 2;
        out[slot..slot + 4].copy_from_slice(&rel.to_be_bytes());
        out.extend_from_slice(bytes);
    }
    if !var_store.is_empty() {
        let off = out.len() as u32;
        out[var_store_slot..var_store_slot + 4].copy_from_slice(&off.to_be_bytes());
        out.extend_from_slice(var_store);
    }
    out
}

/// One-axis ItemVariationStore with a single region peaking at +1 and
/// one int16 delta per row.
fn build_ivs(rows: &[i16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&12u32.to_be_bytes()); // regionListOffset
    out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
    out.extend_from_slice(&(12u32 + 4 + 6).to_be_bytes()); // subtable offset
    out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
    out.extend_from_slice(&f2dot14(0.0));
    out.extend_from_slice(&f2dot14(1.0));
    out.extend_from_slice(&f2dot14(1.0));
    out.extend_from_slice(&(rows.len() as u16).to_be_bytes()); // itemCount
    out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount
    out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
    out.extend_from_slice(&0u16.to_be_bytes()); // regionIndexes[0]
    for d in rows {
        out.extend_from_slice(&d.to_be_bytes());
    }
    out
}

/// PaintSolid (format 2).
fn paint_solid(entry: u16, alpha: f32) -> Vec<u8> {
    let mut p = vec![2u8];
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(alpha));
    p
}

/// PaintVarSolid (format 3).
fn paint_var_solid(entry: u16, alpha: f32, var_index_base: u32) -> Vec<u8> {
    let mut p = vec![3u8];
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(alpha));
    p.extend_from_slice(&var_index_base.to_be_bytes());
    p
}

/// PaintLinearGradient (format 4) over a ColorLine of
/// `(offset, entry, alpha)` stops.
fn paint_linear(stops: &[(f32, u16, f32)]) -> Vec<u8> {
    let mut p = vec![4u8, 0, 0, 16]; // color line right after the 16-byte paint
    for v in [0i16, 0, 100, 0, 0, 100] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.push(0); // extend = Pad
    p.extend_from_slice(&(stops.len() as u16).to_be_bytes());
    for (offset, entry, alpha) in stops {
        p.extend_from_slice(&f2dot14(*offset));
        p.extend_from_slice(&entry.to_be_bytes());
        p.extend_from_slice(&f2dot14(*alpha));
    }
    p
}

/// PaintVarLinearGradient (format 5) over a VarColorLine of
/// `(offset, entry, alpha, var_index_base)` stops.
fn paint_var_linear(stops: &[(f32, u16, f32, u32)]) -> Vec<u8> {
    let mut p = vec![5u8, 0, 0, 20]; // color line right after the 20-byte paint
    for v in [0i16, 0, 100, 0, 0, 100] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.extend_from_slice(&u32::MAX.to_be_bytes()); // paint varIndexBase: none
    p.push(0); // extend = Pad
    p.extend_from_slice(&(stops.len() as u16).to_be_bytes());
    for (offset, entry, alpha, var) in stops {
        p.extend_from_slice(&f2dot14(*offset));
        p.extend_from_slice(&entry.to_be_bytes());
        p.extend_from_slice(&f2dot14(*alpha));
        p.extend_from_slice(&var.to_be_bytes());
    }
    p
}

/// Two palettes: palette 0 = [red, green], palette 1 = [blue, yellow].
const TWO_PALETTES: [&[(u8, u8, u8, u8)]; 2] = [
    &[(255, 0, 0, 255), (0, 255, 0, 255)],
    &[(0, 0, 255, 255), (255, 255, 0, 255)],
];

fn face_bytes(paints: &[(u16, Vec<u8>)]) -> Vec<u8> {
    build_face_bytes(&build_colr(paints, &[]), &build_cpal(&TWO_PALETTES))
}

fn only_solid(cmds: &[DrawCmd]) -> (Color, bool) {
    match cmds {
        [DrawCmd::FillGlyph {
            paint:
                PaintSource::Solid {
                    color,
                    is_foreground,
                },
            ..
        }] => (*color, *is_foreground),
        other => panic!("expected one solid fill, got {other:?}"),
    }
}

fn only_stops(cmds: &[DrawCmd]) -> Vec<ColorStop> {
    match cmds {
        [DrawCmd::FillGlyph {
            paint: PaintSource::Gradient(g),
            ..
        }] => g.stops.clone(),
        other => panic!("expected one gradient fill, got {other:?}"),
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

// =========================================================================
// Foreground solids
// =========================================================================

#[test]
fn foreground_solid_is_flagged_and_keeps_default_white() {
    let bytes = face_bytes(&[(1, paint_solid(FOREGROUND, 0.5))]);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let (color, is_fg) = only_solid(&evaluate(&face, 1));
    assert!(is_fg, "palette entry 0xFFFF must report foreground");
    // Default foreground is opaque white, the pre-flag output.
    assert_eq!((color.r, color.g, color.b), (1.0, 1.0, 1.0));
    assert!(close(color.a, 0.5), "alpha was {}", color.a);
}

#[test]
fn foreground_solid_uses_configured_foreground_times_alpha() {
    let bytes = face_bytes(&[(1, paint_solid(FOREGROUND, 0.5))]);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let fg = Color::new(0.2, 0.4, 0.6, 0.8);
    let (color, is_fg) = only_solid(&evaluate_with(
        &face,
        1,
        &EvalOptions::new().with_foreground(fg),
    ));
    assert!(is_fg);
    assert_eq!((color.r, color.g, color.b), (0.2, 0.4, 0.6));
    // The paint alpha multiplies the foreground's own alpha.
    assert!(close(color.a, 0.4), "alpha was {}", color.a);
}

#[test]
fn palette_solid_is_not_foreground_even_when_white() {
    let white = [(255u8, 255u8, 255u8, 255u8)];
    let colr = build_colr(&[(1, paint_solid(0, 1.0))], &[]);
    let bytes = build_face_bytes(&colr, &build_cpal(&[&white]));
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let (color, is_fg) = only_solid(&evaluate(&face, 1));
    assert!(!is_fg, "a real white palette entry is not the foreground");
    assert_eq!(color, Color::WHITE);
}

// =========================================================================
// Foreground gradient stops
// =========================================================================

#[test]
fn foreground_gradient_stops_are_flagged_per_stop() {
    let paint = paint_linear(&[(0.0, 0, 1.0), (1.0, FOREGROUND, 0.5)]);
    let bytes = face_bytes(&[(1, paint)]);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");

    let stops = only_stops(&evaluate(&face, 1));
    assert_eq!(stops.len(), 2);
    assert!(!stops[0].is_foreground);
    assert_eq!(stops[0].color, Color::new(1.0, 0.0, 0.0, 1.0));
    assert!(stops[1].is_foreground);
    assert_eq!(
        (stops[1].color.r, stops[1].color.g, stops[1].color.b),
        (1.0, 1.0, 1.0)
    );
    assert!(close(stops[1].color.a, 0.5));

    let fg = Color::new(0.0, 0.0, 0.0, 1.0);
    let stops = only_stops(&evaluate_with(
        &face,
        1,
        &EvalOptions::new().with_foreground(fg),
    ));
    assert!(stops[1].is_foreground);
    assert_eq!(
        (stops[1].color.r, stops[1].color.g, stops[1].color.b),
        (0.0, 0.0, 0.0)
    );
    assert!(close(stops[1].color.a, 0.5));
    // The palette stop is untouched by the foreground choice.
    assert_eq!(stops[0].color, Color::new(1.0, 0.0, 0.0, 1.0));
    assert!(!stops[0].is_foreground);
}

// =========================================================================
// Palette selection
// =========================================================================

#[test]
fn non_default_palette_resolves_solids_and_stops() {
    let bytes = face_bytes(&[
        (1, paint_solid(0, 1.0)),
        (2, paint_linear(&[(0.0, 0, 1.0), (1.0, 1, 1.0)])),
    ]);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let dark = EvalOptions::new().with_palette_index(1);

    let (color, is_fg) = only_solid(&evaluate_with(&face, 1, &dark));
    assert!(!is_fg);
    assert_eq!(
        color,
        Color::new(0.0, 0.0, 1.0, 1.0),
        "palette 1 entry 0 is blue"
    );

    let stops = only_stops(&evaluate_with(&face, 2, &dark));
    assert_eq!(stops[0].color, Color::new(0.0, 0.0, 1.0, 1.0));
    assert_eq!(stops[1].color, Color::new(1.0, 1.0, 0.0, 1.0));

    // Palette 0 stays the default.
    let (color, _) = only_solid(&evaluate(&face, 1));
    assert_eq!(color, Color::new(1.0, 0.0, 0.0, 1.0));
}

#[test]
fn out_of_range_palette_falls_back_to_default_palette() {
    let bytes = face_bytes(&[(1, paint_solid(1, 1.0))]);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let options = EvalOptions::new().with_palette_index(7);
    let (color, is_fg) = only_solid(&evaluate_with(&face, 1, &options));
    assert!(!is_fg);
    assert_eq!(color, Color::new(0.0, 1.0, 0.0, 1.0), "palette 0 entry 1");
}

#[test]
fn palette_choice_does_not_affect_foreground() {
    let bytes = face_bytes(&[(1, paint_solid(FOREGROUND, 1.0))]);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let a = only_solid(&evaluate_with(&face, 1, &EvalOptions::new()));
    let b = only_solid(&evaluate_with(
        &face,
        1,
        &EvalOptions::new().with_palette_index(1),
    ));
    assert_eq!(a, b);
    assert_eq!(a, (Color::WHITE, true));
}

// =========================================================================
// Variation coordinates
// =========================================================================

/// Glyph 1: PaintVarSolid on the foreground entry whose alpha drops by
/// 0.5 at axis +1 (IVS row 0).
/// Glyph 2: PaintVarLinearGradient whose second stop is the foreground
/// entry; its alpha drops by 0.25 at axis +1 (IVS row 2; row 1 is the
/// stop offset delta, zero here).
/// Glyph 3: PaintVarSolid on palette entry 0 with the same alpha row as
/// glyph 1.
fn variable_face_bytes() -> Vec<u8> {
    let ivs = build_ivs(&[-8192, 0, -4096]);
    let paints = [
        (1, paint_var_solid(FOREGROUND, 1.0, 0)),
        (
            2,
            paint_var_linear(&[(0.0, 0, 1.0, u32::MAX), (1.0, FOREGROUND, 1.0, 1)]),
        ),
        (3, paint_var_solid(0, 1.0, 0)),
    ];
    build_face_bytes(&build_colr(&paints, &ivs), &build_cpal(&TWO_PALETTES))
}

#[test]
fn coords_apply_deltas_to_foreground_solid() {
    let bytes = variable_face_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let coords = [1.0_f32];

    let (static_color, is_fg) = only_solid(&evaluate_with(&face, 1, &EvalOptions::new()));
    assert!(is_fg);
    assert!(close(static_color.a, 1.0));

    let (varied, is_fg) = only_solid(&evaluate_with(
        &face,
        1,
        &EvalOptions::new().with_coords(&coords),
    ));
    assert!(is_fg, "variation must not drop the foreground flag");
    assert!(close(varied.a, 0.5), "alpha was {}", varied.a);

    let half = [0.5_f32];
    let (half_color, _) = only_solid(&evaluate_with(
        &face,
        1,
        &EvalOptions::new().with_coords(&half),
    ));
    assert!(close(half_color.a, 0.75), "alpha was {}", half_color.a);
}

#[test]
fn coords_apply_deltas_to_foreground_stop() {
    let bytes = variable_face_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let coords = [1.0_f32];
    let fg = Color::new(0.0, 0.5, 0.0, 1.0);
    let options = EvalOptions::new().with_coords(&coords).with_foreground(fg);
    let stops = only_stops(&evaluate_with(&face, 2, &options));
    assert_eq!(stops.len(), 2);
    assert!(!stops[0].is_foreground);
    assert!(stops[1].is_foreground);
    assert_eq!(
        (stops[1].color.r, stops[1].color.g, stops[1].color.b),
        (0.0, 0.5, 0.0)
    );
    assert!(
        close(stops[1].color.a, 0.75),
        "alpha was {}",
        stops[1].color.a
    );
    assert!(close(stops[1].offset, 1.0));
}

#[test]
fn coords_palette_and_foreground_combine() {
    let bytes = variable_face_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let coords = [1.0_f32];
    let options = EvalOptions::new()
        .with_coords(&coords)
        .with_palette_index(1)
        .with_foreground(Color::new(0.0, 0.0, 0.0, 1.0));
    let (color, is_fg) = only_solid(&evaluate_with(&face, 3, &options));
    assert!(!is_fg);
    assert_eq!((color.r, color.g, color.b), (0.0, 0.0, 1.0));
    assert!(close(color.a, 0.5), "alpha was {}", color.a);
}

#[test]
fn legacy_entry_points_match_default_options() {
    let bytes = variable_face_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let coords = [0.5_f32];
    for gid in 1..=3 {
        let a = format!("{:?}", evaluate(&face, gid));
        let b = format!("{:?}", evaluate_with(&face, gid, &EvalOptions::default()));
        assert_eq!(a, b, "evaluate vs evaluate_with for gid {gid}");
        let c = format!("{:?}", evaluate_at_coords(&face, gid, &coords));
        let d = format!(
            "{:?}",
            evaluate_with(&face, gid, &EvalOptions::new().with_coords(&coords))
        );
        assert_eq!(c, d, "evaluate_at_coords vs evaluate_with for gid {gid}");
    }
}

#[test]
fn color_stop_new_is_not_foreground() {
    let stop = ColorStop::new(0.25, Color::WHITE);
    assert_eq!(stop.offset, 0.25);
    assert_eq!(stop.color, Color::WHITE);
    assert!(!stop.is_foreground);
}
