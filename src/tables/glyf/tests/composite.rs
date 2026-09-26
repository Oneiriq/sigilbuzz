//! Tests for composite glyph flattening.

use super::*;

// ------------------------------------------------------------------
// Composite-glyph flattening.
// ------------------------------------------------------------------

#[test]
fn composite_glyph_translates_child_outline() {
    // Child (glyph 1): rectangle at origin 0..100 x 0..100.
    let child = build_simple_glyph(
        &[3],
        &[
            (0, 0, true),
            (100, 0, true),
            (100, 100, true),
            (0, 100, true),
        ],
    );
    // Parent (glyph 0): composite referencing child with translation (+200, +300).
    let mut parent = build_header(-1, 0, 0, 400, 500);
    let flags: u16 = COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS;
    parent.extend_from_slice(&flags.to_be_bytes());
    parent.extend_from_slice(&1u16.to_be_bytes()); // component id = 1
    parent.extend_from_slice(&200i16.to_be_bytes()); // dx
    parent.extend_from_slice(&300i16.to_be_bytes()); // dy
                                                     // no MORE_COMPONENTS -> single component.

    // Lay out glyf with parent first, child second.
    let mut glyf_bytes = Vec::new();
    let parent_off = 0u32;
    glyf_bytes.extend_from_slice(&parent);
    // Pad to even boundary (short loca format needs even offsets).
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let child_off = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&child);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let end_off = glyf_bytes.len() as u32;

    let loca_bytes = build_loca_short(&[
        (parent_off / 2) as u16,
        (child_off / 2) as u16,
        (end_off / 2) as u16,
    ]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
    let glyf = Glyf::new(&glyf_bytes);

    let mut o = Outline::new();
    glyf.outline(&loca, 0, None, None, &mut o).unwrap();
    // Child starts at (0,0), translated to (200, 300). The
    // closing LineTo brings the pen back to the start before
    // Close (matches ttf-parser's convention).
    assert!(matches!(o.ops()[0], PathOp::MoveTo { x: 200.0, y: 300.0 }));
    assert!(matches!(o.ops()[1], PathOp::LineTo { x: 300.0, y: 300.0 }));
    assert!(matches!(o.ops()[2], PathOp::LineTo { x: 300.0, y: 400.0 }));
    assert!(matches!(o.ops()[3], PathOp::LineTo { x: 200.0, y: 400.0 }));
    assert!(matches!(o.ops()[4], PathOp::LineTo { x: 200.0, y: 300.0 }));
    assert!(matches!(o.ops()[5], PathOp::Close));
}

#[test]
fn composite_with_scale_doubles_child() {
    // Child: unit square at (0,0)..(100,100). Parent scales x2.
    let child = build_simple_glyph(
        &[3],
        &[
            (0, 0, true),
            (100, 0, true),
            (100, 100, true),
            (0, 100, true),
        ],
    );
    let mut parent = build_header(-1, 0, 0, 200, 200);
    let flags: u16 = COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_WE_HAVE_A_SCALE;
    parent.extend_from_slice(&flags.to_be_bytes());
    parent.extend_from_slice(&1u16.to_be_bytes());
    parent.extend_from_slice(&0i16.to_be_bytes());
    parent.extend_from_slice(&0i16.to_be_bytes());
    // Scale 2.0 in F2Dot14 = 32768, but that overflows i16: the
    // spec tops out at 2x so store 0x7FFF as a close proxy, or
    // just test with 1.5 (which fits as 24576).
    let scale_raw: i16 = 24576; // 1.5
    parent.extend_from_slice(&scale_raw.to_be_bytes());

    let mut glyf_bytes = Vec::new();
    let parent_off = 0u32;
    glyf_bytes.extend_from_slice(&parent);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let child_off = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&child);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let end_off = glyf_bytes.len() as u32;

    let loca_bytes = build_loca_short(&[
        (parent_off / 2) as u16,
        (child_off / 2) as u16,
        (end_off / 2) as u16,
    ]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
    let glyf = Glyf::new(&glyf_bytes);

    let mut o = Outline::new();
    glyf.outline(&loca, 0, None, None, &mut o).unwrap();
    // 1.5 * (100, 100) = (150, 150).
    match o.ops()[2] {
        PathOp::LineTo { x, y } => {
            assert!((x - 150.0).abs() < 1e-3);
            assert!((y - 150.0).abs() < 1e-3);
        }
        _ => panic!("expected LineTo at 2"),
    }
}

