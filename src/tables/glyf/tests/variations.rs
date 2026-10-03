//! Tests for [`Glyf::outline_at_coords`] and
//! [`Glyf::phantom_points_at_coords`]: gvar deltas on simple glyphs,
//! composite components, and phantom points.

use super::*;
use crate::tables::gvar::testing::{build_gvar, Tuple};
use crate::tables::Gvar;

/// The square every test glyph 1 is: (0, 0) to (100, 100).
fn square() -> Vec<u8> {
    build_simple_glyph(
        &[3],
        &[
            (0, 0, true),
            (100, 0, true),
            (100, 100, true),
            (0, 100, true),
        ],
    )
}

/// The tuple of glyph 1 at peak 1.0: point 2 moves by (20, 10) and
/// point 0 stays, so points 1 and 3 take inferred deltas and the
/// square grows to (120, 110). Phantom point 2 (index 5) moves by 30.
fn square_tuple() -> Tuple {
    Tuple {
        peak: vec![1.0],
        points: Some(vec![0, 2, 5]),
        deltas: vec![(0, 0), (20, 10), (30, 0)],
    }
}

/// One composite component: flags (words and `MORE_COMPONENTS` are
/// added), glyph, the two arguments, and an optional uniform scale.
struct Comp {
    flags: u16,
    glyph: u16,
    args: (i16, i16),
    scale: Option<f32>,
}

fn build_composite(comps: &[Comp]) -> Vec<u8> {
    let mut body = build_header(-1, 0, 0, 1000, 1000);
    for (i, c) in comps.iter().enumerate() {
        let mut flags = c.flags | COMP_ARG_1_AND_2_ARE_WORDS;
        if i + 1 < comps.len() {
            flags |= COMP_MORE_COMPONENTS;
        }
        if c.scale.is_some() {
            flags |= COMP_WE_HAVE_A_SCALE;
        }
        body.extend_from_slice(&flags.to_be_bytes());
        body.extend_from_slice(&c.glyph.to_be_bytes());
        body.extend_from_slice(&c.args.0.to_be_bytes());
        body.extend_from_slice(&c.args.1.to_be_bytes());
        if let Some(s) = c.scale {
            body.extend_from_slice(&((s * 16384.0) as i16).to_be_bytes());
        }
    }
    body
}

/// A font of glyph bodies: `glyf` and short `loca` bytes.
fn build_tables(glyphs: &[Vec<u8>]) -> (Vec<u8>, Vec<u8>) {
    let mut glyf = Vec::new();
    let mut offsets = vec![0u16];
    for g in glyphs {
        glyf.extend_from_slice(&pad_even(g.clone()));
        offsets.push((glyf.len() / 2) as u16);
    }
    (glyf, build_loca_short(&offsets))
}

/// Every point of an outline, in op order, skipping the closing line
/// back to each contour's start.
fn points(o: &Outline) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    let mut start = None;
    for op in o.ops() {
        match *op {
            PathOp::MoveTo { x, y } => {
                start = Some((x, y));
                out.push((x, y));
            }
            PathOp::LineTo { x, y } if Some((x, y)) != start => out.push((x, y)),
            PathOp::QuadTo { cx, cy, x, y } => out.extend([(cx, cy), (x, y)]),
            _ => {}
        }
    }
    out
}

fn assert_points(got: &[(f32, f32)], want: &[(f32, f32)]) {
    assert_eq!(got.len(), want.len(), "{got:?}");
    for (g, w) in got.iter().zip(want) {
        assert!(
            (g.0 - w.0).abs() < 1e-3 && (g.1 - w.1).abs() < 1e-3,
            "got {got:?}, want {want:?}"
        );
    }
}

