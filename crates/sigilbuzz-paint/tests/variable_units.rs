//! Variation deltas land in the units of the field they patch.
//!
//! The item variation store holds deltas as raw integers in the
//! field's own encoding: design units for FWORD fields, 1/16384 for
//! F2DOT14 fields (alpha, scale, angles), and 1/65536 for the 16.16
//! Fixed fields of `VarAffine2x3`. These fixtures give every
//! transform-family `PaintVar*` a delta and check the evaluated
//! transform at the default instance and at the axis maximum.
//!
//! Sweep angles also carry the COLRv1 half-turn bias: a stored angle
//! `a` means `(a + 1) * pi` radians.

use core::f32::consts::PI;

use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate_with, DrawCmd, EvalOptions, GradientKind, PaintSource};

// =========================================================================
// Fixture builders
// =========================================================================

fn f2dot14(v: f32) -> [u8; 2] {
    ((v * 16384.0).round() as i16).to_be_bytes()
}

fn fixed(v: f32) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn offset24(v: usize) -> [u8; 3] {
    let v = v as u32;
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// SFNT directory holding exactly the given tables.
fn sfnt(tables: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let mut offset = 12 + 16 * tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    for (tag, body) in tables {
        out.extend_from_slice(*tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        offset += body.len();
    }
    for (_, body) in tables {
        out.extend_from_slice(body);
    }
    out
}

/// CPAL v0 with one palette holding opaque red.
fn cpal() -> Vec<u8> {
    let mut out = Vec::new();
    for v in [0u16, 1, 1, 1] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&14u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&[0, 0, 255, 255]);
    out
}

/// One-axis item variation store: a single region peaking at +1 and
/// one int16 delta per row.
fn ivs(rows: &[i16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&12u32.to_be_bytes()); // region list
    out.extend_from_slice(&1u16.to_be_bytes()); // one data subtable
    out.extend_from_slice(&22u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // axis count
    out.extend_from_slice(&1u16.to_be_bytes()); // region count
    out.extend_from_slice(&f2dot14(0.0));
    out.extend_from_slice(&f2dot14(1.0));
    out.extend_from_slice(&f2dot14(1.0));
    out.extend_from_slice(&(rows.len() as u16).to_be_bytes()); // item count
    out.extend_from_slice(&1u16.to_be_bytes()); // word delta count
    out.extend_from_slice(&1u16.to_be_bytes()); // region index count
    out.extend_from_slice(&0u16.to_be_bytes()); // region index
    for row in rows {
        out.extend_from_slice(&row.to_be_bytes());
    }
    out
}

/// COLRv1 in the header layout sigilbuzz reads: BaseGlyphList right
/// after the header, the variation store after the paints.
fn colr(paints: &[(u16, Vec<u8>)], var_store: &[u8]) -> Vec<u8> {
    let header_len: u32 = 30;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    let var_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(paints.len() as u32).to_be_bytes());
    let records = out.len();
    for (gid, _) in paints {
        out.extend_from_slice(&gid.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    for (i, (_, bytes)) in paints.iter().enumerate() {
        let rel = out.len() as u32 - header_len;
        let slot = records + i * 6 + 2;
        out[slot..slot + 4].copy_from_slice(&rel.to_be_bytes());
        out.extend_from_slice(bytes);
    }
    let off = out.len() as u32;
    out[var_slot..var_slot + 4].copy_from_slice(&off.to_be_bytes());
    out.extend_from_slice(var_store);
    out
}

fn solid() -> Vec<u8> {
    let mut p = vec![2u8];
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(&f2dot14(1.0));
    p
}

/// `head` (format byte plus fields) followed by its PaintSolid child,
/// with the Offset24 at byte 1 pointing at the child.
fn with_child(mut head: Vec<u8>) -> Vec<u8> {
    let child = head.len();
    head[1..4].copy_from_slice(&offset24(child));
    head.extend_from_slice(&solid());
    head
}

fn var_rotate(angle: f32, base: u32) -> Vec<u8> {
    let mut p = vec![25u8, 0, 0, 0];
    p.extend_from_slice(&f2dot14(angle));
    p.extend_from_slice(&base.to_be_bytes());
    with_child(p)
}

fn var_scale(sx: f32, sy: f32, base: u32) -> Vec<u8> {
    let mut p = vec![17u8, 0, 0, 0];
    p.extend_from_slice(&f2dot14(sx));
    p.extend_from_slice(&f2dot14(sy));
    p.extend_from_slice(&base.to_be_bytes());
    with_child(p)
}

fn var_skew(x: f32, y: f32, base: u32) -> Vec<u8> {
    let mut p = vec![29u8, 0, 0, 0];
    p.extend_from_slice(&f2dot14(x));
    p.extend_from_slice(&f2dot14(y));
    p.extend_from_slice(&base.to_be_bytes());
    with_child(p)
}

/// PaintVarTransform: paint offset, transform offset, then the
/// VarAffine2x3 (six Fixed fields plus varIndexBase), then the child.
fn var_transform(m: [f32; 6], base: u32) -> Vec<u8> {
    let mut p = vec![13u8];
    p.extend_from_slice(&offset24(7 + 28));
    p.extend_from_slice(&offset24(7));
    for v in m {
        p.extend_from_slice(&fixed(v));
    }
    p.extend_from_slice(&base.to_be_bytes());
    p.extend_from_slice(&solid());
    p
}

/// PaintVarSweepGradient centered on the origin with one red stop.
fn var_sweep(start: f32, end: f32, base: u32) -> Vec<u8> {
    let mut p = vec![9u8];
    p.extend_from_slice(&offset24(16));
    p.extend_from_slice(&0i16.to_be_bytes());
    p.extend_from_slice(&0i16.to_be_bytes());
    p.extend_from_slice(&f2dot14(start));
    p.extend_from_slice(&f2dot14(end));
    p.extend_from_slice(&base.to_be_bytes());
    p.push(0); // Pad
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&f2dot14(0.0));
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(&f2dot14(1.0));
    p.extend_from_slice(&u32::MAX.to_be_bytes());
    p
}

/// Glyph 1: VarRotate, angle 0 plus 0.5 (row 0).
/// Glyph 2: VarScale, (1, 1) plus (0.5, -0.25) (rows 1-2).
/// Glyph 3: VarTransform, identity plus xx 0.25 and dx -0.5 (rows 3-8).
/// Glyph 4: VarSweepGradient, start -1 plus 0.5, end 0 (rows 9-12).
/// Glyph 5: VarSkew, x 0 plus 0.25 (rows 13-14).
fn font_bytes() -> Vec<u8> {
    let rows = [
        8192, // rotate angle
        8192, -4096, // scale x, y
        16384, 0, 0, 0, -32768, 0, // affine xx, yx, xy, yy, dx, dy
        0, 0, 8192, 0, // sweep cx, cy, start, end
        4096, 0, // skew x, y
    ];
    let paints = [
        (1, var_rotate(0.0, 0)),
        (2, var_scale(1.0, 1.0, 1)),
        (3, var_transform([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 3)),
        (4, var_sweep(-1.0, 0.0, 9)),
        (5, var_skew(0.0, 0.0, 13)),
    ];
    let colr = colr(&paints, &ivs(&rows));
    let cpal = cpal();
    sfnt(&[(b"COLR", &colr), (b"CPAL", &cpal)])
}

fn one_fill(
    face: &Face<'_>,
    gid: u16,
    coords: &[f32],
) -> (sigilbuzz_paint::Transform2D, PaintSource) {
    let cmds = evaluate_with(face, gid, &EvalOptions::new().with_coords(coords));
    match cmds.as_slice() {
        [DrawCmd::FillGlyph {
            transform, paint, ..
        }] => (*transform, paint.clone()),
        other => panic!("expected one fill for glyph {gid}, got {other:?}"),
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

// =========================================================================
// Tests
// =========================================================================

#[test]
fn var_rotate_angle_delta_is_f2dot14() {
    let bytes = font_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let (t, _) = one_fill(&face, 1, &[0.0]);
    assert!(close(t.xx, 1.0) && close(t.yx, 0.0), "static: {t:?}");
    // 0.5 half-turns is a quarter turn.
    let (t, _) = one_fill(&face, 1, &[1.0]);
    assert!(close(t.xx, 0.0) && close(t.yx, 1.0), "at +1: {t:?}");
    assert!(close(t.xy, -1.0) && close(t.yy, 0.0), "at +1: {t:?}");
}

#[test]
fn var_scale_deltas_are_f2dot14() {
    let bytes = font_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let (t, _) = one_fill(&face, 2, &[1.0]);
    assert!(close(t.xx, 1.5), "sx {}", t.xx);
    assert!(close(t.yy, 0.75), "sy {}", t.yy);
    let (t, _) = one_fill(&face, 2, &[0.5]);
    assert!(close(t.xx, 1.25), "sx {}", t.xx);
    assert!(close(t.yy, 0.875), "sy {}", t.yy);
}

#[test]
fn var_affine_deltas_are_16_16_fixed() {
    let bytes = font_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let (t, _) = one_fill(&face, 3, &[]);
    assert_eq!(t, sigilbuzz_paint::Transform2D::IDENTITY);
    let (t, _) = one_fill(&face, 3, &[1.0]);
    assert!(close(t.xx, 1.25), "xx {}", t.xx);
    assert!(close(t.dx, -0.5), "dx {}", t.dx);
    assert!(close(t.yy, 1.0) && close(t.yx, 0.0) && close(t.xy, 0.0));
}

#[test]
fn var_sweep_angles_are_biased_f2dot14() {
    let bytes = font_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let sweep = |coords: &[f32]| match one_fill(&face, 4, coords).1 {
        PaintSource::Gradient(g) => match g.kind {
            GradientKind::Sweep {
                start_angle,
                end_angle,
                ..
            } => (start_angle, end_angle),
            other => panic!("expected a sweep, got {other:?}"),
        },
        other @ PaintSource::Solid { .. } => panic!("expected a gradient, got {other:?}"),
    };
    // Stored -1 is 0 radians; stored 0 is pi.
    assert_eq!(sweep(&[]), (0.0, PI));
    // A +0.5 delta on the start angle moves it a quarter turn.
    let (start, end) = sweep(&[1.0]);
    assert!(close(start, PI / 2.0), "start {start}");
    assert!(close(end, PI), "end {end}");
}

#[test]
fn var_skew_angle_delta_is_f2dot14() {
    let bytes = font_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let (t, _) = one_fill(&face, 5, &[0.0]);
    assert!(close(t.xy, 0.0), "static: {t:?}");
    // 0.25 half-turns is 45 degrees: the x skew puts -tan(45) in xy.
    let (t, _) = one_fill(&face, 5, &[1.0]);
    assert!(close(t.xy, -1.0), "at +1: {t:?}");
    assert!(close(t.yx, 0.0), "at +1: {t:?}");
}
