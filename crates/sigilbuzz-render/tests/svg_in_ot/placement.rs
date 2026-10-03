//! Placement of SVG-in-OT glyph images relative to the glyph origin.

use sigilbuzz::Face;
use sigilbuzz_render::{Placement, Rasterizer};

use crate::fonts::build_svg_font;

/// A glyph drawn the OpenType way: user space in design units, the
/// glyph origin at user-space `(0, 0)`, and y running down, so the ink
/// above the baseline has negative y. The viewBox shows the em square
/// above the baseline, shifted by `x` units.
fn above_baseline_doc(x: i32) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{x} -1000 1000 1000\">\
         <rect x=\"100\" y=\"-500\" width=\"200\" height=\"500\" fill=\"#000\"/></svg>"
    )
}

#[test]
fn placement_is_the_view_box_corner() {
    let font = build_svg_font(above_baseline_doc(0).as_bytes());
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let (pix, at) = rast
        .rasterize_svg_glyph_placed(&face, 1, 100.0, &[])
        .unwrap();
    assert_eq!(pix, rast.rasterize_svg_glyph(&face, 1, 100.0, &[]).unwrap());
    // 0.1 pixel per unit: the viewBox corner (0, -1000) is 100 pixels
    // above the origin.
    assert_eq!(at, Placement::new(0, -100));
    assert_eq!((pix.width, pix.height), (100, 100));
    // The rectangle covers device pixels x 10..30, y -50..0.
    let alpha = |x: i32, y: i32| pix.get((x - at.left) as u32, (y - at.top) as u32)[3];
    assert_eq!(alpha(10, -50), 255);
    assert_eq!(alpha(29, -1), 255);
    assert_eq!(alpha(9, -25), 0);
    assert_eq!(alpha(30, -25), 0);
    assert_eq!(alpha(20, -51), 0);

    // The text-path entry point places the canvas the same way.
    let placed = rast
        .rasterize_svg_glyph_with_text_paths_placed(&face, 1, 100.0, &[], &[])
        .unwrap();
    assert_eq!(placed, (pix, at));
}

#[test]
fn placement_rounds_a_fractional_view_box_corner() {
    // x = -25 units is -2.5 pixels, which rounds away from zero.
    let font = build_svg_font(above_baseline_doc(-25).as_bytes());
    let face = Face::parse_bytes(&font, 0).unwrap();
    let (_, at) = Rasterizer::new()
        .rasterize_svg_glyph_placed(&face, 1, 100.0, &[])
        .unwrap();
    assert_eq!(at, Placement::new(-3, -100));
}
