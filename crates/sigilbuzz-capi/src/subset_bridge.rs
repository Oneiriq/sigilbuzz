//! `hb_subset_*`: bridge from HarfBuzz's subsetter surface to
//! `sigilbuzz_subset::subset()`.
//!
//! HarfBuzz's subset API hangs off two opaque types:
//! `hb_subset_input_t` (the "what to keep" descriptor: unicode set,
//! glyph set, drop-tables list, ...) and the result, an `hb_face_t`.
//! sigilbuzz's subsetter wants a flat `SubsetInput { gids: Vec<u16> }`,
//! so the bridge has to (a) own the inputs as opaque sets and (b)
//! translate the unicode set through cmap to gids before calling
//! through.
//!
//! Refcount semantics match HarfBuzz: `hb_subset_input_create` -> 1,
//! `hb_subset_input_destroy` decrements.
//!
//! `hb_subset_input_unicode_set` / `hb_subset_input_glyph_set` return
//! a fresh `hb_set_t` handle that observes the input's internal set
//! and the C caller then owns one reference and must destroy it. This
//! mirrors HarfBuzz's contract: "the returned set is shared; mutating
//! it mutates the input".

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr;

use crate::set::{hb_set_t, SharedSet};
use crate::spin_mutex::SpinMutex;
use crate::{hb_face_t, FaceInner};
use sigilbuzz::Face;
use sigilbuzz_subset::{subset, SubsetInput};

/// Shared payload behind `hb_subset_input_t`. Two `hb_set_t` payloads
/// (the unicode set and the glyph set) live behind their own Arcs so
/// the C caller's `_set()` accessors hand out shared handles cleanly.
struct SubsetInputInner {
    unicode_set: SharedSet,
    glyph_set: SharedSet,
}

/// Opaque subset-input handle.
#[repr(C)]
pub struct hb_subset_input_t {
    inner: Arc<SubsetInputInner>,
}

/// Allocates a fresh subset-input. Both internal sets start empty.
#[no_mangle]
pub extern "C" fn hb_subset_input_create() -> *mut hb_subset_input_t {
    let inner = Arc::new(SubsetInputInner {
        unicode_set: Arc::new(SpinMutex::new(BTreeSet::new())),
        glyph_set: Arc::new(SpinMutex::new(BTreeSet::new())),
    });
    Box::into_raw(Box::new(hb_subset_input_t { inner }))
}

/// Releases one reference to the subset-input.
///
/// # Safety
/// `input` must be null or a pointer originally returned by
/// `hb_subset_input_create`.
#[no_mangle]
pub unsafe extern "C" fn hb_subset_input_destroy(input: *mut hb_subset_input_t) {
    if input.is_null() {
        return;
    }
    // SAFETY: `input` is non-null and the caller guarantees it came
    // from `Box::into_raw` in `hb_subset_input_create` and has not
    // been destroyed yet.
    drop(unsafe { Box::from_raw(input) });
}

/// Returns a fresh `hb_set_t` handle observing the input's unicode
/// set. The caller owns the returned reference and must destroy it.
/// Mutating the returned set mutates the input.
///
/// # Safety
/// `input` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_subset_input_unicode_set(
    input: *mut hb_subset_input_t,
) -> *mut hb_set_t {
    if input.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `input` is non-null and the caller guarantees it points
    // to a live `hb_subset_input_t`. The explicit `as_ref` keeps the
    // reference through the raw pointer visible to the autoref lint.
    let arc = unsafe { (*(input)).inner.as_ref().unicode_set.clone() };
    Box::into_raw(Box::new(hb_set_t::from_arc(arc)))
}

/// Returns a fresh `hb_set_t` handle observing the input's glyph set.
/// Mutating the returned set mutates the input.
///
/// # Safety
/// `input` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_subset_input_glyph_set(input: *mut hb_subset_input_t) -> *mut hb_set_t {
    if input.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `input` is non-null and the caller guarantees it points
    // to a live `hb_subset_input_t`.
    let arc = unsafe { (*(input)).inner.as_ref().glyph_set.clone() };
    Box::into_raw(Box::new(hb_set_t::from_arc(arc)))
}

