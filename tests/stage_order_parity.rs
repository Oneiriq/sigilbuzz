//! The GSUB stages of the default and Arabic shapers, checked against
//! HarfBuzz.
//!
//! HarfBuzz's default, Hebrew and Thai shapers run the default
//! features, the direction features and the caller's features in one
//! stage (`hb_ot_shape_collect_features` in `hb-ot-shape.cc`), so their
//! lookups apply in lookup-index order whatever feature they belong
//! to. Its Arabic shaper runs `isol`, `fina`, `medi` and `init`, then
//! `rlig`, then `calt`, then `liga`, `clig`, `mset` and the rest in
//! stages of their own (`collect_features_arabic`), and vertical
//! Arabic takes the default shaper.
//!
//! `fixtures/stage_order.ttf` (built by
//! `tests/tools/build_stage_order_fixture.py`) has one substitution
//! per lookup, interleaved across features. Every expectation is
//! HarfBuzz 14.5.0's output (through uharfbuzz 0.56.2) at
//! `HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES`: glyph id and cluster
//! (a UTF-8 byte offset).

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Feature, Font};

const STAGE_ORDER: &[u8] = include_bytes!("fixtures/stage_order.ttf");

/// Glyph id and cluster of `text` shaped with the stage fixture.
fn glyphs(text: &str, direction: Direction, features: &[Feature]) -> Vec<(u32, u32)> {
    let blob = Blob::new(STAGE_ORDER);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
    let run = shape(&font, &buffer, features).expect("shape");
    run.glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect()
}

#[test]
fn default_features_apply_in_lookup_order() {
    // `liga` (lookup 0) forms a_b before `ccmp` (lookup 1) can turn the
    // a into a.ccmp.
    assert_eq!(glyphs("ab", Direction::Ltr, &[]), [(6, 0)]);
    // `ccmp` (lookup 6) turns f into f.c before `ltra` (lookup 7).
    assert_eq!(glyphs("f", Direction::Ltr, &[]), [(12, 0)]);
    // `vert` (lookup 4) runs before `ccmp` (lookup 5), which only
    // covers the vertical form.
    assert_eq!(glyphs("e", Direction::Ttb, &[]), [(11, 0)]);
}

#[test]
fn caller_features_share_the_default_stage() {
    // `smcp` (lookup 2) gets to c before `calt` (lookup 3).
    let smcp = Feature {
        tag: *b"smcp",
        value: 1,
    };
    assert_eq!(glyphs("c", Direction::Ltr, &[smcp]), [(8, 0)]);
    assert_eq!(glyphs("c", Direction::Ltr, &[]), [(9, 0)]);
}

#[test]
fn arabic_runs_its_features_in_harfbuzz_stages() {
    // `fina` runs before `init`, whose contextual rule wants the final
    // form after it.
    assert_eq!(
        glyphs("\u{0628}\u{0628}", Direction::Rtl, &[]),
        [(15, 2), (17, 0)]
    );
    // `calt` runs in a stage before `liga`, so the lam takes its
    // contextual form and the lam-alef ligature no longer matches.
    assert_eq!(
        glyphs("\u{0644}\u{0627}", Direction::Rtl, &[]),
        [(22, 2), (20, 0)]
    );
    // The Arabic shaper turns `mset` on.
    assert_eq!(glyphs("\u{0627}", Direction::Rtl, &[]), [(23, 0)]);
}

#[test]
fn vertical_arabic_takes_the_default_shaper() {
    // No joining forms and no `mset` in vertical text.
    assert_eq!(
        glyphs("\u{0628}\u{0628}", Direction::Ttb, &[]),
        [(14, 0), (14, 2)]
    );
    assert_eq!(glyphs("\u{0627}", Direction::Ttb, &[]), [(21, 0)]);
}
