//! Face functions: building a face on a blob, reference counting, and
//! the glyph-count and units-per-em queries.

use alloc::sync::Arc;
use core::ffi::c_uint;

use sigilbuzz::Face;

use crate::blob::empty_blob_arc;
use crate::{handle, hb_blob_t, hb_face_t, FaceInner};

// ---------------------------------------------------------------------------
// Face
// ---------------------------------------------------------------------------

/// Fresh empty face, used in error paths. Like [`empty_blob`](crate::blob::empty_blob), an
/// ordinary object the caller destroys as usual.
fn empty_face() -> *mut hb_face_t {
    handle::arc_into_raw(empty_face_arc())
}

pub(crate) fn empty_face_arc() -> Arc<hb_face_t> {
    // An empty face cannot be constructed via `Face::parse_bytes`.
    // Forge one by parsing a four-byte zero header and accepting
    // the error; emit a placeholder FaceInner whose face is a
    // throwaway. We never expose the internal face when num_tables
    // is queried because the `inner.face.num_tables() == 0` branch
    // always answers truthfully.
    //
    // The cleanest path is to lean on the same byte buffer as the
    // empty blob: parse a synthetic minimal header.
    static EMPTY_SFNT: [u8; 12] = [
        0x00, 0x01, 0x00, 0x00, // sfntVersion = TrueType
        0x00, 0x00, // numTables = 0
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // searchRange/entrySelector/rangeShift
    ];
    let face = match Face::parse_bytes(&EMPTY_SFNT, 0) {
        Ok(f) => f,
        Err(_) => unreachable!("synthetic empty SFNT must parse"),
    };
    // Lifetime-erase: the synthetic header is `'static`, so the
    // transmute is a no-op (it's already 'static).
    let face: Face<'static> = face;
    Arc::new(hb_face_t {
        inner: FaceInner::from_blob(empty_blob_arc(), face),
    })
}

/// Builds a face that references `blob` and borrows its bytes.
/// Returns `None` when the bytes do not parse at `index`.
pub(crate) fn face_from_blob(blob: Arc<hb_blob_t>, index: c_uint) -> Option<Arc<hb_face_t>> {
    let parsed = Face::parse_bytes(blob.inner.data.as_slice(), index).ok()?;
    // SAFETY: `parsed` borrows `blob.inner.data`, a heap buffer that is
    // never resized and lives as long as the blob. The FaceInner built
    // below holds a reference to that blob for its whole life, so the
    // erased `'static` borrow never outlives the bytes.
    let face_static: Face<'static> =
        unsafe { core::mem::transmute::<Face<'_>, Face<'static>>(parsed) };
    Some(Arc::new(hb_face_t {
        inner: FaceInner::from_blob(blob, face_static),
    }))
}

/// Constructs a face from the bytes in `blob` at index `index`. The
/// face holds a reference to `blob`, so the caller may destroy its own
/// blob reference right away. Bytes that do not parse yield an empty
/// face (still a new reference the caller must destroy).
///
/// # Safety
/// `blob` must be null or a live blob.
#[no_mangle]
pub unsafe extern "C" fn hb_face_create(blob: *mut hb_blob_t, index: c_uint) -> *mut hb_face_t {
    if blob.is_null() {
        return empty_face();
    }
    // SAFETY: caller asserts `blob` is a live handle.
    let blob = unsafe { handle::retain(blob.cast_const()) };
    match face_from_blob(blob, index) {
        Some(face) => handle::arc_into_raw(face),
        None => empty_face(),
    }
}

/// Releases one reference to `face`. Null is a no-op.
///
/// # Safety
/// `face` must be null or a live face the caller holds a reference to.
#[no_mangle]
pub unsafe extern "C" fn hb_face_destroy(face: *mut hb_face_t) {
    // SAFETY: caller guarantees `face` is null or a live handle it owns
    // a reference to.
    unsafe { handle::destroy(face) };
}

/// Adds one reference to `face` and returns `face` itself. Null in,
/// null out.
///
/// # Safety
/// `face` must be null or a live face.
#[no_mangle]
pub unsafe extern "C" fn hb_face_reference(face: *mut hb_face_t) -> *mut hb_face_t {
    // SAFETY: caller guarantees `face` is null or a live handle.
    unsafe { handle::reference(face) }
}

/// # Safety
/// `face` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hb_face_get_glyph_count(face: *mut hb_face_t) -> c_uint {
    if face.is_null() {
        return 0;
    }
    // SAFETY: caller asserts validity.
    let inner: &FaceInner = unsafe { &(*face).inner };
    inner
        .face
        .maxp()
        .map(|m| u32::from(m.num_glyphs))
        .unwrap_or(0)
}

/// # Safety
/// `face` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hb_face_get_upem(face: *mut hb_face_t) -> c_uint {
    if face.is_null() {
        return 0;
    }
    // SAFETY: caller asserts validity.
    let inner: &FaceInner = unsafe { &(*face).inner };
    inner
        .face
        .head()
        .map(|h| u32::from(h.units_per_em))
        .unwrap_or(0)
}
