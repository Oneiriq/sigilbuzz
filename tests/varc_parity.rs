//! VARC outlines against HarfBuzz.
//!
//! `tests/fixtures/varc_parity.ttf` has three axes and 14 VARC glyphs
//! over two `gvar`-varied glyphs. Between them they use every component
//! record field: transforms (ScaleY defaulting to ScaleX, skew and a
//! transformation center), axis values with deltas over several regions,
//! transform deltas, an axis-values index without axes, a nested
//! composite, conditions of all five formats, reserved flag bits,
//! RESET_UNSPECIFIED_AXES at the top and one level down, and an axis
//! index past HarfBuzz's 4096-axis limit.
//!
//! `tests/fixtures/varc_face.ttf` has the same axes and 48 glyphs the
//! VARC coverage names, for what the face's walk over nested composites
//! does: reset components two and three composites down and in the
//! middle of a chain, components that name their own glyph, cycles of
//! two and three glyphs, a covered glyph without a record, delta sets
//! that end early or cut a run short, conditions inside nested
//! composites, one glyph reached many times at the same coords, a region
//! without axes, which applies at the default instance too, and
//! rotations past a quarter turn about a far center.
//!
//! Each `.expected` file holds HarfBuzz 14.5.0's outline of each glyph
//! at eight locations, which every glyph must match through
//! `Face::glyph_outline_at_coords`, `GlyphOutlines::outline` and
//! `GlyphOutlines::draw`. `tests/tools/build_varc_morx_parity_fixtures.py`
//! and `tests/tools/build_varc_face_fixtures.py` build the fonts, and
//! `tests/tools/varc_morx_parity_expected.py` and
//! `tests/tools/varc_face_expected.py` write the expected files.

use sigilbuzz::tables::{Outline, OutlineSink, PathOp};
use sigilbuzz::{Blob, Face};

const FONT: &[u8] = include_bytes!("fixtures/varc_parity.ttf");
const EXPECTED: &str = include_str!("fixtures/varc_parity.expected");
const FACE_FONT: &[u8] = include_bytes!("fixtures/varc_face.ttf");
const FACE_EXPECTED: &str = include_str!("fixtures/varc_face.expected");

type Ops = Vec<(char, Vec<f32>)>;

fn parse_ops(text: &str) -> Ops {
    let mut tokens = text.split(' ').filter(|t| !t.is_empty());
    let mut out = Vec::new();
    while let Some(op) = tokens.next() {
        let op = op.chars().next().unwrap();
        let n = match op {
            'M' | 'L' => 2,
            'Q' => 4,
            'C' => 6,
            _ => 0,
        };
        let vals = (0..n)
            .map(|_| tokens.next().unwrap().parse().unwrap())
            .collect();
        out.push((op, vals));
    }
    out
}

/// sigilbuzz's ops in the expected file's form: a closing line back to
/// the contour's start is left to `Z`.
fn ops_of(path: &[PathOp]) -> Ops {
    let mut out: Ops = Vec::new();
    let mut start = (0.0, 0.0);
    for op in path {
        match *op {
            PathOp::MoveTo { x, y } => {
                start = (x, y);
                out.push(('M', vec![x, y]));
            }
            PathOp::LineTo { x, y } => out.push(('L', vec![x, y])),
            PathOp::QuadTo { cx, cy, x, y } => out.push(('Q', vec![cx, cy, x, y])),
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => out.push(('C', vec![c1x, c1y, c2x, c2y, x, y])),
            PathOp::Close => {
                if out.last() == Some(&('L', vec![start.0, start.1])) {
                    out.pop();
                }
                out.push(('Z', Vec::new()));
            }
        }
    }
    out
}

/// Normalized coords at `location` (`default` or `TAG=v,...`), rounded
/// to F2DOT14 as HarfBuzz and `Face` keep them.
fn coords_at(face: &Face<'_>, location: &str) -> Vec<f32> {
    let fvar = face.fvar().unwrap().unwrap();
    let mut user: Vec<f32> = fvar.axes().iter().map(|a| a.default_value).collect();
    if location != "default" {
        for kv in location.split(',') {
            let (tag, v) = kv.split_once('=').unwrap();
            let i = fvar
                .axes()
                .iter()
                .position(|a| a.tag == tag.as_bytes())
                .unwrap();
            user[i] = v.parse().unwrap();
        }
    }
    let coords: Vec<f32> = fvar
        .normalize_coords(&user)
        .iter()
        .map(|c| (c * 16384.0 + 0.5).floor() / 16384.0)
        .collect();
    if coords.iter().all(|&c| c == 0.0) {
        Vec::new()
    } else {
        coords
    }
}

