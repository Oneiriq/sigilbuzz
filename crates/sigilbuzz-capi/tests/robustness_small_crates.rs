//! Robustness tests for the C surface: null arguments, hostile font
//! bytes, out-of-range ids, odd strings, and shared sets.

use core::ffi::{c_char, c_int, c_uint};
use core::ptr;

use sigilbuzz_capi::introspect::{hb_face_collect_unicodes, hb_ot_layout_collect_features};
use sigilbuzz_capi::set::{
    hb_set_add, hb_set_create, hb_set_del, hb_set_destroy, hb_set_get_population, hb_set_has,
    hb_set_next, hb_set_reference, hb_set_t,
};
use sigilbuzz_capi::{
    hb_blob_create, hb_blob_destroy, hb_blob_get_data, hb_blob_get_length, hb_blob_reference,
    hb_buffer_add_utf16, hb_buffer_add_utf8, hb_buffer_clear_contents, hb_buffer_create,
    hb_buffer_destroy, hb_buffer_get_glyph_infos, hb_buffer_get_glyph_positions,
    hb_buffer_get_length, hb_buffer_guess_segment_properties, hb_buffer_reference, hb_buffer_reset,
    hb_buffer_set_direction, hb_buffer_set_language, hb_buffer_set_script,
    hb_direction_from_string, hb_face_create, hb_face_destroy, hb_face_get_glyph_count,
    hb_face_get_upem, hb_face_reference, hb_font_create, hb_font_destroy, hb_font_get_scale,
    hb_font_reference, hb_font_set_ppem, hb_font_set_scale, hb_font_set_variations,
    hb_language_from_string, hb_shape, hb_shape_full, hb_tag_from_string, hb_tag_to_string,
    hb_variation_t, hb_version, HB_DIRECTION_INVALID, HB_MEMORY_MODE_READONLY,
};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

fn blob_from(bytes: &[u8]) -> *mut sigilbuzz_capi::hb_blob_t {
    // SAFETY: every pointer passed here is null, a live handle
    // created in this test, or a slice that outlives the call.
    unsafe {
        hb_blob_create(
            bytes.as_ptr().cast::<c_char>(),
            bytes.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            ptr::null_mut(),
            None,
        )
    }
}

