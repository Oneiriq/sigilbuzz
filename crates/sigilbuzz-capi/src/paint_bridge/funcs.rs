//! `hb_paint_funcs_t`: HarfBuzz's table of paint callbacks.
//!
//! Every callback slot holds the function, the `user_data` it was
//! installed with, and an optional destroy callback for that
//! `user_data`, exactly as in HarfBuzz:
//!
//! - `hb_paint_funcs_set_X_func(funcs, func, user_data, destroy)`
//!   installs `func`. The slot's previous `user_data` is destroyed.
//! - Installing a NULL `func` resets the slot to the default (a no-op)
//!   and destroys the new `user_data` right away.
//! - On an immutable table (`hb_paint_funcs_make_immutable`) setters
//!   change nothing and destroy the new `user_data` right away.
//! - When the last reference to the table goes, every slot's
//!   `user_data` is destroyed, in HarfBuzz's slot order.
//! - Each callback receives the table, the `paint_data` passed to
//!   `hb_font_paint_glyph`, its arguments, and its own `user_data`.
//!
//! Slots are read once per callback under a short lock, so a callback
//! may install or replace callbacks on the same table while painting.

use core::ffi::{c_uint, c_void};
use core::sync::atomic::{AtomicBool, Ordering};

use sigilbuzz_paint::Transform2D;

use super::color::{hb_color_t, hb_paint_composite_mode_t};
use super::color_line::hb_color_line_t;
use crate::spin_mutex::SpinMutex;
use crate::{
    handle, hb_blob_t, hb_bool_t, hb_codepoint_t, hb_destroy_func_t, hb_font_t, hb_position_t,
    hb_tag_t,
};

/// HarfBuzz's `hb_glyph_extents_t`, used by the image callback.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct hb_glyph_extents_t {
    /// Left side of the glyph from the origin.
    pub x_bearing: hb_position_t,
    /// Top side of the glyph from the origin.
    pub y_bearing: hb_position_t,
    /// Distance from the left extreme to the right extreme.
    pub width: hb_position_t,
    /// Distance from the top extreme to the bottom extreme.
    pub height: hb_position_t,
}

type PushTransformFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    xx: f32,
    yx: f32,
    xy: f32,
    yy: f32,
    dx: f32,
    dy: f32,
    user_data: *mut c_void,
);
/// Shape shared by `pop_transform`, `pop_clip`, and `push_group`.
type BareFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    user_data: *mut c_void,
);
type PushClipGlyphFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    glyph: hb_codepoint_t,
    font: *mut hb_font_t,
    user_data: *mut c_void,
);
type ColorGlyphFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    glyph: hb_codepoint_t,
    font: *mut hb_font_t,
    user_data: *mut c_void,
) -> hb_bool_t;
type PushClipRectangleFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    xmin: f32,
    ymin: f32,
    xmax: f32,
    ymax: f32,
    user_data: *mut c_void,
);
type ColorFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    is_foreground: hb_bool_t,
    color: hb_color_t,
    user_data: *mut c_void,
);
type ImageFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    image: *mut hb_blob_t,
    width: c_uint,
    height: c_uint,
    format: hb_tag_t,
    slant: f32,
    extents: *mut hb_glyph_extents_t,
    user_data: *mut c_void,
) -> hb_bool_t;
type LinearGradientFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    color_line: *mut hb_color_line_t,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    user_data: *mut c_void,
);
type RadialGradientFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    color_line: *mut hb_color_line_t,
    x0: f32,
    y0: f32,
    r0: f32,
    x1: f32,
    y1: f32,
    r1: f32,
    user_data: *mut c_void,
);
type SweepGradientFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    color_line: *mut hb_color_line_t,
    x0: f32,
    y0: f32,
    start_angle: f32,
    end_angle: f32,
    user_data: *mut c_void,
);
type PopGroupFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    mode: hb_paint_composite_mode_t,
    user_data: *mut c_void,
);
type CustomPaletteColorFn = unsafe extern "C" fn(
    funcs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    color_index: c_uint,
    color: *mut hb_color_t,
    user_data: *mut c_void,
) -> hb_bool_t;

