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
    // design-unit epsilon. Any regression, even a single glyph,
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
    // rewritten to a two-pass scheme that materializes absolute
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

// Locate `glyf` and `loca` raw bytes via the SFNT directory. These
// helpers stay at module scope so the clippy `items_after_statements`
// lint is happy.
fn be_u16(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}
fn be_u32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
fn be_i16(b: &[u8], off: usize) -> i16 {
    i16::from_be_bytes([b[off], b[off + 1]])
}

/// Scans the `glyf` table of a TTF for composite glyphs whose
/// component flags clear `ARGS_ARE_XY_VALUES` (bit 0x0002), i.e.
/// anchor-mode references. Returns `(anchor_components, phantom_anchor_components)`.
///
/// `phantom_anchor_components` further filters anchor-mode components
/// whose `arg1` index lands in the parent's phantom-point range
/// (`>= parent_contour_point_count`).
///
/// Implemented as a small standalone walker so the assertion is
/// independent of sigilbuzz's own parser. We want the test to pin
/// the *fixture's* shape, not just its parsing.
fn scan_anchor_components(bytes: &[u8]) -> (usize, usize) {
    let theirs = ttf_parser::Face::parse(bytes, 0).expect("ttf-parser face");
    let num_tables = be_u16(bytes, 4) as usize;
    let mut glyf: &[u8] = &[];
    let mut loca: &[u8] = &[];
    let mut head: &[u8] = &[];
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        let tag = &bytes[rec..rec + 4];
        let off = be_u32(bytes, rec + 8) as usize;
        let len = be_u32(bytes, rec + 12) as usize;
        match tag {
            b"glyf" => glyf = &bytes[off..off + len],
            b"loca" => loca = &bytes[off..off + len],
            b"head" => head = &bytes[off..off + len],
            _ => {}
        }
    }
    let loc_format = be_i16(head, 50);
    let num_glyphs = theirs.number_of_glyphs() as usize;
    // Resolve glyph offsets.
    let glyph_offset = |gid: usize| -> usize {
        if loc_format == 0 {
            (be_u16(loca, gid * 2) as usize) * 2
        } else {
            be_u32(loca, gid * 4) as usize
        }
    };
    // To know if an anchor-mode component is phantom, we need each
    // referenced parent glyph's contour-point count. Cache it.
    let parent_point_count = |gid: u16| -> Option<u16> {
        let s = glyph_offset(gid as usize);
        let e = glyph_offset(gid as usize + 1);
        if s == e {
            return None;
        }
        let body = &glyf[s..e];
        let n_contours = be_i16(body, 0);
        if n_contours <= 0 {
            return None;
        }
        // Skip header (10) + (n_contours - 1) * 2 to last endPt.
        let last_ep_off = 10 + (n_contours as usize - 1) * 2;
        Some(be_u16(body, last_ep_off) + 1)
    };
    let mut anchor = 0usize;
    let mut phantom = 0usize;
    for gid in 0..num_glyphs {
        let s = glyph_offset(gid);
        let e = glyph_offset(gid + 1);
        if s == e {
            continue;
        }
        let body = &glyf[s..e];
        let n_contours = be_i16(body, 0);
        if n_contours >= 0 {
            continue; // simple glyph
        }
        let mut p = 10usize;
        loop {
            let flags = be_u16(body, p);
            let comp_gid = be_u16(body, p + 2);
            p += 4;
            let words = flags & 0x0001 != 0;
            let xy = flags & 0x0002 != 0;
            let (arg1, _arg2);
            if words {
                arg1 = be_u16(body, p) as i32;
                _arg2 = be_u16(body, p + 2) as i32;
                p += 4;
            } else {
                arg1 = body[p] as i32;
                _arg2 = body[p + 1] as i32;
                p += 2;
            }
            if !xy {
                anchor += 1;
                let parent_pts = parent_point_count(comp_gid).unwrap_or(0) as i32;
                if arg1 >= parent_pts {
                    phantom += 1;
                }
            }
            if flags & 0x0008 != 0 {
                p += 2;
            } else if flags & 0x0040 != 0 {
                p += 4;
            } else if flags & 0x0080 != 0 {
                p += 8;
            }
            if flags & 0x0020 == 0 {
                break;
            }
        }
    }
    (anchor, phantom)
}

