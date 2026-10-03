//! Regression test for CFF2 outlines that left their last contour open.
//!
//! CFF2 charstrings have no `endchar`, and the interpreter only closed a
//! contour at the next moveto or at `endchar`. Every CFF2 glyph therefore
//! ended with an open contour, and scanline fills drew streaks wherever the
//! last contour ended away from its start point. The fixture is a subset of
//! Noto Sans KR VF whose glyphs end that way.

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;

const FONT: &[u8] = include_bytes!("fixtures/noto_sans_kr_vf_cff2_subset.otf");

/// Normalized `wght` coordinates to check, including the default instance.
const COORDS: [&[f32]; 4] = [&[], &[0.0], &[0.5], &[1.0]];

/// Checks that every contour in `ops` is closed. Returns true when the last
/// contour ends away from its start point, which is the case the bug broke.
fn assert_closed(ops: &[PathOp], what: &str) -> bool {
    let mut open = false;
    let mut moves = 0;
    let mut closes = 0;
    let mut start = (0.0, 0.0);
    let mut current = (0.0, 0.0);
    let mut last_gap = false;
    for op in ops {
        match *op {
            PathOp::MoveTo { x, y } => {
                assert!(!open, "{what}: MoveTo while the previous contour is open");
                open = true;
                moves += 1;
                start = (x, y);
                current = start;
            }
            PathOp::LineTo { x, y }
            | PathOp::QuadTo { x, y, .. }
            | PathOp::CubicTo { x, y, .. } => {
                current = (x, y);
            }
            PathOp::Close => {
                assert!(open, "{what}: Close without an open contour");
                open = false;
                closes += 1;
                last_gap = current != start;
            }
        }
    }
    assert!(!open, "{what}: the last contour is open");
    assert_eq!(closes, moves, "{what}: Close count");
    last_gap
}

#[test]
fn every_cff2_contour_is_closed() {
    let face = Face::parse_bytes(FONT, 0).unwrap();
    assert!(face.record(*b"CFF2").is_some(), "fixture must carry CFF2");
    let n = face.maxp().unwrap().num_glyphs;
    let mut drawn = 0;
    let mut gaps = 0;
    for coords in COORDS {
        for gid in 0..n {
            let Some(outline) = face.glyph_outline_at_coords(gid, coords).unwrap() else {
                continue;
            };
            if outline.is_empty() {
                continue;
            }
            drawn += 1;
            if assert_closed(outline.ops(), &format!("gid {gid} at {coords:?}")) {
                gaps += 1;
            }
        }
    }
    assert!(drawn > 0, "fixture drew no outlines");
    // The fixture exists for glyphs whose last contour ends away from its
    // start point. If that stops being true it no longer guards the fix.
    assert!(
        gaps > 0,
        "no glyph ends its last contour away from its start"
    );
}
