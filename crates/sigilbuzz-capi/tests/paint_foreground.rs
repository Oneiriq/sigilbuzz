//! `hb_font_paint_glyph` honors `palette_index`, `foreground_color`,
//! and the font's variation coordinates.
//!
//! The fixture is a synthetic SFNT with COLR (v1), CPAL (two palettes),
//! and fvar (one `wght` axis, 100..400..900). The COLR variation store
//! drops PaintVarSolid alpha by 0.5 at `wght` = 900. Callbacks record
//! into a per-test `RefCell` passed as `paint_data`, so tests running
//! in parallel never share state.

#![cfg(feature = "paint")]

use core::cell::RefCell;
use core::ffi::{c_char, c_uint, c_void};
use core::ptr;

use sigilbuzz_capi::paint_bridge::{
    hb_color_line_get_color_stops, hb_color_line_get_extend, hb_color_line_t, hb_color_stop_t,
    hb_color_t, hb_font_paint_glyph, hb_paint_funcs_create, hb_paint_funcs_destroy,
    hb_paint_funcs_reference, hb_paint_funcs_set_color_func,
    hb_paint_funcs_set_linear_gradient_func, hb_paint_funcs_t, HB_PAINT_EXTEND_PAD,
};
use sigilbuzz_capi::{
    hb_blob_create, hb_blob_destroy, hb_face_create, hb_face_destroy, hb_font_create,
    hb_font_destroy, hb_font_set_variations, hb_font_t, hb_variation_t, HB_MEMORY_MODE_READONLY,
};

// =========================================================================
// Fixture
// =========================================================================

const FOREGROUND: u16 = 0xFFFF;

fn f2dot14(v: f32) -> [u8; 2] {
    ((v * 16384.0).round() as i16).to_be_bytes()
}

fn fixed(v: f32) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
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

/// CPAL v0: palette 0 = [red, green], palette 1 = [blue, yellow].
fn cpal() -> Vec<u8> {
    let mut out = Vec::new();
    for v in [0u16, 2, 2, 4] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&16u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes());
    for (r, g, b) in [(255u8, 0u8, 0u8), (0, 255, 0), (0, 0, 255), (255, 255, 0)] {
        out.extend_from_slice(&[b, g, r, 255]);
    }
    out
}

/// fvar with a single `wght` axis: min 100, default 400, max 900.
fn fvar() -> Vec<u8> {
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

/// One-axis IVS: a single region peaking at +1, one row: -8192
/// (F2DOT14 -0.5).
fn ivs() -> Vec<u8> {
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
    for v in [1u16, 1, 1, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&(-8192i16).to_be_bytes());
    out
}

fn solid(entry: u16, alpha: f32) -> Vec<u8> {
    let mut p = vec![2u8];
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(alpha));
    p
}

fn var_solid(entry: u16, alpha: f32) -> Vec<u8> {
    let mut p = vec![3u8];
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(alpha));
    p.extend_from_slice(&0u32.to_be_bytes()); // IVS row (0, 0)
    p
}

fn linear(stops: &[(f32, u16, f32)]) -> Vec<u8> {
    let mut p = vec![4u8, 0, 0, 16];
    for v in [0i16, 0, 100, 0, 0, 100] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.push(0); // Pad
    p.extend_from_slice(&(stops.len() as u16).to_be_bytes());
    for (offset, entry, alpha) in stops {
        p.extend_from_slice(&f2dot14(*offset));
        p.extend_from_slice(&entry.to_be_bytes());
        p.extend_from_slice(&f2dot14(*alpha));
    }
    p
}

