//! HarfBuzz-style object identity and reference counting.
//!
//! In HarfBuzz the pointer *is* the object. `hb_x_reference(p)` adds one
//! reference and returns `p` itself; `hb_x_destroy(p)` drops one and
//! frees the object when the last reference goes. Code written against
//! HarfBuzz leans on that identity all the time, for example:
//!
//! ```c
//! hb_blob_t *b = hb_blob_create(...);
//! hb_blob_reference(b);   /* return value ignored: it is `b` */
//! ...
//! hb_blob_destroy(b);
//! hb_blob_destroy(b);     /* the second reference */
//! ```
//!
//! Every refcounted type this crate exports (`hb_blob_t`, `hb_face_t`,
//! `hb_font_t`, `hb_buffer_t`, `hb_set_t`, `hb_subset_input_t`,
//! `hb_paint_funcs_t`) follows the same rule. The handle struct itself
//! lives inside an [`Arc`] allocation and C receives the
//! [`Arc::into_raw`] pointer. Referencing bumps that `Arc`'s strong
//! count and hands back the same pointer; destroying decrements it and
//! runs the handle's destructor at zero.
//!
//! Objects that hold other objects (a face holds its blob, a font holds
//! its face, a subset input holds its sets) keep an owning `Arc` to the
//! same allocation C sees, so HarfBuzz's "the font keeps the face alive"
//! lifetime rules carry over unchanged.
//!
//! Null handling matches HarfBuzz: referencing null returns null and
//! destroying null does nothing. sigilbuzz has no inert singleton
//! objects. A constructor that cannot build what was asked for returns
//! a fresh, empty object instead, which the caller destroys like any
//! other (under HarfBuzz, destroying the inert empty object it would
//! return is a harmless no-op, so the same calling code works with
//! both libraries).

use alloc::sync::Arc;

/// Moves `value` into a new refcounted allocation and returns the C
/// handle for it, carrying one reference.
pub(crate) fn into_raw<T>(value: T) -> *mut T {
    arc_into_raw(Arc::new(value))
}

/// Converts an owned `Arc` into a C handle that carries the `Arc`'s
/// reference.
pub(crate) fn arc_into_raw<T>(arc: Arc<T>) -> *mut T {
    Arc::into_raw(arc).cast_mut()
}

/// `hb_x_reference`: adds one reference and returns `ptr` unchanged.
/// Null in, null out.
///
/// # Safety
/// `ptr` must be null or a live handle created by [`into_raw`] or
/// [`arc_into_raw`] for the same `T`.
pub(crate) unsafe fn reference<T>(ptr: *mut T) -> *mut T {
    if !ptr.is_null() {
        // SAFETY: the caller guarantees `ptr` came from `Arc::into_raw`
        // for `T` and that at least one strong reference is still live,
        // which is all `increment_strong_count` requires.
        unsafe { Arc::increment_strong_count(ptr.cast_const()) };
    }
    ptr
}

/// `hb_x_destroy`: drops one reference, freeing the object when it was
/// the last. Null is a no-op.
///
/// # Safety
/// `ptr` must be null or a live handle created by [`into_raw`] or
/// [`arc_into_raw`] for the same `T`, and the caller must own the
/// reference it gives up here.
pub(crate) unsafe fn destroy<T>(ptr: *mut T) {
    if !ptr.is_null() {
        // SAFETY: the caller guarantees `ptr` came from `Arc::into_raw`
        // for `T` and that it owns one of the live strong references,
        // so releasing it cannot underflow the count.
        unsafe { Arc::decrement_strong_count(ptr.cast_const()) };
    }
}

/// Takes a new owning `Arc` to the object behind `ptr`, leaving the
/// caller's own reference untouched. Used when one object keeps
/// another alive (a face holding its blob, for instance).
///
/// # Safety
/// `ptr` must be a non-null, live handle created by [`into_raw`] or
/// [`arc_into_raw`] for the same `T`.
pub(crate) unsafe fn retain<T>(ptr: *const T) -> Arc<T> {
    // SAFETY: the caller guarantees `ptr` is a live `Arc::into_raw`
    // pointer for `T`. Incrementing first means the `Arc` rebuilt by
    // `from_raw` owns a reference of its own, so dropping it later
    // balances the increment and never touches the caller's reference.
    unsafe {
        Arc::increment_strong_count(ptr);
        Arc::from_raw(ptr)
    }
}

#[cfg(test)]
mod tests;
