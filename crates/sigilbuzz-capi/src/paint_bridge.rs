//! `hb_paint_*`: HarfBuzz's paint-funcs API over sigilbuzz's COLR
//! walker.
//!
//! HarfBuzz's color-glyph surface is callback based: the caller fills
//! an `hb_paint_funcs_t` (see the `funcs` submodule) and hands it to
//! `hb_font_paint_glyph`, which walks the glyph and fires callbacks.
//! The walk itself is `sigilbuzz_paint::walk::paint_glyph`, which
//! reports steps in HarfBuzz 11's order; this module turns each step
//! into the matching callback:
//!
//! - A COLRv1 glyph: `push_clip_rectangle` with the glyph's bounds,
//!   `push_transform(root)`, the paint tree, `pop_transform`,
//!   `pop_clip`. The bounds are its ClipList box scaled to font units
//!   (see the `clip` submodule), or the bounds of its paint tree; a
//!   glyph whose paint escapes every clip paints nothing inside the
//!   root transform. The root transform maps design units to font
//!   scale: `(x_scale / upem, 0, 0, y_scale / upem, 0, 0)`.
//! - `PaintGlyph`: `push_transform(inverse root)`,
//!   `push_clip_glyph(gid, font)`, `push_transform(root)`, the child,
//!   then `pop_transform`, `pop_clip`, `pop_transform`. The clip outline
//!   is what `hb_font_draw_glyph` would draw at font scale. The inverse
//!   root transform's `xy` is `-0.0`, as HarfBuzz computes it.
//! - `PaintColrGlyph`: `push_transform(inverse root)`,
//!   `color_glyph(gid, font)`, `pop_transform`; unless the callback
//!   painted the glyph, its ClipList box (if any) as
//!   `push_clip_rectangle` in design units, its paint tree, `pop_clip`.
//! - Transform paints: one `push_transform` / `pop_transform` pair
//!   each, skipped for identity translate, scale, rotate, and skew.
//! - `PaintComposite`: `push_group`, backdrop, `push_group`, source,
//!   `pop_group(mode)`, `pop_group(SRC_OVER)`.
//! - Gradients: coordinates in design units, sweep angles as
//!   `(angle + 1) * pi` radians, stops read back through the
//!   `hb_color_line_t` (see the `color_line` submodule).
//! - A COLRv0 glyph: `push_clip_glyph(layer, font)`, `color`,
//!   `pop_clip` per layer.
//! - Any other glyph: `push_clip_glyph(gid, font)`,
//!   `color(1, foreground)`, `pop_clip`.
//!
//! Colors resolve like HarfBuzz's paint context: palette entry `0xFFFF`
//! is `foreground` with `is_foreground = 1`; any other entry asks
//! `custom_palette_color` first, then CPAL palette `palette_index`, and
//! falls back to `foreground` (with `is_foreground = 0`) when the font
//! has no such palette or entry. The paint alpha multiplies the color's
//! alpha byte and the product is truncated.
//!
//! Not emitted yet: the `image` callback for SVG and bitmap glyphs.

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{c_uint, c_void};

use sigilbuzz::tables::cpal::Cpal;
use sigilbuzz::Face;
use sigilbuzz_paint::walk::{self, ColorLineRef, ColorRef, PaintSink, Painted, RootClip};
use sigilbuzz_paint::{CompositeMode, Transform2D};

use crate::{handle, hb_bool_t, hb_codepoint_t, hb_face_t, hb_font_t};

mod clip;
mod color;
mod color_line;
mod funcs;

