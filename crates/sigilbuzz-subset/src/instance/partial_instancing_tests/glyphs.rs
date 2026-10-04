//! Partial instancing of a hand-built two-axis TrueType font: the
//! gvar tuples on the pinned axis move into glyf and hmtx, the rest
//! merge by region, and the kept axis still draws what the source
//! draws at the pinned location.

use super::*;
use crate::sfnt;
use alloc::vec;
use sigilbuzz::tables::{Outline, PathOp};

fn be16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn bei16(out: &mut Vec<u8>, v: i16) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// A simple glyph with one contour through `points`, all on curve.
fn simple(points: &[(i16, i16)]) -> Vec<u8> {
    let mut b = Vec::new();
    bei16(&mut b, 1);
    let xs = points.iter().map(|p| p.0);
    let ys = points.iter().map(|p| p.1);
    for v in [xs.clone().min(), ys.clone().min(), xs.max(), ys.max()] {
        bei16(&mut b, v.unwrap_or(0));
    }
    be16(&mut b, points.len() as u16 - 1);
    be16(&mut b, 0); // no instructions
    b.extend(core::iter::repeat(0x01).take(points.len())); // on curve, word deltas
    let mut prev = 0;
    for p in points {
        bei16(&mut b, p.0 - prev);
        prev = p.0;
    }
    prev = 0;
    for p in points {
        bei16(&mut b, p.1 - prev);
        prev = p.1;
    }
    b
}

/// A composite glyph of `glyph` at `(x, y)`.
fn composite(glyph: u16, x: i16, y: i16) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [-1i16, 0, 0, 0, 0] {
        bei16(&mut b, v);
    }
    be16(&mut b, 0x0003); // ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES
    be16(&mut b, glyph);
    bei16(&mut b, x);
    bei16(&mut b, y);
    b
}

/// One tuple: its peak on the two axes, the points it lists (`None`
/// for every point), and its x and y deltas.
struct Tuple<'a>(f32, f32, Option<&'a [u16]>, &'a [i16], &'a [i16]);

