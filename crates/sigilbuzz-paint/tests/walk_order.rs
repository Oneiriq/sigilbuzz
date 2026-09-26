//! `walk::paint_glyph` reports paint trees in HarfBuzz's callback
//! order: the root clip, nested transforms, the root and inverse-root
//! transforms around glyph clips, clip boxes on referenced glyphs, two
//! groups per composite, biased sweep angles, and unresolved palette
//! references.

use core::f32::consts::PI;

use sigilbuzz::Face;
use sigilbuzz_paint::walk::{
    paint_glyph, paint_glyph_unclipped, ColorLineRef, ColorRef, PaintSink, Painted, RootClip,
    StopRef,
};
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

/// One ClipList record: glyphs `first..=last`, box
/// `[x_min, y_min, x_max, y_max]`, and a `varIndexBase` for a
/// `ClipBoxFormat2`.
type Clip = (u16, u16, [i16; 4], Option<u32>);

/// COLR with v0 base glyphs (`(gid, [(layer gid, entry)])`) and v1
/// paints (`(gid, paint bytes)`), with the 34-byte v1 header.
fn colr(v0: &[(u16, &[(u16, u16)])], v1: &[(u16, Vec<u8>)]) -> Vec<u8> {
    colr_with(v0, v1, &[], &[], &[])
}

/// [`colr`] plus a ClipList, a LayerList, and a variation store.
fn colr_with(
    v0: &[(u16, &[(u16, u16)])],
    v1: &[(u16, Vec<u8>)],
    clips: &[Clip],
    layers: &[Vec<u8>],
    var_store: &[u8],
) -> Vec<u8> {
    let header_len = 34usize;
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
    let layer_list_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let clip_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // index map
    let var_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
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
    if !layers.is_empty() {
        let at = out.len();
        out[layer_list_slot..layer_list_slot + 4].copy_from_slice(&(at as u32).to_be_bytes());
        out.extend_from_slice(&(layers.len() as u32).to_be_bytes());
        let mut rel = 4 + 4 * layers.len();
        for layer in layers {
            out.extend_from_slice(&(rel as u32).to_be_bytes());
            rel += layer.len();
        }
        for layer in layers {
            out.extend_from_slice(layer);
        }
    }
    if !clips.is_empty() {
        let at = out.len();
        out[clip_slot..clip_slot + 4].copy_from_slice(&(at as u32).to_be_bytes());
        out.push(1);
        out.extend_from_slice(&(clips.len() as u32).to_be_bytes());
        let mut box_at = 5 + 7 * clips.len();
        for (first, last, _, var) in clips {
            out.extend_from_slice(&first.to_be_bytes());
            out.extend_from_slice(&last.to_be_bytes());
            out.extend_from_slice(&(box_at as u32).to_be_bytes()[1..]);
            box_at += if var.is_some() { 13 } else { 9 };
        }
        for (_, _, coords, var) in clips {
            out.push(if var.is_some() { 2 } else { 1 });
            for v in coords {
                out.extend_from_slice(&v.to_be_bytes());
            }
            if let Some(base) = var {
                out.extend_from_slice(&base.to_be_bytes());
            }
        }
    }
    if !var_store.is_empty() {
        let at = out.len();
        out[var_slot..var_slot + 4].copy_from_slice(&(at as u32).to_be_bytes());
        out.extend_from_slice(var_store);
    }
    out
}

