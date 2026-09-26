//! GPOS round trips: Rubik and Amiri positioning, chained context, and
//! the Open Sans PairPos format 2 class collapse.

use super::*;

#[test]
fn rubik_latin_subset_retains_gpos() {
    // Rubik VF carries pair-pos (type 2 fmt 1/2), single-adj (type 1
    // wrapped in extension), mark-base / mark-liga / mark-mark, and
    // an extension-wrapped pair-pos. With a broad Latin subset the
    // pair-pos lookups retain at least one (first, second) pair so
    // the rewriter emits a parseable GPOS table.
    let face = rubik_face();
    if face.gpos().ok().flatten().is_none() {
        return;
    }
    let cmap = face.cmap().unwrap();
    let mut gids: Vec<u16> = Vec::new();
    for c in 'A'..='Z' {
        if let Some(g) = cmap.glyph_id(c) {
            gids.push(g);
        }
    }
    for c in 'a'..='z' {
        if let Some(g) = cmap.glyph_id(c) {
            gids.push(g);
        }
    }
    if gids.is_empty() {
        return;
    }
    let input = SubsetInput {
        gids,
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    assert!(
        subset_face.record(tag::GPOS).is_some(),
        "GPOS must survive a Latin-letter subset of Rubik: \
         the pair-pos lookup retains at least one (first, second) pair",
    );
    // The rewritten GPOS must parse cleanly.
    let parsed = subset_face.gpos();
    assert!(
        parsed.is_ok(),
        "rewritten GPOS must parse: {:?}",
        parsed.err()
    );
}

#[test]
fn rubik_latin_subset_gpos_is_byte_deterministic() {
    // Determinism guard for GPOS rewrites.
    let face = rubik_face();
    if face.gpos().ok().flatten().is_none() {
        return;
    }
    let cmap = face.cmap().unwrap();
    let mut gids: Vec<u16> = Vec::new();
    for c in 'A'..='Z' {
        if let Some(g) = cmap.glyph_id(c) {
            gids.push(g);
        }
    }
    for c in 'a'..='z' {
        if let Some(g) = cmap.glyph_id(c) {
            gids.push(g);
        }
    }
    if gids.is_empty() {
        return;
    }
    let input = SubsetInput {
        gids,
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let a = subset(&face, &input).unwrap();
    let b = subset(&face, &input).unwrap();
    assert_eq!(
        a.bytes, b.bytes,
        "GPOS-bearing subset must be byte-deterministic"
    );
}

#[test]
fn amiri_subset_gpos_round_trips() {
    // Amiri carries mark-to-base / mark-to-mark GPOS lookups. A
    // subset that keeps base glyphs must retain GPOS after the
    // rewriter ships, so the marks the closure pulls in still attach
    // correctly. We only check that GPOS parses post-subset; the
    // attachment math is covered by the unit-test fixtures.
    let face = amiri_face();
    if face.gpos().ok().flatten().is_none() {
        return;
    }
    // Pick a chunk of Arabic letters.
    let chars = [
        '\u{0627}', '\u{0628}', '\u{062A}', '\u{062B}', '\u{062C}', '\u{062D}', '\u{062E}',
        '\u{062F}',
    ];
    let cmap = face.cmap().unwrap();
    let mut gids: Vec<u16> = Vec::new();
    for ch in chars {
        if let Some(gid) = cmap.glyph_id(ch) {
            gids.push(gid);
        }
    }
    if gids.is_empty() {
        return;
    }
    let input = SubsetInput {
        gids,
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    if subset_face.record(tag::GPOS).is_some() {
        let parsed = subset_face.gpos();
        assert!(
            parsed.is_ok(),
            "rewritten Amiri GPOS must parse: {:?}",
            parsed.err()
        );
    }
}

// ===== GPOS type 7 / 8 + PairPos fmt 2 round-trip coverage =====
//
// The unit tests in `gpos.rs` cover the per-format byte shape with
// hand-built fixtures. These integration cases exercise the full
// subset -> reparse pipeline against real fonts so the two-phase GPOS
// driver and the PairPos fmt-1 fallback see realistic input.

#[test]
fn amiri_subset_keeps_gpos_with_chain_context_lookups() {
    // Amiri ships GPOS chained-context lookups for `mark` / `mkmk`
    // positioning of Arabic combining marks. A subset that retains a
    // common Arabic letter exercises the type-8 chained-context
    // rewriter and the GPOS two-phase driver.
    let face = amiri_face();
    if face.gpos().ok().flatten().is_none() {
        return;
    }
    let chars = ['\u{0627}', '\u{0644}', '\u{0645}', '\u{062F}'];
    let cmap = face.cmap().unwrap();
    let mut gids: Vec<u16> = Vec::new();
    for ch in chars {
        if let Some(gid) = cmap.glyph_id(ch) {
            gids.push(gid);
        }
    }
    if gids.is_empty() {
        return;
    }
    let input = SubsetInput {
        gids,
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    if subset_face.record(tag::GPOS).is_some() {
        let parsed = subset_face.gpos();
        assert!(
            parsed.is_ok(),
            "rewritten Amiri GPOS must parse: {:?}",
            parsed.err()
        );
    }
}

#[test]
fn amiri_subset_gpos_round_trip_is_byte_deterministic() {
    let face = amiri_face();
    if face.gpos().ok().flatten().is_none() {
        return;
    }
    let chars = ['\u{0627}', '\u{0644}', '\u{0645}', '\u{062F}'];
    let cmap = face.cmap().unwrap();
    let mut gids: Vec<u16> = Vec::new();
    for ch in chars {
        if let Some(gid) = cmap.glyph_id(ch) {
            gids.push(gid);
        }
    }
    if gids.is_empty() {
        return;
    }
    let input = SubsetInput {
        gids,
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let a = subset(&face, &input).unwrap();
    let b = subset(&face, &input).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn open_sans_av_subset_pairpos_fmt2_class_collapse_round_trips() {
    // Open Sans's `kern` lookup is a PairPos fmt-2 class matrix. A
    // subset down to {A, V} forces the small-subset fmt-1 fallback
    // path: the surviving first x second cross-product is two cells,
    // well under the heuristic's 256-budget. The test only asserts
    // the rewritten GPOS parses. The AV pair is the most-kerned
    // Latin pair, so any class-collapse breakage would surface here.
    let face = open_sans_face();
    let cmap = face.cmap().unwrap();
    let Some(gid_a) = cmap.glyph_id('A') else {
        return;
    };
    let Some(gid_v) = cmap.glyph_id('V') else {
        return;
    };
    let input = SubsetInput {
        gids: vec![gid_a, gid_v],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    // GPOS may or may not survive depending on which lookups carry
    // (A, V) kerning in this build of Open Sans; what matters is that
    // when it survives, the rewritten table parses cleanly.
    if subset_face.record(tag::GPOS).is_some() {
        let parsed = subset_face.gpos();
        assert!(
            parsed.is_ok(),
            "rewritten Open Sans GPOS for {{A, V}} must parse: {:?}",
            parsed.err()
        );
    }
}

#[test]
fn open_sans_av_subset_is_byte_deterministic() {
    // Determinism guard for the PairPos fmt-2 -> fmt-1 fallback.
    let face = open_sans_face();
    let cmap = face.cmap().unwrap();
    let Some(gid_a) = cmap.glyph_id('A') else {
        return;
    };
    let Some(gid_v) = cmap.glyph_id('V') else {
        return;
    };
    let input = SubsetInput {
        gids: vec![gid_a, gid_v],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let a = subset(&face, &input).unwrap();
    let b = subset(&face, &input).unwrap();
    assert_eq!(a.bytes, b.bytes);
}
