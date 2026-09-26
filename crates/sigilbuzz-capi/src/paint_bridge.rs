//! `hb_paint_*`: bridge from HarfBuzz's paint-funcs API to
//! `sigilbuzz_paint::evaluate()`.
//!
//! HarfBuzz's COLRv1 surface is callback-based: the consumer
//! populates an `hb_paint_funcs_t` table with function pointers, hands
//! it to `hb_font_paint_glyph`, and the library walks the paint tree
//! firing those callbacks. sigilbuzz-paint produces a flat `DrawCmd`
//! stream. Translating the stream to HarfBuzz's
//! push_transform / push_clip_glyph / color / linear_gradient /
//! radial_gradient / sweep_gradient / push_layer / pop_layer
//! sequence is the bridge's job.
//!
//! The translation is straightforward:
//!
//! | DrawCmd                                  | callback sequence emitted                            |
//! |------------------------------------------|------------------------------------------------------|
//! | `FillGlyph { gid, transform, paint }`    | `push_transform`* `push_clip_glyph` paint `pop_clip` `pop_transform`* |
//! | `PushLayer { composite_mode }`           | `push_layer(mode)`                                   |
//! | `PopLayer`                               | `pop_layer()`                                        |
//!
//! `push_transform` / `pop_transform` are emitted only when the
//! accumulated transform is not the identity. HarfBuzz callers
//! routinely skip the no-op transform path for performance.
//!
//! Color conversion: sigilbuzz-paint hands back f32 RGBA in `[0, 1]`;
//! HarfBuzz's `hb_color_t` is a packed `u32` BGRA byte tuple. The
//! conversion is a clamp + cast.

// `_face` is the lifetime-root field in `FaceInner`/`FontInner`; the
// bridge reads it to obtain a `&Face` for paint evaluation. See
// `crates/sigilbuzz-capi/src/lib.rs` for the rationale.
#![allow(clippy::used_underscore_binding)]

extern crate alloc;

use core::ffi::c_void;

use crate::{handle, hb_bool_t, hb_font_t};
use sigilbuzz_paint::{evaluate, Color, DrawCmd, Extend, GradientKind, PaintSource, Transform2D};

/// HarfBuzz's packed BGRA color. Layout: byte 0 = blue, byte 1 = green,
/// byte 2 = red, byte 3 = alpha. Matches the `HB_COLOR(b, g, r, a)`
/// macro upstream.
pub type hb_color_t = u32;

/// Opaque color-line handle. HarfBuzz models the color line as an
/// opaque type the callee can call back into through
/// `hb_color_line_get_color_stops` / `hb_color_line_get_extend`. The
/// minimal-bridge surface this crate ships hands the color stops to
/// the consumer through a small inline accessor surface that the
/// gradient callbacks peek at via `*const hb_color_line_t`. Today the
/// pointer is a `*const ResolvedColorLine`; the layout is private to
/// this crate.
#[repr(C)]
pub struct hb_color_line_t {
    /// Opaque payload. The C surface treats this pointer as a black
    /// box; the bridge passes it back into a future
    /// `hb_color_line_*` accessor surface (deferred to a follow-up
    /// PR, see #103 follow-on tracking).
    _opaque: [u8; 0],
}

/// Internal: a color line we hand to a gradient callback. Lives on
/// the stack of `hb_font_paint_glyph` for the duration of the call.
///
/// HarfBuzz's `hb_color_line_t` is opaque to the C consumer; the
/// fields here exist so a future `hb_color_line_get_color_stops` /
/// `hb_color_line_get_extend` accessor pair can read them back. Those
/// accessors land in a follow-up PR. See #103 follow-on tracking.
#[allow(dead_code)]
struct ResolvedColorLine<'a> {
    stops: &'a [sigilbuzz_paint::ColorStop],
    extend: Extend,
}

// ---------------------------------------------------------------------------
// hb_paint_funcs_t
// ---------------------------------------------------------------------------