/// Glyph 0 is `parent`; glyph 1 the square, varied by
/// [`square_tuple`]; `parent_tuples` vary glyph 0. Draws glyph 0 at
/// `coord`.
fn draw(parent: Vec<u8>, parent_tuples: Vec<Tuple>, coord: f32) -> Result<Vec<(f32, f32)>> {
    let (glyf_bytes, loca_bytes) = build_tables(&[parent, square()]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
    let glyf = Glyf::new(&glyf_bytes);
    let gvar_bytes = build_gvar(1, &[parent_tuples, vec![square_tuple()]]);
    let gvar = Gvar::parse(&gvar_bytes).unwrap();
    let mut o = Outline::new();
    glyf.outline_at_coords(&loca, 0, Some(&gvar), &[coord], None, &mut o)?;
    Ok(points(&o))
}

fn translated(points: &[(f32, f32)], dx: f32, dy: f32) -> Vec<(f32, f32)> {
    points.iter().map(|&(x, y)| (x + dx, y + dy)).collect()
}

const GROWN: [(f32, f32); 4] = [(0.0, 0.0), (120.0, 0.0), (120.0, 110.0), (0.0, 110.0)];
const HALF_GROWN: [(f32, f32); 4] = [(0.0, 0.0), (110.0, 0.0), (110.0, 105.0), (0.0, 105.0)];

#[test]
fn simple_glyph_points_take_inferred_deltas() {
    let (glyf_bytes, loca_bytes) = build_tables(&[square()]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 1).unwrap();
    let glyf = Glyf::new(&glyf_bytes);
    let gvar_bytes = build_gvar(1, &[vec![square_tuple()]]);
    let gvar = Gvar::parse(&gvar_bytes).unwrap();
    let mut o = Outline::new();
    assert!(glyf
        .outline_at_coords(&loca, 0, Some(&gvar), &[1.0], None, &mut o)
        .unwrap());
    assert_points(&points(&o), &GROWN);
    let mut o = Outline::new();
    glyf.outline_at_coords(&loca, 0, Some(&gvar), &[0.5], None, &mut o)
        .unwrap();
    assert_points(&points(&o), &HALF_GROWN);

    // The default instance and a missing gvar draw the static square.
    for (gvar, coords) in [(Some(&gvar), &[0.0][..]), (None, &[1.0][..])] {
        let mut o = Outline::new();
        glyf.outline_at_coords(&loca, 0, gvar, coords, None, &mut o)
            .unwrap();
        let mut plain = Outline::new();
        glyf.outline(&loca, 0, None, None, &mut plain).unwrap();
        assert_eq!(o.ops(), plain.ops());
    }
}

/// A one-component composite at offset (200, 300), whose component
/// moves by (10, -20) at peak 1.0.
fn moved_component(flags: u16, scale: Option<f32>, coord: f32) -> Vec<(f32, f32)> {
    let parent = build_composite(&[Comp {
        flags: COMP_ARGS_ARE_XY_VALUES | flags,
        glyph: 1,
        args: (200, 300),
        scale,
    }]);
    let tuple = Tuple {
        peak: vec![1.0],
        points: None,
        deltas: vec![(10, -20), (0, 0), (0, 0), (0, 0), (0, 0)],
    };
    draw(parent, vec![tuple], coord).unwrap()
}

#[test]
fn components_move_by_their_deltas_and_vary_themselves() {
    assert_points(
        &moved_component(0, None, 1.0),
        &translated(&GROWN, 210.0, 280.0),
    );
    assert_points(
        &moved_component(0, None, 0.5),
        &translated(&HALF_GROWN, 205.0, 290.0),
    );
}

#[test]
fn a_scaled_offset_scales_the_delta_too() {
    let half: Vec<(f32, f32)> = GROWN.iter().map(|&(x, y)| (x * 0.5, y * 0.5)).collect();
    // SCALED_COMPONENT_OFFSET: (200 + 10, 300 - 20) goes through the
    // half scale with the points.
    assert_points(
        &moved_component(COMP_SCALED_COMPONENT_OFFSET, Some(0.5), 1.0),
        &translated(&half, 105.0, 140.0),
    );
    // Unscaled, by default or when both flags are set.
    for flags in [
        0,
        COMP_UNSCALED_COMPONENT_OFFSET,
        COMP_SCALED_COMPONENT_OFFSET | COMP_UNSCALED_COMPONENT_OFFSET,
    ] {
        assert_points(
            &moved_component(flags, Some(0.5), 1.0),
            &translated(&half, 210.0, 280.0),
        );
    }
}

#[test]
fn an_anchored_component_follows_its_anchor_not_its_delta() {
    // The first square sits at its offset (0, 0) moved by (5, 5); the
    // second is anchored by its point 0 to the parent's point 2, the
    // first square's grown corner, and ignores its own delta.
    let parent = build_composite(&[
        Comp {
            flags: COMP_ARGS_ARE_XY_VALUES,
            glyph: 1,
            args: (0, 0),
            scale: None,
        },
        Comp {
            flags: 0,
            glyph: 1,
            args: (2, 0),
            scale: None,
        },
    ]);
    let tuple = Tuple {
        peak: vec![1.0],
        points: Some(vec![0, 1]),
        deltas: vec![(5, 5), (77, 77)],
    };
    let got = draw(parent, vec![tuple], 1.0).unwrap();
    let mut want = translated(&GROWN, 5.0, 5.0);
    want.extend(translated(&GROWN, 125.0, 115.0));
    assert_points(&got, &want);
}

#[test]
fn malformed_composite_deltas_fail_the_outline() {
    let parent = build_composite(&[Comp {
        flags: COMP_ARGS_ARE_XY_VALUES,
        glyph: 1,
        args: (0, 0),
        scale: None,
    }]);
    // All points, but deltas for two of the five.
    let tuple = Tuple {
        peak: vec![1.0],
        points: None,
        deltas: vec![(1, 1), (1, 1)],
    };
    assert!(matches!(
        draw(parent, vec![tuple], 1.0),
        Err(Error::Truncated { .. })
    ));
}

/// Glyph 0 is a composite of the square, with `flags` on the
/// component; its own tuple moves its advance point by -100. Returns
/// its phantom points at peak 1.0. hmtx: glyph 0 advances 600, glyph 1
/// 500, both with a zero left side bearing.
fn composite_phantoms(flags: u16) -> Result<[(f32, f32); 4]> {
    let parent = build_composite(&[Comp {
        flags: COMP_ARGS_ARE_XY_VALUES | flags,
        glyph: 1,
        args: (0, 0),
        scale: None,
    }]);
    let (glyf_bytes, loca_bytes) = build_tables(&[parent, square()]);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 2).unwrap();
    let glyf = Glyf::new(&glyf_bytes);
    let tuple = Tuple {
        peak: vec![1.0],
        points: Some(vec![2]),
        deltas: vec![(-100, 0)],
    };
    let gvar_bytes = build_gvar(1, &[vec![tuple], vec![square_tuple()]]);
    let gvar = Gvar::parse(&gvar_bytes).unwrap();
    let hmtx_bytes = build_hmtx(&[(600, 0), (500, 0)]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 2, 2).unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: None,
    };
    glyf.phantom_points_at_coords(&loca, 0, &metrics, Some(&gvar), &[1.0])
}

