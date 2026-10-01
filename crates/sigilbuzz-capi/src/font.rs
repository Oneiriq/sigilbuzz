//! Font functions: creating a font on a face, reference counting, the
//! scale, ppem and variation setters, and the cmap glyph lookups.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{c_int, c_uint};
use core::{ptr, slice};

use sigilbuzz::Font;

use crate::face::empty_face_arc;
use crate::opaque::{FontInner, FontState};
use crate::{
    handle, hb_bool_t, hb_codepoint_t, hb_face_t, hb_font_t, hb_position_t, hb_variation_t,
    spin_mutex, FaceInner,
};

// ---------------------------------------------------------------------------
// Font
// ---------------------------------------------------------------------------

/// The face's units per em, or 1000 when the face has no readable
/// `head` table (the empty face, for one). HarfBuzz falls back to the
/// same value.
pub(crate) fn face_upem(face_inner: &FaceInner) -> i32 {
    face_inner
        .face
        .head()
        .map_or(1000, |h| i32::from(h.units_per_em))
}

/// HarfBuzz's 16.16 multiplier from design units to a font scale:
/// `scale * 65536 / upem`, truncated toward zero.
pub(crate) fn em_mult(scale: i32, upem: i32) -> i64 {
    i64::from(scale) * 65536 / i64::from(upem.max(1))
}