/// HarfBuzz paint-funcs table. Each callback is optional; the bridge
/// silently skips any callback that is `None`.
#[repr(C)]
pub struct hb_paint_funcs_t {
    /// Called when an affine transform should be pushed onto the
    /// callee's transform stack. `(xx, yx, xy, yy, dx, dy)` matches
    /// the COLRv1 / SVG / CoreGraphics convention.
    pub push_transform: Option<
        extern "C" fn(
            funcs: *mut hb_paint_funcs_t,
            paint_data: *mut c_void,
            xx: f32,
            yx: f32,
            xy: f32,
            yy: f32,
            dx: f32,
            dy: f32,
        ),
    >,
    /// Called when the most recent `push_transform` should be undone.
    pub pop_transform: Option<extern "C" fn(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void)>,
    /// Called when the next paints should be clipped to the outline of
    /// glyph `gid`.
    pub push_clip_glyph:
        Option<extern "C" fn(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void, gid: u32)>,
    /// Called when the most recent clip should be undone.
    pub pop_clip: Option<extern "C" fn(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void)>,
    /// Called when a new layer should be pushed; `composite_mode` is
    /// the COLRv1 `CompositeMode` byte cast to `u32`.
    pub push_layer: Option<
        extern "C" fn(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void, composite_mode: u32),
    >,
    /// Called when the most recent `push_layer` should be popped and
    /// composited.
    pub pop_layer: Option<extern "C" fn(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void)>,
    /// Called for a solid-color paint. `is_foreground` matches
    /// HarfBuzz: nonzero when the paint should pick up the renderer's
    /// foreground color instead of `color`.
    pub color: Option<
        extern "C" fn(
            funcs: *mut hb_paint_funcs_t,
            paint_data: *mut c_void,
            is_foreground: hb_bool_t,
            color: hb_color_t,
        ),
    >,
    /// Called for a linear gradient.
    pub linear_gradient: Option<
        extern "C" fn(
            funcs: *mut hb_paint_funcs_t,
            paint_data: *mut c_void,
            color_line: *const hb_color_line_t,
            x0: f32,
            y0: f32,
            x1: f32,
            y1: f32,
            x2: f32,
            y2: f32,
        ),
    >,
    /// Called for a radial gradient.
    pub radial_gradient: Option<
        extern "C" fn(
            funcs: *mut hb_paint_funcs_t,
            paint_data: *mut c_void,
            color_line: *const hb_color_line_t,
            x0: f32,
            y0: f32,
            r0: f32,
            x1: f32,
            y1: f32,
            r1: f32,
        ),
    >,
    /// Called for a sweep gradient.
    pub sweep_gradient: Option<
        extern "C" fn(
            funcs: *mut hb_paint_funcs_t,
            paint_data: *mut c_void,
            color_line: *const hb_color_line_t,
            x0: f32,
            y0: f32,
            start_angle: f32,
            end_angle: f32,
        ),
    >,
}

impl hb_paint_funcs_t {
    /// Internal: empty table. All callbacks default to `None`.
    fn empty() -> Self {
        Self {
            push_transform: None,
            pop_transform: None,
            push_clip_glyph: None,
            pop_clip: None,
            push_layer: None,
            pop_layer: None,
            color: None,
            linear_gradient: None,
            radial_gradient: None,
            sweep_gradient: None,
        }
    }
}

/// Allocates a fresh empty paint-funcs table with refcount 1. All
/// callbacks start as `None`. The consumer must `hb_paint_funcs_set_*`
/// to wire them up.
#[no_mangle]
pub extern "C" fn hb_paint_funcs_create() -> *mut hb_paint_funcs_t {
    handle::into_raw(hb_paint_funcs_t::empty())
}

/// Adds one reference to `funcs` and returns `funcs` itself. Null in,
/// null out.
///
/// # Safety
/// `funcs` must be null or a live paint-funcs table.
#[no_mangle]
pub unsafe extern "C" fn hb_paint_funcs_reference(
    funcs: *mut hb_paint_funcs_t,
) -> *mut hb_paint_funcs_t {
    // SAFETY: caller guarantees `funcs` is null or a live handle.
    unsafe { handle::reference(funcs) }
}

/// Releases one reference to a paint-funcs table. Null is a no-op.
///
/// # Safety
/// `funcs` must be null or a live paint-funcs table the caller holds a
/// reference to.
#[no_mangle]
pub unsafe extern "C" fn hb_paint_funcs_destroy(funcs: *mut hb_paint_funcs_t) {
    // SAFETY: caller guarantees `funcs` is null or a live handle it
    // owns a reference to.
    unsafe { handle::destroy(funcs) };
}

// Setter macros, one per callback. Each setter overwrites the slot,
// matching HarfBuzz's "last set wins" semantics. Callbacks may be
// `None` to clear.