/// `hb_paint_push_transform_func_t`: push `(xx, yx, xy, yy, dx, dy)`.
pub type hb_paint_push_transform_func_t = Option<PushTransformFn>;
/// `hb_paint_pop_transform_func_t`: pop the last transform.
pub type hb_paint_pop_transform_func_t = Option<BareFn>;
/// `hb_paint_color_glyph_func_t` (HarfBuzz 8.2): paint the color glyph
/// a `PaintColrGlyph` references by glyph index; return nonzero if it
/// was painted, so the walk skips its paint tree.
pub type hb_paint_color_glyph_func_t = Option<ColorGlyphFn>;
/// `hb_paint_push_clip_glyph_func_t`: clip to a glyph outline as
/// `hb_font_draw_glyph` would draw it on `font`.
pub type hb_paint_push_clip_glyph_func_t = Option<PushClipGlyphFn>;
/// `hb_paint_push_clip_rectangle_func_t`: clip to a rectangle.
pub type hb_paint_push_clip_rectangle_func_t = Option<PushClipRectangleFn>;
/// `hb_paint_pop_clip_func_t`: pop the last clip.
pub type hb_paint_pop_clip_func_t = Option<BareFn>;
/// `hb_paint_color_func_t`: fill the clip with a solid color.
pub type hb_paint_color_func_t = Option<ColorFn>;
/// `hb_paint_image_func_t`: paint an image. sigilbuzz never calls it.
pub type hb_paint_image_func_t = Option<ImageFn>;
/// `hb_paint_linear_gradient_func_t`.
pub type hb_paint_linear_gradient_func_t = Option<LinearGradientFn>;
/// `hb_paint_radial_gradient_func_t`.
pub type hb_paint_radial_gradient_func_t = Option<RadialGradientFn>;
/// `hb_paint_sweep_gradient_func_t`.
pub type hb_paint_sweep_gradient_func_t = Option<SweepGradientFn>;
/// `hb_paint_push_group_func_t`: start an isolated group.
pub type hb_paint_push_group_func_t = Option<BareFn>;
/// `hb_paint_pop_group_func_t`: composite the last group with `mode`.
pub type hb_paint_pop_group_func_t = Option<PopGroupFn>;
/// `hb_paint_custom_palette_color_func_t`: return nonzero and store a
/// color to override palette entry `color_index`.
pub type hb_paint_custom_palette_color_func_t = Option<CustomPaletteColorFn>;

/// One callback slot.
#[derive(Clone, Copy)]
struct Slot<F> {
    func: Option<F>,
    user_data: *mut c_void,
    destroy: Option<hb_destroy_func_t>,
}

impl<F> Slot<F> {
    const EMPTY: Self = Self {
        func: None,
        user_data: core::ptr::null_mut(),
        destroy: None,
    };
}

/// All slots, declared in HarfBuzz's order (the order destroy
/// callbacks run in when the table is freed).
struct Table {
    push_transform: Slot<PushTransformFn>,
    pop_transform: Slot<BareFn>,
    color_glyph: Slot<ColorGlyphFn>,
    push_clip_glyph: Slot<PushClipGlyphFn>,
    push_clip_rectangle: Slot<PushClipRectangleFn>,
    pop_clip: Slot<BareFn>,
    color: Slot<ColorFn>,
    image: Slot<ImageFn>,
    linear_gradient: Slot<LinearGradientFn>,
    radial_gradient: Slot<RadialGradientFn>,
    sweep_gradient: Slot<SweepGradientFn>,
    push_group: Slot<BareFn>,
    pop_group: Slot<PopGroupFn>,
    custom_palette_color: Slot<CustomPaletteColorFn>,
}

impl Table {
    const EMPTY: Self = Self {
        push_transform: Slot::EMPTY,
        pop_transform: Slot::EMPTY,
        color_glyph: Slot::EMPTY,
        push_clip_glyph: Slot::EMPTY,
        push_clip_rectangle: Slot::EMPTY,
        pop_clip: Slot::EMPTY,
        color: Slot::EMPTY,
        image: Slot::EMPTY,
        linear_gradient: Slot::EMPTY,
        radial_gradient: Slot::EMPTY,
        sweep_gradient: Slot::EMPTY,
        push_group: Slot::EMPTY,
        pop_group: Slot::EMPTY,
        custom_palette_color: Slot::EMPTY,
    };

