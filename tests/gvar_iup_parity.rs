//! Variable `glyf` outlines, extents, and advances against HarfBuzz.
//!
//! `tests/fixtures/hahmlet_gvar_subset.ttf` is a subset of Hahmlet (OFL)
//! whose `gvar` exercises what a renderer has to get right:
//!
//! - simple glyphs (`O`, the Hangul letter and syllables) whose tuples list
//!   only some points, so the rest take inferred deltas;
//! - composite glyphs (`Á`, `Å`) whose components move by their own deltas,
//!   one of them through a sparse tuple, and that take their metrics from a
//!   `USE_MY_METRICS` component;
//! - a space whose advance moves through its phantom points.
//!
//! `tests/fixtures/hahmlet_gvar_subset.expected` holds HarfBuzz 14.5.0's
//! outline points, extents, and advances for every glyph at five weights.
//! `tests/tools/gvar_iup_expected.py` regenerates it. The advances are
//! checked twice: as the font ships, and with `HVAR` hidden, where both
//! HarfBuzz and sigilbuzz take a varied advance from the glyph's varied
//! phantom points instead.

use sigilbuzz::tables::glyf::PhantomMetrics;
use sigilbuzz::tables::PathOp;
use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const FONT: &[u8] = include_bytes!("fixtures/hahmlet_gvar_subset.ttf");
const EXPECTED: &str = include_str!("fixtures/hahmlet_gvar_subset.expected");

/// Characters the subset maps, with the glyph each one shapes to alone.
const MAPPED: &[(char, u16)] = &[
    (' ', 9),
    ('A', 1),
    ('O', 4),
    ('\u{C1}', 2),
    ('\u{C5}', 3),
    ('\u{3143}', 5),
    ('\u{BE60}', 6),
    ('\u{BE75}', 7),
    ('\u{BED0}', 8),
];

/// One record of the expected file.
enum Record {
    Outline(f32, u16, Vec<(f32, f32)>),
    Extents(f32, u16, [i32; 4]),
    Advance(f32, u16, i32, i32),
}

fn records() -> Vec<Record> {
    let mut out = Vec::new();
    for line in EXPECTED.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut fields = line.split(' ');
        let kind = fields.next().unwrap();
        let wght: f32 = fields.next().unwrap().parse().unwrap();
        let gid: u16 = fields.next().unwrap().parse().unwrap();
        let rest: Vec<&str> = fields.collect();
        out.push(match kind {
            "outline" => {
                let v: Vec<f32> = rest.iter().map(|s| s.parse().unwrap()).collect();
                Record::Outline(wght, gid, v.chunks(2).map(|p| (p[0], p[1])).collect())
            }
            "extents" => {
                let v: Vec<i32> = rest.iter().map(|s| s.parse().unwrap()).collect();
                Record::Extents(wght, gid, [v[0], v[1], v[2], v[3]])
            }
            "advance" => Record::Advance(
                wght,
                gid,
                rest[0].parse().unwrap(),
                rest[1].parse().unwrap(),
            ),
            other => panic!("unknown record {other}"),
        });
    }
    out
}

/// The font's normalized coords at `wght`, through `fvar` and `avar`.
fn coords(face: &Face<'_>, wght: f32) -> Vec<f32> {
    let fvar = face.fvar().unwrap().unwrap();
    let normalized = fvar.normalize_coords(&[wght]);
    face.avar().unwrap().unwrap().remap_all(&normalized)
}

fn outline_points(face: &Face<'_>, gid: u16, coords: &[f32]) -> Vec<(f32, f32)> {
    let Some(outline) = face.glyph_outline_at_coords(gid, coords).unwrap() else {
        return Vec::new();
    };
    let mut points = Vec::new();
    for op in outline.ops() {
        match *op {
            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => points.push((x, y)),
            PathOp::QuadTo { cx, cy, x, y } => points.extend([(cx, cy), (x, y)]),
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => points.extend([(c1x, c1y), (c2x, c2y), (x, y)]),
            PathOp::Close => {}
        }
    }
    points
}

