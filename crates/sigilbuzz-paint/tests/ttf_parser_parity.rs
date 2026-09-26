//! Variable COLRv1 paint values match ttf-parser's COLR painter.
//!
//! The fixture is a one-axis variable font whose COLR carries its own
//! DeltaSetIndexMap and ItemVariationStore, with every variable paint
//! format sigilbuzz supports and a variable ClipBox. The map reverses
//! the flat index space, so any lookup that skips it lands on the wrong
//! row. Both implementations paint every glyph at several axis
//! positions; the accumulated transform at each fill, the fill's color
//! or gradient geometry, the gradient stops, and the clip box must
//! agree.
//!
//! ttf-parser applies COLR deltas only when the table has a
//! DeltaSetIndexMap, so the map is required here, not just exercised.

use core::f32::consts::PI;

use sigilbuzz::Face;
use sigilbuzz_paint::walk::{
    paint_glyph, paint_glyph_unclipped, ColorLineRef, ColorRef, PaintSink, Resolver, RootClip,
};
use sigilbuzz_paint::{CompositeMode, EvalOptions, Transform2D};
use ttf_parser::colr::{ClipBox, CompositeMode as TtfMode, Paint, Painter};
use ttf_parser::{GlyphId, RgbaColor, Tag};

// =========================================================================
// Fixture
// =========================================================================

fn f2dot14(v: f32) -> [u8; 2] {
    ((v * 16384.0).round() as i16).to_be_bytes()
}

fn fixed(v: f32) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn set_offset24(p: &mut [u8], at: usize, target: usize) {
    let v = target as u32;
    p[at..at + 3].copy_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
}

/// `head` followed by `child`, with the Offset24 at byte 1 pointing at
/// the child.
fn parent(mut head: Vec<u8>, child: &[u8]) -> Vec<u8> {
    let at = head.len();
    set_offset24(&mut head, 1, at);
    head.extend_from_slice(child);
    head
}

fn var_solid(entry: u16, alpha: f32, base: u32) -> Vec<u8> {
    let mut p = vec![3u8];
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(alpha));
    p.extend_from_slice(&base.to_be_bytes());
    p
}

/// VarColorLine with `(offset, entry, alpha, varIndexBase)` stops.
fn var_line(extend: u8, stops: &[(f32, u16, f32, u32)]) -> Vec<u8> {
    let mut p = vec![extend];
    p.extend_from_slice(&(stops.len() as u16).to_be_bytes());
    for (offset, entry, alpha, base) in stops {
        p.extend_from_slice(&f2dot14(*offset));
        p.extend_from_slice(&entry.to_be_bytes());
        p.extend_from_slice(&f2dot14(*alpha));
        p.extend_from_slice(&base.to_be_bytes());
    }
    p
}

/// A gradient: format byte, color line Offset24, `fields`, base, line.
fn var_gradient(format: u8, fields: &[u8], base: u32, line: &[u8]) -> Vec<u8> {
    let mut p = vec![format, 0, 0, 0];
    p.extend_from_slice(fields);
    p.extend_from_slice(&base.to_be_bytes());
    parent(p, line)
}