pub use color::{
    hb_color, hb_color_get_alpha, hb_color_get_blue, hb_color_get_green, hb_color_get_red,
    hb_color_t, hb_paint_composite_mode_t, HB_PAINT_COMPOSITE_MODE_CLEAR,
    HB_PAINT_COMPOSITE_MODE_COLOR_BURN, HB_PAINT_COMPOSITE_MODE_COLOR_DODGE,
    HB_PAINT_COMPOSITE_MODE_DARKEN, HB_PAINT_COMPOSITE_MODE_DEST,
    HB_PAINT_COMPOSITE_MODE_DEST_ATOP, HB_PAINT_COMPOSITE_MODE_DEST_IN,
    HB_PAINT_COMPOSITE_MODE_DEST_OUT, HB_PAINT_COMPOSITE_MODE_DEST_OVER,
    HB_PAINT_COMPOSITE_MODE_DIFFERENCE, HB_PAINT_COMPOSITE_MODE_EXCLUSION,
    HB_PAINT_COMPOSITE_MODE_HARD_LIGHT, HB_PAINT_COMPOSITE_MODE_HSL_COLOR,
    HB_PAINT_COMPOSITE_MODE_HSL_HUE, HB_PAINT_COMPOSITE_MODE_HSL_LUMINOSITY,
    HB_PAINT_COMPOSITE_MODE_HSL_SATURATION, HB_PAINT_COMPOSITE_MODE_LIGHTEN,
    HB_PAINT_COMPOSITE_MODE_MULTIPLY, HB_PAINT_COMPOSITE_MODE_OVERLAY,
    HB_PAINT_COMPOSITE_MODE_PLUS, HB_PAINT_COMPOSITE_MODE_SCREEN,
    HB_PAINT_COMPOSITE_MODE_SOFT_LIGHT, HB_PAINT_COMPOSITE_MODE_SRC,
    HB_PAINT_COMPOSITE_MODE_SRC_ATOP, HB_PAINT_COMPOSITE_MODE_SRC_IN,
    HB_PAINT_COMPOSITE_MODE_SRC_OUT, HB_PAINT_COMPOSITE_MODE_SRC_OVER, HB_PAINT_COMPOSITE_MODE_XOR,
};
pub use color_line::{
    hb_color_line_get_color_stops, hb_color_line_get_color_stops_func_t, hb_color_line_get_extend,
    hb_color_line_get_extend_func_t, hb_color_line_t, hb_color_stop_t, hb_paint_extend_t,
    HB_PAINT_EXTEND_PAD, HB_PAINT_EXTEND_REFLECT, HB_PAINT_EXTEND_REPEAT,
};
pub use funcs::{
    hb_glyph_extents_t, hb_paint_color_func_t, hb_paint_color_glyph_func_t,
    hb_paint_custom_palette_color_func_t, hb_paint_funcs_create, hb_paint_funcs_destroy,
    hb_paint_funcs_is_immutable, hb_paint_funcs_make_immutable, hb_paint_funcs_reference,
    hb_paint_funcs_set_color_func, hb_paint_funcs_set_color_glyph_func,
    hb_paint_funcs_set_custom_palette_color_func, hb_paint_funcs_set_image_func,
    hb_paint_funcs_set_linear_gradient_func, hb_paint_funcs_set_pop_clip_func,
    hb_paint_funcs_set_pop_group_func, hb_paint_funcs_set_pop_transform_func,
    hb_paint_funcs_set_push_clip_glyph_func, hb_paint_funcs_set_push_clip_rectangle_func,
    hb_paint_funcs_set_push_group_func, hb_paint_funcs_set_push_transform_func,
    hb_paint_funcs_set_radial_gradient_func, hb_paint_funcs_set_sweep_gradient_func,
    hb_paint_funcs_t, hb_paint_image_func_t, hb_paint_linear_gradient_func_t,
    hb_paint_pop_clip_func_t, hb_paint_pop_group_func_t, hb_paint_pop_transform_func_t,
    hb_paint_push_clip_glyph_func_t, hb_paint_push_clip_rectangle_func_t,
    hb_paint_push_group_func_t, hb_paint_push_transform_func_t, hb_paint_radial_gradient_func_t,
    hb_paint_sweep_gradient_func_t,
};

use funcs::Dispatch;

/// COLR palette entry that means "the foreground color".
const FOREGROUND_ENTRY: u16 = 0xFFFF;

