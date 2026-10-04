//! Tests for the user-space entry point, the `OS/2` and `post` fields
//! an instance sets from its axis locations, and the metrics a CFF2
//! instance takes from its outlines.

use super::*;
use sigilbuzz::tables::OutlineSink;

/// An axis like most `wght` axes: 100 to 900, default 400.
fn weight_axis() -> VariationAxis {
    VariationAxis {
        tag: *b"wght",
        min_value: 100.0,
        default_value: 400.0,
        max_value: 900.0,
        flags: 0,
        axis_name_id: 256,
    }
}

fn user(axes: &[([u8; 4], AxisLimit)]) -> UserInstanceInput {
    UserInstanceInput {
        axes: axes.to_vec(),
        ..UserInstanceInput::default()
    }
}

/// Reads a big-endian `u16` at `off` of table `tag`.
fn u16_at(face: &Face<'_>, tag: [u8; 4], off: usize) -> u16 {
    let t = face.table_bytes(tag).unwrap();
    u16::from_be_bytes([t[off], t[off + 1]])
}

fn i16_at(face: &Face<'_>, tag: [u8; 4], off: usize) -> i16 {
    u16_at(face, tag, off) as i16
}

#[test]
fn limits_resolve_as_harfbuzz_takes_them() {
    let axis = weight_axis();
    let pin = |v: f32| Ok((Some(v), Some(v)));
    assert_eq!(resolve_limit(&axis, AxisLimit::Pin(650.0)), pin(650.0));
    assert_eq!(resolve_limit(&axis, AxisLimit::Pin(1200.0)), pin(900.0));
    assert_eq!(resolve_limit(&axis, AxisLimit::Pin(f32::NAN)), pin(400.0));
    assert_eq!(resolve_limit(&axis, AxisLimit::Default), pin(400.0));
    assert_eq!(resolve_limit(&axis, AxisLimit::Keep), Ok((None, None)));
    let range = |min, default, max| AxisLimit::Range { min, default, max };
    // The whole axis keeps it, its location the default.
    assert_eq!(
        resolve_limit(&axis, range(100.0, 400.0, 900.0)),
        Ok((None, Some(400.0)))
    );
    assert_eq!(
        resolve_limit(&axis, range(f32::NAN, f32::NAN, f32::NAN)),
        Ok((None, Some(400.0)))
    );
    assert_eq!(
        resolve_limit(&axis, range(0.0, 400.0, 2000.0)),
        Ok((None, Some(400.0)))
    );
    // One value pins, past either end after clamping too.
    assert_eq!(resolve_limit(&axis, range(500.0, 0.0, 500.0)), pin(500.0));
    assert_eq!(resolve_limit(&axis, range(1000.0, 0.0, 2000.0)), pin(900.0));
    // Narrowing, moving the default, or an inverted range fails.
    for limit in [
        range(200.0, 400.0, 900.0),
        range(100.0, 500.0, 900.0),
        range(800.0, 400.0, 300.0),
    ] {
        assert!(matches!(
            resolve_limit(&axis, limit),
            Err(SubsetError::Unsupported(_))
        ));
    }
}

#[test]
fn user_values_invert_normalize() {
    let axis = weight_axis();
    for v in [100.0, 250.0, 400.0, 650.0, 900.0] {
        let back = user_value(&axis, axis.normalize(v));
        assert!((back - v).abs() < 1e-3, "{v} -> {back}");
    }
    assert_eq!(user_value(&axis, f32::NAN), 400.0);
    assert_eq!(user_value(&axis, 7.0), 900.0);
}

#[test]
fn instance_user_matches_normalized_coords() {
    let face = rubik_face();
    let axis = face.fvar().unwrap().unwrap().axes()[0];
    let by_user = instance_user(&face, &user(&[(*b"wght", AxisLimit::Pin(700.0))])).unwrap();
    let by_coords = instance(
        &face,
        &InstanceInput {
            coords: alloc::vec![axis.normalize(700.0)],
            drop_var_tables: true,
            axis_pins: Vec::new(),
        },
    )
    .unwrap();
    assert_eq!(by_user.bytes, by_coords.bytes);
}

#[test]
fn instance_user_rejects_an_unknown_tag() {
    let face = rubik_face();
    let r = instance_user(&face, &user(&[(*b"wdth", AxisLimit::Pin(75.0))]));
    assert!(matches!(r, Err(SubsetError::Unsupported(_))));
}

#[test]
fn unnamed_axes_are_pinned_or_kept() {
    let face = rubik_face();
    let pinned = instance_user(&face, &UserInstanceInput::default()).unwrap();
    let pinned = Face::parse_bytes(&pinned.bytes, 0).unwrap();
    assert!(pinned.fvar().unwrap().is_none(), "a static instance");
    let kept = UserInstanceInput {
        keep_unnamed_axes: true,
        ..UserInstanceInput::default()
    };
    let kept = instance_user(&face, &kept).unwrap();
    let kept = Face::parse_bytes(&kept.bytes, 0).unwrap();
    assert!(kept.fvar().unwrap().is_some(), "the axis stays");
}

