//! The refcounted handle types C holds (`hb_blob_t`, `hb_face_t`,
//! `hb_font_t`, `hb_buffer_t`) and the state behind each.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

use sigilbuzz::{Buffer, Face, Font};

use crate::{
    buffer_flags, buffer_text, hb_destroy_func_t, hb_direction_t, hb_glyph_info_t,
    hb_glyph_position_t, hb_language_t, hb_script_t, spin_mutex,
};

// ---------------------------------------------------------------------------
// Refcounted opaque types
// ---------------------------------------------------------------------------

/// Owned font bytes plus the user-data destroy callback HarfBuzz
/// callers can hang off a blob. The destroy callback fires when
/// the last reference to the blob goes away.
pub(crate) struct BlobInner {
    /// The actual font bytes. Never resized after construction, so
    /// faces built on the blob can borrow the heap buffer for as long
    /// as they hold a reference to the blob.
    pub(crate) data: Vec<u8>,
    /// Optional caller-supplied destroy callback: fires once, when
    /// the BlobInner is dropped. HarfBuzz's `hb_blob_create` accepts
    /// `mode`, `user_data`, and `destroy` so callers passing
    /// `HB_MEMORY_MODE_READONLY` can use mmap'd buffers and have
    /// the destroy callback unmap on drop.
    pub(crate) user_destroy: Option<hb_destroy_func_t>,
    /// Opaque user_data threaded into the destroy callback. Send
    /// only because the inner is shipped across threads via Arc.
    pub(crate) user_data: *mut c_void,
}

impl BlobInner {
    /// Internal: build a `BlobInner` around bytes the crate produced
    /// itself. No destroy callback: the blob owns the bytes outright.
    pub(crate) fn from_data(data: Vec<u8>) -> Self {
        Self {
            data,
            user_destroy: None,
            user_data: ptr::null_mut(),
        }
    }
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
/// Mirrors HarfBuzz's `hb_blob_t`. C holds the `Arc` pointer to this
/// struct, as the `handle` module describes.
#[repr(C)]
pub struct hb_blob_t {
    pub(crate) inner: BlobInner,
}

/// The face is a parsed SFNT directory plus the blob it borrows
/// from. The `Face<'static>` is a lie: its borrow is actually
/// rooted in `_blob`'s bytes, which live at least as long as the
/// FaceInner. See the module-level lifetime erasure note.
///
/// Fields drop in declaration order, so `face` goes before the bytes
/// it borrows.
pub(crate) struct FaceInner {
    pub(crate) face: Face<'static>,
    /// The blob the face was built from. HarfBuzz faces reference
    /// their blob too, so the blob's destroy callback fires only once
    /// every face (and font) built on it is gone.
    _blob: Arc<hb_blob_t>,
}

impl FaceInner {
    /// Internal: build a FaceInner from a blob plus a lifetime-erased
    /// `Face<'static>` already constructed against the blob's bytes.
    /// Callers do the `transmute::<Face<'_>, Face<'static>>`
    /// themselves so this helper stays unsafe-free.
    pub(crate) fn from_blob(blob: Arc<hb_blob_t>, face: Face<'static>) -> Self {
        Self { face, _blob: blob }
    }
}

// SAFETY: Face<'_> is Clone + Send + Sync (it holds &[u8] + Vec<TableRecord>).
// The 'static lifetime is fictitious; the actual backing storage is
// `_blob`'s byte buffer, which is Send + Sync behind its Arc. As long
// as no thread observes the face after `_blob` drops (which can't
// happen because they're held in the same struct), the bound holds.
unsafe impl Send for FaceInner {}
// SAFETY: see the `Send` impl above. Shared access only reads the
// immutable face and bytes.
unsafe impl Sync for FaceInner {}

/// Opaque, refcounted handle to a parsed font face. Mirrors
/// HarfBuzz's `hb_face_t`. C holds the `Arc` pointer to this struct,
/// as the `handle` module describes.
#[repr(C)]
pub struct hb_face_t {
    pub(crate) inner: FaceInner,
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
    /// borrowed from this face (see the SAFETY note below). It is a
    /// reference to the same face object C sees, so a font keeps its
    /// face alive the way HarfBuzz fonts do. Declared after `state` so
    /// the font drops before the face.
    pub(crate) face: Arc<hb_face_t>,
}

pub(crate) struct FontState {
    /// Mirror of `Font::size()`. Every setter rebuilds `font` from
    /// these fields through `build_font`.
    pub(crate) x_scale: i32,
    pub(crate) y_scale: i32,
    /// Declared before `coords` so it drops before the slice it
    /// borrows.
    pub(crate) font: Font<'static>,
    /// Owned coords. `font` borrows these; mutating the vec
    /// invalidates the borrow, so any setter rebuilds the font.
    pub(crate) coords: Vec<f32>,
}

// SAFETY: Font<'_> is Clone + Send + Sync; the lifetime erasure is
// rooted in `face`, which keeps the face (and its blob) alive. See
// the FaceInner SAFETY note.
unsafe impl Send for FontInner {}
// SAFETY: see the `Send` impl above. The mutable state sits behind
// `SpinMutex`, which serializes access.
unsafe impl Sync for FontInner {}

/// Opaque, refcounted handle to a face bound to a scale and
/// variation coordinates. Mirrors HarfBuzz's `hb_font_t`. C holds the
/// `Arc` pointer to this struct, as the `handle` module describes.
#[repr(C)]
pub struct hb_font_t {
    pub(crate) inner: FontInner,
}

/// The shaping buffer: text in, glyphs out. HarfBuzz makes
/// `hb_buffer_t` mutable (the caller pushes text into it), so we
/// wrap a `Mutex` around the inner state.
pub(crate) struct BufferInner {
    pub(crate) state: spin_mutex::SpinMutex<BufferState>,
}

pub(crate) struct BufferState {
    pub(crate) buffer: Buffer,
    pub(crate) direction: hb_direction_t,
    pub(crate) script: hb_script_t,
    pub(crate) language: hb_language_t,
    /// Cached output of the most recent `hb_shape` call. The
    /// `hb_buffer_get_glyph_infos` / `_positions` accessors hand
    /// these slices back. They survive until the next `hb_shape`,
    /// `hb_buffer_reset`, or `hb_buffer_clear_contents`.
    pub(crate) glyph_infos: Vec<hb_glyph_info_t>,
    pub(crate) glyph_positions: Vec<hb_glyph_position_t>,
    /// Caller-unit cluster for every character added so far, as the
    /// `buffer_text` module describes.
    pub(crate) clusters: buffer_text::ClusterTable,
    /// The flags as `hb_buffer_set_flags` got them, bits sigilbuzz
    /// ignores included, so `hb_buffer_get_flags` returns them.
    pub(crate) flags: buffer_flags::hb_buffer_flags_t,
    /// The cluster level as `hb_buffer_set_cluster_level` got it.
    pub(crate) cluster_level: buffer_flags::hb_buffer_cluster_level_t,
}

/// Opaque, refcounted shaping buffer: text in, glyphs out. Mirrors
/// HarfBuzz's `hb_buffer_t`. C holds the `Arc` pointer to this struct,
/// as the `handle` module describes.
#[repr(C)]
pub struct hb_buffer_t {
    pub(crate) inner: BufferInner,
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
