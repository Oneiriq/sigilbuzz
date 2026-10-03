//! The glyf bake against Rubik: every outline, composite offset and
//! metric of the instance against the variable font drawn at the same
//! coordinates.

use super::*;
use sigilbuzz::tables::glyf::PhantomMetrics;
use sigilbuzz::tables::{Outline, PathOp};

/// Rubik's normalized coordinates at `wght`, before avar.
fn rubik_coords(face: &Face<'_>, wght: f32) -> Vec<f32> {
    let fvar = face.fvar().unwrap().unwrap();
    let mut user: Vec<f32> = fvar.axes().iter().map(|a| a.default_value).collect();
    let i = fvar.axis_index(*b"wght").expect("rubik has wght");
    user[i] = wght;
    fvar.normalize_coords(&user)
}

/// The points an outline passes through, contour by contour. A closing
/// line back to a contour's start that lands within a unit of it is
/// left out: rounding can make a contour's last point meet its first,
/// which decides whether the drawer emits that line.
fn points(outline: &Outline) -> Vec<(f32, f32)> {
    let ops = outline.ops();
    let mut out = Vec::new();
    let mut start = (0.0, 0.0);
    for (i, op) in ops.iter().enumerate() {
        match *op {
            PathOp::MoveTo { x, y } => {
                start = (x, y);
                out.push((x, y));
            }
            PathOp::LineTo { x, y } => {
                let closes = matches!(ops.get(i + 1), Some(PathOp::Close));
                if !(closes && (x - start.0).abs() <= 1.0 && (y - start.1).abs() <= 1.0) {
                    out.push((x, y));
                }
            }
            PathOp::QuadTo { cx, cy, x, y } => out.extend([(cx, cy), (x, y)]),
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => out.extend([(c1x, c1y), (c2x, c2y), (x, y)]),
            PathOp::Close => {}
        }
    }
    out
}

