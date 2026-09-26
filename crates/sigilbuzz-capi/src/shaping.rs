//! Shaping: `hb_shape` and `hb_shape_full` run the core shaper on a
//! font and buffer and store the output in HarfBuzz's glyph layout.

use alloc::vec::Vec;
use core::ffi::{c_char, c_uint};
use core::ptr;
use core::slice;

use sigilbuzz::{shape, Feature};

use crate::{
    hb_bool_t, hb_buffer_t, hb_feature_t, hb_font_t, hb_glyph_info_t, hb_glyph_position_t,
};

// ---------------------------------------------------------------------------
// Shape
// ---------------------------------------------------------------------------

/// # Safety
/// `font` and `buffer` must be valid; `(features, num_features)` must
/// describe a valid `hb_feature_t[]` slice (or both null/zero).
#[no_mangle]
pub unsafe extern "C" fn hb_shape(
    font: *mut hb_font_t,
    buffer: *mut hb_buffer_t,
    features: *const hb_feature_t,
    num_features: c_uint,
) {
    // SAFETY: caller asserts validity.
    let _ = unsafe { hb_shape_full(font, buffer, features, num_features, ptr::null()) };
}

/// # Safety
/// See `hb_shape`.
#[no_mangle]
pub unsafe extern "C" fn hb_shape_full(
    font: *mut hb_font_t,
    buffer: *mut hb_buffer_t,
    features: *const hb_feature_t,
    num_features: c_uint,
    _shaper_list: *const *const c_char,
) -> hb_bool_t {
    if font.is_null() || buffer.is_null() {
        return 0;
    }
    // SAFETY: caller asserts validity.
    let font_inner = unsafe { &(*font).inner };
    let buffer_inner = unsafe { &(*buffer).inner };

    // Build the feature list.
    let raw_features: &[hb_feature_t] = if features.is_null() || num_features == 0 {
        &[]
    } else {
        // SAFETY: caller asserts validity.
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

    // Project sigilbuzz Glyph stream into HarfBuzz's
    // (info, position) split.
    let mut infos = Vec::with_capacity(shaped.glyphs.len());
    let mut positions = Vec::with_capacity(shaped.glyphs.len());
    let text_len = buffer_state.buffer.text().len();
    for g in &shaped.glyphs {
        infos.push(hb_glyph_info_t {
            codepoint: g.glyph_id,
            mask: 0,
            // Core clusters are UTF-8 offsets into the buffer text;
            // report them in the units of the caller's add call.
            cluster: buffer_state.clusters.map(g.cluster, text_len),
            var1: 0,
            var2: 0,
        });
        positions.push(hb_glyph_position_t {
            x_advance: g.x_advance,
            y_advance: g.y_advance,
            x_offset: g.x_offset,
            y_offset: g.y_offset,
            var: 0,
        });
    }
    buffer_state.glyph_infos = infos;
    buffer_state.glyph_positions = positions;
    1
}
