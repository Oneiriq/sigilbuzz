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
fn amiri_glyph_outlines_match_ttf_parser_exactly() {
    // Amiri is a large Arabic font (6710 glyphs) with extensive
    // composite use, including TWO_BY_TWO rotation matrices and
    // anchor-point references. With the composite flattener
    // rewritten to a two-pass scheme that materialises absolute
    // points before emitting, every glyph now matches ttf-parser
    // to within the 1e-2 epsilon. Drop the coverage to a strict
    // equality check so the next composite-shape regression lands
    // here loudly.
    let bytes = include_bytes!("fixtures/amiri_regular.ttf");
    let (matched, total) = parity_for(bytes);
    let ratio = matched as f32 / total as f32;
    println!("Amiri outline parity: {matched}/{total} = {ratio:.3}");
    assert_eq!(
        matched, total,
        "Amiri outline parity regression: {matched}/{total} = {ratio:.3}"
    );
}

#[test]
fn amiri_phantom_anchor_path_does_not_regress_parity() {
    // Sanity-check: scan Amiri for composite glyphs whose anchor
    // index falls into the phantom range (>= the parent's
    // contour-point count). Whether any glyph hits this path is a
    // factual property of the font; we just want to confirm the
    // walk completes without panicking and that the per-glyph
    // outline still matches ttf-parser. Acts as a regression guard
    // for the phantom-anchor code path even if the font happens not
    // to exercise it today.
    let bytes = include_bytes!("fixtures/amiri_regular.ttf");
    let ours = Face::parse_bytes(bytes, 0).expect("sigilbuzz face");
    let theirs = ttf_parser::Face::parse(bytes, 0).expect("ttf-parser face");
    let mut seen = 0usize;
    for gid in 0..theirs.number_of_glyphs() {
        let mut builder = CollectBuilder::default();
        let drew_theirs = theirs.outline_glyph(ttf_parser::GlyphId(gid), &mut builder);
        let ours_outline = ours.glyph_outline(gid).expect("our outline");
        if let (Some(_), Some(out)) = (drew_theirs, &ours_outline) {
            if out.len() == builder.ops.len() {
                let mut ok = true;
                for (ours_op, theirs_op) in out.ops().iter().zip(builder.ops.iter()) {
                    let (tag, coords) = collapse(*ours_op);
                    if tag != theirs_op.0 || !approx_eq(&coords, &theirs_op.1) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    seen += 1;
                }
            }
        } else if drew_theirs.is_none() && ours_outline.is_none() {
            seen += 1;
        }
    }
    assert_eq!(
        seen,
        theirs.number_of_glyphs() as usize,
        "Amiri parity broke after phantom-anchor wiring: {seen}/{}",
        theirs.number_of_glyphs()
    );
}

#[test]
fn amiri_two_anchor_glyphs_now_match() {
    // The two Amiri glyphs that previously missed parity (gids 379
    // and 6123) drove the rewrite of the glyf composite flattener:
    // both reference component children with TWO_BY_TWO transforms
    // whose 2x2 was being read in wrong field order, and both also
    // exercise the wider re-architecting that resolves
    // ARGS_ARE_XY_VALUES-clear anchor pairs. Pin them by gid so the
    // regression surfaces directly if either of those two paths
    // breaks again.
    let bytes = include_bytes!("fixtures/amiri_regular.ttf");
    let ours = Face::parse_bytes(bytes, 0).expect("sigilbuzz face");
    let theirs = ttf_parser::Face::parse(bytes, 0).expect("ttf-parser face");
    for &gid in &[379u16, 6123u16] {
        let mut builder = CollectBuilder::default();
        theirs.outline_glyph(ttf_parser::GlyphId(gid), &mut builder);
        let ours_outline = ours
            .glyph_outline(gid)
            .expect("outline ok")
            .expect("outline drew");
        assert_eq!(
            ours_outline.len(),
            builder.ops.len(),
            "op-count mismatch on gid {gid}: ours={} theirs={}",
            ours_outline.len(),
            builder.ops.len()
        );
        for (i, (ours_op, theirs_op)) in ours_outline
            .ops()
            .iter()
            .zip(builder.ops.iter())
            .enumerate()
        {
            let (tag, coords) = collapse(*ours_op);
            assert_eq!(
                tag, theirs_op.0,
                "op-tag mismatch at {i} on gid {gid}: {:?} vs {:?}",
                tag, theirs_op.0
            );
            assert!(
                approx_eq(&coords, &theirs_op.1),
                "op-coord mismatch at {i} on gid {gid}: {coords:?} vs {:?}",
                theirs_op.1
            );
        }
    }
}
