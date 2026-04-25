//! Integration tests for the COLRv1 paint evaluator.
//!
//! These build minimal COLR + CPAL byte blobs in-process, plug them
//! into a synthetic `Face` via the SFNT directory layout sigilbuzz
//! exposes, and assert the [`DrawCmd`] stream the evaluator emits.
//!
//! The fixture style mirrors the hand-authored COLR tests in
//! `src/tables/colr.rs` so additions stay in sync with the parser
//! tests upstream.

use sigilbuzz::tables::colr::CompositeMode;
use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate, evaluate_at_coords, DrawCmd, Extend, GradientKind, PaintSource};

// =========================================================================
// Synthetic SFNT + COLR/CPAL fixtures
// =========================================================================

/// Builds a minimal SFNT containing exactly the COLR and CPAL tables
/// supplied. The face has no glyf / loca so `glyph_outline` would
/// fail, but the evaluator only ever asks the face for COLR + CPAL,
/// which is the contract this fixture exercises.
fn build_face_bytes(colr: &[u8], cpal: &[u8]) -> Vec<u8> {
    // SFNT header (12) + 2 records (16 each) = 44 bytes of directory.
    let dir_len = 12 + 2 * 16;
    let cpal_off = dir_len;
    let colr_off = cpal_off + cpal.len();

    let mut out = Vec::new();
    // sfnt version: TrueType. (sigilbuzz accepts both — colour fonts
    // typically ship CBDT/SBIX fronts but the directory is identical.)
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes()); // numTables
    out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift

    // Table records must be sorted by tag. 'C' < 'C' (CPAL == COLR
    // first three bytes), so order is COLR before CPAL alphabetically:
    // 'C','O','L','R' vs 'C','P','A','L' — `O` (0x4F) < `P` (0x50).
    // Place COLR first.
    out.extend_from_slice(b"COLR");
    out.extend_from_slice(&0u32.to_be_bytes()); // checksum
    out.extend_from_slice(&(colr_off as u32).to_be_bytes());
    out.extend_from_slice(&(colr.len() as u32).to_be_bytes());

    out.extend_from_slice(b"CPAL");
    out.extend_from_slice(&0u32.to_be_bytes()); // checksum
    out.extend_from_slice(&(cpal_off as u32).to_be_bytes());
    out.extend_from_slice(&(cpal.len() as u32).to_be_bytes());

    out.extend_from_slice(cpal);
    out.extend_from_slice(colr);
    out
}

/// Builds a v0 CPAL with a single palette of `colors`.
fn build_cpal_v0(colors: &[(u8, u8, u8, u8)]) -> Vec<u8> {
    let num_palettes: u16 = 1;
    let entries = colors.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&entries.to_be_bytes());
    out.extend_from_slice(&num_palettes.to_be_bytes());
    out.extend_from_slice(&entries.to_be_bytes()); // numColorRecords
    let header_plus_indices = 12 + num_palettes as usize * 2;
    out.extend_from_slice(&(header_plus_indices as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // colorRecordIndices[0] = 0
    for (r, g, b, a) in colors {
        out.push(*b);
        out.push(*g);
        out.push(*r);
        out.push(*a);
    }
    out
}

/// Header for a v1 COLR with one base-glyph paint record. `num_base`
/// must be 1; the paint body sits at the end of the table starting at
/// offset 10 (relative to the BaseGlyphList start) and the caller
/// appends its bytes after this header returns.
fn build_v1_header(glyph_id: u16) -> Vec<u8> {
    let header_len = 30; // v0 (14) + v1 appendix (16)
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords (v0)
    out.extend_from_slice(&(header_len as u32).to_be_bytes()); // baseGlyphRecordsOffset
    out.extend_from_slice(&(header_len as u32).to_be_bytes()); // layerRecordsOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    out.extend_from_slice(&(header_len as u32).to_be_bytes()); // baseGlyphListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // layerListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // clipListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // varStoreOffset

    // BaseGlyphList: numRecords = 1, then the record { glyphID,
    // paintOffset = 10 }. The body that follows must start at offset
    // 10 from the BaseGlyphList start (4 header + 6 record).
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&glyph_id.to_be_bytes());
    out.extend_from_slice(&10u32.to_be_bytes());
    out
}

/// Convenience: F2DOT14 encoder for raw alphas / scales / angles.
fn f2dot14(v: f32) -> [u8; 2] {
    let raw = (v * 16384.0).round() as i16;
    raw.to_be_bytes()
}