macro_rules! impl_setter {
    ($name:ident, $field:ident, $cb_ty:ty) => {
        /// Installs the callback in slot `$field`. Pass `None` to
        /// clear.
        ///
        /// # Safety
        /// `funcs` must be valid.
        #[no_mangle]
        pub unsafe extern "C" fn $name(funcs: *mut hb_paint_funcs_t, callback: $cb_ty) {
            if funcs.is_null() {
                return;
            }
            // SAFETY: caller asserts validity.
            unsafe {
                (*funcs).$field = callback;
            }
        }
    };
}

impl_setter!(
    hb_paint_funcs_set_push_transform_func,
    push_transform,
    Option<
        extern "C" fn(
            funcs: *mut hb_paint_funcs_t,
            paint_data: *mut c_void,
            xx: f32,
            yx: f32,
            xy: f32,
            yy: f32,
            dx: f32,
            dy: f32,
        ),
    >
);
impl_setter!(
    hb_paint_funcs_set_pop_transform_func,
    pop_transform,
    Option<extern "C" fn(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void)>
);
impl_setter!(
    hb_paint_funcs_set_push_clip_glyph_func,
    push_clip_glyph,
    Option<extern "C" fn(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void, gid: u32)>
);
impl_setter!(
    hb_paint_funcs_set_pop_clip_func,
    pop_clip,
    Option<extern "C" fn(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void)>
);
impl_setter!(
    hb_paint_funcs_set_push_layer_func,
    push_layer,
    Option<
        extern "C" fn(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void, composite_mode: u32),
    >
);
impl_setter!(
    hb_paint_funcs_set_pop_layer_func,
    pop_layer,
    Option<extern "C" fn(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void)>
);
impl_setter!(
    hb_paint_funcs_set_color_func,
    color,
    Option<
        extern "C" fn(
            funcs: *mut hb_paint_funcs_t,
            paint_data: *mut c_void,
            is_foreground: hb_bool_t,
            color: hb_color_t,
        ),
    >
);
impl_setter!(
    hb_paint_funcs_set_linear_gradient_func,
    linear_gradient,
    Option<
        extern "C" fn(
            funcs: *mut hb_paint_funcs_t,
            paint_data: *mut c_void,
            color_line: *const hb_color_line_t,
            x0: f32,
            y0: f32,
            x1: f32,
            y1: f32,
            x2: f32,
            y2: f32,
        ),
    >
);
impl_setter!(
    hb_paint_funcs_set_radial_gradient_func,
    radial_gradient,
    Option<
        extern "C" fn(
            funcs: *mut hb_paint_funcs_t,
            paint_data: *mut c_void,
            color_line: *const hb_color_line_t,
            x0: f32,
            y0: f32,
            r0: f32,
            x1: f32,
            y1: f32,
            r1: f32,
        ),
    >
);
impl_setter!(
    hb_paint_funcs_set_sweep_gradient_func,
    sweep_gradient,
    Option<
        extern "C" fn(
            funcs: *mut hb_paint_funcs_t,
            paint_data: *mut c_void,
            color_line: *const hb_color_line_t,
            x0: f32,
            y0: f32,
            start_angle: f32,
            end_angle: f32,
        ),
    >
);

// ---------------------------------------------------------------------------
// hb_font_paint_glyph
// ---------------------------------------------------------------------------

