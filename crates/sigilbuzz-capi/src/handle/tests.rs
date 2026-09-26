//! Ownership tests: HarfBuzz-style reference / destroy sequences must
//! neither leak nor double-free.
//!
//! Leaks are observed two ways:
//!
//! - A [`Weak`] taken on a handle while it is alive. After the last
//!   `*_destroy`, `Weak::strong_count` must be 0, which proves the
//!   object was dropped. The `Weak` keeps the allocation (not the
//!   object) alive, so checking it afterwards is not a use-after-free.
//! - The blob destroy callback, which HarfBuzz fires exactly once when
//!   the last reference to the blob goes. Faces and fonts hold their
//!   blob, so the callback also proves those were released.

use alloc::sync::{Arc, Weak};
use core::ffi::{c_char, c_uint, c_void};
use core::mem::ManuallyDrop;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::set::{hb_set_add, hb_set_create, hb_set_destroy, hb_set_has, hb_set_reference};
use crate::*;

const OPEN_SANS: &[u8] = include_bytes!("../../../../tests/fixtures/opensans_regular.ttf");

/// Takes a `Weak` observer on a live handle without touching the
/// caller's reference.
///
/// # Safety
/// `ptr` must be a live handle created through this module.
unsafe fn observe<T>(ptr: *const T) -> Weak<T> {
    // SAFETY: the caller passes a live `Arc::into_raw` pointer. The
    // rebuilt `Arc` is never dropped, so the caller's reference stays
    // intact.
    let arc = ManuallyDrop::new(unsafe { Arc::from_raw(ptr) });
    Arc::downgrade(&arc)
}

/// Blob destroy callback that bumps the `AtomicUsize` `user_data`
/// points at.
///
/// # Safety
/// `user_data` must point at a live `AtomicUsize`.
unsafe extern "C" fn count_destroy(user_data: *mut c_void) {
    // SAFETY: every test passes a pointer to a counter that outlives
    // the blob.
    let counter = unsafe { &*user_data.cast::<AtomicUsize>().cast_const() };
    counter.fetch_add(1, Ordering::SeqCst);
}

fn counter_ptr(counter: &AtomicUsize) -> *mut c_void {
    ptr::from_ref(counter).cast_mut().cast::<c_void>()
}

/// Open Sans blob whose destroy callback bumps `counter`.
fn open_sans_blob(counter: &AtomicUsize) -> *mut hb_blob_t {
    // SAFETY: OPEN_SANS is a static slice of the given length and the
    // callback contract is met by `counter`.
    unsafe {
        hb_blob_create(
            OPEN_SANS.as_ptr().cast::<c_char>(),
            OPEN_SANS.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            counter_ptr(counter),
            Some(count_destroy),
        )
    }
}

fn fired(counter: &AtomicUsize) -> usize {
    counter.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// Blob
// ---------------------------------------------------------------------------

#[test]
fn blob_reference_is_identity_and_double_destroy_frees_once() {
    let counter = AtomicUsize::new(0);
    let blob = open_sans_blob(&counter);
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        let weak = observe(blob);
        assert_eq!(weak.strong_count(), 1);
        assert_eq!(
            hb_blob_reference(blob),
            blob,
            "reference returns the same pointer"
        );
        assert_eq!(weak.strong_count(), 2);
        hb_blob_destroy(blob);
        assert_eq!(fired(&counter), 0, "one reference is still out");
        assert_eq!(hb_blob_get_length(blob) as usize, OPEN_SANS.len());
        hb_blob_destroy(blob);
        assert_eq!(fired(&counter), 1, "destroy callback fires exactly once");
        assert_eq!(weak.strong_count(), 0, "blob freed");
    }
}

