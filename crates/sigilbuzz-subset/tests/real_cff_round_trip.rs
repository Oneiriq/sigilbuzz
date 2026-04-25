//! Real-font CFF1 + CFF2 subset round-trip integration tests.
//!
//! Closes the synthetic-only caveat from #120 (non-CID CFF1) — and,
//! once the follow-up commits in this PR land, #135 (CID-keyed CFF1
//! + CFF2 non-identity) and #138 (cross-FD subroutine sharing in
//! CID-keyed fonts) — by exercising the public [`subset`] entry
//! point against real OFL fonts plus a hand-built CID fixture.
//!
//! This commit covers the **CFF1 (non-CID)** axis with a vendored
//! Source Code Pro Latin subset. Subsequent commits add the CFF2
//! and CID-keyed CFF1 fixtures + tests.

use sigilbuzz::tables::tag;
use sigilbuzz::{shape, Buffer, Face, Font};
use sigilbuzz_subset::{subset, SubsetInput};

const SOURCE_CODE_PRO: &[u8] =
    include_bytes!("../../../tests/fonts/SourceCodePro-Latin-Subset.otf");

fn cmap_lookup(face: &Face<'_>, ch: char) -> u16 {
    face.cmap()
        .unwrap()
        .glyph_id(ch)
        .unwrap_or_else(|| panic!("source font does not map {ch}"))
}

/// Shape a single ASCII char through `font`; return the raw glyph id
/// the shaper picked plus the advance the run reported.
fn shape_one(font: &Font<'_>, ch: char) -> (u32, i32) {
    let mut buf = Buffer::new();
    let s = ch.to_string();
    buf.set_text(&s);
    let run = shape(font, &buf, &[]).expect("shape succeeds");
    assert_eq!(run.glyphs.len(), 1, "expected exactly 1 glyph for {ch}");
    (run.glyphs[0].glyph_id, run.glyphs[0].x_advance)
}

#[test]
fn real_cff1_subset_round_trip() {
    // Source Code Pro Latin subset: real, non-synthetic CFF1 (non-CID)
    // OFL fixture. Subset down to {A, B, C, D, E}, reload, reshape.
    let face = Face::parse_bytes(SOURCE_CODE_PRO, 0).expect("source parses");
    assert!(
        face.record(tag::CFF1).is_some(),
        "fixture must carry CFF1 outlines",
    );

    let kept_chars = ['A', 'B', 'C', 'D', 'E'];
    let kept_gids: Vec<u16> = kept_chars.iter().map(|&c| cmap_lookup(&face, c)).collect();

    let input = SubsetInput {
        gids: kept_gids.clone(),
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: false,
    };
    let out = subset(&face, &input).expect("CFF1 real-font subset succeeds");

    // Subset re-parses as a face on its own.
    let subset_face = Face::parse_bytes(&out.bytes, 0).expect("subset re-parses");
    assert!(
        subset_face.record(tag::CFF1).is_some(),
        "subset retains CFF1 outlines",
    );

    // numGlyphs after subset: 1 (.notdef) + 5 kept = 6.
    let new_num_glyphs = subset_face.maxp().unwrap().num_glyphs;
    assert!(
        new_num_glyphs >= 6,
        "expected at least 6 glyphs in subset, got {new_num_glyphs}",
    );

    // Every kept gid's advance survives the renumber.
    let src_hmtx = face.hmtx().unwrap();
    let new_hmtx = subset_face.hmtx().unwrap();
    for (old, new) in &out.gid_map {
        let want = src_hmtx.advance(*old).unwrap_or(0);
        let got = new_hmtx.advance(*new).unwrap_or(0);
        assert_eq!(
            want, got,
            "advance mismatch for gid {old}->{new}: src {want} vs subset {got}",
        );
    }

    // Cmap consistency: each kept char resolves through the new cmap to
    // a gid in [1..new_num_glyphs).
    let new_cmap = subset_face.cmap().unwrap();
    for &ch in &kept_chars {
        let new_gid = new_cmap
            .glyph_id(ch)
            .unwrap_or_else(|| panic!("subset cmap dropped {ch}"));
        assert!(
            new_gid > 0 && new_gid < new_num_glyphs,
            "subset cmap of {ch} = {new_gid}, out of [1..{new_num_glyphs})",
        );
    }

    // End-to-end: shape "ABCDE" through both source and subset; gids
    // come out of different namespaces, but advances for each char
    // must match.
    let src_font = Font::new(face.clone(), 16.0);
    let subset_font = Font::new(subset_face.clone(), 16.0);
    for &ch in &kept_chars {
        let (src_gid, src_adv) = shape_one(&src_font, ch);
        let (new_gid, new_adv) = shape_one(&subset_font, ch);
        assert_eq!(
            src_adv, new_adv,
            "{ch}: shaped advance differs source {src_adv} vs subset {new_adv}",
        );
        // Gid coming out of the subset shaper must be the new cmap's gid.
        let expected_new_gid: u32 = new_cmap.glyph_id(ch).unwrap().into();
        assert_eq!(
            new_gid, expected_new_gid,
            "subset shaper picked gid {new_gid}, cmap says {expected_new_gid}",
        );
        // Sanity: source gid was the source cmap's gid.
        let expected_src_gid: u32 = face.cmap().unwrap().glyph_id(ch).unwrap().into();
        assert_eq!(src_gid, expected_src_gid);
    }
}