fn colr(paints: &[(u16, Vec<u8>)], var_store: &[u8]) -> Vec<u8> {
    let header_len: u32 = 30;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    let var_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(paints.len() as u32).to_be_bytes());
    let records = out.len();
    for (gid, _) in paints {
        out.extend_from_slice(&gid.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    for (i, (_, bytes)) in paints.iter().enumerate() {
        let rel = out.len() as u32 - header_len;
        let slot = records + i * 6 + 2;
        out[slot..slot + 4].copy_from_slice(&rel.to_be_bytes());
        out.extend_from_slice(bytes);
    }
    let off = out.len() as u32;
    out[var_slot..var_slot + 4].copy_from_slice(&off.to_be_bytes());
    out.extend_from_slice(var_store);
    out
}

/// Glyphs:
/// 1 = foreground solid, alpha 0.5
/// 2 = linear gradient: palette entry 0, then foreground at alpha 0.5
/// 3 = palette entry 0 solid
/// 4 = foreground PaintVarSolid, alpha 1.0 minus 0.5 at wght 900
/// 5 = palette entry 1 PaintVarSolid, same variation
fn font_bytes() -> Vec<u8> {
    let paints = [
        (1, solid(FOREGROUND, 0.5)),
        (2, linear(&[(0.0, 0, 1.0), (1.0, FOREGROUND, 0.5)])),
        (3, solid(0, 1.0)),
        (4, var_solid(FOREGROUND, 1.0)),
        (5, var_solid(1, 1.0)),
    ];
    let colr = colr(&paints, &ivs());
    let cpal = cpal();
    let fvar = fvar();
    sfnt(&[(b"COLR", &colr), (b"CPAL", &cpal), (b"fvar", &fvar)])
}

// =========================================================================
// Recorder
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
enum Event {
    Color {
        is_foreground: i32,
        color: hb_color_t,
    },
    Linear {
        stops: Vec<hb_color_stop_t>,
        extend: c_uint,
    },
}

type Log = RefCell<Vec<Event>>;

/// # Safety
/// `data` must be the `&Log` the test passed as `paint_data`.
unsafe fn log<'a>(data: *mut c_void) -> &'a Log {
    // SAFETY: see the function contract.
    unsafe { &*data.cast::<Log>().cast_const() }
}

extern "C" fn on_color(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    is_foreground: i32,
    color: hb_color_t,
) {
    // SAFETY: every test passes a live `Log` as paint_data.
    unsafe { log(data) }.borrow_mut().push(Event::Color {
        is_foreground,
        color,
    });
}

extern "C" fn on_linear(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    line: *const hb_color_line_t,
    _x0: f32,
    _y0: f32,
    _x1: f32,
    _y1: f32,
    _x2: f32,
    _y2: f32,
) {
    // SAFETY: `line` is the live color line of this callback, and the
    // buffer has room for `count` stops.
    let (stops, extend) = unsafe {
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
    };
    // SAFETY: every test passes a live `Log` as paint_data.
    unsafe { log(data) }
        .borrow_mut()
        .push(Event::Linear { stops, extend });
}

/// A font over the fixture plus a paint-funcs table with the color and
/// linear-gradient callbacks wired up.
struct Setup {
    font: *mut hb_font_t,
    funcs: *mut hb_paint_funcs_t,
}

impl Setup {
    fn new() -> Self {
        let bytes = font_bytes();
        // SAFETY: `bytes` is a live buffer of the given length; the
        // blob copies it, and the face and font keep their own
        // references, so ours are released right away.
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
            let funcs = hb_paint_funcs_create();
            hb_paint_funcs_set_color_func(funcs, Some(on_color));
            hb_paint_funcs_set_linear_gradient_func(funcs, Some(on_linear));
            Self { font, funcs }
        }
    }

    fn paint(&self, gid: u32, palette: c_uint, foreground: hb_color_t) -> Vec<Event> {
        let log: Log = RefCell::new(Vec::new());
        let data = core::ptr::from_ref(&log).cast_mut().cast::<c_void>();
        // SAFETY: font and funcs are live; `data` points at `log`,
        // which outlives the call.
        unsafe { hb_font_paint_glyph(self.font, gid, self.funcs, data, palette, foreground) };
        log.into_inner()
    }

    fn set_wght(&self, value: f32) {
        let v = hb_variation_t {
            tag: u32::from_be_bytes(*b"wght"),
            value,
        };
        // SAFETY: the font is live and `v` is one valid variation.
        unsafe { hb_font_set_variations(self.font, &v, 1) };
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

/// HB_COLOR(b, g, r, a).
const fn hb_color(b: u8, g: u8, r: u8, a: u8) -> hb_color_t {
    (b as u32) | ((g as u32) << 8) | ((r as u32) << 16) | ((a as u32) << 24)
}

const FG: hb_color_t = hb_color(0x10, 0x20, 0x30, 0xFF);
const RED: hb_color_t = hb_color(0, 0, 255, 255);
const BLUE: hb_color_t = hb_color(255, 0, 0, 255);

fn color(is_foreground: i32, color: hb_color_t) -> Event {
    Event::Color {
        is_foreground,
        color,
    }
}

// =========================================================================
// Tests
// =========================================================================

#[test]
fn foreground_solid_reports_foreground_with_paint_alpha() {
    let s = Setup::new();
    // HarfBuzz: alpha byte 0xFF * 0.5 = 127.5, truncated to 0x7F.
    assert_eq!(
        s.paint(1, 0, FG),
        vec![color(1, hb_color(0x10, 0x20, 0x30, 0x7F))]
    );
    // The foreground's own alpha is multiplied, not replaced.
    let half_fg = hb_color(0x10, 0x20, 0x30, 0x80);
    assert_eq!(
        s.paint(1, 0, half_fg),
        vec![color(1, hb_color(0x10, 0x20, 0x30, 0x40))]
    );
}

#[test]
fn foreground_gradient_stop_reports_foreground() {
    let s = Setup::new();
    let events = s.paint(2, 0, FG);
    let expected_stops = vec![
        hb_color_stop_t {
            offset: 0.0,
            is_foreground: 0,
            color: RED,
        },
        hb_color_stop_t {
            offset: 1.0,
            is_foreground: 1,
            color: hb_color(0x10, 0x20, 0x30, 0x7F),
        },
    ];
    assert_eq!(
        events,
        vec![Event::Linear {
            stops: expected_stops,
            extend: HB_PAINT_EXTEND_PAD,
        }]
    );
}

#[test]
fn palette_index_selects_cpal_palette() {
    let s = Setup::new();
    assert_eq!(s.paint(3, 0, FG), vec![color(0, RED)]);
    assert_eq!(s.paint(3, 1, FG), vec![color(0, BLUE)]);
    // A palette the font lacks falls back to palette 0.
    assert_eq!(s.paint(3, 2, FG), vec![color(0, RED)]);
    assert_eq!(s.paint(3, c_uint::MAX, FG), vec![color(0, RED)]);
    // Palette choice leaves foreground paints alone.
    assert_eq!(
        s.paint(1, 1, FG),
        vec![color(1, hb_color(0x10, 0x20, 0x30, 0x7F))]
    );
}

#[test]
fn variation_coords_reach_the_paint_tree() {
    let s = Setup::new();
    // Default instance: no delta.
    assert_eq!(s.paint(4, 0, FG), vec![color(1, FG)]);
    // wght 900 normalizes to +1: alpha 1.0 - 0.5.
    s.set_wght(900.0);
    assert_eq!(
        s.paint(4, 0, FG),
        vec![color(1, hb_color(0x10, 0x20, 0x30, 0x7F))]
    );
    // wght 650 normalizes to +0.5: alpha 0.75, 0xFF * 0.75 = 191.25.
    s.set_wght(650.0);
    assert_eq!(
        s.paint(4, 0, FG),
        vec![color(1, hb_color(0x10, 0x20, 0x30, 0xBF))]
    );
    // Back to the default.
    s.set_wght(400.0);
    assert_eq!(s.paint(4, 0, FG), vec![color(1, FG)]);
}

#[test]
fn palette_and_coords_combine() {
    let s = Setup::new();
    s.set_wght(900.0);
    // Palette 1 entry 1 is yellow; alpha 0.5 rounds to 0x80 for
    // palette colors.
    assert_eq!(
        s.paint(5, 1, FG),
        vec![color(0, hb_color(0, 255, 255, 0x80))]
    );
    assert_eq!(s.paint(5, 0, FG), vec![color(0, hb_color(0, 255, 0, 0x80))]);
}

#[test]
fn glyphs_without_paint_fire_nothing() {
    let s = Setup::new();
    assert!(s.paint(9, 0, FG).is_empty());
    assert!(s.paint(0x1_0001, 0, FG).is_empty(), "gid past u16");
}

#[test]
fn paint_funcs_survive_extra_references() {
    let s = Setup::new();
    // SAFETY: `s.funcs` is live; the extra reference is released below.
    let again = unsafe { hb_paint_funcs_reference(s.funcs) };
    assert_eq!(again, s.funcs);
    // SAFETY: releases the extra reference only.
    unsafe { hb_paint_funcs_destroy(again) };
    assert_eq!(s.paint(3, 0, FG), vec![color(0, RED)]);
}