fn words(values: &[i16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// A transform-family paint: format byte, child Offset24, `fields`,
/// base, child.
fn var_op(format: u8, fields: &[u8], base: u32, child: &[u8]) -> Vec<u8> {
    let mut p = vec![format, 0, 0, 0];
    p.extend_from_slice(fields);
    p.extend_from_slice(&base.to_be_bytes());
    parent(p, child)
}

fn f2(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| f2dot14(*v)).collect()
}

fn var_transform(m: [f32; 6], base: u32, child: &[u8]) -> Vec<u8> {
    let mut p = vec![13u8, 0, 0, 0, 0, 0, 7];
    for v in m {
        p.extend_from_slice(&fixed(v));
    }
    p.extend_from_slice(&base.to_be_bytes());
    parent(p, child)
}

/// Number of flat variation indices the fixture uses.
const INDICES: u32 = 120;

/// The raw delta at the axis maximum for flat index `i`: design units
/// for FWORD fields, F2DOT14 ticks for alphas, scales, angles, and stop
/// offsets, and 16.16 ticks for the affine.
fn delta(i: u32) -> i16 {
    match i {
        0 => -4096, // solid alpha: -0.25
        10..=15 => [30, -20, 40, 10, -50, 60][(i - 10) as usize],
        20 | 22 => 2048,  // stop offsets: +0.125
        21 | 23 => -3277, // stop alphas: about -0.2
        30..=35 => [5, -5, 20, 15, 25, 100][(i - 30) as usize],
        40 | 41 => 12,   // sweep center
        42 | 43 => 1638, // sweep angles: about +0.1
        50..=55 => [6554, -3277, 1638, 4915, 32767, -16384][(i - 50) as usize],
        60 | 61 => 25,    // translate
        70 | 71 => -1638, // scale: about -0.1
        72 | 73 => 40,    // scale center
        80 => 2731,       // rotate: about +1/6 turn
        81 | 82 => -30,   // rotate center
        90 | 91 => 1024,  // skew angles
        92 | 93 => 17,    // skew center
        100 => 4096,      // uniform scale: +0.25
        102 => -2048,     // rotate
        104 | 105 => 512, // skew
        110..=113 => [10, -20, 30, 40][(i - 110) as usize],
        _ => 0,
    }
}

/// DeltaSetIndexMap (format 0, 2-byte entries, 16 inner bits) mapping
/// flat index `i` to row `INDICES - 1 - i` of outer subtable 0.
fn index_map() -> Vec<u8> {
    let mut p = vec![0u8, 0x1F];
    p.extend_from_slice(&(INDICES as u16).to_be_bytes());
    for i in 0..INDICES {
        p.extend_from_slice(&((INDICES - 1 - i) as u16).to_be_bytes());
    }
    p
}

/// One-axis store, one region peaking at +1, rows in mapped order.
fn item_variation_store() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&12u32.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&22u32.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&f2(&[0.0, 1.0, 1.0]));
    for v in [INDICES as u16, 1, 1, 0] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    for row in 0..INDICES {
        p.extend_from_slice(&delta(INDICES - 1 - row).to_be_bytes());
    }
    p
}

fn paints() -> Vec<(u16, Vec<u8>)> {
    let solid = |entry| var_solid(entry, 1.0, u32::MAX);
    let line = var_line(0, &[(0.0, 0, 1.0, 20), (0.75, 1, 0.9, 22)]);
    vec![
        (1, var_solid(0, 0.75, 0)),
        (
            2,
            var_gradient(5, &words(&[0, 10, 100, 20, 30, 200]), 10, &line),
        ),
        (3, {
            let mut fields = words(&[10, 20]);
            fields.extend_from_slice(&5u16.to_be_bytes());
            fields.extend(words(&[30, 40]));
            fields.extend_from_slice(&300u16.to_be_bytes());
            var_gradient(7, &fields, 30, &var_line(2, &[(0.25, 2, 1.0, 20)]))
        }),
        (4, {
            let mut fields = words(&[50, 60]);
            fields.extend(f2(&[-0.5, 0.75]));
            var_gradient(9, &fields, 40, &var_line(1, &[(0.0, 0, 1.0, 22)]))
        }),
        (
            5,
            var_transform([1.5, 0.25, -0.5, 0.75, 10.0, -20.0], 50, &solid(1)),
        ),
        (6, var_op(15, &words(&[-30, 45]), 60, &solid(2))),
        (7, {
            let mut fields = f2(&[1.25, 0.5]);
            fields.extend(words(&[100, 200]));
            var_op(19, &fields, 70, &solid(0))
        }),
        (8, {
            let mut fields = f2(&[0.25]);
            fields.extend(words(&[300, -100]));
            var_op(27, &fields, 80, &solid(1))
        }),
        (9, {
            let mut fields = f2(&[0.125, -0.0625]);
            fields.extend(words(&[20, 40]));
            var_op(31, &fields, 90, &solid(2))
        }),
        (10, {
            let skew = var_op(29, &f2(&[0.1, 0.05]), 104, &solid(0));
            let rotate = var_op(25, &f2(&[-0.3]), 102, &skew);
            var_op(21, &f2(&[0.8]), 100, &rotate)
        }),
    ]
}

