//! HarfBuzz-symbol-compatible C API for sigilbuzz.
//!
//! Every public symbol in this crate is named `hb_*` so a binary
//! linker that previously resolved `hb_shape` against
//! `libharfbuzz.so` resolves it against `libsigilbuzz.so` without
//! a source-level change. The header at `include/hb.h` declares the
//! exact subset of HarfBuzz's public API the crate implements.
//!
//! # Refcounting
//!
//! Every opaque type (`hb_blob_t`, `hb_face_t`, `hb_font_t`,
//! `hb_buffer_t`) is a thin `#[repr(C)]` wrapper around an
//! `alloc::sync::Arc<Inner>`. `hb_*_destroy` drops the wrapper:
//! the Arc destructor handles refcount decrement and resource
//! release. `hb_*_reference` allocates a fresh wrapper backed by a
//! cloned Arc handle. Cloning a wrapper without going through
//! `hb_*_reference` is undefined behavior, just as in HarfBuzz
//! itself.
//!
//! # Lifetime erasure
//!
//! `sigilbuzz::Face<'a>` and `sigilbuzz::Font<'a>` borrow from a
//! byte slice. The C surface needs to expose those without the
//! lifetime parameter. We achieve that by:
//!
//! 1. `BlobInner` owns the bytes in an `Arc<Vec<u8>>`.
//! 2. `FaceInner` holds a clone of that Arc *and* a `Face<'static>`
//!    constructed via [`core::mem::transmute`]. The transmute is
//!    sound because the Arc clone keeps the underlying bytes alive
//!    for the lifetime of the FaceInner; the `'static` lifetime is
//!    a fiction the borrow checker accepts because the actual
//!    backing storage outlives every consumer.
//! 3. `FontInner` follows the same pattern and additionally owns
//!    the variation coords slice it lends to `Font` so the
//!    `Font<'static>` it holds remains valid.
//!
//! Every transmute is contained inside this crate; no `unsafe`
//! reaches the public Rust surface.
//!
//! # Null pointers and panics
//!
//! Every entry point accepts NULL for its object arguments and
//! returns a neutral value (an empty object, 0, or NULL) instead of
//! dereferencing it, as HarfBuzz does. No panic can unwind into C:
//! Rust 1.81, the minimum supported version, aborts the process when
//! a panic reaches an `extern "C"` function.

// The exported names follow HarfBuzz (`hb_blob_t`, `hb_shape`), not
// Rust naming conventions.
#![allow(non_camel_case_types, non_snake_case)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_uint, c_void};
use core::ptr;
use core::slice;

use sigilbuzz::{shape, Buffer, Direction, Face, Feature, Font};

// `hb_set_t` lives in its own module, the opaque integer-set type
// the subset and introspection bridges need. It has no dependency on
// the rest of the crate, so it ships unconditionally. `introspect`
// follows the same posture: it doesn't reach into the subsetter or
// paint evaluator, just walks tables sigilbuzz already parses.
pub mod introspect;
pub mod set;
// `subset_bridge` is gated on the `subset` cargo feature so a
// `--no-default-features` build of this crate still compiles cleanly
// without pulling in the companion subsetter crate. `paint_bridge`
// follows the same pattern.
#[cfg(feature = "paint")]
pub mod paint_bridge;
#[cfg(feature = "subset")]
pub mod subset_bridge;

// ---------------------------------------------------------------------------
// Refcounted opaque types
// ---------------------------------------------------------------------------

/// Owned font bytes plus the user-data destroy callback HarfBuzz
/// callers can hang off a blob. The destroy callback fires when
/// the Arc's refcount hits zero.
pub(crate) struct BlobInner {
    /// The actual font bytes. `Arc<Vec<u8>>` so a `FaceInner` can
    /// extend the same backing storage past the original blob's
    /// lifetime.
    pub(crate) data: Arc<Vec<u8>>,
    /// Optional caller-supplied destroy callback: fires once, when
    /// the BlobInner is dropped. HarfBuzz's `hb_blob_create` accepts
    /// `mode`, `user_data`, and `destroy` so callers passing
    /// `HB_MEMORY_MODE_READONLY` can use mmap'd buffers and have
    /// the destroy callback unmap on drop.
    user_destroy: Option<hb_destroy_func_t>,
    /// Opaque user_data threaded into the destroy callback. Send
    /// only because the inner is shipped across threads via Arc.
    user_data: *mut c_void,
}

// SAFETY: BlobInner does not access `user_data` itself. It only
// hands the pointer back to `user_destroy`. The HarfBuzz contract
// puts the burden of synchronization on the consumer; we mirror it.
unsafe impl Send for BlobInner {}
// SAFETY: see the `Send` impl above. No `&BlobInner` method reads or
// writes through `user_data`.
unsafe impl Sync for BlobInner {}

impl Drop for BlobInner {
    fn drop(&mut self) {
        if let Some(destroy) = self.user_destroy {
            // HarfBuzz fires the destroy callback exactly once,
            // when the last reference is released. Match the
            // contract.
            // SAFETY: caller-supplied function pointer; the contract
            // is that it accepts `user_data` and runs to completion.
            unsafe { destroy(self.user_data) };
        }
    }
}

/// Opaque, refcounted handle to a byte buffer, usually font data.
/// Mirrors HarfBuzz's `hb_blob_t`.
#[repr(C)]
pub struct hb_blob_t {
    pub(crate) inner: Arc<BlobInner>,
}

/// The face is a parsed SFNT directory plus the bytes it borrows
/// from. The `Face<'static>` is a lie: its borrow is actually
/// rooted in `_data`'s payload, which lives at least as long as the
/// FaceInner. See the module-level lifetime erasure note.
///
/// Fields drop in declaration order, so `face` goes before the bytes
/// it borrows.
pub(crate) struct FaceInner {
    pub(crate) face: Face<'static>,
    _data: Arc<Vec<u8>>,
}

impl FaceInner {
    /// Internal: build a FaceInner from an Arc'd byte buffer plus a
    /// lifetime-erased `Face<'static>` already constructed against
    /// the same bytes. Callers (the subset bridge) do the
    /// `transmute::<Face<'_>, Face<'static>>` themselves so this
    /// helper stays unsafe-free. Only the `subset` cargo feature
    /// uses this constructor today.
    #[cfg(feature = "subset")]
    pub(crate) fn from_arc(data: Arc<Vec<u8>>, face: Face<'static>) -> Self {
        Self { face, _data: data }
    }
}

// SAFETY: Face<'_> is Clone + Send + Sync (it holds &[u8] + Vec<TableRecord>).
// The 'static lifetime is fictitious; the actual backing storage is
// `_data`, which is itself Send + Sync via Arc<Vec<u8>>. As long as
// no thread observes the face after `_data` drops (which can't
// happen because they're held in the same struct), the bound holds.
unsafe impl Send for FaceInner {}
// SAFETY: see the `Send` impl above. Shared access only reads the
// immutable face and bytes.
unsafe impl Sync for FaceInner {}

/// Opaque, refcounted handle to a parsed font face. Mirrors
/// HarfBuzz's `hb_face_t`.
#[repr(C)]
pub struct hb_face_t {
    pub(crate) inner: Arc<FaceInner>,
}

impl hb_face_t {
    /// Internal: build the public wrapper around an existing
    /// `FaceInner` Arc. Used by the subset bridge to ship the result
    /// of `sigilbuzz_subset::subset()` back as an `hb_face_t*`.
    #[cfg(feature = "subset")]
    pub(crate) fn from_inner(inner: Arc<FaceInner>) -> Self {
        Self { inner }
    }
}

/// Font binds a face to a render size and (optionally) variation
/// coords. We own the coords here so the `Font<'static>` view
/// remains valid; mutation goes through `Mutex` because HarfBuzz's
/// `hb_font_set_*` functions accept a non-const pointer and we
/// expose the same surface. Most callers configure the font once
/// before shaping, so contention is negligible.
pub(crate) struct FontInner {
    pub(crate) state: spin_mutex::SpinMutex<FontState>,
    /// Lifetime root for `state.font`, which holds a `Font<'static>`
    /// borrowed from this Arc (see the SAFETY note below). Declared
    /// after `state` so the font drops before the face.
    pub(crate) face: Arc<FaceInner>,
}

struct FontState {
    /// Mirror of `Font::size()`. Every setter rebuilds `font` from
    /// these fields through `build_font`.
    x_scale: i32,
    y_scale: i32,
    /// Declared before `coords` so it drops before the slice it
    /// borrows.
    font: Font<'static>,
    /// Owned coords. `font` borrows these; mutating the vec
    /// invalidates the borrow, so any setter rebuilds the font.
    coords: Vec<f32>,
}