#[test]
fn composite_anchor_mode_translates_child_to_parent_anchor() {
    // Two-component composite (glyph 0):
    //   1. First component is a contour that *contributes* the
    //      parent's flattened points: a 4-point square anchored
    //      at (10, 20)..(20, 20)..(20, 30)..(10, 30). It draws
    //      itself unchanged.
    //   2. Second component (glyph 2) is a single triangle whose
    //      first point is (0, 0). It is matched in anchor mode
    //      with arg1=1 (parent point index 1 -> (20, 20)) and
    //      arg2=0 (child point index 0 -> (0, 0)). The implied
    //      translation is parent[1] - child[0] = (20, 20).
    //
    // The test confirms:
    //   - Anchor mode reads two unsigned bytes (no XY_VALUES, no
    //     WORDS) and treats them as point indices.
    //   - The translation is computed from the parent's already-
    //     flattened points (component 1) and the child's own
    //     anchor point.
    //   - Child ops are emitted with the resolved translation.
    //
    // Glyph layout: 0 = composite parent, 1 = parent's "anchor"
    // donor (a 4-point square), 2 = anchor-mode child (triangle).

    // Glyph 1: anchor-donor square at (10,20),(20,20),(20,30),(10,30).
    let g1 = build_simple_glyph(
        &[3],
        &[
            (10, 20, true),
            (20, 20, true),
            (20, 30, true),
            (10, 30, true),
        ],
    );

    // Glyph 2: triangle at (0,0),(40,0),(0,40).
    let g2 = build_simple_glyph(&[2], &[(0, 0, true), (40, 0, true), (0, 40, true)]);

    // Glyph 0: composite. First component glyph 1 with xy
    // translation (0, 0); second component glyph 2 in anchor mode
    // (arg1=1 -> parent point 1 = (20, 20); arg2=0 -> child point 0
    // = (0, 0)).
    let mut g0 = build_header(-1, 0, 0, 100, 100);
    // Component A: glyph 1, xy_values, words, MORE_COMPONENTS.
    let flags_a: u16 = COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_MORE_COMPONENTS;
    g0.extend_from_slice(&flags_a.to_be_bytes());
    g0.extend_from_slice(&1u16.to_be_bytes());
    g0.extend_from_slice(&0i16.to_be_bytes());
    g0.extend_from_slice(&0i16.to_be_bytes());
    // Component B: glyph 2, anchor mode (no XY_VALUES, no WORDS,
    // last component).
    let flags_b: u16 = 0; // anchor mode, byte args, last.
    g0.extend_from_slice(&flags_b.to_be_bytes());
    g0.extend_from_slice(&2u16.to_be_bytes());
    g0.push(1u8); // arg1 = parent point 1
    g0.push(0u8); // arg2 = child point 0

    // Lay out the glyf table with each glyph on a 2-byte boundary
    // for the short loca format.
    let mut glyf_bytes = Vec::new();
    let off0 = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&g0);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let off1 = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&g1);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let off2 = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&g2);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let off_end = glyf_bytes.len() as u32;

    let loca_bytes = build_loca_short(&[
        (off0 / 2) as u16,
        (off1 / 2) as u16,
        (off2 / 2) as u16,
        (off_end / 2) as u16,
    ]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 3).unwrap();
    let glyf = Glyf::new(&glyf_bytes);

    let mut o = Outline::new();
    glyf.outline(&loca, 0, None, None, &mut o).unwrap();

    // First six ops: square contour from glyph 1 unchanged.
    assert!(matches!(o.ops()[0], PathOp::MoveTo { x: 10.0, y: 20.0 }));
    assert!(matches!(o.ops()[1], PathOp::LineTo { x: 20.0, y: 20.0 }));
    assert!(matches!(o.ops()[2], PathOp::LineTo { x: 20.0, y: 30.0 }));
    assert!(matches!(o.ops()[3], PathOp::LineTo { x: 10.0, y: 30.0 }));
    assert!(matches!(o.ops()[4], PathOp::LineTo { x: 10.0, y: 20.0 }));
    assert!(matches!(o.ops()[5], PathOp::Close));

    // Anchor-mode triangle: child[0] = (0, 0) lands on parent[1]
    // = (20, 20), so every child point shifts by (+20, +20).
    // (0,0)->(20,20), (40,0)->(60,20), (0,40)->(20,60).
    assert!(matches!(o.ops()[6], PathOp::MoveTo { x: 20.0, y: 20.0 }));
    assert!(matches!(o.ops()[7], PathOp::LineTo { x: 60.0, y: 20.0 }));
    assert!(matches!(o.ops()[8], PathOp::LineTo { x: 20.0, y: 60.0 }));
    assert!(matches!(o.ops()[9], PathOp::LineTo { x: 20.0, y: 20.0 }));
    assert!(matches!(o.ops()[10], PathOp::Close));
}

