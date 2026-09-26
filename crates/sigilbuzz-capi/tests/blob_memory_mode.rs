//! The blob memory mode and destroy callback behave as in HarfBuzz.

use core::ffi::{c_char, c_void};
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use sigilbuzz_capi::{
    hb_blob_create, hb_blob_destroy, hb_blob_get_length, hb_blob_reference,
    HB_MEMORY_MODE_DUPLICATE, HB_MEMORY_MODE_READONLY,
};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

/// Counts calls in the `AtomicU32` that `user_data` points at.
unsafe extern "C" fn count_destroy(user_data: *mut c_void) {
    // SAFETY: the tests pass a pointer to a live `AtomicU32`.
    unsafe { (*user_data.cast::<AtomicU32>()).fetch_add(1, Ordering::SeqCst) };
}

fn counter_ptr(counter: &AtomicU32) -> *mut c_void {
    ptr::from_ref(counter).cast_mut().cast::<c_void>()
}

/// In `HB_MEMORY_MODE_DUPLICATE` HarfBuzz copies the bytes and calls
/// `destroy` before `hb_blob_create` returns. The mode used to be
/// ignored, so the callback waited for the last release.
#[test]
fn duplicate_mode_calls_destroy_before_returning() {
    let calls = AtomicU32::new(0);
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        let blob = hb_blob_create(
            OPEN_SANS.as_ptr().cast::<c_char>(),
            16,
            HB_MEMORY_MODE_DUPLICATE,
            counter_ptr(&calls),
            Some(count_destroy),
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(hb_blob_get_length(blob), 16);
        hb_blob_destroy(blob);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Other modes keep `user_data` until the last reference goes away.
#[test]
fn readonly_mode_calls_destroy_on_last_release() {
    let calls = AtomicU32::new(0);
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        let blob = hb_blob_create(
            OPEN_SANS.as_ptr().cast::<c_char>(),
            16,
            HB_MEMORY_MODE_READONLY,
            counter_ptr(&calls),
            Some(count_destroy),
        );
        let second = hb_blob_reference(blob);
        hb_blob_destroy(blob);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        hb_blob_destroy(second);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// With no bytes to hold, HarfBuzz returns the empty blob and calls
/// `destroy` at once. The callback used to be dropped, which leaked
/// whatever `user_data` owned.
#[test]
fn empty_blob_calls_destroy_at_once() {
    let calls = AtomicU32::new(0);
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        let zero_length = hb_blob_create(
            OPEN_SANS.as_ptr().cast::<c_char>(),
            0,
            HB_MEMORY_MODE_READONLY,
            counter_ptr(&calls),
            Some(count_destroy),
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let null_data = hb_blob_create(
            ptr::null(),
            8,
            HB_MEMORY_MODE_READONLY,
            counter_ptr(&calls),
            Some(count_destroy),
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(hb_blob_get_length(null_data), 0);
        hb_blob_destroy(zero_length);
        hb_blob_destroy(null_data);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
