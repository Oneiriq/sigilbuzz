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
//! # Ownership
//!
//! The rules are HarfBuzz's:
//!
//! - `hb_subset_input_create` / `hb_subset_input_create_or_fail`
//!   return one reference; `hb_subset_input_reference` adds one and
//!   returns the same pointer; `hb_subset_input_destroy` drops one.
//! - `hb_subset_input_unicode_set` / `hb_subset_input_glyph_set` return
//!   a set *owned by the input*. Every call returns the same pointer,
//!   valid until the input is destroyed. The caller must not destroy
//!   it. Adding to or removing from it changes what `hb_subset_or_fail`
//!   keeps. To keep the set past the input's lifetime, take a reference
//!   with `hb_set_reference` (and destroy that reference later).
//! - `hb_subset_or_fail` returns a new face reference (or null), which
//!   the caller destroys.

extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr;

use crate::set::hb_set_t;
use crate::{face_from_blob, handle, hb_blob_t, hb_face_t, BlobInner, FaceInner};
use sigilbuzz_subset::{subset, SubsetInput};

/// Subset-input descriptor. C holds the `Arc` pointer to this struct;
/// see the `handle` module. The two sets are ordinary refcounted
/// `hb_set_t` objects the input holds one reference to each.
pub struct hb_subset_input_t {
    unicode_set: Arc<hb_set_t>,
    glyph_set: Arc<hb_set_t>,
}

/// Allocates a fresh subset-input with refcount 1. Both internal sets
/// start empty.
#[no_mangle]
pub extern "C" fn hb_subset_input_create() -> *mut hb_subset_input_t {
    handle::into_raw(hb_subset_input_t {
        unicode_set: Arc::new(hb_set_t::new()),
        glyph_set: Arc::new(hb_set_t::new()),
    })
}

/// HarfBuzz's name for [`hb_subset_input_create`]. HarfBuzz returns
/// null when allocation fails; sigilbuzz aborts on allocation failure
/// like the rest of Rust, so this never returns null.
#[no_mangle]
pub extern "C" fn hb_subset_input_create_or_fail() -> *mut hb_subset_input_t {
    hb_subset_input_create()
}

/// Adds one reference to `input` and returns `input` itself. Null in,
/// null out.
///
/// # Safety
/// `input` must be null or a live subset input.
#[no_mangle]
pub unsafe extern "C" fn hb_subset_input_reference(
    input: *mut hb_subset_input_t,
) -> *mut hb_subset_input_t {
    // SAFETY: caller guarantees `input` is null or a live handle.
    unsafe { handle::reference(input) }
}

/// Releases one reference to the subset input. The last release also
/// releases the input's references to its unicode and glyph sets.
/// Null is a no-op.
///
/// # Safety
/// `input` must be null or a live subset input the caller holds a
/// reference to.
#[no_mangle]
pub unsafe extern "C" fn hb_subset_input_destroy(input: *mut hb_subset_input_t) {
    // SAFETY: caller guarantees `input` is null or a live handle it
    // owns a reference to.
    unsafe { handle::destroy(input) };
}

/// Returns the input's unicode set: the codepoints to keep.
///
/// The set is owned by the input (HarfBuzz's "transfer none"): every
/// call returns the same pointer, valid until the input is destroyed,
/// and the caller must not destroy it. Mutating it mutates the input.
/// Returns null for a null input (HarfBuzz does not check).
///
/// # Safety
/// `input` must be null or a live subset input.
#[no_mangle]
pub unsafe extern "C" fn hb_subset_input_unicode_set(
    input: *mut hb_subset_input_t,
) -> *mut hb_set_t {
    if input.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `input` is non-null and the caller guarantees it points
    // to a live `hb_subset_input_t`.
    let input = unsafe { &*input };
    Arc::as_ptr(&input.unicode_set).cast_mut()
}