// SAFETY: Font<'_> is Clone + Send + Sync; the lifetime erasure is
// rooted in `face._data`. See FaceInner SAFETY note.
unsafe impl Send for FontInner {}
// SAFETY: see the `Send` impl above. The mutable state sits behind
// `SpinMutex`, which serializes access.
unsafe impl Sync for FontInner {}

/// Opaque, refcounted handle to a face bound to a scale and
/// variation coordinates. Mirrors HarfBuzz's `hb_font_t`.
#[repr(C)]
pub struct hb_font_t {
    pub(crate) inner: Arc<FontInner>,
}

/// The shaping buffer: text in, glyphs out. HarfBuzz makes
/// `hb_buffer_t` mutable (the caller pushes text into it), so we
/// wrap a `Mutex` around the inner state.
struct BufferInner {
    state: spin_mutex::SpinMutex<BufferState>,
}

struct BufferState {
    buffer: Buffer,
    direction: hb_direction_t,
    script: hb_script_t,
    language: hb_language_t,
    /// Cached output of the most recent `hb_shape` call. The
    /// `hb_buffer_get_glyph_infos` / `_positions` accessors hand
    /// these slices back. They survive until the next `hb_shape`,
    /// `hb_buffer_reset`, or `hb_buffer_clear_contents`.
    glyph_infos: Vec<hb_glyph_info_t>,
    glyph_positions: Vec<hb_glyph_position_t>,
}

/// Opaque, refcounted shaping buffer: text in, glyphs out. Mirrors
/// HarfBuzz's `hb_buffer_t`.
#[repr(C)]
pub struct hb_buffer_t {
    inner: Arc<BufferInner>,
}

// SAFETY: `BufferState` carries a `*const c_char` (`language`) that
// is only stored and compared, never dereferenced by this crate. The
// pointers this crate hands out point into the leaked language
// intern table, which lives for the process lifetime. The other
// fields (Buffer, Vec<...>) are already Send + Sync.
unsafe impl Send for BufferInner {}
// SAFETY: see the `Send` impl above. The state sits behind
// `SpinMutex`, which serializes access.
unsafe impl Sync for BufferInner {}

// ---------------------------------------------------------------------------
// Tiny spin mutex so we stay no_std-friendly without pulling in libstd.
// ---------------------------------------------------------------------------

mod spin_mutex {
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

    // SAFETY: SpinMutex owns its `T`, so moving it to another thread
    // moves the `T`, which is fine for `T: Send`.
    unsafe impl<T: Send> Send for SpinMutex<T> {}
    // SAFETY: SpinMutex serializes access to `inner`. The AtomicBool
    // is the only cross-thread observable. As with `std::sync::Mutex`,
    // handing out `&mut T` on another thread only needs `T: Send`.
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
            // SAFETY: the guard exists only while `locked` is true and
            // this guard set it, so no other reference to `inner` is
            // live. The returned borrow cannot outlive the guard.
            unsafe { &*self.mutex.inner.get() }
        }
    }

    impl<T> DerefMut for SpinGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            // SAFETY: as in `deref`. `&mut self` also rules out a
            // second borrow through this same guard.
            unsafe { &mut *self.mutex.inner.get() }
        }
    }

    impl<T> Drop for SpinGuard<'_, T> {
        fn drop(&mut self) {
            self.mutex.locked.store(false, Ordering::Release);
        }
    }
}

// ---------------------------------------------------------------------------
// HarfBuzz primitive types and enum constants
// ---------------------------------------------------------------------------

/// HarfBuzz's `hb_bool_t` is an `int`. 0 == false, non-zero == true.
pub type hb_bool_t = c_int;

/// HarfBuzz codepoints / glyph ids are 32-bit unsigned.
pub type hb_codepoint_t = u32;

/// Tag = four ASCII characters packed BE into a u32 (`'L','a','t','n'` ->
/// `0x4C61746E`). Match HarfBuzz's HB_TAG macro.
pub type hb_tag_t = u32;

/// Buffer cluster: `u32`, just an opaque tag.
pub type hb_mask_t = u32;

/// HarfBuzz's signed 16.16 position type for advances and offsets.
pub type hb_position_t = i32;

/// HarfBuzz's destroy callback signature.
pub type hb_destroy_func_t = unsafe extern "C" fn(*mut c_void);

/// HarfBuzz memory mode. sigilbuzz copies the bytes in every mode, so
/// the mode only decides when `hb_blob_create` calls the destroy
/// callback. `HB_MEMORY_MODE_DUPLICATE` calls it before returning, as
/// HarfBuzz does once it has made its copy. Every other mode calls it
/// when the last reference to the blob is released.
pub type hb_memory_mode_t = c_uint;
/// The library copies the bytes. HarfBuzz value 0.
pub const HB_MEMORY_MODE_DUPLICATE: hb_memory_mode_t = 0;
/// The caller's bytes are read-only. HarfBuzz value 1.
pub const HB_MEMORY_MODE_READONLY: hb_memory_mode_t = 1;
/// The caller's bytes may be written in place. HarfBuzz value 2.
pub const HB_MEMORY_MODE_WRITABLE: hb_memory_mode_t = 2;
/// Read-only bytes that the library may copy to write. HarfBuzz
/// value 3.
pub const HB_MEMORY_MODE_READONLY_MAY_MAKE_WRITABLE: hb_memory_mode_t = 3;

/// HarfBuzz direction enum. Values match `hb-common.h` exactly:
/// LTR=4, RTL=5, TTB=6, BTT=7, INVALID=0.
pub type hb_direction_t = c_uint;
/// Direction not set.
pub const HB_DIRECTION_INVALID: hb_direction_t = 0;
/// Left to right.
pub const HB_DIRECTION_LTR: hb_direction_t = 4;
/// Right to left.
pub const HB_DIRECTION_RTL: hb_direction_t = 5;
/// Top to bottom.
pub const HB_DIRECTION_TTB: hb_direction_t = 6;
/// Bottom to top.
pub const HB_DIRECTION_BTT: hb_direction_t = 7;

/// HarfBuzz script enum: alias for `hb_tag_t`, value is the
/// ISO 15924 four-letter code packed via HB_TAG.
pub type hb_script_t = hb_tag_t;
/// Script not set.
pub const HB_SCRIPT_INVALID: hb_script_t = 0;
/// ISO 15924 `Zyyy`, characters shared by many scripts.
pub const HB_SCRIPT_COMMON: hb_script_t = tag(b"Zyyy");
/// ISO 15924 `Zinh`, marks that take the script of their base.
pub const HB_SCRIPT_INHERITED: hb_script_t = tag(b"Zinh");
/// ISO 15924 `Latn`.
pub const HB_SCRIPT_LATIN: hb_script_t = tag(b"Latn");
/// ISO 15924 `Grek`.
pub const HB_SCRIPT_GREEK: hb_script_t = tag(b"Grek");
/// ISO 15924 `Cyrl`.
pub const HB_SCRIPT_CYRILLIC: hb_script_t = tag(b"Cyrl");
/// ISO 15924 `Arab`.
pub const HB_SCRIPT_ARABIC: hb_script_t = tag(b"Arab");
/// ISO 15924 `Hebr`.
pub const HB_SCRIPT_HEBREW: hb_script_t = tag(b"Hebr");
/// ISO 15924 `Deva`.
pub const HB_SCRIPT_DEVANAGARI: hb_script_t = tag(b"Deva");
/// ISO 15924 `Beng`.
pub const HB_SCRIPT_BENGALI: hb_script_t = tag(b"Beng");
/// ISO 15924 `Hani`.
pub const HB_SCRIPT_HAN: hb_script_t = tag(b"Hani");
/// ISO 15924 `Hang`.
pub const HB_SCRIPT_HANGUL: hb_script_t = tag(b"Hang");
/// ISO 15924 `Khmr`.
pub const HB_SCRIPT_KHMER: hb_script_t = tag(b"Khmr");
/// ISO 15924 `Mymr`.
pub const HB_SCRIPT_MYANMAR: hb_script_t = tag(b"Mymr");
/// ISO 15924 `Thai`.
pub const HB_SCRIPT_THAI: hb_script_t = tag(b"Thai");
/// ISO 15924 `Laoo`.
pub const HB_SCRIPT_LAO: hb_script_t = tag(b"Laoo");