fn colr() -> Vec<u8> {
    let paints = paints();
    let header_len = 34usize;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes()); // BaseGlyphList
    out.extend_from_slice(&0u32.to_be_bytes()); // LayerList
    let slots = out.len();
    out.extend_from_slice(&[0; 12]); // ClipList, index map, store
    out.extend_from_slice(&(paints.len() as u32).to_be_bytes());
    let records = out.len();
    for (gid, _) in &paints {
        out.extend_from_slice(&gid.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    for (i, (_, bytes)) in paints.iter().enumerate() {
        let rel = (out.len() - header_len) as u32;
        out[records + i * 6 + 2..records + i * 6 + 6].copy_from_slice(&rel.to_be_bytes());
        out.extend_from_slice(bytes);
    }
    // ClipList: glyph 1 gets a ClipBoxFormat2 varying from index 110.
    let at = out.len() as u32;
    out[slots..slots + 4].copy_from_slice(&at.to_be_bytes());
    out.push(1);
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0, 0, 12]);
    out.push(2);
    out.extend(words(&[-100, -200, 900, 800]));
    out.extend_from_slice(&110u32.to_be_bytes());
    for (slot, table) in [
        (slots + 4, index_map()),
        (slots + 8, item_variation_store()),
    ] {
        let at = out.len() as u32;
        out[slot..slot + 4].copy_from_slice(&at.to_be_bytes());
        out.extend_from_slice(&table);
    }
    out
}

fn cpal() -> Vec<u8> {
    let mut out = Vec::new();
    for v in [0u16, 3, 1, 3] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&14u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    for (r, g, b, a) in [(255u8, 0u8, 0u8, 255u8), (0, 255, 0, 200), (0, 0, 255, 128)] {
        out.extend_from_slice(&[b, g, r, a]);
    }
    out
}

fn fvar() -> Vec<u8> {
    let mut out = Vec::new();
    for v in [1u16, 0, 16, 2, 1, 20, 0, 8] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(b"wght");
    out.extend_from_slice(&fixed(0.0));
    out.extend_from_slice(&fixed(0.0));
    out.extend_from_slice(&fixed(1.0));
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&256u16.to_be_bytes());
    out
}

fn head() -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    h.extend_from_slice(&0u32.to_be_bytes());
    h.extend_from_slice(&0u32.to_be_bytes());
    h.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    h.extend_from_slice(&0u16.to_be_bytes());
    h.extend_from_slice(&1000u16.to_be_bytes());
    h.extend_from_slice(&[0; 16]);
    h.extend(words(&[0, 0, 1000, 1000]));
    h.extend(words(&[0, 8, 2, 0, 0]));
    h
}

fn hhea() -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    h.extend(words(&[
        800, -200, 0, 1000, 0, 0, 1000, 1, 0, 0, 0, 0, 0, 0, 0, 11,
    ]));
    h
}

fn maxp() -> Vec<u8> {
    let mut m = 0x0000_5000u32.to_be_bytes().to_vec();
    m.extend_from_slice(&11u16.to_be_bytes());
    m
}

fn font() -> Vec<u8> {
    let tables: [(&[u8; 4], Vec<u8>); 6] = [
        (b"COLR", colr()),
        (b"CPAL", cpal()),
        (b"fvar", fvar()),
        (b"head", head()),
        (b"hhea", hhea()),
        (b"maxp", maxp()),
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
        offset += body.len();
    }
    for (_, body) in &tables {
        out.extend_from_slice(body);
    }
    out
}

// =========================================================================
// What a fill looked like, from either implementation
// =========================================================================

type Rgba = [u8; 4];

#[derive(Debug, Clone, PartialEq)]
enum Fill {
    Solid(Rgba),
    /// Points, stops as `(offset, color)`.
    Linear([f32; 6], Vec<(f32, Rgba)>),
    Radial([f32; 6], Vec<(f32, Rgba)>),
    /// Center, then start and end angles in radians.
    Sweep([f32; 4], Vec<(f32, Rgba)>),
}

/// A fill with the accumulated transform `[xx, yx, xy, yy, dx, dy]`.
type Leaf = ([f32; 6], Fill);

fn matrix(t: Transform2D) -> [f32; 6] {
    [t.xx, t.yx, t.xy, t.yy, t.dx, t.dy]
}

// ---- sigilbuzz ----------------------------------------------------------

struct Ours<'r> {
    resolver: Resolver<'r, 'r>,
    transforms: Vec<Transform2D>,
    leaves: Vec<Leaf>,
    root_clip: Option<RootClip>,
}

impl Ours<'_> {
    fn top(&self) -> Transform2D {
        *self.transforms.last().expect("root")
    }

    fn rgba(&self, color: ColorRef) -> Rgba {
        let (c, _) = self.resolver.color(color);
        [c.r, c.g, c.b, c.a].map(|v| (v * 255.0).round() as u8)
    }

    fn stops(&self, line: ColorLineRef<'_>) -> Vec<(f32, Rgba)> {
        line.stops
            .iter()
            .map(|s| (s.offset, self.rgba(s.color)))
            .collect()
    }

    fn leaf(&mut self, fill: Fill) {
        let m = matrix(self.top());
        self.leaves.push((m, fill));
    }
}