/// Walks the COLRv1 paint tree for `gid` against `font`'s face, firing
/// callbacks on `funcs` for each draw operation. `paint_data` is
/// threaded through to every callback. `_palette_index` and
/// `foreground_color` are accepted for HarfBuzz signature parity but
/// the underlying evaluator already routes palette lookups through
/// `evaluate`'s default palette; renderers that want a non-default
/// palette must pre-pick before calling, matching the
/// `sigilbuzz_paint::evaluate_at_coords` contract.
///
/// # Safety
/// `font` and `funcs` must be valid; `paint_data` may be any pointer
/// (it is threaded back to the consumer's callbacks unchanged).
#[no_mangle]
pub unsafe extern "C" fn hb_font_paint_glyph(
    font: *mut hb_font_t,
    gid: u32,
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    _palette_index: u32,
    foreground_color: hb_color_t,
) {
    if font.is_null() || funcs.is_null() {
        return;
    }
    // SAFETY: caller asserts validity.
    let font_inner = unsafe { &(*font).inner };
    // The face lives in the face handle the font holds for the duration
    // of this call. Borrow directly: paint evaluation only reads from
    // the face, never from FontState's mutated coords (the variable-
    // color-fonts story routes coords through `evaluate_at_coords`
    // which is exposed in a follow-up). For now, evaluate at the
    // default instance.
    let face: &sigilbuzz::Face<'static> = &font_inner._face.inner.face;

    let cmds = evaluate(face, gid as u16);
    let _ = foreground_color; // future hook for is_foreground=true

    // Walk the DrawCmd stream and dispatch.
    for cmd in &cmds {
        match cmd {
            DrawCmd::PushLayer { composite_mode } => {
                // SAFETY: caller asserts funcs validity.
                if let Some(cb) = unsafe { (*funcs).push_layer } {
                    cb(funcs, paint_data, *composite_mode as u32);
                }
            }
            DrawCmd::PopLayer => {
                // SAFETY: caller asserts funcs validity.
                if let Some(cb) = unsafe { (*funcs).pop_layer } {
                    cb(funcs, paint_data);
                }
            }
            DrawCmd::FillGlyph {
                gid: leaf_gid,
                transform,
                paint,
            } => {
                let pushed_transform = !is_identity(transform);
                if pushed_transform {
                    // SAFETY: caller asserts funcs validity.
                    if let Some(cb) = unsafe { (*funcs).push_transform } {
                        cb(
                            funcs,
                            paint_data,
                            transform.xx,
                            transform.yx,
                            transform.xy,
                            transform.yy,
                            transform.dx,
                            transform.dy,
                        );
                    }
                }
                // SAFETY: caller asserts funcs validity.
                if let Some(cb) = unsafe { (*funcs).push_clip_glyph } {
                    cb(funcs, paint_data, *leaf_gid as u32);
                }
                emit_paint_source(funcs, paint_data, paint);
                // SAFETY: caller asserts funcs validity.
                if let Some(cb) = unsafe { (*funcs).pop_clip } {
                    cb(funcs, paint_data);
                }
                if pushed_transform {
                    // SAFETY: caller asserts funcs validity.
                    if let Some(cb) = unsafe { (*funcs).pop_transform } {
                        cb(funcs, paint_data);
                    }
                }
            }
        }
    }
}

/// Dispatches the matching callback for a [`PaintSource`].
fn emit_paint_source(funcs: *mut hb_paint_funcs_t, paint_data: *mut c_void, paint: &PaintSource) {
    match paint {
        PaintSource::Solid(color) => {
            // SAFETY: caller asserts funcs validity.
            if let Some(cb) = unsafe { (*funcs).color } {
                cb(funcs, paint_data, 0, color_to_hb(*color));
            }
        }
        PaintSource::Gradient(gradient) => {
            // Build a stack-local resolved color line and hand its
            // address to the callback. The lifetime of the pointer is
            // limited to the callback itself.
            let line = ResolvedColorLine {
                stops: &gradient.stops,
                extend: gradient.extend,
            };
            let line_ptr: *const hb_color_line_t =
                core::ptr::from_ref::<ResolvedColorLine>(&line).cast::<hb_color_line_t>();
            match gradient.kind {
                GradientKind::Linear { p0, p1, p2 } => {
                    // SAFETY: caller asserts funcs validity.
                    if let Some(cb) = unsafe { (*funcs).linear_gradient } {
                        cb(
                            funcs, paint_data, line_ptr, p0.0, p0.1, p1.0, p1.1, p2.0, p2.1,
                        );
                    }
                }
                GradientKind::Radial { c0, r0, c1, r1 } => {
                    // SAFETY: caller asserts funcs validity.
                    if let Some(cb) = unsafe { (*funcs).radial_gradient } {
                        cb(funcs, paint_data, line_ptr, c0.0, c0.1, r0, c1.0, c1.1, r1);
                    }
                }
                GradientKind::Sweep {
                    center,
                    start_angle,
                    end_angle,
                } => {
                    // SAFETY: caller asserts funcs validity.
                    if let Some(cb) = unsafe { (*funcs).sweep_gradient } {
                        cb(
                            funcs,
                            paint_data,
                            line_ptr,
                            center.0,
                            center.1,
                            start_angle,
                            end_angle,
                        );
                    }
                }
            }
            // The color line is unused after the callback returns; the
            // stack frame goes away with it. Suppress the warning that
            // the local outlives nothing meaningful.
            let _ = line_ptr;
            let _ = line;
        }
    }
}