#[test]
fn blob_destroy_callback_timing_follows_harfbuzz() {
    let bytes = [1u8, 2, 3, 4];
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        // Zero length: nothing to keep, callback fires right away.
        let c0 = AtomicUsize::new(0);
        let empty = hb_blob_create(
            bytes.as_ptr().cast::<c_char>(),
            0,
            HB_MEMORY_MODE_READONLY,
            counter_ptr(&c0),
            Some(count_destroy),
        );
        assert_eq!(fired(&c0), 1);
        assert_eq!(hb_blob_get_length(empty), 0);
        hb_blob_destroy(empty);
        assert_eq!(fired(&c0), 1, "not fired a second time");

        // Null data behaves like zero length.
        let c1 = AtomicUsize::new(0);
        let null_blob = hb_blob_create(
            ptr::null(),
            4,
            HB_MEMORY_MODE_READONLY,
            counter_ptr(&c1),
            Some(count_destroy),
        );
        assert_eq!(fired(&c1), 1);
        hb_blob_destroy(null_blob);

        // DUPLICATE copies up front, so the callback fires right away.
        let c2 = AtomicUsize::new(0);
        let dup = hb_blob_create(
            bytes.as_ptr().cast::<c_char>(),
            4,
            HB_MEMORY_MODE_DUPLICATE,
            counter_ptr(&c2),
            Some(count_destroy),
        );
        assert_eq!(fired(&c2), 1);
        assert_eq!(hb_blob_get_length(dup), 4);
        hb_blob_destroy(dup);
        assert_eq!(fired(&c2), 1);

        // Any other mode defers to the last release.
        for mode in [
            HB_MEMORY_MODE_READONLY,
            HB_MEMORY_MODE_WRITABLE,
            HB_MEMORY_MODE_READONLY_MAY_MAKE_WRITABLE,
        ] {
            let c = AtomicUsize::new(0);
            let blob = hb_blob_create(
                bytes.as_ptr().cast::<c_char>(),
                4,
                mode,
                counter_ptr(&c),
                Some(count_destroy),
            );
            assert_eq!(fired(&c), 0, "mode {mode} defers");
            hb_blob_destroy(blob);
            assert_eq!(fired(&c), 1, "mode {mode} fires on release");
        }
    }
}

// ---------------------------------------------------------------------------
// Face and font
// ---------------------------------------------------------------------------

#[test]
fn face_references_its_blob_and_frees_everything() {
    let counter = AtomicUsize::new(0);
    let blob = open_sans_blob(&counter);
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        let blob_weak = observe(blob);
        let face = hb_face_create(blob, 0);
        assert_eq!(blob_weak.strong_count(), 2, "the face holds the blob");
        // Drop our blob reference right away, as HarfBuzz code does.
        hb_blob_destroy(blob);
        assert_eq!(fired(&counter), 0, "face keeps the blob alive");
        assert!(hb_face_get_upem(face) > 0);

        let face_weak = observe(face);
        assert_eq!(hb_face_reference(face), face);
        assert_eq!(face_weak.strong_count(), 2);
        hb_face_destroy(face);
        assert!(hb_face_get_glyph_count(face) > 100, "still alive");
        hb_face_destroy(face);
        assert_eq!(face_weak.strong_count(), 0, "face freed");
        assert_eq!(blob_weak.strong_count(), 0, "blob freed with the face");
        assert_eq!(fired(&counter), 1);
    }
}

#[test]
fn font_references_its_face_and_frees_everything() {
    let counter = AtomicUsize::new(0);
    let blob = open_sans_blob(&counter);
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        let face = hb_face_create(blob, 0);
        hb_blob_destroy(blob);
        let face_weak = observe(face);
        let font = hb_font_create(face);
        assert_eq!(face_weak.strong_count(), 2, "the font holds the face");
        hb_face_destroy(face);
        assert_eq!(face_weak.strong_count(), 1);

        let font_weak = observe(font);
        assert_eq!(hb_font_reference(font), font);
        assert_eq!(font_weak.strong_count(), 2);
        hb_font_destroy(font);

        // The surviving reference still works end to end.
        let mut x = 0;
        let mut y = 0;
        hb_font_get_scale(font, &mut x, &mut y);
        assert!(x > 0 && y > 0);
        let buffer = hb_buffer_create();
        hb_buffer_add_utf8(buffer, c"Hi".as_ptr(), -1, 0, -1);
        hb_buffer_guess_segment_properties(buffer);
        hb_shape(font, buffer, ptr::null(), 0);
        assert_eq!(hb_buffer_get_length(buffer), 2);
        hb_buffer_destroy(buffer);

        assert_eq!(fired(&counter), 0);
        hb_font_destroy(font);
        assert_eq!(font_weak.strong_count(), 0, "font freed");
        assert_eq!(face_weak.strong_count(), 0, "face freed with the font");
        assert_eq!(fired(&counter), 1, "blob freed with the face");
    }
}

