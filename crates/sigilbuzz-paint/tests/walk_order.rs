//! `walk::paint_glyph` reports paint trees in HarfBuzz's callback
//! order: nested transforms, the root and inverse-root transforms
//! around glyph clips, two groups per composite, biased sweep angles,
//! and unresolved palette references.

use core::f32::consts::PI;

use sigilbuzz::Face;
use sigilbuzz_paint::walk::{paint_glyph, ColorLineRef, ColorRef, PaintSink, Painted, StopRef};
use sigilbuzz_paint::{CompositeMode, Extend, Transform2D};

// =========================================================================
// Fixture builders
// =========================================================================

const FOREGROUND: u16 = 0xFFFF;

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

fn sfnt(tables: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let mut offset = 12 + 16 * tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    for (tag, body) in tables {
        out.extend_from_slice(*tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        offset += body.len();
    }
    for (_, body) in tables {
        out.extend_from_slice(body);
    }
    out
}

/// COLR with v0 base glyphs (`(gid, [(layer gid, entry)])`) and v1
/// paints (`(gid, paint bytes)`), in the header layout sigilbuzz reads.
fn colr(v0: &[(u16, &[(u16, u16)])], v1: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let header_len = 30usize;
    let base_off = header_len;
    let layer_off = base_off + 6 * v0.len();
    let num_layers: usize = v0.iter().map(|(_, l)| l.len()).sum();
    let list_off = layer_off + 4 * num_layers;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(v0.len() as u16).to_be_bytes());
    out.extend_from_slice(&(base_off as u32).to_be_bytes());
    out.extend_from_slice(&(layer_off as u32).to_be_bytes());
    out.extend_from_slice(&(num_layers as u16).to_be_bytes());
    out.extend_from_slice(&(list_off as u32).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // layer list
    out.extend_from_slice(&0u32.to_be_bytes()); // clip list
    out.extend_from_slice(&0u32.to_be_bytes()); // var store
    let mut first = 0u16;
    for (gid, layers) in v0 {
        out.extend_from_slice(&gid.to_be_bytes());
        out.extend_from_slice(&first.to_be_bytes());
        out.extend_from_slice(&(layers.len() as u16).to_be_bytes());
        first += layers.len() as u16;
    }
    for (_, layers) in v0 {
        for (gid, entry) in *layers {
            out.extend_from_slice(&gid.to_be_bytes());
            out.extend_from_slice(&entry.to_be_bytes());
        }
    }
    out.extend_from_slice(&(v1.len() as u32).to_be_bytes());
    let records = out.len();
    for (gid, _) in v1 {
        out.extend_from_slice(&gid.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    for (i, (_, bytes)) in v1.iter().enumerate() {
        let rel = (out.len() - list_off) as u32;
        let slot = records + i * 6 + 2;
        out[slot..slot + 4].copy_from_slice(&rel.to_be_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

fn solid(entry: u16, alpha: f32) -> Vec<u8> {
    let mut p = vec![2u8];
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(alpha));
    p
}

/// A paint whose Offset24 at byte 1 points at `child`, appended.
fn parent(mut head: Vec<u8>, child: &[u8]) -> Vec<u8> {
    let at = head.len();
    set_offset24(&mut head, 1, at);
    head.extend_from_slice(child);
    head
}

fn paint_glyph_node(gid: u16, child: &[u8]) -> Vec<u8> {
    let mut head = vec![10u8, 0, 0, 0];
    head.extend_from_slice(&gid.to_be_bytes());
    parent(head, child)
}

fn colr_glyph(gid: u16) -> Vec<u8> {
    let mut p = vec![11u8];
    p.extend_from_slice(&gid.to_be_bytes());
    p
}

/// PaintTransform: paint offset, Affine2x3 offset, affine, child.
fn transform(m: [f32; 6], child: &[u8]) -> Vec<u8> {
    let mut p = vec![12u8, 0, 0, 0, 0, 0, 0];
    set_offset24(&mut p, 4, 7);
    for v in m {
        p.extend_from_slice(&fixed(v));
    }
    let at = p.len();
    set_offset24(&mut p, 1, at);
    p.extend_from_slice(child);
    p
}

fn rotate_around(angle: f32, cx: i16, cy: i16, child: &[u8]) -> Vec<u8> {
    let mut head = vec![26u8, 0, 0, 0];
    head.extend_from_slice(&f2dot14(angle));
    head.extend_from_slice(&cx.to_be_bytes());
    head.extend_from_slice(&cy.to_be_bytes());
    parent(head, child)
}

fn translate(dx: i16, dy: i16, child: &[u8]) -> Vec<u8> {
    let mut head = vec![14u8, 0, 0, 0];
    head.extend_from_slice(&dx.to_be_bytes());
    head.extend_from_slice(&dy.to_be_bytes());
    parent(head, child)
}

fn color_line(stops: &[(f32, u16, f32)]) -> Vec<u8> {
    let mut p = vec![1u8]; // Repeat
    p.extend_from_slice(&(stops.len() as u16).to_be_bytes());
    for (offset, entry, alpha) in stops {
        p.extend_from_slice(&f2dot14(*offset));
        p.extend_from_slice(&entry.to_be_bytes());
        p.extend_from_slice(&f2dot14(*alpha));
    }
    p
}

fn linear(stops: &[(f32, u16, f32)]) -> Vec<u8> {
    let mut p = vec![4u8, 0, 0, 0];
    for v in [0i16, 0, 100, 0, 0, 100] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    let at = p.len();
    set_offset24(&mut p, 1, at);
    p.extend_from_slice(&color_line(stops));
    p
}

fn sweep(start: f32, end: f32) -> Vec<u8> {
    let mut p = vec![8u8, 0, 0, 0];
    p.extend_from_slice(&10i16.to_be_bytes());
    p.extend_from_slice(&20i16.to_be_bytes());
    p.extend_from_slice(&f2dot14(start));
    p.extend_from_slice(&f2dot14(end));
    let at = p.len();
    set_offset24(&mut p, 1, at);
    p.extend_from_slice(&color_line(&[(0.0, 0, 1.0)]));
    p
}

/// PaintComposite: source offset, mode, backdrop offset.
fn composite(source: &[u8], mode: u8, backdrop: &[u8]) -> Vec<u8> {
    let mut p = vec![32u8, 0, 0, 0, mode, 0, 0, 0];
    let src_at = p.len();
    set_offset24(&mut p, 1, src_at);
    p.extend_from_slice(source);
    let back_at = p.len();
    set_offset24(&mut p, 5, back_at);
    p.extend_from_slice(backdrop);
    p
}

// =========================================================================
// Recorder
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Push(Transform2D),
    Root,
    InverseRoot,
    Pop,
    Clip(u16),
    PopClip,
    Group,
    PopGroup(CompositeMode),
    Color(ColorRef),
    Linear(Vec<StopRef>, Extend, [f32; 6]),
    Sweep(Vec<StopRef>, (f32, f32), f32, f32),
    Radial,
}

#[derive(Default)]
struct Rec(Vec<Ev>);

impl PaintSink for Rec {
    fn push_transform(&mut self, t: Transform2D) {
        self.0.push(Ev::Push(t));
    }
    fn push_root_transform(&mut self) {
        self.0.push(Ev::Root);
    }
    fn push_inverse_root_transform(&mut self) {
        self.0.push(Ev::InverseRoot);
    }
    fn pop_transform(&mut self) {
        self.0.push(Ev::Pop);
    }
    fn push_clip_glyph(&mut self, glyph: u16) {
        self.0.push(Ev::Clip(glyph));
    }
    fn pop_clip(&mut self) {
        self.0.push(Ev::PopClip);
    }
    fn push_group(&mut self) {
        self.0.push(Ev::Group);
    }
    fn pop_group(&mut self, mode: CompositeMode) {
        self.0.push(Ev::PopGroup(mode));
    }
    fn color(&mut self, color: ColorRef) {
        self.0.push(Ev::Color(color));
    }
    fn linear_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        p0: (f32, f32),
        p1: (f32, f32),
        p2: (f32, f32),
    ) {
        let pts = [p0.0, p0.1, p1.0, p1.1, p2.0, p2.1];
        self.0
            .push(Ev::Linear(line.stops.to_vec(), line.extend, pts));
    }
    fn radial_gradient(
        &mut self,
        _line: ColorLineRef<'_>,
        _c0: (f32, f32),
        _r0: f32,
        _c1: (f32, f32),
        _r1: f32,
    ) {
        self.0.push(Ev::Radial);
    }
    fn sweep_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        center: (f32, f32),
        start_angle: f32,
        end_angle: f32,
    ) {
        self.0.push(Ev::Sweep(
            line.stops.to_vec(),
            center,
            start_angle,
            end_angle,
        ));
    }
}

fn walk(bytes: &[u8], gid: u16) -> (Painted, Vec<Ev>) {
    let face = Face::parse_bytes(bytes, 0).expect("face parses");
    let mut rec = Rec::default();
    let painted = paint_glyph(&face, gid, &[], &mut rec);
    (painted, rec.0)
}

fn color(palette_entry: u16, alpha: f32) -> Ev {
    Ev::Color(ColorRef {
        palette_entry,
        alpha,
    })
}

fn stop(offset: f32, palette_entry: u16, alpha: f32) -> StopRef {
    StopRef {
        offset,
        color: ColorRef {
            palette_entry,
            alpha,
        },
    }
}

// =========================================================================
// Tests
// =========================================================================

#[test]
fn glyph_clip_sits_outside_the_transform_below_it() {
    // PaintGlyph(42) -> PaintTransform(scale 2) -> linear gradient.
    // The transform under the clip applies to the gradient only.
    let grad = linear(&[(0.0, 0, 1.0), (1.0, FOREGROUND, 0.5)]);
    let scaled = transform([2.0, 0.0, 0.0, 2.0, 5.0, 0.0], &grad);
    let bytes = sfnt(&[(b"COLR", &colr(&[], &[(7, paint_glyph_node(42, &scaled))]))]);
    let (painted, events) = walk(&bytes, 7);
    assert_eq!(painted, Painted::ColrV1);
    let m = Transform2D {
        xx: 2.0,
        yx: 0.0,
        xy: 0.0,
        yy: 2.0,
        dx: 5.0,
        dy: 0.0,
    };
    let stops = vec![stop(0.0, 0, 1.0), stop(1.0, FOREGROUND, 0.5)];
    assert_eq!(
        events,
        vec![
            Ev::Root,
            Ev::InverseRoot,
            Ev::Clip(42),
            Ev::Root,
            Ev::Push(m),
            Ev::Linear(stops, Extend::Repeat, [0.0, 0.0, 100.0, 0.0, 0.0, 100.0]),
            Ev::Pop,
            Ev::Pop,
            Ev::PopClip,
            Ev::Pop,
            Ev::Pop,
        ]
    );
}

#[test]
fn composite_wraps_backdrop_and_source_in_two_groups() {
    let src = paint_glyph_node(2, &solid(1, 1.0));
    let back = paint_glyph_node(1, &solid(0, 0.25));
    let bytes = sfnt(&[(b"COLR", &colr(&[], &[(7, composite(&src, 6, &back))]))]);
    let (_, events) = walk(&bytes, 7);
    let clipped = |gid, c: Ev| {
        vec![
            Ev::InverseRoot,
            Ev::Clip(gid),
            Ev::Root,
            c,
            Ev::Pop,
            Ev::PopClip,
            Ev::Pop,
        ]
    };
    let mut want = vec![Ev::Root, Ev::Group];
    want.extend(clipped(1, color(0, 0.25)));
    want.push(Ev::Group);
    want.extend(clipped(2, color(1, 1.0)));
    want.extend([
        Ev::PopGroup(CompositeMode::DestIn),
        Ev::PopGroup(CompositeMode::SrcOver),
        Ev::Pop,
    ]);
    assert_eq!(events, want);
}

#[test]
fn around_center_pushes_translate_op_translate() {
    // Rotate a quarter turn around (10, 20).
    let bytes = sfnt(&[(
        b"COLR",
        &colr(&[], &[(7, rotate_around(0.5, 10, 20, &solid(0, 1.0)))]),
    )]);
    let (_, events) = walk(&bytes, 7);
    let (s, c) = ((0.5 * PI).sin(), (0.5 * PI).cos());
    let rot = Transform2D {
        xx: c,
        yx: s,
        xy: -s,
        yy: c,
        dx: 0.0,
        dy: 0.0,
    };
    assert_eq!(
        events,
        vec![
            Ev::Root,
            Ev::Push(Transform2D::translate(10.0, 20.0)),
            Ev::Push(rot),
            Ev::Push(Transform2D::translate(-10.0, -20.0)),
            color(0, 1.0),
            Ev::Pop,
            Ev::Pop,
            Ev::Pop,
            Ev::Pop,
        ]
    );
}

#[test]
fn identity_operations_push_nothing() {
    // A zero rotation around the origin and a zero translation.
    let inner = rotate_around(0.0, 0, 0, &solid(0, 1.0));
    let bytes = sfnt(&[(b"COLR", &colr(&[], &[(7, translate(0, 0, &inner))]))]);
    let (_, events) = walk(&bytes, 7);
    assert_eq!(events, vec![Ev::Root, color(0, 1.0), Ev::Pop]);
}

#[test]
fn sweep_angles_are_biased_radians() {
    let bytes = sfnt(&[(b"COLR", &colr(&[], &[(7, sweep(-1.0, 0.5))]))]);
    let (_, events) = walk(&bytes, 7);
    assert_eq!(
        events,
        vec![
            Ev::Root,
            Ev::Sweep(vec![stop(0.0, 0, 1.0)], (10.0, 20.0), 0.0, 1.5 * PI),
            Ev::Pop,
        ]
    );
}

#[test]
fn colr_glyph_reference_recurses_and_stops_on_cycles() {
    // 7 -> PaintColrGlyph(8) -> solid; 9 -> PaintColrGlyph(9).
    let bytes = sfnt(&[(
        b"COLR",
        &colr(
            &[],
            &[(7, colr_glyph(8)), (8, solid(3, 1.0)), (9, colr_glyph(9))],
        ),
    )]);
    assert_eq!(walk(&bytes, 7).1, vec![Ev::Root, color(3, 1.0), Ev::Pop]);
    assert_eq!(walk(&bytes, 9).1, vec![Ev::Root, Ev::Pop]);
}

#[test]
fn colr_v0_layers_are_clip_color_pop_triples() {
    let layers: &[(u16, u16)] = &[(20, 1), (21, FOREGROUND)];
    let bytes = sfnt(&[(b"COLR", &colr(&[(5, layers)], &[]))]);
    let (painted, events) = walk(&bytes, 5);
    assert_eq!(painted, Painted::ColrV0);
    assert_eq!(
        events,
        vec![
            Ev::Clip(20),
            color(1, 1.0),
            Ev::PopClip,
            Ev::Clip(21),
            color(FOREGROUND, 1.0),
            Ev::PopClip,
        ]
    );
}

#[test]
fn colr_v1_wins_over_v0_for_the_same_glyph() {
    let layers: &[(u16, u16)] = &[(20, 1)];
    let bytes = sfnt(&[(b"COLR", &colr(&[(5, layers)], &[(5, solid(2, 1.0))]))]);
    let (painted, events) = walk(&bytes, 5);
    assert_eq!(painted, Painted::ColrV1);
    assert_eq!(events, vec![Ev::Root, color(2, 1.0), Ev::Pop]);
}

#[test]
fn glyphs_without_color_data_report_nothing() {
    let bytes = sfnt(&[(b"COLR", &colr(&[], &[(7, solid(0, 1.0))]))]);
    assert_eq!(walk(&bytes, 8), (Painted::Nothing, vec![]));
    // No COLR table at all.
    let bytes = sfnt(&[(b"CPAL", &[0u8; 14])]);
    assert_eq!(walk(&bytes, 7), (Painted::Nothing, vec![]));
}

#[test]
fn broken_child_offset_still_balances_pushes() {
    // PaintGlyph whose child offset points past the table.
    let mut p = vec![10u8, 0, 0, 0];
    p.extend_from_slice(&42u16.to_be_bytes());
    set_offset24(&mut p, 1, 0x00FF_FFFF);
    let bytes = sfnt(&[(b"COLR", &colr(&[], &[(7, p)]))]);
    let (_, events) = walk(&bytes, 7);
    assert_eq!(
        events,
        vec![
            Ev::Root,
            Ev::InverseRoot,
            Ev::Clip(42),
            Ev::Root,
            Ev::Pop,
            Ev::PopClip,
            Ev::Pop,
            Ev::Pop,
        ]
    );
}
