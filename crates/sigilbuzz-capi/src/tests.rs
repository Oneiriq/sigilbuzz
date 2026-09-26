//! Rust-side equivalence tests: the C surface against the native Rust
//! API, plus refcounting, tags, directions, scripts, the version, and
//! buffer segment properties.

use super::*;
use core::ptr;
use core::slice;
use sigilbuzz::{
    shape as sb_shape, Blob as SbBlob, Buffer as SbBuffer, Face as SbFace, Font as SbFont,
};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

/// Pipe "Hello" through the C surface and through the native
/// Rust API; assert the glyph stream is byte-identical.
#[test]
fn shape_hello_matches_rust_api() {
    // C surface.
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe {
        let blob = hb_blob_create(
            OPEN_SANS.as_ptr().cast::<c_char>(),
            OPEN_SANS.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            ptr::null_mut(),
            None,
        );
        assert!(!blob.is_null());
        let face = hb_face_create(blob, 0);
        assert!(!face.is_null());
        let upem = hb_face_get_upem(face);
        assert!(upem > 0);
        let font = hb_font_create(face);
        assert!(!font.is_null());
        // Default scale is upem; preserve to stay in design units.
        let buffer = hb_buffer_create();
        hb_buffer_add_utf8(buffer, c"Hello".as_ptr(), -1, 0, -1);
        hb_buffer_set_direction(buffer, HB_DIRECTION_LTR);
        hb_buffer_set_script(buffer, HB_SCRIPT_LATIN);

        let ok = hb_shape_full(font, buffer, ptr::null(), 0, ptr::null());
        assert_eq!(ok, 1);

        let mut len: c_uint = 0;
        let infos = hb_buffer_get_glyph_infos(buffer, &mut len);
        assert!(
            len >= 5,
            "expected at least 5 glyphs for 'Hello', got {len}"
        );
        assert!(!infos.is_null());
        let infos_slice = slice::from_raw_parts(infos, len as usize);
        let positions = hb_buffer_get_glyph_positions(buffer, &mut len);
        let positions_slice = slice::from_raw_parts(positions, len as usize);

        // Rust surface.
        let rust_blob = SbBlob::new(OPEN_SANS);
        let rust_face = SbFace::parse(&rust_blob, 0).unwrap();
        let rust_font = SbFont::new(rust_face, upem as f32);
        let mut rust_buffer = SbBuffer::new();
        rust_buffer.push_str("Hello");
        let rust_glyphs = sb_shape(&rust_font, &rust_buffer, &[]).unwrap().glyphs;

        assert_eq!(infos_slice.len(), rust_glyphs.len());
        for (c, r) in infos_slice.iter().zip(rust_glyphs.iter()) {
            assert_eq!(c.codepoint, r.glyph_id);
            assert_eq!(c.cluster, r.cluster);
        }
        for (c, r) in positions_slice.iter().zip(rust_glyphs.iter()) {
            assert_eq!(c.x_advance, r.x_advance);
            assert_eq!(c.y_advance, r.y_advance);
            assert_eq!(c.x_offset, r.x_offset);
            assert_eq!(c.y_offset, r.y_offset);
        }

        hb_buffer_destroy(buffer);
        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}

#[test]
fn refcount_keeps_face_alive_after_blob_destroy() {
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe {
        let blob = hb_blob_create(
            OPEN_SANS.as_ptr().cast::<c_char>(),
            OPEN_SANS.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            ptr::null_mut(),
            None,
        );
        let face = hb_face_create(blob, 0);
        // Drop the blob handle first. Face must still respond.
        hb_blob_destroy(blob);
        let upem = hb_face_get_upem(face);
        assert!(upem > 0);
        hb_face_destroy(face);
    }
}

#[test]
fn null_destroy_is_a_noop() {
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe {
        hb_blob_destroy(ptr::null_mut());
        hb_face_destroy(ptr::null_mut());
        hb_font_destroy(ptr::null_mut());
        hb_buffer_destroy(ptr::null_mut());
    }
}

#[test]
fn tag_round_trips() {
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe {
        let t = hb_tag_from_string(c"Latn".as_ptr(), -1);
        assert_eq!(t, HB_SCRIPT_LATIN);
        let mut buf = [0i8; 4];
        hb_tag_to_string(t, buf.as_mut_ptr());
        let bytes: [u8; 4] = [buf[0] as u8, buf[1] as u8, buf[2] as u8, buf[3] as u8];
        assert_eq!(&bytes, b"Latn");
    }
}

#[test]
fn direction_constants_match_harfbuzz_spec() {
    // HarfBuzz wire values.
    assert_eq!(HB_DIRECTION_INVALID, 0);
    assert_eq!(HB_DIRECTION_LTR, 4);
    assert_eq!(HB_DIRECTION_RTL, 5);
    assert_eq!(HB_DIRECTION_TTB, 6);
    assert_eq!(HB_DIRECTION_BTT, 7);
}

#[test]
fn script_constants_match_iso15924_packing() {
    // Latn = 0x4C 61 74 6E.
    assert_eq!(HB_SCRIPT_LATIN, 0x4C61_746E);
    assert_eq!(HB_SCRIPT_ARABIC, 0x4172_6162);
}

#[test]
fn version_string_identifies_sigilbuzz() {
    let p = hb_version_string();
    assert!(!p.is_null());
    // SAFETY: hb_version_string returns a static NUL-terminated
    // string.
    let s = unsafe { core::ffi::CStr::from_ptr(p) }.to_string_lossy();
    assert!(s.contains("sigilbuzz"));
    assert!(s.contains("hb-compatible"));
}

#[test]
fn version_advertises_hb_compat_eight_two() {
    let mut major: c_uint = 0;
    let mut minor: c_uint = 0;
    let mut micro: c_uint = 0;
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe { hb_version(&mut major, &mut minor, &mut micro) };
    assert_eq!(major, 8);
    assert_eq!(minor, 2);
    assert_eq!(micro, 0);
}

#[test]
fn buffer_guess_segment_properties_seeds_latin_for_ascii() {
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe {
        let buffer = hb_buffer_create();
        hb_buffer_add_utf8(buffer, c"Hello".as_ptr(), -1, 0, -1);
        hb_buffer_guess_segment_properties(buffer);
        // direction defaults to LTR; script should be Latin.
        // Read direction and script back via the state mutex.
        let inner = &(*buffer).inner;
        let st = inner.state.lock();
        assert_eq!(st.direction, HB_DIRECTION_LTR);
        assert_eq!(st.script, HB_SCRIPT_LATIN);
        drop(st);
        hb_buffer_destroy(buffer);
    }
}

#[test]
fn utf16_input_matches_utf8() {
    // SAFETY: every pointer passed here is null or a live handle
    // created in this test, and each handle is destroyed once.
    unsafe {
        // "Hi" in UTF-16.
        let utf16: [u16; 2] = [b'H' as u16, b'i' as u16];
        let buf16 = hb_buffer_create();
        hb_buffer_add_utf16(buf16, utf16.as_ptr(), 2, 0, -1);
        let inner16 = &(*buf16).inner;
        let st16 = inner16.state.lock();
        assert_eq!(st16.buffer.text(), "Hi");
        drop(st16);
        hb_buffer_destroy(buf16);
    }
}