/// Languages are interned `&'static str` pointers. We hand back a
/// `*const c_char` whose backing storage is a leaked `CString`,
/// matching HarfBuzz's "string is owned by the library" contract.
/// HarfBuzz callers never free a language pointer.
pub type hb_language_t = *const c_char;

const fn tag(s: &[u8; 4]) -> hb_tag_t {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

/// One glyph in the shaped output, in HarfBuzz layout. The
/// `mask`/`var1`/`var2` slots exist so binaries compiled against
/// HarfBuzz's struct layout don't observe a size mismatch.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct hb_glyph_info_t {
    /// Before shaping, a Unicode codepoint. After shaping, a glyph id.
    pub codepoint: hb_codepoint_t,
    /// Glyph flags. Always 0 in this implementation.
    pub mask: hb_mask_t,
    /// Index of the input cluster this glyph belongs to.
    pub cluster: u32,
    /// Private slot, kept for layout compatibility. Always 0.
    pub var1: u32,
    /// Private slot, kept for layout compatibility. Always 0.
    pub var2: u32,
}

/// One positioned glyph. Layout matches HarfBuzz's struct: four
/// position deltas plus a `var` slot.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct hb_glyph_position_t {
    /// Horizontal pen advance after this glyph.
    pub x_advance: hb_position_t,
    /// Vertical pen advance after this glyph.
    pub y_advance: hb_position_t,
    /// Horizontal offset of the glyph from the pen position.
    pub x_offset: hb_position_t,
    /// Vertical offset of the glyph from the pen position.
    pub y_offset: hb_position_t,
    /// Private slot, kept for layout compatibility. Always 0.
    pub var: u32,
}

/// One feature override. Matches HarfBuzz's `hb_feature_t` layout.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct hb_feature_t {
    /// OpenType feature tag, such as `liga`.
    pub tag: hb_tag_t,
    /// Feature value. 0 turns the feature off, 1 turns it on, and
    /// larger values pick an alternate.
    pub value: u32,
    /// First cluster the override applies to. Not used by this
    /// implementation, which applies overrides to the whole buffer.
    pub start: c_uint,
    /// One past the last cluster the override applies to. Not used
    /// by this implementation.
    pub end: c_uint,
}

/// One variation-axis override. Layout matches HarfBuzz.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct hb_variation_t {
    /// Axis tag, such as `wght`.
    pub tag: hb_tag_t,
    /// Axis value in user-space units.
    pub value: f32,
}

// ---------------------------------------------------------------------------
// Blob
// ---------------------------------------------------------------------------

/// Empty / null sentinel returned in error paths. Matches HarfBuzz's
/// "always return a valid pointer; callers may pass a null in to
/// destroy and it's a no-op" contract.
fn empty_blob() -> *mut hb_blob_t {
    let inner = Arc::new(BlobInner {
        data: Arc::new(Vec::new()),
        user_destroy: None,
        user_data: ptr::null_mut(),
    });
    Box::into_raw(Box::new(hb_blob_t { inner }))
}

/// Creates a blob holding a copy of `length` bytes at `data`.
///
/// `destroy`, when non-null, is called exactly once with `user_data`.
/// As in HarfBuzz, that happens before this function returns when
/// `mode` is `HB_MEMORY_MODE_DUPLICATE` or when there are no bytes to
/// hold (`length == 0` or a null `data`). Otherwise it happens when
/// the blob's last reference is released.
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
            // SAFETY: caller-supplied function pointer. The contract
            // is that it accepts `user_data`. The empty blob does not
            // keep `user_data`, so this is its only call.
            unsafe { destroy(user_data) };
        }
        return empty_blob();
    }
    // SAFETY: `data` is non-null and the caller guarantees it points
    // to `length` readable bytes that stay valid for this call. The
    // bytes are copied before returning.
    let bytes = unsafe { slice::from_raw_parts(data.cast::<u8>(), length as usize) };
    let owned = bytes.to_vec();
    let user_destroy = if mode == HB_MEMORY_MODE_DUPLICATE {
        if let Some(destroy) = destroy {
            // SAFETY: as above. The blob holds its own copy of the
            // bytes and does not keep `user_data`.
            unsafe { destroy(user_data) };
        }
        None
    } else {
        destroy
    };
    let inner = Arc::new(BlobInner {
        data: Arc::new(owned),
        user_destroy,
        user_data,
    });
    Box::into_raw(Box::new(hb_blob_t { inner }))
}

/// # Safety
/// `file_name` must be a valid NUL-terminated UTF-8 path.
#[cfg(feature = "std")]
#[no_mangle]
pub unsafe extern "C" fn hb_blob_create_from_file(file_name: *const c_char) -> *mut hb_blob_t {
    if file_name.is_null() {
        return empty_blob();
    }
    // SAFETY: `file_name` is non-null and the caller guarantees it
    // points to a NUL-terminated string.
    let path_cstr = unsafe { core::ffi::CStr::from_ptr(file_name) };
    let Ok(path_str) = path_cstr.to_str() else {
        return empty_blob();
    };
    let Ok(bytes) = std::fs::read(path_str) else {
        return empty_blob();
    };
    // Blob lengths cross the C boundary as `unsigned int`. Refuse a
    // file whose length would not fit rather than report a truncated
    // length.
    if c_uint::try_from(bytes.len()).is_err() {
        return empty_blob();
    }
    let inner = Arc::new(BlobInner {
        data: Arc::new(bytes),
        user_destroy: None,
        user_data: ptr::null_mut(),
    });
    Box::into_raw(Box::new(hb_blob_t { inner }))
}

/// # Safety
/// `blob` must be null or a pointer previously returned by an
/// `hb_blob_*` constructor.
#[no_mangle]
pub unsafe extern "C" fn hb_blob_destroy(blob: *mut hb_blob_t) {
    if blob.is_null() {
        return;
    }
    // SAFETY: `blob` is non-null and the caller guarantees it came
    // from `Box::into_raw` in an `hb_blob_*` constructor and has not
    // been destroyed yet, so reclaiming the box is sound.
    drop(unsafe { Box::from_raw(blob) });
}

/// # Safety
/// `blob` must be null or a valid `hb_blob_t*`.
#[no_mangle]
pub unsafe extern "C" fn hb_blob_reference(blob: *mut hb_blob_t) -> *mut hb_blob_t {
    if blob.is_null() {
        return empty_blob();
    }
    // SAFETY: `blob` is non-null and the caller guarantees it points
    // to a live `hb_blob_t`.
    let inner = unsafe { (*blob).inner.clone() };
    Box::into_raw(Box::new(hb_blob_t { inner }))
}

/// # Safety
/// `blob` must be null or valid. `length` may be null.
#[no_mangle]
pub unsafe extern "C" fn hb_blob_get_data(
    blob: *mut hb_blob_t,
    length: *mut c_uint,
) -> *const c_char {
    if blob.is_null() {
        if !length.is_null() {
            // SAFETY: `length` is non-null and the caller guarantees
            // it points to a writable `unsigned int`.
            unsafe { *length = 0 };
        }
        return ptr::null();
    }
    // SAFETY: `blob` is non-null and the caller guarantees it points
    // to a live `hb_blob_t`.
    let inner: &Arc<BlobInner> = unsafe { &(*blob).inner };
    let bytes: &[u8] = inner.data.as_slice();
    if !length.is_null() {
        // SAFETY: `length` is non-null and the caller guarantees it
        // points to a writable `unsigned int`.
        unsafe { *length = bytes.len() as c_uint };
    }
    bytes.as_ptr().cast::<c_char>()
}

/// # Safety
/// `blob` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_blob_get_length(blob: *mut hb_blob_t) -> c_uint {
    if blob.is_null() {
        return 0;
    }
    // SAFETY: `blob` is non-null and the caller guarantees it points
    // to a live `hb_blob_t`.
    let inner: &Arc<BlobInner> = unsafe { &(*blob).inner };
    inner.data.len() as c_uint
}

// ---------------------------------------------------------------------------
// Face
// ---------------------------------------------------------------------------

/// Builds the payload of an empty face: a TrueType header with zero
/// tables. Every table lookup on it misses. Returns `None` only if
/// the core parser rejects that header.
fn empty_face_inner() -> Option<Arc<FaceInner>> {
    static EMPTY_SFNT: [u8; 12] = [
        0x00, 0x01, 0x00, 0x00, // sfntVersion = TrueType
        0x00, 0x00, // numTables = 0
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // searchRange/entrySelector/rangeShift
    ];
    // The header is a `static`, so the parsed face is already
    // `'static` and needs no lifetime erasure.
    let face: Face<'static> = Face::parse_bytes(&EMPTY_SFNT, 0).ok()?;
    Some(Arc::new(FaceInner {
        face,
        _data: Arc::new(Vec::new()),
    }))
}