/// One-axis item variation store: a single region peaking at +1 and
/// one int16 delta per row.
fn ivs(rows: &[i16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&12u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&22u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&f2dot14(0.0));
    out.extend_from_slice(&f2dot14(1.0));
    out.extend_from_slice(&f2dot14(1.0));
    for v in [rows.len() as u16, 1, 1, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    for row in rows {
        out.extend_from_slice(&row.to_be_bytes());
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

fn colr_layers(count: u8, first: u32) -> Vec<u8> {
    let mut p = vec![1u8, count];
    p.extend_from_slice(&first.to_be_bytes());
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
    ClipRect([f32; 4]),
    RootClip(RootClip),
    PopClip,
    Group,
    PopGroup(CompositeMode),
    ColorGlyph(u16),
    Color(ColorRef),
    Linear(Vec<StopRef>, Extend, [f32; 6]),
    Sweep(Vec<StopRef>, (f32, f32), f32, f32),
    Radial,
}

#[derive(Default)]
struct Rec {
    events: Vec<Ev>,
    /// Glyphs `color_glyph` claims to paint itself.
    handles: Vec<u16>,
}

impl PaintSink for Rec {
    fn push_transform(&mut self, t: Transform2D) {
        self.events.push(Ev::Push(t));
    }
    fn push_root_transform(&mut self) {
        self.events.push(Ev::Root);
    }
    fn push_inverse_root_transform(&mut self) {
        self.events.push(Ev::InverseRoot);
    }
    fn pop_transform(&mut self) {
        self.events.push(Ev::Pop);
    }
    fn push_clip_glyph(&mut self, glyph: u16) {
        self.events.push(Ev::Clip(glyph));
    }
    fn push_clip_rectangle(&mut self, x_min: f32, y_min: f32, x_max: f32, y_max: f32) {
        self.events.push(Ev::ClipRect([x_min, y_min, x_max, y_max]));
    }
    fn push_root_clip(&mut self, clip: RootClip) {
        self.events.push(Ev::RootClip(clip));
    }
    fn pop_clip(&mut self) {
        self.events.push(Ev::PopClip);
    }
    fn push_group(&mut self) {
        self.events.push(Ev::Group);
    }
    fn pop_group(&mut self, mode: CompositeMode) {
        self.events.push(Ev::PopGroup(mode));
    }
    fn color_glyph(&mut self, glyph: u16) -> bool {
        self.events.push(Ev::ColorGlyph(glyph));
        self.handles.contains(&glyph)
    }
    fn color(&mut self, color: ColorRef) {
        self.events.push(Ev::Color(color));
    }
    fn linear_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        p0: (f32, f32),
        p1: (f32, f32),
        p2: (f32, f32),
    ) {
        let pts = [p0.0, p0.1, p1.0, p1.1, p2.0, p2.1];
        self.events
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
        self.events.push(Ev::Radial);
    }
    fn sweep_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        center: (f32, f32),
        start_angle: f32,
        end_angle: f32,
    ) {
        self.events.push(Ev::Sweep(
            line.stops.to_vec(),
            center,
            start_angle,
            end_angle,
        ));
    }
}

/// The unclipped walk: the paint tree alone, inside the root transform.
fn walk(bytes: &[u8], gid: u16) -> (Painted, Vec<Ev>) {
    let face = Face::parse_bytes(bytes, 0).expect("face parses");
    let mut rec = Rec::default();
    let painted = paint_glyph_unclipped(&face, gid, &[], &mut rec);
    (painted, rec.events)
}

/// The clipped walk HarfBuzz's `hb_font_paint_glyph` performs.
fn walk_clipped(bytes: &[u8], gid: u16, coords: &[f32], handles: &[u16]) -> Vec<Ev> {
    let face = Face::parse_bytes(bytes, 0).expect("face parses");
    let mut rec = Rec {
        handles: handles.to_vec(),
        ..Rec::default()
    };
    paint_glyph(&face, gid, coords, &mut rec);
    rec.events
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

fn clip_box(x_min: i32, y_min: i32, x_max: i32, y_max: i32) -> Ev {
    Ev::RootClip(RootClip::ClipBox {
        x_min,
        y_min,
        x_max,
        y_max,
    })
}

/// A `PaintColrGlyph(gid)` the sink declines: the color_glyph offer
/// inside the inverse root transform.
fn offered(gid: u16) -> Vec<Ev> {
    vec![Ev::InverseRoot, Ev::ColorGlyph(gid), Ev::Pop]
}

// =========================================================================
// Tests: the paint tree
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
fn colr_glyph_reference_is_offered_then_walked_and_stops_on_cycles() {
    // 7 -> PaintColrGlyph(8) -> solid; 9 -> PaintColrGlyph(9).
    let bytes = sfnt(&[(
        b"COLR",
        &colr(
            &[],
            &[(7, colr_glyph(8)), (8, solid(3, 1.0)), (9, colr_glyph(9))],
        ),
    )]);
    let mut want = vec![Ev::Root];
    want.extend(offered(8));
    want.extend([color(3, 1.0), Ev::Pop]);
    assert_eq!(walk(&bytes, 7).1, want);
    // A glyph already on the walk stack paints nothing at all.
    assert_eq!(walk(&bytes, 9).1, vec![Ev::Root, Ev::Pop]);
}

#[test]
fn colr_glyph_handled_by_the_sink_skips_its_tree() {
    // Glyphs 7 and 8 share a ClipBox; the sink paints 8 itself, so
    // neither 8's clip box nor its tree is walked.
    let clip = [(7, 8, [0, 0, 100, 100], None)];
    let paints = [(7, colr_glyph(8)), (8, solid(3, 1.0))];
    let bytes = sfnt(&[(b"COLR", &colr_with(&[], &paints, &clip, &[], &[]))]);
    let mut want = vec![clip_box(0, 0, 100, 100), Ev::Root];
    want.extend(offered(8));
    want.extend([Ev::Pop, Ev::PopClip]);
    assert_eq!(walk_clipped(&bytes, 7, &[], &[8]), want);
}

#[test]
fn colr_layers_walk_in_order_and_skip_layers_on_the_stack() {
    // Layer 0 is a solid; layer 1 is PaintColrLayers(0..2) again, whose
    // own layer 1 is on the stack and is skipped.
    let layers = [solid(1, 1.0), colr_layers(2, 0)];
    let bytes = sfnt(&[(
        b"COLR",
        &colr_with(&[], &[(7, colr_layers(2, 0))], &[], &layers, &[]),
    )]);
    let (_, events) = walk(&bytes, 7);
    assert_eq!(
        events,
        vec![Ev::Root, color(1, 1.0), color(1, 1.0), Ev::Pop]
    );
}

#[test]
fn colr_v0_layers_are_clip_color_pop_triples() {
    let layers: &[(u16, u16)] = &[(20, 1), (21, FOREGROUND)];
    let bytes = sfnt(&[(b"COLR", &colr(&[(5, layers)], &[]))]);
    let (painted, events) = walk(&bytes, 5);
    assert_eq!(painted, Painted::ColrV0);
    let expected = vec![
        Ev::Clip(20),
        color(1, 1.0),
        Ev::PopClip,
        Ev::Clip(21),
        color(FOREGROUND, 1.0),
        Ev::PopClip,
    ];
    assert_eq!(events, expected);
    // COLRv0 glyphs get no root clip.
    assert_eq!(walk_clipped(&bytes, 5, &[], &[]), expected);
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

// =========================================================================
// Tests: the root clip
// =========================================================================

#[test]
fn clip_box_clips_the_whole_glyph_outside_the_root_transform() {
    let clips = [(5, 9, [-10, -20, 500, 600], None)];
    let bytes = sfnt(&[(
        b"COLR",
        &colr_with(&[], &[(7, solid(0, 1.0))], &clips, &[], &[]),
    )]);
    // The ClipBox bounds even a bare solid, so it paints.
    assert_eq!(
        walk_clipped(&bytes, 7, &[], &[]),
        vec![
            clip_box(-10, -20, 500, 600),
            Ev::Root,
            color(0, 1.0),
            Ev::Pop,
            Ev::PopClip,
        ]
    );
}

#[test]
fn unbounded_glyphs_paint_nothing() {
    // No ClipBox, and the solid escapes every clip.
    let bytes = sfnt(&[(b"COLR", &colr(&[], &[(7, solid(0, 1.0))]))]);
    let events = walk_clipped(&bytes, 7, &[], &[]);
    assert!(
        matches!(
            events[0],
            Ev::RootClip(RootClip::Extents { bounded: false, .. })
        ),
        "{events:?}"
    );
    assert_eq!(events[1..], [Ev::Root, Ev::Pop, Ev::PopClip]);
}

#[test]
fn computed_bounds_follow_clip_boxes_of_referenced_glyphs() {
    // 7 has no ClipBox: its bounds come from the paint tree, here the
    // box of the referenced glyph 8, moved by the translate above it.
    let clips = [(8, 8, [0, 0, 100, 50], None)];
    let paints = [(7, translate(10, 20, &colr_glyph(8))), (8, solid(1, 0.5))];
    let bytes = sfnt(&[(b"COLR", &colr_with(&[], &paints, &clips, &[], &[]))]);
    let events = walk_clipped(&bytes, 7, &[], &[]);
    let mut want = vec![
        Ev::RootClip(RootClip::Extents {
            x_min: 10.0,
            y_min: 20.0,
            x_max: 110.0,
            y_max: 70.0,
            bounded: true,
        }),
        Ev::Root,
        Ev::Push(Transform2D::translate(10.0, 20.0)),
    ];
    want.extend(offered(8));
    want.extend([
        Ev::ClipRect([0.0, 0.0, 100.0, 50.0]),
        color(1, 0.5),
        Ev::PopClip,
        Ev::Pop,
        Ev::Pop,
        Ev::PopClip,
    ]);
    assert_eq!(events, want);
}

#[test]
fn source_in_composites_intersect_computed_bounds() {
    // SrcIn keeps only where both groups are: the two ClipBoxes overlap
    // on [50, 100] x [0, 100].
    let clips = [
        (8, 8, [0, 0, 100, 100], None),
        (9, 9, [50, 0, 150, 100], None),
    ];
    let paints = [
        (7, composite(&colr_glyph(9), 5, &colr_glyph(8))),
        (8, solid(0, 1.0)),
        (9, solid(1, 1.0)),
    ];
    let bytes = sfnt(&[(b"COLR", &colr_with(&[], &paints, &clips, &[], &[]))]);
    let events = walk_clipped(&bytes, 7, &[], &[]);
    assert_eq!(
        events[0],
        Ev::RootClip(RootClip::Extents {
            x_min: 50.0,
            y_min: 0.0,
            x_max: 100.0,
            y_max: 100.0,
            bounded: true,
        })
    );
}

#[test]
fn variable_clip_boxes_take_rounded_deltas() {
    // ClipBoxFormat2 with varIndexBase 0: rows 0..3 move each edge.
    // At coordinate 0.5 the deltas are 5, -3.5, 10.5, -0.5: rounded
    // half up to 5, -3, 11, 0.
    let clips = [(7, 7, [0, 0, 100, 100], Some(0))];
    let store = ivs(&[10, -7, 21, -1]);
    let bytes = sfnt(&[(
        b"COLR",
        &colr_with(&[], &[(7, solid(0, 1.0))], &clips, &[], &store),
    )]);
    assert_eq!(
        walk_clipped(&bytes, 7, &[0.5], &[])[0],
        clip_box(5, -3, 111, 100)
    );
    assert_eq!(
        walk_clipped(&bytes, 7, &[], &[])[0],
        clip_box(0, 0, 100, 100)
    );
}