#[test]
fn empty_objects_from_null_inputs_are_ordinary_references() {
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        let face = hb_face_create(ptr::null_mut(), 0);
        assert!(!face.is_null());
        let face_weak = observe(face);
        let font = hb_font_create(ptr::null_mut());
        assert!(!font.is_null());
        let font_weak = observe(font);
        assert_eq!(hb_face_reference(face), face);
        hb_face_destroy(face);
        hb_face_destroy(face);
        assert_eq!(face_weak.strong_count(), 0);
        hb_font_destroy(font);
        assert_eq!(font_weak.strong_count(), 0);

        // Bytes that do not parse still hand back a destroyable face.
        let junk = [0u8; 8];
        let blob = hb_blob_create(
            junk.as_ptr().cast::<c_char>(),
            junk.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            ptr::null_mut(),
            None,
        );
        let bad = hb_face_create(blob, 0);
        assert!(!bad.is_null());
        assert_eq!(hb_face_get_glyph_count(bad), 0);
        let bad_weak = observe(bad);
        hb_face_destroy(bad);
        hb_blob_destroy(blob);
        assert_eq!(bad_weak.strong_count(), 0);
    }
}

// ---------------------------------------------------------------------------
// Buffer and set
// ---------------------------------------------------------------------------

#[test]
fn buffer_reference_is_identity() {
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        let buffer = hb_buffer_create();
        let weak = observe(buffer);
        assert_eq!(hb_buffer_reference(buffer), buffer);
        assert_eq!(weak.strong_count(), 2);
        hb_buffer_destroy(buffer);
        // Mutations through the surviving reference still land.
        hb_buffer_add_utf8(buffer, c"abc".as_ptr(), -1, 0, -1);
        hb_buffer_destroy(buffer);
        assert_eq!(weak.strong_count(), 0, "buffer freed");
    }
}

#[test]
fn set_reference_is_identity() {
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        let set = hb_set_create();
        let weak = observe(set);
        assert_eq!(hb_set_reference(set), set);
        hb_set_add(set, 42);
        hb_set_destroy(set);
        assert_eq!(hb_set_has(set, 42), 1, "still alive");
        hb_set_destroy(set);
        assert_eq!(weak.strong_count(), 0, "set freed");
    }
}

#[test]
fn references_can_be_released_from_other_threads() {
    const THREADS: usize = 8;
    let set = hb_set_create();
    // SAFETY: `set` is live.
    let weak = unsafe { observe(set) };
    for _ in 0..THREADS {
        // SAFETY: `set` is live.
        assert_eq!(unsafe { hb_set_reference(set) }, set);
    }
    assert_eq!(weak.strong_count(), THREADS + 1);
    // Raw pointers are not `Send`; move the address instead.
    let addr = set as usize;
    let workers: Vec<_> = (0..THREADS)
        .map(|_| {
            std::thread::spawn(move || {
                // SAFETY: each thread releases exactly one of the
                // references taken above.
                unsafe { hb_set_destroy(addr as *mut crate::set::hb_set_t) };
            })
        })
        .collect();
    for w in workers {
        w.join().expect("worker panicked");
    }
    assert_eq!(weak.strong_count(), 1);
    // SAFETY: the creation reference is still ours.
    unsafe { hb_set_destroy(set) };
    assert_eq!(weak.strong_count(), 0);
}

#[test]
fn null_handles_match_harfbuzz() {
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        assert!(hb_blob_reference(ptr::null_mut()).is_null());
        assert!(hb_face_reference(ptr::null_mut()).is_null());
        assert!(hb_font_reference(ptr::null_mut()).is_null());
        assert!(hb_buffer_reference(ptr::null_mut()).is_null());
        assert!(hb_set_reference(ptr::null_mut()).is_null());
        hb_blob_destroy(ptr::null_mut());
        hb_face_destroy(ptr::null_mut());
        hb_font_destroy(ptr::null_mut());
        hb_buffer_destroy(ptr::null_mut());
        hb_set_destroy(ptr::null_mut());
    }
}

// ---------------------------------------------------------------------------
// Subset input
// ---------------------------------------------------------------------------

#[cfg(feature = "subset")]
mod subset {
    use super::*;
    use crate::set::hb_set_get_population;
    use crate::subset_bridge::{
        hb_subset_input_create, hb_subset_input_destroy, hb_subset_input_glyph_set,
        hb_subset_input_reference, hb_subset_input_unicode_set, hb_subset_or_fail,
    };

    #[test]
    fn input_reference_is_identity() {
        // SAFETY: every pointer passed here is null, a live handle created
        // in this test, or data that outlives the call.
        unsafe {
            let input = hb_subset_input_create();
            let weak = observe(input);
            assert_eq!(hb_subset_input_reference(input), input);
            assert_eq!(weak.strong_count(), 2);
            hb_subset_input_destroy(input);
            hb_subset_input_destroy(input);
            assert_eq!(weak.strong_count(), 0);
        }
    }

