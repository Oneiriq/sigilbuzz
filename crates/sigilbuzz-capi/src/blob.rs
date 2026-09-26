//! Blob functions: creating a blob from caller memory or a file,
//! reference counting, and reading the bytes back.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{c_char, c_uint, c_void};
use core::ptr;
use core::slice;

use crate::{
    handle, hb_blob_t, hb_destroy_func_t, hb_memory_mode_t, BlobInner, HB_MEMORY_MODE_DUPLICATE,
};

// ---------------------------------------------------------------------------
// Blob
// ---------------------------------------------------------------------------

/// Fresh empty blob, used in error paths. HarfBuzz returns its inert
/// empty blob there; sigilbuzz returns an ordinary empty blob that the
/// caller destroys as usual (see the `handle` module).
pub(crate) fn empty_blob() -> *mut hb_blob_t {
    handle::arc_into_raw(empty_blob_arc())
}

pub(crate) fn empty_blob_arc() -> Arc<hb_blob_t> {
    Arc::new(hb_blob_t {
        inner: BlobInner::from_data(Vec::new()),
    })
}

/// Creates a blob holding a copy of `length` bytes at `data`.
///
/// sigilbuzz always copies, whatever `mode` says. `destroy` follows
/// HarfBuzz's timing: it runs right away when there is nothing to
/// keep (zero length or null data, which both yield an empty blob) and
/// for `HB_MEMORY_MODE_DUPLICATE`, where HarfBuzz also copies up
/// front; for every other mode it runs once, when the last reference
/// to the blob (including the ones faces built on it hold) goes away.
///
/// # Safety
/// `data` must point to `length` bytes (or be null with `length == 0`).
/// `destroy`, when non-null, must accept `user_data`.
#[no_mangle]
pub unsafe extern "C" fn hb_blob_create(
    data: *const c_char,
    length: c_uint,
    mode: hb_memory_mode_t,
    user_data: *mut c_void,
    destroy: Option<hb_destroy_func_t>,
) -> *mut hb_blob_t {
    if length == 0 || data.is_null() {
        if let Some(destroy) = destroy {
            // SAFETY: caller-supplied callback that accepts `user_data`.
            unsafe { destroy(user_data) };
        }
        return empty_blob();
    }
    // SAFETY: caller asserts (data, length) is a valid byte range.
    let bytes = unsafe { slice::from_raw_parts(data.cast::<u8>(), length as usize) };
    let mut inner = BlobInner::from_data(bytes.to_vec());
    if mode == HB_MEMORY_MODE_DUPLICATE {
        if let Some(destroy) = destroy {
            // SAFETY: caller-supplied callback that accepts `user_data`;
            // the bytes are already copied, so the caller may free them.
            unsafe { destroy(user_data) };
        }
    } else {
        inner.user_destroy = destroy;
        inner.user_data = user_data;
    }
    handle::into_raw(hb_blob_t { inner })
}

/// # Safety
/// `file_name` must be a valid NUL-terminated UTF-8 path.
#[cfg(feature = "std")]
#[no_mangle]
pub unsafe extern "C" fn hb_blob_create_from_file(file_name: *const c_char) -> *mut hb_blob_t {
    if file_name.is_null() {
        return empty_blob();
    }
    // SAFETY: caller asserts NUL-terminated.
    let path_cstr = unsafe { core::ffi::CStr::from_ptr(file_name) };
    let Ok(path_str) = path_cstr.to_str() else {
        return empty_blob();
    };
    let Ok(bytes) = std::fs::read(path_str) else {
        return empty_blob();
    };
    handle::into_raw(hb_blob_t {
        inner: BlobInner::from_data(bytes),
    })
}

/// Releases one reference to `blob`. Null is a no-op.
///
/// # Safety
/// `blob` must be null or a live blob the caller holds a reference to.
#[no_mangle]
pub unsafe extern "C" fn hb_blob_destroy(blob: *mut hb_blob_t) {
    // SAFETY: caller guarantees `blob` is null or a live handle it owns
    // a reference to.
    unsafe { handle::destroy(blob) };
}

/// Adds one reference to `blob` and returns `blob` itself. Null in,
/// null out.
///
/// # Safety
/// `blob` must be null or a live blob.
#[no_mangle]
pub unsafe extern "C" fn hb_blob_reference(blob: *mut hb_blob_t) -> *mut hb_blob_t {
    // SAFETY: caller guarantees `blob` is null or a live handle.
    unsafe { handle::reference(blob) }
}

/// # Safety
/// `blob` must be valid; `length` may be null.
#[no_mangle]
pub unsafe extern "C" fn hb_blob_get_data(
    blob: *mut hb_blob_t,
    length: *mut c_uint,
) -> *const c_char {
    if blob.is_null() {
        if !length.is_null() {
            // SAFETY: caller asserts length is writeable.
            unsafe { *length = 0 };
        }
        return ptr::null();
    }
    // SAFETY: caller asserts validity.
    let inner: &BlobInner = unsafe { &(*blob).inner };
    let bytes: &[u8] = inner.data.as_slice();
    if !length.is_null() {
        // SAFETY: caller asserts length is writeable.
        unsafe { *length = bytes.len() as c_uint };
    }
    bytes.as_ptr().cast::<c_char>()
}

/// # Safety
/// `blob` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hb_blob_get_length(blob: *mut hb_blob_t) -> c_uint {
    if blob.is_null() {
        return 0;
    }
    // SAFETY: caller asserts validity.
    let inner: &BlobInner = unsafe { &(*blob).inner };
    inner.data.len() as c_uint
}