/// Scales a design-unit value by a multiplier from [`em_mult`] and
/// rounds half up, the same arithmetic as HarfBuzz's `em_mult`. The
/// result saturates at the `hb_position_t` range.
pub(crate) fn em_scale(v: i32, mult: i64) -> hb_position_t {
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
unsafe fn build_font(face_inner: &FaceInner, x_scale: i32, coords: &[f32]) -> Font<'static> {
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

/// Creates a font on `face`. The font holds a reference to `face`, so
/// the caller may destroy its own face reference right away. A null
/// face yields a font on a fresh empty face, as in HarfBuzz.
///
/// # Safety
/// `face` must be null or a live face.
#[no_mangle]
pub unsafe extern "C" fn hb_font_create(face: *mut hb_face_t) -> *mut hb_font_t {
    let face_ref: Arc<hb_face_t> = if face.is_null() {
        // Build an empty font around the empty face. Callers that
        // shape against this get an empty buffer back.
        let Some(empty) = empty_face_arc() else {
            return ptr::null_mut();
        };
        empty
    } else {
        // SAFETY: `face` is non-null and the caller guarantees it is a
        // live handle, so taking a new reference to it is sound.
        unsafe { handle::retain(face.cast_const()) }
    };
    // Default x_scale / y_scale follow HarfBuzz: they default to
    // upem so an unscaled font produces design-unit output. A face
    // without a usable `head` (including the empty face) uses 1000.
    let upem_signed = face_upem(&face_ref.inner);
    // SAFETY: an empty coords slice is never borrowed by the font.
    let font = unsafe { build_font(&face_ref.inner, upem_signed, &[]) };
    let state = FontState {
        x_scale: upem_signed,
        y_scale: upem_signed,
        font,
        coords: Vec::new(),
    };
    handle::into_raw(hb_font_t {
        inner: FontInner {
            state: spin_mutex::SpinMutex::new(state),
            face: face_ref,
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
    state.font = unsafe { build_font(&inner.face.inner, x_scale, &state.coords) };
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
    let face = &inner.face.inner.face;
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
    state.font = unsafe { build_font(&inner.face.inner, state.x_scale, &state.coords) };
}

/// The glyph `face` maps `unicode` to, or `None`.
fn nominal_glyph(face: &sigilbuzz::Face<'_>, unicode: hb_codepoint_t) -> Option<u16> {
    let ch = char::from_u32(unicode)?;
    face.cmap().ok()?.glyph_id(ch)
}

/// The glyph `face` maps the variation sequence to, or `None`.
fn variation_glyph(
    face: &sigilbuzz::Face<'_>,
    unicode: hb_codepoint_t,
    variation_selector: hb_codepoint_t,
) -> Option<u16> {
    let ch = char::from_u32(unicode)?;
    let selector = char::from_u32(variation_selector)?;
    face.cmap().ok()?.variation_glyph(ch, selector)
}

/// Writes `found` (or 0) to `glyph` when it is non-null and returns
/// whether a glyph was found. HarfBuzz stores 0 on a miss.
///
/// # Safety
/// `glyph` must be null or point to a writable `hb_codepoint_t`.
unsafe fn store_glyph(found: Option<u16>, glyph: *mut hb_codepoint_t) -> hb_bool_t {
    if !glyph.is_null() {
        // SAFETY: `glyph` is non-null and the caller guarantees it
        // points to a writable `hb_codepoint_t`.
        unsafe { *glyph = found.map_or(0, u32::from) };
    }
    hb_bool_t::from(found.is_some())
}

/// Looks `unicode` up in the font's cmap and stores its glyph in
/// `glyph`, as HarfBuzz's `hb_font_get_nominal_glyph` does. Returns 0
/// and stores 0 when the font does not map it (or `font` is null).
///
/// # Safety
/// `font` must be null or a live font. `glyph` must be null or point
/// to a writable `hb_codepoint_t`.
#[no_mangle]
pub unsafe extern "C" fn hb_font_get_nominal_glyph(
    font: *mut hb_font_t,
    unicode: hb_codepoint_t,
    glyph: *mut hb_codepoint_t,
) -> hb_bool_t {
    let found = if font.is_null() {
        None
    } else {
        // SAFETY: `font` is non-null and the caller guarantees it
        // points to a live `hb_font_t`.
        let inner = unsafe { &(*font).inner };
        nominal_glyph(&inner.face.inner.face, unicode)
    };
    // SAFETY: the caller guarantees `glyph` is null or writable.
    unsafe { store_glyph(found, glyph) }
}

/// Looks the variation sequence `unicode` followed by
/// `variation_selector` up in the font's cmap format 14 subtable and
/// stores its glyph in `glyph`, as HarfBuzz's
/// `hb_font_get_variation_glyph` does. A default sequence gives the
/// glyph of `unicode`. Returns 0 and stores 0 when the font does not
/// list the sequence (or `font` is null).
///
/// # Safety
/// `font` must be null or a live font. `glyph` must be null or point
/// to a writable `hb_codepoint_t`.
#[no_mangle]
pub unsafe extern "C" fn hb_font_get_variation_glyph(
    font: *mut hb_font_t,
    unicode: hb_codepoint_t,
    variation_selector: hb_codepoint_t,
    glyph: *mut hb_codepoint_t,
) -> hb_bool_t {
    let found = if font.is_null() {
        None
    } else {
        // SAFETY: `font` is non-null and the caller guarantees it
        // points to a live `hb_font_t`.
        let inner = unsafe { &(*font).inner };
        variation_glyph(&inner.face.inner.face, unicode, variation_selector)
    };
    // SAFETY: the caller guarantees `glyph` is null or writable.
    unsafe { store_glyph(found, glyph) }
}

/// `hb_font_get_variation_glyph` when `variation_selector` is nonzero,
/// `hb_font_get_nominal_glyph` otherwise, as in HarfBuzz.
///
/// # Safety
/// `font` must be null or a live font. `glyph` must be null or point
/// to a writable `hb_codepoint_t`.
#[no_mangle]
pub unsafe extern "C" fn hb_font_get_glyph(
    font: *mut hb_font_t,
    unicode: hb_codepoint_t,
    variation_selector: hb_codepoint_t,
    glyph: *mut hb_codepoint_t,
) -> hb_bool_t {
    if variation_selector != 0 {
        // SAFETY: the caller's guarantees are this function's.
        unsafe { hb_font_get_variation_glyph(font, unicode, variation_selector, glyph) }
    } else {
        // SAFETY: the caller's guarantees are this function's.
        unsafe { hb_font_get_nominal_glyph(font, unicode, glyph) }
    }
}