    #[test]
    fn accessor_sets_are_owned_by_the_input() {
        // SAFETY: every pointer passed here is null, a live handle created
        // in this test, or data that outlives the call.
        unsafe {
            let input = hb_subset_input_create();
            let unicode = hb_subset_input_unicode_set(input);
            let glyphs = hb_subset_input_glyph_set(input);
            let unicode_weak = observe(unicode);
            let glyphs_weak = observe(glyphs);
            for _ in 0..4 {
                assert_eq!(hb_subset_input_unicode_set(input), unicode);
                assert_eq!(hb_subset_input_glyph_set(input), glyphs);
            }
            // Asking for the sets hands out no references.
            assert_eq!(unicode_weak.strong_count(), 1);
            assert_eq!(glyphs_weak.strong_count(), 1);
            hb_set_add(unicode, 0x41);
            assert_eq!(hb_set_get_population(hb_subset_input_unicode_set(input)), 1);
            // Destroying the input releases both sets.
            hb_subset_input_destroy(input);
            assert_eq!(unicode_weak.strong_count(), 0);
            assert_eq!(glyphs_weak.strong_count(), 0);
        }
    }

    #[test]
    fn referenced_set_outlives_the_input() {
        // SAFETY: every pointer passed here is null, a live handle created
        // in this test, or data that outlives the call.
        unsafe {
            let input = hb_subset_input_create();
            let unicode = hb_set_reference(hb_subset_input_unicode_set(input));
            let weak = observe(unicode);
            hb_set_add(unicode, 7);
            hb_subset_input_destroy(input);
            assert_eq!(weak.strong_count(), 1, "our reference keeps it");
            assert_eq!(hb_set_has(unicode, 7), 1);
            hb_set_destroy(unicode);
            assert_eq!(weak.strong_count(), 0);
        }
    }

    #[test]
    fn harfbuzz_style_subset_sequence_does_not_leak() {
        let counter = AtomicUsize::new(0);
        let blob = open_sans_blob(&counter);
        // SAFETY: every pointer passed here is null, a live handle created
        // in this test, or data that outlives the call.
        unsafe {
            let face = hb_face_create(blob, 0);
            hb_blob_destroy(blob);
            let input = hb_subset_input_create();
            let input_weak = observe(input);
            let unicode = hb_subset_input_unicode_set(input);
            let set_weak = observe(unicode);
            for cp in [0x41, 0x42, 0x43] {
                hb_set_add(hb_subset_input_unicode_set(input), cp);
            }
            let subset_face = hb_subset_or_fail(face, input);
            assert!(!subset_face.is_null());
            assert_eq!(hb_face_get_glyph_count(subset_face), 4);
            let subset_weak = observe(subset_face);

            hb_subset_input_destroy(input);
            hb_face_destroy(subset_face);
            hb_face_destroy(face);
            assert_eq!(input_weak.strong_count(), 0, "input freed");
            assert_eq!(set_weak.strong_count(), 0, "its set freed");
            assert_eq!(subset_weak.strong_count(), 0, "subset face freed");
            assert_eq!(fired(&counter), 1, "source blob freed");
        }
    }

    #[test]
    fn null_subset_handles_match_harfbuzz() {
        // SAFETY: every pointer passed here is null, a live handle created
        // in this test, or data that outlives the call.
        unsafe {
            assert!(hb_subset_input_reference(ptr::null_mut()).is_null());
            hb_subset_input_destroy(ptr::null_mut());
        }
    }
}

// ---------------------------------------------------------------------------
// Paint funcs
// ---------------------------------------------------------------------------

#[cfg(feature = "paint")]
mod paint {
    use super::*;
    use crate::paint_bridge::{
        hb_paint_funcs_create, hb_paint_funcs_destroy, hb_paint_funcs_reference,
    };

    #[test]
    fn paint_funcs_reference_is_identity() {
        // SAFETY: every pointer passed here is null, a live handle created
        // in this test, or data that outlives the call.
        unsafe {
            let funcs = hb_paint_funcs_create();
            let weak = observe(funcs);
            assert_eq!(hb_paint_funcs_reference(funcs), funcs);
            assert_eq!(weak.strong_count(), 2);
            hb_paint_funcs_destroy(funcs);
            hb_paint_funcs_destroy(funcs);
            assert_eq!(weak.strong_count(), 0);
            assert!(hb_paint_funcs_reference(ptr::null_mut()).is_null());
            hb_paint_funcs_destroy(ptr::null_mut());
        }
    }
}
