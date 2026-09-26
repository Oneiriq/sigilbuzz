//! `hb_font_paint_glyph` fires HarfBuzz's callbacks in HarfBuzz's
//! order, with HarfBuzz's arguments.
//!
//! Every test runs against a hand-built COLR + CPAL font through the C
//! entry points, with every callback installed under its own
//! `user_data` tag (see `common`), so each test also checks that
//! callbacks receive their own `user_data` and that `push_clip_glyph`
//! receives the font being painted.

#![cfg(feature = "paint")]

mod common;

use core::f32::consts::PI;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use sigilbuzz_capi::hb_font_set_scale;
use sigilbuzz_capi::paint_bridge::{
    hb_color_stop_t, hb_font_paint_glyph, hb_paint_funcs_create, hb_paint_funcs_destroy,
    hb_paint_funcs_set_color_func, hb_paint_funcs_set_color_glyph_func,
    hb_paint_funcs_set_pop_clip_func, hb_paint_funcs_t, HB_PAINT_COMPOSITE_MODE_DEST_IN,
    HB_PAINT_COMPOSITE_MODE_SRC_OVER, HB_PAINT_EXTEND_PAD, HB_PAINT_EXTEND_REFLECT,
    HB_PAINT_EXTEND_REPEAT,
};

const RED: (u8, u8, u8, u8) = (255, 0, 0, 255);
const GREEN: (u8, u8, u8, u8) = (0, 255, 0, 255);
const HB_RED: u32 = hb_color(0, 0, 255, 255);
const HB_GREEN: u32 = hb_color(0, 255, 0, 255);
const FG: u32 = hb_color(0x10, 0x20, 0x30, 0xFF);

/// Glyphs:
/// 1 = PaintGlyph(42) -> PaintTransform(2x, +5) -> linear gradient
/// 2 = composite: PaintGlyph(3, green) DEST_IN PaintGlyph(4, red)
/// 3 = PaintScaleUniformAroundCenter(0.5 around (10, 20)) -> red solid
/// 4 = sweep, stored angles -1 .. 0.5 (0 .. 1.5 pi)
/// 5 = radial gradient
/// 6 = COLRv0: layers (20, red) and (21, foreground)
fn font_bytes() -> Vec<u8> {
    let grad = linear(&[(0.0, 0, 1.0), (1.0, FOREGROUND, 1.0)]);
    let v1 = [
        (
            1,
            glyph(42, &transform([2.0, 0.0, 0.0, 2.0, 5.0, 0.0], &grad)),
        ),
        (
            2,
            composite(&glyph(3, &solid(1, 1.0)), 6, &glyph(4, &solid(0, 1.0))),
        ),
        (3, scale_around(0.5, 10, 20, &solid(0, 1.0))),
        (4, sweep(-1.0, 0.5, &[(0.0, 0, 1.0), (1.0, 1, 1.0)])),
        (5, radial(&[(0.25, 1, 1.0)])),
    ];
    let layers: &[(u16, u16)] = &[(20, 0), (21, FOREGROUND)];
    let colr = colr(&[(6, layers)], &v1, &[]);
    let cpal = cpal(&[&[RED, GREEN]]);
    sfnt(&[(b"COLR", &colr), (b"CPAL", &cpal)])
}

fn stop(offset: f32, is_foreground: i32, color: u32) -> hb_color_stop_t {
    hb_color_stop_t {
        offset,
        is_foreground,
        color,
    }
}

#[test]
fn glyph_clip_is_not_transformed_by_the_paint_below_it() {
    let s = Setup::new(&font_bytes());
    let gradient = Ev::Linear(
        vec![stop(0.0, 0, HB_RED), stop(1.0, 1, FG)],
        HB_PAINT_EXTEND_PAD,
        [0.0, 0.0, 100.0, 0.0, 0.0, 100.0],
    );
    let inner = vec![
        Ev::PushTransform([2.0, 0.0, 0.0, 2.0, 5.0, 0.0]),
        gradient,
        Ev::PopTransform,
    ];
    assert_eq!(s.paint(1, 0, FG), rooted(clipped(42, inner)));
}

