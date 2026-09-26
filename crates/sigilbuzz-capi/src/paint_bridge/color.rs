//! `hb_color_t` packing and `hb_paint_composite_mode_t` values.
//!
//! HarfBuzz defines `HB_COLOR(b, g, r, a)` as `HB_TAG(b, g, r, a)`, so
//! blue is the most significant byte and alpha the least significant:
//! `hb_color_get_alpha(c)` is `c & 0xFF` and `hb_color_get_blue(c)` is
//! `c >> 24`. That is also CPAL's on-disk `ColorRecord` (blue, green,
//! red, alpha) read as a big-endian `uint32`.

use core::ffi::c_uint;

use sigilbuzz_paint::CompositeMode;

/// HarfBuzz's packed color: `(b << 24) | (g << 16) | (r << 8) | a`.
pub type hb_color_t = u32;

/// `HB_COLOR(b, g, r, a)`.
#[must_use]
pub const fn hb_color(b: u8, g: u8, r: u8, a: u8) -> hb_color_t {
    ((b as u32) << 24) | ((g as u32) << 16) | ((r as u32) << 8) | (a as u32)
}

/// Alpha channel of `color`.
#[no_mangle]
pub extern "C" fn hb_color_get_alpha(color: hb_color_t) -> u8 {
    color as u8
}

/// Red channel of `color`.
#[no_mangle]
pub extern "C" fn hb_color_get_red(color: hb_color_t) -> u8 {
    (color >> 8) as u8
}

/// Green channel of `color`.
#[no_mangle]
pub extern "C" fn hb_color_get_green(color: hb_color_t) -> u8 {
    (color >> 16) as u8
}

/// Blue channel of `color`.
#[no_mangle]
pub extern "C" fn hb_color_get_blue(color: hb_color_t) -> u8 {
    (color >> 24) as u8
}

/// `color` with its alpha byte multiplied by `alpha`, the way HarfBuzz's
/// paint context does it: `HB_COLOR(b, g, r, alpha_byte * alpha)`, a
/// float product truncated to an integer. `alpha` is clamped to
/// `[0, 1]` first; HarfBuzz leaves it unclamped, which only matters for
/// out-of-range values from malformed fonts.
#[must_use]
pub(crate) fn with_alpha(color: hb_color_t, alpha: f32) -> hb_color_t {
    let a = f32::from(hb_color_get_alpha(color)) * alpha.clamp(0.0, 1.0);
    (color & 0xFFFF_FF00) | (a as u32 & 0xFF)
}

/// HarfBuzz's `hb_paint_composite_mode_t`: the COLRv1 `CompositeMode`
/// value.
pub type hb_paint_composite_mode_t = c_uint;

/// Converts a composite mode to the value HarfBuzz passes to
/// `pop_group`.
#[must_use]
pub(crate) fn composite_mode_to_hb(mode: CompositeMode) -> hb_paint_composite_mode_t {
    c_uint::from(mode as u8)
}

macro_rules! modes {
    ($($name:ident = $value:literal,)*) => {
        $(
            #[doc = concat!("`", stringify!($name), "`.")]
            pub const $name: hb_paint_composite_mode_t = $value;
        )*
    };
}

modes! {
    HB_PAINT_COMPOSITE_MODE_CLEAR = 0,
    HB_PAINT_COMPOSITE_MODE_SRC = 1,
    HB_PAINT_COMPOSITE_MODE_DEST = 2,
    HB_PAINT_COMPOSITE_MODE_SRC_OVER = 3,
    HB_PAINT_COMPOSITE_MODE_DEST_OVER = 4,
    HB_PAINT_COMPOSITE_MODE_SRC_IN = 5,
    HB_PAINT_COMPOSITE_MODE_DEST_IN = 6,
    HB_PAINT_COMPOSITE_MODE_SRC_OUT = 7,
    HB_PAINT_COMPOSITE_MODE_DEST_OUT = 8,
    HB_PAINT_COMPOSITE_MODE_SRC_ATOP = 9,
    HB_PAINT_COMPOSITE_MODE_DEST_ATOP = 10,
    HB_PAINT_COMPOSITE_MODE_XOR = 11,
    HB_PAINT_COMPOSITE_MODE_PLUS = 12,
    HB_PAINT_COMPOSITE_MODE_SCREEN = 13,
    HB_PAINT_COMPOSITE_MODE_OVERLAY = 14,
    HB_PAINT_COMPOSITE_MODE_DARKEN = 15,
    HB_PAINT_COMPOSITE_MODE_LIGHTEN = 16,
    HB_PAINT_COMPOSITE_MODE_COLOR_DODGE = 17,
    HB_PAINT_COMPOSITE_MODE_COLOR_BURN = 18,
    HB_PAINT_COMPOSITE_MODE_HARD_LIGHT = 19,
    HB_PAINT_COMPOSITE_MODE_SOFT_LIGHT = 20,
    HB_PAINT_COMPOSITE_MODE_DIFFERENCE = 21,
    HB_PAINT_COMPOSITE_MODE_EXCLUSION = 22,
    HB_PAINT_COMPOSITE_MODE_MULTIPLY = 23,
    HB_PAINT_COMPOSITE_MODE_HSL_HUE = 24,
    HB_PAINT_COMPOSITE_MODE_HSL_SATURATION = 25,
    HB_PAINT_COMPOSITE_MODE_HSL_COLOR = 26,
    HB_PAINT_COMPOSITE_MODE_HSL_LUMINOSITY = 27,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hb_color_puts_blue_high_and_alpha_low() {
        let c = hb_color(0x11, 0x22, 0x33, 0x44);
        assert_eq!(c, 0x1122_3344);
        assert_eq!(hb_color_get_blue(c), 0x11);
        assert_eq!(hb_color_get_green(c), 0x22);
        assert_eq!(hb_color_get_red(c), 0x33);
        assert_eq!(hb_color_get_alpha(c), 0x44);
    }

    #[test]
    fn alpha_multiplies_and_truncates() {
        let c = hb_color(1, 2, 3, 0xFF);
        assert_eq!(with_alpha(c, 1.0), c);
        // 255 * 0.5 = 127.5 truncates to 127.
        assert_eq!(with_alpha(c, 0.5), hb_color(1, 2, 3, 127));
        // 200 * 0.5 = 100 exactly.
        assert_eq!(
            with_alpha(hb_color(0, 0, 0, 200), 0.5),
            hb_color(0, 0, 0, 100)
        );
        // Out-of-range alphas clamp.
        assert_eq!(with_alpha(c, 2.0), c);
        assert_eq!(with_alpha(c, -1.0), hb_color(1, 2, 3, 0));
    }

    #[test]
    fn composite_modes_are_the_colr_values() {
        assert_eq!(
            composite_mode_to_hb(CompositeMode::Clear),
            HB_PAINT_COMPOSITE_MODE_CLEAR
        );
        assert_eq!(
            composite_mode_to_hb(CompositeMode::SrcOver),
            HB_PAINT_COMPOSITE_MODE_SRC_OVER
        );
        assert_eq!(
            composite_mode_to_hb(CompositeMode::HslLuminosity),
            HB_PAINT_COMPOSITE_MODE_HSL_LUMINOSITY
        );
    }
}
