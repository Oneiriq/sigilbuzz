//! What a [`Font`](super::Font) keeps between shaping calls.
//!
//! HarfBuzz keeps a face's lookup accelerators and shape plans, and a
//! font's metrics caches, for as long as the face and font live, which
//! is why its per-call cost is small. A `Font` here keeps the same
//! kinds of data:
//!
//! - [`FaceCache`], which depends on the font data alone: for GSUB and
//!   GPOS, the lookup accelerators (see [`crate::tables::layout::accel`])
//!   and the language systems resolved for each combination of script
//!   tags, language and FeatureVariations record a run used (see
//!   [`crate::ot::layout_select`]).
//!   [`Font::with_coords`](super::Font::with_coords) and
//!   [`Font::with_size`](super::Font::with_size) keep it.
//!
//! A font's first shaping call builds none of this: a font shaped once,
//! which many callers build per run, would spend more building it than
//! it saves. From the second call on, each part is built the first time
//! a call needs it.
//!
//! The cache is shared by every clone of the font made after it was
//! built, through an `Arc`, and every part is built through a
//! [`OnceBox`], so a font can be shared by shaping threads without a
//! lock. Whatever a cache holds is
//! a pure function of the font data and coordinates, so a value one
//! thread computes is the value any other would have, and shaping
//! output does not depend on what is cached.
//!
//! Memory is bounded per font. For each of GSUB and GPOS: one pointer
//! per lookup, 24 bytes per subtable of each lookup a run reached, and
//! at most 16 resolved language systems of at most 1024 features and
//! 16384 lookup indices each.

use alloc::sync::Arc;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sync::OnceBox;
use crate::tables::layout::accel::LayoutCache;
use crate::tables::layout::LayoutTable;

/// The caches of one [`Font`](super::Font).
pub(crate) struct FontCaches {
    face: OnceBox<Arc<FaceCache>>,
    /// Whether the font has been shaped with.
    used: AtomicBool,
}

impl FontCaches {
    /// Nothing built yet.
    pub(crate) const fn new() -> Self {
        Self {
            face: OnceBox::new(),
            used: AtomicBool::new(false),
        }
    }

    /// Records a shaping call with the font. True when an earlier call
    /// used it, or a font it was copied from, so the caches are likely
    /// to pay off.
    pub(crate) fn note_use(&self) -> bool {
        self.used.swap(true, Ordering::Relaxed)
    }

    /// What depends on the font data alone, created when first asked
    /// for.
    pub(crate) fn face(&self) -> &FaceCache {
        self.face.get_or_init(|| Arc::new(FaceCache::new()))
    }

    /// The caches for a font with the same data at other coordinates:
    /// the face cache shared if it exists.
    pub(crate) fn for_other_coords(&self) -> Self {
        self.clone()
    }

    /// Heap bytes the built caches hold.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.face.get().map_or(0, |c| c.heap_bytes())
    }
}

/// A box holding the `Arc` `cell` holds, if any.
fn share<T>(cell: &OnceBox<Arc<T>>) -> OnceBox<Arc<T>> {
    match cell.get() {
        Some(arc) => OnceBox::with_value(Arc::clone(arc)),
        None => OnceBox::new(),
    }
}

impl Clone for FontCaches {
    /// Shares whatever is built; what is not built yet is built
    /// separately by each copy.
    fn clone(&self) -> Self {
        Self {
            face: share(&self.face),
            used: AtomicBool::new(self.used.load(Ordering::Relaxed)),
        }
    }
}

impl Default for FontCaches {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for FontCaches {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FontCaches")
            .field("heap_bytes", &self.heap_bytes())
            .finish()
    }
}

/// What a font keeps that depends on its data alone.
pub(crate) struct FaceCache {
    gsub: OnceBox<LayoutCache>,
    gpos: OnceBox<LayoutCache>,
}

impl FaceCache {
    const fn new() -> Self {
        Self {
            gsub: OnceBox::new(),
            gpos: OnceBox::new(),
        }
    }

    /// What the font keeps for its GSUB, which has `lookup_count`
    /// lookups.
    pub(crate) fn gsub(&self, lookup_count: u16) -> &LayoutCache {
        self.gsub
            .get_or_init(|| LayoutCache::new(LayoutTable::Gsub, lookup_count))
    }

    /// What the font keeps for its GPOS, which has `lookup_count`
    /// lookups.
    pub(crate) fn gpos(&self, lookup_count: u16) -> &LayoutCache {
        self.gpos
            .get_or_init(|| LayoutCache::new(LayoutTable::Gpos, lookup_count))
    }

    fn heap_bytes(&self) -> usize {
        core::mem::size_of::<Self>()
            + [&self.gsub, &self.gpos]
                .iter()
                .filter_map(|c| c.get())
                .map(|a| core::mem::size_of::<LayoutCache>() + a.heap_bytes())
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_are_shared_once_built() {
        let caches = FontCaches::new();
        let empty = caches.clone();
        let face = core::ptr::from_ref(caches.face());
        let shared = caches.clone();
        assert!(core::ptr::eq(shared.face(), face));
        let other = caches.for_other_coords();
        assert!(core::ptr::eq(other.face(), face));
        // A copy made before anything was built builds its own.
        assert!(!core::ptr::eq(empty.face(), face));
        assert!(caches.heap_bytes() > 0);
    }

    #[test]
    fn caches_are_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<FontCaches>();
    }
}