#[test]
fn composite_uses_two_groups_and_reports_the_mode_on_pop() {
    let s = Setup::new(&font_bytes());
    let mut want = vec![Ev::PushGroup];
    want.extend(clipped(4, vec![Ev::Color(0, HB_RED)]));
    want.push(Ev::PushGroup);
    want.extend(clipped(3, vec![Ev::Color(0, HB_GREEN)]));
    want.extend([
        Ev::PopGroup(HB_PAINT_COMPOSITE_MODE_DEST_IN),
        Ev::PopGroup(HB_PAINT_COMPOSITE_MODE_SRC_OVER),
    ]);
    assert_eq!(s.paint(2, 0, FG), rooted(want));
}

#[test]
fn around_center_transforms_nest_translate_scale_translate() {
    let s = Setup::new(&font_bytes());
    let want = vec![
        Ev::PushTransform([1.0, 0.0, 0.0, 1.0, 10.0, 20.0]),
        Ev::PushTransform([0.5, 0.0, 0.0, 0.5, 0.0, 0.0]),
        Ev::PushTransform([1.0, 0.0, 0.0, 1.0, -10.0, -20.0]),
        Ev::Color(0, HB_RED),
        Ev::PopTransform,
        Ev::PopTransform,
        Ev::PopTransform,
    ];
    assert_eq!(s.paint(3, 0, FG), rooted(want));
}

#[test]
fn sweep_angles_are_biased_radians() {
    let s = Setup::new(&font_bytes());
    let want = Ev::Sweep(
        vec![stop(0.0, 0, HB_RED), stop(1.0, 0, HB_GREEN)],
        HB_PAINT_EXTEND_REPEAT,
        [50.0, 60.0, 0.0, 1.5 * PI],
    );
    assert_eq!(s.paint(4, 0, FG), rooted(vec![want]));
}

#[test]
fn radial_gradient_passes_both_circles() {
    let s = Setup::new(&font_bytes());
    let want = Ev::Radial(
        vec![stop(0.25, 0, HB_GREEN)],
        HB_PAINT_EXTEND_REFLECT,
        [10.0, 20.0, 5.0, 30.0, 40.0, 50.0],
    );
    assert_eq!(s.paint(5, 0, FG), rooted(vec![want]));
}

#[test]
fn colr_v0_layers_paint_clip_color_pop_per_layer() {
    let s = Setup::new(&font_bytes());
    assert_eq!(
        s.paint(6, 0, FG),
        vec![
            Ev::PushClipGlyph(20),
            Ev::Color(0, HB_RED),
            Ev::PopClip,
            Ev::PushClipGlyph(21),
            Ev::Color(1, FG),
            Ev::PopClip,
        ]
    );
}

#[test]
fn glyphs_without_color_data_paint_their_outline_in_the_foreground() {
    let s = Setup::new(&font_bytes());
    for gid in [9, 0x1_0005] {
        assert_eq!(
            s.paint(gid, 0, FG),
            vec![Ev::PushClipGlyph(gid), Ev::Color(1, FG), Ev::PopClip],
            "gid {gid:#x}"
        );
    }
    // The fallback color is the foreground exactly, alpha included.
    let translucent = hb_color(1, 2, 3, 0x40);
    assert_eq!(s.paint(9, 0, translucent)[1], Ev::Color(1, translucent));
}

#[test]
fn root_transform_follows_the_font_scale() {
    let s = Setup::new(&font_bytes());
    // The fixture has no head table, so upem is 1000.
    // SAFETY: the font is live.
    unsafe { hb_font_set_scale(s.font, 2000, 4000) };
    let root = [2.0, 0.0, 0.0, 4.0, 0.0, 0.0];
    let inverse = [0.5, 0.0, -0.0, 0.25, 0.0, 0.0];
    // The ClipBox (0, 0, 1000, 1000) scales with the font, outside the
    // root transform.
    let clip = Ev::PushClipRect([0.0, 0.0, 2000.0, 4000.0]);
    let events = s.paint(3, 0, FG);
    assert_eq!(events[..2], [clip.clone(), Ev::PushTransform(root)]);
    assert_eq!(events[events.len() - 2..], [Ev::PopTransform, Ev::PopClip]);
    let events = s.paint(2, 0, FG);
    assert_eq!(
        events[..6],
        [
            clip,
            Ev::PushTransform(root),
            Ev::PushGroup,
            Ev::PushTransform(inverse),
            Ev::PushClipGlyph(4),
            Ev::PushTransform(root),
        ]
    );
}