#[test]
fn composite_two_by_two_uses_column_major_layout() {
    // OpenType stores the 2x2 in column-major order. A 90° CCW
    // rotation has xscale=0, scale01=1, scale10=-1, yscale=0, so
    // (x, y) -> (-y, x). Pin that mapping with a single-point
    // contour at (10, 0): after rotation it should land at
    // (0, 10), and with translation (50, 5) at (50, 15).
    let child = build_simple_glyph(&[0], &[(10, 0, true)]);
    let mut parent = build_header(-1, 0, 0, 100, 100);
    let flags: u16 =
        COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_WE_HAVE_A_TWO_BY_TWO;
    parent.extend_from_slice(&flags.to_be_bytes());
    parent.extend_from_slice(&1u16.to_be_bytes());
    parent.extend_from_slice(&50i16.to_be_bytes()); // dx
    parent.extend_from_slice(&5i16.to_be_bytes()); //  dy
    let one = 16384i16; // 1.0 in F2Dot14
    parent.extend_from_slice(&0i16.to_be_bytes()); // xscale = 0
    parent.extend_from_slice(&one.to_be_bytes()); //  scale01 = 1
    parent.extend_from_slice(&(-one).to_be_bytes()); // scale10 = -1
    parent.extend_from_slice(&0i16.to_be_bytes()); // yscale = 0

    let mut glyf_bytes = Vec::new();
    let p_off = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&parent);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let c_off = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&child);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let end_off = glyf_bytes.len() as u32;
    let loca_bytes =
        build_loca_short(&[(p_off / 2) as u16, (c_off / 2) as u16, (end_off / 2) as u16]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
    let glyf = Glyf::new(&glyf_bytes);
    let mut o = Outline::new();
    glyf.outline(&loca, 0, None, None, &mut o).unwrap();
    match o.ops()[0] {
        PathOp::MoveTo { x, y } => {
            assert!((x - 50.0).abs() < 1e-3, "x = {x}");
            assert!((y - 15.0).abs() < 1e-3, "y = {y}");
        }
        other => panic!("expected MoveTo, got {other:?}"),
    }
}

#[test]
fn composite_anchor_mode_resolves_parent_phantom_point() {
    // Parent (glyph 0) is a composite with two components:
    //   - Component A (glyph 1): a square at (10, 0) -> (40, 30).
    //     Its xMin=10 and lsb=4 imply pp1=(6,0) and
    //     pp2=(6+advance,0).
    //     The composite parent inherits its own metrics from
    //     gid 0 (advance=300, lsb=4); xMin/yMax from the parent
    //     header are 10 and 30. Parent's own pp1=(6,0),
    //     pp2=(306,0).
    //   - Component B (glyph 2): triangle (0,0)/(40,0)/(0,40).
    //     Anchor mode targets parent's pp2 (index =
    //     numContourPoints + 1) and child's own point 0.
    //
    // Expected translation = parent.pp2 - child[0]
    //   = (306, 0) - (0, 0) = (306, 0).
    let g1 = build_simple_glyph(
        &[3],
        &[(10, 0, true), (40, 0, true), (40, 30, true), (10, 30, true)],
    );
    let g2 = build_simple_glyph(&[2], &[(0, 0, true), (40, 0, true), (0, 40, true)]);

    // Parent composite header. xMin=10, yMin=0, xMax=40, yMax=30
    // matching the donor square so the parent's bounds line up
    // with its real points.
    let mut g0 = build_header(-1, 10, 0, 40, 30);
    let flags_a: u16 = COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_MORE_COMPONENTS;
    g0.extend_from_slice(&flags_a.to_be_bytes());
    g0.extend_from_slice(&1u16.to_be_bytes());
    g0.extend_from_slice(&0i16.to_be_bytes());
    g0.extend_from_slice(&0i16.to_be_bytes());
    // Component B in anchor mode (no XY_VALUES, no WORDS, last).
    // Parent has 4 real points after component A; index 5 = pp2.
    // Child has 3 real points; index 0 = first contour point.
    let flags_b: u16 = 0;
    g0.extend_from_slice(&flags_b.to_be_bytes());
    g0.extend_from_slice(&2u16.to_be_bytes());
    g0.push(5u8); // arg1 = parent pp2 (numContourPoints + 1)
    g0.push(0u8); // arg2 = child point 0

    let mut glyf_bytes = Vec::new();
    let off0 = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&g0);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let off1 = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&g1);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let off2 = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&g2);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let off_end = glyf_bytes.len() as u32;

    let loca_bytes = build_loca_short(&[
        (off0 / 2) as u16,
        (off1 / 2) as u16,
        (off2 / 2) as u16,
        (off_end / 2) as u16,
    ]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 3).unwrap();
    let glyf = Glyf::new(&glyf_bytes);

    // Per-glyph metrics. Parent (gid 0): advance=300, lsb=4 ->
    // pp1=(10-4, 0)=(6,0), pp2=(306,0). Other glyphs need only
    // be parseable.
    let hmtx_bytes = build_hmtx(&[(300, 4), (60, 4), (40, 0)]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 3, 3).unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: None,
    };

    let mut o = Outline::new();
    glyf.outline(&loca, 0, None, Some(&metrics), &mut o)
        .unwrap();

    // First six ops are component A's square unchanged.
    // Ops 6..= are the anchor-mode triangle, translated by
    // parent.pp2 = (306, 0).
    // child[0]=(0,0)   -> (306, 0)
    // child[1]=(40,0)  -> (346, 0)
    // child[2]=(0,40)  -> (306, 40)
    match o.ops()[6] {
        PathOp::MoveTo { x, y } => {
            assert!((x - 306.0).abs() < 1e-4, "got x={x}");
            assert!((y - 0.0).abs() < 1e-4, "got y={y}");
        }
        other => panic!("expected MoveTo at 6, got {other:?}"),
    }
    match o.ops()[7] {
        PathOp::LineTo { x, y } => {
            assert!((x - 346.0).abs() < 1e-4);
            assert!((y - 0.0).abs() < 1e-4);
        }
        other => panic!("expected LineTo at 7, got {other:?}"),
    }
    match o.ops()[8] {
        PathOp::LineTo { x, y } => {
            assert!((x - 306.0).abs() < 1e-4);
            assert!((y - 40.0).abs() < 1e-4);
        }
        other => panic!("expected LineTo at 8, got {other:?}"),
    }
}