/// Returns a new handle to an empty face, the neutral result for a
/// null or unparsable input. Falls back to NULL, which every entry
/// point accepts, if the empty face cannot be built.
fn empty_face() -> *mut hb_face_t {
    empty_face_inner().map_or(ptr::null_mut(), |inner| {
        Box::into_raw(Box::new(hb_face_t { inner }))
    })
}

/// Constructs a face from the bytes in `blob` at index `index`.
///
/// # Safety
/// `blob` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_face_create(blob: *mut hb_blob_t, index: c_uint) -> *mut hb_face_t {
    if blob.is_null() {
        return empty_face();
    }
    // SAFETY: `blob` is non-null and the caller guarantees it points
    // to a live `hb_blob_t`.
    let blob_inner = unsafe { (*blob).inner.clone() };
    let bytes_arc: Arc<Vec<u8>> = blob_inner.data.clone();
    let bytes_slice: &[u8] = bytes_arc.as_slice();
    let face = match Face::parse_bytes(bytes_slice, index) {
        Ok(f) => f,
        Err(_) => return empty_face(),
    };
    // SAFETY: `face` borrows the heap buffer behind `bytes_arc`. The
    // FaceInner built below owns a clone of that Arc, so the buffer
    // does not move or drop while the face exists, and `face` drops
    // first because of the field order in `FaceInner`.
    let face_static: Face<'static> =
        unsafe { core::mem::transmute::<Face<'_>, Face<'static>>(face) };
    let inner = Arc::new(FaceInner {
        face: face_static,
        _data: bytes_arc,
    });
    Box::into_raw(Box::new(hb_face_t { inner }))
}

/// # Safety
/// `face` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_face_destroy(face: *mut hb_face_t) {
    if face.is_null() {
        return;
    }
    // SAFETY: `face` is non-null and the caller guarantees it came
    // from `Box::into_raw` in an `hb_face_*` constructor and has not
    // been destroyed yet.
    drop(unsafe { Box::from_raw(face) });
}

/// # Safety
/// `face` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_face_reference(face: *mut hb_face_t) -> *mut hb_face_t {
    if face.is_null() {
        return empty_face();
    }
    // SAFETY: `face` is non-null and the caller guarantees it points
    // to a live `hb_face_t`.
    let inner = unsafe { (*face).inner.clone() };
    Box::into_raw(Box::new(hb_face_t { inner }))
}

/// # Safety
/// `face` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_face_get_glyph_count(face: *mut hb_face_t) -> c_uint {
    if face.is_null() {
        return 0;
    }
    // SAFETY: `face` is non-null and the caller guarantees it points
    // to a live `hb_face_t`.
    let inner: &Arc<FaceInner> = unsafe { &(*face).inner };
    inner
        .face
        .maxp()
        .map(|m| u32::from(m.num_glyphs))
        .unwrap_or(0)
}

/// # Safety
/// `face` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_face_get_upem(face: *mut hb_face_t) -> c_uint {
    if face.is_null() {
        return 0;
    }
    // SAFETY: `face` is non-null and the caller guarantees it points
    // to a live `hb_face_t`.
    let inner: &Arc<FaceInner> = unsafe { &(*face).inner };
    inner
        .face
        .head()
        .map(|h| u32::from(h.units_per_em))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Font
// ---------------------------------------------------------------------------

/// The face's units per em, or 1000 when the face has no readable
/// `head` table (the empty face, for one). HarfBuzz falls back to the
/// same value.
fn face_upem(face_inner: &FaceInner) -> i32 {
    face_inner
        .face
        .head()
        .map_or(1000, |h| i32::from(h.units_per_em))
}

/// HarfBuzz's 16.16 multiplier from design units to a font scale:
/// `scale * 65536 / upem`, truncated toward zero.
fn em_mult(scale: i32, upem: i32) -> i64 {
    i64::from(scale) * 65536 / i64::from(upem.max(1))
}

/// Scales a design-unit value by a multiplier from [`em_mult`] and
/// rounds half up, the same arithmetic as HarfBuzz's `em_mult`. The
/// result saturates at the `hb_position_t` range.
fn em_scale(v: i32, mult: i64) -> hb_position_t {
    let scaled = (i128::from(v) * i128::from(mult) + 32768) >> 16;
    scaled.clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as hb_position_t
}

/// Internal: build the FontState's Font from coords and size.
/// sigilbuzz's `Font` carries a single size, so `x_scale` feeds it.
/// The shaper emits design units whatever the size, so `hb_shape_full`
/// applies the scale to its output.
///
/// # Safety
/// `coords` must stay alive and in place for as long as the returned
/// font is used. Callers pass `FontState::coords` (or an empty slice)
/// and store the result in `FontState::font`, which drops first.
unsafe fn build_font(face_inner: &Arc<FaceInner>, x_scale: i32, coords: &[f32]) -> Font<'static> {
    let face = face_inner.face.clone();
    let font = Font::new(face, x_scale as f32);
    if coords.is_empty() {
        font
    } else {
        // SAFETY: the caller keeps `coords` alive and unmoved for the
        // lifetime of the returned font. See this function's contract.
        let coords_static: &'static [f32] =
            unsafe { core::mem::transmute::<&[f32], &'static [f32]>(coords) };
        font.with_coords(coords_static)
    }
}

/// # Safety
/// `face` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_font_create(face: *mut hb_face_t) -> *mut hb_font_t {
    let face_inner = if face.is_null() {
        // Build an empty font around the empty face. Callers that
        // shape against this get an empty buffer back.
        let Some(inner) = empty_face_inner() else {
            return ptr::null_mut();
        };
        inner
    } else {
        // SAFETY: `face` is non-null and the caller guarantees it
        // points to a live `hb_face_t`.
        unsafe { (*face).inner.clone() }
    };
    // Default x_scale / y_scale follow HarfBuzz: they default to
    // upem so an unscaled font produces design-unit output.
    let upem_signed = face_upem(&face_inner);
    // SAFETY: an empty coords slice is never borrowed by the font.
    let font = unsafe { build_font(&face_inner, upem_signed, &[]) };
    let state = FontState {
        x_scale: upem_signed,
        y_scale: upem_signed,
        font,
        coords: Vec::new(),
    };
    let inner = Arc::new(FontInner {
        state: spin_mutex::SpinMutex::new(state),
        face: face_inner,
    });
    Box::into_raw(Box::new(hb_font_t { inner }))
}

/// # Safety
/// `font` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_font_destroy(font: *mut hb_font_t) {
    if font.is_null() {
        return;
    }
    // SAFETY: `font` is non-null and the caller guarantees it came
    // from `Box::into_raw` in an `hb_font_*` constructor and has not
    // been destroyed yet.
    drop(unsafe { Box::from_raw(font) });
}

/// # Safety
/// `font` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_font_reference(font: *mut hb_font_t) -> *mut hb_font_t {
    if font.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `font` is non-null and the caller guarantees it points
    // to a live `hb_font_t`.
    let inner = unsafe { (*font).inner.clone() };
    Box::into_raw(Box::new(hb_font_t { inner }))
}

/// Sets the scale `hb_shape` reports positions in. A value of `upem`
/// (the default) gives design units. `x_scale` scales horizontal
/// advances and offsets, and `y_scale` scales vertical ones, as in
/// HarfBuzz.
///
/// HarfBuzz scales each advance and each positioning adjustment
/// before it adds them. sigilbuzz shapes in design units and scales
/// the sums, so at a scale that is not a whole multiple of the upem a
/// position can differ from HarfBuzz's by rounding.
///
/// # Safety
/// `font` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_font_set_scale(font: *mut hb_font_t, x_scale: c_int, y_scale: c_int) {
    if font.is_null() {
        return;
    }
    // SAFETY: `font` is non-null and the caller guarantees it points
    // to a live `hb_font_t`.
    let inner = unsafe { &(*font).inner };
    let mut state = inner.state.lock();
    state.x_scale = x_scale;
    state.y_scale = y_scale;
    // SAFETY: the new font borrows `state.coords`, which is not
    // touched again until a later setter rebuilds the font. The font
    // is stored next to the coords and drops before them.
    state.font = unsafe { build_font(&inner.face, x_scale, &state.coords) };
}