impl PaintSink for Ours<'_> {
    fn push_transform(&mut self, t: Transform2D) {
        let t = t.then(self.top());
        self.transforms.push(t);
    }
    fn push_root_transform(&mut self) {
        self.transforms.push(self.top());
    }
    fn push_inverse_root_transform(&mut self) {
        self.transforms.push(self.top());
    }
    fn pop_transform(&mut self) {
        self.transforms.pop();
    }
    fn push_clip_glyph(&mut self, _glyph: u16) {}
    fn push_clip_rectangle(&mut self, _: f32, _: f32, _: f32, _: f32) {}
    fn push_root_clip(&mut self, clip: RootClip) {
        self.root_clip = Some(clip);
    }
    fn pop_clip(&mut self) {}
    fn push_group(&mut self) {}
    fn pop_group(&mut self, _mode: CompositeMode) {}
    fn color(&mut self, color: ColorRef) {
        let rgba = self.rgba(color);
        self.leaf(Fill::Solid(rgba));
    }
    fn linear_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        p0: (f32, f32),
        p1: (f32, f32),
        p2: (f32, f32),
    ) {
        let stops = self.stops(line);
        self.leaf(Fill::Linear([p0.0, p0.1, p1.0, p1.1, p2.0, p2.1], stops));
    }
    fn radial_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        c0: (f32, f32),
        r0: f32,
        c1: (f32, f32),
        r1: f32,
    ) {
        let stops = self.stops(line);
        self.leaf(Fill::Radial([c0.0, c0.1, r0, c1.0, c1.1, r1], stops));
    }
    fn sweep_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        center: (f32, f32),
        start_angle: f32,
        end_angle: f32,
    ) {
        let stops = self.stops(line);
        self.leaf(Fill::Sweep(
            [center.0, center.1, start_angle, end_angle],
            stops,
        ));
    }
}

fn ours(bytes: &[u8], gid: u16, coord: f32) -> (Vec<Leaf>, Option<RootClip>) {
    let face = Face::parse_bytes(bytes, 0).expect("face parses");
    let cpal = face.cpal().ok().flatten();
    let options = EvalOptions::new();
    let mut sink = Ours {
        resolver: Resolver::new(cpal.as_ref(), &options),
        transforms: vec![Transform2D::IDENTITY],
        leaves: Vec::new(),
        root_clip: None,
    };
    let coords = [coord];
    paint_glyph_unclipped(&face, gid, &coords, &mut sink);
    let leaves = core::mem::take(&mut sink.leaves);
    sink.transforms = vec![Transform2D::IDENTITY];
    paint_glyph(&face, gid, &coords, &mut sink);
    (leaves, sink.root_clip)
}

// ---- ttf-parser ---------------------------------------------------------

struct Theirs<'c> {
    palette: u16,
    coords: &'c [ttf_parser::NormalizedCoordinate],
    transforms: Vec<ttf_parser::Transform>,
    leaves: Vec<Leaf>,
    clip_box: Option<[f32; 4]>,
}

fn rgba(c: RgbaColor) -> Rgba {
    [c.red, c.green, c.blue, c.alpha]
}

fn their_stops(iter: ttf_parser::colr::GradientStopsIter<'_, '_>) -> Vec<(f32, Rgba)> {
    iter.map(|s| (s.stop_offset, rgba(s.color))).collect()
}

impl<'c> Painter<'c> for Theirs<'c> {
    fn outline_glyph(&mut self, _glyph_id: GlyphId) {}
    fn paint(&mut self, paint: Paint<'c>) {
        let t = *self.transforms.last().expect("root");
        let m = [t.a, t.b, t.c, t.d, t.e, t.f];
        let fill = match paint {
            Paint::Solid(c) => Fill::Solid(rgba(c)),
            Paint::LinearGradient(g) => Fill::Linear(
                [g.x0, g.y0, g.x1, g.y1, g.x2, g.y2],
                their_stops(g.stops(self.palette, self.coords)),
            ),
            Paint::RadialGradient(g) => Fill::Radial(
                [g.x0, g.y0, g.r0, g.x1, g.y1, g.r1],
                their_stops(g.stops(self.palette, self.coords)),
            ),
            Paint::SweepGradient(g) => Fill::Sweep(
                [
                    g.center_x,
                    g.center_y,
                    (g.start_angle + 1.0) * PI,
                    (g.end_angle + 1.0) * PI,
                ],
                their_stops(g.stops(self.palette, self.coords)),
            ),
        };
        self.leaves.push((m, fill));
    }
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, clipbox: ClipBox) {
        self.clip_box = Some([clipbox.x_min, clipbox.y_min, clipbox.x_max, clipbox.y_max]);
    }
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _mode: TtfMode) {}
    fn pop_layer(&mut self) {}
    fn push_transform(&mut self, transform: ttf_parser::Transform) {
        let top = *self.transforms.last().expect("root");
        self.transforms
            .push(ttf_parser::Transform::combine(top, transform));
    }
    fn pop_transform(&mut self) {
        self.transforms.pop();
    }
}

