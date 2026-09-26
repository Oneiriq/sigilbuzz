//! Buffer functions: creation and reference counting, UTF-8 and UTF-16
//! text ingest, segment properties, and reading the shaped glyphs back.

use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_uint};
use core::ptr;
use core::slice;

use sigilbuzz::Buffer;

use crate::common::{c_str_bytes, map_direction_in};
use crate::opaque::BufferInner;
use crate::{
    buffer_text, handle, hb_buffer_t, hb_direction_t, hb_glyph_info_t, hb_glyph_position_t,
    hb_language_t, hb_script_t, spin_mutex, BufferState, HB_DIRECTION_INVALID, HB_SCRIPT_INVALID,
};

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
        clusters: buffer_text::ClusterTable::default(),
    };
    handle::into_raw(hb_buffer_t {
        inner: BufferInner {
            state: spin_mutex::SpinMutex::new(state),
        },
    })
}

/// Releases one reference to `buffer`. Null is a no-op.
///
/// # Safety
/// `buffer` must be null or a live buffer the caller holds a reference
/// to.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_destroy(buffer: *mut hb_buffer_t) {
    // SAFETY: caller guarantees `buffer` is null or a live handle it
    // owns a reference to.
    unsafe { handle::destroy(buffer) };
}

/// Adds one reference to `buffer` and returns `buffer` itself. Null
/// in, null out.
///
/// # Safety
/// `buffer` must be null or a live buffer.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_reference(buffer: *mut hb_buffer_t) -> *mut hb_buffer_t {
    // SAFETY: caller guarantees `buffer` is null or a live handle.
    unsafe { handle::reference(buffer) }
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
    // Like HarfBuzz, this also resets direction, script, language, and
    // the pre- and post-context.
    buffer_text::clear_contents(&mut state);
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
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    // Clusters are byte offsets into `text`, context comes from the
    // bytes around the item, and malformed UTF-8 becomes U+FFFD, all
    // as in HarfBuzz.
    let Some(item_length) = buffer_text::ItemLength::from_c(item_length) else {
        return;
    };
    buffer_text::add::<buffer_text::Utf8>(
        &mut state,
        total_bytes,
        item_offset as usize,
        item_length,
    );
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
    // SAFETY: `buffer` is non-null and the caller guarantees it points
    // to a live `hb_buffer_t`.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    // Clusters are UTF-16 code-unit offsets into `text`; lone
    // surrogates become U+FFFD, as in HarfBuzz.
    let Some(item_length) = buffer_text::ItemLength::from_c(item_length) else {
        return;
    };
    buffer_text::add::<buffer_text::Utf16>(
        &mut state,
        total_units,
        item_offset as usize,
        item_length,
    );
}

/// Sets the direction `hb_shape` lays the text out in.
///
/// Vertical directions switch to vertical metrics, as in HarfBuzz.
/// RTL and BTT runs come back in visual order (reversed), also as in
/// HarfBuzz. `HB_DIRECTION_INVALID`, or any other value outside
/// LTR..BTT, puts the buffer back to "unset".
///
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
    // HB_DIRECTION_INVALID (or any other value outside LTR..BTT) puts
    // the buffer back to "unset", so the core picks the layout itself
    // again, as in HarfBuzz.
    match map_direction_in(direction) {
        Some(core) => {
            state.direction = direction;
            state.buffer.set_direction(core);
        }
        None => {
            state.direction = HB_DIRECTION_INVALID;
            state.buffer.unset_direction();
        }
    }
}

/// Sets the buffer's script.
///
/// As in HarfBuzz, the script picks the script-specific shaper and the
/// OpenType script tables for the whole buffer. A script sigilbuzz has
/// no shaper for (and Common, Inherited, or Unknown) leaves the core
/// free to split the text into script runs.
///
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
    // A script sigilbuzz has a bucket for shapes the whole buffer.
    // Anything else leaves per-run script segmentation in place.
    state.buffer.set_script(buffer_text::core_script(script));
}

/// Sets the buffer's language.
///
/// As in HarfBuzz, the language picks the OpenType language system
/// `hb_shape` uses. Null, or a tag that does not parse, selects each
/// script's default language system.
///
/// # Safety
/// `buffer` must be null or valid. `language` must be null or a
/// NUL-terminated tag string, such as one `hb_language_from_string`
/// returns.
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
    // SAFETY: HarfBuzz's contract makes `language` null or a
    // NUL-terminated tag string from hb_language_from_string.
    state
        .buffer
        .set_language(unsafe { buffer_text::core_language(language) });
}

/// Fills in the script, direction, and language that are not set yet,
/// in HarfBuzz's order.
///
/// The script comes from the first character whose Script property is
/// not Common, Inherited, or Unknown. The direction comes from that
/// script: RTL for Arabic, Hebrew, and the other right-to-left scripts,
/// LTR otherwise. HarfBuzz takes the language from the process locale.
/// sigilbuzz uses `und`, which selects the default language system.
///
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
    // Script first, then the direction from the script (RTL for
    // Arabic, Hebrew, ...), then the language, as in HarfBuzz.
    buffer_text::guess_segment_properties(&mut state);
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