/// # Safety
/// `font` must be null or valid. `x_scale`/`y_scale` may be null.
#[no_mangle]
pub unsafe extern "C" fn hb_font_get_scale(
    font: *mut hb_font_t,
    x_scale: *mut c_int,
    y_scale: *mut c_int,
) {
    if font.is_null() {
        return;
    }
    // SAFETY: `font` is non-null and the caller guarantees it points
    // to a live `hb_font_t`.
    let inner = unsafe { &(*font).inner };
    let state = inner.state.lock();
    if !x_scale.is_null() {
        // SAFETY: `x_scale` is non-null and the caller guarantees it
        // points to a writable `int`.
        unsafe { *x_scale = state.x_scale };
    }
    if !y_scale.is_null() {
        // SAFETY: `y_scale` is non-null and the caller guarantees it
        // points to a writable `int`.
        unsafe { *y_scale = state.y_scale };
    }
}

/// Accepted so HarfBuzz callers link. It has no effect.
///
/// HarfBuzz uses the pixels-per-em values for hinting adjustments:
/// the ppem-specific deltas in GPOS Device tables, and the bitmap
/// strike it measures glyph extents from. sigilbuzz applies neither,
/// so the values would change nothing and are not stored.
///
/// # Safety
/// Any arguments are accepted. None are dereferenced.
#[no_mangle]
pub unsafe extern "C" fn hb_font_set_ppem(_font: *mut hb_font_t, _x_ppem: c_uint, _y_ppem: c_uint) {
}

/// # Safety
/// `font` must be null or valid. `(variations, length)` must describe
/// a valid `hb_variation_t[]` slice when `variations` is non-null.
#[no_mangle]
pub unsafe extern "C" fn hb_font_set_variations(
    font: *mut hb_font_t,
    variations: *const hb_variation_t,
    variations_length: c_uint,
) {
    if font.is_null() {
        return;
    }
    // SAFETY: `font` is non-null and the caller guarantees it points
    // to a live `hb_font_t`.
    let inner = unsafe { &(*font).inner };
    let mut state = inner.state.lock();
    let vars: &[hb_variation_t] = if variations.is_null() || variations_length == 0 {
        &[]
    } else {
        // SAFETY: `variations` is non-null and the caller guarantees
        // it points to `variations_length` readable records.
        unsafe { slice::from_raw_parts(variations, variations_length as usize) }
    };
    // Resolve user-space axis values through fvar / avar to
    // normalized coords, the format Font expects.
    let face = &inner.face.face;
    let coords = match (face.fvar(), face.avar()) {
        (Ok(Some(fvar)), avar_res) => {
            // Build a user-space vector: one entry per fvar axis,
            // initialized to the axis default; then overlay any
            // `hb_variation_t` whose tag matches.
            let mut user: Vec<f32> = fvar.axes().iter().map(|a| a.default_value).collect();
            for v in vars {
                let slot = fvar
                    .axes()
                    .iter()
                    .position(|a| u32::from_be_bytes(a.tag) == v.tag)
                    .and_then(|idx| user.get_mut(idx));
                if let Some(slot) = slot {
                    *slot = v.value;
                }
            }
            let normalised = fvar.normalize_coords(&user);
            match avar_res {
                Ok(Some(avar)) => avar.remap_all(&normalised),
                _ => normalised,
            }
        }
        _ => Vec::new(),
    };
    // `state.font` borrows `state.coords`, so the font must be rebuilt
    // every time the coords change.
    state.coords = coords;
    // SAFETY: the new font borrows `state.coords`, which is not
    // touched again until a later setter rebuilds the font. The font
    // is stored next to the coords and drops before them.
    state.font = unsafe { build_font(&inner.face, state.x_scale, &state.coords) };
}

// ---------------------------------------------------------------------------
// Buffer
// ---------------------------------------------------------------------------

/// Allocates an empty buffer with refcount 1. Direction, script, and
/// language start unset.
#[no_mangle]
pub extern "C" fn hb_buffer_create() -> *mut hb_buffer_t {
    let state = BufferState {
        buffer: Buffer::new(),
        direction: HB_DIRECTION_INVALID,
        script: HB_SCRIPT_INVALID,
        language: ptr::null(),
        glyph_infos: Vec::new(),
        glyph_positions: Vec::new(),
    };
    let inner = Arc::new(BufferInner {
        state: spin_mutex::SpinMutex::new(state),
    });
    Box::into_raw(Box::new(hb_buffer_t { inner }))
}

/// # Safety
/// `buffer` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_destroy(buffer: *mut hb_buffer_t) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it came
    // from `Box::into_raw` in an `hb_buffer_*` constructor and has not
    // been destroyed yet.
    drop(unsafe { Box::from_raw(buffer) });
}

/// # Safety
/// `buffer` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_reference(buffer: *mut hb_buffer_t) -> *mut hb_buffer_t {
    if buffer.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { (*buffer).inner.clone() };
    Box::into_raw(Box::new(hb_buffer_t { inner }))
}

/// # Safety
/// `buffer` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_reset(buffer: *mut hb_buffer_t) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    state.buffer.clear();
    state.direction = HB_DIRECTION_INVALID;
    state.script = HB_SCRIPT_INVALID;
    state.language = ptr::null();
    state.glyph_infos.clear();
    state.glyph_positions.clear();
}

/// # Safety
/// `buffer` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_clear_contents(buffer: *mut hb_buffer_t) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    state.buffer.set_text("");
    state.glyph_infos.clear();
    state.glyph_positions.clear();
    // direction/script/language survive a clear_contents: only
    // hb_buffer_reset wipes them.
}

/// Returns the half-open range `[start, end)` of an item inside a text
/// of `total` units, or `None` when `item_offset` is past the end. A
/// negative `item_length` means "to the end of the text". The end is
/// clamped to `total`.
fn item_range(total: usize, item_offset: c_uint, item_length: c_int) -> Option<(usize, usize)> {
    let start = usize::try_from(item_offset).ok()?;
    if start > total {
        return None;
    }
    let end = match usize::try_from(item_length) {
        Ok(len) => start.saturating_add(len).min(total),
        Err(_) => total,
    };
    Some((start, end))
}

/// # Safety
/// `buffer` must be null or valid. `text` must point to at least
/// `text_length` bytes (when `text_length >= 0`) or to a NUL-terminated
/// string (when `text_length == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_add_utf8(
    buffer: *mut hb_buffer_t,
    text: *const c_char,
    text_length: c_int,
    item_offset: c_uint,
    item_length: c_int,
) {
    if buffer.is_null() || text.is_null() {
        return;
    }
    // SAFETY: `text` is non-null and the caller guarantees the length
    // contract of `c_str_bytes`. A negative length means
    // NUL-terminated.
    let total_bytes = unsafe { c_str_bytes(text, text_length) };
    let Some((start, end)) = item_range(total_bytes.len(), item_offset, item_length) else {
        return;
    };
    let Some(item) = total_bytes.get(start..end) else {
        return;
    };
    let Ok(s) = core::str::from_utf8(item) else {
        return;
    };
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    state.buffer.push_str(s);
}

/// # Safety
/// `buffer` must be null or valid. `(text, text_length)` must describe a
/// valid `u16[]` slice (or NUL-terminated u16 array if `text_length == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_add_utf16(
    buffer: *mut hb_buffer_t,
    text: *const u16,
    text_length: c_int,
    item_offset: c_uint,
    item_length: c_int,
) {
    if buffer.is_null() || text.is_null() {
        return;
    }
    let total_units: &[u16] = if let Ok(len) = usize::try_from(text_length) {
        // SAFETY: `text` is non-null and the caller guarantees it
        // points to `len` readable, aligned `u16` units.
        unsafe { slice::from_raw_parts(text, len) }
    } else {
        // Walk to the NUL.
        let mut len = 0usize;
        // SAFETY: the caller guarantees a NUL-terminated array, so
        // every unit up to and including the NUL is readable.
        while unsafe { *text.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: the loop above read `len` units before the NUL, so
        // all of them are readable.
        unsafe { slice::from_raw_parts(text, len) }
    };
    let Some((start, end)) = item_range(total_units.len(), item_offset, item_length) else {
        return;
    };
    let Some(units) = total_units.get(start..end) else {
        return;
    };
    let Ok(s) = String::from_utf16(units) else {
        return;
    };
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    state.buffer.push_str(&s);
}

/// # Safety
/// `buffer` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_set_direction(
    buffer: *mut hb_buffer_t,
    direction: hb_direction_t,
) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    state.direction = direction;
    state.buffer.set_direction(map_direction_in(direction));
}