/// A glyph's variation data, every tuple with an embedded peak and its
/// own (word-encoded) points and deltas.
fn glyph_variations(tuples: &[Tuple<'_>]) -> Vec<u8> {
    let mut headers = Vec::new();
    let mut data = Vec::new();
    for Tuple(a, b, points, xs, ys) in tuples {
        let mut t = Vec::new();
        match points {
            Some(points) => {
                t.push(points.len() as u8);
                // One run of word point numbers, each from the last.
                t.push(0x80 | (points.len() as u8 - 1));
                let mut prev = 0;
                for &p in *points {
                    be16(&mut t, p - prev);
                    prev = p;
                }
            }
            None => t.push(0),
        }
        for deltas in [xs, ys] {
            t.push(0x40 | (deltas.len() as u8 - 1));
            for &d in *deltas {
                bei16(&mut t, d);
            }
        }
        be16(&mut headers, t.len() as u16);
        be16(&mut headers, 0x8000 | 0x2000); // embedded peak, private points
        for peak in [a, b] {
            bei16(&mut headers, (peak * 16384.0) as i16);
        }
        data.extend(t);
    }
    let mut out = Vec::new();
    be16(&mut out, tuples.len() as u16);
    be16(&mut out, 4 + headers.len() as u16);
    out.extend(headers);
    out.extend(data);
    out
}

/// The test font: `wght` (100 to 900, default 400) and `wdth` (50 to
/// 200, default 100), and four glyphs:
///
/// 1. A box with three tuples: a sparse one on wght that also widens
///    the advance by 20, a full one on wdth, and a sparse one on both.
/// 2. Glyph 1 placed at (50, 0), moved 15 right by wght and (7, 3) by
///    wdth.
/// 3. A triangle whose wght tuple moves point 1 onto point 2's x, and
///    whose wdth tuple lists points 0, 2 and 3 only, so the instance
///    has to write the point the move shifted.
fn two_axis_font() -> Vec<u8> {
    let glyphs = [
        Vec::new(),
        simple(&[(0, 0), (0, 500), (400, 500), (400, 0)]),
        composite(1, 50, 0),
        simple(&[(0, 0), (50, 0), (100, 0), (50, 100)]),
    ];
    let variations = [
        Vec::new(),
        glyph_variations(&[
            Tuple(1.0, 0.0, Some(&[0, 2, 5]), &[10, 30, 20], &[0, 20, 0]),
            Tuple(
                0.0,
                1.0,
                None,
                &[5, 5, 15, 15, 0, 10, 0, 0],
                &[0, 10, 10, 0, 0, 0, 0, 0],
            ),
            Tuple(1.0, 1.0, Some(&[1]), &[-8], &[6]),
        ]),
        glyph_variations(&[
            Tuple(1.0, 0.0, None, &[15, 0, 20, 0, 0], &[0, 0, 0, 0, 0]),
            Tuple(0.0, 1.0, Some(&[0]), &[7], &[3]),
        ]),
        glyph_variations(&[
            Tuple(1.0, 0.0, Some(&[0, 1, 2, 3]), &[0, 50, 0, 0], &[0, 0, 0, 0]),
            Tuple(0.0, 1.0, Some(&[0, 2, 3]), &[0, 100, 0], &[0, 0, 0]),
        ]),
    ];
    let n = glyphs.len() as u16;

    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for g in &glyphs {
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
        glyf.extend_from_slice(g);
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
    }
    loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());

    let mut gvar = Vec::new();
    for v in [1u16, 0, 2, 0] {
        be16(&mut gvar, v); // version, axisCount, no shared tuples
    }
    let offsets_end = 20 + 4 * (usize::from(n) + 1);
    gvar.extend_from_slice(&(offsets_end as u32).to_be_bytes());
    be16(&mut gvar, n);
    be16(&mut gvar, 1); // long offsets
    gvar.extend_from_slice(&(offsets_end as u32).to_be_bytes());
    let mut at = 0u32;
    for v in &variations {
        gvar.extend_from_slice(&at.to_be_bytes());
        at += v.len() as u32;
    }
    gvar.extend_from_slice(&at.to_be_bytes());
    for v in &variations {
        gvar.extend_from_slice(v);
    }

    let mut fvar = Vec::new();
    for v in [1u16, 0, 16, 2, 2, 20, 0, 0] {
        be16(&mut fvar, v);
    }
    for (tag, min, default, max) in [(*b"wght", 100i32, 400, 900), (*b"wdth", 50, 100, 200)] {
        fvar.extend_from_slice(&tag);
        for v in [min, default, max] {
            fvar.extend_from_slice(&(v << 16).to_be_bytes());
        }
        be16(&mut fvar, 0);
        be16(&mut fvar, 256);
    }

    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    head[50..52].copy_from_slice(&1u16.to_be_bytes()); // long loca
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[34..36].copy_from_slice(&n.to_be_bytes());
    let mut maxp = 0x0000_5000u32.to_be_bytes().to_vec();
    be16(&mut maxp, n);
    let mut hmtx = Vec::new();
    for (advance, lsb) in [(500, 0), (500, 0), (600, 50), (300, 0)] {
        be16(&mut hmtx, advance);
        be16(&mut hmtx, lsb);
    }
    sfnt::build(
        0x0001_0000,
        &[
            (tag::HEAD, head),
            (tag::HHEA, hhea),
            (tag::MAXP, maxp),
            (tag::HMTX, hmtx),
            (tag::LOCA, loca),
            (tag::GLYF, glyf),
            (tag::FVAR, fvar),
            (tag::GVAR, gvar),
        ],
    )
}

