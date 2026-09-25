//! `Face::glyph_points` parity check against `hmtx`-derived phantom
//! formulas on the bundled Open Sans fixture.
//!
//! `glyph_points` returns every contour point followed by the four
//! phantom points (pp1 = LSB origin, pp2 = advance origin, pp3 = TSB
//! origin, pp4 = advance-height origin). The phantom layout is the
//! same one composite anchor-mode and `kerx` format-4 type-0 read
//! against, so the test recomputes pp1 / pp2 from `bounds.x_min -
//! hmtx.lsb` and `pp1 + advance` and asserts the trailing two points
//! in the returned slice match.

use sigilbuzz::{Blob, Face};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

#[test]
fn open_sans_glyph_points_phantoms_match_hmtx_formula() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let hmtx = face.hmtx().unwrap();

    // Sample a handful of glyph ids (covering an empty glyph, a
    // small Latin letter, and a few mid-range glyphs) and assert
    // pp1 / pp2 of `glyph_points` match the spec formula.
    for &gid in &[1u16, 2, 36, 37, 50, 100] {
        let bounds = face.glyph_bounds(gid).unwrap();
        let pts = face.glyph_points(gid).unwrap();
        match (bounds, pts) {
            (Some(b), Some(pts)) => {
                assert!(pts.len() >= 4, "phantom block missing for gid {gid}");
                let advance = i32::from(hmtx.advance(gid).unwrap_or(0));
                let lsb = i32::from(hmtx.lsb(gid).unwrap_or(0));
                let expected_pp1_x = i32::from(b.x_min) - lsb;
                let expected_pp2_x = expected_pp1_x + advance;
                let pp1 = pts[pts.len() - 4];
                let pp2 = pts[pts.len() - 3];
                let pp3 = pts[pts.len() - 2];
                let pp4 = pts[pts.len() - 1];
                assert_eq!(
                    i32::from(pp1.0),
                    expected_pp1_x,
                    "pp1.x for gid {gid}: got {pp1:?}, expected x={expected_pp1_x}"
                );
                assert_eq!(pp1.1, 0, "pp1.y must be 0");
                assert_eq!(
                    i32::from(pp2.0),
                    expected_pp2_x,
                    "pp2.x for gid {gid}: got {pp2:?}, expected x={expected_pp2_x}"
                );
                assert_eq!(pp2.1, 0, "pp2.y must be 0");
                // Open Sans is horizontal-only: no vmtx, so pp3 / pp4
                // collapse to (0, 0).
                assert_eq!(pp3, (0, 0), "pp3 collapses without vmtx");
                assert_eq!(pp4, (0, 0), "pp4 collapses without vmtx");
            }
            (None, None) => {
                // Whitespace glyph: both APIs agree on "no outline".
            }
            (b, p) => panic!("inconsistent for gid {gid}: bounds={b:?}, points={p:?}"),
        }
    }
}

#[test]
fn open_sans_glyph_points_contour_count_matches_glyf_point_count() {
    // `Face::glyph_points` returns contour points + 4 phantoms. The
    // raw `glyf::point_count` reports `endPtsOfContours.last + 1 + 4`
    // (i.e. contour-point count + 4 phantom slots). For *simple*
    // glyphs the two counts must agree; composite glyphs report
    // `None` for `point_count` so we skip them.
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let glyf = face.glyf().unwrap();
    let loca = face.loca().unwrap();

    for gid in 1u16..150 {
        let pts = face.glyph_points(gid).unwrap();
        let pc = glyf.point_count(&loca, gid).unwrap();
        // Empty glyph (`pts == None`) or composite (`pc == None`):
        // the counts can't be cross-checked. Only the simple-glyph
        // pair is asserted.
        if let (Some(pts), Some(pc)) = (pts, pc) {
            assert_eq!(
                pts.len(),
                pc as usize,
                "gid {gid}: glyph_points len {} != point_count {}",
                pts.len(),
                pc
            );
        }
    }
}