fn theirs(bytes: &[u8], gid: u16, coord: f32) -> (Vec<Leaf>, Option<[f32; 4]>) {
    let mut face = ttf_parser::Face::parse(bytes, 0).expect("ttf-parser parses");
    face.set_variation(Tag::from_bytes(b"wght"), coord)
        .expect("variable");
    let face = face;
    let mut painter = Theirs {
        palette: 0,
        coords: face.variation_coordinates(),
        transforms: vec![ttf_parser::Transform::default()],
        leaves: Vec::new(),
        clip_box: None,
    };
    let black = RgbaColor::new(0, 0, 0, 255);
    face.paint_color_glyph(GlyphId(gid), 0, black, &mut painter)
        .expect("color glyph");
    (painter.leaves, painter.clip_box)
}

// =========================================================================
// Comparison
// =========================================================================

fn close(a: &[f32], b: &[f32], tolerance: f32) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tolerance)
}

fn rgba_close(a: Rgba, b: Rgba) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
}

fn stops_close(a: &[(f32, Rgba)], b: &[(f32, Rgba)]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|((ao, ac), (bo, bc))| (ao - bo).abs() < 1e-4 && rgba_close(*ac, *bc))
}

fn fill_close(a: &Fill, b: &Fill) -> bool {
    match (a, b) {
        (Fill::Solid(x), Fill::Solid(y)) => rgba_close(*x, *y),
        (Fill::Linear(p, s), Fill::Linear(q, t)) | (Fill::Radial(p, s), Fill::Radial(q, t)) => {
            close(p, q, 1e-3) && stops_close(s, t)
        }
        (Fill::Sweep(p, s), Fill::Sweep(q, t)) => close(p, q, 1e-4) && stops_close(s, t),
        _ => false,
    }
}

#[test]
fn variable_paints_match_ttf_parser_at_every_axis_position() {
    let bytes = font();
    for coord in [0.0, 0.25, 0.5, 1.0] {
        for gid in 1..=10u16 {
            let (mine, _) = ours(&bytes, gid, coord);
            let (reference, _) = theirs(&bytes, gid, coord);
            assert_eq!(mine.len(), reference.len(), "gid {gid} at {coord}");
            for ((m, f), (n, g)) in mine.iter().zip(&reference) {
                assert!(
                    close(m, n, 1e-3),
                    "gid {gid} at {coord}: transform {m:?} vs {n:?}"
                );
                assert!(
                    fill_close(f, g),
                    "gid {gid} at {coord}: fill {f:?} vs {g:?}"
                );
            }
        }
    }
}

#[test]
fn variable_paints_do_vary() {
    // Guards the parity test against a fixture where nothing moves.
    let bytes = font();
    for gid in 1..=10u16 {
        let (at_default, _) = ours(&bytes, gid, 0.0);
        let (at_max, _) = ours(&bytes, gid, 1.0);
        assert_ne!(at_default, at_max, "gid {gid} ignores the axis");
    }
}

#[test]
fn variable_clip_box_matches_ttf_parser() {
    let bytes = font();
    // Deltas at these positions are whole units, so HarfBuzz's rounding
    // and ttf-parser's float sum agree.
    for coord in [0.0, 0.5, 1.0] {
        let (_, mine) = ours(&bytes, 1, coord);
        let (_, reference) = theirs(&bytes, 1, coord);
        let Some(RootClip::ClipBox {
            x_min,
            y_min,
            x_max,
            y_max,
        }) = mine
        else {
            panic!("glyph 1 has a ClipBox, got {mine:?}");
        };
        let mine = [x_min, y_min, x_max, y_max].map(|v| v as f32);
        let reference = reference.expect("ttf-parser clip box");
        assert!(close(&mine, &reference, 1e-3), "{mine:?} vs {reference:?}");
    }
}