#[test]
fn composite_anchor_phantom_without_metrics_falls_back_to_zero() {
    // Same composite shape as the phantom-resolution test, but
    // with `metrics=None`. The legacy fallback applies: the
    // anchor index is out-of-range and the translation collapses
    // to (0, 0). Pin the behavior so callers that opt out of
    // phantom resolution still get a stable answer.
    let g1 = build_simple_glyph(
        &[3],
        &[(0, 0, true), (10, 0, true), (10, 10, true), (0, 10, true)],
    );
    let g2 = build_simple_glyph(&[0], &[(0, 0, true)]);

    let mut g0 = build_header(-1, 0, 0, 10, 10);
    let flags_a: u16 = COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_MORE_COMPONENTS;
    g0.extend_from_slice(&flags_a.to_be_bytes());
    g0.extend_from_slice(&1u16.to_be_bytes());
    g0.extend_from_slice(&0i16.to_be_bytes());
    g0.extend_from_slice(&0i16.to_be_bytes());
    let flags_b: u16 = 0;
    g0.extend_from_slice(&flags_b.to_be_bytes());
    g0.extend_from_slice(&2u16.to_be_bytes());
    g0.push(5u8); // pp2
    g0.push(0u8);

    let mut glyf_bytes = Vec::new();
    glyf_bytes.extend_from_slice(&g0);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let off1 = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&g1);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let off2 = glyf_bytes.len() as u32;
    glyf_bytes.extend_from_slice(&g2);
    if glyf_bytes.len() % 2 != 0 {
        glyf_bytes.push(0);
    }
    let off_end = glyf_bytes.len() as u32;
    let loca_bytes = build_loca_short(&[
        0,
        (off1 / 2) as u16,
        (off2 / 2) as u16,
        (off_end / 2) as u16,
    ]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 3).unwrap();
    let glyf = Glyf::new(&glyf_bytes);

    let mut o = Outline::new();
    glyf.outline(&loca, 0, None, None, &mut o).unwrap();
    // Component A drew 4 points + close, so child's MoveTo lands
    // at op index 6 with no translation: child[0]=(0,0).
    match o.ops()[6] {
        PathOp::MoveTo { x, y } => {
            assert!((x - 0.0).abs() < 1e-4);
            assert!((y - 0.0).abs() < 1e-4);
        }
        other => panic!("expected MoveTo at 6, got {other:?}"),
    }
}

#[test]
fn composite_recursion_limit_rejects_self_reference() {
    // Glyph 0 references glyph 0: infinite loop.
    let mut body = build_header(-1, 0, 0, 1000, 1000);
    let flags: u16 = COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS;
    body.extend_from_slice(&flags.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes()); // self reference
    body.extend_from_slice(&0i16.to_be_bytes());
    body.extend_from_slice(&0i16.to_be_bytes());
    if body.len() % 2 != 0 {
        body.push(0);
    }
    let loca_bytes = build_loca_short(&[0, (body.len() as u16) / 2]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&body);
    let mut o = Outline::new();
    assert!(glyf.outline(&loca, 0, None, None, &mut o).is_err());
}