/// # Safety
/// `buffer` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_set_script(buffer: *mut hb_buffer_t, script: hb_script_t) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    state.script = script;
}

/// # Safety
/// `buffer` must be null or valid. `language` is stored as an opaque
/// value and never dereferenced.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_set_language(buffer: *mut hb_buffer_t, language: hb_language_t) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    state.language = language;
}

/// # Safety
/// `buffer` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_guess_segment_properties(buffer: *mut hb_buffer_t) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    if state.direction == HB_DIRECTION_INVALID {
        // Default to LTR; sigilbuzz's Buffer also defaults to LTR.
        state.direction = HB_DIRECTION_LTR;
        state.buffer.set_direction(Direction::Ltr);
    }
    if state.script == HB_SCRIPT_INVALID {
        // Use the first script-bearing codepoint to seed the script
        // tag, matching HarfBuzz's behavior. sigilbuzz's
        // `script_runs()` does the heavy lifting; we project its
        // first run's script into the matching ISO 15924 tag.
        let runs = state.buffer.script_runs();
        let chosen = runs.first().map(|r| r.script);
        state.script = chosen.map(script_to_iso15924).unwrap_or(HB_SCRIPT_COMMON);
    }
    if state.language.is_null() {
        // HarfBuzz uses the host locale here; pick "und" as a safe
        // default that lookups always have to fall back through.
        state.language = lang_und();
    }
}

/// # Safety
/// `buffer` must be null or valid. `length` may be null.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_get_glyph_infos(
    buffer: *mut hb_buffer_t,
    length: *mut c_uint,
) -> *mut hb_glyph_info_t {
    if buffer.is_null() {
        if !length.is_null() {
            // SAFETY: `length` is non-null and the caller guarantees
            // it points to a writable `unsigned int`.
            unsafe { *length = 0 };
        }
        return ptr::null_mut();
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    let len = state.glyph_infos.len();
    if !length.is_null() {
        // SAFETY: `length` is non-null and the caller guarantees it
        // points to a writable `unsigned int`.
        unsafe { *length = len as c_uint };
    }
    // The vector lives inside the locked BufferState; the pointer
    // is valid until the next mutation. HarfBuzz's contract is the
    // same: the pointer lives until `hb_shape` runs again or the
    // buffer is reset.
    state.glyph_infos.as_mut_ptr()
}

/// # Safety
/// `buffer` must be null or valid. `length` may be null.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_get_glyph_positions(
    buffer: *mut hb_buffer_t,
    length: *mut c_uint,
) -> *mut hb_glyph_position_t {
    if buffer.is_null() {
        if !length.is_null() {
            // SAFETY: `length` is non-null and the caller guarantees
            // it points to a writable `unsigned int`.
            unsafe { *length = 0 };
        }
        return ptr::null_mut();
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    let len = state.glyph_positions.len();
    if !length.is_null() {
        // SAFETY: `length` is non-null and the caller guarantees it
        // points to a writable `unsigned int`.
        unsafe { *length = len as c_uint };
    }
    state.glyph_positions.as_mut_ptr()
}

/// # Safety
/// `buffer` must be null or valid.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_get_length(buffer: *mut hb_buffer_t) -> c_uint {
    if buffer.is_null() {
        return 0;
    }
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let state = inner.state.lock();
    state.glyph_infos.len() as c_uint
}

// ---------------------------------------------------------------------------
// Shape
// ---------------------------------------------------------------------------

/// # Safety
/// `font` and `buffer` must be null or valid. `(features, num_features)`
/// must describe a valid `hb_feature_t[]` slice when `features` is
/// non-null.
#[no_mangle]
pub unsafe extern "C" fn hb_shape(
    font: *mut hb_font_t,
    buffer: *mut hb_buffer_t,
    features: *const hb_feature_t,
    num_features: c_uint,
) {
    // SAFETY: the caller upholds the contract of `hb_shape_full`,
    // which is this function's contract. A null shaper list is
    // always accepted.
    let _ = unsafe { hb_shape_full(font, buffer, features, num_features, ptr::null()) };
}

/// Shapes `buffer` with `font`, like `hb_shape`, using only the
/// shapers named in `shaper_list`.
///
/// sigilbuzz has one shaper, the OpenType shaper HarfBuzz calls `ot`.
/// A null `shaper_list` means the default list. A list that does not
/// name `ot` has no shaper sigilbuzz can run. The call then returns 0,
/// as HarfBuzz does when none of the requested shapers is available,
/// and the buffer holds no glyphs. An empty buffer returns 1 whatever
/// the list says, also as in HarfBuzz.
///
/// # Safety
/// See `hb_shape`. `shaper_list` must be null or point to an array
/// of NUL-terminated strings that ends with a null pointer.
#[no_mangle]
pub unsafe extern "C" fn hb_shape_full(
    font: *mut hb_font_t,
    buffer: *mut hb_buffer_t,
    features: *const hb_feature_t,
    num_features: c_uint,
    shaper_list: *const *const c_char,
) -> hb_bool_t {
    if font.is_null() || buffer.is_null() {
        return 0;
    }
    // SAFETY: `font` is non-null and the caller guarantees it points
    // to a live `hb_font_t`.
    let font_inner = unsafe { &(*font).inner };
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let buffer_inner = unsafe { &(*buffer).inner };

    // SAFETY: the caller guarantees `shaper_list` is null or a
    // null-terminated array of C strings.
    if !unsafe { shaper_list_names_ot(shaper_list) } {
        let mut buffer_state = buffer_inner.state.lock();
        if buffer_state.buffer.is_empty() {
            return 1;
        }
        buffer_state.glyph_infos.clear();
        buffer_state.glyph_positions.clear();
        return 0;
    }

    // Build the feature list.
    let raw_features: &[hb_feature_t] = if features.is_null() || num_features == 0 {
        &[]
    } else {
        // SAFETY: `features` is non-null and the caller guarantees it
        // points to `num_features` readable records.
        unsafe { slice::from_raw_parts(features, num_features as usize) }
    };
    let sigil_features: Vec<Feature> = raw_features
        .iter()
        .map(|f| Feature {
            tag: f.tag.to_be_bytes(),
            value: f.value,
        })
        .collect();

    // Lock both. Order: font first, then buffer, deterministic so
    // two threads shaping with the same pair never deadlock.
    let font_state = font_inner.state.lock();
    let mut buffer_state = buffer_inner.state.lock();

    // Drive sigilbuzz.
    let result = shape(&font_state.font, &buffer_state.buffer, &sigil_features);
    let shaped = match result {
        Ok(s) => s,
        Err(_) => {
            buffer_state.glyph_infos.clear();
            buffer_state.glyph_positions.clear();
            return 0;
        }
    };

    // The shaper works in design units. Scale to the font's
    // `hb_font_set_scale` values, as HarfBuzz reports positions.
    let upem = face_upem(&font_inner.face);
    let x_mult = em_mult(font_state.x_scale, upem);
    let y_mult = em_mult(font_state.y_scale, upem);

    // Project sigilbuzz Glyph stream into HarfBuzz's
    // (info, position) split.
    let mut infos = Vec::with_capacity(shaped.glyphs.len());
    let mut positions = Vec::with_capacity(shaped.glyphs.len());
    for g in &shaped.glyphs {
        infos.push(hb_glyph_info_t {
            codepoint: g.glyph_id,
            mask: 0,
            cluster: g.cluster,
            var1: 0,
            var2: 0,
        });
        positions.push(hb_glyph_position_t {
            x_advance: em_scale(g.x_advance, x_mult),
            y_advance: em_scale(g.y_advance, y_mult),
            x_offset: em_scale(g.x_offset, x_mult),
            y_offset: em_scale(g.y_offset, y_mult),
            var: 0,
        });
    }
    buffer_state.glyph_infos = infos;
    buffer_state.glyph_positions = positions;
    1
}

// ---------------------------------------------------------------------------
// Tag / Direction / Script / Language helpers
// ---------------------------------------------------------------------------

