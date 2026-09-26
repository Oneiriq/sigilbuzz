//! Buffer flags and cluster levels: `hb_buffer_set_flags`,
//! `hb_buffer_get_flags`, `hb_buffer_set_cluster_level`, and
//! `hb_buffer_get_cluster_level`, with HarfBuzz's constant values.
//!
//! Like HarfBuzz, a buffer starts with `HB_BUFFER_FLAG_DEFAULT` and
//! `HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES`; `hb_buffer_reset`
//! restores both and `hb_buffer_clear_contents` keeps them. (The Rust
//! `Buffer` defaults to MONOTONE_CHARACTERS instead; the C surface
//! follows HarfBuzz.)

use core::ffi::c_uint;

use sigilbuzz::{BufferFlags, ClusterLevel};

use crate::{hb_buffer_t, BufferState};

/// `hb_buffer_flags_t`.
pub type hb_buffer_flags_t = c_uint;
/// No flags.
pub const HB_BUFFER_FLAG_DEFAULT: hb_buffer_flags_t = 0x0000_0000;
/// Beginning of text: a leading combining mark gets a dotted circle.
pub const HB_BUFFER_FLAG_BOT: hb_buffer_flags_t = 0x0000_0001;
/// End of text. Stored; HarfBuzz's OpenType shaper reads no
/// end-of-text state, and neither does sigilbuzz.
pub const HB_BUFFER_FLAG_EOT: hb_buffer_flags_t = 0x0000_0002;
/// Keep default-ignorable glyphs and their advances.
pub const HB_BUFFER_FLAG_PRESERVE_DEFAULT_IGNORABLES: hb_buffer_flags_t = 0x0000_0004;
/// Delete default-ignorable glyphs instead of hiding them.
pub const HB_BUFFER_FLAG_REMOVE_DEFAULT_IGNORABLES: hb_buffer_flags_t = 0x0000_0008;
/// Never insert U+25CC DOTTED CIRCLE.
pub const HB_BUFFER_FLAG_DO_NOT_INSERT_DOTTED_CIRCLE: hb_buffer_flags_t = 0x0000_0010;

/// `hb_buffer_cluster_level_t`.
pub type hb_buffer_cluster_level_t = c_uint;
/// Graphemes merged, clusters monotone (HarfBuzz's default).
pub const HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES: hb_buffer_cluster_level_t = 0;
/// Characters kept apart, clusters monotone.
pub const HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS: hb_buffer_cluster_level_t = 1;
/// Characters kept apart, clusters in shaping order.
pub const HB_BUFFER_CLUSTER_LEVEL_CHARACTERS: hb_buffer_cluster_level_t = 2;
/// Graphemes merged, clusters in shaping order.
pub const HB_BUFFER_CLUSTER_LEVEL_GRAPHEMES: hb_buffer_cluster_level_t = 3;
/// HarfBuzz's default level.
pub const HB_BUFFER_CLUSTER_LEVEL_DEFAULT: hb_buffer_cluster_level_t =
    HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES;

/// The core level for a raw `hb_buffer_cluster_level_t`. HarfBuzz
/// stores any value; one outside its enum is neither monotone nor
/// grapheme-forming there, which is how CHARACTERS behaves.
pub(crate) fn core_level(level: hb_buffer_cluster_level_t) -> ClusterLevel {
    match level {
        HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES => ClusterLevel::MonotoneGraphemes,
        HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS => ClusterLevel::MonotoneCharacters,
        HB_BUFFER_CLUSTER_LEVEL_GRAPHEMES => ClusterLevel::Graphemes,
        _ => ClusterLevel::Characters,
    }
}

/// Puts the flags and cluster level back to HarfBuzz's defaults, on
/// the C state and on the core buffer (`hb_buffer_create` and
/// `hb_buffer_reset`).
pub(crate) fn restore_defaults(state: &mut BufferState) {
    state.flags = HB_BUFFER_FLAG_DEFAULT;
    state.cluster_level = HB_BUFFER_CLUSTER_LEVEL_DEFAULT;
    state.buffer.set_flags(BufferFlags::DEFAULT);
    state
        .buffer
        .set_cluster_level(core_level(HB_BUFFER_CLUSTER_LEVEL_DEFAULT));
}

/// Sets the buffer flags. Like HarfBuzz, the value is stored as given
/// and [`hb_buffer_get_flags`] returns it; bits sigilbuzz has no
/// behavior for (HarfBuzz's `VERIFY`, `PRODUCE_UNSAFE_TO_CONCAT`,
/// `PRODUCE_SAFE_TO_INSERT_TATWEEL`) have no effect.
///
/// # Safety
/// `buffer` must be null or a live buffer.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_set_flags(buffer: *mut hb_buffer_t, flags: hb_buffer_flags_t) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: the caller guarantees `buffer` is live.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    state.flags = flags;
    state
        .buffer
        .set_flags(BufferFlags::from_bits_truncate(flags));
}

/// The flags set with [`hb_buffer_set_flags`]; `HB_BUFFER_FLAG_DEFAULT`
/// for a null buffer.
///
/// # Safety
/// `buffer` must be null or a live buffer.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_get_flags(buffer: *const hb_buffer_t) -> hb_buffer_flags_t {
    if buffer.is_null() {
        return HB_BUFFER_FLAG_DEFAULT;
    }
    // SAFETY: the caller guarantees `buffer` is live.
    let inner = unsafe { &(*buffer).inner };
    let state = inner.state.lock();
    state.flags
}

/// Sets the cluster level. The value is stored as given and
/// [`hb_buffer_get_cluster_level`] returns it; one outside the enum
/// shapes like `HB_BUFFER_CLUSTER_LEVEL_CHARACTERS`.
///
/// # Safety
/// `buffer` must be null or a live buffer.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_set_cluster_level(
    buffer: *mut hb_buffer_t,
    cluster_level: hb_buffer_cluster_level_t,
) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: the caller guarantees `buffer` is live.
    let inner = unsafe { &(*buffer).inner };
    let mut state = inner.state.lock();
    state.cluster_level = cluster_level;
    state.buffer.set_cluster_level(core_level(cluster_level));
}

/// The cluster level set with [`hb_buffer_set_cluster_level`];
/// `HB_BUFFER_CLUSTER_LEVEL_DEFAULT` for a null buffer.
///
/// # Safety
/// `buffer` must be null or a live buffer.
#[no_mangle]
pub unsafe extern "C" fn hb_buffer_get_cluster_level(
    buffer: *const hb_buffer_t,
) -> hb_buffer_cluster_level_t {
    if buffer.is_null() {
        return HB_BUFFER_CLUSTER_LEVEL_DEFAULT;
    }
    // SAFETY: the caller guarantees `buffer` is live.
    let inner = unsafe { &(*buffer).inner };
    let state = inner.state.lock();
    state.cluster_level
}

#[cfg(test)]
mod tests;
