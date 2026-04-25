//! Real-font CFF1 + CFF2 subset round-trip integration tests.
//!
//! Closes the synthetic-only caveat from #120 (non-CID CFF1), #135
//! (CFF2 + CID-keyed CFF1), and #138 (cross-FD subroutine sharing
//! in CID-keyed fonts) by exercising the public [`subset`] entry
//! point against real OFL fonts plus a hand-built CID fixture.
//!
//! Each test loads the source font, subsets it (or, for the
//! non-CID CFF2 path that the rewriter does not yet rebuild,
//! identity-passes-through every gid), reloads via
//! [`Face::parse_bytes`], and asserts:
//!
//! 1. The subset re-parses as a CFF / CFF2 face on its own.
//! 2. Every kept gid's advance survives the renumber.
//! 3. (CFF2 only) Variation axis behaviour survives: shaping at the
//!    default and at the maxima of the `wght` axis through the
//!    subset matches the source's at the same coords.

use sigilbuzz::tables::tag;
use sigilbuzz::{shape, Buffer, Face, Font};
use sigilbuzz_subset::{subset, SubsetInput};

const SOURCE_CODE_PRO: &[u8] =
    include_bytes!("../../../tests/fonts/SourceCodePro-Latin-Subset.otf");
const SOURCE_SANS_3_VF: &[u8] =
    include_bytes!("../../../tests/fonts/SourceSans3VF-Latin-Subset.otf");
