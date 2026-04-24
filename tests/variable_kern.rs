//! Variable-font kerning: the (A, V) pair in `var_kern.ttf` is
//! decorated with a GPOS VariationIndex that pulls a -100 advance
//! delta off a shared ItemVariationStore at wght=900 and zero at
//! the default wght=400.
//!
//! This is the integration cover for issue #13 — before the GPOS
//! feature-variations wiring, sigilbuzz parsed the VariationIndex
//! offsets but threw them away, so kerning was frozen at the
//! default instance. With the fix, the "AV" advance shifts by -100
//! when the caller binds the wght axis to 900, and smaller amounts
//! in between.
//!
//! The fixture is a hand-built 972-byte TTF produced by
//! `tests/tools/build_var_kern_fixture.py`. It packs exactly what
//! the bug fix needs to exercise — one axis, one variation region,
//! one kern pair — without the megabyte of overhead a real variable
//! font would cost.
//!
//! # Why no byte-for-byte rustybuzz parity here
//!
//! The synthetic font relies on PairPos format 1 Device offsets
//! being measured from the PairPos subtable start, which is the
//! OpenType spec's rule. rustybuzz 0.20 (via ttf-parser) resolves
//! those offsets against the enclosing PairSet instead, which
//! means it cannot find the VariationIndex in this fixture and
//! silently drops the delta — rustybuzz returns the default-
//! instance advance regardless of the bound axis. Matching that
//! would mean replicating an upstream bug. The `rubik_vf.ttf`
//! fixture in `variable_fonts.rs` still covers HVAR parity with
//! rustybuzz against a real variable font.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

/// Built by `tests/tools/build_var_kern_fixture.py`. A 972-byte
/// TrueType font with two glyphs ("A" and "V"), one `wght` axis,
/// and a GPOS kern pair whose x_advance delta is -100 at wght=900
/// and 0 at wght=400 via a VariationIndex / ItemVariationStore pair.
const VAR_KERN: &[u8] = include_bytes!("fixtures/var_kern.ttf");

fn normalize_wght(face: &Face<'_>, user_value: f32) -> Vec<f32> {
    let fvar = face.fvar().unwrap().expect("synthetic font has fvar");
    let avar = face.avar().unwrap();
    let normalized = fvar.normalize_coords(&[user_value]);
    match avar {
        Some(a) => a.remap_all(&normalized),
        None => normalized,
    }
}

fn sigilbuzz_advances(coords: &[f32], text: &str) -> Vec<i32> {
    let blob = Blob::new(VAR_KERN);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0).with_coords(coords);
    let mut buf = Buffer::new();
    buf.push_str(text);
    let shaped = shape(&font, &buf, &[]).unwrap();
    shaped.glyphs.iter().map(|g| g.x_advance).collect()
}

#[test]
fn default_instance_kern_delta_is_zero() {
    // At the default wght (400) the VariationIndex resolves to a
    // zero delta, so "AV" emits the same advances as the bare
    // hmtx: 500 for A, 500 for V.
    let advances = sigilbuzz_advances(&[], "AV");
    assert_eq!(advances.len(), 2);
    assert_eq!(advances[0], 500, "A at default wght");
    assert_eq!(advances[1], 500, "V at default wght");
}

#[test]
fn heavy_weight_tightens_av_pair_by_a_hundred_units() {
    // At wght = 900 the variation region peaks → delta = -100
    // lands on the first glyph's x_advance.
    let blob = Blob::new(VAR_KERN);
    let face = Face::parse(&blob, 0).unwrap();
    let coords = normalize_wght(&face, 900.0);
    let advances = sigilbuzz_advances(&coords, "AV");
    assert_eq!(advances.len(), 2);
    assert_eq!(advances[0], 400, "A at wght=900 gets the -100 kern delta");
    assert_eq!(advances[1], 500, "V unchanged (valueFormat2 = empty)");
}

#[test]
fn halfway_axis_coord_gives_halfway_delta() {
    // The variation region is (start=0, peak=1, end=1) so at a
    // normalized coord of 0.5 the scalar is 0.5 and the delta is
    // -50. wght 400 .. 900 user-space → halfway is 650.
    let blob = Blob::new(VAR_KERN);
    let face = Face::parse(&blob, 0).unwrap();
    let coords = normalize_wght(&face, 650.0);
    let advances = sigilbuzz_advances(&coords, "AV");
    assert_eq!(advances[0], 450, "halfway on wght pulls half the delta");
}

#[test]
fn advance_delta_scales_monotonically_across_axis() {
    // Sanity: as the wght coordinate grows, the kern tightens. Any
    // regression to "kern frozen at default" would make every
    // sample return the same advance.
    let blob = Blob::new(VAR_KERN);
    let face = Face::parse(&blob, 0).unwrap();
    let sample = |w: f32| {
        let coords = normalize_wght(&face, w);
        sigilbuzz_advances(&coords, "AV")[0]
    };
    let a0 = sample(400.0);
    let a1 = sample(550.0);
    let a2 = sample(700.0);
    let a3 = sample(900.0);
    assert_eq!(a0, 500);
    assert!(a1 < a0, "wght 550 should tighten vs default");
    assert!(a2 < a1, "wght 700 should tighten further");
    assert!(a3 < a2, "wght 900 should tighten the most");
    assert_eq!(a3, 400);
}
