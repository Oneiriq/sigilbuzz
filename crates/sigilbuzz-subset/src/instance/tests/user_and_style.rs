//! Tests for the `OS/2` and `post` fields an instance sets from its
//! axis locations, and the metrics a CFF2 instance takes from its
//! outlines.

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

/// A full instance at normalized `coords`.
fn normalized(coords: &[f32]) -> InstanceInput {
    InstanceInput {
        coords: coords.to_vec(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
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
fn instance_sets_weight_class_and_average_width() {
    let face = rubik_face();
    let axis = face.fvar().unwrap().unwrap().axes()[0];
    let out = instance(&face, &normalized(&[axis.normalize(651.0)])).unwrap();
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
    let axis = face.fvar().unwrap().unwrap().axes()[0];
    let out = instance(&face, &normalized(&[axis.normalize(613.0)])).unwrap();
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
    let out = instance(&face, &normalized(&[1.0])).unwrap();
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
