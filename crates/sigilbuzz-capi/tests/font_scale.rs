//! `hb_font_set_scale` scales the positions `hb_shape` reports, as it
//! does in HarfBuzz.

use core::ffi::{c_char, c_uint};
use core::ptr;
use core::slice;

use sigilbuzz_capi::{
    hb_blob_create, hb_blob_destroy, hb_blob_t, hb_buffer_add_utf8, hb_buffer_create,
    hb_buffer_destroy, hb_buffer_get_glyph_positions, hb_buffer_set_direction, hb_direction_t,
    hb_face_create, hb_face_destroy, hb_face_get_upem, hb_font_create, hb_font_destroy,
    hb_font_set_scale, hb_font_t, hb_glyph_position_t, hb_shape, HB_DIRECTION_LTR,
    HB_DIRECTION_TTB, HB_MEMORY_MODE_READONLY,
};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

/// Kerned pairs (`AV`, `AT`) so the positions include GPOS or `kern`
/// adjustments as well as plain advances.
const TEXT: &core::ffi::CStr = c"AVATAR Wave";

fn open_sans_blob() -> *mut hb_blob_t {
    // SAFETY: `OPEN_SANS` is a static slice, and the blob copies it.
    unsafe {
        hb_blob_create(
            OPEN_SANS.as_ptr().cast::<c_char>(),
            OPEN_SANS.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            ptr::null_mut(),
            None,
        )
    }
}

/// Shapes `TEXT` with `font` in `direction` and returns the positions.
///
/// # Safety
/// `font` must be a live font handle.
unsafe fn positions(font: *mut hb_font_t, direction: hb_direction_t) -> Vec<hb_glyph_position_t> {
    // SAFETY: `font` is live per this function's contract. The buffer
    // is created and destroyed here, and the positions are copied out
    // before it is destroyed.
    unsafe {
        let buffer = hb_buffer_create();
        hb_buffer_add_utf8(buffer, TEXT.as_ptr(), -1, 0, -1);
        hb_buffer_set_direction(buffer, direction);
        hb_shape(font, buffer, ptr::null(), 0);
        let mut len: c_uint = 0;
        let pos = hb_buffer_get_glyph_positions(buffer, &mut len);
        let out = slice::from_raw_parts(pos, len as usize).to_vec();
        hb_buffer_destroy(buffer);
        out
    }
}

/// HarfBuzz reports positions in the font's scale. The shaper works in
/// design units, and `hb_font_set_scale` used to change nothing in
/// the output. A scale of twice the upem doubles every horizontal
/// value, and a negative scale flips it, as in HarfBuzz.
#[test]
fn set_scale_scales_horizontal_positions() {
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe {
        let blob = open_sans_blob();
        let face = hb_face_create(blob, 0);
        let upem = hb_face_get_upem(face) as i32;
        let font = hb_font_create(face);

        let design = positions(font, HB_DIRECTION_LTR);
        assert!(design.iter().any(|p| p.x_advance > 0));

        hb_font_set_scale(font, 2 * upem, 2 * upem);
        let doubled = positions(font, HB_DIRECTION_LTR);
        assert_eq!(doubled.len(), design.len());
        for (d, s) in design.iter().zip(&doubled) {
            assert_eq!(s.x_advance, 2 * d.x_advance);
            assert_eq!(s.x_offset, 2 * d.x_offset);
            assert_eq!(s.y_offset, 2 * d.y_offset);
        }

        hb_font_set_scale(font, -upem, upem);
        let flipped = positions(font, HB_DIRECTION_LTR);
        for (d, s) in design.iter().zip(&flipped) {
            assert_eq!(s.x_advance, -d.x_advance);
        }

        // Back at the upem the output is design units again.
        hb_font_set_scale(font, upem, upem);
        let restored = positions(font, HB_DIRECTION_LTR);
        for (d, s) in design.iter().zip(&restored) {
            assert_eq!(s.x_advance, d.x_advance);
        }

        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}

/// A fractional scale rounds half up, the way HarfBuzz's `em_mult`
/// rounds. Open Sans has 2048 units per em, so half the upem halves
/// each advance exactly and rounds an odd one up.
#[test]
fn fractional_scale_rounds_half_up() {
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe {
        let blob = open_sans_blob();
        let face = hb_face_create(blob, 0);
        let upem = hb_face_get_upem(face) as i32;
        assert_eq!(upem % 2, 0);
        let font = hb_font_create(face);

        let design = positions(font, HB_DIRECTION_LTR);
        hb_font_set_scale(font, upem / 2, upem / 2);
        let half = positions(font, HB_DIRECTION_LTR);
        assert!(design.iter().any(|p| p.x_advance % 2 != 0));
        for (d, h) in design.iter().zip(&half) {
            assert_eq!(h.x_advance, (d.x_advance + 1).div_euclid(2));
        }

        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}

/// `y_scale` scales vertical values on its own, and `x_scale` leaves
/// them alone.
#[test]
fn y_scale_scales_vertical_advances() {
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe {
        let blob = open_sans_blob();
        let face = hb_face_create(blob, 0);
        let upem = hb_face_get_upem(face) as i32;
        let font = hb_font_create(face);

        let design = positions(font, HB_DIRECTION_TTB);
        assert!(design.iter().any(|p| p.y_advance != 0));

        hb_font_set_scale(font, upem, 3 * upem);
        let tall = positions(font, HB_DIRECTION_TTB);
        for (d, t) in design.iter().zip(&tall) {
            assert_eq!(t.y_advance, 3 * d.y_advance);
            assert_eq!(t.y_offset, 3 * d.y_offset);
            assert_eq!(t.x_offset, d.x_offset);
            assert_eq!(t.x_advance, d.x_advance);
        }

        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}
