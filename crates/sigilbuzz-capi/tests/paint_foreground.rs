//! `hb_font_paint_glyph` resolves colors the way HarfBuzz's paint
//! context does: `foreground` for entry `0xFFFF` (flagged), then
//! `custom_palette_color`, then CPAL palette `palette_index`, then
//! `foreground` again (unflagged) when the font cannot supply the
//! entry. The paint alpha multiplies the alpha byte and the product is
//! truncated. Variation coordinates set on the font reach the walk.

#![cfg(feature = "paint")]

mod common;

use core::ffi::{c_uint, c_void};
use core::ptr;

use common::*;
use sigilbuzz_capi::hb_font_set_variations;
use sigilbuzz_capi::hb_variation_t;
use sigilbuzz_capi::paint_bridge::{
    hb_color_line_get_color_stops, hb_color_line_t, hb_color_stop_t, hb_color_t,
    hb_paint_funcs_set_custom_palette_color_func, hb_paint_funcs_set_linear_gradient_func,
    hb_paint_funcs_t, HB_PAINT_EXTEND_PAD,
};

/// Palette 0 = [red, green, (10, 20, 30, alpha 200)],
/// palette 1 = [blue, yellow, (40, 50, 60, alpha 128)].
fn two_palettes() -> Vec<u8> {
    cpal(&[
        &[(255, 0, 0, 255), (0, 255, 0, 255), (10, 20, 30, 200)],
        &[(0, 0, 255, 255), (255, 255, 0, 255), (40, 50, 60, 128)],
    ])
}

/// Glyphs:
/// 1 = foreground solid, alpha 0.5
/// 2 = linear gradient: entry 0, then foreground at alpha 0.5
/// 3 = entry 0 solid
/// 4 = foreground PaintVarSolid, alpha 1.0 minus 0.5 at wght 900
/// 5 = entry 1 PaintVarSolid, same variation
/// 6 = entry 7 solid (past the end of the palette), alpha 0.5
/// 7 = entry 2 solid, alpha 0.5
/// 8 = entry 0 solid, alpha 0.5
/// 9 = linear gradient: entry 0, entry 1
fn paints() -> Vec<(u16, Vec<u8>)> {
    vec![
        (1, solid(FOREGROUND, 0.5)),
        (2, linear(&[(0.0, 0, 1.0), (1.0, FOREGROUND, 0.5)])),
        (3, solid(0, 1.0)),
        (4, var_solid(FOREGROUND, 1.0, 0)),
        (5, var_solid(1, 1.0, 0)),
        (6, solid(7, 0.5)),
        (7, solid(2, 0.5)),
        (8, solid(0, 0.5)),
        (9, linear(&[(0.0, 0, 1.0), (1.0, 1, 1.0)])),
    ]
}

fn font_bytes() -> Vec<u8> {
    let colr = colr(&[], &paints(), &ivs(&[-8192]));
    let cpal = two_palettes();
    let fvar = fvar();
    sfnt(&[(b"COLR", &colr), (b"CPAL", &cpal), (b"fvar", &fvar)])
}

const FG: hb_color_t = hb_color(0x10, 0x20, 0x30, 0xFF);
const RED: hb_color_t = hb_color(0, 0, 255, 255);
const BLUE: hb_color_t = hb_color(255, 0, 0, 255);

/// The events of a single-paint COLRv1 glyph, without the root
/// transform around them.
fn inner(events: Vec<Ev>) -> Vec<Ev> {
    assert_eq!(events.first(), Some(&Ev::PushTransform(IDENTITY)));
    assert_eq!(events.last(), Some(&Ev::PopTransform));
    events[1..events.len() - 1].to_vec()
}

fn color_of(s: &Setup, gid: u32, palette: c_uint, fg: hb_color_t) -> Ev {
    let events = inner(s.paint(gid, palette, fg));
    assert_eq!(events.len(), 1, "one color event: {events:?}");
    events[0].clone()
}

fn set_wght(s: &Setup, value: f32) {
    let v = hb_variation_t {
        tag: u32::from_be_bytes(*b"wght"),
        value,
    };
    // SAFETY: the font is live and `v` is one valid variation.
    unsafe { hb_font_set_variations(s.font, &v, 1) };
}

// =========================================================================
// Foreground
// =========================================================================

#[test]
fn foreground_solid_reports_foreground_with_paint_alpha() {
    let s = Setup::new(&font_bytes());
    // HarfBuzz: alpha byte 0xFF * 0.5 = 127.5, truncated to 0x7F.
    assert_eq!(
        color_of(&s, 1, 0, FG),
        Ev::Color(1, hb_color(0x10, 0x20, 0x30, 0x7F))
    );
    // The foreground's own alpha is multiplied, not replaced.
    let half_fg = hb_color(0x10, 0x20, 0x30, 0x80);
    assert_eq!(
        color_of(&s, 1, 0, half_fg),
        Ev::Color(1, hb_color(0x10, 0x20, 0x30, 0x40))
    );
}

