//! `hb_set_t`: opaque integer set used by the subset and
//! introspection bridges.
//!
//! HarfBuzz's `hb_set_t` is a refcounted, mutable, sparse set of
//! 32-bit integers. The subset surface uses it for "unicode set" /
//! "glyph set" inputs, and the introspection helpers
//! (`hb_face_collect_unicodes` / `hb_ot_layout_collect_features`)
//! hand back populated sets. We back it with an
//! `Arc<SpinMutex<BTreeSet<u32>>>`. `BTreeSet` keeps iteration in
//! ascending order (the contract `hb_set_next` advertises), and the
//! `Arc` lets the same set be observed through multiple refcount
//! handles, mirroring HarfBuzz's "an `hb_subset_input_t` returns a
//! handle to its internal set" idiom.
//!
//! The spin lock makes every access safe from any thread, the same
//! way the buffer and font handles work. No lock is held across a
//! call back into C, so a set cannot deadlock on itself.
//!
//! # Refcount contract
//!
//! - `hb_set_create` -> refcount 1.
//! - `hb_set_reference(set)` -> refcount + 1, returns a fresh handle.
//! - `hb_set_destroy(set)` -> refcount - 1, frees the BTreeSet when it
//!   hits zero.

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use core::ptr;

use crate::hb_bool_t;
use crate::spin_mutex::SpinMutex;

/// Shared payload behind an `hb_set_t` handle.
pub(crate) type SharedSet = Arc<SpinMutex<BTreeSet<u32>>>;

/// Opaque integer set. The struct itself is a thin handle; the
/// shared payload lives behind the inner `Arc`.
#[repr(C)]
pub struct hb_set_t {
    inner: SharedSet,
}

impl hb_set_t {
    /// Internal constructor.
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(SpinMutex::new(BTreeSet::new())),
        }
    }

    /// Internal: wrap a pre-existing shared BTreeSet so a sibling
    /// crate (e.g. `hb_subset_input_t`'s sets) can hand out an
    /// `hb_set_t` handle that observes the same payload. Only the
    /// `subset` cargo feature uses this helper today.
    #[cfg(feature = "subset")]
    pub(crate) fn from_arc(inner: SharedSet) -> Self {
        Self { inner }
    }

    /// Internal: borrow the underlying BTreeSet for read.
    pub(crate) fn with_inner<R>(&self, f: impl FnOnce(&BTreeSet<u32>) -> R) -> R {
        f(&self.inner.lock())
    }

    /// Internal: borrow the underlying BTreeSet for write.
    pub(crate) fn with_inner_mut<R>(&self, f: impl FnOnce(&mut BTreeSet<u32>) -> R) -> R {
        f(&mut self.inner.lock())
    }

    /// Internal: clone the Arc so a sibling handle observes the same
    /// payload.
    pub(crate) fn share(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Allocates a fresh empty set with refcount 1.
#[no_mangle]
pub extern "C" fn hb_set_create() -> *mut hb_set_t {
    Box::into_raw(Box::new(hb_set_t::new()))
}

/// Releases one reference. Frees the underlying BTreeSet when the
/// last reference drops.
///
/// # Safety
/// `set` must be null or a pointer previously returned by
/// `hb_set_create` / `hb_set_reference`.
#[no_mangle]
pub unsafe extern "C" fn hb_set_destroy(set: *mut hb_set_t) {
    if set.is_null() {
        return;
    }
    // SAFETY: `set` is non-null and the caller guarantees it came
    // from `Box::into_raw` in `hb_set_create` or `hb_set_reference`
    // and has not been destroyed yet. Dropping the box decrements
    // the inner Arc.
    drop(unsafe { Box::from_raw(set) });
}

/// Allocates a fresh handle that observes the same payload.
///
/// # Safety
/// `set` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_set_reference(set: *mut hb_set_t) -> *mut hb_set_t {
    if set.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `set` is non-null and the caller guarantees it points
    // to a live `hb_set_t`.
    let shared = unsafe { (*set).share() };
    Box::into_raw(Box::new(shared))
}

/// Adds `codepoint` to the set. No-op if already present.
///
/// # Safety
/// `set` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_set_add(set: *mut hb_set_t, codepoint: u32) {
    if set.is_null() {
        return;
    }
    // SAFETY: `set` is non-null and the caller guarantees it points
    // to a live `hb_set_t`.
    unsafe { (*set).with_inner_mut(|s| s.insert(codepoint)) };
}

/// Removes `codepoint` from the set. No-op if absent.
///
/// # Safety
/// `set` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_set_del(set: *mut hb_set_t, codepoint: u32) {
    if set.is_null() {
        return;
    }
    // SAFETY: `set` is non-null and the caller guarantees it points
    // to a live `hb_set_t`.
    unsafe { (*set).with_inner_mut(|s| s.remove(&codepoint)) };
}

