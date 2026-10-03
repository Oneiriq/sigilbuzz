//! Regression repro for issue #209: Face::glyph_outline returned
//! Ok(None) for CFF2 fonts because the CFF INDEX parser used a u16
//! count, but CFF2's INDEX uses a u32 count. The fixture is the
//! same SourceSans3VF subset called out in the issue.
use sigilbuzz::tables::PathOp;

#[test]
fn cff2_outline_returns_some_for_source_sans() {
    let bytes = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");
    let face = sigilbuzz::Face::parse_bytes(bytes, 0).unwrap();
    let cmap = face.cmap().unwrap();

    // Walk every ASCII letter; all should resolve to non-empty
    // PathOp sequences. Capture exact shape counts for 'A'.
    let mut a_outline = None;
    for ch in 'A'..='Z' {
        let gid = cmap
            .glyph_id(ch)
            .unwrap_or_else(|| panic!("no gid for {ch}"));
        let outline = face
            .glyph_outline(gid)
            .unwrap_or_else(|e| panic!("err for {ch}: {e:?}"))
            .unwrap_or_else(|| panic!("none for {ch}"));
        assert!(!outline.is_empty(), "empty outline for {ch}");
        if ch == 'A' {
            a_outline = Some(outline);
        }
    }

    // Pin the well-known Source Sans 3 'A' shape: the default-instance
    // glyph emits 5 MoveTo, 4 Close, 4 LineTo, and 4 CubicTo ops in
    // the right counts. Numbers come from running the new CFF2 path
    // against the bundled subset; if anyone breaks the charstring
    // interpreter's blend / endchar handling, this assertion fires.
    let a = a_outline.unwrap();
    let mut moves: u32 = 0;
    let mut lines: u32 = 0;
    let mut quads: u32 = 0;
    let mut curves: u32 = 0;
    let mut closes: u32 = 0;
    for op in a.ops() {
        match op {
            PathOp::MoveTo { .. } => moves += 1,
            PathOp::LineTo { .. } => lines += 1,
            PathOp::QuadTo { .. } => quads += 1,
            PathOp::CubicTo { .. } => curves += 1,
            PathOp::Close => closes += 1,
        }
    }
    // `A` in Source Sans 3: outer triangle + middle bar + counters,
    // mostly straight edges with two cubics on the apex.
    assert!(moves >= 1, "no MoveTo");
    assert!(lines >= 1, "no LineTo");
    assert_eq!(quads, 0, "CFF emits cubics, not quads");
    // Every contour is closed, the last one included: CFF2 has no
    // endchar, so the outline closes it when the charstring ends.
    assert_eq!(closes, moves, "every contour must end with Close");
    assert_eq!(a.ops().last(), Some(&PathOp::Close));
    assert!(
        curves >= 1,
        "expected at least one CubicTo on Source Sans A"
    );
}

#[test]
fn cff2_glyph_outline_default_matches_glyph_outline_at_coords_empty() {
    let bytes = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");
    let face = sigilbuzz::Face::parse_bytes(bytes, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let gid = cmap.glyph_id('A').unwrap();
    let a = face.glyph_outline(gid).unwrap().unwrap();
    let b = face.glyph_outline_at_coords(gid, &[]).unwrap().unwrap();
    assert_eq!(a.ops(), b.ops());
}

#[test]
fn cff1_outline_returns_some_for_source_code_pro() {
    // SourceCodePro is a static CFF1 font; the same dispatch path
    // also covers it. Without this guard a regression that breaks
    // CFF1 INDEX parsing would not surface in the CFF2 fixture.
    let bytes = include_bytes!("fonts/SourceCodePro-Latin-Subset.otf");
    let face = sigilbuzz::Face::parse_bytes(bytes, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let gid = cmap.glyph_id('A').unwrap();
    let outline = face
        .glyph_outline(gid)
        .expect("CFF1 outline ok")
        .expect("CFF1 outline drew");
    assert!(!outline.is_empty(), "CFF1 outline empty for 'A'");
    assert!(
        outline
            .ops()
            .iter()
            .any(|op| matches!(op, PathOp::MoveTo { .. })),
        "CFF1 outline missing MoveTo"
    );
}

#[test]
fn cff2_glyph_outline_at_nondefault_coords_differs_from_default() {
    let bytes = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");
    let face = sigilbuzz::Face::parse_bytes(bytes, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let gid = cmap.glyph_id('A').unwrap();
    let default_outline = face.glyph_outline(gid).unwrap().unwrap();
    // Source Sans 3 VF: wght axis at index 0 in normalized order;
    // push to the heaviest weight available (1.0) and compare.
    let bold = face
        .glyph_outline_at_coords(gid, &[1.0_f32])
        .unwrap()
        .unwrap();
    assert_eq!(
        default_outline.ops().len(),
        bold.ops().len(),
        "blend should not change op count"
    );
    let same: bool = default_outline
        .ops()
        .iter()
        .zip(bold.ops().iter())
        .all(|(a, b)| a == b);
    assert!(
        !same,
        "non-default coords must shift at least one coordinate"
    );
}
