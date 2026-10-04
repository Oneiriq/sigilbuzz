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
//! - [`InstanceCache`], which depends on the variation coordinates too:
//!   the vertical origins and the advances that come from phantom
//!   points, which cost an outline walk per glyph. `with_size` keeps it
//!   and `with_coords` starts a new one.
//!
//! A font's first shaping call builds none of this: a font shaped once,
//! which many callers build per run, would spend more building it than
//! it saves. From the second call on, each part is built the first time
//! a call needs it.
//!
//! Both caches are shared by every clone of the font made after they
//! were built, through an `Arc`, and every part is built through a
//! [`OnceBox`] or written with single atomic stores, so a font can be
//! shared by shaping threads without a lock. Whatever a cache holds is
//! a pure function of the font data and coordinates, so a value one
//! thread computes is the value any other would have, and shaping
//! output does not depend on what is cached.
//!
//! Memory is bounded per font. For each of GSUB and GPOS: one pointer
//! per lookup, 24 bytes per subtable of each lookup a run reached, and
//! at most 16 resolved language systems of at most 1024 features and
//! 16384 lookup indices each. For the glyph caches: at most
//! [`GLYPH_CACHE_MAX`] four-byte entries for each of the three,
//! whatever the glyph count.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::sync::OnceBox;
use crate::tables::layout::accel::LayoutCache;
use crate::tables::layout::LayoutTable;

/// The caches of one [`Font`](super::Font).
pub(crate) struct FontCaches {
    face: OnceBox<Arc<FaceCache>>,
    instance: OnceBox<Arc<InstanceCache>>,
    /// Whether the font has been shaped with.
    used: AtomicBool,
}