/// Instances Rubik at `wght` and returns the instance with the
/// post-avar coordinates it was baked at.
fn rubik_instance(face: &Face<'_>, wght: f32) -> (Vec<u8>, Vec<f32>) {
    let coords = rubik_coords(face, wght);
    let input = InstanceInput {
        coords: coords.clone(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(face, &input).expect("bake");
    (out.bytes, post_avar(face, &coords).unwrap())
}

#[test]
fn every_outline_matches_the_variable_font_within_rounding() {
    let face = rubik_face();
    // 500 sits between masters, so the tuples' inferred deltas count.
    let (bytes, coords) = rubik_instance(&face, 500.0);
    let baked = Face::parse_bytes(&bytes, 0).unwrap();
    let (src_glyf, src_loca) = (face.glyf().unwrap(), face.loca().unwrap());
    let mut worst = [0.0_f32; 2];
    for gid in 0..face.maxp().unwrap().num_glyphs {
        let want = face.glyph_outline_at_coords(gid, &coords).unwrap();
        let got = baked.glyph_outline(gid).unwrap();
        let (want, got) = (
            want.as_ref().map(points).unwrap_or_default(),
            got.as_ref().map(points).unwrap_or_default(),
        );
        assert_eq!(want.len(), got.len(), "gid {gid} point count");
        let dev = want
            .iter()
            .zip(&got)
            .map(|(a, b)| (a.0 - b.0).abs().max((a.1 - b.1).abs()))
            .fold(0.0_f32, f32::max);
        let composite = src_glyf
            .bounds(&src_loca, gid)
            .unwrap()
            .is_some_and(|b| b.num_contours < 0);
        let slot = &mut worst[usize::from(composite)];
        *slot = slot.max(dev);
    }
    // A simple glyph's points round once; a composite's component
    // points and offset round on their own.
    assert!(worst[0] <= 0.5 + 1e-3, "simple glyphs off by {}", worst[0]);
    assert!(worst[1] <= 1.0 + 1e-3, "composites off by {}", worst[1]);
    assert!(worst[0] > 0.0, "the instance moved the outlines");
}

#[test]
fn composite_offsets_move_by_their_component_deltas() {
    let face = rubik_face();
    let (bytes, coords) = rubik_instance(&face, 500.0);
    let baked = Face::parse_bytes(&bytes, 0).unwrap();
    let gvar = face.gvar().unwrap().expect("rubik has gvar");
    let src = face.table_bytes(tag::GLYF).unwrap();
    let out = baked.table_bytes(tag::GLYF).unwrap();
    let (src_loca, out_loca) = (face.loca().unwrap(), baked.loca().unwrap());
    fn body<'g>(glyf: &'g [u8], loca: &sigilbuzz::tables::Loca<'_>, gid: u16) -> &'g [u8] {
        let (s, e) = loca.range(gid).unwrap();
        &glyf[s as usize..e as usize]
    }
    let mut moved = 0;
    for gid in 0..face.maxp().unwrap().num_glyphs {
        let src_body = body(src, &src_loca, gid);
        if src_body.len() < 10 || i16::from_be_bytes([src_body[0], src_body[1]]) >= 0 {
            continue;
        }
        let before = super::glyf::read_component_records(src_body).unwrap();
        let after = super::glyf::read_component_records(body(out, &out_loca, gid)).unwrap();
        let points: Vec<(i32, i32)> = before.iter().map(|c| c.gvar_point()).collect();
        let deltas = gvar.glyph_point_deltas(gid, &coords, &points, &[]).unwrap();
        for ((b, a), d) in before.iter().zip(&after).zip(&deltas) {
            let (x, y) = b.gvar_point();
            let want = (
                crate::util::round_half_up(x as f32 + d.0),
                crate::util::round_half_up(y as f32 + d.1),
            );
            if b.gvar_point() == (0, 0) && points.is_empty() {
                continue;
            }
            assert_eq!(a.gvar_point(), want, "gid {gid}");
            moved += usize::from(want != (x, y));
        }
    }
    assert!(moved > 0, "some component offset varies at wght 500");
}

#[test]
fn metrics_come_from_the_varied_phantom_points() {
    let face = rubik_face();
    let (bytes, coords) = rubik_instance(&face, 750.0);
    let baked = Face::parse_bytes(&bytes, 0).unwrap();
    let (glyf, loca, hmtx) = (
        face.glyf().unwrap(),
        face.loca().unwrap(),
        face.hmtx().unwrap(),
    );
    let gvar = face.gvar().unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: None,
    };
    let (baked_hmtx, baked_glyf, baked_loca) = (
        baked.hmtx().unwrap(),
        baked.glyf().unwrap(),
        baked.loca().unwrap(),
    );
    let mut checked = 0;
    for gid in 0..face.maxp().unwrap().num_glyphs {
        // A composite's own phantom points can differ from the
        // USE_MY_METRICS component's ones the core walk returns.
        if glyf
            .bounds(&loca, gid)
            .unwrap()
            .is_some_and(|b| b.num_contours < 0)
        {
            continue;
        }
        let pp = glyf
            .phantom_points_at_coords(&loca, gid, gvar.as_ref(), &coords, &metrics)
            .unwrap();
        let advance = crate::util::round_half_up(pp[1].0 - pp[0].0).max(0);
        assert_eq!(
            i32::from(baked_hmtx.advance(gid).unwrap()),
            advance,
            "gid {gid}"
        );
        let x_min = baked_glyf
            .bounds(&baked_loca, gid)
            .unwrap()
            .map_or(0, |b| b.x_min);
        let lsb = crate::util::round_half_up(f32::from(x_min) - pp[0].0);
        assert_eq!(i32::from(baked_hmtx.lsb(gid).unwrap()), lsb, "gid {gid}");
        checked += 1;
    }
    assert!(checked > 400, "{checked} simple glyphs");
}

#[test]
fn head_and_hhea_take_the_extremes_of_the_baked_glyphs() {
    let face = rubik_face();
    let (bytes, _) = rubik_instance(&face, 900.0);
    let baked = Face::parse_bytes(&bytes, 0).unwrap();
    let (glyf, loca, hmtx) = (
        baked.glyf().unwrap(),
        baked.loca().unwrap(),
        baked.hmtx().unwrap(),
    );
    let n = baked.maxp().unwrap().num_glyphs;
    let mut bbox = [i16::MAX, i16::MAX, i16::MIN, i16::MIN];
    let (mut max_advance, mut min_lsb, mut min_rsb, mut max_extent) =
        (0, i32::MAX, i32::MAX, i32::MIN);
    for gid in 0..n {
        let advance = hmtx.advance(gid).unwrap();
        max_advance = max_advance.max(advance);
        let Some(b) = glyf.bounds(&loca, gid).unwrap() else {
            continue;
        };
        bbox = [
            bbox[0].min(b.x_min),
            bbox[1].min(b.y_min),
            bbox[2].max(b.x_max),
            bbox[3].max(b.y_max),
        ];
        let lsb = i32::from(hmtx.lsb(gid).unwrap());
        let width = i32::from(b.x_max) - i32::from(b.x_min);
        min_lsb = min_lsb.min(lsb);
        min_rsb = min_rsb.min(i32::from(advance) - lsb - width);
        max_extent = max_extent.max(lsb + width);
    }
    let head_box = |face: &Face<'_>| {
        let head = face.table_bytes(tag::HEAD).unwrap();
        let at = |i: usize| i16::from_be_bytes([head[i], head[i + 1]]);
        [at(36), at(38), at(40), at(42)]
    };
    let head = head_box(&baked);
    assert_eq!(head, bbox);
    let hhea = baked.table_bytes(tag::HHEA).unwrap();
    let at = |i: usize| i16::from_be_bytes([hhea[i], hhea[i + 1]]);
    assert_eq!(u16::from_be_bytes([hhea[10], hhea[11]]), max_advance);
    assert_eq!(i32::from(at(12)), min_lsb);
    assert_eq!(i32::from(at(14)), min_rsb);
    assert_eq!(i32::from(at(16)), max_extent);
    // The black weight is wider than the light default.
    let src_head = head_box(&face);
    assert!(head[2] > src_head[2] || head[3] > src_head[3]);
}
