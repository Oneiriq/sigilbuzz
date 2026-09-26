//! Transform paints: placement relative to PaintGlyph and matrix
//! composition.

use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate, DrawCmd, GradientKind, PaintSource};

use crate::fixtures::{build_cpal_v0, build_face_bytes, build_v1_header, f2dot14};

// =========================================================================
// 5b. A transform below a PaintGlyph moves the paint, not the outline;
//     one above it moves both.
// =========================================================================

/// PaintGlyph(`gid`) whose child starts right after its 6 bytes.
fn paint_glyph_head(gid: u16) -> Vec<u8> {
    let mut p = vec![10u8, 0, 0, 6];
    p.extend_from_slice(&gid.to_be_bytes());
    p
}

/// PaintTransform (scale 2, translate (10, 0)) whose child follows its
/// 7-byte record and 24-byte Affine2x3.
fn scale_two_head() -> Vec<u8> {
    let mut p = vec![12u8, 0, 0, 31, 0, 0, 7];
    for v in [2i32, 0, 0, 2, 10, 0] {
        p.extend_from_slice(&(v << 16).to_be_bytes());
    }
    p
}

/// Linear gradient from (0, 0) to (100, 0), rotation point (0, 100).
fn linear_red_blue() -> Vec<u8> {
    let mut p = vec![4u8, 0, 0, 16];
    for v in [0i16, 0, 100, 0, 0, 100] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.push(0);
    p.extend_from_slice(&2u16.to_be_bytes());
    for (offset, entry) in [(0.0, 0u16), (1.0, 1)] {
        p.extend_from_slice(&f2dot14(offset));
        p.extend_from_slice(&entry.to_be_bytes());
        p.extend_from_slice(&f2dot14(1.0));
    }
    p
}

fn only_fill(cmds: &[DrawCmd]) -> (u16, sigilbuzz_paint::Transform2D, GradientKind) {
    match cmds {
        [DrawCmd::FillGlyph {
            gid,
            transform,
            paint: PaintSource::Gradient(g),
        }] => (*gid, *transform, g.kind),
        other => panic!("expected one gradient fill, got {other:?}"),
    }
}

#[test]
fn transform_below_paint_glyph_moves_only_the_gradient() {
    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 0, 255, 255)]);

    // PaintGlyph(5) -> PaintTransform -> gradient.
    let mut below = build_v1_header(7);
    below.extend_from_slice(&paint_glyph_head(5));
    below.extend_from_slice(&scale_two_head());
    below.extend_from_slice(&linear_red_blue());
    let bytes = build_face_bytes(&below, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let (gid, transform, kind) = only_fill(&evaluate(&face, 7));
    assert_eq!(gid, 5);
    assert_eq!(transform, sigilbuzz_paint::Transform2D::IDENTITY);
    assert_eq!(
        kind,
        GradientKind::Linear {
            p0: (10.0, 0.0),
            p1: (210.0, 0.0),
            p2: (10.0, 200.0),
        }
    );

    // PaintTransform -> PaintGlyph(5) -> gradient: outline and paint
    // move together, and the gradient keeps its own coordinates.
    let mut above = build_v1_header(7);
    above.extend_from_slice(&scale_two_head());
    above.extend_from_slice(&paint_glyph_head(5));
    above.extend_from_slice(&linear_red_blue());
    let bytes = build_face_bytes(&above, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let (gid, transform, kind) = only_fill(&evaluate(&face, 7));
    assert_eq!(gid, 5);
    assert_eq!(transform.apply(1.0, 1.0), (12.0, 2.0));
    assert_eq!(
        kind,
        GradientKind::Linear {
            p0: (0.0, 0.0),
            p1: (100.0, 0.0),
            p2: (0.0, 100.0),
        }
    );
}

// =========================================================================
// 6. Transform composition: Translate then Scale must yield the same
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
