//! Parity check for CFF2 outlines: sigilbuzz's
//! `Face::glyph_outline_at_coords` emits the same op sequence as
//! ttf-parser's `OutlineBuilder` callbacks, across the `wght` axis.
//!
//! Both fixtures route glyphs through an FDArray, Private DICTs, Local
//! Subrs, and `blend`, so this covers the per-glyph Font DICT lookup
//! and the blend region cache as well as the closing of the last
//! contour. Coordinates are compared with a 1e-2 design-unit epsilon,
//! as in `outline_parity.rs`.

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;

#[derive(Default)]
struct Collect {
    ops: Vec<PathOp>,
}

impl ttf_parser::OutlineBuilder for Collect {
    fn move_to(&mut self, x: f32, y: f32) {
        self.ops.push(PathOp::MoveTo { x, y });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.ops.push(PathOp::LineTo { x, y });
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.ops.push(PathOp::QuadTo { cx, cy, x, y });
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.ops.push(PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        });
    }
    fn close(&mut self) {
        self.ops.push(PathOp::Close);
    }
}

fn coords_of(op: &PathOp) -> Vec<f32> {
    match *op {
        PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => vec![x, y],
        PathOp::QuadTo { cx, cy, x, y } => vec![cx, cy, x, y],
        PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        } => vec![c1x, c1y, c2x, c2y, x, y],
        PathOp::Close => Vec::new(),
    }
}

fn same_op(a: &PathOp, b: &PathOp) -> bool {
    core::mem::discriminant(a) == core::mem::discriminant(b)
        && coords_of(a)
            .iter()
            .zip(coords_of(b).iter())
            .all(|(x, y)| (x - y).abs() < 1e-2)
}

/// Normalized coords for a `wght` user value, through `fvar` and `avar`.
fn normalized(face: &Face<'_>, wght: f32) -> Vec<f32> {
    let fvar = face.fvar().unwrap().expect("fvar");
    let user: Vec<f32> = fvar
        .axes()
        .iter()
        .map(|a| {
            if &a.tag == b"wght" {
                wght
            } else {
                a.default_value
            }
        })
        .collect();
    let n = fvar.normalize_coords(&user);
    match face.avar().unwrap() {
        Some(avar) => avar.remap_all(&n),
        None => n,
    }
}

/// Compares every glyph at each `wght` value. Returns the number of
/// glyphs that drew an outline, summed over the weights.
fn assert_parity(bytes: &[u8], weights: &[f32]) -> usize {
    let ours = Face::parse_bytes(bytes, 0).unwrap();
    let mut theirs = ttf_parser::Face::parse(bytes, 0).unwrap();
    let mut drawn = 0;
    for &wght in weights {
        theirs
            .set_variation(ttf_parser::Tag::from_bytes(b"wght"), wght)
            .expect("wght axis");
        let coords = normalized(&ours, wght);
        for gid in 0..theirs.number_of_glyphs() {
            let mut expected = Collect::default();
            let drew = theirs.outline_glyph(ttf_parser::GlyphId(gid), &mut expected);
            // ttf-parser 0.25 leaves the last CFF2 contour open, as sigilbuzz
            // used to: CFF2 has no endchar to close it. Add that Close to
            // its side; every other op must match as is.
            if expected.ops.last().is_some_and(|op| *op != PathOp::Close) {
                expected.ops.push(PathOp::Close);
            }
            let got = ours.glyph_outline_at_coords(gid, &coords).unwrap();
            let got = got.filter(|o| !o.is_empty());
            match (drew, got) {
                (None, None) => {}
                (Some(_), Some(outline)) => {
                    drawn += 1;
                    let ok = outline.len() == expected.ops.len()
                        && outline
                            .ops()
                            .iter()
                            .zip(expected.ops.iter())
                            .all(|(a, b)| same_op(a, b));
                    assert!(
                        ok,
                        "gid {gid} at wght {wght}:\n ours   {:?}\n theirs {:?}",
                        outline.ops(),
                        expected.ops
                    );
                }
                (drew, got) => panic!(
                    "gid {gid} at wght {wght}: ttf-parser drew {}, sigilbuzz drew {}",
                    drew.is_some(),
                    got.is_some()
                ),
            }
        }
    }
    drawn
}

#[test]
fn noto_sans_kr_vf_cff2_outlines_match_ttf_parser() {
    let bytes = include_bytes!("fixtures/noto_sans_kr_vf_cff2_subset.otf");
    let drawn = assert_parity(bytes, &[100.0, 250.0, 400.0, 650.0, 900.0]);
    assert!(drawn > 0);
}

#[test]
fn source_sans_3_vf_cff2_outlines_match_ttf_parser() {
    let bytes = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");
    let drawn = assert_parity(bytes, &[200.0, 400.0, 900.0]);
    assert!(drawn > 0);
}