/// Returns 1 if `codepoint` is in the set, 0 otherwise.
///
/// # Safety
/// `set` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_set_has(set: *const hb_set_t, codepoint: u32) -> hb_bool_t {
    if set.is_null() {
        return 0;
    }
    // SAFETY: `set` is non-null and the caller guarantees it points
    // to a live `hb_set_t`.
    unsafe {
        if (*set).with_inner(|s| s.contains(&codepoint)) {
            1
        } else {
            0
        }
    }
}

/// Returns the number of integers currently in the set.
///
/// # Safety
/// `set` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_set_get_population(set: *const hb_set_t) -> u32 {
    if set.is_null() {
        return 0;
    }
    // SAFETY: `set` is non-null and the caller guarantees it points
    // to a live `hb_set_t`.
    unsafe { (*set).with_inner(|s| s.len()) as u32 }
}

/// Iterator over the set, ascending. The HarfBuzz contract is:
/// `*codepoint == HB_SET_VALUE_INVALID` (we treat as `u32::MAX` here)
/// before the first call; subsequent calls advance to the next member
/// strictly greater than `*codepoint`. Returns 0 (false) when no
/// further member exists; in that case `*codepoint` is left untouched.
///
/// # Safety
/// `set` must be null or valid; `codepoint` must be null or point to
/// a writable `u32`.
#[no_mangle]
pub unsafe extern "C" fn hb_set_next(set: *const hb_set_t, codepoint: *mut u32) -> hb_bool_t {
    if set.is_null() || codepoint.is_null() {
        return 0;
    }
    // SAFETY: `codepoint` is non-null and the caller guarantees it
    // points to a readable and writable `u32`.
    let current = unsafe { *codepoint };
    // SAFETY: `set` is non-null and the caller guarantees it points
    // to a live `hb_set_t`.
    let next = unsafe {
        (*set).with_inner(|s| {
            if current == u32::MAX {
                // Sentinel "before the first member": pick the
                // smallest entry. HarfBuzz documents
                // `HB_SET_VALUE_INVALID == 0xFFFFFFFFu`.
                s.iter().next().copied()
            } else {
                // Return the smallest member strictly greater than `current`.
                s.range((
                    core::ops::Bound::Excluded(current),
                    core::ops::Bound::Unbounded,
                ))
                .next()
                .copied()
            }
        })
    };
    match next {
        Some(v) => {
            // SAFETY: `codepoint` is non-null and the caller
            // guarantees it points to a writable `u32`.
            unsafe { *codepoint = v };
            1
        }
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_destroy_null_safe() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            hb_set_destroy(ptr::null_mut());
        }
    }

    #[test]
    fn add_has_population() {
        let s = hb_set_create();
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            assert_eq!(hb_set_get_population(s), 0);
            hb_set_add(s, 65);
            hb_set_add(s, 66);
            hb_set_add(s, 65); // dup
            assert_eq!(hb_set_get_population(s), 2);
            assert_eq!(hb_set_has(s, 65), 1);
            assert_eq!(hb_set_has(s, 67), 0);
            hb_set_del(s, 65);
            assert_eq!(hb_set_has(s, 65), 0);
            assert_eq!(hb_set_get_population(s), 1);
            hb_set_destroy(s);
        }
    }

    #[test]
    fn next_walks_ascending() {
        let s = hb_set_create();
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            hb_set_add(s, 100);
            hb_set_add(s, 1);
            hb_set_add(s, 50);
            let mut cp: u32 = u32::MAX;
            assert_eq!(hb_set_next(s, &mut cp), 1);
            assert_eq!(cp, 1);
            assert_eq!(hb_set_next(s, &mut cp), 1);
            assert_eq!(cp, 50);
            assert_eq!(hb_set_next(s, &mut cp), 1);
            assert_eq!(cp, 100);
            assert_eq!(hb_set_next(s, &mut cp), 0);
            // cp remains untouched after a terminal call.
            assert_eq!(cp, 100);
            hb_set_destroy(s);
        }
    }

    #[test]
    fn reference_shares_payload() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            let a = hb_set_create();
            hb_set_add(a, 7);
            let b = hb_set_reference(a);
            // Mutating through `a` must be observable through `b`.
            hb_set_add(a, 9);
            assert_eq!(hb_set_has(b, 7), 1);
            assert_eq!(hb_set_has(b, 9), 1);
            assert_eq!(hb_set_get_population(b), 2);
            // Destroying `a` first must leave `b` valid.
            hb_set_destroy(a);
            assert_eq!(hb_set_has(b, 9), 1);
            hb_set_destroy(b);
        }
    }

    #[test]
    fn empty_next_yields_false() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            let s = hb_set_create();
            let mut cp: u32 = u32::MAX;
            assert_eq!(hb_set_next(s, &mut cp), 0);
            hb_set_destroy(s);
        }
    }

    #[test]
    fn null_setters_are_noops() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            hb_set_add(ptr::null_mut(), 5);
            hb_set_del(ptr::null_mut(), 5);
            assert_eq!(hb_set_has(ptr::null(), 5), 0);
            assert_eq!(hb_set_get_population(ptr::null()), 0);
            let mut cp: u32 = u32::MAX;
            assert_eq!(hb_set_next(ptr::null(), &mut cp), 0);
        }
    }
}