fn mul(p: [f32; 6], c: [f32; 6]) -> [f32; 6] {
    [
        p[0] * c[0] + p[1] * c[2],
        p[0] * c[1] + p[1] * c[3],
        p[2] * c[0] + p[3] * c[2],
        p[2] * c[1] + p[3] * c[3],
        p[0] * c[4] + p[1] * c[5] + p[4],
        p[2] * c[4] + p[3] * c[5] + p[5],
    ]
}

/// Draws `gid` the way HarfBuzz's VARC walk does, passing the font's
/// coords down so nested RESET_UNSPECIFIED_AXES components start from
/// them. Leaves come from `Face::glyph_outline_at_coords`.
fn flatten(face: &Face<'_>, gid: u16, coords: &[f32], font: &[f32], t: [f32; 6], out: &mut Ops) {
    let varc = face.varc().unwrap().unwrap();
    if varc.covers(gid) {
        let composite = varc.composite_with_font_coords(gid, coords, font).unwrap();
        for c in &composite.components {
            flatten(face, c.gid, &c.coords, font, mul(t, c.transform), out);
        }
        return;
    }
    let Some(outline) = face.glyph_outline_at_coords(gid, coords).unwrap() else {
        return;
    };
    for (op, vals) in ops_of(outline.ops()) {
        let mut mapped = Vec::with_capacity(vals.len());
        for p in vals.chunks(2) {
            mapped.push(t[0] * p[0] + t[1] * p[1] + t[4]);
            mapped.push(t[2] * p[0] + t[3] * p[1] + t[5]);
        }
        out.push((op, mapped));
    }
}

/// An [`OutlineSink`] that records what `GlyphOutlines::draw` sends it.
#[derive(Default)]
struct Recorder(Outline);

impl OutlineSink for Recorder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(PathOp::MoveTo { x, y });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(PathOp::LineTo { x, y });
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.push(PathOp::QuadTo { cx, cy, x, y });
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.0.push(PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        });
    }
    fn close(&mut self) {
        self.0.push(PathOp::Close);
    }
}

#[track_caller]
fn assert_close(got: &Ops, want: &Ops, what: &str) {
    let kinds = |ops: &Ops| ops.iter().map(|(op, _)| *op).collect::<String>();
    assert_eq!(kinds(got), kinds(want), "{what}: ops");
    for ((_, g), (_, w)) in got.iter().zip(want) {
        for (a, b) in g.iter().zip(w) {
            assert!((a - b).abs() < 0.01, "{what}: {g:?} vs HarfBuzz {w:?}");
        }
    }
}

/// Checks every outline of `expected` against what `font` draws through
/// `Face::glyph_outline_at_coords`, `GlyphOutlines::outline` and
/// `GlyphOutlines::draw`, and `flatten`'s walk over
/// `Varc::composite_with_font_coords` when `walk` is set. Returns the
/// number of outlines checked.
fn check_against_harfbuzz(font: &[u8], expected: &str, walk: bool) -> usize {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).unwrap();
    let mut checked = 0;
    for line in expected.lines().filter(|l| l.starts_with("outline ")) {
        let mut fields = line.splitn(5, ' ').skip(1);
        let location = fields.next().unwrap();
        let gid: u16 = fields.next().unwrap().parse().unwrap();
        let name = fields.next().unwrap();
        let want = parse_ops(fields.next().unwrap_or(""));
        let coords = coords_at(&face, location);
        let what = format!("{name} at {location}");

        let outline = face.glyph_outline_at_coords(gid, &coords).unwrap();
        assert_close(
            &outline.map_or_else(Vec::new, |o| ops_of(o.ops())),
            &want,
            &format!("{what}, Face"),
        );
        let outlines = face.glyph_outlines(&coords);
        let outline = outlines.outline(gid).unwrap();
        assert_close(
            &outline.map_or_else(Vec::new, |o| ops_of(o.ops())),
            &want,
            &format!("{what}, GlyphOutlines::outline"),
        );
        let mut sink = Recorder::default();
        assert!(outlines.draw(gid, &mut sink).unwrap(), "{what}");
        assert_close(
            &ops_of(sink.0.ops()),
            &want,
            &format!("{what}, GlyphOutlines::draw"),
        );
        if walk {
            let mut ops = Vec::new();
            let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            flatten(&face, gid, &coords, &coords, identity, &mut ops);
            assert_close(&ops, &want, &format!("{what}, composite_with_font_coords"));
        }
        checked += 1;
    }
    checked
}

#[test]
fn varc_outlines_match_harfbuzz() {
    assert_eq!(check_against_harfbuzz(FONT, EXPECTED, true), 14 * 8);
}

#[test]
fn varc_walks_through_nested_composites_match_harfbuzz() {
    // Nested resets, self-references, cycles, a missing record, short
    // delta sets, conditions inside nested composites, regions without
    // axes, large rotations. `flatten` recurses on a glyph that names
    // itself, so only the face's walk draws this font.
    assert_eq!(
        check_against_harfbuzz(FACE_FONT, FACE_EXPECTED, false),
        48 * 8
    );
}