#[test]
fn instance_sets_weight_class_and_average_width() {
    let face = rubik_face();
    let out = instance_user(&face, &user(&[(*b"wght", AxisLimit::Pin(651.0))])).unwrap();
    let baked = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert_eq!(u16_at(&baked, *b"OS/2", 4), 651);
    let hmtx = baked.hmtx().unwrap();
    let advances: Vec<u32> = (0..baked.maxp().unwrap().num_glyphs)
        .map(|g| u32::from(hmtx.advance(g).unwrap_or(0)))
        .filter(|&a| a != 0)
        .collect();
    let mean = advances.iter().sum::<u32>() as f64 / advances.len() as f64;
    assert_eq!(i16_at(&baked, *b"OS/2", 2), mean.round() as i16);
    // The source's weight class differs, so the field moved.
    assert_eq!(u16_at(&face, *b"OS/2", 4), 300);
}

#[test]
fn a_kept_axis_keeps_its_weight_class() {
    let face = rubik_face();
    let input = user(&[(*b"wght", AxisLimit::Keep)]);
    let out = instance_user(&face, &input).unwrap();
    let baked = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert_eq!(u16_at(&baked, *b"OS/2", 4), 300);
    // A range over the whole axis keeps it too, but records its
    // default, as HarfBuzz does.
    let whole = AxisLimit::Range {
        min: 300.0,
        default: 300.0,
        max: 900.0,
    };
    let out = instance_user(&face, &user(&[(*b"wght", whole)])).unwrap();
    let baked = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert!(baked.fvar().unwrap().is_some());
    assert_eq!(u16_at(&baked, *b"OS/2", 4), 300);
}

/// Collects every point an outline draws to or pulls toward.
#[derive(Default)]
struct Points(Vec<(f32, f32)>);

impl OutlineSink for Points {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push((x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push((x, y));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.extend([(cx, cy), (x, y)]);
    }
    fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
        self.0.extend([(ax, ay), (bx, by), (x, y)]);
    }
    fn close(&mut self) {}
}

#[test]
fn a_cff2_instance_writes_whole_numbers() {
    // Every blend rounds, so every point of the static outlines falls
    // on a whole unit, as in HarfBuzz's instance.
    let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
    let out = instance_user(&face, &user(&[(*b"wght", AxisLimit::Pin(613.0))])).unwrap();
    let baked = Face::parse_bytes(&out.bytes, 0).unwrap();
    let cff2 = baked.cff2().unwrap();
    for gid in 0..baked.maxp().unwrap().num_glyphs {
        let mut points = Points::default();
        cff2.outline(gid, &[], &mut points).unwrap();
        for (x, y) in points.0 {
            assert!(
                x.fract() == 0.0 && y.fract() == 0.0,
                "gid {gid}: ({x}, {y})"
            );
        }
    }
}

#[test]
fn a_cff2_instance_takes_its_metrics_from_its_outlines() {
    // wght 900 is the axis' end, 1.0 before and after avar.
    let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
    let out = instance_user(&face, &user(&[(*b"wght", AxisLimit::Pin(900.0))])).unwrap();
    let baked = Face::parse_bytes(&out.bytes, 0).unwrap();
    let n = face.maxp().unwrap().num_glyphs;
    let extents = cff2_metrics::cff2_extents(&face, &[1.0], n).unwrap();
    let extents: Vec<_> = extents.into_iter().map(Option::unwrap).collect();
    let source_hmtx = face.hmtx().unwrap();
    let hmtx = baked.hmtx().unwrap();
    let mut boxed = 0;
    let mut union: Option<[i32; 4]> = None;
    for (gid, e) in (0..n).zip(&extents) {
        let lsb = i32::from(hmtx.lsb(gid).unwrap());
        if *e == cff2_metrics::Extents::default() {
            assert_eq!(lsb, i32::from(source_hmtx.lsb(gid).unwrap()), "gid {gid}");
            continue;
        }
        boxed += 1;
        assert_eq!(lsb, e.x_bearing, "gid {gid}");
        let b = [
            e.x_bearing,
            e.y_bearing + e.height,
            e.x_bearing + e.width,
            e.y_bearing,
        ];
        union = Some(match union {
            None => b,
            Some(u) => [
                u[0].min(b[0]),
                u[1].min(b[1]),
                u[2].max(b[2]),
                u[3].max(b[3]),
            ],
        });
    }
    assert!(boxed > 100);
    let union = union.unwrap();
    let head: Vec<i32> = (0..4)
        .map(|i| i32::from(i16_at(&baked, *b"head", 36 + 2 * i)))
        .collect();
    assert_eq!(head, union);
    // The weight class follows the pinned value.
    assert_eq!(u16_at(&baked, *b"OS/2", 4), 900);
}
