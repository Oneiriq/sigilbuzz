//! Integration tests for the rasterizer.
//!
//! Uses bundled fonts from the workspace `tests/` tree:
//!   - Open Sans (TrueType, no variations) for basic outline raster.
//!   - Rubik Variable (`wght` axis) for variable-font coord routing.
//!
//! COLRv0 is exercised against an in-process synthetic SFNT that
//! ships exactly the tables the rasterizer needs (head, maxp, hhea,
//! hmtx, loca, glyf, COLR, CPAL).

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::{Pixmap, Rasterizer};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");
const SOURCE_SANS_VF: &[u8] = include_bytes!("../../../tests/fonts/SourceSans3VF-Latin-Subset.otf");
const SOURCE_CODE_PRO: &[u8] =
    include_bytes!("../../../tests/fonts/SourceCodePro-Latin-Subset.otf");

fn count_lit(p: &Pixmap) -> usize {
    p.data.iter().filter(|&&a| a > 0).count()
}

fn count_partial(p: &Pixmap) -> usize {
    p.data.iter().filter(|&&a| a > 0 && a < 255).count()
}

fn glyph_for(face: &Face<'_>, ch: char) -> u16 {
    face.cmap()
        .unwrap()
        .glyph_id(ch)
        .unwrap_or_else(|| panic!("missing cmap mapping for {ch:?}"))
}

#[test]
fn open_sans_a_rasterizes_at_three_sizes() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let gid = glyph_for(&face, 'A');
    let rast = Rasterizer::new();
    let mut prev_lit = 0usize;
    for &size in &[24.0, 48.0, 96.0] {
        let pix = rast.rasterize_glyph(&face, gid, size, &[]).unwrap();
        assert!(pix.width > 0 && pix.height > 0, "size {size}: empty pixmap");
        let lit = count_lit(&pix);
        assert!(lit > 0, "size {size}: no pixels lit");
        let partial = count_partial(&pix);
        assert!(
            partial > 0,
            "size {size}: no anti-aliased pixels (partial = 0)"
        );
        // Coarse: bigger renders should generally light more pixels.
        assert!(
            lit >= prev_lit,
            "size {size}: pixel count regressed ({lit} < {prev_lit})"
        );
        prev_lit = lit;

        // The pixmap should roughly cover the glyph; check the
        // bounding box ratio is ballpark-square or wider, since 'A'
        // is shaped like an A.
        assert!(pix.width as f32 >= 0.5 * size && pix.width as f32 <= 2.5 * size);
        assert!(pix.height as f32 >= 0.5 * size && pix.height as f32 <= 2.5 * size);
    }
}

#[test]
fn open_sans_rasterization_is_deterministic() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let gid = glyph_for(&face, 'A');
    let rast = Rasterizer::new();
    let a = rast.rasterize_glyph(&face, gid, 32.0, &[]).unwrap();
    let b = rast.rasterize_glyph(&face, gid, 32.0, &[]).unwrap();
    assert_eq!(a, b, "same input must yield byte-identical pixmap");
}

#[test]
fn whitespace_glyph_returns_no_outline_error() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let gid = glyph_for(&face, ' ');
    let rast = Rasterizer::new();
    assert!(rast.rasterize_glyph(&face, gid, 24.0, &[]).is_err());
}

#[test]
fn bad_size_is_rejected() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let gid = glyph_for(&face, 'A');
    let rast = Rasterizer::new();
    assert!(rast.rasterize_glyph(&face, gid, 0.0, &[]).is_err());
    assert!(rast.rasterize_glyph(&face, gid, -1.0, &[]).is_err());
    assert!(rast.rasterize_glyph(&face, gid, f32::NAN, &[]).is_err());
    assert!(rast
        .rasterize_glyph(&face, gid, f32::INFINITY, &[])
        .is_err());
}

#[test]
fn source_sans_vf_cff2_a_rasterizes_with_lit_pixels() {
    // Regression for issue #209: before the CFF2 INDEX-count fix, the
    // CharStrings INDEX was parsed as zero-length and Face::glyph_outline
    // returned Ok(None), which surfaced here as RenderError::NoOutline.
    let blob = Blob::new(SOURCE_SANS_VF);
    let face = Face::parse(&blob, 0).unwrap();
    let gid = glyph_for(&face, 'A');
    let rast = Rasterizer::new();
    let pix = rast.rasterize_glyph(&face, gid, 48.0, &[]).unwrap();
    assert!(pix.width > 0 && pix.height > 0, "empty pixmap");
    assert!(count_lit(&pix) > 0, "no pixels lit on Source Sans 3 VF 'A'");
    assert!(count_partial(&pix) > 0, "no anti-aliased pixels");
}

#[test]
fn source_code_pro_cff1_a_rasterizes_with_lit_pixels() {
    // Coverage for the static-CFF1 path. Same outline plumbing:
    // ensures the fix to read_index didn't break u16 INDEX parsing.
    let blob = Blob::new(SOURCE_CODE_PRO);
    let face = Face::parse(&blob, 0).unwrap();
    let gid = glyph_for(&face, 'A');
    let rast = Rasterizer::new();
    let pix = rast.rasterize_glyph(&face, gid, 48.0, &[]).unwrap();
    assert!(count_lit(&pix) > 0, "no pixels lit on Source Code Pro 'A'");
}

#[test]
fn rubik_variable_heavy_weight_lights_more_pixels() {
    // wght=900 should produce a heavier 'A' than the default
    // wght=400; the lit-pixel count grows because thicker strokes
    // cover more area.
    let blob = Blob::new(RUBIK);
    let face = Face::parse(&blob, 0).unwrap();
    let gid = glyph_for(&face, 'A');
    let rast = Rasterizer::new();

    let default = rast.rasterize_glyph(&face, gid, 64.0, &[]).unwrap();
    let fvar = face.fvar().unwrap().expect("rubik has fvar");
    let avar = face.avar().unwrap();
    let norm = fvar.normalize_coords(&[900.0]);
    let heavy_coords = match avar {
        Some(a) => a.remap_all(&norm),
        None => norm,
    };
    let heavy = rast
        .rasterize_glyph(&face, gid, 64.0, &heavy_coords)
        .unwrap();

    let lit_default = count_lit(&default);
    let lit_heavy = count_lit(&heavy);
    assert!(
        lit_heavy > lit_default,
        "wght=900 should light more pixels than default (heavy={lit_heavy} default={lit_default})"
    );
}