impl FontCaches {
    /// Nothing built yet.
    pub(crate) const fn new() -> Self {
        Self {
            face: OnceBox::new(),
            instance: OnceBox::new(),
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

    /// What depends on the coordinates too, created when first asked
    /// for.
    pub(crate) fn instance(&self) -> &InstanceCache {
        self.instance.get_or_init(|| Arc::new(InstanceCache::new()))
    }

    /// The caches for a font with the same data at other coordinates:
    /// the face cache shared if it exists, no instance cache.
    pub(crate) fn for_other_coords(&self) -> Self {
        Self {
            face: share(&self.face),
            instance: OnceBox::new(),
            used: AtomicBool::new(self.used.load(Ordering::Relaxed)),
        }
    }

    /// Heap bytes the built caches hold.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.face.get().map_or(0, |c| c.heap_bytes())
            + self.instance.get().map_or(0, |c| c.heap_bytes())
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
            instance: share(&self.instance),
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

/// What a font keeps that depends on its coordinates too: per-glyph
/// values whose computation walks an outline.
pub(crate) struct InstanceCache {
    v_origins: OnceBox<GlyphCache>,
    v_advances: OnceBox<GlyphCache>,
    h_advances: OnceBox<GlyphCache>,
}

/// Which per-glyph value of an [`InstanceCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlyphValue {
    /// The y of the vertical origin.
    VOrigin,
    /// The vertical advance from varied phantom points, if they could
    /// be computed.
    VPhantomAdvance,
    /// The horizontal advance from varied phantom points, if they
    /// could be computed.
    HPhantomAdvance,
}

impl InstanceCache {
    const fn new() -> Self {
        Self {
            v_origins: OnceBox::new(),
            v_advances: OnceBox::new(),
            h_advances: OnceBox::new(),
        }
    }

    fn cache(&self, which: GlyphValue) -> &OnceBox<GlyphCache> {
        match which {
            GlyphValue::VOrigin => &self.v_origins,
            GlyphValue::VPhantomAdvance => &self.v_advances,
            GlyphValue::HPhantomAdvance => &self.h_advances,
        }
    }

    /// The cached `which` of glyph `gid`, if any.
    pub(crate) fn get(&self, which: GlyphValue, gid: u16) -> Option<Known> {
        self.cache(which).get()?.get(gid)
    }

    /// Caches `which` of glyph `gid` in a font of `num_glyphs` glyphs.
    /// A value outside the range a cache entry holds is not cached.
    pub(crate) fn set(&self, which: GlyphValue, gid: u16, num_glyphs: u16, value: Option<i32>) {
        self.cache(which)
            .get_or_init(|| GlyphCache::new(num_glyphs))
            .set(gid, value);
    }

    fn heap_bytes(&self) -> usize {
        core::mem::size_of::<Self>()
            + [&self.v_origins, &self.v_advances, &self.h_advances]
                .iter()
                .filter_map(|c| c.get())
                .map(GlyphCache::heap_bytes)
                .sum::<usize>()
    }
}

/// A cached per-glyph value: `None` for a glyph whose value could not
/// be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Known(pub(crate) Option<i32>);

/// Entries of the largest [`GlyphCache`]: 16 KiB.
pub(crate) const GLYPH_CACHE_MAX: usize = 4096;

/// A direct-mapped cache of one `i16`-sized value per glyph, HarfBuzz's
/// `hb_cache_t`: entry `gid % len` holds the glyph id in its high half
/// and the value in its low half, in one atomic word, so a reader sees
/// a whole entry or none and needs no lock. A glyph that shares its
/// entry with another evicts it.
struct GlyphCache {
    entries: Box<[AtomicU32]>,
}

/// An entry no glyph has written: glyph id 0xFFFF, which a font never
/// has (it has at most 65535 glyphs) and [`GlyphCache::set`] skips.
const EMPTY: u32 = u32::MAX;
/// The stored value of a glyph whose value could not be computed.
const NONE: u16 = 0x8000;

impl GlyphCache {
    /// Entries for every glyph of a small font, [`GLYPH_CACHE_MAX`] for
    /// a large one.
    fn new(num_glyphs: u16) -> Self {
        let len = usize::from(num_glyphs)
            .max(1)
            .next_power_of_two()
            .min(GLYPH_CACHE_MAX);
        Self {
            entries: (0..len).map(|_| AtomicU32::new(EMPTY)).collect(),
        }
    }

    fn slot(&self, gid: u16) -> &AtomicU32 {
        // The length is a power of two, never zero.
        &self.entries[usize::from(gid) & (self.entries.len() - 1)]
    }

    fn get(&self, gid: u16) -> Option<Known> {
        let entry = self.slot(gid).load(Ordering::Relaxed);
        if entry >> 16 != u32::from(gid) || gid == u16::MAX {
            return None;
        }
        let stored = entry as u16;
        Some(Known((stored != NONE).then_some(i32::from(stored as i16))))
    }

    fn set(&self, gid: u16, value: Option<i32>) {
        let stored = match value {
            None => NONE,
            Some(v) => match i16::try_from(v) {
                Ok(v) if v as u16 != NONE => v as u16,
                _ => return,
            },
        };
        if gid == u16::MAX {
            return;
        }
        self.slot(gid)
            .store(u32::from(gid) << 16 | u32::from(stored), Ordering::Relaxed);
    }

    fn heap_bytes(&self) -> usize {
        core::mem::size_of::<Self>() + self.entries.len() * core::mem::size_of::<AtomicU32>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_caches_round_trip_and_evict() {
        let cache = GlyphCache::new(10);
        assert_eq!(cache.entries.len(), 16);
        assert_eq!(cache.get(3), None);
        cache.set(3, Some(-120));
        cache.set(4, None);
        cache.set(5, Some(i32::from(i16::MAX)));
        assert_eq!(cache.get(3), Some(Known(Some(-120))));
        assert_eq!(cache.get(4), Some(Known(None)));
        assert_eq!(cache.get(5), Some(Known(Some(i32::from(i16::MAX)))));
        // Glyph 19 shares glyph 3's entry and evicts it.
        cache.set(19, Some(7));
        assert_eq!(cache.get(3), None);
        assert_eq!(cache.get(19), Some(Known(Some(7))));
        // Values an entry cannot hold are not cached.
        cache.set(6, Some(40_000));
        cache.set(7, Some(i32::from(i16::MIN)));
        assert_eq!(cache.get(6), None);
        assert_eq!(cache.get(7), None);
        cache.set(u16::MAX, Some(1));
        assert_eq!(cache.get(u16::MAX), None);
        // Large fonts get a bounded cache.
        assert_eq!(GlyphCache::new(u16::MAX).entries.len(), GLYPH_CACHE_MAX);
        assert_eq!(GlyphCache::new(0).entries.len(), 1);
    }

    #[test]
    fn caches_are_shared_once_built() {
        let caches = FontCaches::new();
        let empty = caches.clone();
        let face = core::ptr::from_ref(caches.face());
        caches
            .instance()
            .set(GlyphValue::VOrigin, 1, 100, Some(880));
        let shared = caches.clone();
        assert!(core::ptr::eq(shared.face(), face));
        assert_eq!(
            shared.instance().get(GlyphValue::VOrigin, 1),
            Some(Known(Some(880)))
        );
        let other = caches.for_other_coords();
        assert!(core::ptr::eq(other.face(), face));
        assert_eq!(other.instance().get(GlyphValue::VOrigin, 1), None);
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
