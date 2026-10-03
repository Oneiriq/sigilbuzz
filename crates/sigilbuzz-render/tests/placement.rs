//! Placement of rasterized glyph images relative to the glyph origin.
//!
//! Every `*_placed` entry point returns the offset of its image's
//! top-left pixel from the pen position on the baseline, y down. These
//! tests check that offset against the boxes the rasterizer documents
//! (the outline's pixel box, the COLRv1 clip box, the COLRv0 layer
//! union) and that a run drawn at pen position plus offset puts every
//! glyph's ink exactly where its outline says.
//!
//! The synthetic font has 1000 units per em and renders at 100 pixels
//! per em, 0.1 pixel per unit, so its rectangles land on whole pixels
//! and their coverage is exactly 0 or 255.
//!
//! - gid 1: outline rectangle (-100, -150) to (300, 450).
//! - gid 2: outline rectangle (50, 0) to (250, 200).
//! - gid 3: COLRv1 `PaintGlyph(1, red)` with ClipBox equal to gid 1's box.
//! - gid 4: COLRv1 `PaintGlyph(2, blue)` without a ClipBox.
//! - gid 5: COLRv1 `PaintGlyph(1, red)` with ClipBox (-33, -77, 333, 444).
//! - gid 6: COLRv0 layers gid 1 (red) under gid 2 (blue).

use sigilbuzz::Face;
use sigilbuzz_render::{ColorPixmap, Pixmap, Placement, Rasterizer};

/// Pixels per em; 0.1 pixel per design unit.
const SIZE: f32 = 100.0;

// =========================================================================
// Fixture
// =========================================================================

