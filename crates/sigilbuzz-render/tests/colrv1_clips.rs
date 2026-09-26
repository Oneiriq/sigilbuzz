//! COLRv1 rendering follows HarfBuzz's paint model.
//!
//! - A transform below a `PaintGlyph` moves the fill, never the glyph
//!   outline that clips it.
//! - `PaintComposite` blends isolated source and backdrop groups, so
//!   whatever was painted before the composite is left alone.
//! - The glyph is clipped to its ClipList box, a referenced glyph
//!   (`PaintColrGlyph`) to its own box, and an unbounded glyph renders
//!   empty.
//! - Gradients are exact under any transform: a radial gradient under
//!   a non-uniform scale is an ellipse.
//!
//! The font has two outlines at 1000 units per em, rendered at 100
//! pixels per em (0.1 pixel per unit): gid 1 is a 200-unit square and
//! gid 2 a 400 by 200 rectangle.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::{ColorPixmap, Rasterizer};

// =========================================================================
// Fixture
// =========================================================================

fn f2dot14(v: f32) -> [u8; 2] {
    ((v * 16384.0).round() as i16).to_be_bytes()
}

fn words(values: &[i16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

fn set_offset24(p: &mut [u8], at: usize, target: usize) {
    let v = target as u32;
    p[at..at + 3].copy_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
}

fn parent(mut head: Vec<u8>, child: &[u8]) -> Vec<u8> {
    let at = head.len();
    set_offset24(&mut head, 1, at);
    head.extend_from_slice(child);
    head
}

fn solid(entry: u16) -> Vec<u8> {
    let mut p = vec![2u8];
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(1.0));
    p
}

fn glyph(gid: u16, child: &[u8]) -> Vec<u8> {
    let mut head = vec![10u8, 0, 0, 0];
    head.extend_from_slice(&gid.to_be_bytes());
    parent(head, child)
}

fn colr_glyph(gid: u16) -> Vec<u8> {
    let mut p = vec![11u8];
    p.extend_from_slice(&gid.to_be_bytes());
    p
}

fn scale(sx: f32, sy: f32, child: &[u8]) -> Vec<u8> {
    let mut head = vec![16u8, 0, 0, 0];
    head.extend_from_slice(&f2dot14(sx));
    head.extend_from_slice(&f2dot14(sy));
    parent(head, child)
}

/// PaintTransform (format 12) applying the 2x3 matrix `m`
/// (`xx, yx, xy, yy, dx, dy`) to `child`.
fn transform(m: [f32; 6], child: &[u8]) -> Vec<u8> {
    let mut p = vec![12u8, 0, 0, 0, 0, 0, 0];
    let at = p.len();
    set_offset24(&mut p, 4, at);
    for v in m {
        p.extend_from_slice(&((v * 65536.0).round() as i32).to_be_bytes());
    }
    let at = p.len();
    set_offset24(&mut p, 1, at);
    p.extend_from_slice(child);
    p
}

fn composite(source: &[u8], mode: u8, backdrop: &[u8]) -> Vec<u8> {
    let mut p = vec![32u8, 0, 0, 0, mode, 0, 0, 0];
    let at = p.len();
    set_offset24(&mut p, 1, at);
    p.extend_from_slice(source);
    let at = p.len();
    set_offset24(&mut p, 5, at);
    p.extend_from_slice(backdrop);
    p
}

/// Red (entry 0) to blue (entry 2) color line, pad.
fn red_to_blue() -> Vec<u8> {
    let mut p = vec![0u8];
    p.extend_from_slice(&2u16.to_be_bytes());
    for (offset, entry) in [(0.0, 0u16), (1.0, 2)] {
        p.extend_from_slice(&f2dot14(offset));
        p.extend_from_slice(&entry.to_be_bytes());
        p.extend_from_slice(&f2dot14(1.0));
    }
    p
}

fn linear(points: [i16; 6]) -> Vec<u8> {
    let mut p = vec![4u8, 0, 0, 0];
    p.extend(words(&points));
    parent(p, &red_to_blue())
}

fn radial(c: (i16, i16), r1: u16) -> Vec<u8> {
    let mut p = vec![6u8, 0, 0, 0];
    p.extend(words(&[c.0, c.1]));
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend(words(&[c.0, c.1]));
    p.extend_from_slice(&r1.to_be_bytes());
    parent(p, &red_to_blue())
}

fn colr_layers(count: u8, first: u32) -> Vec<u8> {
    let mut p = vec![1u8, count];
    p.extend_from_slice(&first.to_be_bytes());
    p
}

/// COLR v1: base glyph paints, a LayerList, and ClipList records
/// `(first gid, last gid, [x_min, y_min, x_max, y_max])`.
fn colr(paints: &[(u16, Vec<u8>)], layers: &[Vec<u8>], clips: &[(u16, u16, [i16; 4])]) -> Vec<u8> {
    let header_len = 34usize;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    let layer_slot = out.len();
    out.extend_from_slice(&[0; 16]); // layers, clips, index map, store
    out.extend_from_slice(&(paints.len() as u32).to_be_bytes());
    let records = out.len();
    for (gid, _) in paints {
        out.extend_from_slice(&gid.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    for (i, (_, bytes)) in paints.iter().enumerate() {
        let rel = (out.len() - header_len) as u32;
        out[records + i * 6 + 2..records + i * 6 + 6].copy_from_slice(&rel.to_be_bytes());
        out.extend_from_slice(bytes);
    }
    let at = out.len() as u32;
    out[layer_slot..layer_slot + 4].copy_from_slice(&at.to_be_bytes());
    out.extend_from_slice(&(layers.len() as u32).to_be_bytes());
    let mut rel = 4 + 4 * layers.len();
    for layer in layers {
        out.extend_from_slice(&(rel as u32).to_be_bytes());
        rel += layer.len();
    }
    for layer in layers {
        out.extend_from_slice(layer);
    }
    let at = out.len() as u32;
    out[layer_slot + 4..layer_slot + 8].copy_from_slice(&at.to_be_bytes());
    out.push(1);
    out.extend_from_slice(&(clips.len() as u32).to_be_bytes());
    for (i, (first, last, _)) in clips.iter().enumerate() {
        out.extend_from_slice(&first.to_be_bytes());
        out.extend_from_slice(&last.to_be_bytes());
        let box_at = (5 + 7 * clips.len() + 9 * i) as u32;
        out.extend_from_slice(&box_at.to_be_bytes()[1..]);
    }
    for (_, _, coords) in clips {
        out.push(1);
        out.extend(words(coords));
    }
    out
}

fn rect_glyph(x1: i16, y1: i16) -> Vec<u8> {
    let mut g = words(&[1, 0, 0, x1, y1]);
    g.extend(words(&[3, 0]));
    g.extend_from_slice(&[0x01; 4]);
    g.extend(words(&[0, x1, 0, -x1]));
    g.extend(words(&[0, 0, y1, 0]));
    g
}

fn font() -> Vec<u8> {
    let (square, wide) = (rect_glyph(200, 200), rect_glyph(400, 200));
    let mut glyf = square.clone();
    glyf.extend_from_slice(&wide);
    let mut loca = Vec::new();
    for off in [0, 0, square.len(), square.len() + wide.len()] {
        loca.extend_from_slice(&((off / 2) as u16).to_be_bytes());
    }
    let mut head = Vec::new();
    head.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    head.extend_from_slice(&[0; 8]);
    head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head.extend(words(&[0, 1000]));
    head.extend_from_slice(&[0; 16]);
    head.extend(words(&[0, 0, 400, 200, 0, 8, 2, 0, 0]));
    let mut hhea = 0x0001_0000u32.to_be_bytes().to_vec();
    hhea.extend(words(&[
        800, -200, 0, 500, 0, 0, 400, 1, 0, 0, 0, 0, 0, 0, 0, 3,
    ]));
    let mut maxp = 0x0000_5000u32.to_be_bytes().to_vec();
    maxp.extend(words(&[3]));
    let hmtx = words(&[500, 0, 500, 0, 500, 0]);
    let cpal = {
        let mut c = words(&[0, 3, 1, 3]);
        c.extend_from_slice(&14u32.to_be_bytes());
        c.extend(words(&[0]));
        for (r, g, b) in [(255u8, 0u8, 0u8), (0, 255, 0), (0, 0, 255)] {
            c.extend_from_slice(&[b, g, r, 255]);
        }
        c
    };
    let paints = [
        // A scale below the clip: the fill doubles, the square stays.
        (
            10,
            glyph(1, &scale(2.0, 2.0, &linear([0, 0, 200, 0, 0, 200]))),
        ),
        (11, glyph(1, &linear([0, 0, 200, 0, 0, 200]))),
        // Green wide glyph, then blue kept only inside a red square.
        (12, colr_layers(2, 0)),
        (13, glyph(1, &solid(0))),
        (14, colr_glyph(15)),
        (15, glyph(1, &solid(0))),
        (16, solid(0)),
        (17, solid(0)),
        // A radial under a 2 by 1 scale: an ellipse.
        (18, glyph(2, &scale(2.0, 1.0, &radial((100, 100), 100)))),
        // The square scaled 1000 times, far past the canvas.
        (
            19,
            transform([1000.0, 0.0, 0.0, 1000.0, 0.0, 0.0], &glyph(1, &solid(0))),
        ),
        // A composite whose source and backdrop are itself.
        (20, vec![32u8, 0, 0, 0, 3, 0, 0, 0]),
    ];
    let layers = [
        glyph(2, &solid(1)),
        composite(&glyph(1, &solid(2)), 5, &glyph(1, &solid(0))),
    ];
    let clips = [
        (13, 13, [0, 0, 100, 200]),
        (14, 14, [0, 0, 200, 200]),
        (15, 15, [0, 0, 100, 200]),
        (17, 17, [0, 0, 100, 100]),
        (19, 20, [0, 0, 200, 200]),
    ];
    let colr = colr(&paints, &layers, &clips);
    let tables: [(&[u8; 4], Vec<u8>); 8] = [
        (b"COLR", colr),
        (b"CPAL", cpal),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
    ];
    let mut offset = 12 + 16 * tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    for (tag, body) in &tables {
        out.extend_from_slice(*tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        offset += body.len().div_ceil(4) * 4;
    }
    for (_, body) in &tables {
        out.extend_from_slice(body);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

fn render(gid: u16) -> ColorPixmap {
    let bytes = font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).expect("face parses");
    Rasterizer::new()
        .rasterize_colrv1_glyph(&face, gid, 0, 100.0, &[])
        .expect("renders")
}

/// The pixel covering design point `(x, y)`: the canvas starts one
/// pixel left of `x_min` and one pixel above `y_max`.
fn at(pix: &ColorPixmap, x_min: f32, y_max: f32, x: f32, y: f32) -> [u8; 4] {
    let px = (x - x_min) * 0.1 + 1.0;
    let py = (y_max - y) * 0.1 + 1.0;
    pix.get(px as u32, py as u32)
}

// =========================================================================
// Tests
// =========================================================================

#[test]
fn transform_below_paint_glyph_moves_the_fill_not_the_outline() {
    let scaled = render(10);
    let plain = render(11);
    // The clip is the 200-unit square either way: 20 pixels plus the
    // margin. Scaling the outline too would double it.
    assert_eq!((scaled.width, scaled.height), (22, 22));
    assert_eq!((plain.width, plain.height), (22, 22));
    // Near the right edge the plain gradient is almost blue; the
    // doubled one is only half way there.
    let p = at(&plain, 0.0, 200.0, 195.0, 100.0);
    let s = at(&scaled, 0.0, 200.0, 195.0, 100.0);
    assert!(p[2] > 230 && p[0] < 25, "plain {p:?}");
    assert!(s[0] > 110 && s[2] > 110, "scaled {s:?}");
}

#[test]
fn composites_blend_isolated_groups() {
    let pix = render(12);
    assert_eq!((pix.width, pix.height), (42, 22));
    // SrcIn keeps the blue source where the red backdrop is.
    assert_eq!(at(&pix, 0.0, 200.0, 100.0, 100.0), [0, 0, 255, 255]);
    // Outside the composite's groups the green layer below survives; a
    // non-isolated SrcIn would have cleared it.
    assert_eq!(at(&pix, 0.0, 200.0, 300.0, 100.0), [0, 255, 0, 255]);
}

#[test]
fn clip_box_clips_the_glyph() {
    let pix = render(13);
    // The ClipBox is half the square: 10 pixels plus the margin.
    assert_eq!((pix.width, pix.height), (12, 22));
    assert_eq!(at(&pix, 0.0, 200.0, 50.0, 100.0), [255, 0, 0, 255]);
    let right_column: Vec<[u8; 4]> = (0..pix.height).map(|y| pix.get(11, y)).collect();
    assert!(right_column.iter().all(|p| p[3] == 0), "{right_column:?}");
}

#[test]
fn referenced_glyphs_clip_to_their_own_box() {
    let pix = render(14);
    assert_eq!((pix.width, pix.height), (22, 22));
    assert_eq!(at(&pix, 0.0, 200.0, 50.0, 100.0), [255, 0, 0, 255]);
    // Glyph 15's box ends at x = 100 even though its square does not.
    assert_eq!(at(&pix, 0.0, 200.0, 150.0, 100.0), [0, 0, 0, 0]);
}

#[test]
fn unbounded_glyphs_render_empty_and_clip_boxes_bound_them() {
    let pix = render(16);
    assert_eq!((pix.width, pix.height), (0, 0));
    let pix = render(17);
    assert_eq!((pix.width, pix.height), (12, 12));
    assert_eq!(pix.get(5, 5), [255, 0, 0, 255]);
    assert_eq!(pix.get(0, 0), [0, 0, 0, 0], "the margin stays clear");
}

#[test]
fn radial_gradients_under_non_uniform_scale_are_ellipses() {
    let pix = render(18);
    assert_eq!((pix.width, pix.height), (42, 22));
    // Paint space is design space with x halved; the gradient runs
    // from red at the center (100, 100) to blue at radius 100.
    for (x, y) in [
        (350.0, 100.0),
        (200.0, 175.0),
        (300.0, 150.0),
        (120.0, 60.0),
    ] {
        let p = at(&pix, 0.0, 200.0, x, y);
        // The pixel center in design units, then in paint space.
        let cx = ((x * 0.1 + 1.0).floor() + 0.5 - 1.0) * 10.0;
        let cy = 200.0 - ((200.0 - y) * 0.1 + 1.0).floor() * 10.0 - 5.0 + 10.0;
        let (px, py) = (cx / 2.0, cy);
        let t: f32 = (((px - 100.0).powi(2) + (py - 100.0).powi(2)).sqrt() / 100.0).min(1.0);
        let red = ((1.0 - t) * 255.0).round();
        assert!(
            (f32::from(p[0]) - red).abs() <= 3.0,
            "({x}, {y}): {p:?}, expected red {red}"
        );
        assert_eq!(p[3], 255);
    }
}

#[test]
fn outline_far_larger_than_the_canvas_still_covers_it() {
    // PaintTransform scales the square 1000 times, to 20000 pixels a
    // side. Only the part inside the 200-unit clip box is rasterized,
    // and every pixel of the box lies inside the scaled square.
    let pix = render(19);
    assert_eq!((pix.width, pix.height), (22, 22));
    assert_eq!(at(&pix, 0.0, 200.0, 100.0, 100.0), [255, 0, 0, 255]);
    assert_eq!(at(&pix, 0.0, 200.0, 5.0, 195.0), [255, 0, 0, 255]);
}

#[test]
fn self_referencing_composite_renders_within_its_budgets() {
    // Both children of the composite are the composite itself. The walk
    // stops at its depth and paint budgets, every group it opens is
    // closed, and nothing is filled, so the canvas stays transparent.
    let pix = render(20);
    assert_eq!((pix.width, pix.height), (22, 22));
    assert!(pix.data.chunks_exact(4).all(|p| p[3] == 0));
}