/// Largest distance from a point of one set to the nearest point of the
/// other. HarfBuzz and sigilbuzz start some contours at different points,
/// so the sequences differ while the sets agree.
fn distance(a: &[(f32, f32)], b: &[(f32, f32)]) -> f32 {
    let one_way = |p: &[(f32, f32)], q: &[(f32, f32)]| {
        p.iter()
            .map(|&(x, y)| {
                q.iter()
                    .map(|&(u, v)| (x - u).abs().max((y - v).abs()))
                    .fold(f32::INFINITY, f32::min)
            })
            .fold(0.0_f32, f32::max)
    };
    if a.is_empty() || b.is_empty() {
        return if a.is_empty() && b.is_empty() {
            0.0
        } else {
            f32::INFINITY
        };
    }
    one_way(a, b).max(one_way(b, a))
}

/// The fixture with its `HVAR` record renamed, as the generator does.
fn without_hvar() -> Vec<u8> {
    let mut bytes = FONT.to_vec();
    let num_tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    for i in 0..num_tables {
        let rec = 12 + 16 * i;
        if &bytes[rec..rec + 4] == b"HVAR" {
            bytes[rec..rec + 4].copy_from_slice(b"HVAX");
        }
    }
    bytes
}

#[test]
fn outlines_match_harfbuzz_at_every_weight() {
    let blob = Blob::new(FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let mut checked = 0;
    for record in records() {
        let Record::Outline(wght, gid, expected) = record else {
            continue;
        };
        let ours = outline_points(&face, gid, &coords(&face, wght));
        let d = distance(&ours, &expected);
        assert!(d <= 0.05, "glyph {gid} at wght {wght}: off by {d}");
        checked += 1;
    }
    assert_eq!(checked, 60);
}

#[test]
fn extents_match_harfbuzz_at_every_weight() {
    let blob = Blob::new(FONT);
    let face = Face::parse(&blob, 0).unwrap();
    for record in records() {
        let Record::Extents(wght, gid, expected) = record else {
            continue;
        };
        let got = face
            .glyph_bounds_at_coords(gid, &coords(&face, wght))
            .unwrap()
            .map_or([0; 4], |b| {
                let (x_min, y_min) = (i32::from(b.x_min), i32::from(b.y_min));
                let (x_max, y_max) = (i32::from(b.x_max), i32::from(b.y_max));
                [x_min, y_max, x_max - x_min, y_min - y_max]
            });
        assert_eq!(got, expected, "glyph {gid} at wght {wght}");
    }
}

#[test]
fn phantom_points_give_harfbuzz_advances_without_hvar() {
    let blob = Blob::new(FONT);
    let face = Face::parse(&blob, 0).unwrap();
    let (loca, glyf, gvar) = (
        face.loca().unwrap(),
        face.glyf().unwrap(),
        face.gvar().unwrap(),
    );
    let hmtx = face.hmtx().unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: None,
    };
    for record in records() {
        let Record::Advance(wght, gid, _, expected) = record else {
            continue;
        };
        let pp = glyf
            .phantom_points_at_coords(&loca, gid, &metrics, gvar.as_ref(), &coords(&face, wght))
            .unwrap();
        let advance = (pp[1].0 - pp[0].0).round().max(0.0) as i32;
        assert_eq!(advance, expected, "glyph {gid} at wght {wght}");
    }
}

#[test]
fn shaped_advances_match_harfbuzz_with_and_without_hvar() {
    let hidden = without_hvar();
    for (bytes, with_hvar) in [(FONT, true), (hidden.as_slice(), false)] {
        let blob = Blob::new(bytes);
        let face = Face::parse(&blob, 0).unwrap();
        assert_eq!(face.hvar().unwrap().is_some(), with_hvar);
        for record in records() {
            let Record::Advance(wght, gid, hvar_advance, phantom_advance) = record else {
                continue;
            };
            let Some(&(ch, _)) = MAPPED.iter().find(|&&(_, g)| g == gid) else {
                continue;
            };
            let coords = coords(&face, wght);
            let font = Font::new(face.clone(), 1000.0).with_coords(&coords);
            let mut buffer = Buffer::new();
            buffer.push_str(&ch.to_string());
            let run = shape(&font, &buffer, &[]).unwrap();
            assert_eq!(run.glyphs.len(), 1);
            assert_eq!(run.glyphs[0].glyph_id, u32::from(gid));
            let expected = if with_hvar {
                hvar_advance
            } else {
                phantom_advance
            };
            assert_eq!(
                run.glyphs[0].x_advance, expected,
                "{ch:?} at wght {wght}, HVAR {with_hvar}"
            );
        }
    }
}