fn words(values: &[i16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// A one-contour glyf rectangle, counter-clockwise in design space.
fn rect_glyph(x0: i16, y0: i16, x1: i16, y1: i16) -> Vec<u8> {
    let mut g = words(&[1, x0, y0, x1, y1]);
    g.extend(words(&[3, 0])); // endPtsOfContours, instructionLength
    g.extend_from_slice(&[0x01; 4]); // on-curve, two-byte deltas
    g.extend(words(&[x0, x1 - x0, 0, x0 - x1]));
    g.extend(words(&[y0, 0, y1 - y0, 0]));
    g
}

/// `PaintGlyph(gid)` over `PaintSolid(entry, alpha 1)`.
fn paint_glyph_solid(gid: u16, entry: u16) -> Vec<u8> {
    let mut p = vec![10u8, 0, 0, 6];
    p.extend_from_slice(&gid.to_be_bytes());
    p.push(2);
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&0x4000u16.to_be_bytes());
    p
}

/// COLR v1 with v0 base glyph 6 (layers gid 1 entry 0, gid 2 entry 1),
/// v1 paints for gids 3 to 5, and ClipBoxes for gids 3 and 5.
fn colr() -> Vec<u8> {
    let v0_bases: [[u16; 3]; 1] = [[6, 0, 2]];
    let v0_layers: [[u16; 2]; 2] = [[1, 0], [2, 1]];
    let paints = [
        (3u16, paint_glyph_solid(1, 0)),
        (4, paint_glyph_solid(2, 1)),
        (5, paint_glyph_solid(1, 0)),
    ];
    let clips: [(u16, [i16; 4]); 2] = [(3, [-100, -150, 300, 450]), (5, [-33, -77, 333, 444])];

    let bases_at = 34u32;
    let layers_at = bases_at + 6 * v0_bases.len() as u32;
    let list_at = layers_at + 4 * v0_layers.len() as u32;
    let mut list = Vec::new();
    list.extend_from_slice(&(paints.len() as u32).to_be_bytes());
    let mut offset = 4 + 6 * paints.len();
    for (gid, p) in &paints {
        list.extend_from_slice(&gid.to_be_bytes());
        list.extend_from_slice(&(offset as u32).to_be_bytes());
        offset += p.len();
    }
    for (_, p) in &paints {
        list.extend_from_slice(p);
    }
    let clips_at = list_at + list.len() as u32;

    let mut c = Vec::new();
    c.extend_from_slice(&1u16.to_be_bytes()); // version
    c.extend_from_slice(&(v0_bases.len() as u16).to_be_bytes());
    c.extend_from_slice(&bases_at.to_be_bytes());
    c.extend_from_slice(&layers_at.to_be_bytes());
    c.extend_from_slice(&(v0_layers.len() as u16).to_be_bytes());
    c.extend_from_slice(&list_at.to_be_bytes());
    c.extend_from_slice(&0u32.to_be_bytes()); // layerList
    c.extend_from_slice(&clips_at.to_be_bytes());
    c.extend_from_slice(&[0; 8]); // varIndexMap, itemVariationStore
    for base in v0_bases {
        c.extend(base.iter().flat_map(|v| v.to_be_bytes()));
    }
    for layer in v0_layers {
        c.extend(layer.iter().flat_map(|v| v.to_be_bytes()));
    }
    c.extend_from_slice(&list);
    // ClipList format 1: records, then one format-1 ClipBox per record.
    c.push(1);
    c.extend_from_slice(&(clips.len() as u32).to_be_bytes());
    for (i, (gid, _)) in clips.iter().enumerate() {
        c.extend_from_slice(&gid.to_be_bytes());
        c.extend_from_slice(&gid.to_be_bytes());
        let box_at = (5 + 7 * clips.len() + 9 * i) as u32;
        c.extend_from_slice(&box_at.to_be_bytes()[1..]);
    }
    for (_, b) in clips {
        c.push(1);
        c.extend(words(&b));
    }
    c
}

/// Advance widths in design units, all multiples of 10 so pen
/// positions stay on whole pixels.
const ADVANCES: [i16; 7] = [500, 450, 300, 450, 300, 500, 450];

fn font() -> Vec<u8> {
    let outlines = [
        Vec::new(),
        rect_glyph(-100, -150, 300, 450),
        rect_glyph(50, 0, 250, 200),
    ];
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for g in &outlines {
        loca.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());
        glyf.extend_from_slice(g);
    }
    // gids 2 to 6 end where the outlines end; 3 to 6 are empty.
    for _ in outlines.len()..=ADVANCES.len() {
        loca.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());
    }
    let num_glyphs = ADVANCES.len() as i16;
    let mut head = 0x0001_0000u32.to_be_bytes().to_vec();
    head.extend_from_slice(&[0; 8]);
    head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head.extend(words(&[0, 1000]));
    head.extend_from_slice(&[0; 16]);
    head.extend(words(&[-100, -150, 300, 450, 0, 8, 2, 0, 0]));
    let mut hhea = 0x0001_0000u32.to_be_bytes().to_vec();
    hhea.extend(words(&[
        800, -200, 0, 500, 0, 0, 400, 1, 0, 0, 0, 0, 0, 0, 0, num_glyphs,
    ]));
    let mut maxp = 0x0000_5000u32.to_be_bytes().to_vec();
    maxp.extend(words(&[num_glyphs]));
    let hmtx: Vec<u8> = ADVANCES.iter().flat_map(|&a| words(&[a, 0])).collect();
    // CPAL: one palette, entry 0 red and entry 1 blue (BGRA).
    let mut cpal = words(&[0, 2, 1, 2]);
    cpal.extend_from_slice(&14u32.to_be_bytes());
    cpal.extend(words(&[0]));
    cpal.extend_from_slice(&[0, 0, 255, 255, 255, 0, 0, 255]);

    let tables: [(&[u8; 4], Vec<u8>); 8] = [
        (b"COLR", colr()),
        (b"CPAL", cpal),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
    ];
    let mut offset = 12 + 16 * tables.len();
    let mut out = 0x0001_0000u32.to_be_bytes().to_vec();
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

// =========================================================================
// Helpers
// =========================================================================

/// A device-space pixel rectangle `[x0, x1) x [y0, y1)`, y down from
/// the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PxBox {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl PxBox {
    /// The pixels a design-unit rectangle covers at [`SIZE`]. Every
    /// fixture coordinate is a multiple of 10, so this is exact.
    fn design(x_min: i32, y_min: i32, x_max: i32, y_max: i32) -> Self {
        Self {
            x0: x_min / 10,
            y0: -y_max / 10,
            x1: x_max / 10,
            y1: -y_min / 10,
        }
    }

    fn contains(self, x: i32, y: i32) -> bool {
        (self.x0..self.x1).contains(&x) && (self.y0..self.y1).contains(&y)
    }

    fn shifted(self, dx: i32) -> Self {
        Self {
            x0: self.x0 + dx,
            x1: self.x1 + dx,
            ..self
        }
    }
}

const GID1_INK: PxBox = PxBox {
    x0: -10,
    y0: -45,
    x1: 30,
    y1: 15,
};
const GID2_INK: PxBox = PxBox {
    x0: 5,
    y0: -20,
    x1: 25,
    y1: 0,
};