#[test]
fn phantom_points_move_by_their_deltas() {
    // The composite's own advance point: 600 - 100.
    let pp = composite_phantoms(0).unwrap();
    assert_points(&pp, &[(0.0, 0.0), (500.0, 0.0), (0.0, 0.0), (0.0, 0.0)]);
    // USE_MY_METRICS: the square's, 500 + 30.
    let pp = composite_phantoms(COMP_USE_MY_METRICS).unwrap();
    assert_points(&pp, &[(0.0, 0.0), (530.0, 0.0), (0.0, 0.0), (0.0, 0.0)]);
}

/// Composites whose `USE_MY_METRICS` components lead back to them:
/// `uses[g]` is the glyph composite `g` names. hmtx gives glyph `g` an
/// advance of 600 - 100 * g, and a tuple at peak 1.0 moves each glyph's
/// advance point by 10 * (g + 1). Returns glyph 0's phantom points at
/// `coord`.
fn cyclic_phantoms(uses: &[u16], coord: f32) -> Result<[(f32, f32); 4]> {
    let glyphs: Vec<Vec<u8>> = uses
        .iter()
        .map(|&glyph| {
            build_composite(&[Comp {
                flags: COMP_ARGS_ARE_XY_VALUES | COMP_USE_MY_METRICS,
                glyph,
                args: (0, 0),
                scale: None,
            }])
        })
        .collect();
    let n = uses.len() as u16;
    let (glyf_bytes, loca_bytes) = build_tables(&glyphs);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, n).unwrap();
    let glyf = Glyf::new(&glyf_bytes);
    // One component, so the advance point is point 2.
    let tuples: Vec<Vec<Tuple>> = (0..n)
        .map(|g| {
            vec![Tuple {
                peak: vec![1.0],
                points: Some(vec![2]),
                deltas: vec![(10 * (g as i16 + 1), 0)],
            }]
        })
        .collect();
    let gvar_bytes = build_gvar(1, &tuples);
    let gvar = Gvar::parse(&gvar_bytes).unwrap();
    let metrics: Vec<(u16, i16)> = (0..n).map(|g| (600 - 100 * g, 0)).collect();
    let hmtx_bytes = build_hmtx(&metrics);
    let hmtx = Hmtx::parse(&hmtx_bytes, n, n).unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: None,
    };
    glyf.phantom_points_at_coords(&loca, 0, &metrics, Some(&gvar), &[coord])
}