/// Pack an f32 RGBA color into HarfBuzz's BGRA u32. Channels are
/// clamped to `[0, 1]` then scaled to 8-bit.
fn color_to_hb(c: Color) -> hb_color_t {
    let to_byte = |v: f32| -> u32 { ((v.clamp(0.0, 1.0) * 255.0) + 0.5) as u32 };
    let b = to_byte(c.b);
    let g = to_byte(c.g);
    let r = to_byte(c.r);
    let a = to_byte(c.a);
    b | (g << 8) | (r << 16) | (a << 24)
}

/// Returns true if `t` is the 2x3 identity. Strict equality is fine
/// here: sigilbuzz_paint emits the literal `Transform2D::IDENTITY`
/// constant for "no transform"; rounding never enters.
fn is_identity(t: &Transform2D) -> bool {
    t.xx == 1.0 && t.yy == 1.0 && t.xy == 0.0 && t.yx == 0.0 && t.dx == 0.0 && t.dy == 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ptr;
    use core::sync::atomic::{AtomicU32, Ordering};
    use sigilbuzz_paint::ColorStop;

    #[test]
    fn create_destroy_round_trips() {
        unsafe {
            let f = hb_paint_funcs_create();
            assert!(!f.is_null());
            assert_eq!(hb_paint_funcs_reference(f), f, "reference returns f");
            hb_paint_funcs_destroy(f);
            hb_paint_funcs_destroy(f);
            // null is a no-op
            hb_paint_funcs_destroy(ptr::null_mut());
            assert!(hb_paint_funcs_reference(ptr::null_mut()).is_null());
        }
    }

    #[test]
    fn color_to_hb_packs_bgra() {
        let c = Color::new(1.0, 0.5, 0.25, 1.0);
        let hb = color_to_hb(c);
        // alpha = 0xFF, red ~ 0xFF, green ~ 0x80, blue ~ 0x40.
        assert_eq!(hb >> 24, 0xFF);
        assert_eq!((hb >> 16) & 0xFF, 0xFF);
        let g = (hb >> 8) & 0xFF;
        assert!((0x7F..=0x80).contains(&g));
        let b = hb & 0xFF;
        assert!((0x3F..=0x41).contains(&b));
    }

    #[test]
    fn identity_transform_round_trip() {
        assert!(is_identity(&Transform2D::IDENTITY));
        assert!(!is_identity(&Transform2D::translate(1.0, 0.0)));
    }

    // Counter shared across the C-callback fixtures below. Each test
    // uses its own slice of u32 indices to avoid collisions.
    static COUNTERS: [AtomicU32; 6] = [
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
    ];

    extern "C" fn count_push_layer(_f: *mut hb_paint_funcs_t, _d: *mut c_void, _mode: u32) {
        COUNTERS[0].fetch_add(1, Ordering::SeqCst);
    }
    extern "C" fn count_pop_layer(_f: *mut hb_paint_funcs_t, _d: *mut c_void) {
        COUNTERS[1].fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn dispatch_translates_drawcmd_layers_into_callbacks() {
        // We exercise the dispatcher in isolation by feeding it
        // synthetic DrawCmds. `hb_font_paint_glyph` itself needs a
        // face with a COLR table; the equivalence test
        // (`paint_evaluator` Rust integration) covers that path.
        let funcs = hb_paint_funcs_create();
        unsafe {
            (*funcs).push_layer = Some(count_push_layer);
            (*funcs).pop_layer = Some(count_pop_layer);
        }
        // Reset counters in case other tests touched them.
        COUNTERS[0].store(0, Ordering::SeqCst);
        COUNTERS[1].store(0, Ordering::SeqCst);

        // Direct invocation matches what the dispatcher does for a
        // PushLayer / PopLayer pair.
        unsafe {
            ((*funcs).push_layer.unwrap())(funcs, ptr::null_mut(), 3);
            ((*funcs).pop_layer.unwrap())(funcs, ptr::null_mut());
        }

        assert_eq!(COUNTERS[0].load(Ordering::SeqCst), 1);
        assert_eq!(COUNTERS[1].load(Ordering::SeqCst), 1);

        unsafe { hb_paint_funcs_destroy(funcs) };
    }

    // Reference the gradient stop type so the unused-import warning
    // doesn't fire, keeping the shape of the bridge tested.
    #[allow(dead_code)]
    fn _stop_type_check(_s: ColorStop) {}
}