fn with_face<R>(f: impl FnOnce(&Face<'_>) -> R) -> R {
    let bytes = font();
    let face = Face::parse_bytes(&bytes, 0).expect("fixture parses");
    f(&face)
}

/// Alpha of the color image `pix` placed at `at` for the glyph origin,
/// at device pixel `(x, y)`; zero outside the image.
fn alpha_at(pix: &ColorPixmap, at: Placement, x: i32, y: i32) -> u8 {
    let (lx, ly) = (x - at.left, y - at.top);
    if lx < 0 || ly < 0 {
        return 0;
    }
    pix.get(lx as u32, ly as u32)[3]
}

/// [`alpha_at`] for an alpha image.
fn coverage_at(pix: &Pixmap, at: Placement, x: i32, y: i32) -> u8 {
    let (lx, ly) = (x - at.left, y - at.top);
    if lx < 0 || ly < 0 {
        return 0;
    }
    pix.get(lx as u32, ly as u32)
}

// =========================================================================
// Single glyphs
// =========================================================================

#[test]
fn outline_placement_is_the_pixel_box_corner_less_the_margin() {
    with_face(|face| {
        let rast = Rasterizer::new();
        let (pix, at) = rast.rasterize_glyph_placed(face, 1, SIZE, &[]).unwrap();
        assert_eq!(pix, rast.rasterize_glyph(face, 1, SIZE, &[]).unwrap());
        // floor(x_min * s) - 1 and floor(-y_max * s) - 1.
        assert_eq!(at, Placement::new(-11, -46));
        assert_eq!((pix.width, pix.height), (42, 62));
        for y in at.top..at.top + pix.height as i32 {
            for x in at.left..at.left + pix.width as i32 {
                let want = if GID1_INK.contains(x, y) { 255 } else { 0 };
                assert_eq!(coverage_at(&pix, at, x, y), want, "({x}, {y})");
            }
        }
    });
}

#[test]
fn colrv1_placement_is_the_clip_box_corner_less_the_margin() {
    with_face(|face| {
        let rast = Rasterizer::new();
        let (pix, at) = rast
            .rasterize_colrv1_glyph_placed(face, 3, 0, SIZE, &[])
            .unwrap();
        assert_eq!(
            pix,
            rast.rasterize_colrv1_glyph(face, 3, 0, SIZE, &[]).unwrap()
        );
        // ClipBox (-100, -150, 300, 450) at 0.1 pixel per unit.
        let clip = PxBox::design(-100, -150, 300, 450);
        assert_eq!(at, Placement::new(clip.x0 - 1, clip.y0 - 1));
        assert_eq!(
            (pix.width as i32, pix.height as i32),
            (clip.x1 - clip.x0 + 2, clip.y1 - clip.y0 + 2)
        );
        // The color image lines up with the outline image of the glyph
        // it paints, pixel for pixel.
        let (outline, outline_at) = rast.rasterize_glyph_placed(face, 1, SIZE, &[]).unwrap();
        for y in -60..30 {
            for x in -30..50 {
                assert_eq!(
                    alpha_at(&pix, at, x, y),
                    coverage_at(&outline, outline_at, x, y),
                    "({x}, {y})"
                );
            }
        }
    });
}

#[test]
fn colrv1_placement_rounds_a_fractional_clip_box_out() {
    with_face(|face| {
        let (pix, at) = Rasterizer::new()
            .rasterize_colrv1_glyph_placed(face, 5, 0, SIZE, &[])
            .unwrap();
        // ClipBox (-33, -77, 333, 444) is (-3.3, -44.4) to (33.3, 7.7) in
        // pixels, rounded out to (-4, -45) to (34, 8), plus the margin.
        assert_eq!(at, Placement::new(-5, -46));
        assert_eq!((pix.width, pix.height), (40, 55));
        let snapped = PxBox {
            x0: -4,
            y0: -45,
            x1: 34,
            y1: 8,
        };
        for y in -60..30 {
            for x in -30..50 {
                let inside = GID1_INK.contains(x, y) && snapped.contains(x, y);
                let want = if inside { 255 } else { 0 };
                assert_eq!(alpha_at(&pix, at, x, y), want, "({x}, {y})");
            }
        }
    });
}

#[test]
fn colrv1_without_a_clip_box_is_placed_by_its_paint_bounds() {
    with_face(|face| {
        let rast = Rasterizer::new();
        let (pix, at) = rast
            .rasterize_colrv1_glyph_placed(face, 4, 0, SIZE, &[])
            .unwrap();
        // The bounds are gid 2's outline box, so the canvas is the box
        // the outline rasterizer picks for gid 2.
        let (outline, outline_at) = rast.rasterize_glyph_placed(face, 2, SIZE, &[]).unwrap();
        assert_eq!(at, outline_at);
        assert_eq!(at, Placement::new(GID2_INK.x0 - 1, GID2_INK.y0 - 1));
        assert_eq!((pix.width, pix.height), (outline.width, outline.height));
        assert_eq!(alpha_at(&pix, at, 10, -10), 255);
        assert_eq!(pix.get(1, 1), [0, 0, 255, 255], "blue ink at the corner");
    });
}

#[test]
fn colrv0_placement_is_the_layer_union_corner() {
    with_face(|face| {
        let rast = Rasterizer::new();
        let (pix, at) = rast
            .rasterize_colrv0_glyph_placed(face, 6, 0, SIZE, &[])
            .unwrap();
        assert_eq!(
            pix,
            rast.rasterize_colrv0_glyph(face, 6, 0, SIZE, &[]).unwrap()
        );
        // gid 1's box contains gid 2's, so the union is gid 1's box.
        assert_eq!(at, Placement::new(-11, -46));
        assert_eq!((pix.width, pix.height), (42, 62));
        let color = |x: i32, y: i32| pix.get((x - at.left) as u32, (y - at.top) as u32);
        assert_eq!(color(10, -10), [0, 0, 255, 255], "blue layer on top");
        assert_eq!(color(0, 10), [255, 0, 0, 255], "red layer below");
        assert_eq!(color(-11, -46), [0, 0, 0, 0], "margin");
    });
}

#[test]
fn every_placed_variant_returns_the_unplaced_pixmap() {
    with_face(|face| {
        let rast = Rasterizer::new();
        for size in [7.5, 24.0, 61.3] {
            for gid in [1, 2] {
                let (pix, _) = rast.rasterize_glyph_placed(face, gid, size, &[]).unwrap();
                assert_eq!(pix, rast.rasterize_glyph(face, gid, size, &[]).unwrap());
            }
            for gid in [3, 4, 5] {
                let (pix, _) = rast
                    .rasterize_colrv1_glyph_placed(face, gid, 0, size, &[])
                    .unwrap();
                let plain = rast.rasterize_colrv1_glyph(face, gid, 0, size, &[]);
                assert_eq!(pix, plain.unwrap());
            }
            let (pix, _) = rast
                .rasterize_colrv0_glyph_placed(face, 6, 0, size, &[])
                .unwrap();
            let plain = rast.rasterize_colrv0_glyph(face, 6, 0, size, &[]);
            assert_eq!(pix, plain.unwrap());
        }
        // Errors match too.
        assert_eq!(
            rast.rasterize_glyph_placed(face, 3, SIZE, &[]).unwrap_err(),
            rast.rasterize_glyph(face, 3, SIZE, &[]).unwrap_err()
        );
        assert_eq!(
            rast.rasterize_colrv1_glyph_placed(face, 1, 0, SIZE, &[])
                .unwrap_err(),
            rast.rasterize_colrv1_glyph(face, 1, 0, SIZE, &[])
                .unwrap_err()
        );
    });
}

// =========================================================================
// Runs
// =========================================================================

#[test]
fn run_drawn_at_pen_plus_placement_lines_up_with_the_outlines() {
    // Lay out gids 3, 4, 5, 6, and 1 on one baseline, drawing each
    // with the entry point for its format, and compare the composed
    // coverage with the outline boxes moved to their pen positions.
    let run = [3u16, 4, 5, 6, 1];
    let (width, height, baseline) = (260_i32, 90_i32, 60_i32);
    let mut canvas = vec![0u8; (width * height) as usize];
    let mut expected = vec![0u8; (width * height) as usize];
    with_face(|face| {
        let rast = Rasterizer::new();
        let mut pen = 20; // whole pixels
        for gid in run {
            let ink: Vec<PxBox> = match gid {
                3 | 1 => vec![GID1_INK],
                4 => vec![GID2_INK],
                5 => vec![PxBox {
                    x0: -4,
                    y0: -45,
                    x1: 30,
                    y1: 8,
                }],
                6 => vec![GID1_INK, GID2_INK],
                _ => unreachable!(),
            };
            let image: Vec<(i32, i32, u8)> = match gid {
                1 => {
                    let (pix, at) = rast.rasterize_glyph_placed(face, gid, SIZE, &[]).unwrap();
                    let (x0, y0) = at.top_left(pen, baseline);
                    (0..pix.height)
                        .flat_map(|y| (0..pix.width).map(move |x| (x, y)))
                        .map(|(x, y)| (x0 + x as i32, y0 + y as i32, pix.get(x, y)))
                        .collect()
                }
                6 => {
                    let (pix, at) = rast
                        .rasterize_colrv0_glyph_placed(face, gid, 0, SIZE, &[])
                        .unwrap();
                    placed_alpha(&pix, at.top_left(pen, baseline))
                }
                _ => {
                    let (pix, at) = rast
                        .rasterize_colrv1_glyph_placed(face, gid, 0, SIZE, &[])
                        .unwrap();
                    placed_alpha(&pix, at.top_left(pen, baseline))
                }
            };
            for (x, y, a) in image {
                assert!((0..width).contains(&x) && (0..height).contains(&y));
                let px = &mut canvas[(y * width + x) as usize];
                *px = (*px).max(a);
            }
            for b in ink {
                let b = b.shifted(pen);
                for y in b.y0..b.y1 {
                    for x in b.x0..b.x1 {
                        expected[((y + baseline) * width + x) as usize] = 255;
                    }
                }
            }
            pen += i32::from(ADVANCES[usize::from(gid)]) / 10;
        }
    });
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) as usize;
            assert_eq!(canvas[i], expected[i], "run pixel ({x}, {y})");
        }
    }
}

