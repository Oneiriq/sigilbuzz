//! `hb_shape_full` honors its shaper list, as HarfBuzz does.

use core::ffi::{c_char, c_uint};
use core::ptr;

use sigilbuzz_capi::{
    hb_blob_create, hb_blob_destroy, hb_blob_t, hb_buffer_add_utf8, hb_buffer_create,
    hb_buffer_destroy, hb_buffer_get_length, hb_face_create, hb_face_destroy, hb_font_create,
    hb_font_destroy, hb_shape_full, HB_MEMORY_MODE_READONLY,
};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

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

/// Shapes "AV" with `shaper_list` and returns the result and the
/// glyph count.
///
/// # Safety
/// `shaper_list` must be null or a null-terminated array of C strings.
unsafe fn shape_with_list(shaper_list: *const *const c_char) -> (i32, c_uint) {
    // SAFETY: every handle is created and destroyed here, and
    // `shaper_list` is valid per this function's contract.
    unsafe {
        let blob = open_sans_blob();
        let face = hb_face_create(blob, 0);
        let font = hb_font_create(face);
        let buffer = hb_buffer_create();
        hb_buffer_add_utf8(buffer, c"AV".as_ptr(), -1, 0, -1);
        let ok = hb_shape_full(font, buffer, ptr::null(), 0, shaper_list);
        let len = hb_buffer_get_length(buffer);
        hb_buffer_destroy(buffer);
        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
        (ok, len)
    }
}

/// sigilbuzz's only shaper is `ot`. A list that names it shapes as
/// usual. A list that does not name it fails, as HarfBuzz fails when
/// none of the requested shapers is available. The list used to be
/// ignored.
#[test]
fn shaper_list_must_name_ot() {
    // SAFETY: each list is a null-terminated array of C strings.
    unsafe {
        assert_eq!(shape_with_list(ptr::null()), (1, 2));

        let with_ot = [c"fallback".as_ptr(), c"ot".as_ptr(), ptr::null()];
        assert_eq!(shape_with_list(with_ot.as_ptr()), (1, 2));

        let other = [c"coretext".as_ptr(), ptr::null()];
        assert_eq!(shape_with_list(other.as_ptr()), (0, 0));

        let empty = [ptr::null::<c_char>()];
        assert_eq!(shape_with_list(empty.as_ptr()), (0, 0));
    }
}

/// An empty buffer shapes successfully whatever the list says, as in
/// HarfBuzz.
#[test]
fn empty_buffer_ignores_the_shaper_list() {
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe {
        let blob = open_sans_blob();
        let face = hb_face_create(blob, 0);
        let font = hb_font_create(face);
        let buffer = hb_buffer_create();
        let other = [c"coretext".as_ptr(), ptr::null()];
        assert_eq!(
            hb_shape_full(font, buffer, ptr::null(), 0, other.as_ptr()),
            1
        );
        hb_buffer_destroy(buffer);
        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}