/// Returns the input's glyph set: raw glyph ids to keep.
///
/// Same ownership as [`hb_subset_input_unicode_set`]: owned by the
/// input, same pointer every call, never destroyed by the caller.
///
/// # Safety
/// `input` must be null or a live subset input.
#[no_mangle]
pub unsafe extern "C" fn hb_subset_input_glyph_set(input: *mut hb_subset_input_t) -> *mut hb_set_t {
    if input.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `input` is non-null and the caller guarantees it points
    // to a live `hb_subset_input_t`.
    let input = unsafe { &*input };
    Arc::as_ptr(&input.glyph_set).cast_mut()
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
/// 4. Wrap the resulting bytes in a fresh blob and a face that
///    references it.
///
/// # Safety
/// `face` and `input` must be null or live objects.
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
    let face_inner: &FaceInner = unsafe { &(*face).inner };
    // SAFETY: `input` is non-null and the caller guarantees it points
    // to a live `hb_subset_input_t`.
    let input = unsafe { &*input };

    // Step 1: union gid set.
    let mut gids: BTreeSet<u16> = BTreeSet::new();
    // Always retain .notdef.
    gids.insert(0);

    // Translate unicode codepoints to gids.
    if let Ok(cmap) = face_inner.face.cmap() {
        input.unicode_set.with_inner(|unicode| {
            for &cp in unicode {
                if let Some(gid) = char::from_u32(cp).and_then(|c| cmap.glyph_id(c)) {
                    gids.insert(gid);
                }
            }
        });
    }

    // Add raw gids the caller pushed into the glyph set.
    // sigilbuzz_subset::SubsetInput rejects out-of-range gids. We
    // silently skip the ones that do not fit in u16 so the FFI
    // surface doesn't expose internal validation errors.
    input.glyph_set.with_inner(|glyph_set| {
        gids.extend(glyph_set.iter().filter_map(|&g| u16::try_from(g).ok()));
    });

    let gid_vec: Vec<u16> = gids.into_iter().collect();
    let subset_input = SubsetInput {
        gids: gid_vec,
        ..SubsetInput::default()
    };

    // Step 2: drive sigilbuzz_subset.
    let Ok(out) = subset(&face_inner.face, &subset_input) else {
        return ptr::null_mut();
    };

    // Step 3: wrap output bytes in a fresh blob + face. The face holds
    // the only reference to the blob.
    let blob = Arc::new(hb_blob_t {
        inner: BlobInner::from_data(out.bytes),
    });
    match face_from_blob(blob, 0) {
        Some(subset_face) => handle::arc_into_raw(subset_face),
        None => ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::set::{hb_set_add, hb_set_get_population, hb_set_has};
    use crate::{
        hb_blob_create, hb_blob_destroy, hb_face_create, hb_face_destroy, hb_face_get_glyph_count,
        HB_MEMORY_MODE_READONLY,
    };
    use core::ffi::{c_char, c_uint};

    const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

    unsafe fn open_sans_face() -> *mut hb_face_t {
        // SAFETY: OPEN_SANS is a static byte slice of the given length.
        let blob = unsafe {
            hb_blob_create(
                OPEN_SANS.as_ptr().cast::<c_char>(),
                OPEN_SANS.len() as c_uint,
                HB_MEMORY_MODE_READONLY,
                ptr::null_mut(),
                None,
            )
        };
        // SAFETY: `blob` was just created; the face keeps its own
        // reference, so ours can go right away.
        unsafe {
            let face = hb_face_create(blob, 0);
            hb_blob_destroy(blob);
            face
        }
    }

    #[test]
    fn subset_open_sans_to_abc_via_unicode_set() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each reference is released once.
        unsafe {
            let face = open_sans_face();
            assert!(!face.is_null());
            let original_count = hb_face_get_glyph_count(face);
            assert!(original_count > 100);

            let input = hb_subset_input_create();
            let unicode = hb_subset_input_unicode_set(input);
            hb_set_add(unicode, b'A' as u32);
            hb_set_add(unicode, b'B' as u32);
            hb_set_add(unicode, b'C' as u32);
            // The set belongs to the input: no hb_set_destroy here.

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
        }
    }

    #[test]
    fn subset_input_sets_are_stable_and_shared() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each reference is released once.
        unsafe {
            let input = hb_subset_input_create();
            let a = hb_subset_input_unicode_set(input);
            let b = hb_subset_input_unicode_set(input);
            assert_eq!(a, b, "same set on every call");
            hb_set_add(a, 65);
            assert_eq!(hb_set_has(b, 65), 1);
            let g1 = hb_subset_input_glyph_set(input);
            let g2 = hb_subset_input_glyph_set(input);
            assert_eq!(g1, g2);
            assert_ne!(a, g1, "unicode and glyph sets are distinct");
            assert_eq!(hb_set_get_population(g1), 0);
            hb_subset_input_destroy(input);
        }
    }

    #[test]
    fn glyph_set_mutations_reach_subset() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each reference is released once.
        unsafe {
            let face = open_sans_face();
            let input = hb_subset_input_create();
            // Glyph ids 1..=3 plus an id too large for u16, which the
            // bridge skips.
            let glyphs = hb_subset_input_glyph_set(input);
            for g in [1, 2, 3, 70_000] {
                hb_set_add(glyphs, g);
            }
            let subset_face = hb_subset_or_fail(face, input);
            assert!(!subset_face.is_null());
            assert_eq!(hb_face_get_glyph_count(subset_face), 4);
            hb_face_destroy(subset_face);
            hb_subset_input_destroy(input);
            hb_face_destroy(face);
        }
    }

    #[test]
    fn null_inputs() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each reference is released once.
        unsafe {
            assert!(hb_subset_or_fail(ptr::null_mut(), ptr::null_mut()).is_null());
            assert!(hb_subset_input_unicode_set(ptr::null_mut()).is_null());
            assert!(hb_subset_input_glyph_set(ptr::null_mut()).is_null());
            assert!(hb_subset_input_reference(ptr::null_mut()).is_null());
            hb_subset_input_destroy(ptr::null_mut());
        }
    }

    #[test]
    fn create_or_fail_is_an_ordinary_input() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each reference is released once.
        unsafe {
            let input = hb_subset_input_create_or_fail();
            assert!(!input.is_null());
            assert!(!hb_subset_input_unicode_set(input).is_null());
            hb_subset_input_destroy(input);
        }
    }
}
