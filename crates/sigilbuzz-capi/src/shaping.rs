//! Shaping: `hb_shape` and `hb_shape_full` run the core shaper on a
//! font and buffer and store the output in HarfBuzz's glyph layout.

use alloc::vec::Vec;
use core::ffi::{c_char, c_uint};
use core::ptr;
use core::slice;

use sigilbuzz::{shape, Feature};

use crate::common::shaper_list_names_ot;
use crate::font::{em_mult, em_scale, face_upem};
use crate::{
    hb_bool_t, hb_buffer_t, hb_feature_t, hb_font_t, hb_glyph_info_t, hb_glyph_position_t,
};

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
    let upem = face_upem(&font_inner.face.inner);
    let x_mult = em_mult(font_state.x_scale, upem);
    let y_mult = em_mult(font_state.y_scale, upem);

    // Project sigilbuzz Glyph stream into HarfBuzz's
    // (info, position) split.
    let mut infos = Vec::with_capacity(shaped.glyphs.len());
    let mut positions = Vec::with_capacity(shaped.glyphs.len());
    let text_len = buffer_state.buffer.text().len();
    for g in &shaped.glyphs {
        infos.push(hb_glyph_info_t {
            codepoint: g.glyph_id,
            mask: g.flags.bits(),
            // Core clusters are UTF-8 offsets into the buffer text;
            // report them in the units of the caller's add call.
            cluster: buffer_state.clusters.map(g.cluster, text_len),
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
