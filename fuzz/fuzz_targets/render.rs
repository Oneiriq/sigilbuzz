//! Rasterizes glyphs from arbitrary font bytes through every rendering path:
//! outlines, COLRv0, COLRv1, SVG-in-OT, and embedded bitmaps.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sigilbuzz::Face;
use sigilbuzz_fuzz::{axis_coords, glyph_ids, split_control, Knobs};
use sigilbuzz_render::Rasterizer;

fuzz_target!(|data: &[u8]| {
    let (control, font) = split_control(data, 16);
    let mut knobs = Knobs::new(control);
    let Ok(face) = Face::parse_bytes(font, 0) else { return };

    let rast = Rasterizer::new();
    // Sizes stay small so each run is fast. Size-limit handling is covered
    // by the unit tests.
    let size = match knobs.byte() {
        0 => 0.0,
        1 => f32::NAN,
        b => f32::from(b % 48) + 1.0,
    };
    let palette = u16::from(knobs.byte() % 4);
    let coords = axis_coords(&face, &mut knobs);
    for gid in glyph_ids(&face, &mut knobs).into_iter().take(12) {
        let _ = rast.rasterize_glyph(&face, gid, size, &coords);
        let _ = rast.rasterize_colrv0_glyph(&face, gid, palette, size, &coords);
        let _ = rast.rasterize_colrv1_glyph(&face, gid, palette, size, &coords);
        let _ = rast.rasterize_svg_glyph(&face, gid, size, &coords);
        let _ = rast.rasterize_bitmap_glyph(&face, gid, size, &coords);
    }
});