#[test]
fn foreground_gradient_stop_reports_foreground() {
    let s = Setup::new(&font_bytes());
    let stops = vec![
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
        inner(s.paint(2, 0, FG)),
        vec![Ev::Linear(
            stops,
            HB_PAINT_EXTEND_PAD,
            [0.0, 0.0, 100.0, 0.0, 0.0, 100.0]
        )]
    );
}

// =========================================================================
// Palettes
// =========================================================================

#[test]
fn palette_index_selects_cpal_palette() {
    let s = Setup::new(&font_bytes());
    assert_eq!(color_of(&s, 3, 0, FG), Ev::Color(0, RED));
    assert_eq!(color_of(&s, 3, 1, FG), Ev::Color(0, BLUE));
    // Palette choice leaves foreground paints alone.
    assert_eq!(
        color_of(&s, 1, 1, FG),
        Ev::Color(1, hb_color(0x10, 0x20, 0x30, 0x7F))
    );
}

#[test]
fn out_of_range_palette_paints_the_foreground_unflagged() {
    let s = Setup::new(&font_bytes());
    for palette in [2, 0xFFFF, 0x1_0000, c_uint::MAX] {
        assert_eq!(
            color_of(&s, 3, palette, FG),
            Ev::Color(0, FG),
            "{palette:#x}"
        );
        // The paint alpha still applies, truncated.
        assert_eq!(
            color_of(&s, 8, palette, FG),
            Ev::Color(0, hb_color(0x10, 0x20, 0x30, 0x7F)),
            "{palette:#x}"
        );
    }
    // Gradient stops resolve the same way.
    let fg_stop = |offset| hb_color_stop_t {
        offset,
        is_foreground: 0,
        color: FG,
    };
    assert_eq!(
        inner(s.paint(9, 5, FG)),
        vec![Ev::Linear(
            vec![fg_stop(0.0), fg_stop(1.0)],
            HB_PAINT_EXTEND_PAD,
            [0.0, 0.0, 100.0, 0.0, 0.0, 100.0]
        )]
    );
}

#[test]
fn missing_palette_entry_paints_the_foreground_unflagged() {
    let s = Setup::new(&font_bytes());
    let want = Ev::Color(0, hb_color(0x10, 0x20, 0x30, 0x7F));
    assert_eq!(color_of(&s, 6, 0, FG), want);
    assert_eq!(color_of(&s, 6, 1, FG), want);
}

#[test]
fn font_without_cpal_paints_palette_entries_in_the_foreground() {
    let colr = colr(&[], &paints(), &[]);
    let s = Setup::new(&sfnt(&[(b"COLR", &colr)]));
    assert_eq!(color_of(&s, 3, 0, FG), Ev::Color(0, FG));
    assert_eq!(
        color_of(&s, 1, 0, FG),
        Ev::Color(1, hb_color(0x10, 0x20, 0x30, 0x7F))
    );
}

#[test]
fn palette_alpha_is_truncated_not_rounded() {
    let s = Setup::new(&font_bytes());
    // 255 * 0.5 = 127.5 truncates to 0x7F (rounding would give 0x80).
    assert_eq!(
        color_of(&s, 8, 0, FG),
        Ev::Color(0, hb_color(0, 0, 255, 0x7F))
    );
    // 200 * 0.5 = 100 exactly.
    assert_eq!(
        color_of(&s, 7, 0, FG),
        Ev::Color(0, hb_color(30, 20, 10, 100))
    );
    // 128 * 0.5 = 64 exactly.
    assert_eq!(
        color_of(&s, 7, 1, FG),
        Ev::Color(0, hb_color(60, 50, 40, 64))
    );
}

// =========================================================================
// custom_palette_color
// =========================================================================

/// Overrides entry 1 with `OVERRIDE` and logs every query into the
/// paint_data log. `user_data` must be the address of `USER_DATA`.
const OVERRIDE: hb_color_t = hb_color(0xAA, 0xBB, 0xCC, 0xFF);
static USER_DATA: u8 = 0;

unsafe extern "C" fn custom_palette(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    color_index: c_uint,
    color: *mut hb_color_t,
    user_data: *mut c_void,
) -> i32 {
    assert_eq!(
        user_data.cast_const(),
        ptr::from_ref(&USER_DATA).cast::<c_void>()
    );
    // SAFETY: every test passes a live `Log` as paint_data.
    unsafe { log(data) }
        .events
        .borrow_mut()
        .push(Ev::CustomPalette(color_index));
    if color_index == 1 {
        // SAFETY: HarfBuzz hands the callback a writable color.
        unsafe { *color = OVERRIDE };
        1
    } else {
        0
    }
}