/// The points of an outline, in order.
fn points(outline: Option<Outline>) -> Vec<(f32, f32)> {
    outline
        .map(|o| {
            o.ops()
                .iter()
                .filter_map(|op| match *op {
                    PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => Some((x, y)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The largest distance between the outlines of every glyph of `a` at
/// `a_coords` and of `b` at `b_coords`.
fn worst_deviation(a: &Face<'_>, a_coords: &[f32], b: &Face<'_>, b_coords: &[f32]) -> f32 {
    (0..4)
        .map(|gid| {
            let (pa, pb) = (
                points(a.glyph_outline_at_coords(gid, a_coords).unwrap()),
                points(b.glyph_outline_at_coords(gid, b_coords).unwrap()),
            );
            assert_eq!(pa.len(), pb.len(), "gid {gid}");
            pa.iter()
                .zip(&pb)
                .map(|(p, q)| (p.0 - q.0).abs().max((p.1 - q.1).abs()))
                .fold(0.0, f32::max)
        })
        .fold(0.0, f32::max)
}

/// Pins wght at the normalized `wght` and keeps wdth.
fn partial(face: &Face<'_>, wght: f32) -> Vec<u8> {
    let input = InstanceInput {
        coords: vec![wght, 0.0],
        drop_var_tables: true,
        axis_pins: vec![AxisPin::Pin, AxisPin::Keep],
    };
    instance(face, &input).expect("partial instance").bytes
}

#[test]
fn the_kept_axis_draws_what_the_source_draws_at_the_pin() {
    let font = two_axis_font();
    let face = Face::parse_bytes(&font, 0).unwrap();
    let out = partial(&face, 1.0);
    let inst = Face::parse_bytes(&out, 0).unwrap();
    // Every delta is whole at a pin of 1, so nothing rounds.
    for wdth in [0.0, 0.25, 0.5, 1.0] {
        let dev = worst_deviation(&face, &[1.0, wdth], &inst, &[wdth]);
        assert!(dev < 1e-3, "wdth {wdth}: off by {dev}");
    }
    // A pin between masters rounds the baked outline and the merged
    // tuples once each.
    let out = partial(&face, 0.5);
    let inst = Face::parse_bytes(&out, 0).unwrap();
    for wdth in [0.0, 0.5, 1.0] {
        let dev = worst_deviation(&face, &[0.5, wdth], &inst, &[wdth]);
        assert!(dev <= 1.0 + 1e-3, "wdth {wdth}: off by {dev}");
    }
}

#[test]
fn the_pinned_tuples_move_into_glyf_and_hmtx() {
    let font = two_axis_font();
    let face = Face::parse_bytes(&font, 0).unwrap();
    let inst_bytes = partial(&face, 1.0);
    let inst = Face::parse_bytes(&inst_bytes, 0).unwrap();
    // The default outlines are the source's at wght 900.
    let dev = worst_deviation(&face, &[1.0, 0.0], &inst, &[]);
    assert!(dev < 1e-3, "default off by {dev}");
    let hmtx = inst.hmtx().unwrap();
    assert_eq!(hmtx.advance(1), Some(520), "the wght tuple widens the box");
    assert_eq!(hmtx.advance(2), Some(620));
    // The composite's offset took its wght delta.
    let glyf = inst.table_bytes(tag::GLYF).unwrap();
    let (start, _) = inst.loca().unwrap().range(2).unwrap();
    let records = super::super::glyf::read_component_records(&glyf[start as usize..]).unwrap();
    assert_eq!(records[0].gvar_point(), (65, 0));
}

#[test]
fn tuples_left_on_one_region_merge() {
    let font = two_axis_font();
    let face = Face::parse_bytes(&font, 0).unwrap();
    let inst_bytes = partial(&face, 1.0);
    let inst = Face::parse_bytes(&inst_bytes, 0).unwrap();
    let gvar = inst.gvar().unwrap().expect("the kept axis still varies");
    assert_eq!(gvar.axis_count(), 1);
    let raw = inst.table_bytes(tag::GVAR).unwrap();
    // Every glyph keeps a single tuple on wdth: glyph 1's full and
    // sparse ones merged, the others had one each.
    let long = raw[15] & 1 != 0;
    let at = |i: usize| -> usize {
        if long {
            u32::from_be_bytes([
                raw[20 + 4 * i],
                raw[21 + 4 * i],
                raw[22 + 4 * i],
                raw[23 + 4 * i],
            ]) as usize
        } else {
            usize::from(u16::from_be_bytes([raw[20 + 2 * i], raw[21 + 2 * i]])) * 2
        }
    };
    let data = u32::from_be_bytes([raw[16], raw[17], raw[18], raw[19]]) as usize;
    let tuple_count = |gid: usize| {
        let start = data + at(gid);
        (at(gid + 1) > at(gid)).then(|| u16::from_be_bytes([raw[start], raw[start + 1]]) & 0x0FFF)
    };
    assert_eq!(
        (0..4).map(tuple_count).collect::<Vec<_>>(),
        vec![None, Some(1), Some(1), Some(1)]
    );
}

#[test]
fn a_sparse_tuple_the_move_would_shift_lists_every_point() {
    // Glyph 3's wdth tuple lists points 0, 2 and 3. Point 1 sits
    // halfway between 0 and 2 in the source, so it takes half of point
    // 2's delta; the wght fold moves it onto point 2's x, where a
    // sparse tuple would give it all of it.
    let font = two_axis_font();
    let face = Face::parse_bytes(&font, 0).unwrap();
    let inst_bytes = partial(&face, 1.0);
    let inst = Face::parse_bytes(&inst_bytes, 0).unwrap();
    let want = points(face.glyph_outline_at_coords(3, &[1.0, 1.0]).unwrap());
    let got = points(inst.glyph_outline_at_coords(3, &[1.0]).unwrap());
    assert_eq!(got, want);
    assert_eq!(got[1], (150.0, 0.0), "point 1: 50 + 50 (wght) + 50 (wdth)");
}
