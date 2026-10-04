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

/// The points of glyph `gid`'s outline, `None` between contours.
fn outline_points(
    glyf: &Glyf<'_>,
    loca: &Loca<'_>,
    gid: u16,
    metrics: &PhantomMetrics<'_>,
) -> Vec<Option<(f32, f32)>> {
    let mut o = Outline::new();
    glyf.outline(loca, gid, None, Some(metrics), &mut o)
        .unwrap();
    o.ops()
        .iter()
        .map(|op| match *op {
            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => Some((x, y)),
            PathOp::Close => None,
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

#[test]
fn anchors_index_the_running_point_list_like_harfbuzz() {
    // HarfBuzz matches an anchored component's point `arg2` to point
    // `arg1` of the running point list of the whole walk: the points
    // placed so far, then the component's own points and its four
    // phantom points. An index past the points before the component
    // is not one of the composite's phantom points.
    //
    // Glyph 1 is a square (10, 0) to (40, 30), glyph 2 a triangle
    // (0, 0), (40, 0), (0, 40) advancing 100 units.
    //
    // - Glyph 3 places the square, then the triangle with arg1 = 5:
    //   the triangle's own point 1, (40, 0), so it moves by (40, 0).
    // - Glyph 4 does the same with arg1 = 8: the triangle's advance
    //   phantom point, (100, 0).
    // - Glyph 5 places the square, then glyph 6 at (5, 7), which
    //   places the triangle with arg1 = 1: point 1 of the walk, the
    //   square's (40, 0), though glyph 6 has no points before the
    //   triangle.
    //
    // HarfBuzz 14.5.0 draws the same glyphs (built byte for byte in
    // Python) with these points, after shifting each outline left by
    // its first phantom point, which sigilbuzz does not do.
    let square = build_simple_glyph(
        &[3],
        &[(10, 0, true), (40, 0, true), (40, 30, true), (10, 30, true)],
    );
    let triangle = build_simple_glyph(&[2], &[(0, 0, true), (40, 0, true), (0, 40, true)]);
    let xy = COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS;
    let placed = |flags: u16, gid: u16, x: i16, y: i16| {
        let mut c = flags.to_be_bytes().to_vec();
        c.extend_from_slice(&gid.to_be_bytes());
        c.extend_from_slice(&x.to_be_bytes());
        c.extend_from_slice(&y.to_be_bytes());
        c
    };
    let anchored = |gid: u16, to: u8, from: u8| {
        let mut c = 0u16.to_be_bytes().to_vec();
        c.extend_from_slice(&gid.to_be_bytes());
        c.extend_from_slice(&[to, from]);
        c
    };
    let composite = |bbox: [i16; 4], parts: &[Vec<u8>]| {
        let mut g = build_header(-1, bbox[0], bbox[1], bbox[2], bbox[3]);
        for part in parts {
            g.extend_from_slice(part);
        }
        g
    };
    let glyphs = [
        Vec::new(),
        square,
        triangle,
        composite(
            [10, 0, 40, 30],
            &[
                placed(xy | COMP_MORE_COMPONENTS, 1, 0, 0),
                anchored(2, 5, 0),
            ],
        ),
        composite(
            [10, 0, 40, 30],
            &[
                placed(xy | COMP_MORE_COMPONENTS, 1, 0, 0),
                anchored(2, 8, 0),
            ],
        ),
        composite(
            [0, 0, 100, 100],
            &[
                placed(xy | COMP_MORE_COMPONENTS, 1, 0, 0),
                placed(xy, 6, 5, 7),
            ],
        ),
        composite([0, 0, 100, 100], &[anchored(2, 1, 0)]),
    ];
    let mut glyf_bytes = Vec::new();
    let mut offsets = Vec::new();
    for g in &glyphs {
        offsets.push((glyf_bytes.len() / 2) as u16);
        glyf_bytes.extend_from_slice(g);
        if glyf_bytes.len() % 2 != 0 {
            glyf_bytes.push(0);
        }
    }
    offsets.push((glyf_bytes.len() / 2) as u16);
    let loca_bytes = build_loca_short(&offsets);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 7).unwrap();
    let glyf = Glyf::new(&glyf_bytes);
    let hmtx_bytes = build_hmtx(&[
        (300, 4),
        (60, 4),
        (100, 0),
        (300, 4),
        (300, 4),
        (300, 0),
        (300, 0),
    ]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 7, 7).unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: None,
    };
    let square_at = |dx: f32, dy: f32| {
        [
            (10.0, 0.0),
            (40.0, 0.0),
            (40.0, 30.0),
            (10.0, 30.0),
            (10.0, 0.0),
        ]
        .map(|(x, y)| Some((x + dx, y + dy)))
    };
    let triangle_at = |dx: f32, dy: f32| {
        [(0.0, 0.0), (40.0, 0.0), (0.0, 40.0), (0.0, 0.0)].map(|(x, y)| Some((x + dx, y + dy)))
    };
    for (gid, (dx, dy)) in [(3, (40.0, 0.0)), (4, (100.0, 0.0)), (5, (45.0, 7.0))] {
        let mut want = square_at(0.0, 0.0).to_vec();
        want.push(None);
        want.extend(triangle_at(dx, dy));
        want.push(None);
        assert_eq!(
            outline_points(&glyf, &loca, gid, &metrics),
            want,
            "glyph {gid}"
        );
    }
}
#[test]
fn composite_anchor_phantom_without_metrics_falls_back_to_zero() {
    // Glyph 0 places a square, then anchors glyph 2 (one point) with
    // arg1 = 5: past the square's 4 points and glyph 2's own point, so
    // glyph 2's first phantom point in the running list (see
    // `anchors_index_the_running_point_list_like_harfbuzz`). With
    // `metrics=None` no phantom point is known, so the anchor is
    // skipped and the component stays at its (0, 0) offset. Pin the
    // behavior so callers that opt out of phantom resolution still get
    // a stable answer.
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
fn a_component_that_closes_a_cycle_is_skipped_like_harfbuzz() {
    // Glyph 0 references glyph 0. HarfBuzz's decycler lets the first
    // visit through and skips the second, so the glyph draws nothing
    // rather than recursing until the depth cap.
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
    assert!(glyf.outline(&loca, 0, None, None, &mut o).unwrap());
    assert!(o.ops().is_empty());
}

/// A chain of `levels` composites, glyph `i` placing glyph `i + 1` as
/// its only component, flagged `USE_MY_METRICS` so the phantom walk
/// follows it too, and a square as glyph `levels`. Returns the `glyf`
/// and short `loca` bytes and the byte offset of every glyph.
fn composite_chain(levels: u16) -> (Vec<u8>, Vec<u8>, Vec<usize>) {
    let mut glyf_bytes = Vec::new();
    let mut offsets = Vec::new();
    let flags = COMP_ARGS_ARE_XY_VALUES | COMP_ARG_1_AND_2_ARE_WORDS | COMP_USE_MY_METRICS;
    for gid in 0..=levels {
        offsets.push(glyf_bytes.len());
        if gid < levels {
            glyf_bytes.extend_from_slice(&build_header(-1, 0, 0, 100, 100));
            glyf_bytes.extend_from_slice(&flags.to_be_bytes());
            glyf_bytes.extend_from_slice(&(gid + 1).to_be_bytes());
            glyf_bytes.extend_from_slice(&1i16.to_be_bytes());
            glyf_bytes.extend_from_slice(&0i16.to_be_bytes());
        } else {
            glyf_bytes.extend_from_slice(&pad_even(build_simple_glyph(
                &[3],
                &[
                    (0, 0, true),
                    (100, 0, true),
                    (100, 100, true),
                    (0, 100, true),
                ],
            )));
        }
    }
    let mut loca_words: Vec<u16> = offsets.iter().map(|&o| (o / 2) as u16).collect();
    loca_words.push((glyf_bytes.len() / 2) as u16);
    (glyf_bytes, build_loca_short(&loca_words), offsets)
}

#[test]
fn composites_nested_past_the_depth_cap_fail_at_the_deepest_glyph() {
    // HarfBuzz stops a glyf walk deeper than 64 composites
    // (HB_MAX_NESTING_LEVEL). An acyclic chain never meets the cycle
    // check, so only the depth cap ends it. The root sits at depth 0,
    // so a chain of 64 composites reaches the square at depth 64 and
    // draws; one more level puts glyph 65 past the cap, and the error
    // names that glyph's offset in `glyf`, in the outline walk and in
    // the phantom walk alike.
    for (levels, fails) in [(64u16, false), (65, true), (80, true)] {
        let (glyf_bytes, loca_bytes, offsets) = composite_chain(levels);
        let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, levels + 1).unwrap();
        let glyf = Glyf::new(&glyf_bytes);
        let hmtx_bytes = build_hmtx(&vec![(120, 0); usize::from(levels) + 1]);
        let hmtx = Hmtx::parse(&hmtx_bytes, levels + 1, levels + 1).unwrap();
        let metrics = PhantomMetrics {
            hmtx: &hmtx,
            vmtx: None,
        };
        let mut o = Outline::new();
        let outline = glyf.outline(&loca, 0, None, Some(&metrics), &mut o);
        let phantoms = glyf.phantom_points_at_coords(&loca, 0, None, &[], &metrics);
        if !fails {
            assert!(outline.unwrap(), "{levels} levels");
            // 64 components, each moved one unit right.
            assert_eq!(o.ops()[0], PathOp::MoveTo { x: 64.0, y: 0.0 });
            assert_eq!(phantoms.unwrap()[1], (120.0, 0.0));
            continue;
        }
        let want = Error::Malformed {
            offset: offsets[65],
            context: "glyf composite recursion exceeded cap",
        };
        assert_eq!(outline.unwrap_err(), want, "{levels} levels");
        assert_eq!(phantoms.unwrap_err(), want, "{levels} levels");
    }
}
