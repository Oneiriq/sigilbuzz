//! Font functions: creating a font on a face, reference counting, and
//! the scale, ppem and variation setters.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{c_int, c_uint};
use core::slice;

use sigilbuzz::Font;

use crate::face::empty_face_arc;
use crate::opaque::{FontInner, FontState};
use crate::{handle, hb_face_t, hb_font_t, hb_variation_t, spin_mutex, FaceInner};

// ---------------------------------------------------------------------------
// Font
// ---------------------------------------------------------------------------

/// Internal: build the FontState's Font from coords and size.
fn build_font(
    face_inner: &FaceInner,
    x_scale: i32,
    _y_scale: i32,
    coords: &[f32],
) -> Font<'static> {
    // sigilbuzz Font carries a single size; mirror x_scale into it.
    // y_scale is preserved for hb_font_get_scale round-tripping.
    let face = face_inner.face.clone();
    let font = Font::new(face, x_scale as f32);
    if coords.is_empty() {
        font
    } else {
        // SAFETY: `coords` lives in the FontState alongside this
        // Font; the FontState owns both, so the borrow holds for
        // the same lifetime as the Font<'static> lie itself:
        // both are rooted in the FontInner's heap allocation.
        let coords_static: &'static [f32] =
            unsafe { core::mem::transmute::<&[f32], &'static [f32]>(coords) };
        font.with_coords(coords_static)
    }
}

/// Creates a font on `face`. The font holds a reference to `face`, so
/// the caller may destroy its own face reference right away. A null
/// face yields a font on a fresh empty face, as in HarfBuzz.
///
/// # Safety
/// `face` must be null or a live face.
#[no_mangle]
pub unsafe extern "C" fn hb_font_create(face: *mut hb_face_t) -> *mut hb_font_t {
    let face_ref: Arc<hb_face_t> = if face.is_null() {
        empty_face_arc()
    } else {
        // SAFETY: caller asserts `face` is a live handle.
        unsafe { handle::retain(face.cast_const()) }
    };
    // Default x_scale / y_scale follow HarfBuzz: they default to
    // upem so an unscaled font produces design-unit output. A face
    // without a usable `head` (including the empty face) uses 1000.
    let upem_signed = i32::from(
        face_ref
            .inner
            .face
            .head()
            .map(|h| h.units_per_em)
            .unwrap_or(1000),
    );
    let coords: Vec<f32> = Vec::new();
    let font = build_font(&face_ref.inner, upem_signed, upem_signed, &coords);
    let state = FontState {
        x_scale: upem_signed,
        y_scale: upem_signed,
        x_ppem: 0,
        y_ppem: 0,
        coords,
        font,
    };
    handle::into_raw(hb_font_t {
        inner: FontInner {
            _face: face_ref,
            state: spin_mutex::SpinMutex::new(state),
        },
    })
}

/// Releases one reference to `font`. Null is a no-op.
///
/// # Safety
/// `font` must be null or a live font the caller holds a reference to.
#[no_mangle]
pub unsafe extern "C" fn hb_font_destroy(font: *mut hb_font_t) {
    // SAFETY: caller guarantees `font` is null or a live handle it owns
    // a reference to.
    unsafe { handle::destroy(font) };
}

/// Adds one reference to `font` and returns `font` itself. Null in,
/// null out.
///
/// # Safety
/// `font` must be null or a live font.
#[no_mangle]
pub unsafe extern "C" fn hb_font_reference(font: *mut hb_font_t) -> *mut hb_font_t {
    // SAFETY: caller guarantees `font` is null or a live handle.
    unsafe { handle::reference(font) }
}

