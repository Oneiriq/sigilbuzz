//! Defensive coverage for `Face::glyph_outline` against CFF2 fixtures
//! with truncated / out-of-range bytes. Locked in alongside the #209
//! fix so future tweaks to the CFF/CFF2 INDEX walk or the charstring
//! interpreter can't silently regress to a panic on hostile input.

#[test]
fn cff2_outline_oob_glyph_id_returns_none() {
    // Real CFF2 font, but ask for a glyph past num_glyphs. Should
    // return Ok(None), not panic and not error.
    const SS3: &[u8] = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");
    let face = sigilbuzz::Face::parse_bytes(SS3, 0).unwrap();
    let cff2 = face.cff2().unwrap();
    let n = cff2.num_glyphs();
    assert!(n > 0);
    let res = face.glyph_outline(n + 5).unwrap();
    assert!(res.is_none(), "OOB CFF2 gid must return None, got Some");
}

#[test]
fn cff2_outline_at_max_u16_glyph_id_returns_none_not_panic() {
    const SS3: &[u8] = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");
    let face = sigilbuzz::Face::parse_bytes(SS3, 0).unwrap();
    // u16::MAX is well past any real font's num_glyphs. Must not panic.
    let res = face.glyph_outline(u16::MAX);
    match res {
        Ok(None) => {}
        Ok(Some(_)) => panic!("u16::MAX gid should not return an outline"),
        Err(e) => panic!("u16::MAX gid should return Ok(None), not Err: {e:?}"),
    }
}

#[test]
fn cff2_outline_at_explicit_default_coords_matches_no_coords() {
    // Edge case from the wave 16 brief: blend at default coords (all
    // zeros) should produce master values byte-identical to passing an
    // empty coord slice.
    const SS3: &[u8] = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");
    let face = sigilbuzz::Face::parse_bytes(SS3, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let gid = cmap.glyph_id('A').unwrap();
    let none_coords = face.glyph_outline_at_coords(gid, &[]).unwrap().unwrap();
    let zero_coords = face.glyph_outline_at_coords(gid, &[0.0]).unwrap().unwrap();
    assert_eq!(
        none_coords.ops(),
        zero_coords.ops(),
        "explicit default coords must match no-coords path"
    );
}