    /// Every slot's destroy callback and `user_data`, in slot order.
    fn destroys(&self) -> [(Option<hb_destroy_func_t>, *mut c_void); 14] {
        [
            (self.push_transform.destroy, self.push_transform.user_data),
            (self.pop_transform.destroy, self.pop_transform.user_data),
            (self.color_glyph.destroy, self.color_glyph.user_data),
            (self.push_clip_glyph.destroy, self.push_clip_glyph.user_data),
            (
                self.push_clip_rectangle.destroy,
                self.push_clip_rectangle.user_data,
            ),
            (self.pop_clip.destroy, self.pop_clip.user_data),
            (self.color.destroy, self.color.user_data),
            (self.image.destroy, self.image.user_data),
            (self.linear_gradient.destroy, self.linear_gradient.user_data),
            (self.radial_gradient.destroy, self.radial_gradient.user_data),
            (self.sweep_gradient.destroy, self.sweep_gradient.user_data),
            (self.push_group.destroy, self.push_group.user_data),
            (self.pop_group.destroy, self.pop_group.user_data),
            (
                self.custom_palette_color.destroy,
                self.custom_palette_color.user_data,
            ),
        ]
    }
}

/// HarfBuzz paint-funcs table. Opaque to C; see the module docs for
/// the slot semantics.
pub struct hb_paint_funcs_t {
    table: SpinMutex<Table>,
    immutable: AtomicBool,
}

impl hb_paint_funcs_t {
    fn new() -> Self {
        Self {
            table: SpinMutex::new(Table::EMPTY),
            immutable: AtomicBool::new(false),
        }
    }
}

impl Drop for hb_paint_funcs_t {
    fn drop(&mut self) {
        let destroys = self.table.lock().destroys();
        for (destroy, user_data) in destroys {
            run_destroy(destroy, user_data);
        }
    }
}

fn run_destroy(destroy: Option<hb_destroy_func_t>, user_data: *mut c_void) {
    if let Some(destroy) = destroy {
        // SAFETY: the C caller handed us `destroy` together with
        // `user_data` and asked for exactly one call once the slot
        // lets go of it; every caller of this helper is that one call.
        unsafe { destroy(user_data) };
    }
}

/// Installs `func` in the slot `pick` selects, following HarfBuzz's
/// rules for NULL funcs, immutable tables, and destroy callbacks.
///
/// # Safety
/// `funcs` must be null or a live paint-funcs table.
unsafe fn install<F: Copy>(
    funcs: *mut hb_paint_funcs_t,
    func: Option<F>,
    user_data: *mut c_void,
    destroy: Option<hb_destroy_func_t>,
    pick: impl FnOnce(&mut Table) -> &mut Slot<F>,
) {
    // SAFETY: the caller guarantees `funcs` is null or live.
    let Some(funcs) = (unsafe { funcs.as_ref() }) else {
        run_destroy(destroy, user_data);
        return;
    };
    if funcs.immutable.load(Ordering::Acquire) {
        run_destroy(destroy, user_data);
        return;
    }
    let new = if func.is_some() {
        Slot {
            func,
            user_data,
            destroy,
        }
    } else {
        run_destroy(destroy, user_data);
        Slot::EMPTY
    };
    let old = {
        let mut table = funcs.table.lock();
        core::mem::replace(pick(&mut table), new)
    };
    // Outside the lock: the destroy callback may touch the table.
    run_destroy(old.destroy, old.user_data);
}

