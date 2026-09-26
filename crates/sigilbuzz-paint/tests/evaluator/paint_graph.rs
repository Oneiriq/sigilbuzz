//! Paint-graph walks: solid leaves, PaintGlyph, cycle truncation,
//! composites, determinism and unknown glyphs.

use sigilbuzz::tables::colr::CompositeMode;
use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate, DrawCmd, PaintSource};

use crate::fixtures::{build_cpal_v0, build_face_bytes, build_v1_header, f2dot14};

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
            PaintSource::Solid { color: c, .. } => {
                assert!((c.r).abs() < 1e-6);
                assert!((c.g - 1.0).abs() < 1e-6);
                assert!((c.b).abs() < 1e-6);
                assert!((c.a - 1.0).abs() < 1e-6);
            }
            other @ PaintSource::Gradient(_) => panic!("expected solid, got {other:?}"),
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
            paint: PaintSource::Solid { color: c, .. },
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
            assert!(matches!(paint, PaintSource::Solid { .. }));
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
    let header_len = 34;
    let mut colr = Vec::new();
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&(header_len as u32).to_be_bytes());
    colr.extend_from_slice(&(header_len as u32).to_be_bytes());
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&(header_len as u32).to_be_bytes()); // baseGlyphListOffset
    colr.extend_from_slice(&[0; 16]); // layer list, clip list, index map, store

    // BaseGlyphList: 2 records.
    let bgl_start = colr.len();
    colr.extend_from_slice(&2u32.to_be_bytes());
    colr.extend_from_slice(&1u16.to_be_bytes()); // glyph 1
    colr.extend_from_slice(&16u32.to_be_bytes()); // paint offset rel = 16 (4 + 6 + 6)
    colr.extend_from_slice(&2u16.to_be_bytes()); // glyph 2
    colr.extend_from_slice(&19u32.to_be_bytes()); // 16 + 3

    // Paint for glyph 1: PaintColrGlyph(2), { u8 fmt=11, u16 gid }
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
    // No leaf paints in either tree. Output must be empty, but the
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
    // Expected: an isolating SrcOver layer holding the backdrop fill
    // and a Screen layer holding the source fill.
    assert_eq!(cmds.len(), 6, "{cmds:?}");
    let layer_mode = |cmd: &DrawCmd| match cmd {
        DrawCmd::PushLayer { composite_mode } => *composite_mode,
        other => panic!("expected PushLayer, got {other:?}"),
    };
    assert_eq!(layer_mode(&cmds[0]), CompositeMode::SrcOver);
    assert!(matches!(cmds[1], DrawCmd::FillGlyph { .. }));
    assert_eq!(layer_mode(&cmds[2]), CompositeMode::Screen);
    assert!(matches!(cmds[3], DrawCmd::FillGlyph { .. }));
    assert!(matches!(cmds[4], DrawCmd::PopLayer));
    assert!(matches!(cmds[5], DrawCmd::PopLayer));
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
                    paint: PaintSource::Solid { color: c1, .. },
                    ..
                },
                DrawCmd::FillGlyph {
                    gid: g2,
                    paint: PaintSource::Solid { color: c2, .. },
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
