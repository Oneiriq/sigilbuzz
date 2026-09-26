//! Feeds glyphs from arbitrary font bytes to the output crates: COLRv1 paint
//! evaluation, SVG, PDF font emitters, and the GPU (Slug) encoder.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sigilbuzz::Face;
use sigilbuzz_fuzz::{axis_coords, glyph_ids, split_control, Knobs};
use sigilbuzz_gpu::{encode_glyph, encode_glyph_at_coords, SlugOptions};
use sigilbuzz_paint::{evaluate, evaluate_at_coords};
use sigilbuzz_pdf::{emit_otf_embedded_font, emit_type1_font, emit_type3_font};
use sigilbuzz_svg::{glyph_to_svg, glyph_to_svg_at_coords, glyph_to_svg_color};

fuzz_target!(|data: &[u8]| {
    let (control, font) = split_control(data, 16);
    let mut knobs = Knobs::new(control);
    let Ok(face) = Face::parse_bytes(font, 0) else { return };

    let coords = axis_coords(&face, &mut knobs);
    let gids: Vec<u16> = glyph_ids(&face, &mut knobs).into_iter().take(12).collect();
    let opts = SlugOptions::default();
    for &gid in &gids {
        let _ = evaluate(&face, gid);
        let _ = evaluate_at_coords(&face, gid, &coords);
        let _ = glyph_to_svg(&face, gid);
        let _ = glyph_to_svg_at_coords(&face, gid, &coords);
        let _ = glyph_to_svg_color(&face, gid);
        let _ = encode_glyph(&face, gid, &opts);
        let _ = encode_glyph_at_coords(&face, gid, &coords, &opts);
    }
    let _ = emit_type3_font(&face, &gids);
    let _ = emit_type1_font(&face, &gids);
    let _ = emit_otf_embedded_font(&face, font, &gids);
});