/// Reads a C string argument as bytes. A negative `len` means the
/// string is NUL-terminated.
///
/// # Safety
/// `s` must be non-null. When `len >= 0` it must point to `len`
/// readable bytes, otherwise to a NUL-terminated string. The bytes
/// must stay valid and unmodified for `'a`.
unsafe fn c_str_bytes<'a>(s: *const c_char, len: c_int) -> &'a [u8] {
    match usize::try_from(len) {
        // SAFETY: the caller guarantees `len` readable bytes at `s`.
        Ok(len) => unsafe { slice::from_raw_parts(s.cast::<u8>(), len) },
        // SAFETY: the caller guarantees a NUL-terminated string at `s`
        // when `len` is negative.
        Err(_) => unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes(),
    }
}

/// True when `shaper_list` is null (the default list) or names the
/// `ot` shaper.
///
/// # Safety
/// `shaper_list` must be null or point to an array of NUL-terminated
/// strings that ends with a null pointer.
unsafe fn shaper_list_names_ot(shaper_list: *const *const c_char) -> bool {
    if shaper_list.is_null() {
        return true;
    }
    let mut i = 0usize;
    loop {
        // SAFETY: the caller guarantees a null-terminated array, and
        // the loop stops at the terminator, so index `i` is in bounds.
        let name = unsafe { *shaper_list.add(i) };
        if name.is_null() {
            return false;
        }
        // SAFETY: every entry before the terminator is a
        // NUL-terminated string, per the caller's contract.
        if unsafe { core::ffi::CStr::from_ptr(name) }.to_bytes() == b"ot" {
            return true;
        }
        i += 1;
    }
}

/// # Safety
/// `s` must be null, or point to at least `len` bytes (or be
/// NUL-terminated when `len == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_tag_from_string(s: *const c_char, len: c_int) -> hb_tag_t {
    if s.is_null() {
        return 0;
    }
    // SAFETY: `s` is non-null and the caller guarantees the length
    // contract of `c_str_bytes`.
    let bytes = unsafe { c_str_bytes(s, len) };
    let mut buf = [b' '; 4];
    for (dst, src) in buf.iter_mut().zip(bytes) {
        *dst = *src;
    }
    u32::from_be_bytes(buf)
}

/// Writes the four ASCII bytes of `tag` into `buf`. `buf` must point
/// to at least four writable bytes. HarfBuzz's `hb_tag_to_string`
/// signature does not include a length argument; the caller is
/// expected to size the buffer.
///
/// # Safety
/// `buf` must be null or writeable for at least four bytes.
#[no_mangle]
pub unsafe extern "C" fn hb_tag_to_string(tag: hb_tag_t, buf: *mut c_char) {
    if buf.is_null() {
        return;
    }
    let bytes = tag.to_be_bytes();
    for (i, b) in bytes.iter().enumerate() {
        // SAFETY: `buf` is non-null and the caller guarantees four
        // writable bytes. `i` is below 4.
        unsafe { *buf.add(i) = *b as c_char };
    }
}

/// # Safety
/// `s` must be null, or point to at least `len` bytes (or be
/// NUL-terminated when `len == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_direction_from_string(s: *const c_char, len: c_int) -> hb_direction_t {
    if s.is_null() {
        return HB_DIRECTION_INVALID;
    }
    // SAFETY: `s` is non-null and the caller guarantees the length
    // contract of `c_str_bytes`.
    let bytes = unsafe { c_str_bytes(s, len) };
    // HarfBuzz only inspects the first character (case-insensitive).
    let Some(first) = bytes.first() else {
        return HB_DIRECTION_INVALID;
    };
    match first.to_ascii_lowercase() {
        b'l' => HB_DIRECTION_LTR,
        b'r' => HB_DIRECTION_RTL,
        b't' => HB_DIRECTION_TTB,
        b'b' => HB_DIRECTION_BTT,
        _ => HB_DIRECTION_INVALID,
    }
}

/// HarfBuzz's `hb_script_from_iso15924_tag` is a passthrough: the
/// tag IS the script identifier.
#[no_mangle]
pub extern "C" fn hb_script_from_iso15924_tag(tag: hb_tag_t) -> hb_script_t {
    tag
}

/// Languages are pointer-interned. We leak a CString the first time
/// we see a given normalized language tag; subsequent lookups return
/// the same pointer. As in C, the tag ends at the first NUL byte even
/// when `len` counts past it.
///
/// # Safety
/// `s` must be null, or point to at least `len` bytes (or be
/// NUL-terminated when `len == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_language_from_string(s: *const c_char, len: c_int) -> hb_language_t {
    if s.is_null() {
        return ptr::null();
    }
    // SAFETY: `s` is non-null and the caller guarantees the length
    // contract of `c_str_bytes`.
    let bytes = unsafe { c_str_bytes(s, len) };
    let bytes = bytes.split(|&b| b == 0).next().unwrap_or_default();
    if bytes.is_empty() {
        return ptr::null();
    }
    let normalised: String = bytes
        .iter()
        .map(|b| b.to_ascii_lowercase() as char)
        .collect();
    intern_language(&normalised)
}

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

/// HarfBuzz ABI version sigilbuzz advertises. We pick 8.0.0, the
/// current stable major as of 2025, so consumers that gate on
/// `hb_version_atleast(8, 0, 0)` succeed. The actual sigilbuzz
/// version is exposed via `hb_version_string()`.
const HB_COMPAT_MAJOR: c_uint = 8;
const HB_COMPAT_MINOR: c_uint = 0;
const HB_COMPAT_MICRO: c_uint = 0;

/// # Safety
/// All pointers, when non-null, must be writeable.
#[no_mangle]
pub unsafe extern "C" fn hb_version(major: *mut c_uint, minor: *mut c_uint, micro: *mut c_uint) {
    if !major.is_null() {
        // SAFETY: `major` is non-null and the caller guarantees it
        // points to a writable `unsigned int`.
        unsafe { *major = HB_COMPAT_MAJOR };
    }
    if !minor.is_null() {
        // SAFETY: `minor` is non-null and the caller guarantees it
        // points to a writable `unsigned int`.
        unsafe { *minor = HB_COMPAT_MINOR };
    }
    if !micro.is_null() {
        // SAFETY: `micro` is non-null and the caller guarantees it
        // points to a writable `unsigned int`.
        unsafe { *micro = HB_COMPAT_MICRO };
    }
}