// =========================================================================
// 1. Solid leaf: PaintSolid resolves through the palette.
// =========================================================================

#[test]
fn solid_paint_emits_single_fill_with_palette_color() {
    let mut colr = build_v1_header(7);
    colr.push(2); // PaintSolid
    colr.extend_from_slice(&1u16.to_be_bytes()); // paletteIndex
    colr.extend_from_slice(&f2dot14(1.0)); // alpha
    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 255, 0, 255)]);

    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let cmds = evaluate(&face, 7);
    assert_eq!(cmds.len(), 1, "expected exactly one FillGlyph");
    match &cmds[0] {
        DrawCmd::FillGlyph { paint, .. } => match paint {
            PaintSource::Solid(c) => {
                assert!((c.r).abs() < 1e-6);
                assert!((c.g - 1.0).abs() < 1e-6);
                assert!((c.b).abs() < 1e-6);
                assert!((c.a - 1.0).abs() < 1e-6);
            }
            other => panic!("expected solid, got {other:?}"),
        },
        other => panic!("expected FillGlyph, got {other:?}"),
    }
}

#[test]
fn solid_paint_alpha_multiplies_palette_alpha() {
    let mut colr = build_v1_header(7);
    colr.push(2);
    colr.extend_from_slice(&0u16.to_be_bytes()); // paletteIndex 0
    colr.extend_from_slice(&f2dot14(0.5)); // alpha 0.5
    let cpal = build_cpal_v0(&[(255, 255, 255, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate(&face, 7);
    match &cmds[0] {
        DrawCmd::FillGlyph {
            paint: PaintSource::Solid(c),
            ..
        } => {
            assert!((c.a - 0.5).abs() < 1e-3, "alpha was {}", c.a);
        }
        other => panic!("unexpected {other:?}"),
    }
}

// =========================================================================
// 2. PaintGlyph wraps a leaf and sets the gid on the emitted fill.
// =========================================================================

#[test]
fn paint_glyph_overrides_fill_gid() {
    let mut colr = build_v1_header(100);
    let pglyph_start = colr.len();
    colr.push(10); // PaintGlyph
    colr.extend_from_slice(&[0, 0, 0]); // Offset24 placeholder
    colr.extend_from_slice(&201u16.to_be_bytes()); // outline glyph id

    let solid_start = colr.len();
    let rel = (solid_start - pglyph_start) as u32;
    colr[pglyph_start + 1] = ((rel >> 16) & 0xff) as u8;
    colr[pglyph_start + 2] = ((rel >> 8) & 0xff) as u8;
    colr[pglyph_start + 3] = (rel & 0xff) as u8;

    colr.push(2); // child Solid
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(0, 0, 255, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate(&face, 100);
    assert_eq!(cmds.len(), 1);
    match &cmds[0] {
        DrawCmd::FillGlyph { gid, paint, .. } => {
            assert_eq!(*gid, 201, "PaintGlyph should rewrite the fill gid");
            assert!(matches!(paint, PaintSource::Solid(_)));
        }
        other => panic!("unexpected {other:?}"),
    }
}

// =========================================================================
// 3. Linear gradient: stops resolve, geometry passes through.
// =========================================================================

#[test]
fn linear_gradient_resolves_stops_against_palette() {
    let mut colr = build_v1_header(33);
    let paint_start = colr.len();
    colr.push(4); // PaintLinearGradient
    colr.extend_from_slice(&[0, 0, 0]); // Offset24 colorLine placeholder
    colr.extend_from_slice(&10i16.to_be_bytes());
    colr.extend_from_slice(&20i16.to_be_bytes());
    colr.extend_from_slice(&30i16.to_be_bytes());
    colr.extend_from_slice(&40i16.to_be_bytes());
    colr.extend_from_slice(&50i16.to_be_bytes());
    colr.extend_from_slice(&60i16.to_be_bytes());

    let cl_start = colr.len();
    let cl_rel = (cl_start - paint_start) as u32;
    colr[paint_start + 1] = ((cl_rel >> 16) & 0xff) as u8;
    colr[paint_start + 2] = ((cl_rel >> 8) & 0xff) as u8;
    colr[paint_start + 3] = (cl_rel & 0xff) as u8;
    colr.push(0); // extend = Pad
    colr.extend_from_slice(&2u16.to_be_bytes()); // numStops
                                                 // Stop 0: offset=0.0, palette=0, alpha=1.0
    colr.extend_from_slice(&f2dot14(0.0));
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    // Stop 1: offset=1.0, palette=1, alpha=1.0
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 0, 255, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate(&face, 33);
    assert_eq!(cmds.len(), 1);
    match &cmds[0] {
        DrawCmd::FillGlyph {
            paint: PaintSource::Gradient(g),
            ..
        } => {
            match g.kind {
                GradientKind::Linear { p0, p1, p2 } => {
                    assert_eq!(p0, (10.0, 20.0));
                    assert_eq!(p1, (30.0, 40.0));
                    assert_eq!(p2, (50.0, 60.0));
                }
                _ => panic!("expected linear gradient"),
            }
            assert_eq!(g.stops.len(), 2);
            assert!((g.stops[0].color.r - 1.0).abs() < 1e-6);
            assert!((g.stops[1].color.b - 1.0).abs() < 1e-6);
        }
        other => panic!("unexpected {other:?}"),
    }
}

// =========================================================================
// 4. Cycle: A -> B -> A returns truncated.
// =========================================================================

#[test]
fn paint_colr_glyph_cycle_truncates() {
    // We hand-build a COLR with TWO base-glyph paint records: glyph 1
    // and glyph 2. Glyph 1's paint is a `PaintColrGlyph(2)`; glyph 2's
    // is a `PaintColrGlyph(1)`. The walker must bail when it sees gid 1
    // again on the visited stack rather than recursing forever.
    let header_len = 30;
    let mut colr = Vec::new();
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&(header_len as u32).to_be_bytes());
    colr.extend_from_slice(&(header_len as u32).to_be_bytes());
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&(header_len as u32).to_be_bytes()); // baseGlyphListOffset
    colr.extend_from_slice(&0u32.to_be_bytes());
    colr.extend_from_slice(&0u32.to_be_bytes());
    colr.extend_from_slice(&0u32.to_be_bytes());

    // BaseGlyphList: 2 records.
    let bgl_start = colr.len();
    colr.extend_from_slice(&2u32.to_be_bytes());
    colr.extend_from_slice(&1u16.to_be_bytes()); // glyph 1
    colr.extend_from_slice(&16u32.to_be_bytes()); // paint offset rel = 16 (4 + 6 + 6)
    colr.extend_from_slice(&2u16.to_be_bytes()); // glyph 2
    colr.extend_from_slice(&19u32.to_be_bytes()); // 16 + 3

    // Paint for glyph 1: PaintColrGlyph(2) — { u8 fmt=11, u16 gid }
    let p1_start = colr.len();
    assert_eq!(p1_start - bgl_start, 16);
    colr.push(11);
    colr.extend_from_slice(&2u16.to_be_bytes());

    // Paint for glyph 2: PaintColrGlyph(1)
    let p2_start = colr.len();
    assert_eq!(p2_start - bgl_start, 19);
    colr.push(11);
    colr.extend_from_slice(&1u16.to_be_bytes());

    let cpal = build_cpal_v0(&[(0, 0, 0, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate(&face, 1);
    // No leaf paints in either tree — output must be empty, but the
    // important property is that we did not stack-overflow.
    assert!(cmds.is_empty(), "cycle should truncate, got {cmds:?}");
}

// =========================================================================
// 5. Composite emits PushLayer / PopLayer around the source paint.
// =========================================================================

#[test]
fn composite_wraps_source_with_push_pop() {
    let mut colr = build_v1_header(7);
    let composite_start = colr.len();
    colr.push(32); // PaintComposite
    colr.extend_from_slice(&[0, 0, 0]); // source Offset24
    colr.push(13); // mode = Screen
    colr.extend_from_slice(&[0, 0, 0]); // backdrop Offset24

    let src_start = colr.len();
    let src_rel = (src_start - composite_start) as u32;
    colr[composite_start + 1] = ((src_rel >> 16) & 0xff) as u8;
    colr[composite_start + 2] = ((src_rel >> 8) & 0xff) as u8;
    colr[composite_start + 3] = (src_rel & 0xff) as u8;
    colr.push(2); // src = Solid(palette 0)
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let bd_start = colr.len();
    let bd_rel = (bd_start - composite_start) as u32;
    colr[composite_start + 5] = ((bd_rel >> 16) & 0xff) as u8;
    colr[composite_start + 6] = ((bd_rel >> 8) & 0xff) as u8;
    colr[composite_start + 7] = (bd_rel & 0xff) as u8;
    colr.push(2); // backdrop = Solid(palette 1)
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 255, 0, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate(&face, 7);
    // Expected: backdrop fill, PushLayer, source fill, PopLayer.
    assert_eq!(cmds.len(), 4, "{cmds:?}");
    assert!(matches!(cmds[0], DrawCmd::FillGlyph { .. }));
    match cmds[1] {
        DrawCmd::PushLayer { composite_mode } => assert_eq!(composite_mode, CompositeMode::Screen),
        ref other => panic!("expected PushLayer, got {other:?}"),
    }
    assert!(matches!(cmds[2], DrawCmd::FillGlyph { .. }));
    assert!(matches!(cmds[3], DrawCmd::PopLayer));
}

// =========================================================================
// 6. Transform composition — Translate then Scale must yield the same
//    matrix the consumer would compose by hand.
// =========================================================================

#[test]
fn translate_then_scale_composes_into_fill_transform() {
    // Tree: Translate(10, 0) -> Scale(2, 2) -> Solid.
    // Walker collapses both into the FillGlyph's transform.
    let mut colr = build_v1_header(7);
    let translate_start = colr.len();
    colr.push(14); // PaintTranslate
    colr.extend_from_slice(&[0, 0, 0]);
    colr.extend_from_slice(&10i16.to_be_bytes());
    colr.extend_from_slice(&0i16.to_be_bytes());

    let scale_start = colr.len();
    let rel = (scale_start - translate_start) as u32;
    colr[translate_start + 1] = ((rel >> 16) & 0xff) as u8;
    colr[translate_start + 2] = ((rel >> 8) & 0xff) as u8;
    colr[translate_start + 3] = (rel & 0xff) as u8;
    colr.push(16); // PaintScale
    colr.extend_from_slice(&[0, 0, 0]);
    colr.extend_from_slice(&f2dot14(2.0));
    colr.extend_from_slice(&f2dot14(2.0));

    let solid_start = colr.len();
    let rel2 = (solid_start - scale_start) as u32;
    colr[scale_start + 1] = ((rel2 >> 16) & 0xff) as u8;
    colr[scale_start + 2] = ((rel2 >> 8) & 0xff) as u8;
    colr[scale_start + 3] = (rel2 & 0xff) as u8;
    colr.push(2);
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(255, 255, 255, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate(&face, 7);
    assert_eq!(cmds.len(), 1);
    match cmds[0] {
        DrawCmd::FillGlyph { transform, .. } => {
            // COLRv1 transforms wrap their child: the outer paint's
            // transform applies last. Tree is `Translate(Scale(Solid))`,
            // so a point in the child's frame is first scaled, then
            // translated. Point (1, 0) in solid space goes to (2, 0)
            // by Scale(2, 2), then to (12, 0) by Translate(10, 0).
            let (x, y) = transform.apply(1.0, 0.0);
            assert!((x - 12.0).abs() < 1e-3, "x was {x}");
            assert!((y).abs() < 1e-3, "y was {y}");
            // And the origin lands at the translation alone.
            let (ox, oy) = transform.apply(0.0, 0.0);
            assert!((ox - 10.0).abs() < 1e-3, "origin x was {ox}");
            assert!((oy).abs() < 1e-3, "origin y was {oy}");
        }
        ref other => panic!("unexpected {other:?}"),
    }
}

// =========================================================================
// 7. PaintVar* under default coords matches the static path.
// =========================================================================

#[test]
fn var_solid_with_empty_coords_is_identity() {
    // PaintVarSolid: format=3, paletteIndex, alpha, varIndexBase.
    // With empty coords no deltas apply — output must match the
    // PaintSolid case byte-for-byte aside from variant.
    let mut colr = build_v1_header(7);
    colr.push(3); // PaintVarSolid
    colr.extend_from_slice(&0u16.to_be_bytes()); // paletteIndex
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // var sentinel

    let cpal = build_cpal_v0(&[(64, 128, 192, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate_at_coords(&face, 7, &[]);
    assert_eq!(cmds.len(), 1);
    match &cmds[0] {
        DrawCmd::FillGlyph {
            paint: PaintSource::Solid(c),
            ..
        } => {
            assert!((c.r - 64.0 / 255.0).abs() < 1e-6);
            assert!((c.g - 128.0 / 255.0).abs() < 1e-6);
            assert!((c.b - 192.0 / 255.0).abs() < 1e-6);
            assert!((c.a - 1.0).abs() < 1e-6);
        }
        other => panic!("unexpected {other:?}"),
    }
}

// =========================================================================
// 8. Determinism: same inputs => same outputs.
// =========================================================================

#[test]
fn determinism_of_repeated_evaluate() {
    let mut colr = build_v1_header(7);
    colr.push(2);
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    let cpal = build_cpal_v0(&[(10, 20, 30, 40)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let a = evaluate(&face, 7);
    let b = evaluate(&face, 7);
    let c = evaluate(&face, 7);
    assert_eq!(a.len(), b.len());
    assert_eq!(b.len(), c.len());
    for (x, y) in a.iter().zip(b.iter()) {
        // DrawCmd doesn't impl PartialEq so spot-check the shape.
        match (x, y) {
            (
                DrawCmd::FillGlyph {
                    gid: g1,
                    paint: PaintSource::Solid(c1),
                    ..
                },
                DrawCmd::FillGlyph {
                    gid: g2,
                    paint: PaintSource::Solid(c2),
                    ..
                },
            ) => {
                assert_eq!(g1, g2);
                assert_eq!(c1, c2);
            }
            _ => panic!("unexpected divergence"),
        }
    }
}

// =========================================================================
// 9. Malformed input: bad offset truncates without panicking.
// =========================================================================

#[test]
fn unknown_glyph_returns_empty() {
    let mut colr = build_v1_header(7);
    colr.push(2);
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    let cpal = build_cpal_v0(&[(0, 0, 0, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    // Glyph id 999 has no paint record.
    assert!(evaluate(&face, 999).is_empty());
}

// =========================================================================
// 10. PaintRadialGradient: two-circle gradient resolves geometry, stops,
//     and extend mode.
// =========================================================================

#[test]
fn radial_gradient_resolves_two_circle_geometry_and_stops() {
    // PaintRadialGradient layout: u8 fmt=6, Offset24 colorLine, i16 x0,
    // i16 y0, u16 r0, i16 x1, i16 y1, u16 r1 — 14 bytes after the
    // header. Inner circle: (0,0) radius 0; outer circle: (100,100)
    // radius 200; two stops (red @0, blue @1); Pad extend.
    let mut colr = build_v1_header(50);
    let paint_start = colr.len();
    colr.push(6); // PaintRadialGradient
    colr.extend_from_slice(&[0, 0, 0]); // Offset24 colorLine placeholder
    colr.extend_from_slice(&0i16.to_be_bytes()); // x0
    colr.extend_from_slice(&0i16.to_be_bytes()); // y0
    colr.extend_from_slice(&0u16.to_be_bytes()); // r0
    colr.extend_from_slice(&100i16.to_be_bytes()); // x1
    colr.extend_from_slice(&100i16.to_be_bytes()); // y1
    colr.extend_from_slice(&200u16.to_be_bytes()); // r1

    let cl_start = colr.len();
    let cl_rel = (cl_start - paint_start) as u32;
    colr[paint_start + 1] = ((cl_rel >> 16) & 0xff) as u8;
    colr[paint_start + 2] = ((cl_rel >> 8) & 0xff) as u8;
    colr[paint_start + 3] = (cl_rel & 0xff) as u8;
    colr.push(0); // extend = Pad
    colr.extend_from_slice(&2u16.to_be_bytes()); // numStops
                                                 // Stop 0: offset=0.0, palette=0 (red), alpha=1.0
    colr.extend_from_slice(&f2dot14(0.0));
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    // Stop 1: offset=1.0, palette=1 (blue), alpha=1.0
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 0, 255, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let cmds = evaluate(&face, 50);
    assert_eq!(cmds.len(), 1, "expected exactly one FillGlyph");
    match &cmds[0] {
        DrawCmd::FillGlyph {
            paint: PaintSource::Gradient(g),
            ..
        } => {
            match g.kind {
                GradientKind::Radial { c0, r0, c1, r1 } => {
                    assert_eq!(c0, (0.0, 0.0), "inner centre");
                    assert!(r0.abs() < 1e-6, "inner radius was {r0}");
                    assert_eq!(c1, (100.0, 100.0), "outer centre");
                    assert!((r1 - 200.0).abs() < 1e-6, "outer radius was {r1}");
                }
                ref other => panic!("expected radial gradient, got {other:?}"),
            }
            assert_eq!(g.extend, Extend::Pad);
            assert_eq!(g.stops.len(), 2);
            // Stop 0 → red (palette 0).
            assert!((g.stops[0].offset).abs() < 1e-6);
            assert!((g.stops[0].color.r - 1.0).abs() < 1e-6);
            assert!((g.stops[0].color.g).abs() < 1e-6);
            assert!((g.stops[0].color.b).abs() < 1e-6);
            // Stop 1 → blue (palette 1).
            assert!((g.stops[1].offset - 1.0).abs() < 1e-3);
            assert!((g.stops[1].color.b - 1.0).abs() < 1e-6);
            assert!((g.stops[1].color.r).abs() < 1e-6);
        }
        other => panic!("unexpected {other:?}"),
    }
}

// =========================================================================
// 11. PaintSweepGradient: conic gradient resolves centre, angles, stops,
//     and extend mode.
// =========================================================================

#[test]
fn sweep_gradient_resolves_centre_angles_and_three_stops() {
    // PaintSweepGradient layout: u8 fmt=8, Offset24 colorLine, i16 cx,
    // i16 cy, F2Dot14 startAngle, F2Dot14 endAngle — 11 bytes after
    // the header. COLRv1 stores angles as F2Dot14 multiples of 180°,
    // so endAngle = π radians == on-disk 1.0.
    let mut colr = build_v1_header(60);
    let paint_start = colr.len();
    colr.push(8); // PaintSweepGradient
    colr.extend_from_slice(&[0, 0, 0]); // Offset24 colorLine placeholder
    colr.extend_from_slice(&50i16.to_be_bytes()); // cx
    colr.extend_from_slice(&50i16.to_be_bytes()); // cy
    colr.extend_from_slice(&f2dot14(0.0)); // startAngle = 0
    colr.extend_from_slice(&f2dot14(1.0)); // endAngle = 1.0 → π rad

    let cl_start = colr.len();
    let cl_rel = (cl_start - paint_start) as u32;
    colr[paint_start + 1] = ((cl_rel >> 16) & 0xff) as u8;
    colr[paint_start + 2] = ((cl_rel >> 8) & 0xff) as u8;
    colr[paint_start + 3] = (cl_rel & 0xff) as u8;
    colr.push(2); // extend = Reflect
    colr.extend_from_slice(&3u16.to_be_bytes()); // numStops
                                                 // Stop 0: offset=0.0, palette=0 (red), alpha=1.0
    colr.extend_from_slice(&f2dot14(0.0));
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    // Stop 1: offset=0.5, palette=1 (green), alpha=1.0
    colr.extend_from_slice(&f2dot14(0.5));
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    // Stop 2: offset=1.0, palette=2 (blue), alpha=1.0
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&2u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[
        (255, 0, 0, 255),
        (0, 255, 0, 255),
        (0, 0, 255, 255),
    ]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let cmds = evaluate(&face, 60);
    assert_eq!(cmds.len(), 1, "expected exactly one FillGlyph");
    match &cmds[0] {
        DrawCmd::FillGlyph {
            paint: PaintSource::Gradient(g),
            ..
        } => {
            match g.kind {
                GradientKind::Sweep {
                    center,
                    start_angle,
                    end_angle,
                } => {
                    assert_eq!(center, (50.0, 50.0));
                    assert!(start_angle.abs() < 1e-6, "start_angle = {start_angle}");
                    // F2Dot14 1.0 corresponds to π radians.
                    let pi = core::f32::consts::PI;
                    assert!(
                        (end_angle - pi).abs() < 1e-4,
                        "end_angle was {end_angle}, expected π"
                    );
                }
                ref other => panic!("expected sweep gradient, got {other:?}"),
            }
            assert_eq!(g.extend, Extend::Reflect);
            assert_eq!(g.stops.len(), 3);
            // Verify per-stop colours map to red / green / blue.
            assert!((g.stops[0].color.r - 1.0).abs() < 1e-6);
            assert!((g.stops[1].color.g - 1.0).abs() < 1e-6);
            assert!((g.stops[2].color.b - 1.0).abs() < 1e-6);
            // And offsets ascend.
            assert!(g.stops[0].offset < g.stops[1].offset);
            assert!(g.stops[1].offset < g.stops[2].offset);
            assert!((g.stops[1].offset - 0.5).abs() < 1e-3);
        }
        other => panic!("unexpected {other:?}"),
    }
}