#[test]
fn phantom_points_skip_a_self_referencing_composite() {
    // HarfBuzz's decycler stops at the first repeat: glyph 0 inside
    // glyph 0 keeps its own points, 600 moved by 10.
    let pp = cyclic_phantoms(&[0], 1.0).unwrap();
    assert_points(&pp[..2], &[(0.0, 0.0), (610.0, 0.0)]);
    // At the default instance nothing moves.
    let pp = cyclic_phantoms(&[0], 0.0).unwrap();
    assert_points(&pp[..2], &[(0.0, 0.0), (600.0, 0.0)]);
}

#[test]
fn phantom_points_skip_a_longer_cycle_where_harfbuzz_does() {
    // The decycler's tortoise moves at half speed, so it sees a longer
    // cycle a few levels late, and the glyph where it does keeps its own
    // points. The advances match HarfBuzz 14.5.0 on the same glyphs.
    for (uses, advance) in [
        // 0, 1, 0, 1: glyph 1 at depth 3, 500 moved by 20.
        (&[1, 0][..], 520.0),
        // 0, 1, 2, 0, 1, 2: glyph 2 at depth 5, 400 moved by 30.
        (&[1, 2, 0][..], 430.0),
        // Glyph 3 at depth 7, 300 moved by 40.
        (&[1, 2, 3, 0][..], 340.0),
    ] {
        let pp = cyclic_phantoms(uses, 1.0).unwrap();
        assert_points(&pp[..2], &[(0.0, 0.0), (advance, 0.0)]);
    }
}

/// A composite tree over the square: glyph 0 is the square and glyph
/// `k` names glyph `k - 1` `fanout` times, with `flags` on every
/// component, up to the root, glyph `levels`. Every glyph has `tuples`
/// tuples at peak 1.0 over all of its points, with zero deltas.
fn composite_tree(levels: u16, fanout: u16, tuples: usize, flags: u16) -> [Vec<u8>; 3] {
    let mut glyphs = vec![square()];
    let mut variations = Vec::new();
    for level in 1..=levels {
        let comps: Vec<Comp> = (0..fanout)
            .map(|_| Comp {
                flags: COMP_ARGS_ARE_XY_VALUES | flags,
                glyph: level - 1,
                args: (0, 0),
                scale: None,
            })
            .collect();
        glyphs.push(build_composite(&comps));
    }
    for glyph in 0..=levels {
        let count = if glyph == 0 {
            8
        } else {
            usize::from(fanout) + 4
        };
        let tuple = || Tuple {
            peak: vec![1.0],
            points: None,
            deltas: vec![(0, 0); count],
        };
        variations.push((0..tuples).map(|_| tuple()).collect());
    }
    let (glyf, loca) = build_tables(&glyphs);
    [glyf, loca, build_gvar(1, &variations)]
}

