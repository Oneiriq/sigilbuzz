//! HarfBuzz-symbol-compatible C API for sigilbuzz.
//!
//! Every public symbol in this crate is named `hb_*` so a binary
//! linker that previously resolved `hb_shape` against
//! `libharfbuzz.so` resolves it against `libsigilbuzz.so` without
//! a source-level change. The header at `include/hb.h` declares the
//! exact subset of HarfBuzz's public API the crate implements.
//!
//! # Refcounting and ownership
//!
//! Handles have HarfBuzz identity semantics: the pointer is the
//! object. Every refcounted type (`hb_blob_t`, `hb_face_t`,
//! `hb_font_t`, `hb_buffer_t`, `hb_set_t`, `hb_subset_input_t`,
//! `hb_paint_funcs_t`) lives inside an `alloc::sync::Arc` whose raw
//! pointer is what C holds. `hb_*_reference(p)` adds a reference and
//! returns `p`; `hb_*_destroy(p)` drops one and frees the object at
//! zero. Both accept null (reference returns null, destroy does
//! nothing). See the `handle` module for the details.
//!
//! The ownership rules are HarfBuzz's:
//!
//! - Every `*_create` result and every `*_reference` call is one
//!   reference the caller must release with the matching `*_destroy`.
//! - A face references its blob and a font references its face, so
//!   destroying the blob (or face) right after building on it is fine.
//! - `hb_subset_input_unicode_set` / `hb_subset_input_glyph_set` return
//!   a set owned by the input. The caller must not destroy it; it stays
//!   valid until the input is destroyed.
//!
//! # Lifetime erasure
//!
//! `sigilbuzz::Face<'a>` and `sigilbuzz::Font<'a>` borrow from a
//! byte slice. The C surface needs to expose those without the
//! lifetime parameter. We achieve that by:
//!
//! 1. `BlobInner` owns the bytes in a `Vec<u8>` that is never resized.
//! 2. `FaceInner` holds a reference to its blob *and* a `Face<'static>`
//!    constructed via [`core::mem::transmute`]. The transmute is
//!    sound because the blob reference keeps the underlying bytes alive
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

use core::ffi::{c_char, c_int, c_uint, c_void};

// `handle` holds the shared reference/destroy plumbing every opaque
// type goes through.
mod handle;
// `hb_set_t` lives in its own module, the opaque integer-set type
// the subset and introspection bridges need. It has no dependency on
// the rest of the crate, so it ships unconditionally. `introspect`
// follows the same posture: it doesn't reach into the subsetter or
// paint evaluator, just walks tables sigilbuzz already parses.
pub mod introspect;
pub mod set;
// Text ingest (`hb_buffer_add_*`), cluster mapping, and segment
// property handling for `hb_buffer_t`.
mod buffer_text;
pub use buffer_text::{hb_buffer_add_codepoints, hb_buffer_add_latin1, hb_buffer_add_utf32};
// `subset_bridge` is gated on the `subset` cargo feature so a
// `--no-default-features` build of this crate still compiles cleanly
// without pulling in the companion subsetter crate. `paint_bridge`
// follows the same pattern.
#[cfg(feature = "paint")]
pub mod paint_bridge;
#[cfg(feature = "subset")]
pub mod subset_bridge;

mod blob;
mod buffer;
// `hb_buffer_set_flags`, `hb_buffer_set_cluster_level`, and their getters.
mod buffer_flags;
mod common;
mod face;
mod font;
mod opaque;
mod shaping;

#[cfg(feature = "std")]
pub use blob::hb_blob_create_from_file;
pub use blob::{
    hb_blob_create, hb_blob_destroy, hb_blob_get_data, hb_blob_get_length, hb_blob_reference,
};
pub use buffer::{
    hb_buffer_add_utf16, hb_buffer_add_utf8, hb_buffer_clear_contents, hb_buffer_create,
    hb_buffer_destroy, hb_buffer_get_glyph_infos, hb_buffer_get_glyph_positions,
    hb_buffer_get_length, hb_buffer_guess_segment_properties, hb_buffer_reference, hb_buffer_reset,
    hb_buffer_set_direction, hb_buffer_set_language, hb_buffer_set_script, hb_glyph_flags_t,
    hb_glyph_info_get_glyph_flags, HB_GLYPH_FLAG_DEFINED, HB_GLYPH_FLAG_SAFE_TO_INSERT_TATWEEL,
    HB_GLYPH_FLAG_UNSAFE_TO_BREAK, HB_GLYPH_FLAG_UNSAFE_TO_CONCAT,
};
pub use buffer_flags::{
    hb_buffer_cluster_level_t, hb_buffer_flags_t, hb_buffer_get_cluster_level, hb_buffer_get_flags,
    hb_buffer_get_not_found_variation_selector_glyph, hb_buffer_set_cluster_level,
    hb_buffer_set_flags, hb_buffer_set_not_found_variation_selector_glyph,
    HB_BUFFER_CLUSTER_LEVEL_CHARACTERS, HB_BUFFER_CLUSTER_LEVEL_DEFAULT,
    HB_BUFFER_CLUSTER_LEVEL_GRAPHEMES, HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS,
    HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES, HB_BUFFER_FLAG_BOT, HB_BUFFER_FLAG_DEFAULT,
    HB_BUFFER_FLAG_DO_NOT_INSERT_DOTTED_CIRCLE, HB_BUFFER_FLAG_EOT,
    HB_BUFFER_FLAG_PRESERVE_DEFAULT_IGNORABLES, HB_BUFFER_FLAG_PRODUCE_SAFE_TO_INSERT_TATWEEL,
    HB_BUFFER_FLAG_PRODUCE_UNSAFE_TO_CONCAT, HB_BUFFER_FLAG_REMOVE_DEFAULT_IGNORABLES,
    HB_CODEPOINT_INVALID,
};
pub(crate) use common::lang_und;
pub use common::{
    hb_direction_from_string, hb_language_from_string, hb_script_from_iso15924_tag,
    hb_tag_from_string, hb_tag_to_string, hb_version, hb_version_string,
};
#[cfg(feature = "subset")]
pub(crate) use face::face_from_blob;
pub use face::{
    hb_face_create, hb_face_destroy, hb_face_get_glyph_count, hb_face_get_upem, hb_face_reference,
};
pub use font::{
    hb_font_create, hb_font_destroy, hb_font_get_glyph, hb_font_get_nominal_glyph,
    hb_font_get_scale, hb_font_get_variation_glyph, hb_font_reference, hb_font_set_ppem,
    hb_font_set_scale, hb_font_set_variations,
};
pub use opaque::{hb_blob_t, hb_buffer_t, hb_face_t, hb_font_t};
pub(crate) use opaque::{BlobInner, BufferState, FaceInner};
pub use shaping::{hb_shape, hb_shape_full};

// ---------------------------------------------------------------------------
// Tiny spin mutex so we stay no_std-friendly without pulling in libstd.
// ---------------------------------------------------------------------------

mod spin_mutex;

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
    /// Glyph flags (`hb_glyph_flags_t`). Read them with
    /// [`hb_glyph_info_get_glyph_flags`].
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
// Tests: Rust-side equivalence harness
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