/// Paints `glyph` of `font` through the callbacks in `pfuncs`, passing
/// `paint_data` to every callback. See the module docs for the
/// callback sequence.
///
/// `palette_index` selects the CPAL palette. Palette entries the font
/// cannot supply (no such palette, no such entry, no CPAL) paint in
/// `foreground`, as in HarfBuzz. Paints on the COLR foreground entry
/// report `is_foreground = 1` and `foreground` with the paint alpha
/// applied. The walk runs at the font's current variation coordinates
/// and scale.
///
/// # Safety
/// `font` and `pfuncs` must be null or live objects; `paint_data` may
/// be any pointer (it is threaded back to the callbacks unchanged).
#[no_mangle]
pub unsafe extern "C" fn hb_font_paint_glyph(
    font: *mut hb_font_t,
    glyph: hb_codepoint_t,
    pfuncs: *mut hb_paint_funcs_t,
    paint_data: *mut c_void,
    palette_index: c_uint,
    foreground: hb_color_t,
) {
    if font.is_null() || pfuncs.is_null() {
        return;
    }
    // Keep our own references for the whole walk, so a callback that
    // destroys the caller's font or funcs cannot free them under us.
    // SAFETY: `font` is non-null and the caller guarantees it is a live
    // handle, so taking a new reference to it is sound.
    let font_ref: Arc<hb_font_t> = unsafe { handle::retain(font.cast_const()) };
    // SAFETY: `pfuncs` is non-null and the caller guarantees it is a
    // live handle, so taking a new reference to it is sound.
    let funcs_ref: Arc<hb_paint_funcs_t> = unsafe { handle::retain(pfuncs.cast_const()) };

    // Copy the font state before any callback runs, so the font lock is
    // not held while user code runs and a callback that changes the
    // font does not affect this walk.
    let face: Arc<hb_face_t> = Arc::clone(&font_ref.inner.face);
    let (coords, x_scale, y_scale): (Vec<f32>, i32, i32) = {
        let state = font_ref.inner.state.lock();
        (state.coords.clone(), state.x_scale, state.y_scale)
    };
    let face = &face.inner.face;

    let dispatch = Dispatch::new(&funcs_ref, pfuncs, paint_data);
    let cpal = face.cpal().ok().flatten();
    let colors = Colors {
        dispatch: &dispatch,
        cpal: cpal.as_ref(),
        palette_index,
        foreground,
    };
    let upem = upem(face);
    let (root, inverse_root) = root_transforms(f32::from(upem), x_scale, y_scale);
    let mut bridge = Bridge {
        colors: &colors,
        font,
        root,
        inverse_root,
        scale: clip::Scale {
            upem,
            x_scale,
            y_scale,
        },
    };
    let painted = match u16::try_from(glyph) {
        Ok(gid) => walk::paint_glyph(face, gid, &coords, &mut bridge),
        // COLR glyph ids are 16-bit; larger ones have no color data.
        Err(_) => Painted::Nothing,
    };
    if painted == Painted::Nothing {
        dispatch.push_clip_glyph(glyph, font);
        dispatch.color(1, foreground);
        dispatch.pop_clip();
    }
}

/// The face's units per em as HarfBuzz reads them: `head.unitsPerEm`
/// when it is in 16..=16384, else 1000.
fn upem(face: &Face<'_>) -> u16 {
    face.head()
        .ok()
        .map(|h| h.units_per_em)
        .filter(|u| (16..=16384).contains(u))
        .unwrap_or(1000)
}

/// The root transform (design units to font scale) and its inverse, as
/// HarfBuzz builds them. HarfBuzz folds its synthetic slant into the
/// `xy` terms; sigilbuzz fonts have none, so the root's `xy` is `0.0`
/// and the inverse's is `-0.0` (HarfBuzz computes `-slant * ...`). A
/// zero scale inverts as if it were `upem`, as in HarfBuzz.
fn root_transforms(upem: f32, x_scale: i32, y_scale: i32) -> (Transform2D, Transform2D) {
    let (xs, ys) = (x_scale as f32, y_scale as f32);
    let root = Transform2D::scale(xs / upem, ys / upem);
    let inv_x = if x_scale == 0 { upem } else { xs };
    let inv_y = if y_scale == 0 { upem } else { ys };
    let inverse = Transform2D {
        xy: -0.0,
        ..Transform2D::scale(upem / inv_x, upem / inv_y)
    };
    (root, inverse)
}

/// Palette state for one paint call: HarfBuzz's color resolution.
pub(crate) struct Colors<'c> {
    dispatch: &'c Dispatch<'c>,
    cpal: Option<&'c Cpal<'c>>,
    palette_index: c_uint,
    foreground: hb_color_t,
}

impl Colors<'_> {
    /// Resolves a color reference to `(is_foreground, color)`.
    pub(crate) fn get_color(&self, color: ColorRef) -> (hb_bool_t, hb_color_t) {
        let mut packed = self.foreground;
        let mut is_foreground = 1;
        if color.palette_entry != FOREGROUND_ENTRY {
            let entry = color.palette_entry;
            if !self
                .dispatch
                .custom_palette_color(c_uint::from(entry), &mut packed)
            {
                if let Some(c) = self.cpal_color(entry) {
                    packed = c;
                }
            }
            is_foreground = 0;
        }
        (is_foreground, color::with_alpha(packed, color.alpha))
    }

    fn cpal_color(&self, entry: u16) -> Option<hb_color_t> {
        let palette = u16::try_from(self.palette_index).ok()?;
        let c = self.cpal?.color(palette, entry)?;
        Some(hb_color(c.b, c.g, c.r, c.a))
    }
}

