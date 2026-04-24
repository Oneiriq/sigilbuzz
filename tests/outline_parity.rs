//! Parity check: sigilbuzz's `Face::glyph_outline` emits the same op
//! sequence as ttf-parser's `OutlineBuilder` callbacks for every
//! glyph in the bundled fixture fonts.
//!
//! Exact numeric equality is brittle (both sides route through
//! `f32` composition chains that accumulate rounding). The test
//! therefore compares op shape (MoveTo / LineTo / QuadTo / CubicTo /
//! Close in the same order) and uses a 1e-2 design-unit epsilon for
//! coordinates, which is well below a single pixel at any practical
//! size.

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;

#[derive(Debug, Clone, Copy, PartialEq)]
enum SimpleOp {
    Move,
    Line,
    Quad,
    Cubic,
    Close,
}

#[derive(Default)]
struct CollectBuilder {
    ops: Vec<(SimpleOp, [f32; 6])>,
}

impl ttf_parser::OutlineBuilder for CollectBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.ops.push((SimpleOp::Move, [x, y, 0.0, 0.0, 0.0, 0.0]));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.ops.push((SimpleOp::Line, [x, y, 0.0, 0.0, 0.0, 0.0]));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.ops.push((SimpleOp::Quad, [x1, y1, x, y, 0.0, 0.0]));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.ops.push((SimpleOp::Cubic, [x1, y1, x2, y2, x, y]));
    }
    fn close(&mut self) {
        self.ops
            .push((SimpleOp::Close, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]));
    }
}

fn collapse(op: PathOp) -> (SimpleOp, [f32; 6]) {
    match op {
        PathOp::MoveTo { x, y } => (SimpleOp::Move, [x, y, 0.0, 0.0, 0.0, 0.0]),
        PathOp::LineTo { x, y } => (SimpleOp::Line, [x, y, 0.0, 0.0, 0.0, 0.0]),
        PathOp::QuadTo { cx, cy, x, y } => (SimpleOp::Quad, [cx, cy, x, y, 0.0, 0.0]),
        PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        } => (SimpleOp::Cubic, [c1x, c1y, c2x, c2y, x, y]),
        PathOp::Close => (SimpleOp::Close, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    }
}

fn approx_eq(a: &[f32; 6], b: &[f32; 6]) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 1e-2)
}

fn parity_for(bytes: &[u8]) -> (usize, usize) {
    let ours = Face::parse_bytes(bytes, 0).expect("sigilbuzz face");
    let theirs = ttf_parser::Face::parse(bytes, 0).expect("ttf-parser face");

    let mut matched = 0usize;
    let mut total = 0usize;
    for gid in 0..theirs.number_of_glyphs() {
        total += 1;
        let mut builder = CollectBuilder::default();
        let drew_theirs = theirs.outline_glyph(ttf_parser::GlyphId(gid), &mut builder);
        let ours_outline = ours.glyph_outline(gid).expect("our outline");

        match (drew_theirs, ours_outline) {
            (None, None) => matched += 1,
            (Some(_), None) | (None, Some(_)) => {}
            (Some(_), Some(out)) => {
                if out.len() != builder.ops.len() {
                    continue;
                }
                let mut ok = true;
                for (ours_op, theirs_op) in out.ops().iter().zip(builder.ops.iter()) {
                    let (ours_tag, ours_coords) = collapse(*ours_op);
                    if ours_tag != theirs_op.0 || !approx_eq(&ours_coords, &theirs_op.1) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    matched += 1;
                }
            }
        }
    }
    (matched, total)
}

#[test]
fn opensans_glyph_outlines_match_ttf_parser_exactly() {
    let bytes = include_bytes!("fixtures/opensans_regular.ttf");
    let (matched, total) = parity_for(bytes);
    let ratio = matched as f32 / total as f32;
    println!("Open Sans outline parity: {matched}/{total} = {ratio:.3}");
    // Every glyph of Open Sans (938 glyphs at the time of writing)
    // matches ttf-parser's OutlineBuilder sequence within a 1e-2
    // design-unit epsilon. Any regression — even a single glyph —
    // is surfaced here immediately.
    assert_eq!(
        matched, total,
        "Open Sans outline parity regression: {matched}/{total}"
    );
}

#[test]
fn amiri_glyph_outlines_match_ttf_parser_majority() {
    // Amiri is a large Arabic font with many composites. Hold it
    // to a strong majority; 100% parity is plausible but not
    // guaranteed on first pass because of composite anchor-point
    // matching corners.
    let bytes = include_bytes!("fixtures/amiri_regular.ttf");
    let (matched, total) = parity_for(bytes);
    let ratio = matched as f32 / total as f32;
    println!("Amiri outline parity: {matched}/{total} = {ratio:.3}");
    assert!(
        ratio >= 0.95,
        "Amiri outline parity regression: {matched}/{total} = {ratio:.3}"
    );
}
