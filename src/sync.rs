//! A write-once box that threads can share without a lock, for the
//! caches a [`crate::Font`] builds the first time it needs them.
//!
//! The core crate has no `std`, so `std::sync::OnceLock` is out of
//! reach. [`OnceBox`] is the lock-free alternative: the first thread to
//! need the value builds it and publishes it with one compare-and-swap.
//! Threads that race it may build the value too, and drop theirs when
//! they lose. Shaping output never depends on which copy wins: every
//! cache built here reads the same font. Most hold the same data
//! whichever thread builds them. The lookup accelerators are the
//! exception: what they keep depends on the font's shared work budget
//! when they are built, so racing threads can keep different digests
//! (and memory and speed can follow call order), but a lookup without a
//! digest admits every glyph, so the glyphs it applies to are the same.

use alloc::boxed::Box;
use core::fmt;
use core::marker::PhantomData;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

/// A value built at most once and then shared, behind an atomic
/// pointer: null until the value is set, then a pointer from
/// [`Box::into_raw`] that only [`Drop`] frees.
pub(crate) struct OnceBox<T> {
    ptr: AtomicPtr<T>,
    /// The box is owned, so `OnceBox<T>` drops a `T`.
    _owns: PhantomData<Box<T>>,
}

impl<T> OnceBox<T> {
    /// An empty box.
    pub(crate) const fn new() -> Self {
        Self {
            ptr: AtomicPtr::new(ptr::null_mut()),
            _owns: PhantomData,
        }
    }

    /// A box that already holds `value`.
    pub(crate) fn with_value(value: T) -> Self {
        Self {
            ptr: AtomicPtr::new(Box::into_raw(Box::new(value))),
            _owns: PhantomData,
        }
    }

    /// The value, if it has been set.
    pub(crate) fn get(&self) -> Option<&T> {
        let p = self.ptr.load(Ordering::Acquire);
        // SAFETY: a non-null pointer came from `Box::into_raw` in
        // `with_value` or `get_or_init` and was published with a
        // release store or swap that the acquire load above pairs with,
        // so the value is fully written. It is only freed in `drop`,
        // which takes `&mut self`, so it outlives this `&self` borrow.
        unsafe { p.as_ref() }
    }

    /// The value, built with `init` if it has not been set. When two
    /// threads race, both may run `init`; the first to publish wins and
    /// the other's value is dropped.
    pub(crate) fn get_or_init(&self, init: impl FnOnce() -> T) -> &T {
        if let Some(value) = self.get() {
            return value;
        }
        let new = Box::into_raw(Box::new(init()));
        match self
            .ptr
            .compare_exchange(ptr::null_mut(), new, Ordering::AcqRel, Ordering::Acquire)
        {
            // SAFETY: `new` came from `Box::into_raw` just above and is
            // now owned by `self`, which frees it only in `drop`.
            Ok(_) => unsafe { &*new },
            Err(winner) => {
                // SAFETY: the swap failed, so `new` was never published
                // and this thread still owns it; `Box::from_raw` takes
                // it back to drop it.
                drop(unsafe { Box::from_raw(new) });
                // SAFETY: `winner` is the non-null pointer another
                // thread published (the swap only fails when the slot
                // is no longer null), acquired by the failure ordering,
                // and freed only in `drop`.
                unsafe { &*winner }
            }
        }
    }
}

impl<T> Drop for OnceBox<T> {
    fn drop(&mut self) {
        let p = *self.ptr.get_mut();
        if !p.is_null() {
            // SAFETY: a non-null pointer came from `Box::into_raw` and
            // `&mut self` means no borrow handed out by `get` is alive,
            // so this is the last use of it.
            drop(unsafe { Box::from_raw(p) });
        }
    }
}

impl<T> Default for OnceBox<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: fmt::Debug> fmt::Debug for OnceBox<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OnceBox").field(&self.get()).finish()
    }
}

// SAFETY: `OnceBox<T>` owns a `T` like `Box<T>`, so sending it to
// another thread sends the `T`.
unsafe impl<T: Send> Send for OnceBox<T> {}
// SAFETY: a shared `OnceBox<T>` hands out `&T` to every thread (needs
// `T: Sync`), and a thread that loses an initialization race drops the
// `T` it built while the winner's is in use elsewhere, and the box
// drops whichever `T` it holds on the thread that drops it (needs
// `T: Send`).
unsafe impl<T: Send + Sync> Sync for OnceBox<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::sync::atomic::AtomicUsize;

    #[test]
    fn builds_once_and_keeps_the_value() {
        let cell: OnceBox<Vec<u32>> = OnceBox::new();
        assert!(cell.get().is_none());
        assert_eq!(cell.get_or_init(|| alloc::vec![1, 2, 3]), &[1, 2, 3]);
        assert_eq!(cell.get_or_init(|| alloc::vec![9]), &[1, 2, 3]);
        assert_eq!(cell.get(), Some(&alloc::vec![1, 2, 3]));
        let set = OnceBox::with_value(7u8);
        assert_eq!(set.get_or_init(|| 8), &7);
    }

    #[test]
    fn drops_the_value_once() {
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let cell = OnceBox::new();
            cell.get_or_init(|| Counted(drops.clone()));
            cell.get_or_init(|| Counted(drops.clone()));
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(OnceBox::<Counted>::new());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "std")]
    #[test]
    fn racing_threads_agree_on_one_value() {
        for _ in 0..50 {
            let cell: Arc<OnceBox<Vec<usize>>> = Arc::new(OnceBox::new());
            let builds = Arc::new(AtomicUsize::new(0));
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let (cell, builds) = (cell.clone(), builds.clone());
                    std::thread::spawn(move || {
                        let v = cell.get_or_init(|| {
                            builds.fetch_add(1, Ordering::SeqCst);
                            (0..64).collect()
                        });
                        (v.as_ptr() as usize, v.len())
                    })
                })
                .collect();
            let seen: Vec<(usize, usize)> =
                handles.into_iter().map(|h| h.join().unwrap()).collect();
            assert!(seen.iter().all(|s| *s == seen[0]), "{seen:?}");
            assert!(builds.load(Ordering::SeqCst) >= 1);
        }
    }
}