const CID_CFF1: &[u8] = include_bytes!("../../../tests/fonts/CidCff1Synthetic.otf");

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

    // Size shrinkage (#167): subsetting Source Code Pro to {A..E} must
    // drop the CFF1 table to well under half the source size now that
    // unreachable subroutines are pruned. Pre-#167 the rewriter kept
    // every source subr verbatim and the table stayed ~equal.
    let src_cff_len = face.table_bytes(tag::CFF1).unwrap().len();
    let new_cff_len = subset_face.table_bytes(tag::CFF1).unwrap().len();
    assert!(
        new_cff_len * 2 < src_cff_len,
        "CFF1 subset {new_cff_len} bytes should shrink to <50% of source {src_cff_len}",
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

#[test]
fn real_cff2_subset_round_trip() {
    // Source Sans 3 VF Latin subset: real CFF2 + variable-font OFL
    // fixture (single `wght` axis spanning 200..900). Adobe's CFF2
    // builds use a single Font DICT and elide FDSelect (the CFF2 spec
    // marks FDSelect optional when only one FD applies); the parser
    // synthesises an implicit "every gid → FD 0" mapping for that
    // case so the non-identity rewriter handles it like any other
    // single-FD source. The non-identity round-trip lives in its own
    // test below; here we exercise the **identity-passthrough** path
    // to prove the byte-identical guarantee for the full-coverage
    // case.
    let face = Face::parse_bytes(SOURCE_SANS_3_VF, 0).expect("CFF2 source parses");
    assert!(
        face.record(tag::CFF2).is_some(),
        "fixture must carry CFF2 outlines",
    );
    let src_num_glyphs = face.maxp().unwrap().num_glyphs;

    // Identity-passthrough requires every gid in the kept set —
    // pass the full 0..N range so the closure's kept set equals the
    // source's identity.
    let all_gids: Vec<u16> = (0..src_num_glyphs).collect();
    let input = SubsetInput {
        gids: all_gids,
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: true,
    };
    let out = subset(&face, &input).expect("CFF2 identity passthrough succeeds");

    let subset_face = Face::parse_bytes(&out.bytes, 0).expect("CFF2 subset re-parses");
    assert!(
        subset_face.record(tag::CFF2).is_some(),
        "subset retains CFF2 outlines",
    );
    assert_eq!(
        subset_face.maxp().unwrap().num_glyphs,
        src_num_glyphs,
        "identity passthrough must preserve glyph count",
    );

    // CFF2 body must travel byte-identical.
    let src_cff2 = face.table_bytes(tag::CFF2).unwrap();
    let new_cff2 = subset_face.table_bytes(tag::CFF2).unwrap();
    assert_eq!(
        src_cff2, new_cff2,
        "CFF2 identity passthrough must preserve table bytes",
    );

    // Variable-font tables must travel byte-identical too.
    let src_fvar_bytes = face.table_bytes(tag::FVAR).unwrap();
    let new_fvar_bytes = subset_face.table_bytes(tag::FVAR).unwrap();
    assert_eq!(src_fvar_bytes, new_fvar_bytes, "fvar must pass through");
    let src_hvar_bytes = face.table_bytes(tag::HVAR).unwrap();
    let new_hvar_bytes = subset_face.table_bytes(tag::HVAR).unwrap();
    assert_eq!(src_hvar_bytes, new_hvar_bytes, "HVAR must pass through");

    // Default-instance advances survive (trivially — bytes are
    // identical, but we still check via the public APIs to prove the
    // re-parse hits the same numbers).
    let src_hmtx = face.hmtx().unwrap();
    let new_hmtx = subset_face.hmtx().unwrap();
    for (old, new) in &out.gid_map {
        let want = src_hmtx.advance(*old).unwrap_or(0);
        let got = new_hmtx.advance(*new).unwrap_or(0);
        assert_eq!(
            want, got,
            "CFF2 advance mismatch for gid {old}->{new}: src {want} vs subset {got}",
        );
    }

    // Variation: shape "ABCDE" through both source and subset at
    // default and at extreme coords; advances must match. This is the
    // assertion that makes the test exercise the `wght` axis, not
    // just the static instance.
    let src_fvar = face.fvar().unwrap().expect("source has fvar");
    let axes = src_fvar.axes();
    assert!(!axes.is_empty(), "source must have at least one axis");
    let max_user = axes[0].max_value;
    let default_user = axes[0].default_value;
    let coords_default = src_fvar.normalize_coords(&[default_user]);
    let coords_extreme = src_fvar.normalize_coords(&[max_user]);

    for &ch in &['A', 'B', 'C', 'D', 'E'] {
        let src_def = Font::new(face.clone(), 16.0).with_coords(&coords_default);
        let new_def = Font::new(subset_face.clone(), 16.0).with_coords(&coords_default);
        let (_, src_adv) = shape_one(&src_def, ch);
        let (_, new_adv) = shape_one(&new_def, ch);
        assert_eq!(
            src_adv, new_adv,
            "CFF2 {ch} default coords: src {src_adv} vs subset {new_adv}",
        );

        let src_max = Font::new(face.clone(), 16.0).with_coords(&coords_extreme);
        let new_max = Font::new(subset_face.clone(), 16.0).with_coords(&coords_extreme);
        let (_, src_adv_x) = shape_one(&src_max, ch);
        let (_, new_adv_x) = shape_one(&new_max, ch);
        assert_eq!(
            src_adv_x, new_adv_x,
            "CFF2 {ch} extreme coords: src {src_adv_x} vs subset {new_adv_x}",
        );
        let _ = (src_adv, src_adv_x);
    }

    // Sanity: at the extreme of the wght axis, at least one Latin
    // glyph's advance must differ from the default. If this fires,
    // either the fixture lost its variations or the shaper isn't
    // applying HVAR.
    let src_def = Font::new(face.clone(), 16.0).with_coords(&coords_default);
    let src_max = Font::new(face.clone(), 16.0).with_coords(&coords_extreme);
    let any_changed = ['A', 'B', 'C', 'D', 'E']
        .iter()
        .any(|&ch| shape_one(&src_def, ch).1 != shape_one(&src_max, ch).1);
    assert!(
        any_changed,
        "CFF2 fixture's wght axis is inert — fixture or shaper regression",
    );
}

#[test]
fn real_cff_cid_subset_round_trip() {
    // Hand-built CID-keyed CFF1 fixture (FDArray + FDSelect, with at
    // least one cross-FD shared subr to exercise #138). The build
    // script lives at tests/tools/build_cid_cff1_fixture.py.
    //
    // The fixture is a 2-FD CID font; cmap maps U+0041..U+0045 onto
    // five separate CID glyphs distributed across both FDs (gids 1..2
    // → FD 0, gids 3..5 → FD 1). One charstring in FD 0 invokes a
    // global subr; one charstring in FD 1 invokes its FD's local subr;
    // the round-trip exercises both per-FD Subr renumbering (#135) and
    // the cross-FD subroutine-keep-set machinery (#138).
    let face = Face::parse_bytes(CID_CFF1, 0).expect("CID source parses");
    assert!(
        face.record(tag::CFF1).is_some(),
        "CID fixture must carry CFF1 outlines",
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
    let out = subset(&face, &input).expect("CID CFF1 real subset succeeds");

    let subset_face = Face::parse_bytes(&out.bytes, 0).expect("CID subset re-parses");
    assert!(
        subset_face.record(tag::CFF1).is_some(),
        "subset retains CFF1 outlines",
    );

    let new_num_glyphs = subset_face.maxp().unwrap().num_glyphs;
    assert!(
        new_num_glyphs >= 6,
        "expected at least 6 glyphs in CID subset, got {new_num_glyphs}",
    );

    let src_hmtx = face.hmtx().unwrap();
    let new_hmtx = subset_face.hmtx().unwrap();
    for (old, new) in &out.gid_map {
        let want = src_hmtx.advance(*old).unwrap_or(0);
        let got = new_hmtx.advance(*new).unwrap_or(0);
        assert_eq!(
            want, got,
            "CID advance mismatch for gid {old}->{new}: src {want} vs subset {got}",
        );
    }

    let new_cmap = subset_face.cmap().unwrap();
    for &ch in &kept_chars {
        let new_gid = new_cmap
            .glyph_id(ch)
            .unwrap_or_else(|| panic!("CID subset cmap dropped {ch}"));
        assert!(new_gid > 0 && new_gid < new_num_glyphs);
    }

    // Shaping through the subset must produce gids in the *new*
    // namespace and advances matching the source's. This is the
    // assertion that proves the FDArray + FDSelect rebuild and the
    // cross-FD subroutine renumber landed glyphs that draw correctly
    // and advance correctly under the new gid namespace.
    let src_font = Font::new(face.clone(), 16.0);
    let subset_font = Font::new(subset_face.clone(), 16.0);
    for &ch in &kept_chars {
        let (_, src_adv) = shape_one(&src_font, ch);
        let (new_gid, new_adv) = shape_one(&subset_font, ch);
        assert_eq!(
            src_adv, new_adv,
            "CID {ch}: shaped advance differs source {src_adv} vs subset {new_adv}",
        );
        let expected_new_gid: u32 = new_cmap.glyph_id(ch).unwrap().into();
        assert_eq!(new_gid, expected_new_gid);
    }
}

#[test]
fn real_cff2_subset_non_identity_round_trip() {
    // Source Sans 3 VF, real Adobe CFF2 with single-FD elided
    // FDSelect. Subset to {A, B, C} on a non-identity gid map; the
    // CFF2 rewriter must synthesise an explicit FDSelect format 0
    // for the rebuild and re-emit charstrings + local subrs +
    // FDArray under the new gid namespace.
    let face = Face::parse_bytes(SOURCE_SANS_3_VF, 0).expect("CFF2 source parses");
    assert!(
        face.record(tag::CFF2).is_some(),
        "fixture must carry CFF2 outlines",
    );

    let kept_chars = ['A', 'B', 'C'];
    let kept_gids: Vec<u16> = kept_chars.iter().map(|&c| cmap_lookup(&face, c)).collect();

    let input = SubsetInput {
        gids: kept_gids.clone(),
        retain_hints: false,
        drop_unhandled: true,
        // Layout / variations off — the non-identity path drops
        // them today (matches the CFF1 non-identity flow).
        retain_layout: false,
        retain_variations: false,
    };
    let out = subset(&face, &input).expect("CFF2 non-identity subset succeeds");

    let subset_face = Face::parse_bytes(&out.bytes, 0).expect("CFF2 subset re-parses");
    assert!(
        subset_face.record(tag::CFF2).is_some(),
        "subset retains CFF2 outlines",
    );

    let new_num_glyphs = subset_face.maxp().unwrap().num_glyphs;
    assert!(
        (4..=8).contains(&new_num_glyphs),
        "expected 1 .notdef + ~3 kept glyphs, got {new_num_glyphs}",
    );

    // Size shrinkage (#167): subsetting Source Sans 3 VF to {A, B, C}
    // must shrink the CFF2 table now that unreachable per-FD locals +
    // globals are pruned. Pre-#167 the rewriter kept every source subr
    // verbatim; the table stayed ~equal to the source. The fixture is
    // already a heavily Latin-pre-subset CFF2 with a small VariationStore
    // and Top DICT overhead that doesn't shrink, so the threshold is
    // looser than the CFF1 case but still meaningful (subset must come
    // in under 80% of source).
    let src_cff_len = face.table_bytes(tag::CFF2).unwrap().len();
    let new_cff_len = subset_face.table_bytes(tag::CFF2).unwrap().len();
    assert!(
        new_cff_len * 5 < src_cff_len * 4,
        "CFF2 subset {new_cff_len} bytes should shrink to <80% of source {src_cff_len}",
    );

    // Every kept gid's advance survives the renumber.
    let src_hmtx = face.hmtx().unwrap();
    let new_hmtx = subset_face.hmtx().unwrap();
    for (old, new) in &out.gid_map {
        let want = src_hmtx.advance(*old).unwrap_or(0);
        let got = new_hmtx.advance(*new).unwrap_or(0);
        assert_eq!(
            want, got,
            "CFF2 non-identity advance mismatch for gid {old}->{new}: src {want} vs subset {got}",
        );
    }

    // Cmap consistency: each kept char resolves through the new cmap.
    let new_cmap = subset_face.cmap().unwrap();
    for &ch in &kept_chars {
        let new_gid = new_cmap
            .glyph_id(ch)
            .unwrap_or_else(|| panic!("CFF2 subset cmap dropped {ch}"));
        assert!(
            new_gid > 0 && new_gid < new_num_glyphs,
            "subset cmap of {ch} = {new_gid}, out of [1..{new_num_glyphs})",
        );
    }

    // End-to-end shaping: gids land in the new namespace and advances
    // (at default coords; variations are dropped on this path) match.
    let src_font = Font::new(face.clone(), 16.0);
    let subset_font = Font::new(subset_face.clone(), 16.0);
    for &ch in &kept_chars {
        let (_, src_adv) = shape_one(&src_font, ch);
        let (new_gid, new_adv) = shape_one(&subset_font, ch);
        assert_eq!(
            src_adv, new_adv,
            "CFF2 {ch}: shaped advance differs source {src_adv} vs subset {new_adv}",
        );
        let expected_new_gid: u32 = new_cmap.glyph_id(ch).unwrap().into();
        assert_eq!(new_gid, expected_new_gid);
    }
}