/// Device pixels and alphas of a color image whose top-left pixel is at
/// `(x0, y0)`.
fn placed_alpha(pix: &ColorPixmap, (x0, y0): (i32, i32)) -> Vec<(i32, i32, u8)> {
    (0..pix.height)
        .flat_map(|y| (0..pix.width).map(move |x| (x, y)))
        .map(|(x, y)| (x0 + x as i32, y0 + y as i32, pix.get(x, y)[3]))
        .collect()
}

#[test]
fn real_font_run_ink_matches_outline_extents() {
    // Open Sans at a size that puts outlines between pixels. Every
    // glyph's ink, drawn at its rounded pen position plus placement,
    // covers its outline's control box to within the anti-aliasing
    // pixel on each side.
    let data = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
    let face = Face::parse_bytes(data, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let hmtx = face.hmtx().unwrap();
    let size = 37.3_f32;
    let scale = size / f32::from(face.head().unwrap().units_per_em);
    let rast = Rasterizer::new();
    let baseline = 50;
    let mut pen = 3.0_f32;
    for ch in "Hamburgefontsiv".chars() {
        let gid = cmap.glyph_id(ch).unwrap();
        let (pix, at) = rast.rasterize_glyph_placed(&face, gid, size, &[]).unwrap();
        let pen_px = pen.round() as i32;
        let (x0, y0) = at.top_left(pen_px, baseline);
        let (mut ink_x0, mut ink_y0, mut ink_x1, mut ink_y1) =
            (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for y in 0..pix.height {
            for x in 0..pix.width {
                if pix.get(x, y) > 0 {
                    ink_x0 = ink_x0.min(x0 + x as i32);
                    ink_y0 = ink_y0.min(y0 + y as i32);
                    ink_x1 = ink_x1.max(x0 + x as i32 + 1);
                    ink_y1 = ink_y1.max(y0 + y as i32 + 1);
                }
            }
        }
        // The outline's control box, in device pixels at the pen.
        let outline = face.glyph_outline(gid).unwrap().unwrap();
        let (mut bx0, mut by0, mut bx1, mut by1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for op in outline.ops() {
            use sigilbuzz::tables::PathOp;
            let points: &[(f32, f32)] = &match *op {
                PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => vec![(x, y)],
                PathOp::QuadTo { cx, cy, x, y } => vec![(cx, cy), (x, y)],
                PathOp::CubicTo {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => vec![(c1x, c1y), (c2x, c2y), (x, y)],
                PathOp::Close => vec![],
            };
            for &(x, y) in points {
                bx0 = bx0.min(x);
                bx1 = bx1.max(x);
                by0 = by0.min(y);
                by1 = by1.max(y);
            }
        }
        let want = (
            (pen_px as f32 + bx0 * scale).floor() as i32,
            (baseline as f32 - by1 * scale).floor() as i32,
            (pen_px as f32 + bx1 * scale).ceil() as i32,
            (baseline as f32 - by0 * scale).ceil() as i32,
        );
        let got = (ink_x0, ink_y0, ink_x1, ink_y1);
        for (g, w) in [
            (got.0, want.0),
            (got.1, want.1),
            (got.2, want.2),
            (got.3, want.3),
        ] {
            assert!((g - w).abs() <= 1, "{ch:?}: ink {got:?}, outline {want:?}");
        }
        pen += f32::from(hmtx.advance(gid).unwrap()) * scale;
    }
}