/// Allocates an empty paint-funcs table with one reference. Every
/// callback starts as the default no-op.
#[no_mangle]
pub extern "C" fn hb_paint_funcs_create() -> *mut hb_paint_funcs_t {
    handle::into_raw(hb_paint_funcs_t::new())
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

/// Releases one reference to a paint-funcs table. When the last one
/// goes, every installed `user_data` is destroyed. Null is a no-op.
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

/// Makes `funcs` immutable: later setters change nothing and destroy
/// the `user_data` they were given. Null is a no-op.
///
/// # Safety
/// `funcs` must be null or a live paint-funcs table.
#[no_mangle]
pub unsafe extern "C" fn hb_paint_funcs_make_immutable(funcs: *mut hb_paint_funcs_t) {
    // SAFETY: caller guarantees `funcs` is null or live.
    if let Some(funcs) = unsafe { funcs.as_ref() } {
        funcs.immutable.store(true, Ordering::Release);
    }
}

/// Returns nonzero once `funcs` is immutable. Null reports 0.
///
/// # Safety
/// `funcs` must be null or a live paint-funcs table.
#[no_mangle]
pub unsafe extern "C" fn hb_paint_funcs_is_immutable(funcs: *mut hb_paint_funcs_t) -> hb_bool_t {
    // SAFETY: caller guarantees `funcs` is null or live.
    let immutable = unsafe { funcs.as_ref() }.is_some_and(|f| f.immutable.load(Ordering::Acquire));
    hb_bool_t::from(immutable)
}

macro_rules! setter {
    ($(#[$doc:meta])* $name:ident, $field:ident, $ty:ty) => {
        $(#[$doc])*
        ///
        /// `user_data` reaches every call of the callback; `destroy`
        /// (if non-null) runs on `user_data` once the slot lets go of
        /// it. A NULL `func` restores the default no-op.
        ///
        /// # Safety
        /// `funcs` must be null or a live paint-funcs table. `func` and
        /// `destroy` must be null or callable with the documented
        /// arguments for as long as they stay installed.
        #[no_mangle]
        pub unsafe extern "C" fn $name(
            funcs: *mut hb_paint_funcs_t,
            func: $ty,
            user_data: *mut c_void,
            destroy: Option<hb_destroy_func_t>,
        ) {
            // SAFETY: forwarded from the caller.
            unsafe { install(funcs, func, user_data, destroy, |t| &mut t.$field) }
        }
    };
}

setter!(
    /// Installs the push-transform callback.
    hb_paint_funcs_set_push_transform_func,
    push_transform,
    hb_paint_push_transform_func_t
);
setter!(
    /// Installs the pop-transform callback.
    hb_paint_funcs_set_pop_transform_func,
    pop_transform,
    hb_paint_pop_transform_func_t
);
setter!(
    /// Installs the color-glyph callback (HarfBuzz 8.2), offered every
    /// glyph a `PaintColrGlyph` references.
    hb_paint_funcs_set_color_glyph_func,
    color_glyph,
    hb_paint_color_glyph_func_t
);
setter!(
    /// Installs the push-clip-glyph callback.
    hb_paint_funcs_set_push_clip_glyph_func,
    push_clip_glyph,
    hb_paint_push_clip_glyph_func_t
);
setter!(
    /// Installs the push-clip-rectangle callback, called with a COLRv1
    /// glyph's bounds and with the ClipList box of every glyph a
    /// `PaintColrGlyph` references.
    hb_paint_funcs_set_push_clip_rectangle_func,
    push_clip_rectangle,
    hb_paint_push_clip_rectangle_func_t
);
setter!(
    /// Installs the pop-clip callback.
    hb_paint_funcs_set_pop_clip_func,
    pop_clip,
    hb_paint_pop_clip_func_t
);
setter!(
    /// Installs the solid-color callback.
    hb_paint_funcs_set_color_func,
    color,
    hb_paint_color_func_t
);
setter!(
    /// Installs the image callback. sigilbuzz paints no image glyphs,
    /// so it is never called.
    hb_paint_funcs_set_image_func,
    image,
    hb_paint_image_func_t
);
setter!(
    /// Installs the linear-gradient callback.
    hb_paint_funcs_set_linear_gradient_func,
    linear_gradient,
    hb_paint_linear_gradient_func_t
);
setter!(
    /// Installs the radial-gradient callback.
    hb_paint_funcs_set_radial_gradient_func,
    radial_gradient,
    hb_paint_radial_gradient_func_t
);
setter!(
    /// Installs the sweep-gradient callback.
    hb_paint_funcs_set_sweep_gradient_func,
    sweep_gradient,
    hb_paint_sweep_gradient_func_t
);
setter!(
    /// Installs the push-group callback.
    hb_paint_funcs_set_push_group_func,
    push_group,
    hb_paint_push_group_func_t
);
setter!(
    /// Installs the pop-group callback.
    hb_paint_funcs_set_pop_group_func,
    pop_group,
    hb_paint_pop_group_func_t
);
setter!(
    /// Installs the custom-palette-color callback, consulted before
    /// CPAL for every palette entry other than the foreground.
    hb_paint_funcs_set_custom_palette_color_func,
    custom_palette_color,
    hb_paint_custom_palette_color_func_t
);

/// Fires callbacks on one table for one `hb_font_paint_glyph` call.
pub(crate) struct Dispatch<'f> {
    funcs: &'f hb_paint_funcs_t,
    /// The pointer C passed in, handed back to every callback.
    raw: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
}

impl<'f> Dispatch<'f> {
    /// `raw` must be the handle whose object `funcs` borrows.
    pub(crate) fn new(
        funcs: &'f hb_paint_funcs_t,
        raw: *mut hb_paint_funcs_t,
        paint_data: *mut c_void,
    ) -> Self {
        Self {
            funcs,
            raw,
            paint_data,
        }
    }

    fn slot<F: Copy>(&self, pick: impl FnOnce(&Table) -> Slot<F>) -> Slot<F> {
        pick(&self.funcs.table.lock())
    }

    pub(crate) fn push_transform(&self, t: Transform2D) {
        let s = self.slot(|t| t.push_transform);
        if let Some(f) = s.func {
            // SAFETY: the C caller installed `f` with this signature
            // and `user_data`; HarfBuzz passes it the table pointer and
            // the caller's `paint_data`, which is what we pass.
            unsafe {
                f(
                    self.raw,
                    self.paint_data,
                    t.xx,
                    t.yx,
                    t.xy,
                    t.yy,
                    t.dx,
                    t.dy,
                    s.user_data,
                );
            }
        }
    }

    pub(crate) fn pop_transform(&self) {
        let s = self.slot(|t| t.pop_transform);
        if let Some(f) = s.func {
            // SAFETY: see `push_transform`.
            unsafe { f(self.raw, self.paint_data, s.user_data) };
        }
    }

    pub(crate) fn push_clip_glyph(&self, glyph: hb_codepoint_t, font: *mut hb_font_t) {
        let s = self.slot(|t| t.push_clip_glyph);
        if let Some(f) = s.func {
            // SAFETY: see `push_transform`; `font` is the live font
            // the caller passed to `hb_font_paint_glyph`.
            unsafe { f(self.raw, self.paint_data, glyph, font, s.user_data) };
        }
    }

    pub(crate) fn push_clip_rectangle(&self, r: [f32; 4]) {
        let s = self.slot(|t| t.push_clip_rectangle);
        if let Some(f) = s.func {
            // SAFETY: see `push_transform`.
            unsafe {
                f(
                    self.raw,
                    self.paint_data,
                    r[0],
                    r[1],
                    r[2],
                    r[3],
                    s.user_data,
                );
            }
        }
    }

    /// Offers `glyph` to the color-glyph callback. False when there is
    /// no callback or it did not paint the glyph.
    pub(crate) fn color_glyph(&self, glyph: hb_codepoint_t, font: *mut hb_font_t) -> bool {
        let s = self.slot(|t| t.color_glyph);
        let Some(f) = s.func else {
            return false;
        };
        // SAFETY: see `push_clip_glyph`.
        unsafe { f(self.raw, self.paint_data, glyph, font, s.user_data) != 0 }
    }

    pub(crate) fn pop_clip(&self) {
        let s = self.slot(|t| t.pop_clip);
        if let Some(f) = s.func {
            // SAFETY: see `push_transform`.
            unsafe { f(self.raw, self.paint_data, s.user_data) };
        }
    }

    pub(crate) fn color(&self, is_foreground: hb_bool_t, color: hb_color_t) {
        let s = self.slot(|t| t.color);
        if let Some(f) = s.func {
            // SAFETY: see `push_transform`.
            unsafe { f(self.raw, self.paint_data, is_foreground, color, s.user_data) };
        }
    }

    pub(crate) fn linear_gradient(&self, line: *mut hb_color_line_t, p: [f32; 6]) {
        let s = self.slot(|t| t.linear_gradient);
        if let Some(f) = s.func {
            // SAFETY: see `push_transform`; `line` points at a color
            // line that outlives this call.
            unsafe {
                f(
                    self.raw,
                    self.paint_data,
                    line,
                    p[0],
                    p[1],
                    p[2],
                    p[3],
                    p[4],
                    p[5],
                    s.user_data,
                );
            }
        }
    }

    pub(crate) fn radial_gradient(&self, line: *mut hb_color_line_t, c: [f32; 6]) {
        let s = self.slot(|t| t.radial_gradient);
        if let Some(f) = s.func {
            // SAFETY: see `linear_gradient`.
            unsafe {
                f(
                    self.raw,
                    self.paint_data,
                    line,
                    c[0],
                    c[1],
                    c[2],
                    c[3],
                    c[4],
                    c[5],
                    s.user_data,
                );
            }
        }
    }

    pub(crate) fn sweep_gradient(&self, line: *mut hb_color_line_t, a: [f32; 4]) {
        let s = self.slot(|t| t.sweep_gradient);
        if let Some(f) = s.func {
            // SAFETY: see `linear_gradient`.
            unsafe {
                f(
                    self.raw,
                    self.paint_data,
                    line,
                    a[0],
                    a[1],
                    a[2],
                    a[3],
                    s.user_data,
                );
            }
        }
    }

    pub(crate) fn push_group(&self) {
        let s = self.slot(|t| t.push_group);
        if let Some(f) = s.func {
            // SAFETY: see `push_transform`.
            unsafe { f(self.raw, self.paint_data, s.user_data) };
        }
    }

    pub(crate) fn pop_group(&self, mode: hb_paint_composite_mode_t) {
        let s = self.slot(|t| t.pop_group);
        if let Some(f) = s.func {
            // SAFETY: see `push_transform`.
            unsafe { f(self.raw, self.paint_data, mode, s.user_data) };
        }
    }

    /// Asks the custom-palette-color callback for entry `index`. Returns
    /// false (leaving `color` as the callback left it) when there is no
    /// callback or it declines.
    pub(crate) fn custom_palette_color(&self, index: c_uint, color: &mut hb_color_t) -> bool {
        let s = self.slot(|t| t.custom_palette_color);
        let Some(f) = s.func else {
            return false;
        };
        // SAFETY: see `push_transform`; `color` is a valid, writable
        // `hb_color_t` for the duration of the call.
        unsafe { f(self.raw, self.paint_data, index, color, s.user_data) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ptr;
    use std::sync::Mutex;
    use std::vec::Vec;

    /// Destroy log keyed by the `user_data` pointer value. Each test
    /// uses its own distinct `user_data` values.
    static DESTROYED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    unsafe extern "C" fn record_destroy(user_data: *mut c_void) {
        DESTROYED.lock().expect("log").push(user_data as usize);
    }

    fn destroyed(tag: usize) -> usize {
        DESTROYED
            .lock()
            .expect("log")
            .iter()
            .filter(|&&t| t == tag)
            .count()
    }

    unsafe extern "C" fn noop(_: *mut hb_paint_funcs_t, _: *mut c_void, _: *mut c_void) {}

    fn tag(v: usize) -> *mut c_void {
        v as *mut c_void
    }

    #[test]
    fn create_reference_destroy_round_trips() {
        let f = hb_paint_funcs_create();
        assert!(!f.is_null());
        // SAFETY: `f` is live with one reference; the extra one taken
        // here is released, then the original.
        unsafe {
            assert_eq!(hb_paint_funcs_reference(f), f, "reference returns f");
            hb_paint_funcs_destroy(f);
            hb_paint_funcs_destroy(f);
            hb_paint_funcs_destroy(ptr::null_mut());
            assert!(hb_paint_funcs_reference(ptr::null_mut()).is_null());
        }
    }

    #[test]
    fn replacing_a_callback_destroys_the_old_user_data_once() {
        let f = hb_paint_funcs_create();
        // SAFETY: `f` is live until the final destroy; the user_data
        // tags are never dereferenced.
        unsafe {
            hb_paint_funcs_set_pop_clip_func(f, Some(noop), tag(0x1001), Some(record_destroy));
            assert_eq!(destroyed(0x1001), 0);
            hb_paint_funcs_set_pop_clip_func(f, Some(noop), tag(0x1002), Some(record_destroy));
            assert_eq!(destroyed(0x1001), 1, "old user_data destroyed on replace");
            assert_eq!(destroyed(0x1002), 0);
            hb_paint_funcs_destroy(f);
        }
        assert_eq!(
            destroyed(0x1002),
            1,
            "installed user_data destroyed on free"
        );
        assert_eq!(destroyed(0x1001), 1);
    }

    #[test]
    fn null_func_destroys_the_new_user_data_immediately() {
        let f = hb_paint_funcs_create();
        // SAFETY: `f` is live until the final destroy; the user_data
        // tags are never dereferenced.
        unsafe {
            hb_paint_funcs_set_push_group_func(f, Some(noop), tag(0x2001), Some(record_destroy));
            hb_paint_funcs_set_push_group_func(f, None, tag(0x2002), Some(record_destroy));
            assert_eq!(destroyed(0x2002), 1, "new user_data destroyed at once");
            assert_eq!(destroyed(0x2001), 1, "old user_data released too");
            let slot = f.as_ref().expect("live").table.lock().push_group;
            assert!(slot.func.is_none() && slot.user_data.is_null());
            hb_paint_funcs_destroy(f);
        }
        assert_eq!(destroyed(0x2001), 1);
        assert_eq!(destroyed(0x2002), 1);
    }

    #[test]
    fn immutable_tables_refuse_setters() {
        let f = hb_paint_funcs_create();
        // SAFETY: `f` is live until its destroy; null is accepted by
        // every function called on it here.
        unsafe {
            assert_eq!(hb_paint_funcs_is_immutable(f), 0);
            hb_paint_funcs_set_pop_transform_func(f, Some(noop), tag(0x3001), Some(record_destroy));
            hb_paint_funcs_make_immutable(f);
            assert_eq!(hb_paint_funcs_is_immutable(f), 1);
            hb_paint_funcs_set_pop_transform_func(f, Some(noop), tag(0x3002), Some(record_destroy));
            assert_eq!(destroyed(0x3002), 1, "refused user_data destroyed at once");
            assert_eq!(destroyed(0x3001), 0, "installed callback kept");
            let slot = f.as_ref().expect("live").table.lock().pop_transform;
            assert_eq!(slot.user_data, tag(0x3001));
            hb_paint_funcs_destroy(f);
            assert_eq!(hb_paint_funcs_is_immutable(ptr::null_mut()), 0);
            hb_paint_funcs_make_immutable(ptr::null_mut());
        }
        assert_eq!(destroyed(0x3001), 1);
    }

    #[test]
    fn null_table_still_destroys_the_user_data() {
        // SAFETY: a null table is accepted; the tag is never
        // dereferenced.
        unsafe {
            hb_paint_funcs_set_color_func(ptr::null_mut(), None, tag(0x4001), Some(record_destroy));
        }
        assert_eq!(destroyed(0x4001), 1);
    }

    #[test]
    fn freeing_destroys_every_slot_in_harfbuzz_order() {
        let f = hb_paint_funcs_create();
        // SAFETY: `f` is live until its destroy; the user_data tags are
        // never dereferenced.
        unsafe {
            // Install in reverse order; destruction follows slot order.
            hb_paint_funcs_set_pop_group_func(f, None, tag(0x5000), None);
            hb_paint_funcs_set_push_group_func(f, Some(noop), tag(0x5012), Some(record_destroy));
            hb_paint_funcs_set_pop_clip_func(f, Some(noop), tag(0x5005), Some(record_destroy));
            hb_paint_funcs_set_push_transform_func(f, None, tag(0x5006), None);
            hb_paint_funcs_set_pop_transform_func(f, Some(noop), tag(0x5002), Some(record_destroy));
            hb_paint_funcs_destroy(f);
        }
        let log = DESTROYED.lock().expect("log").clone();
        let ours: Vec<usize> = log
            .into_iter()
            .filter(|t| (0x5000..0x5100).contains(t))
            .collect();
        assert_eq!(ours, [0x5002, 0x5005, 0x5012]);
    }
}