/// Returns a static, NUL-terminated string identifying the
/// implementation. The pointer is valid for the lifetime of the
/// process.
#[no_mangle]
pub extern "C" fn hb_version_string() -> *const c_char {
    // Compose at compile time.
    // Format: "sigilbuzz X.Y.Z (hb-compatible)"
    static VERSION: &[u8] = concat!(
        "sigilbuzz ",
        env!("CARGO_PKG_VERSION"),
        " (hb-compatible)\0"
    )
    .as_bytes();
    VERSION.as_ptr().cast::<c_char>()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn map_direction_in(d: hb_direction_t) -> Direction {
    match d {
        HB_DIRECTION_RTL => Direction::Rtl,
        HB_DIRECTION_TTB => Direction::Ttb,
        HB_DIRECTION_BTT => Direction::Btt,
        _ => Direction::Ltr,
    }
}

fn script_to_iso15924(s: sigilbuzz::unicode::Script) -> hb_script_t {
    use sigilbuzz::unicode::Script;
    match s {
        Script::Latin => HB_SCRIPT_LATIN,
        Script::Han => HB_SCRIPT_HAN,
        Script::Arabic => HB_SCRIPT_ARABIC,
        Script::Hebrew => HB_SCRIPT_HEBREW,
        Script::Cyrillic => HB_SCRIPT_CYRILLIC,
        Script::Greek => HB_SCRIPT_GREEK,
        Script::Devanagari => HB_SCRIPT_DEVANAGARI,
        Script::Bengali => HB_SCRIPT_BENGALI,
        Script::Hangul => HB_SCRIPT_HANGUL,
        Script::Khmer => HB_SCRIPT_KHMER,
        Script::Myanmar => HB_SCRIPT_MYANMAR,
        Script::Thai => HB_SCRIPT_THAI,
        Script::Lao => HB_SCRIPT_LAO,
        _ => HB_SCRIPT_COMMON,
    }
}

// Language interning. Each distinct tag is leaked once into a
// Mutex<Vec<&'static CStr>> and looked up by value afterward.

#[cfg(feature = "std")]
fn intern_language(tag: &str) -> hb_language_t {
    use std::sync::{Mutex, OnceLock, PoisonError};
    static INTERN: OnceLock<Mutex<Vec<&'static core::ffi::CStr>>> = OnceLock::new();
    let intern = INTERN.get_or_init(|| Mutex::new(Vec::new()));
    // The table stays consistent even if a holder panicked: entries
    // are only ever appended whole.
    let mut guard = intern.lock().unwrap_or_else(PoisonError::into_inner);
    for cstr in guard.iter() {
        if cstr.to_bytes() == tag.as_bytes() {
            return cstr.as_ptr();
        }
    }
    // A tag with a NUL byte would be stored under a different key
    // than it is looked up by, and leak again on every call. The
    // caller strips NUL bytes, so this only guards the invariant.
    let Ok(owned) = std::ffi::CString::new(tag) else {
        return lang_und();
    };
    // Leak a fresh CString: language tags survive the lifetime of
    // the process, just as they do in HarfBuzz itself.
    let leaked: &'static core::ffi::CStr = Box::leak(owned.into_boxed_c_str());
    guard.push(leaked);
    leaked.as_ptr()
}

#[cfg(not(feature = "std"))]
fn intern_language(_tag: &str) -> hb_language_t {
    // Without std we cannot maintain a runtime intern table; return
    // the canonical "und" so consumers get a stable, non-null
    // pointer. Real applications that care about language-tag
    // dispatch link with std on.
    lang_und()
}

fn lang_und() -> hb_language_t {
    static UND: &[u8] = b"und\0";
    UND.as_ptr().cast::<c_char>()
}

// ---------------------------------------------------------------------------
// Tests: Rust-side equivalence harness
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sigilbuzz::{
        shape as sb_shape, Blob as SbBlob, Buffer as SbBuffer, Face as SbFace, Font as SbFont,
    };

    const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

    /// Pipe "Hello" through the C surface and through the native
    /// Rust API; assert the glyph stream is byte-identical.
    #[test]
    fn shape_hello_matches_rust_api() {
        // C surface.
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            let blob = hb_blob_create(
                OPEN_SANS.as_ptr().cast::<c_char>(),
                OPEN_SANS.len() as c_uint,
                HB_MEMORY_MODE_READONLY,
                ptr::null_mut(),
                None,
            );
            assert!(!blob.is_null());
            let face = hb_face_create(blob, 0);
            assert!(!face.is_null());
            let upem = hb_face_get_upem(face);
            assert!(upem > 0);
            let font = hb_font_create(face);
            assert!(!font.is_null());
            // Default scale is upem; preserve to stay in design units.
            let buffer = hb_buffer_create();
            hb_buffer_add_utf8(buffer, c"Hello".as_ptr(), -1, 0, -1);
            hb_buffer_set_direction(buffer, HB_DIRECTION_LTR);
            hb_buffer_set_script(buffer, HB_SCRIPT_LATIN);

            let ok = hb_shape_full(font, buffer, ptr::null(), 0, ptr::null());
            assert_eq!(ok, 1);

            let mut len: c_uint = 0;
            let infos = hb_buffer_get_glyph_infos(buffer, &mut len);
            assert!(
                len >= 5,
                "expected at least 5 glyphs for 'Hello', got {len}"
            );
            assert!(!infos.is_null());
            let infos_slice = slice::from_raw_parts(infos, len as usize);
            let positions = hb_buffer_get_glyph_positions(buffer, &mut len);
            let positions_slice = slice::from_raw_parts(positions, len as usize);

            // Rust surface.
            let rust_blob = SbBlob::new(OPEN_SANS);
            let rust_face = SbFace::parse(&rust_blob, 0).unwrap();
            let rust_font = SbFont::new(rust_face, upem as f32);
            let mut rust_buffer = SbBuffer::new();
            rust_buffer.push_str("Hello");
            let rust_glyphs = sb_shape(&rust_font, &rust_buffer, &[]).unwrap().glyphs;

            assert_eq!(infos_slice.len(), rust_glyphs.len());
            for (c, r) in infos_slice.iter().zip(rust_glyphs.iter()) {
                assert_eq!(c.codepoint, r.glyph_id);
                assert_eq!(c.cluster, r.cluster);
            }
            for (c, r) in positions_slice.iter().zip(rust_glyphs.iter()) {
                assert_eq!(c.x_advance, r.x_advance);
                assert_eq!(c.y_advance, r.y_advance);
                assert_eq!(c.x_offset, r.x_offset);
                assert_eq!(c.y_offset, r.y_offset);
            }

            hb_buffer_destroy(buffer);
            hb_font_destroy(font);
            hb_face_destroy(face);
            hb_blob_destroy(blob);
        }
    }

    #[test]
    fn refcount_keeps_face_alive_after_blob_destroy() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            let blob = hb_blob_create(
                OPEN_SANS.as_ptr().cast::<c_char>(),
                OPEN_SANS.len() as c_uint,
                HB_MEMORY_MODE_READONLY,
                ptr::null_mut(),
                None,
            );
            let face = hb_face_create(blob, 0);
            // Drop the blob handle first. Face must still respond.
            hb_blob_destroy(blob);
            let upem = hb_face_get_upem(face);
            assert!(upem > 0);
            hb_face_destroy(face);
        }
    }

    #[test]
    fn null_destroy_is_a_noop() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            hb_blob_destroy(ptr::null_mut());
            hb_face_destroy(ptr::null_mut());
            hb_font_destroy(ptr::null_mut());
            hb_buffer_destroy(ptr::null_mut());
        }
    }

    #[test]
    fn tag_round_trips() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            let t = hb_tag_from_string(c"Latn".as_ptr(), -1);
            assert_eq!(t, HB_SCRIPT_LATIN);
            let mut buf = [0i8; 4];
            hb_tag_to_string(t, buf.as_mut_ptr());
            let bytes: [u8; 4] = [buf[0] as u8, buf[1] as u8, buf[2] as u8, buf[3] as u8];
            assert_eq!(&bytes, b"Latn");
        }
    }

    #[test]
    fn direction_constants_match_harfbuzz_spec() {
        // HarfBuzz wire values.
        assert_eq!(HB_DIRECTION_INVALID, 0);
        assert_eq!(HB_DIRECTION_LTR, 4);
        assert_eq!(HB_DIRECTION_RTL, 5);
        assert_eq!(HB_DIRECTION_TTB, 6);
        assert_eq!(HB_DIRECTION_BTT, 7);
    }

    #[test]
    fn script_constants_match_iso15924_packing() {
        // Latn = 0x4C 61 74 6E.
        assert_eq!(HB_SCRIPT_LATIN, 0x4C61_746E);
        assert_eq!(HB_SCRIPT_ARABIC, 0x4172_6162);
    }

    #[test]
    fn version_string_identifies_sigilbuzz() {
        let p = hb_version_string();
        assert!(!p.is_null());
        // SAFETY: hb_version_string returns a static NUL-terminated
        // string.
        let s = unsafe { core::ffi::CStr::from_ptr(p) }.to_string_lossy();
        assert!(s.contains("sigilbuzz"));
        assert!(s.contains("hb-compatible"));
    }

    #[test]
    fn version_advertises_hb_compat_eight() {
        let mut major: c_uint = 0;
        let mut minor: c_uint = 0;
        let mut micro: c_uint = 0;
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe { hb_version(&mut major, &mut minor, &mut micro) };
        assert_eq!(major, 8);
        assert_eq!(minor, 0);
        assert_eq!(micro, 0);
    }

    #[test]
    fn buffer_guess_segment_properties_seeds_latin_for_ascii() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            let buffer = hb_buffer_create();
            hb_buffer_add_utf8(buffer, c"Hello".as_ptr(), -1, 0, -1);
            hb_buffer_guess_segment_properties(buffer);
            // direction defaults to LTR; script should be Latin.
            // Read direction and script back via the state mutex.
            let inner = &(*buffer).inner;
            let st = inner.state.lock();
            assert_eq!(st.direction, HB_DIRECTION_LTR);
            assert_eq!(st.script, HB_SCRIPT_LATIN);
            drop(st);
            hb_buffer_destroy(buffer);
        }
    }

    #[test]
    fn utf16_input_matches_utf8() {
        // SAFETY: every pointer passed here is null or a live handle
        // created in this test, and each handle is destroyed once.
        unsafe {
            // "Hi" in UTF-16.
            let utf16: [u16; 2] = [b'H' as u16, b'i' as u16];
            let buf16 = hb_buffer_create();
            hb_buffer_add_utf16(buf16, utf16.as_ptr(), 2, 0, -1);
            let inner16 = &(*buf16).inner;
            let st16 = inner16.state.lock();
            assert_eq!(st16.buffer.text(), "Hi");
            drop(st16);
            hb_buffer_destroy(buf16);
        }
    }
}