/// # Safety
/// `font` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hb_font_set_scale(font: *mut hb_font_t, x_scale: c_int, y_scale: c_int) {
    if font.is_null() {
        return;
    }
    // SAFETY: caller asserts validity.
    let inner = unsafe { &(*font).inner };
    let mut state = inner.state.lock();
    state.x_scale = x_scale;
    state.y_scale = y_scale;
    // Rebuild Font borrowing from the canonical `state.coords`. See
    // `hb_font_set_variations` for the partial-borrow rationale.
    let coords_ptr: *const [f32] = core::ptr::from_ref::<[f32]>(state.coords.as_slice());
    // SAFETY: `state.coords` is heap-pinned for the duration of the
    // lock; the raw pointer is solely used to bypass Rust's
    // partial-borrow check on disjoint fields.
    let coords_ref: &[f32] = unsafe { &*coords_ptr };
    state.font = build_font(&inner._face.inner, x_scale, y_scale, coords_ref);
}

/// # Safety
/// `font` must be valid; `x_scale`/`y_scale` may be null.
#[no_mangle]
pub unsafe extern "C" fn hb_font_get_scale(
    font: *mut hb_font_t,
    x_scale: *mut c_int,
    y_scale: *mut c_int,
) {
    if font.is_null() {
        return;
    }
    // SAFETY: caller asserts validity.
    let inner = unsafe { &(*font).inner };
    let state = inner.state.lock();
    if !x_scale.is_null() {
        // SAFETY: caller asserts writeable.
        unsafe { *x_scale = state.x_scale };
    }
    if !y_scale.is_null() {
        // SAFETY: caller asserts writeable.
        unsafe { *y_scale = state.y_scale };
    }
}

/// # Safety
/// `font` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hb_font_set_ppem(font: *mut hb_font_t, x_ppem: c_uint, y_ppem: c_uint) {
    if font.is_null() {
        return;
    }
    // SAFETY: caller asserts validity.
    let inner = unsafe { &(*font).inner };
    let mut state = inner.state.lock();
    state.x_ppem = x_ppem;
    state.y_ppem = y_ppem;
}

/// # Safety
/// `font` must be valid; `(variations, length)` must describe a valid
/// `hb_variation_t[]` slice.
#[no_mangle]
pub unsafe extern "C" fn hb_font_set_variations(
    font: *mut hb_font_t,
    variations: *const hb_variation_t,
    variations_length: c_uint,
) {
    if font.is_null() {
        return;
    }
    // SAFETY: caller asserts validity.
    let inner = unsafe { &(*font).inner };
    let mut state = inner.state.lock();
    let vars: &[hb_variation_t] = if variations.is_null() || variations_length == 0 {
        &[]
    } else {
        // SAFETY: caller asserts (variations, length) is a valid slice.
        unsafe { slice::from_raw_parts(variations, variations_length as usize) }
    };
    // Resolve user-space axis values through fvar / avar to
    // normalized coords, the format Font expects.
    let face = &inner._face.inner.face;
    let coords = match (face.fvar(), face.avar()) {
        (Ok(Some(fvar)), avar_res) => {
            // Build a user-space vector: one entry per fvar axis,
            // initialized to the axis default; then overlay any
            // `hb_variation_t` whose tag matches.
            let mut user: Vec<f32> = fvar.axes().iter().map(|a| a.default_value).collect();
            for v in vars {
                if let Some(idx) = fvar
                    .axes()
                    .iter()
                    .position(|a| u32::from_be_bytes(a.tag) == v.tag)
                {
                    if idx < user.len() {
                        user[idx] = v.value;
                    }
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
    state.coords = coords;
    // `state.coords` is now the canonical owner. `state.font` borrows
    // from it via the transmute inside `build_font`; we must rebuild
    // `state.font` whenever `state.coords` changes: the realloc
    // could move the heap allocation and invalidate the borrow.
    let coords_ptr: *const [f32] = core::ptr::from_ref::<[f32]>(state.coords.as_slice());
    // SAFETY: `state.coords` is pinned to the FontState's heap
    // allocation for as long as `state` is locked; we are the sole
    // mutator. The pointer round-trips through a raw pointer to
    // sidestep the partial-borrow check: Rust forbids holding
    // `&state.coords` and `&mut state.font` simultaneously even
    // though the two fields don't overlap.
    let coords_ref: &[f32] = unsafe { &*coords_ptr };
    state.font = build_font(&inner._face.inner, state.x_scale, state.y_scale, coords_ref);
}
