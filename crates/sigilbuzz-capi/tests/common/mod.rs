//! Shared fixtures and a callback recorder for the `hb_paint_*` tests.
//!
//! Fixtures are hand-built SFNTs carrying COLR (v0 records and v1
//! paints, in the header layout sigilbuzz reads), CPAL, and optionally
//! fvar. The recorder installs every paint callback with its own
//! `user_data` tag, logs each call into the `Log` passed as
//! `paint_data`, and checks that every callback received its own tag.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use core::cell::RefCell;
use core::ffi::{c_char, c_uint, c_void};
use core::ptr;

use sigilbuzz_capi::paint_bridge::{
    hb_color_line_get_color_stops, hb_color_line_get_extend, hb_color_line_t, hb_color_stop_t,
    hb_color_t, hb_font_paint_glyph, hb_paint_composite_mode_t, hb_paint_funcs_create,
    hb_paint_funcs_destroy, hb_paint_funcs_set_color_func, hb_paint_funcs_set_linear_gradient_func,
    hb_paint_funcs_set_pop_clip_func, hb_paint_funcs_set_pop_group_func,
    hb_paint_funcs_set_pop_transform_func, hb_paint_funcs_set_push_clip_glyph_func,
    hb_paint_funcs_set_push_group_func, hb_paint_funcs_set_push_transform_func,
    hb_paint_funcs_set_radial_gradient_func, hb_paint_funcs_set_sweep_gradient_func,
    hb_paint_funcs_t,
};
use sigilbuzz_capi::{
    hb_blob_create, hb_blob_destroy, hb_face_create, hb_face_destroy, hb_font_create,
    hb_font_destroy, hb_font_t, HB_MEMORY_MODE_READONLY,
};

// =========================================================================
// Fixture builders
// =========================================================================

pub const FOREGROUND: u16 = 0xFFFF;

pub fn f2dot14(v: f32) -> [u8; 2] {
    ((v * 16384.0).round() as i16).to_be_bytes()
}

pub fn fixed(v: f32) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn set_offset24(p: &mut [u8], at: usize, target: usize) {
    let v = target as u32;
    p[at..at + 3].copy_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
}

pub fn sfnt(tables: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
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

/// CPAL v0; every palette must have the same number of `(r, g, b, a)`
/// entries.
pub fn cpal(palettes: &[&[(u8, u8, u8, u8)]]) -> Vec<u8> {
    let entries = palettes[0].len() as u16;
    let count = palettes.len() as u16;
    let mut out = Vec::new();
    for v in [0u16, entries, count, entries * count] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&(12 + 2 * u32::from(count)).to_be_bytes());
    for i in 0..count {
        out.extend_from_slice(&(i * entries).to_be_bytes());
    }
    for palette in palettes {
        for (r, g, b, a) in *palette {
            out.extend_from_slice(&[*b, *g, *r, *a]);
        }
    }
    out
}

/// fvar with a single `wght` axis: min 100, default 400, max 900.
pub fn fvar() -> Vec<u8> {
    let mut out = Vec::new();
    for v in [1u16, 0, 16, 2, 1, 20, 0, 4] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(b"wght");
    out.extend_from_slice(&fixed(100.0));
    out.extend_from_slice(&fixed(400.0));
    out.extend_from_slice(&fixed(900.0));
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&256u16.to_be_bytes());
    out
}

/// One-axis item variation store: one region peaking at +1 and one
/// int16 delta per row.
pub fn ivs(rows: &[i16]) -> Vec<u8> {
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

/// COLR with v0 base glyphs (`(gid, [(layer gid, entry)])`), v1 paints
/// (`(gid, paint bytes)`, sorted by gid), and an optional variation
/// store, in the header layout sigilbuzz reads.
pub fn colr(v0: &[(u16, &[(u16, u16)])], v1: &[(u16, Vec<u8>)], var_store: &[u8]) -> Vec<u8> {
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
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
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
    if !var_store.is_empty() {
        let off = out.len() as u32;
        out[var_slot..var_slot + 4].copy_from_slice(&off.to_be_bytes());
        out.extend_from_slice(var_store);
    }
    out
}

pub fn solid(entry: u16, alpha: f32) -> Vec<u8> {
    let mut p = vec![2u8];
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(alpha));
    p
}