/// Subsets `face` according to `input`. Returns a fresh `hb_face_t`
/// (refcount 1) on success, or NULL on failure.
///
/// Flow:
/// 1. Pull the unicode set; map each codepoint through the source
///    face's cmap to gids; union with the glyph set.
/// 2. Build a [`SubsetInput`] with `gids = union`, defaults
///    elsewhere.
/// 3. Call [`sigilbuzz_subset::subset`].
/// 4. Parse the resulting bytes into a fresh `hb_face_t` that owns
///    them.
///
/// # Safety
/// `face` and `input` must each be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_subset_or_fail(
    face: *mut hb_face_t,
    input: *mut hb_subset_input_t,
) -> *mut hb_face_t {
    if face.is_null() || input.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `face` is non-null and the caller guarantees it points
    // to a live `hb_face_t`.
    let face_inner: &FaceInner = unsafe { (*face).inner.as_ref() };
    // SAFETY: `input` is non-null and the caller guarantees it points
    // to a live `hb_subset_input_t`.
    let input_inner = unsafe { (*(input)).inner.clone() };

    // Step 1: union gid set.
    let mut gids: BTreeSet<u16> = BTreeSet::new();
    // Always retain .notdef.
    gids.insert(0);

    // Translate unicode codepoints to gids.
    if let Ok(cmap) = face_inner.face.cmap() {
        let unicode = input_inner.unicode_set.lock();
        for &cp in unicode.iter() {
            if let Some(c) = char::from_u32(cp) {
                if let Some(gid) = cmap.glyph_id(c) {
                    gids.insert(gid);
                }
            }
        }
    }

    // Add raw gids the caller pushed into the glyph set.
    // sigilbuzz_subset::SubsetInput rejects out-of-range gids. We
    // silently skip the ones that do not fit in u16 so the FFI
    // surface doesn't expose internal validation errors.
    {
        let glyph_set = input_inner.glyph_set.lock();
        gids.extend(glyph_set.iter().filter_map(|&g| u16::try_from(g).ok()));
    }

    let gid_vec: Vec<u16> = gids.into_iter().collect();
    let subset_input = SubsetInput {
        gids: gid_vec,
        ..SubsetInput::default()
    };

    // Step 2: drive sigilbuzz_subset.
    let out = match subset(&face_inner.face, &subset_input) {
        Ok(o) => o,
        Err(_) => return ptr::null_mut(),
    };

    // Step 3: parse the output bytes into a face that owns them
    // through an Arc clone.
    let bytes_arc: Arc<Vec<u8>> = Arc::new(out.bytes);
    let bytes_slice: &[u8] = bytes_arc.as_slice();
    let parsed = match Face::parse_bytes(bytes_slice, 0) {
        Ok(f) => f,
        Err(_) => return ptr::null_mut(),
    };
    // SAFETY: `parsed` borrows the heap buffer behind `bytes_arc`.
    // The FaceInner built below owns that Arc, so the buffer does not
    // move or drop while the face exists. See the `FaceInner` note in
    // the crate root.
    let face_static: Face<'static> =
        unsafe { core::mem::transmute::<Face<'_>, Face<'static>>(parsed) };

    let inner = Arc::new(FaceInner::from_arc(bytes_arc, face_static));
    Box::into_raw(Box::new(hb_face_t::from_inner(inner)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::set::{hb_set_add, hb_set_destroy};
    use crate::{
        hb_blob_create, hb_blob_destroy, hb_face_create, hb_face_destroy, hb_face_get_glyph_count,
        HB_MEMORY_MODE_READONLY,
    };
    use core::ffi::{c_char, c_uint};

    const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

    #[test]
    fn subset_open_sans_to_abc_via_unicode_set() {
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
            assert!(!face.is_null());
            let original_count = hb_face_get_glyph_count(face);
            assert!(original_count > 100);

            let input = hb_subset_input_create();
            let unicode = hb_subset_input_unicode_set(input);
            hb_set_add(unicode, b'A' as u32);
            hb_set_add(unicode, b'B' as u32);
            hb_set_add(unicode, b'C' as u32);
            hb_set_destroy(unicode);

            let subset_face = hb_subset_or_fail(face, input);
            assert!(!subset_face.is_null(), "subset failed");
            let new_count = hb_face_get_glyph_count(subset_face);
            // .notdef + A + B + C = 4 minimum. Closure walk may pull
            // in composites but Open Sans's A/B/C are simple glyphs,
            // so the count should be exactly 4.
            assert_eq!(
                new_count, 4,
                "expected 4 glyphs (.notdef + A + B + C), got {new_count}",
            );

            hb_face_destroy(subset_face);
            hb_subset_input_destroy(input);
            hb_face_destroy(face);
            hb_blob_destroy(blob);
        }
    }

    #[test]
    fn subset_input_unicode_set_is_shared() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            let input = hb_subset_input_create();
            let a = hb_subset_input_unicode_set(input);
            let b = hb_subset_input_unicode_set(input);
            hb_set_add(a, 65);
            assert_eq!(crate::set::hb_set_has(b, 65), 1);
            hb_set_destroy(a);
            hb_set_destroy(b);
            hb_subset_input_destroy(input);
        }
    }

    #[test]
    fn subset_or_fail_null_inputs_return_null() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            assert!(hb_subset_or_fail(ptr::null_mut(), ptr::null_mut()).is_null());
        }
    }
}