#[test]
fn an_outline_walk_charges_every_visit_to_one_budget() {
    // Two levels of three components over the square, two tuples per
    // glyph. A tuple costs one unit for its header and one per point
    // (its own points plus the four phantom points): the root and the
    // three middle composites 2 + 2 * 7 each, the nine squares 2 + 2 * 8.
    let [glyf_bytes, loca_bytes, gvar_bytes] = composite_tree(2, 3, 2, 0);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 3).unwrap();
    let glyf = Glyf::new(&glyf_bytes);
    let gvar = Gvar::parse(&gvar_bytes).unwrap();
    let cx = FlattenCtx {
        loca: &loca,
        metrics: None,
        var: Variation::new(Some(&gvar), &[1.0]),
    };
    let mut budget = FlattenBudget::new();
    let mut flat = FlatGlyph::default();
    glyf.flatten(
        &cx,
        2,
        None,
        &Transform::identity(),
        &mut flat,
        0,
        &mut budget,
    )
    .unwrap();
    assert_eq!(flat.points.len(), 9 * 4);
    assert_eq!(MAX_TUPLE_WORK - budget.work, 4 * 16 + 9 * 18);
}

#[test]
fn a_composite_tree_shares_its_gvar_budget() {
    // 225 squares and 16 composites, 64 tuples each: 64 + 64 * 8 units
    // per square and 64 + 64 * 19 per composite, about 150,000 in all.
    // Each glyph's own tuples fit a budget of 100,000; the tree's do
    // not, whether drawn or walked for its USE_MY_METRICS phantoms.
    let [glyf_bytes, loca_bytes, gvar_bytes] = composite_tree(2, 15, 64, COMP_USE_MY_METRICS);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Short, 3).unwrap();
    let glyf = Glyf::new(&glyf_bytes);
    let gvar = Gvar::parse(&gvar_bytes).unwrap();
    for (glyph, points) in [(0, 4), (1, 15), (2, 15)] {
        let mut work = 100_000;
        assert!(gvar
            .phantom_deltas(glyph, &[1.0], points, &mut work)
            .is_ok());
    }
    let hmtx_bytes = build_hmtx(&[(600, 0), (600, 0), (600, 0)]);
    let hmtx = Hmtx::parse(&hmtx_bytes, 3, 3).unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: None,
    };
    let cx = FlattenCtx {
        loca: &loca,
        metrics: Some(&metrics),
        var: Variation::new(Some(&gvar), &[1.0]),
    };
    let small = || FlattenBudget {
        work: 100_000,
        ..FlattenBudget::new()
    };
    let assert_cap = |r: Result<()>| {
        assert!(
            matches!(
                r,
                Err(Error::Malformed {
                    context: "gvar variation work exceeds the cap",
                    ..
                })
            ),
            "{r:?}"
        );
    };
    let identity = Transform::identity();
    let mut flat = FlatGlyph::default();
    let drawn = glyf.flatten(&cx, 2, None, &identity, &mut flat, 0, &mut small());
    assert_cap(drawn.map(drop));
    let phantoms = glyf.varied_phantoms(&cx, &metrics, 2, 0, &mut small());
    assert_cap(phantoms.map(drop));
    // The full cap covers the tree.
    let mut flat = FlatGlyph::default();
    let mut budget = FlattenBudget::new();
    assert!(glyf
        .flatten(&cx, 2, None, &identity, &mut flat, 0, &mut budget)
        .is_ok());
}