#[test]
fn phantom_anchor_fixture_outlines_match_harfbuzz() {
    // Hand-crafted fixture (see `tests/tools/build_phantom_anchor_fixture.py`)
    // whose gid 3 (`combo`) has one component in plain XY mode and one
    // in anchor mode whose `arg1`, 5, lies past the 4 points placed
    // before it. HarfBuzz indexes its running point list there: the
    // square's 4 points, then the anchored mark's own 3 points and 4
    // phantom points, so point 5 is the mark's (550, 0), and the mark
    // moves by (550, 0) - (500, 0). ttf-parser ignores anchor mode and
    // draws the mark where it is, so it is the reference for the other
    // glyphs only. HarfBuzz 14.5.0 draws `combo` with these points (its
    // first phantom point is the origin, so its left shift is zero).
    let bytes: &[u8] = include_bytes!("fixtures/phantom_anchor.ttf");
    let (anchor, phantom) = scan_anchor_components(bytes);
    assert!(
        anchor >= 1,
        "fixture must carry at least one anchor-mode component, got {anchor}"
    );
    assert!(
        phantom >= 1,
        "fixture must carry at least one component anchored past the points before it, got {phantom}"
    );
    let harfbuzz_combo: [(SimpleOp, [f32; 2]); 11] = [
        (SimpleOp::Move, [0.0, 0.0]),
        (SimpleOp::Line, [500.0, 0.0]),
        (SimpleOp::Line, [500.0, 500.0]),
        (SimpleOp::Line, [0.0, 500.0]),
        (SimpleOp::Line, [0.0, 0.0]),
        (SimpleOp::Close, [0.0, 0.0]),
        (SimpleOp::Move, [550.0, 0.0]),
        (SimpleOp::Line, [600.0, 0.0]),
        (SimpleOp::Line, [550.0, 50.0]),
        (SimpleOp::Line, [550.0, 0.0]),
        (SimpleOp::Close, [0.0, 0.0]),
    ];
    // Per-glyph diagnostic to surface the offending gid clearly.
    let ours = Face::parse_bytes(bytes, 0).expect("sigilbuzz face");
    let theirs = ttf_parser::Face::parse(bytes, 0).expect("ttf-parser face");
    for gid in 0..theirs.number_of_glyphs() {
        let mut builder = CollectBuilder::default();
        let drew_theirs = theirs.outline_glyph(ttf_parser::GlyphId(gid), &mut builder);
        if gid == 3 {
            builder.ops = harfbuzz_combo
                .iter()
                .map(|&(op, [x, y])| (op, [x, y, 0.0, 0.0, 0.0, 0.0]))
                .collect();
        }
        let ours_outline = ours.glyph_outline(gid).expect("our outline");
        match (drew_theirs, ours_outline) {
            (None, None) => {}
            (Some(_), None) => panic!("gid {gid}: ttf-parser drew but sigilbuzz did not"),
            (None, Some(_)) => panic!("gid {gid}: sigilbuzz drew but ttf-parser did not"),
            (Some(_), Some(out)) => {
                assert_eq!(
                    out.len(),
                    builder.ops.len(),
                    "gid {gid}: op count mismatch ours={} theirs={}",
                    out.len(),
                    builder.ops.len()
                );
                for (i, (ours_op, theirs_op)) in
                    out.ops().iter().zip(builder.ops.iter()).enumerate()
                {
                    let (tag, coords) = collapse(*ours_op);
                    assert_eq!(
                        tag, theirs_op.0,
                        "gid {gid} op {i}: tag mismatch {:?} vs {:?}",
                        tag, theirs_op.0
                    );
                    assert!(
                        approx_eq(&coords, &theirs_op.1),
                        "gid {gid} op {i}: coord mismatch {coords:?} vs {:?}",
                        theirs_op.1
                    );
                }
            }
        }
    }
}

#[test]
fn anchors_to_vertical_phantom_points_without_vmtx_match_harfbuzz() {
    // The phantom-anchor fixture has no `vmtx`. HarfBuzz then gives
    // every glyph a top side bearing of zero and a vertical advance of
    // an em, so the anchored mark's top phantom point (running list
    // index 9: the square's 4 points, the mark's 3, then its pp1 and
    // pp2) is (0, yMax) = (0, 50) and its bottom one (index 10) is
    // (0, 50 - 1000). Rewriting `combo`'s anchor from 5 to 9 and 10
    // moves the mark's point 0, (500, 0), there. HarfBuzz 14.5.0 draws
    // the rewritten fonts with these marks.
    let fixture: &[u8] = include_bytes!("fixtures/phantom_anchor.ttf");
    let face = Face::parse_bytes(fixture, 0).expect("fixture face");
    let glyf = face.record(*b"glyf").expect("glyf").offset as usize;
    let combo = glyf + face.loca().expect("loca").range(3).expect("combo").0 as usize;
    // The header, the square's flags, glyph id and byte offsets, then
    // the mark's flags and glyph id: `arg1` and `arg2` follow.
    let anchor = combo + 10 + 6 + 4;
    assert_eq!(fixture[anchor..anchor + 2], [5, 0], "fixture layout");
    for (arg1, (dx, dy)) in [(9u8, (-500.0f32, 50.0f32)), (10, (-500.0, -950.0))] {
        let mut bytes = fixture.to_vec();
        bytes[anchor] = arg1;
        let face = Face::parse_bytes(&bytes, 0).expect("patched face");
        let outline = face.glyph_outline(3).expect("outline").expect("drew");
        let mark: Vec<(f32, f32)> = outline.ops()[6..]
            .iter()
            .filter_map(|op| match *op {
                PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => Some((x, y)),
                _ => None,
            })
            .collect();
        let want: Vec<(f32, f32)> = [(500.0, 0.0), (550.0, 0.0), (500.0, 50.0), (500.0, 0.0)]
            .iter()
            .map(|&(x, y)| (x + dx, y + dy))
            .collect();
        assert_eq!(mark, want, "arg1 = {arg1}");
    }
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