fn install_custom_palette(s: &Setup) {
    // SAFETY: the table is live; USER_DATA is a static.
    unsafe {
        hb_paint_funcs_set_custom_palette_color_func(
            s.funcs,
            Some(custom_palette),
            ptr::from_ref(&USER_DATA).cast_mut().cast::<c_void>(),
            None,
        );
    }
}

#[test]
fn custom_palette_color_overrides_cpal_before_the_lookup() {
    let s = Setup::new(&font_bytes());
    install_custom_palette(&s);
    // Entry 0 declined: CPAL red.
    assert_eq!(
        inner(s.paint(3, 0, FG)),
        vec![Ev::CustomPalette(0), Ev::Color(0, RED)]
    );
    // Entry 1 overridden, even in a palette the font lacks, with the
    // paint alpha applied (1.0 minus 0.5 at wght 900, truncated).
    set_wght(&s, 900.0);
    assert_eq!(
        inner(s.paint(5, 7, FG)),
        vec![
            Ev::CustomPalette(1),
            Ev::Color(0, hb_color(0xAA, 0xBB, 0xCC, 0x7F))
        ]
    );
    // The foreground entry never asks.
    assert_eq!(
        inner(s.paint(4, 0, FG)),
        vec![Ev::Color(1, hb_color(0x10, 0x20, 0x30, 0x7F))]
    );
}

#[test]
fn custom_palette_color_runs_when_a_callback_reads_the_stops() {
    let s = Setup::new(&font_bytes());
    install_custom_palette(&s);
    // The recording linear callback reads every stop, so both entries
    // are queried inside the callback, before it logs the gradient.
    let stops = vec![
        hb_color_stop_t {
            offset: 0.0,
            is_foreground: 0,
            color: RED,
        },
        hb_color_stop_t {
            offset: 1.0,
            is_foreground: 0,
            color: OVERRIDE,
        },
    ];
    assert_eq!(
        inner(s.paint(9, 0, FG)),
        vec![
            Ev::CustomPalette(0),
            Ev::CustomPalette(1),
            Ev::Linear(
                stops,
                HB_PAINT_EXTEND_PAD,
                [0.0, 0.0, 100.0, 0.0, 0.0, 100.0]
            ),
        ]
    );

    // A callback that only asks for the stop count resolves nothing.
    unsafe extern "C" fn count_only(
        _funcs: *mut hb_paint_funcs_t,
        data: *mut c_void,
        line: *mut hb_color_line_t,
        _x0: f32,
        _y0: f32,
        _x1: f32,
        _y1: f32,
        _x2: f32,
        _y2: f32,
        _user_data: *mut c_void,
    ) {
        // SAFETY: `line` is live for this callback.
        let total =
            unsafe { hb_color_line_get_color_stops(line, 0, ptr::null_mut(), ptr::null_mut()) };
        let ev = Ev::Linear(Vec::new(), total, [0.0; 6]);
        // SAFETY: every test passes a live `Log` as paint_data.
        unsafe { log(data) }.events.borrow_mut().push(ev);
    }
    // SAFETY: the table is live.
    unsafe {
        hb_paint_funcs_set_linear_gradient_func(s.funcs, Some(count_only), ptr::null_mut(), None);
    }
    assert_eq!(
        inner(s.paint(9, 0, FG)),
        vec![Ev::Linear(Vec::new(), 2, [0.0; 6])]
    );
}

// =========================================================================
// Variation coordinates
// =========================================================================

#[test]
fn variation_coords_reach_the_paint_tree() {
    let s = Setup::new(&font_bytes());
    // Default instance: no delta.
    assert_eq!(color_of(&s, 4, 0, FG), Ev::Color(1, FG));
    // wght 900 normalizes to +1: alpha 1.0 - 0.5.
    set_wght(&s, 900.0);
    assert_eq!(
        color_of(&s, 4, 0, FG),
        Ev::Color(1, hb_color(0x10, 0x20, 0x30, 0x7F))
    );
    // wght 650 normalizes to +0.5: alpha 0.75, 0xFF * 0.75 = 191.25.
    set_wght(&s, 650.0);
    assert_eq!(
        color_of(&s, 4, 0, FG),
        Ev::Color(1, hb_color(0x10, 0x20, 0x30, 0xBF))
    );
    // Back to the default.
    set_wght(&s, 400.0);
    assert_eq!(color_of(&s, 4, 0, FG), Ev::Color(1, FG));
}

#[test]
fn palette_and_coords_combine() {
    let s = Setup::new(&font_bytes());
    set_wght(&s, 900.0);
    // Palette 1 entry 1 is yellow; 0xFF * 0.5 truncates to 0x7F.
    assert_eq!(
        color_of(&s, 5, 1, FG),
        Ev::Color(0, hb_color(0, 255, 255, 0x7F))
    );
    assert_eq!(
        color_of(&s, 5, 0, FG),
        Ev::Color(0, hb_color(0, 255, 0, 0x7F))
    );
}