/// Every entry point must accept NULL object arguments without
/// dereferencing them.
#[test]
fn every_entry_point_accepts_null() {
    // SAFETY: every pointer passed here is null, a live handle
    // created in this test, or a slice that outlives the call.
    unsafe {
        hb_blob_destroy(ptr::null_mut());
        let b = hb_blob_reference(ptr::null_mut());
        assert_eq!(hb_blob_get_length(b), 0);
        hb_blob_destroy(b);
        let mut len: c_uint = 7;
        assert!(hb_blob_get_data(ptr::null_mut(), &mut len).is_null());
        assert_eq!(len, 0);
        assert!(hb_blob_get_data(ptr::null_mut(), ptr::null_mut()).is_null());
        assert_eq!(hb_blob_get_length(ptr::null_mut()), 0);
        let empty = hb_blob_create(ptr::null(), 10, 0, ptr::null_mut(), None);
        assert_eq!(hb_blob_get_length(empty), 0);
        hb_blob_destroy(empty);

        let face = hb_face_create(ptr::null_mut(), 0);
        assert_eq!(hb_face_get_glyph_count(face), 0);
        assert_eq!(hb_face_get_upem(face), 0);
        hb_face_destroy(face);
        let face = hb_face_reference(ptr::null_mut());
        hb_face_destroy(face);
        assert_eq!(hb_face_get_glyph_count(ptr::null_mut()), 0);
        assert_eq!(hb_face_get_upem(ptr::null_mut()), 0);

        let font = hb_font_create(ptr::null_mut());
        assert!(!font.is_null());
        hb_font_destroy(font);
        assert!(hb_font_reference(ptr::null_mut()).is_null());
        hb_font_set_scale(ptr::null_mut(), 1, 1);
        hb_font_get_scale(ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        hb_font_set_ppem(ptr::null_mut(), 1, 1);
        hb_font_set_variations(ptr::null_mut(), ptr::null(), 3);

        assert!(hb_buffer_reference(ptr::null_mut()).is_null());
        hb_buffer_reset(ptr::null_mut());
        hb_buffer_clear_contents(ptr::null_mut());
        hb_buffer_add_utf8(ptr::null_mut(), c"x".as_ptr(), -1, 0, -1);
        hb_buffer_add_utf16(ptr::null_mut(), [0u16].as_ptr(), -1, 0, -1);
        hb_buffer_set_direction(ptr::null_mut(), 4);
        hb_buffer_set_script(ptr::null_mut(), 0);
        hb_buffer_set_language(ptr::null_mut(), ptr::null());
        hb_buffer_guess_segment_properties(ptr::null_mut());
        let mut n: c_uint = 9;
        assert!(hb_buffer_get_glyph_infos(ptr::null_mut(), &mut n).is_null());
        assert_eq!(n, 0);
        n = 9;
        assert!(hb_buffer_get_glyph_positions(ptr::null_mut(), &mut n).is_null());
        assert_eq!(n, 0);
        assert_eq!(hb_buffer_get_length(ptr::null_mut()), 0);

        let buffer = hb_buffer_create();
        hb_buffer_add_utf8(buffer, ptr::null(), 4, 0, -1);
        hb_buffer_add_utf16(buffer, ptr::null(), 4, 0, -1);
        hb_shape(ptr::null_mut(), buffer, ptr::null(), 0);
        assert_eq!(
            hb_shape_full(
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
                0,
                ptr::null()
            ),
            0
        );
        hb_buffer_destroy(buffer);

        assert_eq!(hb_tag_from_string(ptr::null(), -1), 0);
        hb_tag_to_string(0, ptr::null_mut());
        assert_eq!(
            hb_direction_from_string(ptr::null(), -1),
            HB_DIRECTION_INVALID
        );
        assert!(hb_language_from_string(ptr::null(), -1).is_null());
        hb_version(ptr::null_mut(), ptr::null_mut(), ptr::null_mut());

        hb_face_collect_unicodes(ptr::null(), ptr::null_mut());
        hb_ot_layout_collect_features(ptr::null(), 0, ptr::null(), ptr::null(), ptr::null_mut());
        assert!(hb_set_reference(ptr::null_mut()).is_null());
    }
}

/// Garbage, truncated, and empty font bytes must yield an empty face
/// that shapes to nothing, never a crash.
#[test]
fn hostile_font_bytes_shape_without_crashing() {
    let mut inputs: Vec<Vec<u8>> = vec![
        Vec::new(),
        vec![0u8; 3],
        vec![0xFF; 64],
        b"ttcf\x00\x02\x00\x00\xff\xff\xff\xff".to_vec(),
        b"wOFF".to_vec(),
    ];
    // Truncated copies of a real font at a few cut points.
    for cut in [12, 100, 1000, OPEN_SANS.len() / 2] {
        inputs.push(OPEN_SANS[..cut].to_vec());
    }
    // A real font with a corrupted table directory.
    let mut corrupt = OPEN_SANS.to_vec();
    for b in corrupt.iter_mut().skip(12).take(160) {
        *b = 0xFF;
    }
    inputs.push(corrupt);

    for bytes in &inputs {
        // SAFETY: every pointer passed here is null, a live handle
        // created in this test, or a slice that outlives the call.
        unsafe {
            let blob = blob_from(bytes);
            for index in [0, 1, u32::MAX] {
                let face = hb_face_create(blob, index);
                assert!(!face.is_null());
                let font = hb_font_create(face);
                let buffer = hb_buffer_create();
                hb_buffer_add_utf8(buffer, c"Hello".as_ptr(), -1, 0, -1);
                hb_buffer_guess_segment_properties(buffer);
                hb_shape(font, buffer, ptr::null(), 0);
                let mut n: c_uint = 0;
                let _ = hb_buffer_get_glyph_infos(buffer, &mut n);
                let set = hb_set_create();
                hb_ot_layout_collect_features(
                    face,
                    u32::from_be_bytes(*b"GSUB"),
                    ptr::null(),
                    ptr::null(),
                    set,
                );
                hb_set_destroy(set);
                hb_buffer_destroy(buffer);
                hb_font_destroy(font);
                hb_face_destroy(face);
            }
            hb_blob_destroy(blob);
        }
    }
}

/// Item offsets and lengths far outside the text are clamped or
/// ignored.
#[test]
fn out_of_range_item_offsets_are_ignored() {
    // SAFETY: every pointer passed here is null, a live handle
    // created in this test, or a slice that outlives the call.
    unsafe {
        let buffer = hb_buffer_create();
        let text = b"abc";
        let p = text.as_ptr().cast::<c_char>();
        hb_buffer_add_utf8(buffer, p, 3, u32::MAX, c_int::MAX);
        hb_buffer_add_utf8(buffer, p, 3, 4, -1);
        hb_buffer_add_utf8(buffer, p, 3, 3, c_int::MAX);
        hb_buffer_add_utf8(buffer, p, 3, 1, c_int::MAX);
        hb_buffer_add_utf8(buffer, p, 3, 0, c_int::MIN);
        // Invalid UTF-8 and a split multi-byte sequence are dropped.
        let bad = [0xE2u8, 0x82, 0xAC, 0xFF];
        hb_buffer_add_utf8(buffer, bad.as_ptr().cast::<c_char>(), 4, 0, -1);
        hb_buffer_add_utf8(buffer, bad.as_ptr().cast::<c_char>(), 3, 1, -1);
        let units = [0x0041u16, 0xD800, 0x0042];
        hb_buffer_add_utf16(buffer, units.as_ptr(), 3, u32::MAX, c_int::MAX);
        hb_buffer_add_utf16(buffer, units.as_ptr(), 3, 0, -1);
        hb_buffer_add_utf16(buffer, units.as_ptr(), 3, 2, c_int::MAX);
        hb_buffer_destroy(buffer);
    }
}

/// Variation records with unknown tags, NaN, and infinity must not
/// disturb shaping.
#[test]
fn odd_variation_values_are_harmless() {
    // SAFETY: every pointer passed here is null, a live handle
    // created in this test, or a slice that outlives the call.
    unsafe {
        let blob = blob_from(OPEN_SANS);
        let face = hb_face_create(blob, 0);
        let font = hb_font_create(face);
        let vars = [
            hb_variation_t {
                tag: u32::from_be_bytes(*b"wght"),
                value: f32::NAN,
            },
            hb_variation_t {
                tag: u32::from_be_bytes(*b"wdth"),
                value: f32::INFINITY,
            },
            hb_variation_t {
                tag: 0,
                value: -1.0e30,
            },
        ];
        hb_font_set_variations(font, vars.as_ptr(), vars.len() as c_uint);
        hb_font_set_scale(font, i32::MIN, i32::MAX);
        let buffer = hb_buffer_create();
        hb_buffer_add_utf8(buffer, c"Hi".as_ptr(), -1, 0, -1);
        hb_buffer_guess_segment_properties(buffer);
        hb_shape(font, buffer, ptr::null(), 0);
        hb_buffer_destroy(buffer);
        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}

/// A language tag given with an explicit length that runs past a NUL
/// byte used to intern the empty string under a key that never
/// matched, leaking a fresh allocation on every call and returning a
/// different pointer each time. The tag now ends at the NUL, as it
/// does in C. Without `std` every tag maps to "und", so the test
/// needs the intern table.
#[cfg(feature = "std")]
#[test]
fn language_with_embedded_nul_interns_once() {
    // SAFETY: every pointer passed here is null, a live handle
    // created in this test, or a slice that outlives the call.
    unsafe {
        let raw = b"xq\0zz";
        let a = hb_language_from_string(raw.as_ptr().cast::<c_char>(), raw.len() as c_int);
        let b = hb_language_from_string(raw.as_ptr().cast::<c_char>(), raw.len() as c_int);
        let plain = hb_language_from_string(c"xq".as_ptr(), -1);
        assert!(!a.is_null());
        assert_eq!(a, b);
        assert_eq!(a, plain);
        assert_eq!(core::ffi::CStr::from_ptr(a).to_bytes(), b"xq");
        // A tag that is empty before its first NUL is no tag at all.
        let lead = b"\0en";
        assert!(
            hb_language_from_string(lead.as_ptr().cast::<c_char>(), lead.len() as c_int).is_null()
        );
    }
}

/// Tags shorter than four bytes are padded with spaces and longer
/// ones are cut.
#[test]
fn tag_from_string_pads_and_truncates() {
    // SAFETY: every pointer passed here is null, a live handle
    // created in this test, or a slice that outlives the call.
    unsafe {
        assert_eq!(
            hb_tag_from_string(c"ab".as_ptr(), -1),
            u32::from_be_bytes(*b"ab  ")
        );
        assert_eq!(
            hb_tag_from_string(c"abcdef".as_ptr(), 6),
            u32::from_be_bytes(*b"abcd")
        );
        assert_eq!(
            hb_tag_from_string(c"".as_ptr(), 0),
            u32::from_be_bytes(*b"    ")
        );
        assert_eq!(
            hb_direction_from_string(c"".as_ptr(), -1),
            HB_DIRECTION_INVALID
        );
    }
}

/// Wraps a set pointer so worker threads can share it.
#[derive(Clone, Copy)]
struct SharedSetPtr(*mut hb_set_t);
// SAFETY: `hb_set_t` serializes every access with its own lock, so
// the raw handle may be used from any thread while it is alive.
unsafe impl Send for SharedSetPtr {}

/// One `hb_set_t` read and written from several threads at once. The
/// set used to sit behind a `RefCell`, whose unsynchronized borrow
/// flag made this a data race.
#[test]
fn set_is_safe_to_share_between_threads() {
    let set = SharedSetPtr(hb_set_create());
    let handles: Vec<_> = (0..4u32)
        .map(|t| {
            std::thread::spawn(move || {
                let s = set;
                for i in 0..2000u32 {
                    // SAFETY: every pointer passed here is null, a live handle
                    // created in this test, or a slice that outlives the call.
                    unsafe {
                        hb_set_add(s.0, t * 10_000 + i);
                        let _ = hb_set_has(s.0, i);
                        let _ = hb_set_get_population(s.0);
                        let mut cp = u32::MAX;
                        let _ = hb_set_next(s.0, &mut cp);
                        if i % 2 == 1 {
                            hb_set_del(s.0, t * 10_000 + i);
                        }
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("worker thread");
    }
    // SAFETY: every pointer passed here is null, a live handle
    // created in this test, or a slice that outlives the call.
    unsafe {
        assert_eq!(hb_set_get_population(set.0), 4 * 1000);
        let mut cp = u32::MAX;
        assert_eq!(hb_set_next(set.0, &mut cp), 1);
        assert_eq!(cp, 0);
        hb_set_destroy(set.0);
    }
}

/// Collecting unicodes from a face with no cmap leaves the set empty.
#[test]
fn collect_unicodes_on_empty_face_is_empty() {
    // SAFETY: every pointer passed here is null, a live handle
    // created in this test, or a slice that outlives the call.
    unsafe {
        let blob = blob_from(&[0u8; 12]);
        let face = hb_face_create(blob, 0);
        let set = hb_set_create();
        hb_face_collect_unicodes(face, set);
        assert_eq!(hb_set_get_population(set), 0);
        hb_set_destroy(set);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}

#[cfg(feature = "paint")]
mod paint {
    use super::*;
    use core::ffi::c_void;
    use std::sync::atomic::{AtomicU32, Ordering};

    use sigilbuzz_capi::paint_bridge::{
        hb_color_t, hb_font_paint_glyph, hb_paint_funcs_create, hb_paint_funcs_destroy,
        hb_paint_funcs_set_color_func, hb_paint_funcs_set_push_clip_glyph_func, hb_paint_funcs_t,
    };

    fn build_face_bytes(colr: &[u8], cpal: &[u8]) -> Vec<u8> {
        let dir_len = 12 + 2 * 16;
        let cpal_off = dir_len;
        let colr_off = cpal_off + cpal.len();
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(b"COLR");
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(colr_off as u32).to_be_bytes());
        out.extend_from_slice(&(colr.len() as u32).to_be_bytes());
        out.extend_from_slice(b"CPAL");
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(cpal_off as u32).to_be_bytes());
        out.extend_from_slice(&(cpal.len() as u32).to_be_bytes());
        out.extend_from_slice(cpal);
        out.extend_from_slice(colr);
        out
    }

    /// CPAL v0 with one palette holding a single opaque red entry.
    fn build_cpal_red() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0u16.to_be_bytes()); // version
        out.extend_from_slice(&1u16.to_be_bytes()); // numPaletteEntries
        out.extend_from_slice(&1u16.to_be_bytes()); // numPalettes
        out.extend_from_slice(&1u16.to_be_bytes()); // numColorRecords
        out.extend_from_slice(&14u32.to_be_bytes()); // colorRecordsArrayOffset
        out.extend_from_slice(&0u16.to_be_bytes()); // colorRecordIndices[0]
        out.extend_from_slice(&[0, 0, 255, 255]); // BGRA red
        out
    }

    /// COLRv1 with one base glyph whose paint is
    /// PaintGlyph(42) -> PaintSolid(palette 0).
    fn build_colr(base_glyph: u16) -> Vec<u8> {
        let header_len: u32 = 30;
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // version
        out.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords
        out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphRecordsOffset
        out.extend_from_slice(&header_len.to_be_bytes()); // layerRecordsOffset
        out.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
        out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphListOffset
        out.extend_from_slice(&[0; 12]); // layerList, clipList, varIndexMap
        out.extend_from_slice(&1u32.to_be_bytes()); // BaseGlyphList count
        out.extend_from_slice(&base_glyph.to_be_bytes());
        out.extend_from_slice(&10u32.to_be_bytes()); // paint offset
        out.push(10); // PaintGlyph
        out.extend_from_slice(&[0, 0, 6]); // Offset24 to the PaintSolid
        out.extend_from_slice(&42u16.to_be_bytes());
        out.push(2); // PaintSolid
        out.extend_from_slice(&0u16.to_be_bytes()); // palette index
        out.extend_from_slice(&0x4000u16.to_be_bytes()); // alpha 1.0
        out
    }

    extern "C" fn count_color(
        _funcs: *mut hb_paint_funcs_t,
        data: *mut c_void,
        _is_foreground: i32,
        _color: hb_color_t,
    ) {
        // SAFETY: the test passes a pointer to a live AtomicU32.
        let counter = unsafe { &*data.cast::<AtomicU32>() };
        counter.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn count_clip(_funcs: *mut hb_paint_funcs_t, data: *mut c_void, _gid: u32) {
        // SAFETY: the test passes a pointer to a live AtomicU32.
        let counter = unsafe { &*data.cast::<AtomicU32>() };
        counter.fetch_add(1, Ordering::SeqCst);
    }

    fn callbacks_for(gid: u32) -> u32 {
        let bytes = build_face_bytes(&build_colr(7), &build_cpal_red());
        let counter = AtomicU32::new(0);
        // SAFETY: every pointer passed here is null, a live handle
        // created in this test, or a slice that outlives the call.
        unsafe {
            let blob = blob_from(&bytes);
            let face = hb_face_create(blob, 0);
            let font = hb_font_create(face);
            let funcs = hb_paint_funcs_create();
            hb_paint_funcs_set_color_func(funcs, Some(count_color));
            hb_paint_funcs_set_push_clip_glyph_func(funcs, Some(count_clip));
            let data = core::ptr::from_ref(&counter).cast_mut().cast::<c_void>();
            hb_font_paint_glyph(font, gid, funcs, data, 0, 0);
            hb_paint_funcs_destroy(funcs);
            hb_font_destroy(font);
            hb_face_destroy(face);
            hb_blob_destroy(blob);
        }
        counter.load(Ordering::SeqCst)
    }

    /// The fixture paints glyph 7. Asking for 7 + 65536 used to be
    /// truncated to 7 and painted it. Glyph ids above 65535 do not
    /// exist, so nothing may be painted.
    #[test]
    fn paint_glyph_above_u16_range_paints_nothing() {
        assert_eq!(callbacks_for(7), 2, "the fixture itself must paint");
        assert_eq!(callbacks_for(7 + 0x1_0000), 0);
        assert_eq!(callbacks_for(u32::MAX), 0);
    }

    /// A glyph with no COLR record and a font built from junk paint
    /// nothing.
    #[test]
    fn paint_glyph_on_hostile_faces_is_quiet() {
        assert_eq!(callbacks_for(8), 0);
        let counter = AtomicU32::new(0);
        // SAFETY: every pointer passed here is null, a live handle
        // created in this test, or a slice that outlives the call.
        unsafe {
            let blob = blob_from(&[0xAB; 40]);
            let face = hb_face_create(blob, 0);
            let font = hb_font_create(face);
            let funcs = hb_paint_funcs_create();
            hb_paint_funcs_set_color_func(funcs, Some(count_color));
            let data = core::ptr::from_ref(&counter).cast_mut().cast::<c_void>();
            hb_font_paint_glyph(font, 0, funcs, data, 0, 0);
            hb_font_paint_glyph(ptr::null_mut(), 0, funcs, data, 0, 0);
            hb_font_paint_glyph(font, 0, ptr::null_mut(), data, 0, 0);
            hb_paint_funcs_destroy(funcs);
            hb_font_destroy(font);
            hb_face_destroy(face);
            hb_blob_destroy(blob);
        }
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }
}

#[cfg(feature = "subset")]
mod subset {
    use super::*;
    use sigilbuzz_capi::subset_bridge::{
        hb_subset_input_create, hb_subset_input_destroy, hb_subset_input_glyph_set,
        hb_subset_input_unicode_set, hb_subset_or_fail,
    };

    /// Glyph ids above 65535 and codepoints that are not scalar values
    /// are skipped. A face that cannot be subset returns NULL.
    #[test]
    fn subset_ignores_out_of_range_ids() {
        // SAFETY: every pointer passed here is null, a live handle
        // created in this test, or a slice that outlives the call.
        unsafe {
            let blob = blob_from(OPEN_SANS);
            let face = hb_face_create(blob, 0);
            let input = hb_subset_input_create();
            let glyphs = hb_subset_input_glyph_set(input);
            hb_set_add(glyphs, 0x1_0000);
            hb_set_add(glyphs, u32::MAX);
            hb_set_destroy(glyphs);
            let unicodes = hb_subset_input_unicode_set(input);
            hb_set_add(unicodes, 0xD800);
            hb_set_add(unicodes, 0x11_0000);
            hb_set_add(unicodes, u32::from(b'A'));
            hb_set_destroy(unicodes);
            let out = hb_subset_or_fail(face, input);
            assert!(!out.is_null());
            // .notdef + A.
            assert_eq!(hb_face_get_glyph_count(out), 2);
            hb_face_destroy(out);

            let junk_blob = blob_from(&[0x5A; 64]);
            let junk_face = hb_face_create(junk_blob, 0);
            let junk_out = hb_subset_or_fail(junk_face, input);
            if !junk_out.is_null() {
                hb_face_destroy(junk_out);
            }
            hb_face_destroy(junk_face);
            hb_blob_destroy(junk_blob);

            hb_subset_input_destroy(input);
            hb_face_destroy(face);
            hb_blob_destroy(blob);
        }
    }
}