#[test]
fn inverse_root_transform_has_harfbuzzs_negative_zero() {
    // HarfBuzz prints the inverse root transform as "1 0 -0 1 0 0".
    let s = Setup::new(&font_bytes());
    let events = s.paint(1, 0, FG);
    let Ev::PushTransform(inverse) = events[2] else {
        panic!("expected the inverse root transform: {events:?}");
    };
    assert_eq!(inverse, INVERSE_IDENTITY);
    assert!(inverse[2].is_sign_negative(), "{inverse:?}");
    let Ev::PushTransform(root) = events[1] else {
        panic!("expected the root transform: {events:?}");
    };
    assert!(root[2].is_sign_positive(), "{root:?}");
}

#[test]
fn clip_boxes_round_to_font_units() {
    // 1000 upem at 3 units per em: the 16.16 multiplier truncates to
    // 196 / 65536, so 1000 scales to 3 and the box is (0, 0, 3, 3).
    let s = Setup::new(&font_bytes());
    // SAFETY: the font is live.
    unsafe { hb_font_set_scale(s.font, 3, 3) };
    assert_eq!(s.paint(3, 0, FG)[0], Ev::PushClipRect([0.0, 0.0, 3.0, 3.0]));
}

/// Glyphs without a ClipList: 1 = PaintColrGlyph(2), 2 = a bare solid
/// (unbounded), 3 = PaintColrGlyph(4), whose ClipBox bounds it.
fn unclipped_font() -> Vec<u8> {
    let v1 = [
        (1, colr_glyph(2)),
        (2, solid(0, 1.0)),
        (3, colr_glyph(4)),
        (4, solid(1, 0.5)),
    ];
    let colr = colr_with_clips(&[], &v1, &[], &[(4, 4, [10, 20, 30, 40])]);
    let cpal = cpal(&[&[RED, GREEN]]);
    sfnt(&[(b"COLR", &colr), (b"CPAL", &cpal)])
}

#[test]
fn unbounded_glyphs_clip_to_their_computed_extents_and_paint_nothing() {
    let s = Setup::new(&unclipped_font());
    for gid in [1, 2] {
        let events = s.paint(gid, 0, FG);
        assert!(
            matches!(events[0], Ev::PushClipRect(_)),
            "gid {gid}: {events:?}"
        );
        assert_eq!(
            events[1..],
            [Ev::PushTransform(IDENTITY), Ev::PopTransform, Ev::PopClip],
            "gid {gid}"
        );
    }
}

#[test]
fn colr_glyph_references_are_offered_then_clipped_to_their_box() {
    let s = Setup::new(&unclipped_font());
    let want = vec![
        // Bounds computed from glyph 4's ClipBox.
        Ev::PushClipRect([10.0, 20.0, 30.0, 40.0]),
        Ev::PushTransform(IDENTITY),
        // The color_glyph offer, declined: no callback is installed.
        Ev::PushTransform(INVERSE_IDENTITY),
        Ev::PopTransform,
        // Glyph 4's ClipBox, in design units inside the root transform.
        Ev::PushClipRect([10.0, 20.0, 30.0, 40.0]),
        Ev::Color(0, hb_color(0, 255, 0, 127)),
        Ev::PopClip,
        Ev::PopTransform,
        Ev::PopClip,
    ];
    assert_eq!(s.paint(3, 0, FG), want);
}

#[test]
fn a_color_glyph_callback_that_paints_skips_the_reference() {
    unsafe extern "C" fn paint_it(
        _funcs: *mut hb_paint_funcs_t,
        data: *mut c_void,
        glyph: u32,
        _font: *mut sigilbuzz_capi::hb_font_t,
        user_data: *mut c_void,
    ) -> i32 {
        assert_eq!(user_data as usize, 0x2A, "own user_data");
        // SAFETY: every test passes a live `Log` as paint_data.
        unsafe { log(data) }
            .events
            .borrow_mut()
            .push(Ev::CustomPalette(glyph));
        1
    }
    let s = Setup::new(&unclipped_font());
    // SAFETY: the table is live; the tag is never dereferenced.
    unsafe {
        hb_paint_funcs_set_color_glyph_func(s.funcs, Some(paint_it), 0x2A as *mut c_void, None);
    }
    let events = s.paint(3, 0, FG);
    assert_eq!(
        events[2..],
        [
            Ev::PushTransform(INVERSE_IDENTITY),
            Ev::CustomPalette(4),
            Ev::PopTransform,
            Ev::PopTransform,
            Ev::PopClip,
        ]
    );
}