/// PaintVarSolid reading its alpha delta from IVS row `row`.
pub fn var_solid(entry: u16, alpha: f32, row: u32) -> Vec<u8> {
    let mut p = vec![3u8];
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(alpha));
    p.extend_from_slice(&row.to_be_bytes());
    p
}

fn parent(mut head: Vec<u8>, child: &[u8]) -> Vec<u8> {
    let at = head.len();
    set_offset24(&mut head, 1, at);
    head.extend_from_slice(child);
    head
}

/// PaintGlyph clipping `child` to `gid`.
pub fn glyph(gid: u16, child: &[u8]) -> Vec<u8> {
    let mut head = vec![10u8, 0, 0, 0];
    head.extend_from_slice(&gid.to_be_bytes());
    parent(head, child)
}

/// PaintTransform with an Affine2x3.
pub fn transform(m: [f32; 6], child: &[u8]) -> Vec<u8> {
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

/// PaintScaleUniformAroundCenter.
pub fn scale_around(scale: f32, cx: i16, cy: i16, child: &[u8]) -> Vec<u8> {
    let mut head = vec![22u8, 0, 0, 0];
    head.extend_from_slice(&f2dot14(scale));
    head.extend_from_slice(&cx.to_be_bytes());
    head.extend_from_slice(&cy.to_be_bytes());
    parent(head, child)
}

/// PaintComposite of `source` over `backdrop` with COLR mode `mode`.
pub fn composite(source: &[u8], mode: u8, backdrop: &[u8]) -> Vec<u8> {
    let mut p = vec![32u8, 0, 0, 0, mode, 0, 0, 0];
    let src_at = p.len();
    set_offset24(&mut p, 1, src_at);
    p.extend_from_slice(source);
    let back_at = p.len();
    set_offset24(&mut p, 5, back_at);
    p.extend_from_slice(backdrop);
    p
}

/// ColorLine with extend `extend` and `(offset, entry, alpha)` stops.
pub fn color_line(extend: u8, stops: &[(f32, u16, f32)]) -> Vec<u8> {
    let mut p = vec![extend];
    p.extend_from_slice(&(stops.len() as u16).to_be_bytes());
    for (offset, entry, alpha) in stops {
        p.extend_from_slice(&f2dot14(*offset));
        p.extend_from_slice(&entry.to_be_bytes());
        p.extend_from_slice(&f2dot14(*alpha));
    }
    p
}

/// PaintLinearGradient with points (0, 0), (100, 0), (0, 100).
pub fn linear(stops: &[(f32, u16, f32)]) -> Vec<u8> {
    let mut p = vec![4u8, 0, 0, 0];
    for v in [0i16, 0, 100, 0, 0, 100] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    let at = p.len();
    set_offset24(&mut p, 1, at);
    p.extend_from_slice(&color_line(0, stops));
    p
}

/// PaintRadialGradient: circles (10, 20, r 5) and (30, 40, r 50).
pub fn radial(stops: &[(f32, u16, f32)]) -> Vec<u8> {
    let mut p = vec![6u8, 0, 0, 0];
    for v in [10i16, 20] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.extend_from_slice(&5u16.to_be_bytes());
    for v in [30i16, 40] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.extend_from_slice(&50u16.to_be_bytes());
    let at = p.len();
    set_offset24(&mut p, 1, at);
    p.extend_from_slice(&color_line(2, stops));
    p
}

/// PaintSweepGradient centered at (50, 60) with the stored angles.
pub fn sweep(start: f32, end: f32, stops: &[(f32, u16, f32)]) -> Vec<u8> {
    let mut p = vec![8u8, 0, 0, 0];
    p.extend_from_slice(&50i16.to_be_bytes());
    p.extend_from_slice(&60i16.to_be_bytes());
    p.extend_from_slice(&f2dot14(start));
    p.extend_from_slice(&f2dot14(end));
    let at = p.len();
    set_offset24(&mut p, 1, at);
    p.extend_from_slice(&color_line(1, stops));
    p
}

// =========================================================================
// Recorder
// =========================================================================

/// `HB_COLOR(b, g, r, a)`.
pub const fn hb_color(b: u8, g: u8, r: u8, a: u8) -> hb_color_t {
    ((b as u32) << 24) | ((g as u32) << 16) | ((r as u32) << 8) | (a as u32)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ev {
    PushTransform([f32; 6]),
    PopTransform,
    PushClipGlyph(u32),
    PopClip,
    Color(i32, hb_color_t),
    Linear(Vec<hb_color_stop_t>, c_uint, [f32; 6]),
    Radial(Vec<hb_color_stop_t>, c_uint, [f32; 6]),
    Sweep(Vec<hb_color_stop_t>, c_uint, [f32; 4]),
    PushGroup,
    PopGroup(hb_paint_composite_mode_t),
    /// Logged by tests that install `custom_palette_color`.
    CustomPalette(c_uint),
}

/// Everything one paint call reported, plus the font pointer each
/// `push_clip_glyph` received and any `user_data` mismatch.
#[derive(Default)]
pub struct Log {
    pub events: RefCell<Vec<Ev>>,
    pub clip_fonts: RefCell<Vec<usize>>,
    pub bad_user_data: RefCell<Vec<&'static str>>,
}

pub const TAG_PUSH_TRANSFORM: usize = 0x101;
pub const TAG_POP_TRANSFORM: usize = 0x102;
pub const TAG_PUSH_CLIP_GLYPH: usize = 0x103;
pub const TAG_POP_CLIP: usize = 0x104;
pub const TAG_COLOR: usize = 0x105;
pub const TAG_LINEAR: usize = 0x106;
pub const TAG_RADIAL: usize = 0x107;
pub const TAG_SWEEP: usize = 0x108;
pub const TAG_PUSH_GROUP: usize = 0x109;
pub const TAG_POP_GROUP: usize = 0x10A;

/// # Safety
/// `data` must be the `&Log` the test passed as `paint_data`.
pub unsafe fn log<'a>(data: *mut c_void) -> &'a Log {
    // SAFETY: see the function contract.
    unsafe { &*data.cast::<Log>().cast_const() }
}

fn record(data: *mut c_void, user_data: *mut c_void, tag: usize, name: &'static str, ev: Ev) {
    // SAFETY: every test passes a live `Log` as paint_data.
    let log = unsafe { log(data) };
    if user_data as usize != tag {
        log.bad_user_data.borrow_mut().push(name);
    }
    log.events.borrow_mut().push(ev);
}

/// Reads every stop and the extend mode from a live color line.
///
/// # Safety
/// `line` must be the color line of a running gradient callback.
pub unsafe fn read_line(line: *mut hb_color_line_t) -> (Vec<hb_color_stop_t>, c_uint) {
    // SAFETY: forwarded from the caller; the buffer has room for
    // `total` stops.
    unsafe {
        let total = hb_color_line_get_color_stops(line, 0, ptr::null_mut(), ptr::null_mut());
        let blank = hb_color_stop_t {
            offset: -1.0,
            is_foreground: -1,
            color: 0,
        };
        let mut stops = vec![blank; total as usize];
        let mut count = total;
        hb_color_line_get_color_stops(line, 0, &mut count, stops.as_mut_ptr());
        stops.truncate(count as usize);
        (stops, hb_color_line_get_extend(line))
    }
}

unsafe extern "C" fn on_push_transform(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    xx: f32,
    yx: f32,
    xy: f32,
    yy: f32,
    dx: f32,
    dy: f32,
    user_data: *mut c_void,
) {
    let ev = Ev::PushTransform([xx, yx, xy, yy, dx, dy]);
    record(data, user_data, TAG_PUSH_TRANSFORM, "push_transform", ev);
}

unsafe extern "C" fn on_pop_transform(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    user_data: *mut c_void,
) {
    record(
        data,
        user_data,
        TAG_POP_TRANSFORM,
        "pop_transform",
        Ev::PopTransform,
    );
}

unsafe extern "C" fn on_push_clip_glyph(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    glyph: u32,
    font: *mut hb_font_t,
    user_data: *mut c_void,
) {
    // SAFETY: every test passes a live `Log` as paint_data.
    unsafe { log(data) }
        .clip_fonts
        .borrow_mut()
        .push(font as usize);
    let ev = Ev::PushClipGlyph(glyph);
    record(data, user_data, TAG_PUSH_CLIP_GLYPH, "push_clip_glyph", ev);
}

unsafe extern "C" fn on_pop_clip(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    user_data: *mut c_void,
) {
    record(data, user_data, TAG_POP_CLIP, "pop_clip", Ev::PopClip);
}

unsafe extern "C" fn on_color(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    is_foreground: i32,
    color: hb_color_t,
    user_data: *mut c_void,
) {
    let ev = Ev::Color(is_foreground, color);
    record(data, user_data, TAG_COLOR, "color", ev);
}

unsafe extern "C" fn on_linear(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    line: *mut hb_color_line_t,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    user_data: *mut c_void,
) {
    // SAFETY: `line` is this callback's live color line.
    let (stops, extend) = unsafe { read_line(line) };
    let ev = Ev::Linear(stops, extend, [x0, y0, x1, y1, x2, y2]);
    record(data, user_data, TAG_LINEAR, "linear_gradient", ev);
}

unsafe extern "C" fn on_radial(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    line: *mut hb_color_line_t,
    x0: f32,
    y0: f32,
    r0: f32,
    x1: f32,
    y1: f32,
    r1: f32,
    user_data: *mut c_void,
) {
    // SAFETY: `line` is this callback's live color line.
    let (stops, extend) = unsafe { read_line(line) };
    let ev = Ev::Radial(stops, extend, [x0, y0, r0, x1, y1, r1]);
    record(data, user_data, TAG_RADIAL, "radial_gradient", ev);
}

unsafe extern "C" fn on_sweep(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    line: *mut hb_color_line_t,
    cx: f32,
    cy: f32,
    start: f32,
    end: f32,
    user_data: *mut c_void,
) {
    // SAFETY: `line` is this callback's live color line.
    let (stops, extend) = unsafe { read_line(line) };
    let ev = Ev::Sweep(stops, extend, [cx, cy, start, end]);
    record(data, user_data, TAG_SWEEP, "sweep_gradient", ev);
}

unsafe extern "C" fn on_push_group(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    user_data: *mut c_void,
) {
    record(data, user_data, TAG_PUSH_GROUP, "push_group", Ev::PushGroup);
}

unsafe extern "C" fn on_pop_group(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    mode: hb_paint_composite_mode_t,
    user_data: *mut c_void,
) {
    record(
        data,
        user_data,
        TAG_POP_GROUP,
        "pop_group",
        Ev::PopGroup(mode),
    );
}

fn tag(v: usize) -> *mut c_void {
    v as *mut c_void
}

/// A paint-funcs table with every recording callback installed, each
/// with its own `user_data` tag.
pub fn recording_funcs() -> *mut hb_paint_funcs_t {
    let f = hb_paint_funcs_create();
    // SAFETY: `f` is live; the tags are never dereferenced and no
    // destroy callbacks are installed.
    unsafe {
        hb_paint_funcs_set_push_transform_func(
            f,
            Some(on_push_transform),
            tag(TAG_PUSH_TRANSFORM),
            None,
        );
        hb_paint_funcs_set_pop_transform_func(
            f,
            Some(on_pop_transform),
            tag(TAG_POP_TRANSFORM),
            None,
        );
        hb_paint_funcs_set_push_clip_glyph_func(
            f,
            Some(on_push_clip_glyph),
            tag(TAG_PUSH_CLIP_GLYPH),
            None,
        );
        hb_paint_funcs_set_pop_clip_func(f, Some(on_pop_clip), tag(TAG_POP_CLIP), None);
        hb_paint_funcs_set_color_func(f, Some(on_color), tag(TAG_COLOR), None);
        hb_paint_funcs_set_linear_gradient_func(f, Some(on_linear), tag(TAG_LINEAR), None);
        hb_paint_funcs_set_radial_gradient_func(f, Some(on_radial), tag(TAG_RADIAL), None);
        hb_paint_funcs_set_sweep_gradient_func(f, Some(on_sweep), tag(TAG_SWEEP), None);
        hb_paint_funcs_set_push_group_func(f, Some(on_push_group), tag(TAG_PUSH_GROUP), None);
        hb_paint_funcs_set_pop_group_func(f, Some(on_pop_group), tag(TAG_POP_GROUP), None);
    }
    f
}

/// A font over `bytes` plus a recording paint-funcs table.
pub struct Setup {
    pub font: *mut hb_font_t,
    pub funcs: *mut hb_paint_funcs_t,
}

impl Setup {
    pub fn new(bytes: &[u8]) -> Self {
        // SAFETY: `bytes` is a live buffer of the given length; the blob
        // copies it, and the face and font keep their own references,
        // so ours are released right away.
        unsafe {
            let blob = hb_blob_create(
                bytes.as_ptr().cast::<c_char>(),
                bytes.len() as c_uint,
                HB_MEMORY_MODE_READONLY,
                ptr::null_mut(),
                None,
            );
            let face = hb_face_create(blob, 0);
            hb_blob_destroy(blob);
            let font = hb_font_create(face);
            hb_face_destroy(face);
            Self {
                font,
                funcs: recording_funcs(),
            }
        }
    }

    /// Paints `gid` and returns the full log.
    pub fn paint_log(&self, gid: u32, palette: c_uint, foreground: hb_color_t) -> Log {
        let log = Log::default();
        let data = ptr::from_ref(&log).cast_mut().cast::<c_void>();
        // SAFETY: font and funcs are live; `data` points at `log`,
        // which outlives the call.
        unsafe { hb_font_paint_glyph(self.font, gid, self.funcs, data, palette, foreground) };
        log
    }

    /// Paints `gid` and returns the events, checking that every callback
    /// saw its own `user_data` and every clip saw this font.
    pub fn paint(&self, gid: u32, palette: c_uint, foreground: hb_color_t) -> Vec<Ev> {
        let log = self.paint_log(gid, palette, foreground);
        assert!(
            log.bad_user_data.borrow().is_empty(),
            "callbacks with the wrong user_data: {:?}",
            log.bad_user_data.borrow()
        );
        for font in log.clip_fonts.borrow().iter() {
            assert_eq!(
                *font, self.font as usize,
                "push_clip_glyph got another font"
            );
        }
        log.events.into_inner()
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        // SAFETY: both handles were created in `new` and are released
        // once here.
        unsafe {
            hb_paint_funcs_destroy(self.funcs);
            hb_font_destroy(self.font);
        }
    }
}

/// `push_transform(root)` for a font at its default scale.
pub const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// The events `PaintGlyph(gid)` around `inner` produces at default
/// scale: inverse root, clip, root, `inner`, pop, pop clip, pop.
pub fn clipped(gid: u32, inner: Vec<Ev>) -> Vec<Ev> {
    let mut out = vec![
        Ev::PushTransform(IDENTITY),
        Ev::PushClipGlyph(gid),
        Ev::PushTransform(IDENTITY),
    ];
    out.extend(inner);
    out.extend([Ev::PopTransform, Ev::PopClip, Ev::PopTransform]);
    out
}

/// Wraps a COLRv1 glyph's events in the default-scale root transform.
pub fn rooted(inner: Vec<Ev>) -> Vec<Ev> {
    let mut out = vec![Ev::PushTransform(IDENTITY)];
    out.extend(inner);
    out.push(Ev::PopTransform);
    out
}
