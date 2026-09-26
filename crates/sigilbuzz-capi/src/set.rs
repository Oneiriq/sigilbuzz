//! `hb_set_t`: opaque integer set used by the subset and
//! introspection bridges.
//!
//! HarfBuzz's `hb_set_t` is a refcounted, mutable, sparse set of
//! 32-bit integers. The subset surface uses it for "unicode set" /
//! "glyph set" inputs, and the introspection helpers
//! (`hb_face_collect_unicodes` / `hb_ot_layout_collect_features`)
//! hand back populated sets. We back it with a
//! `RefCell<BTreeSet<u32>>`. `BTreeSet` keeps iteration in
//! ascending order (the contract `hb_set_next` advertises).
//!
//! `RefCell` is sound here because every mutator routes through a
//! C-side pointer; the C ABI never observes a held borrow.
//!
//! # Refcount contract
//!
//! Sets have HarfBuzz identity semantics (see the crate-level
//! "Refcounting and ownership" notes):
//!
//! - `hb_set_create` -> refcount 1.
//! - `hb_set_reference(set)` -> refcount + 1, returns `set` itself.
//! - `hb_set_destroy(set)` -> refcount - 1, frees the set when it hits
//!   zero.
//! - A set returned by `hb_subset_input_unicode_set` /
//!   `hb_subset_input_glyph_set` belongs to the input: do not destroy
//!   it. Reference it if it must outlive the input.

extern crate alloc;

use alloc::collections::BTreeSet;
use core::cell::RefCell;

use crate::{handle, hb_bool_t};

/// Opaque integer set. C holds the `Arc` pointer to this struct; see
/// the `handle` module.
#[repr(C)]
pub struct hb_set_t {
    inner: RefCell<BTreeSet<u32>>,
}

// SAFETY: `RefCell` is `!Sync`, but every access is gated by the C
// ABI surface: the C caller never holds a `&` to the underlying
// BTreeSet across a callback boundary. The Send/Sync claims here
// match the contract HarfBuzz itself documents: an `hb_set_t` is
// safe to share between threads as long as accesses are serialized
// externally. See the module-level note on the safety story.
unsafe impl Send for hb_set_t {}
unsafe impl Sync for hb_set_t {}

impl hb_set_t {
    /// Internal constructor.
    pub(crate) fn new() -> Self {
        Self {
            inner: RefCell::new(BTreeSet::new()),
        }
    }

    /// Internal: borrow the underlying BTreeSet for read.
    pub(crate) fn with_inner<R>(&self, f: impl FnOnce(&BTreeSet<u32>) -> R) -> R {
        f(&self.inner.borrow())
    }

    /// Internal: borrow the underlying BTreeSet for write.
    pub(crate) fn with_inner_mut<R>(&self, f: impl FnOnce(&mut BTreeSet<u32>) -> R) -> R {
        f(&mut self.inner.borrow_mut())
    }
}

/// Allocates a fresh empty set with refcount 1.
#[no_mangle]
pub extern "C" fn hb_set_create() -> *mut hb_set_t {
    handle::into_raw(hb_set_t::new())
}

/// Releases one reference. Frees the set when the last reference
/// drops. Null is a no-op.
///
/// # Safety
/// `set` must be null or a live set the caller holds a reference to.
/// A set returned by `hb_subset_input_unicode_set` /
/// `hb_subset_input_glyph_set` is owned by the input and must not be
/// passed here unless the caller took its own reference first.
#[no_mangle]
pub unsafe extern "C" fn hb_set_destroy(set: *mut hb_set_t) {
    // SAFETY: caller guarantees `set` is null or a live handle it owns
    // a reference to.
    unsafe { handle::destroy(set) };
}

/// Adds one reference to `set` and returns `set` itself. Null in, null
/// out.
///
/// # Safety
/// `set` must be null or a live set.
#[no_mangle]
pub unsafe extern "C" fn hb_set_reference(set: *mut hb_set_t) -> *mut hb_set_t {
    // SAFETY: caller guarantees `set` is null or a live handle.
    unsafe { handle::reference(set) }
}

/// Adds `codepoint` to the set. No-op if already present.
///
/// # Safety
/// `set` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hb_set_add(set: *mut hb_set_t, codepoint: u32) {
    if set.is_null() {
        return;
    }
    // SAFETY: caller asserts validity.
    unsafe { (*set).with_inner_mut(|s| s.insert(codepoint)) };
}

/// Removes `codepoint` from the set. No-op if absent.
///
/// # Safety
/// `set` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hb_set_del(set: *mut hb_set_t, codepoint: u32) {
    if set.is_null() {
        return;
    }
    // SAFETY: caller asserts validity.
    unsafe { (*set).with_inner_mut(|s| s.remove(&codepoint)) };
}

/// Returns 1 if `codepoint` is in the set, 0 otherwise.
///
/// # Safety
/// `set` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hb_set_has(set: *const hb_set_t, codepoint: u32) -> hb_bool_t {
    if set.is_null() {
        return 0;
    }
    // SAFETY: caller asserts validity.
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
/// `set` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hb_set_get_population(set: *const hb_set_t) -> u32 {
    if set.is_null() {
        return 0;
    }
    // SAFETY: caller asserts validity.
    unsafe { (*set).with_inner(|s| s.len()) as u32 }
}

/// Iterator over the set, ascending. The HarfBuzz contract is:
/// `*codepoint == HB_SET_VALUE_INVALID` (we treat as `u32::MAX` here)
/// before the first call; subsequent calls advance to the next member
/// strictly greater than `*codepoint`. Returns 0 (false) when no
/// further member exists; in that case `*codepoint` is left untouched.
///
/// # Safety
/// `set` must be valid; `codepoint` must point to a writable `u32`.
#[no_mangle]
pub unsafe extern "C" fn hb_set_next(set: *const hb_set_t, codepoint: *mut u32) -> hb_bool_t {
    if set.is_null() || codepoint.is_null() {
        return 0;
    }
    // SAFETY: caller asserts validity.
    let current = unsafe { *codepoint };
    // SAFETY: caller asserts validity.
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
            // SAFETY: caller asserts writeable.
            unsafe { *codepoint = v };
            1
        }
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::ptr;

    #[test]
    fn create_destroy_null_safe() {
        unsafe {
            hb_set_destroy(ptr::null_mut());
            assert!(hb_set_reference(ptr::null_mut()).is_null());
        }
    }

    #[test]
    fn add_has_population() {
        let s = hb_set_create();
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
        unsafe {
            let a = hb_set_create();
            hb_set_add(a, 7);
            let b = hb_set_reference(a);
            // HarfBuzz identity: referencing hands back the same object.
            assert_eq!(a, b);
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
        unsafe {
            let s = hb_set_create();
            let mut cp: u32 = u32::MAX;
            assert_eq!(hb_set_next(s, &mut cp), 0);
            hb_set_destroy(s);
        }
    }

    #[test]
    fn null_setters_are_noops() {
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