/// Turns walk steps into callbacks.
struct Bridge<'b> {
    colors: &'b Colors<'b>,
    font: *mut hb_font_t,
    root: Transform2D,
    inverse_root: Transform2D,
    /// Font scale for the root clip rectangle.
    scale: clip::Scale,
}

impl Bridge<'_> {
    fn dispatch(&self) -> &Dispatch<'_> {
        self.colors.dispatch
    }
}

impl PaintSink for Bridge<'_> {
    fn push_transform(&mut self, transform: Transform2D) {
        self.dispatch().push_transform(transform);
    }

    fn push_root_transform(&mut self) {
        self.dispatch().push_transform(self.root);
    }

    fn push_inverse_root_transform(&mut self) {
        self.dispatch().push_transform(self.inverse_root);
    }

    fn pop_transform(&mut self) {
        self.dispatch().pop_transform();
    }

    fn push_clip_glyph(&mut self, glyph: u16) {
        self.dispatch()
            .push_clip_glyph(hb_codepoint_t::from(glyph), self.font);
    }

    fn push_clip_rectangle(&mut self, x_min: f32, y_min: f32, x_max: f32, y_max: f32) {
        self.dispatch()
            .push_clip_rectangle([x_min, y_min, x_max, y_max]);
    }

    fn push_root_clip(&mut self, clip: RootClip) {
        self.dispatch().push_clip_rectangle(self.scale.rect(clip));
    }

    fn color_glyph(&mut self, glyph: u16) -> bool {
        self.dispatch()
            .color_glyph(hb_codepoint_t::from(glyph), self.font)
    }

    fn pop_clip(&mut self) {
        self.dispatch().pop_clip();
    }

    fn push_group(&mut self) {
        self.dispatch().push_group();
    }

    fn pop_group(&mut self, mode: CompositeMode) {
        self.dispatch().pop_group(color::composite_mode_to_hb(mode));
    }

    fn color(&mut self, color: ColorRef) {
        let (is_foreground, packed) = self.colors.get_color(color);
        self.dispatch().color(is_foreground, packed);
    }

    fn linear_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        p0: (f32, f32),
        p1: (f32, f32),
        p2: (f32, f32),
    ) {
        let points = [p0.0, p0.1, p1.0, p1.1, p2.0, p2.1];
        color_line::with_color_line(line, self.colors, |cl| {
            self.dispatch().linear_gradient(cl, points);
        });
    }

    fn radial_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        c0: (f32, f32),
        r0: f32,
        c1: (f32, f32),
        r1: f32,
    ) {
        let circles = [c0.0, c0.1, r0, c1.0, c1.1, r1];
        color_line::with_color_line(line, self.colors, |cl| {
            self.dispatch().radial_gradient(cl, circles);
        });
    }

    fn sweep_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        center: (f32, f32),
        start_angle: f32,
        end_angle: f32,
    ) {
        let args = [center.0, center.1, start_angle, end_angle];
        color_line::with_color_line(line, self.colors, |cl| {
            self.dispatch().sweep_gradient(cl, args);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_transform_scales_design_units_to_font_scale() {
        let (root, inverse) = root_transforms(1000.0, 2000, 500);
        assert_eq!(root, Transform2D::scale(2.0, 0.5));
        assert_eq!(inverse, Transform2D::scale(0.5, 2.0));
        // HarfBuzz's `-slant * upem / x_scale` with no slant.
        assert!(inverse.xy.is_sign_negative() && root.xy.is_sign_positive());
        // Default scale (upem) is the identity matrix.
        let (root, inverse) = root_transforms(2048.0, 2048, 2048);
        assert_eq!(root, Transform2D::IDENTITY);
        assert_eq!(inverse, Transform2D::IDENTITY);
        // A zero scale inverts as upem.
        let (root, inverse) = root_transforms(1000.0, 0, 1000);
        assert_eq!(root, Transform2D::scale(0.0, 1.0));
        assert_eq!(inverse, Transform2D::IDENTITY);
    }
}