#[test]
fn unset_callbacks_are_skipped() {
    let bytes = font_bytes();
    let s = Setup::new(&bytes);
    let only_color = hb_paint_funcs_create();
    unsafe extern "C" fn count(
        _f: *mut hb_paint_funcs_t,
        data: *mut c_void,
        _is_fg: i32,
        _color: u32,
        _user_data: *mut c_void,
    ) {
        // SAFETY: the test passes a live counter as paint_data.
        unsafe { &*data.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
    }
    let calls = AtomicUsize::new(0);
    // SAFETY: the table and font are live; `calls` outlives the paint.
    unsafe {
        hb_paint_funcs_set_color_func(only_color, Some(count), ptr::null_mut(), None);
        let data = ptr::from_ref(&calls).cast_mut().cast::<c_void>();
        hb_font_paint_glyph(s.font, 2, only_color, data, 0, FG);
        hb_paint_funcs_destroy(only_color);
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "two solid fills in glyph 2"
    );
}

#[test]
fn a_callback_may_replace_callbacks_while_painting() {
    // The first pop_clip swaps color for a no-op; later colors vanish.
    unsafe extern "C" fn pop_clip_then_mute(
        funcs: *mut hb_paint_funcs_t,
        data: *mut c_void,
        _user_data: *mut c_void,
    ) {
        // SAFETY: `funcs` is the live table being painted with.
        unsafe { hb_paint_funcs_set_color_func(funcs, None, ptr::null_mut(), None) };
        // SAFETY: every test passes a live `Log` as paint_data.
        unsafe { log(data) }.events.borrow_mut().push(Ev::PopClip);
    }
    let s = Setup::new(&font_bytes());
    // SAFETY: the table is live.
    unsafe {
        hb_paint_funcs_set_pop_clip_func(s.funcs, Some(pop_clip_then_mute), ptr::null_mut(), None);
    }
    let events = s.paint_log(6, 0, FG).events.into_inner();
    assert_eq!(
        events,
        vec![
            Ev::PushClipGlyph(20),
            Ev::Color(0, HB_RED),
            Ev::PopClip,
            Ev::PushClipGlyph(21),
            Ev::PopClip,
        ]
    );
}

#[test]
fn a_callback_may_drop_the_callers_last_references() {
    // pop_clip destroys the funcs table and font the caller passed in;
    // the paint call keeps its own references until it returns.
    struct Owned {
        funcs: *mut hb_paint_funcs_t,
        font: *mut sigilbuzz_capi::hb_font_t,
        pops: AtomicUsize,
    }
    unsafe extern "C" fn release_on_first_pop(
        _funcs: *mut hb_paint_funcs_t,
        data: *mut c_void,
        _user_data: *mut c_void,
    ) {
        // SAFETY: the test passes a live `Owned` as paint_data.
        let owned = unsafe { &*data.cast::<Owned>() };
        if owned.pops.fetch_add(1, Ordering::SeqCst) == 0 {
            // SAFETY: these are the test's only references.
            unsafe {
                hb_paint_funcs_destroy(owned.funcs);
                sigilbuzz_capi::hb_font_destroy(owned.font);
            }
        }
    }
    let s = Setup::new(&font_bytes());
    let funcs = hb_paint_funcs_create();
    // The callback releases `funcs` (its only reference) and an extra
    // font reference, both while they are being painted with.
    // SAFETY: both handles are live.
    let owned = unsafe {
        hb_paint_funcs_set_pop_clip_func(funcs, Some(release_on_first_pop), ptr::null_mut(), None);
        Owned {
            funcs,
            font: sigilbuzz_capi::hb_font_reference(s.font),
            pops: AtomicUsize::new(0),
        }
    };
    // SAFETY: the handles are live at the call; `owned` outlives it.
    unsafe {
        let data = ptr::from_ref(&owned).cast_mut().cast::<c_void>();
        hb_font_paint_glyph(owned.font, 6, owned.funcs, data, 0, FG);
    }
    assert_eq!(
        owned.pops.load(Ordering::SeqCst),
        2,
        "second layer still painted"
    );
}

#[test]
fn null_font_or_funcs_is_a_no_op() {
    let s = Setup::new(&font_bytes());
    let log = Log::default();
    let data = ptr::from_ref(&log).cast_mut().cast::<c_void>();
    // SAFETY: null handles are accepted and ignored.
    unsafe {
        hb_font_paint_glyph(ptr::null_mut(), 1, s.funcs, data, 0, FG);
        hb_font_paint_glyph(s.font, 1, ptr::null_mut(), data, 0, FG);
    }
    assert!(log.events.borrow().is_empty());
}
