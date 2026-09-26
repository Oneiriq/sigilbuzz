//! A minimal spin lock for the mutable handle state, so the C surface
//! needs neither `std::sync::Mutex` nor a dependency.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// Minimal spin lock; HarfBuzz callers rarely contend a buffer
/// across threads and the critical sections are microseconds at
/// most. A real Mutex would drag std into the no_std story.
pub(crate) struct SpinMutex<T> {
    locked: AtomicBool,
    inner: UnsafeCell<T>,
}

// SAFETY: SpinMutex serializes access to `inner`; the AtomicBool
// is the only cross-thread observable.
unsafe impl<T: Send> Send for SpinMutex<T> {}
unsafe impl<T: Send> Sync for SpinMutex<T> {}

impl<T> SpinMutex<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            inner: UnsafeCell::new(value),
        }
    }

    pub(crate) fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinGuard { mutex: self }
    }
}

pub(crate) struct SpinGuard<'a, T> {
    mutex: &'a SpinMutex<T>,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: lock() is held for the lifetime of the guard.
        unsafe { &*self.mutex.inner.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: lock() is held for the lifetime of the guard.
        unsafe { &mut *self.mutex.inner.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.locked.store(false, Ordering::Release);
    }
}
