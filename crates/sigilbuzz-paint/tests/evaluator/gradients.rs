//! Gradient paints: linear, radial and sweep stops and geometry.

use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate, DrawCmd, Extend, GradientKind, PaintSource};

use crate::fixtures::{build_cpal_v0, build_face_bytes, build_v1_header, f2dot14};

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
// 10. PaintRadialGradient: two-circle gradient resolves geometry, stops,
//     and extend mode.
// =========================================================================

#[test]
fn radial_gradient_resolves_two_circle_geometry_and_stops() {
    // PaintRadialGradient layout: u8 fmt=6, Offset24 colorLine, i16 x0,
    // i16 y0, u16 r0, i16 x1, i16 y1, u16 r1, 14 bytes after the
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
            // Stop 0 -> red (palette 0).
            assert!((g.stops[0].offset).abs() < 1e-6);
            assert!((g.stops[0].color.r - 1.0).abs() < 1e-6);
            assert!((g.stops[0].color.g).abs() < 1e-6);
            assert!((g.stops[0].color.b).abs() < 1e-6);
            // Stop 1 -> blue (palette 1).
            assert!((g.stops[1].offset - 1.0).abs() < 1e-3);
            assert!((g.stops[1].color.b - 1.0).abs() < 1e-6);
            assert!((g.stops[1].color.r).abs() < 1e-6);
        }
        other => panic!("unexpected {other:?}"),
    }
}

// =========================================================================
// 11. PaintSweepGradient: conic gradient resolves center, angles, stops,
//     and extend mode.
// =========================================================================

#[test]
fn sweep_gradient_resolves_centre_angles_and_three_stops() {
    // PaintSweepGradient layout: u8 fmt=8, Offset24 colorLine, i16 cx,
    // i16 cy, F2Dot14 startAngle, F2Dot14 endAngle, 11 bytes after
    // the header. COLRv1 stores sweep angles as F2Dot14 multiples of
    // 180 degrees with a bias of 1.0, so on-disk -1.0 is 0 radians and
    // on-disk 0.0 is pi.
    let mut colr = build_v1_header(60);
    let paint_start = colr.len();
    colr.push(8); // PaintSweepGradient
    colr.extend_from_slice(&[0, 0, 0]); // Offset24 colorLine placeholder
    colr.extend_from_slice(&50i16.to_be_bytes()); // cx
    colr.extend_from_slice(&50i16.to_be_bytes()); // cy
    colr.extend_from_slice(&f2dot14(-1.0)); // startAngle -> 0 rad
    colr.extend_from_slice(&f2dot14(0.0)); // endAngle -> pi rad

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

    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 255, 0, 255), (0, 0, 255, 255)]);
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
                    assert_eq!(start_angle, 0.0, "start_angle = {start_angle}");
                    // Biased F2Dot14 0.0 corresponds to pi radians.
                    let pi = core::f32::consts::PI;
                    assert_eq!(end_angle, pi, "end_angle was {end_angle}, expected pi");
                }
                ref other => panic!("expected sweep gradient, got {other:?}"),
            }
            assert_eq!(g.extend, Extend::Reflect);
            assert_eq!(g.stops.len(), 3);
            // Verify per-stop colors map to red / green / blue.
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
